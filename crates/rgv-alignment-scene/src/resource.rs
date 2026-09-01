use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
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

/// Reuse priority carried by mutable cache ownership. Protection is deliberately orthogonal:
/// committed/candidate scenes retain immutable pins while this class answers only why a cache
/// owner should keep its reloadable lookup reference. Variant order is eviction order.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentCacheResidencyClass {
    /// Work prepared without a current visible demand (for example whole-stack warming).
    DisposableSpeculation,
    /// A disjoint previous working-set island. It may use only residual capacity.
    RecentLocusReturnCache,
    /// Same-locus pages outside the current horizontal/vertical working buffer.
    ActiveLocusCold,
    #[default]
    /// Pages inside the configured nearby pan/scroll buffer.
    ActiveNearBuffer,
    /// Exact pages required by the current camera or its in-flight successor.
    ForegroundVisible,
}

/// Byte-weighted cache ownership inventory. An allocation shared by several cache entries is
/// assigned once, to the strongest residency class which references it. Entry counts remain
/// useful complexity telemetry but never drive capacity policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentCacheResidencyInventory {
    pub disposable_speculation_entries: usize,
    pub disposable_speculation_allocations: usize,
    pub disposable_speculation_bytes: usize,
    pub recent_locus_return_cache_entries: usize,
    pub recent_locus_return_cache_allocations: usize,
    pub recent_locus_return_cache_bytes: usize,
    pub active_locus_cold_entries: usize,
    pub active_locus_cold_allocations: usize,
    pub active_locus_cold_bytes: usize,
    pub active_near_buffer_entries: usize,
    pub active_near_buffer_allocations: usize,
    pub active_near_buffer_bytes: usize,
    pub foreground_visible_entries: usize,
    pub foreground_visible_allocations: usize,
    pub foreground_visible_bytes: usize,
    /// Distinct bytes which currently have at least one committed/candidate pin.
    pub pinned_bytes: usize,
    /// Distinct bytes with no committed/candidate pin. Other shared object owners may still keep
    /// them charged after this cache reference yields, so physical release remains ledger-proven.
    pub unpinned_bytes: usize,
}

impl AlignmentCacheResidencyInventory {
    pub(crate) fn observe_entry(&mut self, residency: AlignmentCacheResidencyClass) {
        match residency {
            AlignmentCacheResidencyClass::DisposableSpeculation => {
                self.disposable_speculation_entries =
                    self.disposable_speculation_entries.saturating_add(1);
            }
            AlignmentCacheResidencyClass::RecentLocusReturnCache => {
                self.recent_locus_return_cache_entries =
                    self.recent_locus_return_cache_entries.saturating_add(1);
            }
            AlignmentCacheResidencyClass::ActiveLocusCold => {
                self.active_locus_cold_entries = self.active_locus_cold_entries.saturating_add(1);
            }
            AlignmentCacheResidencyClass::ActiveNearBuffer => {
                self.active_near_buffer_entries = self.active_near_buffer_entries.saturating_add(1);
            }
            AlignmentCacheResidencyClass::ForegroundVisible => {
                self.foreground_visible_entries = self.foreground_visible_entries.saturating_add(1);
            }
        }
    }

    pub(crate) fn observe_distinct_allocation(
        &mut self,
        residency: AlignmentCacheResidencyClass,
        allocation: AllocationRecordSnapshot,
    ) {
        let (allocations, bytes) = match residency {
            AlignmentCacheResidencyClass::DisposableSpeculation => (
                &mut self.disposable_speculation_allocations,
                &mut self.disposable_speculation_bytes,
            ),
            AlignmentCacheResidencyClass::RecentLocusReturnCache => (
                &mut self.recent_locus_return_cache_allocations,
                &mut self.recent_locus_return_cache_bytes,
            ),
            AlignmentCacheResidencyClass::ActiveLocusCold => (
                &mut self.active_locus_cold_allocations,
                &mut self.active_locus_cold_bytes,
            ),
            AlignmentCacheResidencyClass::ActiveNearBuffer => (
                &mut self.active_near_buffer_allocations,
                &mut self.active_near_buffer_bytes,
            ),
            AlignmentCacheResidencyClass::ForegroundVisible => (
                &mut self.foreground_visible_allocations,
                &mut self.foreground_visible_bytes,
            ),
        };
        *allocations = allocations.saturating_add(1);
        *bytes = bytes.saturating_add(allocation.charged_capacity);
        if allocation.pins > 0 {
            self.pinned_bytes = self
                .pinned_bytes
                .saturating_add(allocation.charged_capacity);
        } else {
            self.unpinned_bytes = self
                .unpinned_bytes
                .saturating_add(allocation.charged_capacity);
        }
    }

    pub fn merge(&mut self, other: Self) {
        macro_rules! add {
            ($field:ident) => {
                self.$field = self.$field.saturating_add(other.$field);
            };
        }
        add!(disposable_speculation_entries);
        add!(disposable_speculation_allocations);
        add!(disposable_speculation_bytes);
        add!(recent_locus_return_cache_entries);
        add!(recent_locus_return_cache_allocations);
        add!(recent_locus_return_cache_bytes);
        add!(active_locus_cold_entries);
        add!(active_locus_cold_allocations);
        add!(active_locus_cold_bytes);
        add!(active_near_buffer_entries);
        add!(active_near_buffer_allocations);
        add!(active_near_buffer_bytes);
        add!(foreground_visible_entries);
        add!(foreground_visible_allocations);
        add!(foreground_visible_bytes);
        add!(pinned_bytes);
        add!(unpinned_bytes);
    }

    #[must_use]
    pub const fn distinct_bytes(self) -> usize {
        self.disposable_speculation_bytes
            .saturating_add(self.recent_locus_return_cache_bytes)
            .saturating_add(self.active_locus_cold_bytes)
            .saturating_add(self.active_near_buffer_bytes)
            .saturating_add(self.foreground_visible_bytes)
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
    const COUNT: usize = 13;

    const fn index(self) -> usize {
        self as usize
    }

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

    const fn is_gpu_transition_allocation(self) -> bool {
        matches!(self, Self::GpuPage | Self::TransitionFrame)
    }
}

/// O(1) retained-byte reconciliation by allocation class. The fixed array is internal so adding
/// a class cannot silently change a public positional contract; callers query it with the typed
/// enum and may use the grouped helpers for diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LedgerRetainedBreakdown {
    by_class: [usize; AllocationClass::COUNT],
}

impl LedgerRetainedBreakdown {
    #[must_use]
    pub const fn bytes(self, class: AllocationClass) -> usize {
        self.by_class[class.index()]
    }

    #[must_use]
    pub fn evidence_bytes(self) -> usize {
        [
            AllocationClass::CoreChunk,
            AllocationClass::PayloadSidecar,
            AllocationClass::OverlapIndex,
            AllocationClass::EvidenceRootMetadata,
        ]
        .into_iter()
        .fold(0_usize, |total, class| {
            total.saturating_add(self.bytes(class))
        })
    }

    #[must_use]
    pub fn summary_bytes(self) -> usize {
        [
            AllocationClass::ExactSummary,
            AllocationClass::SummaryRootMetadata,
            AllocationClass::AnnotationIndex,
        ]
        .into_iter()
        .fold(0_usize, |total, class| {
            total.saturating_add(self.bytes(class))
        })
    }

    #[must_use]
    pub const fn row_atlas_bytes(self) -> usize {
        self.bytes(AllocationClass::RowAtlas)
    }

    #[must_use]
    pub fn semantic_scene_bytes(self) -> usize {
        [
            AllocationClass::SemanticPage,
            AllocationClass::PresentationScene,
            AllocationClass::NavigationHistory,
        ]
        .into_iter()
        .fold(0_usize, |total, class| {
            total.saturating_add(self.bytes(class))
        })
    }

    #[must_use]
    pub fn gpu_bytes(self) -> usize {
        self.bytes(AllocationClass::GpuPage)
            .saturating_add(self.bytes(AllocationClass::TransitionFrame))
    }

    #[must_use]
    pub fn total(self) -> usize {
        self.by_class
            .into_iter()
            .fold(0_usize, usize::saturating_add)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationKind {
    Normal,
    Transition,
}

/// Caller-defined identity for one retained allocation in an atomic foreground candidate.
/// Slots make it possible to attach the leases returned at settlement to the GPU objects that
/// were staged under the corresponding demand without relying on vector position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AllocationBundleSlot(u32);

impl AllocationBundleSlot {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Conservative retained capacity for one allocation staged as part of a foreground candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationBundleDemand {
    pub slot: AllocationBundleSlot,
    pub class: AllocationClass,
    pub maximum_bytes: usize,
}

impl AllocationBundleDemand {
    #[must_use]
    pub const fn new(
        slot: AllocationBundleSlot,
        class: AllocationClass,
        maximum_bytes: usize,
    ) -> Self {
        Self {
            slot,
            class,
            maximum_bytes,
        }
    }
}

/// One ledger-minted component of an exact retirement proof. Production callers cannot construct
/// a claim from an allocation ID or a numeric reference count; they must present the actual owned
/// leases to [`AllocationRetirementClaimBuilder`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationRetirementClaim {
    id: AllocationId,
    owned_references: usize,
}

impl AllocationRetirementClaim {
    #[must_use]
    pub const fn id(self) -> AllocationId {
        self.id
    }

    #[must_use]
    pub const fn owned_references(self) -> usize {
        self.owned_references
    }
}

/// Opaque, ledger-bound proof that one owning retirement token currently contains every live lease
/// reference for each claimed allocation. The set may be cloned into an allocation-bundle spec,
/// but its claims cannot be altered or rebound to another ledger.
#[derive(Clone, Debug)]
pub struct AllocationRetirementClaimSet {
    ledger: Weak<Mutex<LedgerInner>>,
    claims: Vec<AllocationRetirementClaim>,
}

impl AllocationRetirementClaimSet {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.claims.len()
    }

    fn belongs_to(&self, ledger: &Arc<Mutex<LedgerInner>>) -> bool {
        Weak::ptr_eq(&self.ledger, &Arc::downgrade(ledger))
    }
}

impl PartialEq for AllocationRetirementClaimSet {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.ledger, &other.ledger) && self.claims == other.claims
    }
}

impl Eq for AllocationRetirementClaimSet {}

/// Collects borrowed references from a renderer-owned, non-cloneable retirement token. Holding
/// the borrows until `finish` prevents a lease object from being moved and counted twice; the
/// ledger also rejects the same physical reference twice and revalidates the aggregate reference
/// count under its mutex.
pub struct AllocationRetirementClaimBuilder<'a> {
    ledger: &'a AllocationLedger,
    references: Vec<&'a AllocationLease>,
}

impl<'a> AllocationRetirementClaimBuilder<'a> {
    pub fn include_lease(
        &mut self,
        lease: &'a AllocationLease,
    ) -> Result<(), AllocationBundleError> {
        self.include_reference(lease)
    }

    pub fn include_pin(
        &mut self,
        pin: &'a PinnedAllocationLease,
    ) -> Result<(), AllocationBundleError> {
        self.include_reference(&pin.lease)
    }

    fn include_reference(
        &mut self,
        lease: &'a AllocationLease,
    ) -> Result<(), AllocationBundleError> {
        if !Arc::ptr_eq(&self.ledger.inner, &lease.inner) {
            return Err(AllocationBundleError::ForeignRetirementReference { id: lease.id });
        }
        if self
            .references
            .iter()
            .any(|existing| std::ptr::eq(*existing, lease))
        {
            return Err(AllocationBundleError::DuplicateRetirementReference(
                lease.id,
            ));
        }
        self.references.push(lease);
        Ok(())
    }

    pub fn finish(self) -> Result<AllocationRetirementClaimSet, AllocationBundleError> {
        self.finish_with_policy(RetirementClaimSelectionPolicy::RequireEveryReference)
    }

