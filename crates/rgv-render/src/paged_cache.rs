//! Device-page cache shared by the paged renderers.
//!
//! The alignment-scene and alignment-summary renderers both keep immutable device pages in a
//! sorted page table, charge the table and every page to the shared allocation ledger, and
//! reclaim unpinned pages in deterministic least-recently-used order. This module owns that
//! store and its reclaim rules once. What a page *is* (its key, its buffers, how it is packed
//! and drawn) stays with each renderer behind [`CachedGpuPage`]; so do the replacement
//! transaction and camera capture, whose shapes differ between the two.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use rgv_alignment_scene::{
    AllocationClass, AllocationLease, AllocationLedger, LedgerError, LedgerProgress,
    ReservationKind,
};
use thiserror::Error;

use crate::{GpuReclaimGoal, GpuReclaimReport};

/// The page-specific half of the cache contract.
pub(crate) trait CachedGpuPage {
    type Key: Copy + Ord;

    fn key(&self) -> Self::Key;

    /// Renderer revision at which the page was last admitted or drawn; the LRU order.
    fn last_used_revision(&self) -> u64;

    fn set_last_used_revision(&mut self, revision: u64);

    /// Ledger charge of the page's device buffer. `None` for a resident page that needs no
    /// buffer; such a page holds no reclaimable capacity and is never an eviction candidate.
    fn allocation(&self) -> Option<&AllocationLease>;
}

/// One physical frame slot of a renderer's capture ring, as far as reclaim is concerned.
pub(crate) trait CachedFrameSlot {
    /// Retained CPU-frame capacity charged with the slot.
    fn frame_allocation(&self) -> &AllocationLease;

    /// Whether the slot belongs to the pre-charged capture ring and must survive an ordinary
    /// idle sweep.
    fn capture_ring(&self) -> bool;

    /// Ledger charges of the slot's device buffers.
    fn gpu_allocations(&self) -> [Option<&AllocationLease>; 2];
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum PageTableError {
    #[error("GPU page table capacity overflow")]
    CapacityOverflow,
    #[error("GPU page table needs {required} entries or bytes but only {actual} were provided")]
    Capacity { required: usize, actual: usize },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// Sorted page table, its ledger charge, and the online LRU reclaim authority.
#[derive(Debug)]
pub(crate) struct PagedGpuCache<P: CachedGpuPage> {
    /// Sorted by key.
    pub(crate) pages: Vec<P>,
    /// Online LRU authority. Keeping this index current when a page is admitted, touched, or
    /// retired removes the failure-time full page scan and sort from the render thread.
    pub(crate) reclaim_index: BTreeSet<(u64, P::Key)>,
    pub(crate) page_table_allocation: Option<AllocationLease>,
    pub(crate) page_table_capacity: usize,
}

impl<P: CachedGpuPage> Default for PagedGpuCache<P> {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            reclaim_index: BTreeSet::new(),
            page_table_allocation: None,
            page_table_capacity: 0,
        }
    }
}

/// Smallest power-of-two capacity holding `count` entries (at least one).
pub(crate) fn next_power_of_two_capacity(count: usize) -> Option<usize> {
    count.max(1).checked_next_power_of_two()
}

/// Conservative retained charge for a sorted page table of `capacity` entries and its online
/// B-tree reclaim authority. Four pointer words per key cover node links, occupancy slack, and
/// allocator metadata without depending on the standard library's private B-tree node layout.
pub(crate) fn page_table_retained_bytes<P: CachedGpuPage>(capacity: usize) -> Option<usize> {
    let page_table = capacity.checked_mul(size_of::<P>())?;
    let reclaim_entry =
        size_of::<(u64, P::Key)>().checked_add(4_usize.checked_mul(size_of::<usize>())?)?;
    page_table.checked_add(capacity.checked_mul(reclaim_entry)?)
}

pub(crate) fn shared_ledger_satisfies_goal(
    snapshot: &LedgerProgress,
    goal: GpuReclaimGoal,
) -> bool {
    goal.is_satisfied_at(snapshot.charged.saturating_add(snapshot.reserved))
}

