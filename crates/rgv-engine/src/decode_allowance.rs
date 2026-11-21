//! Process-wide allowance of concurrently active alignment decode threads.
//!
//! Every alignment source keeps its own decode thread (ARCH-6: a shared, visibility-keyed
//! scheduler is later work). This allowance only bounds how many of those threads decode at the
//! same moment: a thread takes a permit for each source unit it reads (one stream start, one
//! BGZF block, one CRAM slice) and gives it back before handing the unit over, so a source
//! waits its turn at unit granularity instead of every open source decoding at once.
//!
//! Order of service among waiting threads:
//!
//! 1. a speculative waiter that foreground waiters have overtaken
//!    [`MAXIMUM_SPECULATIVE_BYPASSES`] times (oldest first);
//! 2. otherwise the oldest foreground waiter (a stream serving a user-visible intent);
//! 3. otherwise the oldest speculative waiter (read-ahead).
//!
//! Foreground work is therefore served ahead of queued read-ahead, foreground waiters are
//! first-come first-served among themselves, and read-ahead still gets at least one unit in
//! every `MAXIMUM_SPECULATIVE_BYPASSES + 1` grants however much foreground work arrives.
//! A permit is never held while a thread waits for anything but its own source read.
//!
//! Priority is decided at each grant, and a stream asks again after every unit. With every
//! permit in use, a foreground stream therefore waits at most until the next unit in flight
//! finishes; it does not reserve a permit between its own units.

use std::{
    num::NonZeroUsize,
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
};

/// Foreground grants that may overtake one waiting speculative unit before it is served.
pub(crate) const MAXIMUM_SPECULATIVE_BYPASSES: u32 = 8;

/// Whose work a decode unit serves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecodeClass {
    /// A stream acquiring evidence a visible camera is waiting for.
    Foreground,
    /// Optional read-ahead.
    Speculative,
}

/// Counters and gauges of the process-wide decode allowance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentDecodeAllowanceSnapshot {
    /// Decode threads that may read a source unit at the same time.
    pub limit: usize,
    /// Threads reading a source unit right now.
    pub active: usize,
    /// Most threads that ever read at the same time.
    pub peak_active: usize,
    /// Threads waiting for their turn right now.
    pub waiting: usize,
    /// Most threads that ever waited at the same time.
    pub peak_waiting: usize,
    /// Units granted to streams serving a foreground intent.
    pub foreground_grants: u64,
    /// Units granted to read-ahead streams.
    pub speculative_grants: u64,
    /// Read-ahead units served by the starvation bound, ahead of any waiting foreground unit.
    pub promoted_speculative_grants: u64,
    /// Units that had to wait for a permit.
    pub contended_grants: u64,
    /// Waits abandoned because the stream was cancelled.
    pub withdrawn_waits: u64,
}

#[derive(Debug)]
struct Waiter {
    ticket: u64,
    class: DecodeClass,
    bypassed: u32,
}

/// The waiting line and the permits in use. Pure state: no blocking, so the order of service
/// can be tested without threads.
#[derive(Debug)]
pub(crate) struct AllowanceQueue {
    limit: usize,
    active: usize,
    next_ticket: u64,
    /// In arrival order.
    waiters: Vec<Waiter>,
    snapshot: AlignmentDecodeAllowanceSnapshot,
}

