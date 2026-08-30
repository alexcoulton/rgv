use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;

/// Identity of one retained CPU or GPU allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AllocationId(u64);

impl AllocationId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AllocationClass {
    CoreChunk,
    PayloadSidecar,
    OverlapIndex,
    EvidenceRootMetadata,
    ExactSummary,
    SummaryRootMetadata,
    AnnotationIndex,
    RowAtlas,
    SemanticPage,
    PresentationScene,
    GpuPage,
    TransitionFrame,
    NavigationHistory,
}

impl AllocationClass {
    const fn eviction_rank(self) -> u8 {
        match self {
            Self::GpuPage => 0,
            Self::SemanticPage => 1,
            Self::PresentationScene => 2,
            Self::RowAtlas => 3,
            Self::ExactSummary => 4,
            Self::SummaryRootMetadata => 5,
            Self::AnnotationIndex => 6,
            Self::OverlapIndex => 7,
            Self::EvidenceRootMetadata => 8,
            Self::PayloadSidecar => 9,
            Self::CoreChunk => 10,
            Self::NavigationHistory => 11,
            Self::TransitionFrame => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationKind {
    Normal,
    Transition,
}

/// Temporary memory which must be bounded while work is active but does not become retained
/// scene/cache state with an allocation identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TransientResourceClass {
    DecoderScratch,
    EvidenceSelection,
    ExactSummaryBuilder,
    CpuProjection,
    CpuSummaryProjection,
    SemanticPageBuilder,
    SummaryPageBuilder,
    AnnotationBuilder,
    GpuUpload,
    HttpBody,
    GpuDriver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransientResourceSnapshot {
    pub class: TransientResourceClass,
    pub current: usize,
    pub high_water: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationRecordSnapshot {
    pub id: AllocationId,
    pub class: AllocationClass,
    pub charged_capacity: usize,
    pub references: usize,
    pub pins: usize,
    pub last_used_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerSnapshot {
    pub budget: usize,
    pub transition_reserve: usize,
    pub transient_budget: usize,
    pub charged: usize,
    /// Capacity reserved for allocations which may become retained leases.
    pub reserved: usize,
    /// Temporary builder/decoder/upload memory currently reserved and separately reported.
    pub transient: usize,
    /// Maximum concurrent transient reservation observed by this ledger.
    pub transient_high_water: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
    pub records: Vec<AllocationRecordSnapshot>,
    pub transient_resources: Vec<TransientResourceSnapshot>,
}

#[derive(Debug)]
struct AllocationRecord {
    class: AllocationClass,
    charged_capacity: usize,
    references: usize,
    pins: usize,
    last_used_revision: u64,
}

#[derive(Debug)]
struct LedgerInner {
    budget: usize,
    transition_reserve: usize,
    transient_budget: usize,
    next_id: u64,
    charged: usize,
    reserved_normal: usize,
    reserved_transition: usize,
    transient_normal: usize,
    transient_transition: usize,
    transient_high_water: usize,
    transient_resources: HashMap<TransientResourceClass, TransientUsage>,
    records: HashMap<AllocationId, AllocationRecord>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TransientUsage {
    current: usize,
    high_water: usize,
}

impl LedgerInner {
    fn total_reserved(&self) -> usize {
        self.reserved_normal
            .saturating_add(self.reserved_transition)
    }

    fn total_transient(&self) -> usize {
        self.transient_normal
            .saturating_add(self.transient_transition)
    }

    fn reservation_limit(&self, kind: ReservationKind) -> usize {
        self.budget.saturating_add(match kind {
            ReservationKind::Normal => 0,
            ReservationKind::Transition => self.transition_reserve,
        })
    }

    fn transient_reservation_limit(&self, kind: ReservationKind) -> usize {
        self.transient_budget.saturating_add(match kind {
            ReservationKind::Normal => 0,
            ReservationKind::Transition => self.transition_reserve,
        })
    }

    fn reserved_mut(&mut self, kind: ReservationKind) -> &mut usize {
        match kind {
            ReservationKind::Normal => &mut self.reserved_normal,
            ReservationKind::Transition => &mut self.reserved_transition,
        }
    }

    fn transient_mut(&mut self, kind: ReservationKind) -> &mut usize {
        match kind {
            ReservationKind::Normal => &mut self.transient_normal,
            ReservationKind::Transition => &mut self.transient_transition,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AllocationLedger {
    inner: Arc<Mutex<LedgerInner>>,
}

impl AllocationLedger {
    #[must_use]
    pub fn new(budget: usize, transition_reserve: usize) -> Self {
        Self::new_with_transient_budget(budget, transition_reserve, budget)
    }

    #[must_use]
    pub fn new_with_transient_budget(
        budget: usize,
        transition_reserve: usize,
        transient_budget: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerInner {
                budget,
                transition_reserve,
                transient_budget,
                next_id: 1,
                charged: 0,
                reserved_normal: 0,
                reserved_transition: 0,
                transient_normal: 0,
                transient_transition: 0,
                transient_high_water: 0,
                transient_resources: HashMap::new(),
                records: HashMap::new(),
            })),
        }
    }

    pub fn reserve(
        &self,
        class: AllocationClass,
        maximum_bytes: usize,
        kind: ReservationKind,
    ) -> Result<AllocationReservation, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let requested_total = inner
            .charged
            .saturating_add(inner.total_reserved())
            .saturating_add(maximum_bytes);
        let limit = inner.reservation_limit(kind);
        if requested_total > limit {
            return Err(LedgerError::BudgetExceeded {
                requested: maximum_bytes,
                charged: inner.charged,
                reserved: inner.total_reserved(),
                limit,
            });
        }
        *inner.reserved_mut(kind) = inner.reserved_mut(kind).saturating_add(maximum_bytes);
        drop(inner);
        Ok(AllocationReservation {
            inner: Arc::clone(&self.inner),
            class,
            kind,
            remaining: maximum_bytes,
        })
    }

    /// Reserves temporary capacity before decoder, builder, transport, or upload work allocates
    /// it. Transient work has an enforcing ceiling separate from the retained cache budget, is
    /// reported independently, and disappears automatically when the guard is dropped.
    pub fn reserve_transient(
        &self,
        class: TransientResourceClass,
        maximum_bytes: usize,
        kind: ReservationKind,
    ) -> Result<TransientResourceReservation, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let requested_total = inner.total_transient().saturating_add(maximum_bytes);
        let limit = inner.transient_reservation_limit(kind);
        if requested_total > limit {
            return Err(LedgerError::TransientBudgetExceeded {
                class,
                requested: maximum_bytes,
                transient: inner.total_transient(),
                limit,
            });
        }
        *inner.transient_mut(kind) = inner.transient_mut(kind).saturating_add(maximum_bytes);
        let total_transient = inner.total_transient();
        inner.transient_high_water = inner.transient_high_water.max(total_transient);
        let usage = inner.transient_resources.entry(class).or_default();
        usage.current = usage.current.saturating_add(maximum_bytes);
        usage.high_water = usage.high_water.max(usage.current);
        drop(inner);
        Ok(TransientResourceReservation {
            inner: Arc::clone(&self.inner),
            class,
            kind,
            bytes: maximum_bytes,
        })
    }

    pub fn set_budget(&self, budget: usize) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        inner.budget = budget;
        Ok(())
    }

    pub fn set_transient_budget(&self, budget: usize) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        inner.transient_budget = budget;
        Ok(())
    }

    pub fn touch(&self, id: AllocationId, revision: u64) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let record = inner
            .records
            .get_mut(&id)
            .ok_or(LedgerError::UnknownAllocation(id))?;
        record.last_used_revision = revision;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<LedgerSnapshot, LedgerError> {
        let inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let mut records = inner
            .records
            .iter()
            .map(|(id, record)| AllocationRecordSnapshot {
                id: *id,
                class: record.class,
                charged_capacity: record.charged_capacity,
                references: record.references,
                pins: record.pins,
                last_used_revision: record.last_used_revision,
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.id);
        let pinned = records
            .iter()
            .filter(|record| record.pins > 0)
            .fold(0_usize, |total, record| {
                total.saturating_add(record.charged_capacity)
            });
        let mut transient_resources = inner
            .transient_resources
            .iter()
            .map(|(class, usage)| TransientResourceSnapshot {
                class: *class,
                current: usage.current,
                high_water: usage.high_water,
            })
            .collect::<Vec<_>>();
        transient_resources.sort_by_key(|usage| usage.class);
        Ok(LedgerSnapshot {
            budget: inner.budget,
            transition_reserve: inner.transition_reserve,
            transient_budget: inner.transient_budget,
            charged: inner.charged,
            reserved: inner.total_reserved(),
            transient: inner.total_transient(),
            transient_high_water: inner.transient_high_water,
            pinned,
            pinned_overage: pinned.saturating_sub(inner.budget),
            records,
            transient_resources,
        })
    }

    pub fn evictable_in_policy_order(&self) -> Result<Vec<AllocationId>, LedgerError> {
        let mut candidates = self
            .snapshot()?
            .records
            .into_iter()
            .filter(|record| record.pins == 0)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|record| {
            (
                record.class.eviction_rank(),
                record.last_used_revision,
                record.id,
            )
        });
        Ok(candidates.into_iter().map(|record| record.id).collect())
    }
}

/// RAII guard for separately reported temporary capacity. It cannot be converted into an
/// allocation lease: retained output must have its own [`AllocationReservation`].
#[derive(Debug)]
pub struct TransientResourceReservation {
    inner: Arc<Mutex<LedgerInner>>,
    class: TransientResourceClass,
    kind: ReservationKind,
    bytes: usize,
}

impl TransientResourceReservation {
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub const fn class(&self) -> TransientResourceClass {
        self.class
    }

    /// Reduces the reported live capacity after a builder releases part of its worst-case
    /// reservation. Historical high-water marks intentionally remain unchanged.
    pub fn shrink_to(&mut self, bytes: usize) -> Result<(), LedgerError> {
        if bytes > self.bytes {
            return Err(LedgerError::ReservationExceeded {
                requested: bytes,
                remaining: self.bytes,
            });
        }
        let released = self.bytes.saturating_sub(bytes);
        if released == 0 {
            return Ok(());
        }
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        *inner.transient_mut(self.kind) = inner.transient_mut(self.kind).saturating_sub(released);
        if let Some(usage) = inner.transient_resources.get_mut(&self.class) {
            usage.current = usage.current.saturating_sub(released);
        }
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for TransientResourceReservation {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            *inner.transient_mut(self.kind) =
                inner.transient_mut(self.kind).saturating_sub(self.bytes);
            if let Some(usage) = inner.transient_resources.get_mut(&self.class) {
                usage.current = usage.current.saturating_sub(self.bytes);
            }
        }
    }
}

/// Capacity held before performing work which would allocate memory.
#[derive(Debug)]
pub struct AllocationReservation {
    inner: Arc<Mutex<LedgerInner>>,
    class: AllocationClass,
    kind: ReservationKind,
    remaining: usize,
}

impl AllocationReservation {
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining
    }

    pub fn register(
        &mut self,
        charged_capacity: usize,
        last_used_revision: u64,
    ) -> Result<AllocationLease, LedgerError> {
        if charged_capacity > self.remaining {
            return Err(LedgerError::ReservationExceeded {
                requested: charged_capacity,
                remaining: self.remaining,
            });
        }
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let id = AllocationId(inner.next_id);
        inner.next_id = inner
            .next_id
            .checked_add(1)
            .ok_or(LedgerError::IdOverflow)?;
        inner.charged = inner.charged.saturating_add(charged_capacity);
        *inner.reserved_mut(self.kind) = inner
            .reserved_mut(self.kind)
            .saturating_sub(charged_capacity);
        inner.records.insert(
            id,
            AllocationRecord {
                class: self.class,
                charged_capacity,
                references: 1,
                pins: 0,
                last_used_revision,
            },
        );
        self.remaining = self.remaining.saturating_sub(charged_capacity);
        Ok(AllocationLease {
            inner: Arc::clone(&self.inner),
            id,
        })
    }
}

impl Drop for AllocationReservation {
    fn drop(&mut self) {
        if self.remaining == 0 {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            *inner.reserved_mut(self.kind) =
                inner.reserved_mut(self.kind).saturating_sub(self.remaining);
        }
    }
}

/// A reference to a charged allocation. Clones do not charge it again.
#[derive(Debug)]
pub struct AllocationLease {
    inner: Arc<Mutex<LedgerInner>>,
    id: AllocationId,
}

impl AllocationLease {
    #[must_use]
    pub const fn id(&self) -> AllocationId {
        self.id
    }

