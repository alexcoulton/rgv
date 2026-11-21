use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiserror::Error;

type UiWaker = Arc<dyn Fn() + Send + Sync>;

static UI_WAKER: OnceLock<UiWaker> = OnceLock::new();

/// Registers the callback engines use to wake the application's event loop whenever a worker
/// publishes a result, so the UI does not have to poll for it. Only the first registration in
/// a process takes effect; returns whether this call was that one.
pub fn set_ui_waker(waker: impl Fn() + Send + Sync + 'static) -> bool {
    UI_WAKER.set(Arc::new(waker)).is_ok()
}

pub(crate) fn wake_ui() {
    if let Some(waker) = UI_WAKER.get() {
        waker();
    }
}

/// Non-blocking scheduling failure shared by the bounded source, reference, and contextual-lane
/// actors. This is deliberately independent of any particular data engine.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EngineQueueError {
    #[error("the bounded engine queue is full")]
    Full,
    #[error("the engine actor is disconnected")]
    Disconnected,
}

struct Stamped<T> {
    sequence: u128,
    value: T,
}

struct MailboxState<T> {
    controls: VecDeque<Stamped<T>>,
    latest: VecDeque<(u128, Stamped<T>)>,
    next_sequence: u128,
    sender_open: bool,
    receiver_open: bool,
    /// Set by a [`MailboxWaker`]; the next blocking receive returns early and clears it.
    woken: bool,
}

struct MailboxInner<T> {
    control_capacity: usize,
    /// Whether the receiver is the UI thread, which is woken on every send.
    wakes_ui: bool,
    state: Mutex<MailboxState<T>>,
    has_data: Condvar,
    has_control_capacity: Condvar,
}

pub(crate) struct CoalescingSender<T> {
    inner: Arc<MailboxInner<T>>,
}

pub(crate) struct CoalescingReceiver<T> {
    inner: Arc<MailboxInner<T>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MailboxDisconnected;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TryMailboxSendError<T> {
    Full(T),
    Disconnected(T),
}

pub(crate) fn bounded_coalescing<T>(
    control_capacity: usize,
) -> (CoalescingSender<T>, CoalescingReceiver<T>) {
    mailbox(control_capacity, false)
}

/// A worker-to-UI mailbox: every send also wakes the application's event loop.
pub(crate) fn bounded_coalescing_to_ui<T>(
    control_capacity: usize,
) -> (CoalescingSender<T>, CoalescingReceiver<T>) {
    mailbox(control_capacity, true)
}

fn mailbox<T>(
    control_capacity: usize,
    wakes_ui: bool,
) -> (CoalescingSender<T>, CoalescingReceiver<T>) {
    let inner = Arc::new(MailboxInner {
        control_capacity: control_capacity.max(1),
        wakes_ui,
        state: Mutex::new(MailboxState {
            controls: VecDeque::new(),
            latest: VecDeque::new(),
            next_sequence: 0,
            sender_open: true,
            receiver_open: true,
            woken: false,
        }),
        has_data: Condvar::new(),
        has_control_capacity: Condvar::new(),
    });
    (
        CoalescingSender {
            inner: Arc::clone(&inner),
        },
        CoalescingReceiver { inner },
    )
}

impl<T> CoalescingSender<T> {
    pub(crate) fn is_closed(&self) -> bool {
        !self
            .inner
            .state
            .lock()
            .expect("mailbox mutex poisoned")
            .receiver_open
    }

    /// Enqueues a non-discardable control without ever waiting for capacity. UI and render
    /// threads use this form so a saturated worker cannot stall input handling.
    pub(crate) fn try_send_control(&self, value: T) -> Result<(), TryMailboxSendError<T>> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        if !state.receiver_open {
            return Err(TryMailboxSendError::Disconnected(value));
        }
        if state.controls.len() >= self.inner.control_capacity {
            return Err(TryMailboxSendError::Full(value));
        }
        let stamped = stamp(&mut state, value);
        state.controls.push_back(stamped);
        self.inner.has_data.notify_one();
        drop(state);
        if self.inner.wakes_ui {
            wake_ui();
        }
        Ok(())
    }