impl AllowanceQueue {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            limit: limit.get(),
            active: 0,
            next_ticket: 1,
            waiters: Vec::new(),
            snapshot: AlignmentDecodeAllowanceSnapshot::default(),
        }
    }

    /// Joins the line and returns the ticket to ask about.
    pub(crate) fn enqueue(&mut self, class: DecodeClass) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.waiters.push(Waiter {
            ticket,
            class,
            bypassed: 0,
        });
        self.snapshot.peak_waiting = self.snapshot.peak_waiting.max(self.waiters.len());
        ticket
    }

    /// Service tier of a waiter: lower is served first; arrival order decides within a tier.
    fn tier(waiter: &Waiter) -> u8 {
        match waiter.class {
            DecodeClass::Speculative if waiter.bypassed >= MAXIMUM_SPECULATIVE_BYPASSES => 0,
            DecodeClass::Foreground => 1,
            DecodeClass::Speculative => 2,
        }
    }

    /// Index of the waiter served next, whether or not a permit is free.
    #[cfg(test)]
    fn next_index(&self) -> Option<usize> {
        // `min_by_key` keeps the first of equal keys, which is the earliest arrival.
        self.waiters
            .iter()
            .enumerate()
            .min_by_key(|(_, waiter)| Self::tier(waiter))
            .map(|(index, _)| index)
    }

    /// The ticket that takes the next free permit, if one is free.
    #[cfg(test)]
    pub(crate) fn next_grant(&self) -> Option<u64> {
        if self.active >= self.limit {
            return None;
        }
        self.next_index().map(|index| self.waiters[index].ticket)
    }

    /// Takes a permit for `ticket` when it is among the waiters the free permits go to: with
    /// `n` permits free, the first `n` waiters in service order. (Not only the very first: a
    /// waiter must not have to wait for the one ahead of it to wake up and take its own.)
    pub(crate) fn try_grant(&mut self, ticket: u64) -> bool {
        let Some(index) = self
            .waiters
            .iter()
            .position(|waiter| waiter.ticket == ticket)
        else {
            return false;
        };
        let key = (Self::tier(&self.waiters[index]), index);
        let ahead = self
            .waiters
            .iter()
            .enumerate()
            .filter(|(other, waiter)| (Self::tier(waiter), *other) < key)
            .count();
        if ahead >= self.limit.saturating_sub(self.active) {
            return false;
        }
        let granted = self.waiters.remove(index);
        match granted.class {
            DecodeClass::Foreground => {
                // Everything read-ahead that arrived earlier was overtaken once more.
                for waiter in &mut self.waiters[..index] {
                    if waiter.class == DecodeClass::Speculative {
                        waiter.bypassed = waiter.bypassed.saturating_add(1);
                    }
                }
                self.snapshot.foreground_grants += 1;
            }
            DecodeClass::Speculative => {
                self.snapshot.speculative_grants += 1;
                if granted.bypassed >= MAXIMUM_SPECULATIVE_BYPASSES {
                    self.snapshot.promoted_speculative_grants += 1;
                }
            }
        }
        self.active += 1;
        self.snapshot.peak_active = self.snapshot.peak_active.max(self.active);
        true
    }

    /// Leaves the line without having been served.
    pub(crate) fn withdraw(&mut self, ticket: u64) {
        let before = self.waiters.len();
        self.waiters.retain(|waiter| waiter.ticket != ticket);
        if self.waiters.len() != before {
            self.snapshot.withdrawn_waits += 1;
        }
    }

    /// Gives a permit back.
    pub(crate) fn release(&mut self) {
        self.active = self.active.saturating_sub(1);
    }

    pub(crate) fn snapshot(&self) -> AlignmentDecodeAllowanceSnapshot {
        AlignmentDecodeAllowanceSnapshot {
            limit: self.limit,
            active: self.active,
            waiting: self.waiters.len(),
            ..self.snapshot
        }
    }
}

/// Blocking front of an [`AllowanceQueue`], shared by the decode threads it bounds.
#[derive(Debug)]
pub(crate) struct DecodeAllowance {
    queue: Mutex<AllowanceQueue>,
    changed: Condvar,
}

/// One unit's right to decode; dropping it gives the permit back, also on unwind.
#[derive(Debug)]
pub(crate) struct DecodePermit<'a> {
    allowance: &'a DecodeAllowance,
}

impl Drop for DecodePermit<'_> {
    fn drop(&mut self) {
        self.allowance.lock().release();
        self.allowance.changed.notify_all();
    }
}