impl<P: CachedGpuPage> PagedGpuCache<P> {
    /// Drops every page and the table itself, releasing their ledger charges.
    pub(crate) fn clear(&mut self) {
        self.pages = Vec::new();
        self.reclaim_index.clear();
        self.page_table_allocation = None;
        self.page_table_capacity = 0;
    }

    pub(crate) fn page_index(&self, key: P::Key) -> Option<usize> {
        self.pages
            .binary_search_by_key(&key, CachedGpuPage::key)
            .ok()
    }

    pub(crate) fn page(&self, key: P::Key) -> Option<&P> {
        self.page_index(key).map(|index| &self.pages[index])
    }

    pub(crate) fn page_mut(&mut self, key: P::Key) -> Option<&mut P> {
        self.page_index(key).map(|index| &mut self.pages[index])
    }

    pub(crate) fn rebuild_reclaim_index(&mut self) {
        self.reclaim_index.clear();
        self.reclaim_index.extend(
            self.pages
                .iter()
                .map(|page| (page.last_used_revision(), page.key())),
        );
        debug_assert_eq!(self.reclaim_index.len(), self.pages.len());
    }

    pub(crate) fn touch_page_at(&mut self, index: usize, revision: u64) {
        let page = &mut self.pages[index];
        self.reclaim_index
            .remove(&(page.last_used_revision(), page.key()));
        page.set_last_used_revision(revision);
        assert!(self.reclaim_index.insert((revision, page.key())));
    }

    /// Ledger charge of every page buffer and the page table.
    pub(crate) fn charged_bytes(&self) -> Result<usize, LedgerError> {
        let mut total = 0_usize;
        for page in &self.pages {
            if let Some(allocation) = page.allocation() {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
        }
        if let Some(allocation) = &self.page_table_allocation {
            total = total.saturating_add(allocation.snapshot()?.charged_capacity);
        }
        Ok(total)
    }

    /// Grows the page table to hold `required` entries, charging the larger table before the
    /// pages move into it. A table that already fits is left untouched.
    pub(crate) fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        reservation_kind: ReservationKind,
        revision: u64,
    ) -> Result<(), PageTableError> {
        if required <= self.page_table_capacity {
            return Ok(());
        }
        let requested_capacity =
            next_power_of_two_capacity(required).ok_or(PageTableError::CapacityOverflow)?;
        let maximum_bytes = requested_capacity
            .checked_mul(2)
            .and_then(page_table_retained_bytes::<P>)
            .ok_or(PageTableError::CapacityOverflow)?;
        let mut reservation =
            ledger.reserve(AllocationClass::GpuPage, maximum_bytes, reservation_kind)?;
        let mut candidate = Vec::with_capacity(requested_capacity);
        let actual_capacity = candidate.capacity();
        if actual_capacity < required {
            return Err(PageTableError::Capacity {
                required,
                actual: actual_capacity,
            });
        }
        let actual_bytes = page_table_retained_bytes::<P>(actual_capacity)
            .ok_or(PageTableError::CapacityOverflow)?;
        if actual_bytes > maximum_bytes {
            return Err(PageTableError::Capacity {
                required: maximum_bytes,
                actual: actual_bytes,
            });
        }
        let allocation = reservation.register(actual_bytes, revision)?;
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = actual_capacity;
        self.page_table_allocation = Some(allocation);
        Ok(())
    }