    /// Enqueues an event that must not be discarded. This applies backpressure
    /// when the bounded control queue is full and unblocks if the receiver is
    /// closed during shutdown.
    pub(crate) fn send_control(&self, value: T) -> Result<(), T> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        while state.controls.len() >= self.inner.control_capacity && state.receiver_open {
            state = self
                .inner
                .has_control_capacity
                .wait(state)
                .expect("mailbox mutex poisoned");
        }
        if !state.receiver_open {
            return Err(value);
        }
        let stamped = stamp(&mut state, value);
        state.controls.push_back(stamped);
        self.inner.has_data.notify_one();
        drop(state);
        if self.inner.wakes_ui {
            wake_ui();
        }
        Ok(())
    }

    /// Publishes replaceable viewport data. At most one such event is retained,
    /// and a newer result atomically supersedes an older result the UI has not
    /// consumed yet.
    /// Publishes replaceable viewport data for one stable owner. A newer
    /// value supersedes only that owner's unread value; other panels retain
    /// their own latest result.
    pub(crate) fn send_latest_for(&self, owner: u64, value: T) -> Result<(), T> {
        self.send_latest_for_key(u128::from(owner), value)
    }

    /// Publishes replaceable process-wide work outside the owner key space. A policy retry is
    /// source-scoped rather than panel-owned, so even `u64::MAX` must remain a valid owner key.
    pub(crate) fn send_latest_system(&self, key: u64, value: T) -> Result<(), T> {
        self.send_latest_for_key(u128::from(u64::MAX) + 1 + u128::from(key), value)
    }

    /// Publishes owner-scoped work in a system namespace without colliding with viewport-owner
    /// keys or another system work class. This is used for resumptions which must neither fill a
    /// control FIFO nor replace an unread newer camera.
    pub(crate) fn send_latest_system_for(
        &self,
        namespace: u64,
        owner: u64,
        value: T,
    ) -> Result<(), T> {
        self.send_latest_for_key(system_owner_key(namespace, owner), value)
    }

    /// A handle through which a producer that does not own this mailbox can publish
    /// replaceable values into it. See [`CoalescingSideSender`].
    pub(crate) fn side_sender(&self) -> CoalescingSideSender<T> {
        CoalescingSideSender {
            inner: Arc::clone(&self.inner),
        }
    }

    fn send_latest_for_key(&self, key: u128, value: T) -> Result<(), T> {
        self.inner.send_latest_for_key(key, value, false)
    }
}

fn system_owner_key(namespace: u64, owner: u64) -> u128 {
    let width = u128::from(u64::MAX) + 1;
    width
        .saturating_mul(u128::from(namespace).saturating_add(1))
        .saturating_add(u128::from(owner))
}

/// A second way into a mailbox, for a producer on another thread that is not the mailbox's
/// owner (one engine handing a result straight to another engine's worker). It publishes
/// replaceable values only, so it can never fill the owner's control queue; it does not keep
/// the mailbox open; and every send fails once the owning sender or the receiver is gone.
pub(crate) struct CoalescingSideSender<T> {
    inner: Arc<MailboxInner<T>>,
}

impl<T> Clone for CoalescingSideSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> CoalescingSideSender<T> {
    /// As [`CoalescingSender::send_latest_system_for`]: at most one unread value per
    /// `(namespace, owner)`, the newest.
    pub(crate) fn send_latest_system_for(
        &self,
        namespace: u64,
        owner: u64,
        value: T,
    ) -> Result<(), T> {
        self.inner
            .send_latest_for_key(system_owner_key(namespace, owner), value, true)
    }
}

impl<T> MailboxInner<T> {
    fn send_latest_for_key(
        &self,
        key: u128,
        value: T,
        requires_owning_sender: bool,
    ) -> Result<(), T> {
        let mut state = self.state.lock().expect("mailbox mutex poisoned");
        if !state.receiver_open || (requires_owning_sender && !state.sender_open) {
            return Err(value);
        }
        if let Some(index) = state
            .latest
            .iter()
            .position(|(candidate, _)| *candidate == key)
        {
            state.latest.remove(index);
        }
        let stamped = stamp(&mut state, value);
        state.latest.push_back((key, stamped));
        self.has_data.notify_one();
        drop(state);
        if self.wakes_ui {
            wake_ui();
        }
        Ok(())
    }
}