/// A [`DecodePermit`] that owns its allowance; dropping it gives the permit back.
#[derive(Debug)]
pub(crate) struct OwnedDecodePermit {
    allowance: Arc<DecodeAllowance>,
}

impl Drop for OwnedDecodePermit {
    fn drop(&mut self) {
        self.allowance.lock().release();
        self.allowance.changed.notify_all();
    }
}

impl DecodeAllowance {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            queue: Mutex::new(AllowanceQueue::new(limit)),
            changed: Condvar::new(),
        }
    }

    /// The allowance every production decode thread shares.
    pub(crate) fn process() -> Arc<Self> {
        static PROCESS: OnceLock<Arc<DecodeAllowance>> = OnceLock::new();
        Arc::clone(PROCESS.get_or_init(|| Arc::new(Self::new(default_limit()))))
    }

    /// The queue holds only counters and tickets, which stay consistent if a holder panicked.
    fn lock(&self) -> MutexGuard<'_, AllowanceQueue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for this unit's turn. Returns `None`, having left the line, once `still_wanted`
    /// is false; whoever makes it false must call [`Self::wake`].
    pub(crate) fn acquire(
        &self,
        class: DecodeClass,
        still_wanted: &dyn Fn() -> bool,
    ) -> Option<DecodePermit<'_>> {
        let mut queue = self.lock();
        let ticket = queue.enqueue(class);
        let mut waited = false;
        loop {
            if !still_wanted() {
                queue.withdraw(ticket);
                drop(queue);
                // Whoever is next in line may have been waiting behind this ticket.
                self.changed.notify_all();
                return None;
            }
            if queue.try_grant(ticket) {
                if waited {
                    queue.snapshot.contended_grants += 1;
                }
                drop(queue);
                // A grant changes who is next (and may promote overtaken read-ahead), so the
                // other waiters look again rather than sleep beside a free permit.
                self.changed.notify_all();
                return Some(DecodePermit { allowance: self });
            }
            waited = true;
            queue = self
                .changed
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// [`Self::acquire`] for a holder that must be able to give the permit up and take it
    /// again from inside a callback (the coverage worker's cancellation probe), where a
    /// borrowed permit cannot live.
    pub(crate) fn acquire_owned(
        self: &Arc<Self>,
        class: DecodeClass,
        still_wanted: &dyn Fn() -> bool,
    ) -> Option<OwnedDecodePermit> {
        // The borrowed permit's release is taken over by the owned one.
        std::mem::forget(self.acquire(class, still_wanted)?);
        Some(OwnedDecodePermit {
            allowance: Arc::clone(self),
        })
    }

    /// Whether a unit serving a visible camera is waiting for a permit right now.
    pub(crate) fn foreground_waiting(&self) -> bool {
        self.lock()
            .waiters
            .iter()
            .any(|waiter| waiter.class == DecodeClass::Foreground)
    }

    /// Makes every waiter look at its cancellation state again.
    pub(crate) fn wake(&self) {
        // Taking the lock orders this wake after a waiter's own check, so none is lost.
        drop(self.lock());
        self.changed.notify_all();
    }

    pub(crate) fn set_limit(&self, limit: NonZeroUsize) {
        self.lock().limit = limit.get();
        self.changed.notify_all();
    }

    pub(crate) fn snapshot(&self) -> AlignmentDecodeAllowanceSnapshot {
        self.lock().snapshot()
    }
}

/// One decode thread per available core, at least four and at most sixteen. Decoding a local
/// source is CPU-bound, so more than the cores only adds contention with scene building and
/// rendering. The floor and the generous ceiling are for remote sources, whose decode threads
/// spend most of a unit waiting on the network while they hold a permit.
fn default_limit() -> NonZeroUsize {
    let cores = std::thread::available_parallelism().map_or(4, NonZeroUsize::get);
    NonZeroUsize::new(cores.clamp(4, 16)).expect("the clamped limit is at least four")
}