    /// Produces the maximal exact claim set from a recursively owned epoch graph.
    ///
    /// A detached semantic epoch can own pins for an allocation whose immutable object is still
    /// shared by a current source root or cache. Such a pin will disappear on commit, but the
    /// allocation itself will not, so it must not receive retirement credit. Callers add every
    /// physical reference which their move-only token truly owns; this method atomically keeps
    /// only allocation identities for which those references are the complete live set.
    ///
    /// This is deliberately different from [`Self::finish`], whose strict mismatch is useful for
    /// renderer tokens that are expected to own a closed allocation set. Unknown, foreign,
    /// duplicate, or already-claimed references remain errors in both modes.
    pub(crate) fn finish_maximal_exact(
        self,
    ) -> Result<AllocationRetirementClaimSet, AllocationBundleError> {
        self.finish_with_policy(RetirementClaimSelectionPolicy::KeepCompleteAllocations)
    }

    fn finish_with_policy(
        self,
        policy: RetirementClaimSelectionPolicy,
    ) -> Result<AllocationRetirementClaimSet, AllocationBundleError> {
        let mut ids = self
            .references
            .iter()
            .map(|lease| lease.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        let inner = self
            .ledger
            .inner
            .lock()
            .map_err(|_| LedgerError::Poisoned)?;
        let mut claims = Vec::new();
        let mut start = 0;
        while start < ids.len() {
            let id = ids[start];
            let end = ids[start..]
                .iter()
                .position(|candidate| *candidate != id)
                .map_or(ids.len(), |offset| start + offset);
            let owned_references = end - start;
            let record = inner
                .records
                .get(&id)
                .ok_or(LedgerError::UnknownAllocation(id))?;
            if record.retirement_claim.is_some() {
                return Err(AllocationBundleError::RetirementAlreadyClaimed(id));
            }
            if record.references != owned_references {
                match policy {
                    RetirementClaimSelectionPolicy::RequireEveryReference => {
                        return Err(AllocationBundleError::RetirementReferenceMismatch {
                            id,
                            expected: owned_references,
                            actual: record.references,
                        });
                    }
                    RetirementClaimSelectionPolicy::KeepCompleteAllocations => {
                        start = end;
                        continue;
                    }
                }
            }
            claims.push(AllocationRetirementClaim {
                id,
                owned_references,
            });
            start = end;
        }
        drop(inner);
        Ok(AllocationRetirementClaimSet {
            ledger: Arc::downgrade(&self.ledger.inner),
            claims,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetirementClaimSelectionPolicy {
    RequireEveryReference,
    KeepCompleteAllocations,
}

/// Implemented by renderer-owned, non-cloneable objects which retain the exact old presentation
/// resources credited by a replacement bundle. Settlement checks this opaque authority before it
/// consumes and drops the token.
pub trait AllocationRetirementToken {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet;
}

/// Immutable input to one atomic replacement reservation. `policy_revision` must come from the
/// ledger snapshot used to construct the renderer plans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationBundleSpec {
    pub policy_revision: u64,
    pub demands: Vec<AllocationBundleDemand>,
    pub retiring: AllocationRetirementClaimSet,
}

impl AllocationBundleSpec {
    #[must_use]
    pub fn new(
        policy_revision: u64,
        demands: Vec<AllocationBundleDemand>,
        retiring: AllocationRetirementClaimSet,
    ) -> Self {
        Self {
            policy_revision,
            demands,
            retiring,
        }
    }
}

/// Actual retained capacity produced for one bundle slot after staging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationBundleActual {
    pub slot: AllocationBundleSlot,
    pub charged_capacity: usize,
    pub last_used_revision: u64,
}

impl AllocationBundleActual {
    #[must_use]
    pub const fn new(
        slot: AllocationBundleSlot,
        charged_capacity: usize,
        last_used_revision: u64,
    ) -> Self {
        Self {
            slot,
            charged_capacity,
            last_used_revision,
        }
    }
}

/// One settled allocation paired with the caller-defined slot that requested it.
#[derive(Debug)]
pub struct AllocationBundleLease {
    pub slot: AllocationBundleSlot,
    pub allocation: AllocationLease,
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
    /// Capacity-mutation revision shared with [`LedgerProgress`].
    pub progress_revision: u64,
    /// Changes whenever a retained-budget policy changes. Candidate reservations capture this
    /// value so a concurrent budget edit cannot publish work proved under an obsolete limit.
    pub policy_revision: u64,
    pub budget: usize,
    pub transition_reserve: usize,
    pub transient_budget: usize,
    pub charged: usize,
    /// Capacity reserved for allocations which may become retained leases.
    pub reserved: usize,
    /// Base-budget reservations held by ordinary work.
    pub reserved_normal: usize,
    /// Old/new-overlap reservations, including an active replacement bundle.
    pub reserved_transition: usize,
    /// The single-flight retained GPU replacement, if one currently owns transition authority.
    pub active_replacement_bundle: Option<u64>,
    /// Temporary builder/decoder/upload memory currently reserved and separately reported.
    pub transient: usize,
    /// Maximum concurrent transient reservation observed by this ledger.
    pub transient_high_water: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
    pub retained: LedgerRetainedBreakdown,
    pub records: Vec<AllocationRecordSnapshot>,
    pub transient_resources: Vec<TransientResourceSnapshot>,
}

/// Explicit, potentially linear-time allocation inventory. `LedgerSnapshot` remains as the
/// compatibility name while callers migrate; control-plane code must use [`LedgerProgress`].
pub type LedgerInventory = LedgerSnapshot;

/// Constant-size view of the capacity state relevant to admission and deferred retries.
///
/// Unlike [`LedgerSnapshot`], constructing this value never enumerates, clones, or sorts
/// allocation records. UI and scheduler control paths must use this type; the full inventory is
/// reserved for explicit diagnostics and eviction planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerProgress {
    pub policy_revision: u64,
    pub budget: usize,
    pub transition_reserve: usize,
    pub transient_budget: usize,
    pub charged: usize,
    pub reserved: usize,
    pub reserved_normal: usize,
    pub reserved_transition: usize,
    pub active_replacement_bundle: Option<u64>,
    pub transient: usize,
    pub transient_high_water: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
    pub retained: LedgerRetainedBreakdown,
    revision: u64,
}

impl LedgerProgress {
    /// Opaque equality key for capacity changes which may unblock deferred work.
    #[must_use]
    pub const fn progress_epoch(self) -> LedgerProgressEpoch {
        LedgerProgressEpoch {
            revision: self.revision,
            policy_revision: self.policy_revision,
            budget: self.budget,
            transition_reserve: self.transition_reserve,
            transient_budget: self.transient_budget,
            charged: self.charged,
            reserved: self.reserved,
            active_replacement_bundle: self.active_replacement_bundle,
            transient: self.transient,
            pinned: self.pinned,
        }
    }
}

/// Constant-size instrumentation proving whether a caller has requested an expensive allocation
/// inventory. These counters are deliberately independent of the capacity progress epoch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LedgerOperationCounters {
    pub progress_snapshots: u64,
    pub full_inventory_snapshots: u64,
    pub allocation_records_enumerated: u64,
}

/// Coalescing notification stream for ledger capacity progress. The ledger never blocks while
/// publishing: at most one unread notification is retained for each subscriber.
#[derive(Debug)]
pub struct LedgerProgressSubscription {
    receiver: Receiver<()>,
}

impl LedgerProgressSubscription {
    /// Returns true once for one or more capacity mutations since the preceding call.
    pub fn take_pending(&self) -> bool {
        let pending = match self.receiver.try_recv() {
            Ok(()) => true,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => false,
        };
        if pending {
            while self.receiver.try_recv().is_ok() {}
        }
        pending
    }
}

/// Opaque equality key for retained/transient capacity which can make a previously refused
/// allocation retryable. Unlike `policy_revision`, ordinary lease retirement and transition
/// settlement also change this value. Callers use equality only: it is deliberately not a
/// globally ordered clock and therefore cannot overflow during long-running browsing sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerProgressEpoch {
    revision: u64,
    policy_revision: u64,
    budget: usize,
    transition_reserve: usize,
    transient_budget: usize,
    charged: usize,
    reserved: usize,
    active_replacement_bundle: Option<u64>,
    transient: usize,
    pinned: usize,
}

impl LedgerSnapshot {
    /// Extracts the constant-size control-plane view from an explicit inventory.
    #[must_use]
    pub const fn capacity_progress(&self) -> LedgerProgress {
        LedgerProgress {
            policy_revision: self.policy_revision,
            budget: self.budget,
            transition_reserve: self.transition_reserve,
            transient_budget: self.transient_budget,
            charged: self.charged,
            reserved: self.reserved,
            reserved_normal: self.reserved_normal,
            reserved_transition: self.reserved_transition,
            active_replacement_bundle: self.active_replacement_bundle,
            transient: self.transient,
            transient_high_water: self.transient_high_water,
            pinned: self.pinned,
            pinned_overage: self.pinned_overage,
            retained: self.retained,
            revision: self.progress_revision,
        }
    }

    /// Returns the capacity epoch relevant to a deferred allocation retry. Read-only metadata and
    /// LRU touches do not perturb it, while budget edits, reclamation, released pins, transient
    /// checkpoints, and replacement-bundle settlement do.
    #[must_use]
    pub const fn progress_epoch(&self) -> LedgerProgressEpoch {
        LedgerProgressEpoch {
            revision: self.progress_revision,
            policy_revision: self.policy_revision,
            budget: self.budget,
            transition_reserve: self.transition_reserve,
            transient_budget: self.transient_budget,
            charged: self.charged,
            reserved: self.reserved,
            active_replacement_bundle: self.active_replacement_bundle,
            transient: self.transient,
            pinned: self.pinned,
        }
    }
}

#[derive(Debug)]
struct AllocationRecord {
    class: AllocationClass,
    charged_capacity: usize,
    references: usize,
    pins: usize,
    last_used_revision: u64,
    /// A replacement bundle may count this allocation as guaranteed retirement exactly once.
    /// The claim survives candidate staging and is cleared by settlement, abort, or allocation
    /// destruction.
    retirement_claim: Option<u64>,
}

#[derive(Debug)]
struct LedgerInner {
    progress_revision: u64,
    progress_watchers: Vec<SyncSender<()>>,
    operation_counters: LedgerOperationCounters,
    policy_revision: u64,
    budget: usize,
    transition_reserve: usize,
    transient_budget: usize,
    next_id: u64,
    next_bundle_id: u64,
    /// Retained foreground replacement is deliberately single-flight per shared ledger. Normal
    /// base-budget work may continue, but no second transition may consume overlap capacity
    /// before the first candidate has retired its predecessor and settled.
    active_replacement_bundle: Option<u64>,
    /// A successful settlement preflight temporarily closes retained-budget mutation. This makes
    /// the following token-destruction suffix non-fallible: no ordinary reservation or policy
    /// edit can invalidate the proved post-retirement state between the two operations.
    prepared_replacement_settlement: Option<u64>,
    charged: usize,
    charged_by_class: [usize; AllocationClass::COUNT],
    reserved_normal: usize,
    reserved_transition: usize,
    transient_normal: usize,
    transient_transition: usize,
    transient_high_water: usize,
    /// Sum of the charged capacity of records with at least one pin. A record contributes once,
    /// irrespective of its pin count, matching the historical full-snapshot definition.
    pinned: usize,
    transient_resources: HashMap<TransientResourceClass, TransientUsage>,
    records: HashMap<AllocationId, AllocationRecord>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TransientUsage {
    current: usize,
    high_water: usize,
}

impl LedgerInner {
    fn note_progress(&mut self) {
        self.progress_revision = self.progress_revision.wrapping_add(1);
        self.progress_watchers
            .retain(|watcher| matches!(watcher.try_send(()), Ok(()) | Err(TrySendError::Full(()))));
    }