    pub(crate) fn ledger(&self) -> AllocationLedger {
        AllocationLedger {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn snapshot(&self) -> Result<AllocationRecordSnapshot, LedgerError> {
        let inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let record = inner
            .records
            .get(&self.id)
            .ok_or(LedgerError::UnknownAllocation(self.id))?;
        Ok(AllocationRecordSnapshot {
            id: self.id,
            class: record.class,
            charged_capacity: record.charged_capacity,
            references: record.references,
            pins: record.pins,
            last_used_revision: record.last_used_revision,
        })
    }

    pub fn pin(&self) -> Result<PinnedAllocationLease, LedgerError> {
        let lease = self.clone();
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let record = inner
            .records
            .get_mut(&self.id)
            .ok_or(LedgerError::UnknownAllocation(self.id))?;
        record.pins = record.pins.saturating_add(1);
        drop(inner);
        Ok(PinnedAllocationLease { lease })
    }
}

impl Clone for AllocationLease {
    fn clone(&self) -> Self {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(record) = inner.records.get_mut(&self.id)
        {
            record.references = record.references.saturating_add(1);
        }
        Self {
            inner: Arc::clone(&self.inner),
            id: self.id,
        }
    }
}

impl Drop for AllocationLease {
    fn drop(&mut self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some(record) = inner.records.get_mut(&self.id) else {
            return;
        };
        record.references = record.references.saturating_sub(1);
        if record.references == 0 {
            let charged_capacity = record.charged_capacity;
            inner.records.remove(&self.id);
            inner.charged = inner.charged.saturating_sub(charged_capacity);
        }
    }
}

/// A scene pin which makes an allocation ineligible for eviction.
#[derive(Debug)]
pub struct PinnedAllocationLease {
    lease: AllocationLease,
}

impl PinnedAllocationLease {
    #[must_use]
    pub const fn id(&self) -> AllocationId {
        self.lease.id
    }
}

impl Drop for PinnedAllocationLease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.lease.inner.lock()
            && let Some(record) = inner.records.get_mut(&self.lease.id)
        {
            record.pins = record.pins.saturating_sub(1);
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LedgerError {
    #[error(
        "allocation budget exceeded: requested {requested}, charged {charged}, reserved {reserved}, limit {limit}"
    )]
    BudgetExceeded {
        requested: usize,
        charged: usize,
        reserved: usize,
        limit: usize,
    },
    #[error(
        "transient {class:?} budget exceeded: requested {requested}, active {transient}, limit {limit}"
    )]
    TransientBudgetExceeded {
        class: TransientResourceClass,
        requested: usize,
        transient: usize,
        limit: usize,
    },
    #[error("allocation exceeds reservation: requested {requested}, remaining {remaining}")]
    ReservationExceeded { requested: usize, remaining: usize },
    #[error("unknown allocation {0:?}")]
    UnknownAllocation(AllocationId),
    #[error("allocation identity space exhausted")]
    IdOverflow,
    #[error("allocation ledger mutex was poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_allocation_stays_charged_until_its_last_live_lease_drops() {
        let ledger = AllocationLedger::new(1_000, 100);
        let mut reservation = ledger
            .reserve(AllocationClass::CoreChunk, 400, ReservationKind::Normal)
            .unwrap();
        let cache_lease = reservation.register(400, 1).unwrap();
        let scene_lease = cache_lease.clone();
        assert_eq!(ledger.snapshot().unwrap().charged, 400);
        assert_eq!(ledger.snapshot().unwrap().records[0].references, 2);