/// How the process-wide alignment decode allowance stands: its limit, the threads decoding and
/// waiting now, and what it has granted so far.
#[must_use]
pub fn alignment_decode_allowance() -> AlignmentDecodeAllowanceSnapshot {
    DecodeAllowance::process().snapshot()
}

/// Changes how many alignment decode threads may read a source unit at the same time, across
/// every open alignment source. Units already being read finish; a lower limit applies to the
/// next grant.
pub fn set_alignment_decode_concurrency(limit: NonZeroUsize) {
    DecodeAllowance::process().set_limit(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(limit: usize) -> AllowanceQueue {
        AllowanceQueue::new(NonZeroUsize::new(limit).unwrap())
    }

    #[test]
    fn no_more_permits_are_granted_than_the_limit() {
        let mut queue = queue(2);
        let tickets = (0..5)
            .map(|_| queue.enqueue(DecodeClass::Foreground))
            .collect::<Vec<_>>();
        assert!(queue.try_grant(tickets[0]));
        assert!(queue.try_grant(tickets[1]));
        assert_eq!(queue.next_grant(), None);
        assert!(!queue.try_grant(tickets[2]));
        assert_eq!(queue.snapshot().active, 2);
        assert_eq!(queue.snapshot().waiting, 3);

        queue.release();
        // Not out of turn, even with a permit free.
        assert!(!queue.try_grant(tickets[3]));
        assert!(queue.try_grant(tickets[2]));
        assert_eq!(queue.snapshot().peak_active, 2);
        assert_eq!(queue.snapshot().peak_waiting, 5);
    }

    #[test]
    fn a_foreground_unit_is_served_ahead_of_read_ahead_that_was_already_waiting() {
        let mut queue = queue(1);
        let holder = queue.enqueue(DecodeClass::Speculative);
        assert!(queue.try_grant(holder));
        let read_ahead = [
            queue.enqueue(DecodeClass::Speculative),
            queue.enqueue(DecodeClass::Speculative),
        ];
        let foreground = queue.enqueue(DecodeClass::Foreground);

        queue.release();
        assert_eq!(queue.next_grant(), Some(foreground));
        assert!(!queue.try_grant(read_ahead[0]));
        assert!(queue.try_grant(foreground));
        queue.release();
        // With no foreground waiting, read-ahead runs in arrival order.
        assert!(queue.try_grant(read_ahead[0]));
        queue.release();
        assert!(queue.try_grant(read_ahead[1]));
        let snapshot = queue.snapshot();
        assert_eq!(snapshot.foreground_grants, 1);
        assert_eq!(snapshot.speculative_grants, 3);
        assert_eq!(snapshot.promoted_speculative_grants, 0);
    }

    #[test]
    fn read_ahead_is_served_within_a_bounded_number_of_foreground_grants() {
        let mut queue = queue(1);
        let read_ahead = queue.enqueue(DecodeClass::Speculative);
        let mut foreground_before_read_ahead = 0_u32;
        // Foreground work never stops arriving: there are always two foreground units queued.
        let mut waiting = std::collections::VecDeque::from([
            queue.enqueue(DecodeClass::Foreground),
            queue.enqueue(DecodeClass::Foreground),
        ]);
        loop {
            let next = queue.next_grant().expect("a permit is free");
            assert!(queue.try_grant(next));
            queue.release();
            if next == read_ahead {
                break;
            }
            // Foreground units are served in arrival order.
            assert_eq!(waiting.pop_front(), Some(next));
            foreground_before_read_ahead += 1;
            waiting.push_back(queue.enqueue(DecodeClass::Foreground));
            assert!(
                foreground_before_read_ahead <= MAXIMUM_SPECULATIVE_BYPASSES,
                "read-ahead was starved"
            );
        }
        assert_eq!(foreground_before_read_ahead, MAXIMUM_SPECULATIVE_BYPASSES);
        assert_eq!(queue.snapshot().promoted_speculative_grants, 1);
        // The promotion did not cost the foreground line its order.
        assert_eq!(queue.next_grant(), waiting.front().copied());
    }

    #[test]
    fn every_queued_unit_of_either_class_is_eventually_served() {
        let mut queue = queue(3);
        let mut tickets = Vec::new();
        for index in 0..64 {
            tickets.push(queue.enqueue(if index % 3 == 0 {
                DecodeClass::Speculative
            } else {
                DecodeClass::Foreground
            }));
        }
        let mut served = Vec::new();
        while let Some(next) = queue.next_grant() {
            assert!(queue.try_grant(next));
            served.push(next);
            queue.release();
        }
        served.sort_unstable();
        assert_eq!(served, tickets);
        assert_eq!(queue.snapshot().waiting, 0);
    }

    #[test]
    fn free_permits_go_to_the_first_waiters_in_any_order_of_asking() {
        let mut queue = queue(2);
        let holders = [
            queue.enqueue(DecodeClass::Foreground),
            queue.enqueue(DecodeClass::Foreground),
        ];
        assert!(queue.try_grant(holders[0]) && queue.try_grant(holders[1]));
        let read_ahead = queue.enqueue(DecodeClass::Speculative);
        let first = queue.enqueue(DecodeClass::Foreground);
        let second = queue.enqueue(DecodeClass::Foreground);
        let third = queue.enqueue(DecodeClass::Foreground);
        // Both permits come back before any waiter has looked.
        queue.release();
        queue.release();
        // The second in line does not have to wait for the first to take its permit, and
        // nobody further back gets in.
        assert!(!queue.try_grant(third));
        assert!(!queue.try_grant(read_ahead));
        assert!(queue.try_grant(second));
        assert!(!queue.try_grant(third));
        assert!(queue.try_grant(first));
        assert_eq!(queue.snapshot().active, 2);
        assert_eq!(queue.snapshot().waiting, 2);
    }

    #[test]
    fn a_withdrawn_waiter_does_not_block_the_line() {
        let mut queue = queue(1);
        let holder = queue.enqueue(DecodeClass::Foreground);
        assert!(queue.try_grant(holder));
        let cancelled = queue.enqueue(DecodeClass::Foreground);
        let next = queue.enqueue(DecodeClass::Speculative);
        queue.withdraw(cancelled);
        queue.release();
        assert!(queue.try_grant(next));
        assert_eq!(queue.snapshot().withdrawn_waits, 1);
    }

    #[test]
    fn a_blocked_acquire_returns_when_its_stream_is_cancelled_or_a_permit_is_released() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let allowance = Arc::new(DecodeAllowance::new(NonZeroUsize::new(1).unwrap()));
        let held = allowance
            .acquire(DecodeClass::Foreground, &|| true)
            .expect("the first unit is granted at once");

        let wanted = Arc::new(AtomicBool::new(true));
        let cancelled = {
            let (allowance, wanted) = (Arc::clone(&allowance), Arc::clone(&wanted));
            std::thread::spawn(move || {
                allowance
                    .acquire(DecodeClass::Foreground, &|| wanted.load(Ordering::Acquire))
                    .is_some()
            })
        };
        let served = {
            let allowance = Arc::clone(&allowance);
            std::thread::spawn(move || {
                let permit = allowance.acquire(DecodeClass::Speculative, &|| true);
                permit.is_some()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while allowance.snapshot().waiting < 2 {
            assert!(std::time::Instant::now() < deadline, "waiters never queued");
            std::thread::yield_now();
        }

        wanted.store(false, Ordering::Release);
        allowance.wake();
        assert!(!cancelled.join().unwrap(), "a cancelled wait took a permit");
        assert_eq!(allowance.snapshot().active, 1);

        drop(held);
        assert!(served.join().unwrap());
        let snapshot = allowance.snapshot();
        assert_eq!((snapshot.active, snapshot.waiting), (0, 0));
        assert_eq!(snapshot.peak_active, 1);
        assert_eq!(snapshot.contended_grants, 1);
    }
}