impl<T> Drop for CoalescingSender<T> {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        state.sender_open = false;
        self.inner.has_data.notify_all();
    }
}

impl<T> CoalescingReceiver<T> {
    pub(crate) fn try_receive(&self) -> Result<Option<T>, MailboxDisconnected> {
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        if let Some(value) = take_next(&mut state) {
            self.inner.has_control_capacity.notify_one();
            return Ok(Some(value));
        }
        if state.sender_open {
            Ok(None)
        } else {
            Err(MailboxDisconnected)
        }
    }

    pub(crate) fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<T>, MailboxDisconnected> {
        let started = Instant::now();
        let mut state = self.inner.state.lock().expect("mailbox mutex poisoned");
        loop {
            if let Some(value) = take_next(&mut state) {
                self.inner.has_control_capacity.notify_one();
                return Ok(Some(value));
            }
            if !state.sender_open {
                return Err(MailboxDisconnected);
            }
            if std::mem::take(&mut state.woken) {
                // Work became runnable outside the mailbox (for example a decoder finished a
                // unit). Return so the owner re-evaluates instead of sleeping out the timeout.
                return Ok(None);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next_state, wait) = self
                .inner
                .has_data
                .wait_timeout(state, remaining)
                .expect("mailbox mutex poisoned");
            state = next_state;
            if wait.timed_out() && state.controls.is_empty() && state.latest.is_empty() {
                return Ok(None);
            }
        }
    }

    /// Closes the consumer side before worker joins so a producer waiting on
    /// bounded control capacity can terminate cleanly.
    pub(crate) fn close(&self) {
        close_receiver(&self.inner);
    }
}

impl<T: Send + 'static> CoalescingReceiver<T> {
    /// A handle another thread uses to end this receiver's current (or next) blocking wait
    /// without enqueueing a value. It keeps neither side of the mailbox open.
    pub(crate) fn waker(&self) -> MailboxWaker {
        let inner = Arc::clone(&self.inner);
        Arc::new(move || {
            // A poisoned mailbox already belongs to a panicking owner; there is nobody to wake.
            if let Ok(mut state) = inner.state.lock() {
                state.woken = true;
                inner.has_data.notify_all();
            }
        })
    }
}

/// Dropping the consumer is a disconnection: without this a worker that exited (or panicked)
/// would leave its senders succeeding forever into a mailbox nobody reads.
impl<T> Drop for CoalescingReceiver<T> {
    fn drop(&mut self) {
        close_receiver(&self.inner);
    }
}

fn close_receiver<T>(inner: &MailboxInner<T>) {
    // Runs during unwinding too, so a poisoned mutex must not turn into a double panic.
    let mut state = inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.receiver_open = false;
    inner.has_control_capacity.notify_all();
}

/// Ends a blocking mailbox receive from another thread. See [`CoalescingReceiver::waker`].
pub(crate) type MailboxWaker = Arc<dyn Fn() + Send + Sync>;

/// Takes over the join of a worker which has already been told to stop, so the caller never
/// waits for whatever I/O that worker is in the middle of. A finished worker is joined on the
/// spot; a running one is handed to a short-lived reaper thread, whose handle is returned for
/// callers which want to observe the exit. `None` means there is nothing left to wait for (or
/// the reaper could not be started and the worker was detached).
pub(crate) fn reap_worker(
    worker: std::thread::JoinHandle<()>,
    reaper_name: &'static str,
    worker_description: &'static str,
) -> Option<std::thread::JoinHandle<()>> {
    let finished = worker.is_finished();
    let join = move || {
        if worker.join().is_err() {
            tracing::warn!(
                worker = worker_description,
                "worker panicked while retiring"
            );
        }
    };
    if finished {
        join();
        return None;
    }
    match std::thread::Builder::new()
        .name(reaper_name.to_owned())
        .spawn(join)
    {
        Ok(reaper) => Some(reaper),
        Err(error) => {
            tracing::warn!(
                %error,
                worker = worker_description,
                "could not spawn a reaper; worker detached"
            );
            None
        }
    }
}