    /// Shrinks an unpinned page table that is at most a quarter full, or drops an empty one.
    /// Failing to reserve the smaller table is not an error: the larger one simply stays.
    pub(crate) fn reclaim_sparse_page_table(
        &mut self,
        ledger: &AllocationLedger,
        revision: u64,
    ) -> Result<(), LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(());
        };
        if allocation.snapshot()?.pins > 0 {
            return Ok(());
        }
        if self.pages.is_empty() {
            self.pages = Vec::new();
            self.page_table_allocation = None;
            self.page_table_capacity = 0;
            return Ok(());
        }
        let Some(requested_capacity) = self.pages.len().checked_next_power_of_two() else {
            return Ok(());
        };
        if requested_capacity.saturating_mul(4) > self.page_table_capacity {
            return Ok(());
        }
        let maximum_bytes = page_table_retained_bytes::<P>(requested_capacity.saturating_mul(2))
            .unwrap_or(usize::MAX);
        let Ok(mut reservation) = ledger.reserve(
            AllocationClass::GpuPage,
            maximum_bytes,
            ReservationKind::Transition,
        ) else {
            return Ok(());
        };
        let mut candidate = Vec::with_capacity(requested_capacity);
        if candidate.capacity() < self.pages.len() {
            return Ok(());
        }
        let actual_bytes =
            page_table_retained_bytes::<P>(candidate.capacity()).unwrap_or(usize::MAX);
        let Ok(allocation) = reservation.register(actual_bytes, revision) else {
            return Ok(());
        };
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = self.pages.capacity();
        self.page_table_allocation = Some(allocation);
        Ok(())
    }

    /// Walks the LRU index and collects unpinned, unprotected pages until `sufficient` accepts
    /// the capacity they would release. `keys_inspected` counts every index entry visited.
    pub(crate) fn indexed_unpinned_lru_candidates_until(
        &self,
        protected_pages: &[P::Key],
        keys_inspected: &AtomicU64,
        mut sufficient: impl FnMut(usize) -> bool,
    ) -> Result<Vec<P::Key>, LedgerError> {
        let mut protected = protected_pages.to_vec();
        protected.sort_unstable();
        protected.dedup();
        let mut candidates = Vec::new();
        let mut reclaimable_bytes = 0_usize;
        for (_last_used_revision, key) in &self.reclaim_index {
            keys_inspected.fetch_add(1, Ordering::Relaxed);
            if protected.binary_search(key).is_ok() {
                continue;
            }
            let page = self
                .page(*key)
                .expect("online reclaim authority contains only resident pages");
            let Some(allocation) = page.allocation() else {
                continue;
            };
            let allocation = allocation.snapshot()?;
            if allocation.pins == 0 {
                candidates.push(page.key());
                reclaimable_bytes = reclaimable_bytes.saturating_add(allocation.charged_capacity);
                if sufficient(reclaimable_bytes) {
                    break;
                }
            }
        }
        Ok(candidates)
    }

    /// How many pages may be removed before the page table becomes eligible to shrink, so that
    /// a reclaim loop can stop and release the table instead of overshooting its goal.
    pub(crate) fn removals_until_sparse_page_table_checkpoint(
        &self,
    ) -> Result<Option<usize>, LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(None);
        };
        if allocation.snapshot()?.pins > 0 || self.pages.is_empty() {
            return Ok(None);
        }
        let quarter_capacity = self.page_table_capacity / 4;
        if quarter_capacity == 0 {
            return Ok(Some(self.pages.len()));
        }
        let maximum_sparse_len = 1_usize
            .checked_shl(usize::BITS - quarter_capacity.leading_zeros() - 1)
            .unwrap_or(0);
        if self.pages.len() <= maximum_sparse_len {
            // The immediately preceding shrink attempt was already eligible but could not
            // reserve its replacement. Retry after one more page releases capacity; combining
            // more removals would be able to overshoot a goal that the table release satisfies.
            return Ok(Some(1));
        }
        Ok(Some(self.pages.len().saturating_sub(maximum_sparse_len)))
    }

    pub(crate) fn remove_reclaim_candidates(&mut self, candidates: &[P::Key]) {
        let mut keys = candidates.to_vec();
        keys.sort_unstable();
        let mut next_key = 0;
        self.pages.retain(|page| {
            while keys
                .get(next_key)
                .is_some_and(|candidate| *candidate < page.key())
            {
                next_key = next_key.saturating_add(1);
            }
            if keys.get(next_key).is_some_and(|key| *key == page.key()) {
                next_key = next_key.saturating_add(1);
                false
            } else {
                true
            }
        });
        self.rebuild_reclaim_index();
    }

    /// Removes LRU pages in table-shrink-sized batches until `satisfied` accepts the shared
    /// ledger, or no safe candidate remains. `sufficient(occupied, released)` decides when a
    /// batch plan already covers the goal.
    fn reclaim_pages_until(
        &mut self,
        ledger: &AllocationLedger,
        protected_pages: &[P::Key],
        revision: u64,
        keys_inspected: &AtomicU64,
        satisfied: impl Fn(&LedgerProgress) -> bool,
        sufficient: impl Fn(usize, usize) -> bool,
    ) -> Result<(), LedgerError> {
        loop {
            let snapshot = ledger.progress()?;
            if satisfied(&snapshot) {
                return Ok(());
            }
            let occupied = snapshot.charged.saturating_add(snapshot.reserved);
            let candidates = self.indexed_unpinned_lru_candidates_until(
                protected_pages,
                keys_inspected,
                |released| sufficient(occupied, released),
            )?;
            if candidates.is_empty() {
                return Ok(());
            }
            let checkpoint = self
                .removals_until_sparse_page_table_checkpoint()?
                .unwrap_or(candidates.len());
            let remove = checkpoint.min(candidates.len());
            if remove == 0 {
                return Ok(());
            }
            self.remove_reclaim_candidates(&candidates[..remove]);
            self.reclaim_sparse_page_table(ledger, revision)?;
        }
    }

    /// Evicts unpinned pages, never those in `protected`, until `required` more bytes fit under
    /// `limit` in the shared ledger or nothing safe remains. The caller drops its idle frame
    /// slots first.
    pub(crate) fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        limit: usize,
        protected: &[P::Key],
        revision: u64,
        keys_inspected: &AtomicU64,
    ) -> Result<(), LedgerError> {
        self.reclaim_sparse_page_table(ledger, revision)?;
        self.reclaim_pages_until(
            ledger,
            protected,
            revision,
            keys_inspected,
            |snapshot| {
                snapshot
                    .charged
                    .saturating_add(snapshot.reserved)
                    .saturating_add(required)
                    <= limit
            },
            |occupied, released| {
                occupied.saturating_sub(released).saturating_add(required) <= limit
            },
        )
    }
}