    fn advance_policy_revision(&mut self) -> Result<(), LedgerError> {
        self.policy_revision = self
            .policy_revision
            .checked_add(1)
            .ok_or(LedgerError::IdOverflow)?;
        self.note_progress();
        Ok(())
    }

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
                progress_revision: 1,
                progress_watchers: Vec::new(),
                operation_counters: LedgerOperationCounters::default(),
                policy_revision: 1,
                budget,
                transition_reserve,
                transient_budget,
                next_id: 1,
                next_bundle_id: 1,
                active_replacement_bundle: None,
                prepared_replacement_settlement: None,
                charged: 0,
                charged_by_class: [0; AllocationClass::COUNT],
                reserved_normal: 0,
                reserved_transition: 0,
                transient_normal: 0,
                transient_transition: 0,
                transient_high_water: 0,
                pinned: 0,
                transient_resources: HashMap::new(),
                records: HashMap::new(),
            })),
        }
    }

    /// Subscribes to coalesced capacity changes without exposing the allocation inventory.
    #[must_use]
    pub fn subscribe_progress(&self) -> LedgerProgressSubscription {
        let (sender, receiver) = sync_channel(1);
        if let Ok(mut inner) = self.inner.lock() {
            inner.progress_watchers.push(sender);
        }
        LedgerProgressSubscription { receiver }
    }

    /// Returns the constant-size capacity state used by scheduling and retry decisions.
    pub fn progress(&self) -> Result<LedgerProgress, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        inner.operation_counters.progress_snapshots = inner
            .operation_counters
            .progress_snapshots
            .saturating_add(1);
        Ok(LedgerProgress {
            policy_revision: inner.policy_revision,
            budget: inner.budget,
            transition_reserve: inner.transition_reserve,
            transient_budget: inner.transient_budget,
            charged: inner.charged,
            reserved: inner.total_reserved(),
            reserved_normal: inner.reserved_normal,
            reserved_transition: inner.reserved_transition,
            active_replacement_bundle: inner.active_replacement_bundle,
            transient: inner.total_transient(),
            transient_high_water: inner.transient_high_water,
            pinned: inner.pinned,
            pinned_overage: inner.pinned.saturating_sub(inner.budget),
            retained: LedgerRetainedBreakdown {
                by_class: inner.charged_by_class,
            },
            revision: inner.progress_revision,
        })
    }

    /// Returns constant-size control-path instrumentation without perturbing it.
    pub fn operation_counters(&self) -> Result<LedgerOperationCounters, LedgerError> {
        let inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        Ok(inner.operation_counters)
    }

    /// Begins an exact retirement proof from references physically owned by a renderer token.
    /// Every live reference for each included allocation must be presented before `finish`.
    #[must_use]
    pub fn retirement_claim_builder(&self) -> AllocationRetirementClaimBuilder<'_> {
        AllocationRetirementClaimBuilder {
            ledger: self,
            references: Vec::new(),
        }
    }

    /// Creates a ledger-bound empty proof for a first-frame candidate with no predecessor.
    #[must_use]
    pub fn empty_retirement_claim_set(&self) -> AllocationRetirementClaimSet {
        AllocationRetirementClaimSet {
            ledger: Arc::downgrade(&self.inner),
            claims: Vec::new(),
        }
    }

    /// Combines independently owned renderer retirement authorities for one atomic foreground
    /// bundle. Every input must belong to this ledger, and one allocation identity may be credited
    /// by only one child token. Passing no sets returns a ledger-bound empty authority.
    pub fn combine_retirement_claim_sets<'a>(
        &self,
        sets: impl IntoIterator<Item = &'a AllocationRetirementClaimSet>,
    ) -> Result<AllocationRetirementClaimSet, AllocationBundleError> {
        let mut claims = Vec::new();
        for set in sets {
            if !set.belongs_to(&self.inner) {
                return Err(AllocationBundleError::ForeignRetirementClaimSet);
            }
            claims.extend_from_slice(&set.claims);
        }
        claims.sort_unstable_by_key(|claim| claim.id);
        for pair in claims.windows(2) {
            if pair[0].id == pair[1].id {
                return Err(AllocationBundleError::DuplicateRetirement(pair[0].id));
            }
        }
        Ok(AllocationRetirementClaimSet {
            ledger: Arc::downgrade(&self.inner),
            claims,
        })
    }

    pub fn reserve(
        &self,
        class: AllocationClass,
        maximum_bytes: usize,
        kind: ReservationKind,
    ) -> Result<AllocationReservation, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if let Some(bundle_id) = inner.prepared_replacement_settlement {
            return Err(LedgerError::ReplacementTransitionActive { bundle_id });
        }
        // GPU old/new overlap is single-flight. The shared ledger also serves source and semantic
        // transitions, so those classes remain admissible but are restricted to base-budget
        // headroom while a GPU bundle owns the overlap allowance.
        let effective_kind = if kind == ReservationKind::Transition
            && let Some(bundle_id) = inner.active_replacement_bundle
        {
            if class.is_gpu_transition_allocation() {
                return Err(LedgerError::ReplacementTransitionActive { bundle_id });
            }
            ReservationKind::Normal
        } else {
            kind
        };
        let requested_total = inner
            .charged
            .saturating_add(inner.total_reserved())
            .saturating_add(maximum_bytes);
        let limit = inner.reservation_limit(effective_kind);
        if requested_total > limit {
            return Err(LedgerError::BudgetExceeded {
                requested: maximum_bytes,
                charged: inner.charged,
                reserved: inner.total_reserved(),
                limit,
            });
        }
        *inner.reserved_mut(effective_kind) = inner
            .reserved_mut(effective_kind)
            .saturating_add(maximum_bytes);
        inner.note_progress();
        drop(inner);
        Ok(AllocationReservation {
            inner: Arc::clone(&self.inner),
            class,
            kind: effective_kind,
            remaining: maximum_bytes,
        })
    }

    /// Atomically reserves every retained allocation required by a foreground replacement.
    ///
    /// Two independent limits are proved while the ledger mutex is held:
    ///
    /// - the candidate plus all state not guaranteed to retire must fit the normal budget; and
    /// - the old and new presentations may overlap only within the bounded transition reserve.
    ///
    /// This prevents transition capacity from becoming permanent steady-state overage and avoids
    /// partial page/table/frame reservations whose later phase can still fail. Retirement credit
    /// is granted only for exact allocation identities wholly owned by the caller's retirement
    /// object.
    pub fn reserve_replacement_bundle(
        &self,
        mut spec: AllocationBundleSpec,
    ) -> Result<AllocationBundleReservation, AllocationBundleError> {
        spec.demands.sort_unstable_by_key(|demand| demand.slot);
        for pair in spec.demands.windows(2) {
            if pair[0].slot == pair[1].slot {
                return Err(AllocationBundleError::DuplicateDemandSlot(pair[0].slot));
            }
        }
        let maximum_bytes = spec.demands.iter().try_fold(0_usize, |total, demand| {
            total.checked_add(demand.maximum_bytes).ok_or(
                AllocationBundleError::AccountingOverflow {
                    operation: AllocationBundleAccountingOperation::CandidateDemand,
                },
            )
        })?;
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if let Some(bundle_id) = inner.active_replacement_bundle {
            return Err(AllocationBundleError::ReplacementTransitionActive { bundle_id });
        }
        if !spec.retiring.belongs_to(&self.inner) {
            return Err(AllocationBundleError::ForeignRetirementClaimSet);
        }
        if inner.policy_revision != spec.policy_revision {
            return Err(AllocationBundleError::PolicyRevisionChanged {
                planned: spec.policy_revision,
                current: inner.policy_revision,
            });
        }

        let mut retiring_bytes = 0_usize;
        for claim in &spec.retiring.claims {
            if claim.owned_references == 0 {
                return Err(AllocationBundleError::EmptyRetirementClaim(claim.id));
            }
            let record = inner
                .records
                .get(&claim.id)
                .ok_or(LedgerError::UnknownAllocation(claim.id))?;
            if record.retirement_claim.is_some() {
                return Err(AllocationBundleError::RetirementAlreadyClaimed(claim.id));
            }
            if record.references != claim.owned_references {
                return Err(AllocationBundleError::RetirementReferenceMismatch {
                    id: claim.id,
                    expected: claim.owned_references,
                    actual: record.references,
                });
            }
            retiring_bytes = retiring_bytes.checked_add(record.charged_capacity).ok_or(
                AllocationBundleError::AccountingOverflow {
                    operation: AllocationBundleAccountingOperation::RetirementCredit,
                },
            )?;
        }

        let occupied = inner
            .charged
            .checked_add(inner.reserved_normal)
            .and_then(|total| total.checked_add(inner.reserved_transition))
            .ok_or(AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::Occupied,
            })?;
        let projected_steady = occupied
            .checked_sub(retiring_bytes)
            .and_then(|total| total.checked_add(maximum_bytes))
            .ok_or(AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::SteadyProjection,
            })?;
        if projected_steady > inner.budget {
            return Err(AllocationBundleError::SteadyBudgetExceeded {
                candidate: maximum_bytes,
                occupied,
                retiring: retiring_bytes,
                limit: inner.budget,
            });
        }
        let overlap_limit = inner.budget.checked_add(inner.transition_reserve).ok_or(
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::OverlapLimit,
            },
        )?;
        let projected_overlap = occupied.checked_add(maximum_bytes).ok_or(
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::OverlapProjection,
            },
        )?;
        if projected_overlap > overlap_limit {
            return Err(AllocationBundleError::TransitionOverlapBudgetExceeded {
                candidate: maximum_bytes,
                occupied,
                limit: overlap_limit,
            });
        }

        let bundle_id = inner.next_bundle_id;
        inner.next_bundle_id = inner
            .next_bundle_id
            .checked_add(1)
            .ok_or(LedgerError::IdOverflow)?;
        let reserved_transition = inner.reserved_transition.checked_add(maximum_bytes).ok_or(
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::TransitionReservation,
            },
        )?;
        inner.active_replacement_bundle = Some(bundle_id);
        for claim in &spec.retiring.claims {
            inner
                .records
                .get_mut(&claim.id)
                .expect("retirement records were validated under the same ledger lock")
                .retirement_claim = Some(bundle_id);
        }
        inner.reserved_transition = reserved_transition;
        inner.note_progress();
        let base_limit_bytes = inner.budget;
        drop(inner);

        Ok(AllocationBundleReservation {
            inner: Arc::clone(&self.inner),
            bundle_id,
            policy_revision: spec.policy_revision,
            base_limit_bytes,
            demands: spec
                .demands
                .into_iter()
                .map(BundleDemandState::from)
                .collect(),
            retiring: spec.retiring,
            reserved_bytes: maximum_bytes,
            committed: false,
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
        inner.note_progress();
        drop(inner);
        Ok(TransientResourceReservation {
            inner: Arc::clone(&self.inner),
            class,
            kind,
            bytes: maximum_bytes,
        })
    }

    /// Atomically changes every allocation policy limit and advances the policy revision exactly
    /// once when any value differs. A bundle planned against the prior tuple cannot observe a
    /// partially updated base/overlap/transient policy.
    pub fn set_limits(
        &self,
        budget: usize,
        transition_reserve: usize,
        transient_budget: usize,
    ) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if (
            inner.budget,
            inner.transition_reserve,
            inner.transient_budget,
        ) != (budget, transition_reserve, transient_budget)
        {
            if let Some(bundle_id) = inner.prepared_replacement_settlement {
                return Err(LedgerError::ReplacementTransitionActive { bundle_id });
            }
            inner.advance_policy_revision()?;
            inner.budget = budget;
            inner.transition_reserve = transition_reserve;
            inner.transient_budget = transient_budget;
        }
        Ok(())
    }

    /// Compatibility wrapper for callers changing only the retained base limit.
    pub fn set_budget(&self, budget: usize) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if inner.budget != budget {
            if let Some(bundle_id) = inner.prepared_replacement_settlement {
                return Err(LedgerError::ReplacementTransitionActive { bundle_id });
            }
            inner.advance_policy_revision()?;
            inner.budget = budget;
        }
        Ok(())
    }

    /// Compatibility wrapper for callers changing only the old/new overlap allowance.
    pub fn set_transition_reserve(&self, transition_reserve: usize) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if inner.transition_reserve != transition_reserve {
            if let Some(bundle_id) = inner.prepared_replacement_settlement {
                return Err(LedgerError::ReplacementTransitionActive { bundle_id });
            }
            inner.advance_policy_revision()?;
            inner.transition_reserve = transition_reserve;
        }
        Ok(())
    }

    /// Compatibility wrapper for callers changing only the transient working-memory limit.
    pub fn set_transient_budget(&self, budget: usize) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if inner.transient_budget != budget {
            if let Some(bundle_id) = inner.prepared_replacement_settlement {
                return Err(LedgerError::ReplacementTransitionActive { bundle_id });
            }
            inner.advance_policy_revision()?;
            inner.transient_budget = budget;
        }
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

    pub fn inventory(&self) -> Result<LedgerInventory, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let enumerated = u64::try_from(inner.records.len()).unwrap_or(u64::MAX);
        inner.operation_counters.full_inventory_snapshots = inner
            .operation_counters
            .full_inventory_snapshots
            .saturating_add(1);
        inner.operation_counters.allocation_records_enumerated = inner
            .operation_counters
            .allocation_records_enumerated
            .saturating_add(enumerated);
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
        let pinned = inner.pinned;
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
            progress_revision: inner.progress_revision,
            policy_revision: inner.policy_revision,
            budget: inner.budget,
            transition_reserve: inner.transition_reserve,
            transient_budget: inner.transient_budget,
            charged: inner.charged,
            reserved: inner.total_reserved(),
            reserved_normal: inner.reserved_normal,
            reserved_transition: inner.reserved_transition,
            active_replacement_bundle: inner.active_replacement_bundle,
            transient: inner.total_transient(),
            transient_high_water: inner.transient_high_water,
            pinned,
            pinned_overage: pinned.saturating_sub(inner.budget),
            retained: LedgerRetainedBreakdown {
                by_class: inner.charged_by_class,
            },
            records,
            transient_resources,
        })
    }

    /// Compatibility spelling for an explicit full inventory. This is deliberately not used by
    /// UI polling, camera capture, admission, or retry decisions.
    pub fn snapshot(&self) -> Result<LedgerSnapshot, LedgerError> {
        self.inventory()
    }

    pub fn evictable_in_policy_order(&self) -> Result<Vec<AllocationId>, LedgerError> {
        let mut candidates = self
            .inventory()?
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
        inner.note_progress();
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
            inner.note_progress();
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
        inner.charged_by_class[self.class.index()] =
            inner.charged_by_class[self.class.index()].saturating_add(charged_capacity);
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
                retirement_claim: None,
            },
        );
        inner.note_progress();
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
            inner.note_progress();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BundleDemandState {
    slot: AllocationBundleSlot,
    class: AllocationClass,
    maximum_bytes: usize,
}