/// Best-effort text of a caught panic payload, for a terminal error event.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&'static str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic payload was not a string".to_owned())
}

/// Runs one thread's whole body. A panic never unwinds out of the thread silently: it is
/// handed to `on_panic` as text so the owner can publish a terminal error event.
pub(crate) fn run_thread_root(body: impl FnOnce(), on_panic: impl FnOnce(String)) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        on_panic(panic_message(payload.as_ref()));
    }
}

fn stamp<T>(state: &mut MailboxState<T>, value: T) -> Stamped<T> {
    let sequence = state.next_sequence;
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .expect("mailbox sequence exhausted");
    Stamped { sequence, value }
}

fn take_next<T>(state: &mut MailboxState<T>) -> Option<T> {
    let control_is_next = match (state.controls.front(), state.latest.front()) {
        (Some(control), Some((_, latest))) => control.sequence < latest.sequence,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => return None,
    };
    if control_is_next {
        state.controls.pop_front().map(|event| event.value)
    } else {
        state.latest.pop_front().map(|(_, event)| event.value)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn a_side_sender_publishes_replaceable_values_only_while_the_mailbox_has_both_owners() {
        let (sender, receiver) = bounded_coalescing(1);
        let side = sender.side_sender();
        // The owner's control queue is full; a side sender is not affected and cannot add to it.
        sender.try_send_control("control").unwrap();
        side.send_latest_system_for(4, 7, "older offer").unwrap();
        side.clone()
            .send_latest_system_for(4, 7, "newer offer")
            .unwrap();
        side.send_latest_system_for(4, 8, "another panel").unwrap();
        // The owner's own slot in another namespace with the same owner number is distinct.
        sender.send_latest_system_for(1, 7, "retry").unwrap();
        let mut values = Vec::new();
        while let Some(value) = receiver.try_receive().unwrap() {
            values.push(value);
        }
        assert_eq!(values, ["control", "newer offer", "another panel", "retry"]);

        // It does not keep the mailbox open, and refuses once the owning sender is gone.
        drop(sender);
        assert_eq!(side.send_latest_system_for(4, 7, "late"), Err("late"));
        assert_eq!(receiver.try_receive(), Err(MailboxDisconnected));

        let (sender, receiver) = bounded_coalescing(1);
        let side = sender.side_sender();
        drop(receiver);
        assert_eq!(side.send_latest_system_for(4, 7, "late"), Err("late"));
    }

    #[test]
    fn newest_replaceable_value_supersedes_an_unread_value() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(0, 1).unwrap();
        sender.send_latest_for(0, 2).unwrap();
        sender.send_latest_for(0, 3).unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some(3));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn newest_replaceable_value_is_retained_per_owner() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(1, "panel 1 old").unwrap();
        sender.send_latest_for(2, "panel 2").unwrap();
        sender.send_latest_for(1, "panel 1 new").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("panel 2"));
        assert_eq!(receiver.try_receive().unwrap(), Some("panel 1 new"));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn system_latest_key_cannot_replace_the_maximum_owner_key() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(u64::MAX, "owner").unwrap();
        sender.send_latest_system(0, "old policy").unwrap();
        sender.send_latest_system(0, "new policy").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("owner"));
        assert_eq!(receiver.try_receive().unwrap(), Some("new policy"));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn owner_scoped_system_namespaces_do_not_replace_cameras_or_each_other() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_latest_for(7, "camera").unwrap();
        sender.send_latest_system_for(1, 7, "old retry").unwrap();
        sender.send_latest_system_for(2, 7, "other work").unwrap();
        sender.send_latest_system_for(1, 7, "new retry").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("camera"));
        assert_eq!(receiver.try_receive().unwrap(), Some("other work"));
        assert_eq!(receiver.try_receive().unwrap(), Some("new retry"));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn control_events_retain_order_around_latest_value() {
        let (sender, receiver) = bounded_coalescing(2);
        sender.send_control("ready").unwrap();
        sender.send_latest_for(0, "old viewport").unwrap();
        sender.send_control("warning").unwrap();
        sender.send_latest_for(0, "new viewport").unwrap();

        assert_eq!(receiver.try_receive().unwrap(), Some("ready"));
        assert_eq!(receiver.try_receive().unwrap(), Some("warning"));
        assert_eq!(receiver.try_receive().unwrap(), Some("new viewport"));
    }

    #[test]
    fn closing_receiver_unblocks_a_full_control_queue() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_control(1).unwrap();
        let blocked = thread::spawn(move || sender.send_control(2));

        receiver.close();

        assert_eq!(blocked.join().unwrap(), Err(2));
    }

    #[test]
    fn nonblocking_control_reports_full_and_disconnected_without_dropping_order() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.try_send_control(1).unwrap();
        assert_eq!(
            sender.try_send_control(2),
            Err(TryMailboxSendError::Full(2))
        );
        assert_eq!(receiver.try_receive().unwrap(), Some(1));
        receiver.close();
        assert_eq!(
            sender.try_send_control(3),
            Err(TryMailboxSendError::Disconnected(3))
        );
    }

    #[test]
    fn dropping_the_receiver_disconnects_every_send_form() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.try_send_control(1).unwrap();
        drop(receiver);

        assert!(sender.is_closed());
        assert_eq!(
            sender.try_send_control(2),
            Err(TryMailboxSendError::Disconnected(2))
        );
        assert_eq!(sender.send_control(3), Err(3));
        assert_eq!(sender.send_latest_for(0, 4), Err(4));
        assert_eq!(sender.send_latest_system(0, 5), Err(5));
    }

    #[test]
    fn dropping_the_receiver_unblocks_a_full_control_queue() {
        let (sender, receiver) = bounded_coalescing(1);
        sender.send_control(1).unwrap();
        let blocked = thread::spawn(move || sender.send_control(2));

        drop(receiver);

        assert_eq!(blocked.join().unwrap(), Err(2));
    }

    #[test]
    fn a_waker_ends_a_blocking_receive_without_a_value() {
        let (sender, receiver) = bounded_coalescing::<u8>(1);
        let waker = receiver.waker();
        // Signalled before the wait starts: the wake must not be lost.
        waker();
        let started = Instant::now();
        assert_eq!(
            receiver.receive_timeout(Duration::from_secs(30)).unwrap(),
            None
        );
        // Signalled from another thread while (or before) this thread waits.
        let remote = thread::spawn(move || waker());
        assert_eq!(
            receiver.receive_timeout(Duration::from_secs(30)).unwrap(),
            None
        );
        remote.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(20));
        // The wake is consumed; values and disconnection behave as before.
        sender.send_control(7).unwrap();
        assert_eq!(
            receiver.receive_timeout(Duration::from_secs(30)).unwrap(),
            Some(7)
        );
        drop(sender);
        assert_eq!(receiver.try_receive(), Err(MailboxDisconnected));
    }

    #[test]
    fn a_thread_root_reports_the_panic_message() {
        let reported = Mutex::new(None);
        run_thread_root(
            || panic!("decoder exploded at block {}", 7),
            |message| *reported.lock().unwrap() = Some(message),
        );
        assert_eq!(
            reported.into_inner().unwrap().as_deref(),
            Some("decoder exploded at block 7")
        );

        let mut completed = false;
        run_thread_root(|| completed = true, |_| unreachable!());
        assert!(completed);
    }

    #[test]
    fn timeout_and_sender_disconnect_are_distinct() {
        let (sender, receiver) = bounded_coalescing::<u8>(1);
        assert_eq!(
            receiver.receive_timeout(Duration::from_millis(1)).unwrap(),
            None
        );
        drop(sender);
        assert_eq!(receiver.try_receive(), Err(MailboxDisconnected));
    }
}