/// Drops idle frame slots that are not part of the pre-charged capture ring.
pub(crate) fn reclaim_idle_frame_slots<S: CachedFrameSlot>(slots: &mut Vec<Arc<S>>) {
    slots.retain(|slot| slot.capture_ring() || Arc::strong_count(slot) > 1);
}

/// Drops every frame slot no in-flight frame still references.
pub(crate) fn reclaim_all_idle_frame_slots<S: CachedFrameSlot>(slots: &mut Vec<Arc<S>>) {
    slots.retain(|slot| Arc::strong_count(slot) > 1);
}

/// Device-buffer charge of a renderer: pages, page table and frame-slot buffers.
pub(crate) fn resident_gpu_bytes<P: CachedGpuPage, S: CachedFrameSlot>(
    cache: &PagedGpuCache<P>,
    slots: &[Arc<S>],
) -> Result<usize, LedgerError> {
    let mut total = cache.charged_bytes()?;
    for slot in slots {
        for allocation in slot.gpu_allocations().into_iter().flatten() {
            total = total.saturating_add(allocation.snapshot()?.charged_capacity);
        }
    }
    Ok(total)
}

/// Everything a renderer retains in the shared ledger: device buffers plus frame capacity.
pub(crate) fn resident_retained_bytes<P: CachedGpuPage, S: CachedFrameSlot>(
    cache: &PagedGpuCache<P>,
    slots: &[Arc<S>],
) -> Result<usize, LedgerError> {
    let mut total = resident_gpu_bytes(cache, slots)?;
    for slot in slots {
        total = total.saturating_add(slot.frame_allocation().snapshot()?.charged_capacity);
    }
    Ok(total)
}