impl From<AllocationBundleDemand> for BundleDemandState {
    fn from(demand: AllocationBundleDemand) -> Self {
        Self {
            slot: demand.slot,
            class: demand.class,
            maximum_bytes: demand.maximum_bytes,
        }
    }
}

/// Whole-candidate retained reservation. GPU objects may be staged while this token holds every
/// byte they could retain; `commit` creates every allocation identity in one ledger transaction.
/// If staging fails, dropping the token releases all capacity and retirement claims together.
#[derive(Debug)]
#[must_use = "dropping an uncommitted bundle releases the candidate reservation"]
pub struct AllocationBundleReservation {
    inner: Arc<Mutex<LedgerInner>>,
    bundle_id: u64,
    policy_revision: u64,
    base_limit_bytes: usize,
    demands: Vec<BundleDemandState>,
    retiring: AllocationRetirementClaimSet,
    reserved_bytes: usize,
    committed: bool,
}

impl AllocationBundleReservation {
    #[must_use]
    pub const fn maximum_bytes(&self) -> usize {
        self.reserved_bytes
    }

    #[must_use]
    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    /// Converts the candidate reservation into charged allocations atomically. The returned
    /// commit token intentionally withholds its leases until `retire_and_settle` consumes the
    /// exact old presentation token.
    pub fn commit(
        mut self,
        mut actuals: Vec<AllocationBundleActual>,
    ) -> Result<AllocationBundleCommit, AllocationBundleError> {
        actuals.sort_unstable_by_key(|actual| actual.slot);
        for pair in actuals.windows(2) {
            if pair[0].slot == pair[1].slot {
                return Err(AllocationBundleError::DuplicateActualSlot(pair[0].slot));
            }
        }
        for actual in &actuals {
            let Some(demand) = self
                .demands
                .iter()
                .find(|demand| demand.slot == actual.slot)
            else {
                return Err(AllocationBundleError::UnexpectedActualSlot(actual.slot));
            };
            if actual.charged_capacity > demand.maximum_bytes {
                return Err(AllocationBundleError::ActualExceedsDemand {
                    slot: actual.slot,
                    actual: actual.charged_capacity,
                    maximum: demand.maximum_bytes,
                });
            }
        }
        for demand in &self.demands {
            if !actuals.iter().any(|actual| actual.slot == demand.slot) {
                return Err(AllocationBundleError::MissingActualSlot(demand.slot));
            }
        }

        let allocation_count = actuals
            .iter()
            .filter(|actual| actual.charged_capacity > 0)
            .count();
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if inner.policy_revision != self.policy_revision {
            return Err(AllocationBundleError::PolicyRevisionChanged {
                planned: self.policy_revision,
                current: inner.policy_revision,
            });
        }
        for claim in &self.retiring.claims {
            let record = inner
                .records
                .get(&claim.id)
                .ok_or(AllocationBundleError::RetirementClaimLost(claim.id))?;
            if record.retirement_claim != Some(self.bundle_id) {
                return Err(AllocationBundleError::RetirementClaimLost(claim.id));
            }
            if record.references != claim.owned_references {
                return Err(AllocationBundleError::RetirementReferenceMismatch {
                    id: claim.id,
                    expected: claim.owned_references,
                    actual: record.references,
                });
            }
        }
        let allocation_count_u64 =
            u64::try_from(allocation_count).map_err(|_| LedgerError::IdOverflow)?;
        inner
            .next_id
            .checked_add(allocation_count_u64)
            .ok_or(LedgerError::IdOverflow)?;

        let reserved_transition = inner
            .reserved_transition
            .checked_sub(self.reserved_bytes)
            .ok_or(AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::CommitReservationRelease,
            })?;
        let actual_bytes = actuals.iter().try_fold(0_usize, |total, actual| {
            total.checked_add(actual.charged_capacity).ok_or(
                AllocationBundleError::AccountingOverflow {
                    operation: AllocationBundleAccountingOperation::CommitActual,
                },
            )
        })?;
        let charged = inner.charged.checked_add(actual_bytes).ok_or(
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::CommitCharge,
            },
        )?;
        inner.reserved_transition = reserved_transition;
        inner.charged = charged;
        let mut allocations = Vec::with_capacity(allocation_count);
        for actual in actuals {
            if actual.charged_capacity == 0 {
                continue;
            }
            let demand = self
                .demands
                .iter()
                .find(|demand| demand.slot == actual.slot)
                .expect("bundle actual slots were validated before ledger mutation");
            let id = AllocationId(inner.next_id);
            inner.next_id += 1;
            inner.charged_by_class[demand.class.index()] = inner.charged_by_class
                [demand.class.index()]
            .saturating_add(actual.charged_capacity);
            inner.records.insert(
                id,
                AllocationRecord {
                    class: demand.class,
                    charged_capacity: actual.charged_capacity,
                    references: 1,
                    pins: 0,
                    last_used_revision: actual.last_used_revision,
                    retirement_claim: None,
                },
            );
            allocations.push(AllocationBundleLease {
                slot: actual.slot,
                allocation: AllocationLease {
                    inner: Arc::clone(&self.inner),
                    id,
                },
            });
        }
        inner.note_progress();
        drop(inner);

        self.reserved_bytes = 0;
        self.committed = true;
        let retiring = self.retiring.clone();
        Ok(AllocationBundleCommit {
            inner: Arc::clone(&self.inner),
            bundle_id: self.bundle_id,
            policy_revision: self.policy_revision,
            base_limit_bytes: self.base_limit_bytes,
            retiring,
            allocations,
            settled: false,
        })
    }
}

impl Drop for AllocationBundleReservation {
    fn drop(&mut self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if self.reserved_bytes > 0 {
            inner.reserved_transition = inner
                .reserved_transition
                .checked_sub(self.reserved_bytes)
                .expect("bundle reservation release cannot exceed held transition bytes");
        }
        if !self.committed {
            for claim in &self.retiring.claims {
                if let Some(record) = inner.records.get_mut(&claim.id)
                    && record.retirement_claim == Some(self.bundle_id)
                {
                    record.retirement_claim = None;
                }
            }
            if inner.active_replacement_bundle == Some(self.bundle_id) {
                inner.active_replacement_bundle = None;
            }
        }
        inner.note_progress();
    }
}

/// Candidate allocations committed under the bounded overlap policy but not yet eligible for
/// publication. A replacement with retirement credit must use `retire_and_settle`, which verifies
/// and consumes the exact old presentation token before releasing candidate leases.
#[derive(Debug)]
#[must_use = "an allocation bundle must settle before its candidate can be published"]
pub struct AllocationBundleCommit {
    inner: Arc<Mutex<LedgerInner>>,
    bundle_id: u64,
    policy_revision: u64,
    base_limit_bytes: usize,
    retiring: AllocationRetirementClaimSet,
    allocations: Vec<AllocationBundleLease>,
    settled: bool,
}

/// A replacement whose complete fallible settlement preflight has succeeded.
///
/// This value keeps the old renderer token borrowed and the candidate unpublished while the
/// ledger briefly excludes retained reservations and policy edits. Dropping it aborts the
/// candidate, releases the settlement gate, and leaves `token` untouched. Calling
/// [`Self::retire_and_settle`] is therefore the only operation which consumes the old token, and
/// everything after that consumption consists solely of invariant-preserving ledger mutation.
#[must_use = "dropping a prepared settlement aborts its unpublished candidate"]
pub struct PreparedAllocationBundleSettlement<'a, T: AllocationRetirementToken> {
    commit: Option<AllocationBundleCommit>,
    token: &'a mut Option<T>,
    settlement_limit: usize,
}

impl<T: AllocationRetirementToken> std::fmt::Debug for PreparedAllocationBundleSettlement<'_, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAllocationBundleSettlement")
            .field(
                "bundle_id",
                &self.commit.as_ref().map(|commit| commit.bundle_id),
            )
            .field("settlement_limit", &self.settlement_limit)
            .finish_non_exhaustive()
    }
}

impl AllocationBundleCommit {
    #[must_use]
    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    /// Settles a first-frame candidate which claimed no predecessor. Replacements must use
    /// [`Self::retire_and_settle`] so production code cannot separate old-resource destruction
    /// from the ledger's settlement check.
    pub fn settle(self) -> Result<Vec<AllocationBundleLease>, AllocationBundleError> {
        if !self.retiring.is_empty() {
            return Err(AllocationBundleError::RetirementTokenRequired);
        }
        self.settle_after_retirement()
    }