        drop(cache_lease);
        assert_eq!(ledger.snapshot().unwrap().charged, 400);
        assert_eq!(ledger.snapshot().unwrap().records[0].references, 1);
        drop(scene_lease);
        assert_eq!(ledger.snapshot().unwrap().charged, 0);
    }

    #[test]
    fn reservations_fail_before_candidate_allocation_changes_current_state() {
        let ledger = AllocationLedger::new(1_000, 100);
        let mut current_reservation = ledger
            .reserve(AllocationClass::CoreChunk, 800, ReservationKind::Normal)
            .unwrap();
        let current = current_reservation.register(800, 1).unwrap();

        let error = ledger
            .reserve(AllocationClass::CoreChunk, 300, ReservationKind::Normal)
            .unwrap_err();
        assert!(matches!(error, LedgerError::BudgetExceeded { .. }));
        assert_eq!(ledger.snapshot().unwrap().charged, 800);
        assert_eq!(ledger.snapshot().unwrap().records.len(), 1);
        drop(current);
    }

    #[test]
    fn transition_reserve_is_explicit_and_bounded() {
        let ledger = AllocationLedger::new(1_000, 200);
        let mut current_reservation = ledger
            .reserve(AllocationClass::SemanticPage, 900, ReservationKind::Normal)
            .unwrap();
        let _current = current_reservation.register(900, 1).unwrap();
        assert!(
            ledger
                .reserve(
                    AllocationClass::TransitionFrame,
                    250,
                    ReservationKind::Normal
                )
                .is_err()
        );
        let _transition = ledger
            .reserve(
                AllocationClass::TransitionFrame,
                200,
                ReservationKind::Transition,
            )
            .unwrap();
        assert_eq!(ledger.snapshot().unwrap().reserved, 200);
    }

    #[test]
    fn pinned_overage_is_reported_after_budget_reduction() {
        let ledger = AllocationLedger::new(1_000, 0);
        let mut reservation = ledger
            .reserve(AllocationClass::CoreChunk, 800, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(800, 1).unwrap();
        let _pin = lease.pin().unwrap();
        ledger.set_budget(500).unwrap();

        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.pinned, 800);
        assert_eq!(snapshot.pinned_overage, 300);
        assert!(
            !ledger
                .evictable_in_policy_order()
                .unwrap()
                .contains(&lease.id())
        );
    }

    #[test]
    fn transient_capacity_is_bounded_reported_and_released_without_a_lease() {
        let ledger = AllocationLedger::new(1_000, 100);
        let scratch = ledger
            .reserve_transient(
                TransientResourceClass::SemanticPageBuilder,
                600,
                ReservationKind::Normal,
            )
            .unwrap();
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(scratch.bytes(), 600);
        assert_eq!(snapshot.charged, 0);
        assert_eq!(snapshot.reserved, 0);
        assert_eq!(snapshot.transient, 600);
        assert_eq!(snapshot.transient_high_water, 600);
        assert_eq!(snapshot.transient_resources.len(), 1);
        assert_eq!(
            snapshot.transient_resources[0],
            TransientResourceSnapshot {
                class: TransientResourceClass::SemanticPageBuilder,
                current: 600,
                high_water: 600,
            }
        );
        let retained = ledger
            .reserve(
                AllocationClass::SemanticPage,
                1_000,
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(matches!(
            ledger.reserve_transient(
                TransientResourceClass::EvidenceSelection,
                401,
                ReservationKind::Normal,
            ),
            Err(LedgerError::TransientBudgetExceeded { .. })
        ));

        drop(scratch);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert_eq!(snapshot.transient_high_water, 600);
        assert_eq!(snapshot.transient_resources[0].current, 0);
        assert!(
            ledger
                .reserve_transient(
                    TransientResourceClass::SemanticPageBuilder,
                    1_000,
                    ReservationKind::Normal,
                )
                .is_ok()
        );
        drop(retained);
    }

    #[test]
    fn transient_classes_have_independent_and_aggregate_high_water_marks() {
        let ledger = AllocationLedger::new(1_000, 0);
        let evidence = ledger
            .reserve_transient(
                TransientResourceClass::EvidenceSelection,
                200,
                ReservationKind::Normal,
            )
            .unwrap();
        {
            let page = ledger
                .reserve_transient(
                    TransientResourceClass::SemanticPageBuilder,
                    300,
                    ReservationKind::Normal,
                )
                .unwrap();
            let snapshot = ledger.snapshot().unwrap();
            assert_eq!(snapshot.transient, 500);
            assert_eq!(snapshot.transient_high_water, 500);
            drop(page);
        }
        drop(evidence);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert_eq!(snapshot.transient_high_water, 500);
        assert_eq!(
            snapshot
                .transient_resources
                .iter()
                .find(|usage| usage.class == TransientResourceClass::EvidenceSelection)
                .unwrap()
                .high_water,
            200
        );
        assert_eq!(
            snapshot
                .transient_resources
                .iter()
                .find(|usage| usage.class == TransientResourceClass::SemanticPageBuilder)
                .unwrap()
                .high_water,
            300
        );
    }
}