/// Cooperatively releases renderer-local retained capacity until the exact failed shared-ledger
/// request fits its original limit or no safe candidate remains. This preserves transition-
/// reserve semantics when the current charged total is legitimately above the base budget. Idle
/// frame-ring slots are dropped first, followed by an unpinned sparse page table and then
/// unpinned pages from the online deterministic `(last_used_revision, page_key)` index. Planning
/// stops once the exact goal is covered; it never rebuilds or sorts a full failure-time page
/// inventory. `protected_pages` are never evicted by this pass; active frames and residency
/// leases are independently protected by ledger pins.
pub(crate) fn reclaim_for_goal<P: CachedGpuPage, S: CachedFrameSlot>(
    cache: &mut PagedGpuCache<P>,
    slots: &mut Vec<Arc<S>>,
    ledger: &AllocationLedger,
    goal: GpuReclaimGoal,
    protected_pages: &[P::Key],
    revision: u64,
    keys_inspected: &AtomicU64,
) -> Result<GpuReclaimReport, LedgerError> {
    let ledger_before = ledger.progress()?;
    let pages_before = cache.pages.len();
    let retained_bytes_before = resident_retained_bytes(cache, slots)?;

    if goal.is_feasible() && !shared_ledger_satisfies_goal(&ledger_before, goal) {
        reclaim_all_idle_frame_slots(slots);
        cache.reclaim_sparse_page_table(ledger, revision)?;
        cache.reclaim_pages_until(
            ledger,
            protected_pages,
            revision,
            keys_inspected,
            |snapshot| shared_ledger_satisfies_goal(snapshot, goal),
            |occupied, released| goal.is_satisfied_at(occupied.saturating_sub(released)),
        )?;
    }

    let retained_bytes_after = resident_retained_bytes(cache, slots)?;
    let ledger_after = ledger.progress()?;
    Ok(GpuReclaimReport {
        pressure: goal.pressure,
        baseline_occupied_bytes: goal.baseline_occupied_bytes,
        pages_before,
        pages_after: cache.pages.len(),
        retained_bytes_before,
        retained_bytes_after,
        ledger_charged_before: ledger_before.charged,
        ledger_charged_after: ledger_after.charged,
        headroom_satisfied: shared_ledger_satisfies_goal(&ledger_after, goal),
    })
}

#[cfg(test)]
mod tests {
    use rgv_alignment_scene::PinnedAllocationLease;

    use super::*;
    use crate::GpuBudgetPressure;

    const PAGE_BYTES: usize = 4_096;

    #[derive(Debug)]
    struct Page {
        key: u32,
        last_used_revision: u64,
        allocation: Option<AllocationLease>,
    }

    impl CachedGpuPage for Page {
        type Key = u32;

        fn key(&self) -> u32 {
            self.key
        }

        fn last_used_revision(&self) -> u64 {
            self.last_used_revision
        }

        fn set_last_used_revision(&mut self, revision: u64) {
            self.last_used_revision = revision;
        }

        fn allocation(&self) -> Option<&AllocationLease> {
            self.allocation.as_ref()
        }
    }

    struct Slot {
        frame: AllocationLease,
        capture_ring: bool,
    }

    impl CachedFrameSlot for Slot {
        fn frame_allocation(&self) -> &AllocationLease {
            &self.frame
        }

        fn capture_ring(&self) -> bool {
            self.capture_ring
        }

        fn gpu_allocations(&self) -> [Option<&AllocationLease>; 2] {
            [None, None]
        }
    }

    fn lease(ledger: &AllocationLedger, bytes: usize) -> AllocationLease {
        ledger
            .reserve(AllocationClass::GpuPage, bytes, ReservationKind::Normal)
            .unwrap()
            .register(bytes, 1)
            .unwrap()
    }

    /// Pages `0..count`, page `n` last used at revision `10 + n`; `bufferless` pages carry no
    /// device allocation.
    fn cache(ledger: &AllocationLedger, count: u32, bufferless: &[u32]) -> PagedGpuCache<Page> {
        let mut cache = PagedGpuCache::default();
        cache
            .ensure_page_table_capacity(ledger, count as usize, ReservationKind::Normal, 1)
            .unwrap();
        for key in 0..count {
            cache.pages.push(Page {
                key,
                last_used_revision: 10 + u64::from(key),
                allocation: (!bufferless.contains(&key)).then(|| lease(ledger, PAGE_BYTES)),
            });
        }
        cache.rebuild_reclaim_index();
        cache
    }