    /// Verifies a renderer-owned token against the exact committed claim set, revalidates that the
    /// token still owns every live reference, then consumes/drops it and settles. `token` remains
    /// `Some` on any preflight mismatch, so an incorrect token cannot destroy the old presentation.
    pub fn retire_and_settle<T: AllocationRetirementToken>(
        self,
        token: &mut Option<T>,
    ) -> Result<Vec<AllocationBundleLease>, AllocationBundleError> {
        Ok(self
            .prepare_retirement_settlement(token)?
            .retire_and_settle())
    }

    /// Performs every recoverable check while the renderer still owns its old presentation.
    ///
    /// On error, this consumes and aborts the unpublished candidate but leaves `token` as `Some`,
    /// so the caller may restore the detached renderer state. On success, the returned boundary
    /// excludes retained-budget mutations until it is either dropped (rollback) or settled.
    pub fn prepare_retirement_settlement<'a, T: AllocationRetirementToken>(
        self,
        token: &'a mut Option<T>,
    ) -> Result<PreparedAllocationBundleSettlement<'a, T>, AllocationBundleError> {
        let retirement = token
            .as_ref()
            .ok_or(AllocationBundleError::MissingRetirementToken)?;
        if retirement.retirement_claim_set() != &self.retiring {
            return Err(AllocationBundleError::RetirementClaimSetMismatch);
        }

        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        if inner.active_replacement_bundle != Some(self.bundle_id) {
            return Err(AllocationBundleError::ReplacementTransitionLost {
                bundle_id: self.bundle_id,
            });
        }
        if let Some(bundle_id) = inner.prepared_replacement_settlement {
            return Err(AllocationBundleError::ReplacementTransitionActive { bundle_id });
        }
        let mut retiring_bytes = 0_usize;
        for claim in &self.retiring.claims {
            let record = inner
                .records
                .get(&claim.id)
                .ok_or(AllocationBundleError::RetirementClaimLost(claim.id))?;
            if record.retirement_claim != Some(self.bundle_id) {
                return Err(AllocationBundleError::RetirementClaimLost(claim.id));
            }
            if record.references != claim.owned_references {
                return Err(AllocationBundleError::RetirementReferenceMismatch {
                    id: claim.id,
                    expected: claim.owned_references,
                    actual: record.references,
                });
            }
            retiring_bytes = retiring_bytes.checked_add(record.charged_capacity).ok_or(
                AllocationBundleError::AccountingOverflow {
                    operation: AllocationBundleAccountingOperation::SettlementRetirementCredit,
                },
            )?;
        }
        let occupied = inner
            .charged
            .checked_add(inner.reserved_normal)
            .and_then(|total| total.checked_add(inner.reserved_transition))
            .ok_or(AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::SettlementOccupied,
            })?;
        let projected_steady = occupied.checked_sub(retiring_bytes).ok_or(
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::SettlementSteadyProjection,
            },
        )?;
        let settlement_limit = self.base_limit_bytes.max(inner.budget);
        if projected_steady > settlement_limit {
            return Err(AllocationBundleError::SteadyStateNotReached {
                occupied: projected_steady,
                limit: settlement_limit,
            });
        }
        inner.prepared_replacement_settlement = Some(self.bundle_id);
        drop(inner);

        Ok(PreparedAllocationBundleSettlement {
            commit: Some(self),
            token,
            settlement_limit,
        })
    }

    /// Verifies that every credited allocation disappeared with the retirement token and that the
    /// candidate fits the captured base limit. A later budget reduction is handled as
    /// post-publication overage; a later increase may safely relax settlement to the new limit.
    fn settle_after_retirement(
        mut self,
    ) -> Result<Vec<AllocationBundleLease>, AllocationBundleError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        for claim in &self.retiring.claims {
            if inner.records.contains_key(&claim.id) {
                return Err(AllocationBundleError::RetirementIncomplete(claim.id));
            }
        }
        let occupied = inner
            .charged
            .checked_add(inner.reserved_normal)
            .and_then(|total| total.checked_add(inner.reserved_transition))
            .ok_or(AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::SettlementOccupied,
            })?;
        let settlement_limit = self.base_limit_bytes.max(inner.budget);
        if occupied > settlement_limit {
            return Err(AllocationBundleError::SteadyStateNotReached {
                occupied,
                limit: settlement_limit,
            });
        }
        // Any claim records should already have disappeared with their allocations. Clear
        // defensively in case a zero-capacity retirement representation is introduced later.
        for claim in &self.retiring.claims {
            if let Some(record) = inner.records.get_mut(&claim.id)
                && record.retirement_claim == Some(self.bundle_id)
            {
                record.retirement_claim = None;
            }
        }
        if inner.active_replacement_bundle == Some(self.bundle_id) {
            inner.active_replacement_bundle = None;
        }
        inner.note_progress();
        drop(inner);
        self.settled = true;
        Ok(std::mem::take(&mut self.allocations))
    }

    /// Completes a successful [`Self::prepare_retirement_settlement`] without an ordinary error
    /// path. The preflight gate makes every assertion below an internal ledger invariant. A mutex
    /// poison or invariant violation deliberately panics: returning an error after the old token
    /// has been consumed would falsely promise that rollback remained possible.
    fn settle_after_prepared_retirement(
        mut self,
        settlement_limit: usize,
    ) -> Vec<AllocationBundleLease> {
        let mut inner = self
            .inner
            .lock()
            .expect("allocation ledger poisoned after settlement consumed retirement token");
        assert_eq!(
            inner.prepared_replacement_settlement,
            Some(self.bundle_id),
            "prepared replacement settlement lost its exclusive ledger gate"
        );
        assert_eq!(
            inner.active_replacement_bundle,
            Some(self.bundle_id),
            "prepared replacement settlement lost active bundle authority"
        );
        for claim in &self.retiring.claims {
            assert!(
                !inner.records.contains_key(&claim.id),
                "prepared retirement token did not release allocation {:?}",
                claim.id
            );
        }
        let occupied = inner
            .charged
            .checked_add(inner.reserved_normal)
            .and_then(|total| total.checked_add(inner.reserved_transition))
            .expect("prepared replacement settlement occupied-byte arithmetic overflowed");
        assert!(
            occupied <= settlement_limit,
            "prepared replacement settlement exceeded its proved steady-state limit: occupied {occupied}, limit {settlement_limit}"
        );
        inner.prepared_replacement_settlement = None;
        inner.active_replacement_bundle = None;
        inner.note_progress();
        drop(inner);
        self.settled = true;
        std::mem::take(&mut self.allocations)
    }
}

impl<T: AllocationRetirementToken> PreparedAllocationBundleSettlement<'_, T> {
    /// Destroys the verified old renderer state and publishes the candidate leases.
    ///
    /// All fallible work occurred in [`AllocationBundleCommit::prepare_retirement_settlement`].
    /// Consequently this suffix does not return `Result`: after token consumption, failure would
    /// no longer be recoverable and is treated as an internal invariant violation instead.
    #[must_use]
    pub fn retire_and_settle(mut self) -> Vec<AllocationBundleLease> {
        let retirement = self
            .token
            .take()
            .expect("prepared replacement settlement must retain its retirement token");
        drop(retirement);
        self.commit
            .take()
            .expect("prepared replacement settlement must retain its commit")
            .settle_after_prepared_retirement(self.settlement_limit)
    }
}

impl<T: AllocationRetirementToken> Drop for PreparedAllocationBundleSettlement<'_, T> {
    fn drop(&mut self) {
        let Some(commit) = self.commit.as_ref() else {
            return;
        };
        if let Ok(mut inner) = commit.inner.lock()
            && inner.prepared_replacement_settlement == Some(commit.bundle_id)
        {
            inner.prepared_replacement_settlement = None;
            inner.note_progress();
        }
        // `commit` then drops normally, aborting the candidate and releasing its retirement
        // claims. The borrowed token was never taken and remains available to its caller.
    }
}

impl Drop for AllocationBundleCommit {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            if inner.prepared_replacement_settlement == Some(self.bundle_id) {
                inner.prepared_replacement_settlement = None;
            }
            for claim in &self.retiring.claims {
                if let Some(record) = inner.records.get_mut(&claim.id)
                    && record.retirement_claim == Some(self.bundle_id)
                {
                    record.retirement_claim = None;
                }
            }
            if inner.active_replacement_bundle == Some(self.bundle_id) {
                inner.active_replacement_bundle = None;
            }
            inner.note_progress();
        }
        // Unsettled candidate leases then drop normally and remove their exact charges.
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
        let newly_pinned_bytes = {
            let record = inner
                .records
                .get_mut(&self.id)
                .ok_or(LedgerError::UnknownAllocation(self.id))?;
            let newly_pinned_bytes = (record.pins == 0).then_some(record.charged_capacity);
            record.pins = record.pins.saturating_add(1);
            newly_pinned_bytes
        };
        if let Some(bytes) = newly_pinned_bytes {
            inner.pinned = inner.pinned.saturating_add(bytes);
        }
        inner.note_progress();
        drop(inner);
        Ok(PinnedAllocationLease { lease })
    }

    /// Atomically acquires an optional pin only when doing so keeps the ledger's unique pinned
    /// capacity at or below `maximum_total_pinned_bytes`. An already-pinned allocation adds no
    /// unique bytes and may therefore be shared even at the ceiling. Mandatory presentation pins
    /// continue to use [`Self::pin`].
    pub fn pin_if_total_at_most(
        &self,
        maximum_total_pinned_bytes: usize,
    ) -> Result<Option<PinnedAllocationLease>, LedgerError> {
        let mut inner = self.inner.lock().map_err(|_| LedgerError::Poisoned)?;
        let (newly_pinned_bytes, allowed) = {
            let record = inner
                .records
                .get(&self.id)
                .ok_or(LedgerError::UnknownAllocation(self.id))?;
            let newly_pinned_bytes = (record.pins == 0).then_some(record.charged_capacity);
            let allowed = newly_pinned_bytes.is_none_or(|bytes| {
                inner.pinned.saturating_add(bytes) <= maximum_total_pinned_bytes
            });
            (newly_pinned_bytes, allowed)
        };
        if !allowed {
            return Ok(None);
        }
        let record = inner
            .records
            .get_mut(&self.id)
            .expect("optional pin allocation was validated under the same ledger lock");
        record.references = record.references.saturating_add(1);
        record.pins = record.pins.saturating_add(1);
        if let Some(bytes) = newly_pinned_bytes {
            inner.pinned = inner.pinned.saturating_add(bytes);
        }
        inner.note_progress();
        drop(inner);
        Ok(Some(PinnedAllocationLease {
            lease: AllocationLease {
                inner: Arc::clone(&self.inner),
                id: self.id,
            },
        }))
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
            let class = record.class;
            inner.records.remove(&self.id);
            inner.charged = inner.charged.saturating_sub(charged_capacity);
            inner.charged_by_class[class.index()] =
                inner.charged_by_class[class.index()].saturating_sub(charged_capacity);
            inner.note_progress();
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
        if let Ok(mut inner) = self.lease.inner.lock() {
            let unpinned_bytes = inner.records.get_mut(&self.lease.id).and_then(|record| {
                let unpinned_bytes = (record.pins == 1).then_some(record.charged_capacity);
                record.pins = record.pins.saturating_sub(1);
                unpinned_bytes
            });
            if let Some(bytes) = unpinned_bytes {
                inner.pinned = inner.pinned.saturating_sub(bytes);
            }
            if unpinned_bytes.is_some() {
                inner.note_progress();
            }
        }
    }
}

/// Exact phase whose checked byte arithmetic exceeded the platform address space. Keeping the
/// phase typed lets coordinators distinguish an intrinsically invalid plan from budget pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationBundleAccountingOperation {
    CandidateDemand,
    RetirementCredit,
    Occupied,
    SteadyProjection,
    OverlapLimit,
    OverlapProjection,
    TransitionReservation,
    CommitReservationRelease,
    CommitActual,
    CommitCharge,
    SettlementRetirementCredit,
    SettlementOccupied,
    SettlementSteadyProjection,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AllocationBundleError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("allocation-bundle accounting overflow during {operation:?}")]
    AccountingOverflow {
        operation: AllocationBundleAccountingOperation,
    },
    #[error("replacement GPU transition bundle {bundle_id} is already active")]
    ReplacementTransitionActive { bundle_id: u64 },
    #[error("duplicate allocation-bundle demand slot {0:?}")]
    DuplicateDemandSlot(AllocationBundleSlot),
    #[error("duplicate allocation-bundle actual slot {0:?}")]
    DuplicateActualSlot(AllocationBundleSlot),
    #[error("allocation-bundle actual references unknown slot {0:?}")]
    UnexpectedActualSlot(AllocationBundleSlot),
    #[error("allocation-bundle actual is missing required slot {0:?}")]
    MissingActualSlot(AllocationBundleSlot),
    #[error(
        "allocation-bundle slot {slot:?} retained {actual} bytes, exceeding its {maximum}-byte demand"
    )]
    ActualExceedsDemand {
        slot: AllocationBundleSlot,
        actual: usize,
        maximum: usize,
    },
    #[error("duplicate retirement claim for allocation {0:?}")]
    DuplicateRetirement(AllocationId),
    #[error("the same physical retirement reference for allocation {0:?} was included twice")]
    DuplicateRetirementReference(AllocationId),
    #[error("retirement reference for allocation {id:?} belongs to another ledger")]
    ForeignRetirementReference { id: AllocationId },
    #[error("retirement claim set belongs to another allocation ledger")]
    ForeignRetirementClaimSet,
    #[error("retirement claim for allocation {0:?} owns no lease references")]
    EmptyRetirementClaim(AllocationId),
    #[error("allocation {0:?} is already claimed by another replacement bundle")]
    RetirementAlreadyClaimed(AllocationId),
    #[error(
        "retirement claim for allocation {id:?} owns {expected} references but the ledger has {actual}"
    )]
    RetirementReferenceMismatch {
        id: AllocationId,
        expected: usize,
        actual: usize,
    },
    #[error("retirement claim for allocation {0:?} changed before bundle commit")]
    RetirementClaimLost(AllocationId),
    #[error("replacement bundle {bundle_id} lost its active transition authority")]
    ReplacementTransitionLost { bundle_id: u64 },
    #[error("allocation {0:?} credited to the candidate has not retired")]
    RetirementIncomplete(AllocationId),
    #[error("a committed replacement with retirement credit requires an owning retirement token")]
    RetirementTokenRequired,
    #[error("no retirement token was supplied")]
    MissingRetirementToken,
    #[error("the supplied retirement token does not match the committed claim set")]
    RetirementClaimSetMismatch,
    #[error(
        "allocation-bundle policy changed after planning: planned revision {planned}, current revision {current}"
    )]
    PolicyRevisionChanged { planned: u64, current: u64 },
    #[error(
        "replacement candidate cannot fit the steady budget: candidate {candidate}, occupied {occupied}, guaranteed retirement {retiring}, limit {limit}"
    )]
    SteadyBudgetExceeded {
        candidate: usize,
        occupied: usize,
        retiring: usize,
        limit: usize,
    },
    #[error(
        "replacement overlap exceeds transition allowance: candidate {candidate}, occupied {occupied}, limit {limit}"
    )]
    TransitionOverlapBudgetExceeded {
        candidate: usize,
        occupied: usize,
        limit: usize,
    },
    #[error(
        "replacement candidate has not reached steady state: occupied {occupied}, base limit {limit}"
    )]
    SteadyStateNotReached { occupied: usize, limit: usize },
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LedgerError {
    #[error("replacement GPU transition bundle {bundle_id} is already active")]
    ReplacementTransitionActive { bundle_id: u64 },
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
    #[error("allocation ledger identity or policy revision space exhausted")]
    IdOverflow,
    #[error("allocation ledger mutex was poisoned")]
    Poisoned,
}

impl LedgerError {
    /// Whether this request cannot fit even in an otherwise empty budget of the relevant class.
    /// Occupancy pressure and an active replacement transition are temporary; retrying them after
    /// a capacity epoch changes is useful. An intrinsically oversized request requires a policy or
    /// contract change instead.
    #[must_use]
    pub const fn intrinsically_exceeds_limit(self) -> bool {
        match self {
            Self::BudgetExceeded {
                requested, limit, ..
            }
            | Self::TransientBudgetExceeded {
                requested, limit, ..
            } => requested > limit,
            Self::ReplacementTransitionActive { .. }
            | Self::ReservationExceeded { .. }
            | Self::UnknownAllocation(_)
            | Self::IdOverflow
            | Self::Poisoned => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestRetirementToken {
        claims: AllocationRetirementClaimSet,
        _leases: Vec<AllocationLease>,
        _pins: Vec<PinnedAllocationLease>,
    }

    impl AllocationRetirementToken for TestRetirementToken {
        fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
            &self.claims
        }
    }

    #[derive(Debug)]
    struct CompositeRetirementToken {
        claims: AllocationRetirementClaimSet,
        _children: Vec<TestRetirementToken>,
    }

    impl AllocationRetirementToken for CompositeRetirementToken {
        fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
            &self.claims
        }
    }

    fn retirement_token(
        ledger: &AllocationLedger,
        leases: Vec<AllocationLease>,
    ) -> TestRetirementToken {
        let claims = {
            let mut builder = ledger.retirement_claim_builder();
            for lease in &leases {
                builder.include_lease(lease).unwrap();
            }
            builder.finish().unwrap()
        };
        TestRetirementToken {
            claims,
            _leases: leases,
            _pins: Vec::new(),
        }
    }

    fn replacement_spec(
        ledger: &AllocationLedger,
        demands: Vec<AllocationBundleDemand>,
        retirement: Option<&TestRetirementToken>,
    ) -> AllocationBundleSpec {
        AllocationBundleSpec::new(
            ledger.snapshot().unwrap().policy_revision,
            demands,
            retirement.map_or_else(
                || ledger.empty_retirement_claim_set(),
                |token| token.claims.clone(),
            ),
        )
    }

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
    fn atomic_limit_update_advances_one_policy_revision_and_wrappers_remain_coherent() {
        let ledger = AllocationLedger::new_with_transient_budget(1_000, 200, 300);
        let initial = ledger.snapshot().unwrap();
        ledger.set_limits(900, 180, 270).unwrap();
        let atomic = ledger.snapshot().unwrap();
        assert_eq!(atomic.policy_revision, initial.policy_revision + 1);
        assert_eq!(atomic.budget, 900);
        assert_eq!(atomic.transition_reserve, 180);
        assert_eq!(atomic.transient_budget, 270);

        ledger.set_limits(900, 180, 270).unwrap();
        assert_eq!(
            ledger.snapshot().unwrap().policy_revision,
            atomic.policy_revision
        );
        ledger.set_budget(800).unwrap();
        ledger.set_transition_reserve(160).unwrap();
        ledger.set_transient_budget(240).unwrap();
        let wrapped = ledger.snapshot().unwrap();
        assert_eq!(wrapped.policy_revision, atomic.policy_revision + 3);
        assert_eq!(
            (
                wrapped.budget,
                wrapped.transition_reserve,
                wrapped.transient_budget
            ),
            (800, 160, 240)
        );
    }