    fn candidates(
        cache: &PagedGpuCache<Page>,
        protected: &[u32],
        enough: usize,
    ) -> (Vec<u32>, u64) {
        let inspected = AtomicU64::new(0);
        let keys = cache
            .indexed_unpinned_lru_candidates_until(protected, &inspected, |released| {
                released >= enough
            })
            .unwrap();
        (keys, inspected.load(Ordering::Relaxed))
    }

    #[test]
    fn lookup_and_lru_order_follow_touches() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let mut cache = cache(&ledger, 4, &[]);
        assert_eq!(cache.page_index(2), Some(2));
        assert_eq!(cache.page(3).map(|page| page.key), Some(3));
        assert!(cache.page(4).is_none());
        assert!(cache.page_mut(1).is_some());
        assert_eq!(candidates(&cache, &[], usize::MAX).0, [0, 1, 2, 3]);

        // Touching the oldest page makes it the newest.
        cache.touch_page_at(0, 99);
        assert_eq!(cache.pages[0].last_used_revision, 99);
        assert_eq!(cache.reclaim_index.len(), 4);
        assert_eq!(candidates(&cache, &[], usize::MAX).0, [1, 2, 3, 0]);
    }

    #[test]
    fn candidates_skip_protected_pinned_and_bufferless_pages_and_stop_when_sufficient() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let cache = cache(&ledger, 6, &[1]);
        let _pin: PinnedAllocationLease =
            cache.pages[2].allocation.as_ref().unwrap().pin().unwrap();

        // 0 is protected, 1 has no buffer, 2 is pinned: 3 and 4 cover two pages' worth.
        let (keys, inspected) = candidates(&cache, &[0, 0], 2 * PAGE_BYTES);
        assert_eq!(keys, [3, 4]);
        assert_eq!(
            inspected, 5,
            "the walk stops at the page that satisfied the goal"
        );
        // An unreachable goal returns every safe candidate.
        assert_eq!(candidates(&cache, &[0], usize::MAX).0, [3, 4, 5]);
    }

    #[test]
    fn removing_candidates_keeps_the_table_sorted_and_the_index_exact() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let mut cache = cache(&ledger, 6, &[]);
        let charged = ledger.progress().unwrap().charged;
        cache.remove_reclaim_candidates(&[4, 1, 7]);
        assert_eq!(
            cache.pages.iter().map(|page| page.key).collect::<Vec<_>>(),
            [0, 2, 3, 5]
        );
        assert_eq!(cache.reclaim_index.len(), 4);
        assert_eq!(
            ledger.progress().unwrap().charged,
            charged - 2 * PAGE_BYTES,
            "dropping a page releases exactly its charge"
        );
    }

    #[test]
    fn page_table_grows_once_is_charged_and_shrinks_when_sparse() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let mut cache = cache(&ledger, 16, &[]);
        assert_eq!(cache.page_table_capacity, 16);
        let table_bytes = cache
            .page_table_allocation
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .charged_capacity;
        assert_eq!(table_bytes, page_table_retained_bytes::<Page>(16).unwrap());
        assert_eq!(
            cache.charged_bytes().unwrap(),
            table_bytes + 16 * PAGE_BYTES
        );
        // Already large enough: nothing is reserved.
        let charged = ledger.progress().unwrap().charged;
        cache
            .ensure_page_table_capacity(&ledger, 9, ReservationKind::Normal, 2)
            .unwrap();
        assert_eq!(ledger.progress().unwrap().charged, charged);

        // A table more than a quarter full keeps its capacity.
        cache.remove_reclaim_candidates(&(5..16).collect::<Vec<_>>());
        cache.reclaim_sparse_page_table(&ledger, 3).unwrap();
        assert_eq!(cache.page_table_capacity, 16);
        // At a quarter full it shrinks to the next power of two that holds the pages.
        cache.remove_reclaim_candidates(&[4]);
        assert_eq!(
            cache.removals_until_sparse_page_table_checkpoint().unwrap(),
            Some(1)
        );
        cache.reclaim_sparse_page_table(&ledger, 3).unwrap();
        assert_eq!(cache.pages.len(), 4);
        assert!(cache.page_table_capacity < 16);
        assert!(cache.page_table_capacity >= 4);
        // An empty table is dropped entirely.
        cache.remove_reclaim_candidates(&[0, 1, 2, 3]);
        cache.reclaim_sparse_page_table(&ledger, 4).unwrap();
        assert_eq!(cache.page_table_capacity, 0);
        assert!(cache.page_table_allocation.is_none());
        assert_eq!(cache.charged_bytes().unwrap(), 0);
        assert_eq!(ledger.progress().unwrap().charged, 0);
    }

    #[test]
    fn reclaim_for_goal_drops_idle_slots_then_lru_pages_until_the_request_fits() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let mut cache = cache(&ledger, 8, &[]);
        let idle = Arc::new(Slot {
            frame: lease(&ledger, 1_000),
            capture_ring: true,
        });
        let in_flight = Arc::new(Slot {
            frame: lease(&ledger, 1_000),
            capture_ring: false,
        });
        let _frame = Arc::clone(&in_flight);
        let mut slots = vec![idle, in_flight];

        // Ordinary idle sweeps keep the capture ring.
        reclaim_idle_frame_slots(&mut slots);
        assert_eq!(slots.len(), 2);

        let snapshot = ledger.progress().unwrap();
        let occupied = snapshot.charged + snapshot.reserved;
        // A request that needs the idle slot's 1000 bytes plus two and a bit pages.
        let requested = 1_000 + 2 * PAGE_BYTES + 100;
        let goal = GpuReclaimGoal::new(
            GpuBudgetPressure::from_budget_failure(requested, occupied, 0, occupied),
            occupied,
        );
        assert!(goal.is_feasible());
        let inspected = AtomicU64::new(0);
        let report =
            reclaim_for_goal(&mut cache, &mut slots, &ledger, goal, &[0], 5, &inspected).unwrap();

        assert!(report.headroom_satisfied);
        assert_eq!(
            slots.len(),
            1,
            "the idle slot goes first; the in-flight one stays"
        );
        assert_eq!(report.pages_before, 8);
        // Page 0 is protected, so the three oldest unprotected pages are released.
        assert_eq!(
            cache.pages.iter().map(|page| page.key).collect::<Vec<_>>(),
            [0, 4, 5, 6, 7]
        );
        assert_eq!(report.pages_after, 5);
        assert_eq!(report.reclaimed_pages(), 3);
        assert!(report.released_retained_bytes() >= requested);
        assert_eq!(
            report.released_retained_bytes(),
            report.released_ledger_bytes(),
            "this cache is the only owner in the ledger"
        );
        assert!(inspected.load(Ordering::Relaxed) >= 4);

        // An already satisfied goal changes nothing.
        let again =
            reclaim_for_goal(&mut cache, &mut slots, &ledger, goal, &[], 6, &inspected).unwrap();
        assert_eq!(again.reclaimed_pages(), 0);
        assert_eq!(again.released_retained_bytes(), 0);
    }

    #[test]
    fn eviction_for_a_request_stops_as_soon_as_it_fits_under_the_limit() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 18);
        let mut cache = cache(&ledger, 8, &[]);
        let occupied = ledger.progress().unwrap().charged;
        let inspected = AtomicU64::new(0);
        // Room for one more page is needed under a limit equal to the current occupancy.
        cache
            .evict_unpinned_pages_for_excluding(&ledger, PAGE_BYTES, occupied, &[0], 7, &inspected)
            .unwrap();
        assert_eq!(
            cache.pages.iter().map(|page| page.key).collect::<Vec<_>>(),
            [0, 2, 3, 4, 5, 6, 7]
        );
        // Nothing is evicted for a request that already fits.
        cache
            .evict_unpinned_pages_for_excluding(&ledger, PAGE_BYTES, occupied, &[], 8, &inspected)
            .unwrap();
        assert_eq!(cache.pages.len(), 7);
        cache.clear();
        assert_eq!(ledger.progress().unwrap().charged, 0);
    }
}