    #[test]
    fn failed_policy_revision_advance_leaves_the_limit_tuple_unchanged() {
        let ledger = AllocationLedger::new_with_transient_budget(1_000, 200, 300);
        {
            let mut inner = ledger.inner.lock().unwrap();
            inner.policy_revision = u64::MAX;
        }
        assert_eq!(
            ledger.set_limits(900, 180, 270),
            Err(LedgerError::IdOverflow)
        );
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.policy_revision, u64::MAX);
        assert_eq!(
            (
                snapshot.budget,
                snapshot.transition_reserve,
                snapshot.transient_budget
            ),
            (1_000, 200, 300)
        );
    }

    #[test]
    fn bundle_diagnostics_separate_normal_and_transition_reservations() {
        let ledger = AllocationLedger::new(2_000, 1_000);
        let _normal = ledger
            .reserve(AllocationClass::CoreChunk, 100, ReservationKind::Normal)
            .unwrap();
        let mut old_reservation = ledger
            .reserve(AllocationClass::GpuPage, 500, ReservationKind::Normal)
            .unwrap();
        let old = old_reservation.register(500, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    AllocationBundleSlot::new(0),
                    AllocationClass::GpuPage,
                    600,
                )],
                Some(&token),
            ))
            .unwrap();
        let active = ledger.snapshot().unwrap();
        assert_eq!(active.reserved_normal, 100);
        assert_eq!(active.reserved_transition, 600);
        assert_eq!(active.reserved, 700);
        assert!(active.active_replacement_bundle.is_some());

        drop(bundle);
        let released = ledger.snapshot().unwrap();
        assert_eq!(released.reserved_normal, 100);
        assert_eq!(released.reserved_transition, 0);
        assert_eq!(released.reserved, 100);
        assert_eq!(released.active_replacement_bundle, None);
    }

    #[test]
    fn bundle_accounting_overflow_is_typed_and_has_no_side_effects() {
        let ledger = AllocationLedger::new(usize::MAX, usize::MAX);
        let error = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![
                    AllocationBundleDemand::new(
                        AllocationBundleSlot::new(0),
                        AllocationClass::GpuPage,
                        usize::MAX,
                    ),
                    AllocationBundleDemand::new(
                        AllocationBundleSlot::new(1),
                        AllocationClass::TransitionFrame,
                        1,
                    ),
                ],
                None,
            ))
            .unwrap_err();
        assert_eq!(
            error,
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::CandidateDemand,
            }
        );
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.reserved, 0);
        assert_eq!(snapshot.active_replacement_bundle, None);

        let overlap = AllocationLedger::new(usize::MAX, 1);
        let error = overlap
            .reserve_replacement_bundle(replacement_spec(&overlap, Vec::new(), None))
            .unwrap_err();
        assert_eq!(
            error,
            AllocationBundleError::AccountingOverflow {
                operation: AllocationBundleAccountingOperation::OverlapLimit,
            }
        );
    }

    #[test]
    fn active_gpu_bundle_restricts_semantic_transition_to_safe_base_headroom() {
        let ledger = AllocationLedger::new(1_000, 500);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    AllocationBundleSlot::new(0),
                    AllocationClass::GpuPage,
                    100,
                )],
                None,
            ))
            .unwrap();
        let semantic = ledger
            .reserve(
                AllocationClass::SemanticPage,
                100,
                ReservationKind::Transition,
            )
            .unwrap();
        assert_eq!(semantic.kind, ReservationKind::Normal);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.reserved_normal, 100);
        assert_eq!(snapshot.reserved_transition, 100);
        assert!(matches!(
            ledger.reserve(AllocationClass::GpuPage, 1, ReservationKind::Transition),
            Err(LedgerError::ReplacementTransitionActive { .. })
        ));
        drop(semantic);
        drop(bundle);
    }

    #[test]
    fn replacement_bundle_proves_steady_and_overlap_then_settles_atomically() {
        let ledger = AllocationLedger::new(1_000, 400);
        let mut old_reservation = ledger
            .reserve(
                AllocationClass::TransitionFrame,
                600,
                ReservationKind::Normal,
            )
            .unwrap();
        let old = old_reservation.register(600, 1).unwrap();
        let old_id = old.id();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(7);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    700,
                )],
                Some(&token),
            ))
            .unwrap();
        let reserved = ledger.snapshot().unwrap();
        assert_eq!(reserved.charged, 600);
        assert_eq!(reserved.reserved, 700);

        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 700, 2)])
            .unwrap();
        assert!(matches!(
            committed.settle(),
            Err(AllocationBundleError::RetirementTokenRequired)
        ));
        // The failed settlement drops the unpublished candidate and releases its claim.
        assert_eq!(ledger.snapshot().unwrap().charged, 600);

        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    700,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 640, 3)])
            .unwrap();
        let mut token = Some(token);
        let allocations = committed.retire_and_settle(&mut token).unwrap();
        assert!(token.is_none());
        assert!(
            !ledger
                .snapshot()
                .unwrap()
                .records
                .iter()
                .any(|r| r.id == old_id)
        );
        assert_eq!(allocations.len(), 1);
        assert_eq!(allocations[0].slot, slot);
        assert_eq!(
            allocations[0]
                .allocation
                .snapshot()
                .unwrap()
                .charged_capacity,
            640
        );
        let settled = ledger.snapshot().unwrap();
        assert_eq!(settled.charged, 640);
        assert_eq!(settled.reserved, 0);
    }

    #[test]
    fn transition_allowance_cannot_establish_an_over_budget_first_frame() {
        let ledger = AllocationLedger::new(1_000, 500);
        let error = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    AllocationBundleSlot::new(0),
                    AllocationClass::GpuPage,
                    1_100,
                )],
                None,
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            AllocationBundleError::SteadyBudgetExceeded {
                candidate: 1_100,
                retiring: 0,
                limit: 1_000,
                ..
            }
        ));
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn transition_allowance_cannot_hide_an_over_budget_replacement() {
        let ledger = AllocationLedger::new(1_000, 1_000);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 900, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(900, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let error = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    AllocationBundleSlot::new(0),
                    AllocationClass::GpuPage,
                    1_100,
                )],
                Some(&token),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            AllocationBundleError::SteadyBudgetExceeded {
                candidate: 1_100,
                retiring: 900,
                limit: 1_000,
                ..
            }
        ));
    }

    #[test]
    fn replacement_bundle_refuses_excessive_old_new_overlap() {
        let ledger = AllocationLedger::new(1_000, 200);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 900, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(900, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let error = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    AllocationBundleSlot::new(0),
                    AllocationClass::GpuPage,
                    900,
                )],
                Some(&token),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            AllocationBundleError::TransitionOverlapBudgetExceeded {
                candidate: 900,
                occupied: 900,
                limit: 1_200,
            }
        ));
    }

    #[test]
    fn retirement_credit_requires_every_live_lease_reference() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 600, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(600, 1).unwrap();
        let _unowned_clone = old.clone();
        let mut builder = ledger.retirement_claim_builder();
        builder.include_lease(&old).unwrap();
        let error = builder.finish().unwrap_err();
        assert!(matches!(
            error,
            AllocationBundleError::RetirementReferenceMismatch {
                expected: 1,
                actual: 2,
                ..
            }
        ));
    }

    #[test]
    fn maximal_exact_retirement_skips_shared_allocations_without_weakening_strict_proofs() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 600, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(600, 1).unwrap();
        let external = old.clone();

        let maximal = {
            let mut builder = ledger.retirement_claim_builder();
            builder.include_lease(&old).unwrap();
            builder.finish_maximal_exact().unwrap()
        };
        assert!(maximal.is_empty());

        drop(external);
        let complete = {
            let mut builder = ledger.retirement_claim_builder();
            builder.include_lease(&old).unwrap();
            builder.finish_maximal_exact().unwrap()
        };
        assert_eq!(complete.len(), 1);
        assert_eq!(complete.claims[0].id(), old.id());
    }

    #[test]
    fn retirement_builder_rejects_duplicate_physical_and_foreign_references() {
        let ledger = AllocationLedger::new(1_000, 500);
        let foreign = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 100, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(100, 1).unwrap();

        let mut duplicate = ledger.retirement_claim_builder();
        duplicate.include_lease(&lease).unwrap();
        assert_eq!(
            duplicate.include_lease(&lease),
            Err(AllocationBundleError::DuplicateRetirementReference(
                lease.id()
            ))
        );
        let mut wrong_ledger = foreign.retirement_claim_builder();
        assert_eq!(
            wrong_ledger.include_lease(&lease),
            Err(AllocationBundleError::ForeignRetirementReference { id: lease.id() })
        );
    }

    #[test]
    fn retirement_builder_counts_owned_lease_and_pin_references_exactly() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 100, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(100, 1).unwrap();
        let pin = lease.pin().unwrap();
        let claims = {
            let mut builder = ledger.retirement_claim_builder();
            builder.include_lease(&lease).unwrap();
            builder.include_pin(&pin).unwrap();
            builder.finish().unwrap()
        };
        assert_eq!(claims.len(), 1);
        assert_eq!(claims.claims[0].id(), lease.id());
        assert_eq!(claims.claims[0].owned_references(), 2);
    }

    #[test]
    fn combined_claim_sets_support_composite_retirement_and_reject_overlap() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut first_reservation = ledger
            .reserve(AllocationClass::GpuPage, 200, ReservationKind::Normal)
            .unwrap();
        let first = first_reservation.register(200, 1).unwrap();
        let mut second_reservation = ledger
            .reserve(
                AllocationClass::TransitionFrame,
                200,
                ReservationKind::Normal,
            )
            .unwrap();
        let second = second_reservation.register(200, 1).unwrap();
        let first = retirement_token(&ledger, vec![first]);
        let second = retirement_token(&ledger, vec![second]);
        let combined = ledger
            .combine_retirement_claim_sets([&first.claims, &second.claims])
            .unwrap();
        assert_eq!(combined.len(), 2);
        assert!(
            ledger
                .combine_retirement_claim_sets(std::iter::empty())
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            ledger.combine_retirement_claim_sets([&first.claims, &first.claims]),
            Err(AllocationBundleError::DuplicateRetirement(_))
        ));
        let foreign = AllocationLedger::new(1_000, 500);
        let foreign_empty = foreign.empty_retirement_claim_set();
        assert_eq!(
            ledger.combine_retirement_claim_sets([&foreign_empty]),
            Err(AllocationBundleError::ForeignRetirementClaimSet)
        );

        let composite = CompositeRetirementToken {
            claims: combined,
            _children: vec![first, second],
        };
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                ledger.snapshot().unwrap().policy_revision,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    300,
                )],
                composite.claims.clone(),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 300, 2)])
            .unwrap();
        let mut composite = Some(composite);
        let allocations = committed.retire_and_settle(&mut composite).unwrap();
        assert!(composite.is_none());
        assert_eq!(allocations.len(), 1);
        assert_eq!(ledger.snapshot().unwrap().charged, 300);
    }

    #[test]
    fn mismatched_retirement_token_is_preserved_and_candidate_is_aborted() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut first_reservation = ledger
            .reserve(AllocationClass::GpuPage, 200, ReservationKind::Normal)
            .unwrap();
        let first = first_reservation.register(200, 1).unwrap();
        let mut second_reservation = ledger
            .reserve(AllocationClass::GpuPage, 200, ReservationKind::Normal)
            .unwrap();
        let second = second_reservation.register(200, 1).unwrap();
        let correct = retirement_token(&ledger, vec![first]);
        let wrong = retirement_token(&ledger, vec![second]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    300,
                )],
                Some(&correct),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 300, 2)])
            .unwrap();
        let mut wrong = Some(wrong);
        assert!(matches!(
            committed.retire_and_settle(&mut wrong),
            Err(AllocationBundleError::RetirementClaimSetMismatch)
        ));
        assert!(wrong.is_some());
        assert_eq!(ledger.snapshot().unwrap().charged, 400);
        assert_eq!(ledger.snapshot().unwrap().active_replacement_bundle, None);
        drop(correct);
    }

    #[test]
    fn prepared_settlement_success_has_an_infallible_suffix_and_releases_its_gate() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 400, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(400, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    500,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 450, 2)])
            .unwrap();
        let mut token = Some(token);
        let prepared = committed.prepare_retirement_settlement(&mut token).unwrap();

        let active = ledger.snapshot().unwrap().active_replacement_bundle;
        assert!(active.is_some());
        assert!(matches!(
            ledger.reserve(AllocationClass::CoreChunk, 1, ReservationKind::Normal),
            Err(LedgerError::ReplacementTransitionActive { bundle_id }) if Some(bundle_id) == active
        ));
        assert!(matches!(
            ledger.set_budget(999),
            Err(LedgerError::ReplacementTransitionActive { bundle_id }) if Some(bundle_id) == active
        ));
        assert!(matches!(
            ledger.set_budget(1_001),
            Err(LedgerError::ReplacementTransitionActive { bundle_id }) if Some(bundle_id) == active
        ));

        // There is deliberately no `Result` after this point: every recoverable check completed
        // while `token` was still available to the caller.
        let allocations = prepared.retire_and_settle();
        assert_eq!(allocations.len(), 1);
        assert!(token.is_none());
        let settled = ledger.snapshot().unwrap();
        assert_eq!(settled.active_replacement_bundle, None);
        assert_eq!(settled.charged, 450);
        assert!(ledger.set_budget(1_001).is_ok());
    }

    #[test]
    fn dropping_a_prepared_settlement_preserves_token_and_releases_bundle_authority() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 400, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(400, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    500,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 450, 2)])
            .unwrap();
        let mut token = Some(token);

        drop(committed.prepare_retirement_settlement(&mut token).unwrap());

        assert!(token.is_some());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.active_replacement_bundle, None);
        assert_eq!(snapshot.charged, 400);
        assert_eq!(snapshot.reserved, 0);
    }

    #[test]
    fn settlement_budget_failure_preserves_token_and_does_not_leak_active_bundle() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut unrelated = ledger
            .reserve(AllocationClass::CoreChunk, 700, ReservationKind::Normal)
            .unwrap();
        let _unrelated = unrelated.register(700, 1).unwrap();
        let mut old_reservation = ledger
            .reserve(AllocationClass::GpuPage, 100, ReservationKind::Normal)
            .unwrap();
        let old = old_reservation.register(100, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    200,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 200, 2)])
            .unwrap();

        // Work admitted under a temporary larger policy can make this already-committed
        // candidate unable to settle under either its captured base or the current reduced base.
        ledger.set_budget(1_500).unwrap();
        let mut extra = ledger
            .reserve(
                AllocationClass::NavigationHistory,
                500,
                ReservationKind::Normal,
            )
            .unwrap();
        let _extra = extra.register(500, 3).unwrap();
        ledger.set_budget(500).unwrap();
        let mut token = Some(token);

        assert!(matches!(
            committed.prepare_retirement_settlement(&mut token),
            Err(AllocationBundleError::SteadyStateNotReached {
                occupied: 1_400,
                limit: 1_000,
            })
        ));
        assert!(token.is_some());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.active_replacement_bundle, None);
        assert_eq!(snapshot.charged, 1_300);
        assert_eq!(snapshot.reserved, 0);
    }

    #[test]
    fn settlement_refcount_failure_preserves_token_and_does_not_leak_active_bundle() {
        let ledger = AllocationLedger::new(1_000, 500);
        let mut reservation = ledger
            .reserve(AllocationClass::GpuPage, 400, ReservationKind::Normal)
            .unwrap();
        let old = reservation.register(400, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let old_id = token._leases[0].id();
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    500,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 450, 2)])
            .unwrap();
        // Simulate a renderer violating exclusivity after commit. Settlement must detect this
        // before taking the token rather than discovering the remaining reference afterward.
        let leaked_reference = token._leases[0].clone();
        let mut token = Some(token);

        assert_eq!(
            committed
                .prepare_retirement_settlement(&mut token)
                .unwrap_err(),
            AllocationBundleError::RetirementReferenceMismatch {
                id: old_id,
                expected: 1,
                actual: 2,
            }
        );
        assert!(token.is_some());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.active_replacement_bundle, None);
        assert_eq!(snapshot.charged, 400);
        drop(leaked_reference);
    }

    #[test]
    fn budget_policy_change_invalidates_an_uncommitted_bundle() {
        let ledger = AllocationLedger::new(1_000, 200);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    500,
                )],
                None,
            ))
            .unwrap();
        let planned_revision = bundle.policy_revision();
        ledger.set_limits(900, 180, 700).unwrap();
        let error = bundle
            .commit(vec![AllocationBundleActual::new(slot, 500, 1)])
            .unwrap_err();
        assert!(matches!(
            error,
            AllocationBundleError::PolicyRevisionChanged { .. }
        ));
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
        assert_eq!(ledger.snapshot().unwrap().charged, 0);
        let changed = ledger.snapshot().unwrap();
        assert_eq!(changed.policy_revision, planned_revision + 1);
        assert_eq!(
            (
                changed.budget,
                changed.transition_reserve,
                changed.transient_budget
            ),
            (900, 180, 700)
        );
    }

    #[test]
    fn budget_reduction_after_commit_cannot_discard_both_presentations() {
        let ledger = AllocationLedger::new(1_000, 400);
        let mut old_reservation = ledger
            .reserve(
                AllocationClass::TransitionFrame,
                600,
                ReservationKind::Normal,
            )
            .unwrap();
        let old = old_reservation.register(600, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::TransitionFrame,
                    700,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 700, 2)])
            .unwrap();

        // The new limit applies to subsequent work and reclamation. It cannot retroactively
        // invalidate the already staged old->new transaction between retirement and publish.
        ledger.set_limits(500, 200, 300).unwrap();
        let mut token = Some(token);
        let allocations = committed.retire_and_settle(&mut token).unwrap();
        assert_eq!(allocations.len(), 1);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.budget, 500);
        assert_eq!(snapshot.transition_reserve, 200);
        assert_eq!(snapshot.transient_budget, 300);
        assert_eq!(snapshot.charged, 700);
        assert_eq!(snapshot.reserved, 0);
    }

    #[test]
    fn replacement_bundle_excludes_every_other_retained_transition_until_settlement() {
        let ledger = AllocationLedger::new(1_000, 400);
        let mut old_reservation = ledger
            .reserve(AllocationClass::GpuPage, 600, ReservationKind::Normal)
            .unwrap();
        let old = old_reservation.register(600, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    700,
                )],
                Some(&token),
            ))
            .unwrap();

        let snapshot = ledger.snapshot().unwrap();

        assert!(matches!(
            ledger.reserve(
                AllocationClass::TransitionFrame,
                1,
                ReservationKind::Transition,
            ),
            Err(LedgerError::ReplacementTransitionActive { .. })
        ));
        assert!(matches!(
            ledger.reserve_replacement_bundle(AllocationBundleSpec::new(
                snapshot.policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            )),
            Err(AllocationBundleError::ReplacementTransitionActive { .. })
        ));

        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 700, 2)])
            .unwrap();
        assert!(matches!(
            ledger.reserve(
                AllocationClass::TransitionFrame,
                1,
                ReservationKind::Transition,
            ),
            Err(LedgerError::ReplacementTransitionActive { .. })
        ));
        let mut token = Some(token);
        let allocations = committed.retire_and_settle(&mut token).unwrap();
        assert!(
            ledger
                .reserve(
                    AllocationClass::TransitionFrame,
                    1,
                    ReservationKind::Transition,
                )
                .is_ok()
        );
        drop(allocations);
    }

    #[test]
    fn budget_increase_between_commit_and_settlement_uses_the_new_base_limit() {
        let ledger = AllocationLedger::new(1_000, 200);
        let mut unrelated_reservation = ledger
            .reserve(AllocationClass::CoreChunk, 700, ReservationKind::Normal)
            .unwrap();
        let _unrelated = unrelated_reservation.register(700, 1).unwrap();
        let mut old_reservation = ledger
            .reserve(AllocationClass::GpuPage, 100, ReservationKind::Normal)
            .unwrap();
        let old = old_reservation.register(100, 1).unwrap();
        let token = retirement_token(&ledger, vec![old]);
        let slot = AllocationBundleSlot::new(0);
        let bundle = ledger
            .reserve_replacement_bundle(replacement_spec(
                &ledger,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    200,
                )],
                Some(&token),
            ))
            .unwrap();
        let committed = bundle
            .commit(vec![AllocationBundleActual::new(slot, 200, 2)])
            .unwrap();
        ledger.set_limits(1_500, 300, 800).unwrap();
        let mut normal = ledger
            .reserve(
                AllocationClass::NavigationHistory,
                500,
                ReservationKind::Normal,
            )
            .unwrap();
        let _normal = normal.register(500, 3).unwrap();
        let mut token = Some(token);
        let allocations = committed.retire_and_settle(&mut token).unwrap();
        assert_eq!(allocations.len(), 1);
        assert_eq!(ledger.snapshot().unwrap().charged, 1_400);
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

    #[test]
    fn constant_size_progress_never_enumerates_allocations() {
        let ledger = AllocationLedger::new(16_384, 0);
        let mut allocations = Vec::new();
        for revision in 0..128 {
            let mut reservation = ledger
                .reserve(AllocationClass::SemanticPage, 64, ReservationKind::Normal)
                .unwrap();
            allocations.push(reservation.register(64, revision).unwrap());
        }

        let before = ledger.operation_counters().unwrap();
        for _ in 0..1_000 {
            let progress = ledger.progress().unwrap();
            assert_eq!(progress.charged, 128 * 64);
        }
        let after = ledger.operation_counters().unwrap();

        assert_eq!(after.progress_snapshots - before.progress_snapshots, 1_000);
        assert_eq!(
            after.full_inventory_snapshots,
            before.full_inventory_snapshots
        );
        assert_eq!(
            after.allocation_records_enumerated,
            before.allocation_records_enumerated
        );
        drop(allocations);
    }

    #[test]
    fn progress_notifications_are_coalesced_and_only_follow_capacity_changes() {
        let ledger = AllocationLedger::new(1_000, 0);
        let updates = ledger.subscribe_progress();
        assert!(!updates.take_pending());

        let mut reservation = ledger
            .reserve(AllocationClass::SemanticPage, 100, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(100, 1).unwrap();
        // Both the reservation and registration are represented by one coalesced wake-up.
        assert!(updates.take_pending());
        assert!(!updates.take_pending());

        ledger.touch(lease.id(), 2).unwrap();
        let clone = lease.clone();
        drop(clone);
        assert!(!updates.take_pending());

        drop(lease);
        assert!(updates.take_pending());
        assert!(!updates.take_pending());
    }

    #[test]
    fn incrementally_maintained_pinned_capacity_matches_full_inventory() {
        let ledger = AllocationLedger::new(1_000, 0);
        let mut reservation = ledger
            .reserve(AllocationClass::CoreChunk, 320, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(320, 1).unwrap();
        let first_pin = lease.pin().unwrap();
        let second_pin = lease.pin().unwrap();

        assert_eq!(ledger.progress().unwrap().pinned, 320);
        assert_eq!(ledger.snapshot().unwrap().pinned, 320);
        drop(first_pin);
        assert_eq!(ledger.progress().unwrap().pinned, 320);
        drop(second_pin);
        assert_eq!(ledger.progress().unwrap().pinned, 0);
        assert_eq!(ledger.snapshot().unwrap().pinned, 0);
    }

    #[test]
    fn optional_pin_ceiling_is_atomic_and_counts_unique_capacity() {
        let ledger = AllocationLedger::new(1_000, 0);
        let mut first = ledger
            .reserve(AllocationClass::GpuPage, 600, ReservationKind::Normal)
            .unwrap();
        let first = first.register(600, 1).unwrap();
        let mut second = ledger
            .reserve(AllocationClass::GpuPage, 300, ReservationKind::Normal)
            .unwrap();
        let second = second.register(300, 2).unwrap();

        let first_pin = first.pin_if_total_at_most(700).unwrap().unwrap();
        assert_eq!(ledger.progress().unwrap().pinned, 600);
        assert!(second.pin_if_total_at_most(700).unwrap().is_none());
        let shared_first = first.pin_if_total_at_most(600).unwrap().unwrap();
        assert_eq!(ledger.progress().unwrap().pinned, 600);
        drop(first_pin);
        assert_eq!(ledger.progress().unwrap().pinned, 600);
        drop(shared_first);
        assert_eq!(ledger.progress().unwrap().pinned, 0);
        assert!(second.pin_if_total_at_most(300).unwrap().is_some());
    }

    #[test]
    fn retained_class_breakdown_reconciles_without_inventory() {
        let ledger = AllocationLedger::new(4_096, 0);
        let mut evidence = ledger
            .reserve(AllocationClass::CoreChunk, 300, ReservationKind::Normal)
            .unwrap();
        let evidence = evidence.register(300, 1).unwrap();
        let mut rows = ledger
            .reserve(AllocationClass::RowAtlas, 200, ReservationKind::Normal)
            .unwrap();
        let rows = rows.register(200, 2).unwrap();
        let mut gpu = ledger
            .reserve(AllocationClass::GpuPage, 500, ReservationKind::Normal)
            .unwrap();
        let gpu = gpu.register(500, 3).unwrap();

        let before = ledger.operation_counters().unwrap();
        let progress = ledger.progress().unwrap();
        let after = ledger.operation_counters().unwrap();
        assert_eq!(progress.retained.evidence_bytes(), 300);
        assert_eq!(progress.retained.row_atlas_bytes(), 200);
        assert_eq!(progress.retained.gpu_bytes(), 500);
        assert_eq!(progress.retained.total(), progress.charged);
        assert_eq!(
            after.full_inventory_snapshots,
            before.full_inventory_snapshots
        );

        drop(rows);
        assert_eq!(ledger.progress().unwrap().retained.row_atlas_bytes(), 0);
        drop(evidence);
        drop(gpu);
        assert_eq!(ledger.progress().unwrap().retained.total(), 0);
    }

    #[test]
    fn cache_residency_order_disposes_speculation_and_return_cache_first() {
        assert!(
            AlignmentCacheResidencyClass::DisposableSpeculation
                < AlignmentCacheResidencyClass::RecentLocusReturnCache
        );
        assert!(
            AlignmentCacheResidencyClass::RecentLocusReturnCache
                < AlignmentCacheResidencyClass::ActiveLocusCold
        );
        assert!(
            AlignmentCacheResidencyClass::ActiveLocusCold
                < AlignmentCacheResidencyClass::ActiveNearBuffer
        );
        assert!(
            AlignmentCacheResidencyClass::ActiveNearBuffer
                < AlignmentCacheResidencyClass::ForegroundVisible
        );
    }

    #[test]
    fn cache_inventory_reports_distinct_bytes_separately_from_entry_count() {
        let ledger = AllocationLedger::new(1_000, 0);
        let mut reservation = ledger
            .reserve(AllocationClass::SemanticPage, 320, ReservationKind::Normal)
            .unwrap();
        let lease = reservation.register(320, 1).unwrap();
        let pin = lease.pin().unwrap();
        let mut inventory = AlignmentCacheResidencyInventory::default();
        inventory.observe_entry(AlignmentCacheResidencyClass::ForegroundVisible);
        inventory.observe_entry(AlignmentCacheResidencyClass::ForegroundVisible);
        inventory.observe_distinct_allocation(
            AlignmentCacheResidencyClass::ForegroundVisible,
            lease.snapshot().unwrap(),
        );

        assert_eq!(inventory.foreground_visible_entries, 2);
        assert_eq!(inventory.foreground_visible_allocations, 1);
        assert_eq!(inventory.foreground_visible_bytes, 320);
        assert_eq!(inventory.pinned_bytes, 320);
        assert_eq!(inventory.unpinned_bytes, 0);
        assert_eq!(inventory.distinct_bytes(), 320);
        drop(pin);
    }
}
