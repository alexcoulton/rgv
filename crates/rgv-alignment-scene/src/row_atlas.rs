use std::{cmp::Reverse, collections::BinaryHeap, mem::size_of, ops::Range, sync::Arc};

use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, GenomicInterval,
    ReadStackingMode,
};
use rgv_hts::AlignmentSourcePosition;
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    AllocationReservation, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, LayoutContract, LedgerError, PinnedAllocationLease, ReservationKind,
    RowAtlasRevision,
    fingerprint::StableHasher,
    row_oracle::{AlignmentRowGroup, OracleRowExtent, RowOracleError, build_row_oracle},
};

/// One immutable row assignment addressed by physical source identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowAtlasRecord {
    handle: AlignmentHandle,
    row: u32,
    start: u64,
    end: u64,
}

impl RowAtlasRecord {
    #[must_use]
    pub const fn handle(self) -> AlignmentHandle {
        self.handle
    }

    #[must_use]
    pub const fn row(self) -> u32 {
        self.row
    }

    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }
}

/// Ordered picking result. `layer` and `record` are stable only inside this atlas root; the
/// physical alignment handle is stable across compatible roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowAtlasRecordHit {
    pub record: RowAtlasRecord,
    pub layer: u32,
    pub record_index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowExtent {
    minimum_start: u64,
    maximum_end: u64,
    record_count: u32,
}

/// One immutable pair-view connector in genomic/row coordinates. It is owned by the row atlas so
/// pages covering the gap between mates do not depend on either read overlapping that page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentRowConnector {
    template: u64,
    left: AlignmentHandle,
    right: Option<AlignmentHandle>,
    row: u32,
    start: u64,
    end: u64,
}

impl AlignmentRowConnector {
    #[must_use]
    pub const fn template(self) -> u64 {
        self.template
    }

    #[must_use]
    pub const fn left(self) -> AlignmentHandle {
        self.left
    }

    #[must_use]
    pub const fn right(self) -> Option<AlignmentHandle> {
        self.right
    }

    #[must_use]
    pub const fn row(self) -> u32 {
        self.row
    }

    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }
}

impl RowExtent {
    #[must_use]
    pub const fn minimum_start(self) -> u64 {
        self.minimum_start
    }

    #[must_use]
    pub const fn maximum_end(self) -> u64 {
        self.maximum_end
    }

    #[must_use]
    pub const fn record_count(self) -> u32 {
        self.record_count
    }
}

impl From<OracleRowExtent> for RowExtent {
    fn from(value: OracleRowExtent) -> Self {
        Self {
            minimum_start: value.minimum_start,
            maximum_end: value.maximum_end,
            record_count: value.record_count,
        }
    }
}

#[derive(Debug)]
struct RowAtlasLayer {
    /// Source-handle order makes lookup deterministic and logarithmic.
    records: Arc<[RowAtlasRecord]>,
    /// Indices into `records`, ordered by `(row, start, end, handle)`.
    row_order: Arc<[u32]>,
    /// Running maximum end within each row in `row_order`. This permits interval queries to stop
    /// before scanning the entire prefix when a row contains long reads.
    row_prefix_max_end: Arc<[u64]>,
    allocation: AllocationLease,
}

impl RowAtlasLayer {
    fn new(
        mut records: Vec<RowAtlasRecord>,
        mut reservation: AllocationReservation,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentRowAtlasError> {
        records.sort_unstable_by_key(|record| record.handle);
        if records
            .windows(2)
            .any(|pair| pair[0].handle == pair[1].handle)
        {
            return Err(AlignmentRowAtlasError::DuplicateHandle);
        }
        let mut row_order = (0..records.len())
            .map(|index| u32::try_from(index).map_err(|_| AlignmentRowAtlasError::IndexOverflow))
            .collect::<Result<Vec<_>, _>>()?;
        row_order.sort_unstable_by_key(|index| {
            let record = records[*index as usize];
            (record.row, record.start, record.end, record.handle)
        });
        let mut row_prefix_max_end = Vec::with_capacity(row_order.len());
        let mut previous_row = None;
        let mut maximum_end = 0_u64;
        for index in &row_order {
            let record = records[*index as usize];
            if previous_row != Some(record.row) {
                previous_row = Some(record.row);
                maximum_end = 0;
            }
            maximum_end = maximum_end.max(record.end);
            row_prefix_max_end.push(maximum_end);
        }
        records.shrink_to_fit();
        row_order.shrink_to_fit();
        row_prefix_max_end.shrink_to_fit();
        let retained_bytes = layer_bytes(records.len(), row_order.len(), row_prefix_max_end.len());
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            records: records.into(),
            row_order: row_order.into(),
            row_prefix_max_end: row_prefix_max_end.into(),
            allocation,
        }))
    }

    fn record(&self, handle: AlignmentHandle) -> Option<(u32, RowAtlasRecord)> {
        let index = self
            .records
            .binary_search_by_key(&handle, |record| record.handle)
            .ok()?;
        Some((u32::try_from(index).ok()?, self.records[index]))
    }

    fn row_range(&self, row: u32) -> Range<usize> {
        let start = self
            .row_order
            .partition_point(|index| self.records[*index as usize].row < row);
        let end = self
            .row_order
            .partition_point(|index| self.records[*index as usize].row <= row);
        start..end
    }

    fn overlapping_count(&self, row: u32, interval: GenomicInterval) -> usize {
        let mut count = 0_usize;
        self.visit_overlapping(row, interval, 0, |_| count = count.saturating_add(1));
        count
    }

    fn visit_overlapping(
        &self,
        row: u32,
        interval: GenomicInterval,
        layer: u32,
        mut visit: impl FnMut(RowAtlasRecordHit),
    ) {
        let row_range = self.row_range(row);
        let end = self.row_order[row_range.clone()]
            .partition_point(|index| self.records[*index as usize].start < interval.end())
            + row_range.start;
        let mut cursor = end;
        while cursor > row_range.start {
            let previous = cursor - 1;
            if self.row_prefix_max_end[previous] <= interval.start() {
                break;
            }
            let record_index = self.row_order[previous];
            let record = self.records[record_index as usize];
            if record.end > interval.start() {
                visit(RowAtlasRecordHit {
                    record,
                    layer,
                    record_index,
                });
            }
            cursor = previous;
        }
    }
}

/// Immutable, viewport-independent row authority.
///
/// Extensions append private immutable layers and share every incumbent layer. No published map
/// is registered or mutated when a camera moves or a query is projected.
#[derive(Debug)]
pub struct AlignmentRowAtlas {
    revision: RowAtlasRevision,
    derived_from_evidence: EvidenceRevision,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    layout_contract: LayoutContract,
    interval: GenomicInterval,
    layers: Arc<[Arc<RowAtlasLayer>]>,
    row_extents: Arc<[RowExtent]>,
    groups: Arc<[AlignmentRowGroup]>,
    connectors: Arc<[AlignmentRowConnector]>,
    record_count: usize,
    assignment_fingerprint: ContractFingerprint,
    metadata_allocation: AllocationLease,
}

impl AlignmentRowAtlas {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        revision: RowAtlasRevision,
        evidence: &Arc<EvidenceRoot>,
        interval: GenomicInterval,
        layout_contract: LayoutContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentRowAtlasError> {
        let maximum_records = maximum_selected_records(evidence, interval);
        let layer_reservation = ledger.reserve(
            AllocationClass::RowAtlas,
            layer_bytes(maximum_records, maximum_records, maximum_records),
            reservation_kind,
        )?;
        let oracle = build_row_oracle(evidence, interval, layout_contract)?;
        let retained_group_bytes = oracle
            .groups
            .iter()
            .map(crate::row_oracle::OracleGroup::retained_bytes)
            .sum::<usize>();
        let metadata_reservation = ledger.reserve(
            AllocationClass::RowAtlas,
            atlas_metadata_bytes(oracle.extents.len(), 1)
                .saturating_add(retained_group_bytes)
                .saturating_add(connector_bytes(oracle.connectors.len())),
            reservation_kind,
        )?;
        let record_count = oracle.assignments.len();
        let records = oracle
            .assignments
            .into_iter()
            .map(|record| RowAtlasRecord {
                handle: record.handle,
                row: record.row,
                start: record.start,
                end: record.end,
            })
            .collect();
        let row_extents = oracle.extents.into_iter().map(RowExtent::from).collect();
        let groups = oracle
            .groups
            .into_iter()
            .map(crate::row_oracle::OracleGroup::into_retained)
            .collect();
        let connectors = oracle
            .connectors
            .into_iter()
            .map(|connector| AlignmentRowConnector {
                template: connector.template,
                left: connector.left,
                right: connector.right,
                row: connector.row,
                start: connector.start,
                end: connector.end,
            })
            .collect();
        let layer = RowAtlasLayer::new(records, layer_reservation, publication_revision)?;
        Self::from_parts(
            revision,
            evidence.revision(),
            evidence.source_revision(),
            evidence.record_contract(),
            layout_contract,
            interval,
            vec![layer],
            row_extents,
            groups,
            connectors,
            record_count,
            metadata_reservation,
            publication_revision,
        )
    }

    /// Extends a canonical atlas without selecting or repacking incumbent records.
    ///
    /// Compact left extension is rebuilt canonically because inserting a prefix can change every
    /// compact row. Stable left extension freezes incumbent rows and places only new handles.
    #[allow(clippy::too_many_arguments)]
    pub fn extend(
        self: &Arc<Self>,
        revision: RowAtlasRevision,
        evidence: &Arc<EvidenceRoot>,
        target: GenomicInterval,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentRowAtlasError> {
        self.validate_extension(evidence, target)?;
        if target == self.interval {
            return Ok(Arc::clone(self));
        }
        if !supports_layered_extension(self.layout_contract)
            || (self.layout_contract.stacking == ReadStackingMode::Compact
                && target.start() < self.interval.start())
        {
            return Self::build(
                revision,
                evidence,
                target,
                self.layout_contract,
                ledger,
                reservation_kind,
                publication_revision,
            );
        }

        let maximum_records =
            maximum_selected_records(evidence, target).saturating_sub(self.record_count);
        let layer_reservation = ledger.reserve(
            AllocationClass::RowAtlas,
            layer_bytes(maximum_records, maximum_records, maximum_records),
            reservation_kind,
        )?;
        let metadata_reservation = ledger.reserve(
            AllocationClass::RowAtlas,
            atlas_metadata_bytes(
                self.row_extents.len().saturating_add(maximum_records),
                self.layers.len().saturating_add(1),
            )
            .saturating_add(size_of::<AlignmentRowGroup>()),
            reservation_kind,
        )?;

        let mut novel = Vec::<RowInput>::new();
        if target.start() < self.interval.start() {
            let left = GenomicInterval::new(target.contig(), target.start(), self.interval.start())
                .map_err(|_| AlignmentRowAtlasError::InvalidExtensionInterval)?;
            self.collect_novel(evidence, left, &mut novel)?;
        }
        if target.end() > self.interval.end() {
            let right = GenomicInterval::new(target.contig(), self.interval.end(), target.end())
                .map_err(|_| AlignmentRowAtlasError::InvalidExtensionInterval)?;
            self.collect_novel(evidence, right, &mut novel)?;
        }
        novel.sort_unstable_by_key(|record| (record.start, record.end, record.handle));
        novel.dedup_by_key(|record| record.handle);
        for record in &novel {
            if record.start < self.interval.end() && record.end > self.interval.start() {
                return Err(AlignmentRowAtlasError::NovelRecordInsideIncumbent {
                    handle: record.handle,
                    start: record.start,
                    end: record.end,
                    incumbent: self.interval,
                });
            }
        }

        let mut extents = self.row_extents.to_vec();
        let records = assign_extension_rows(&novel, &mut extents, self.interval)?;
        let mut layers = self.layers.to_vec();
        if !records.is_empty() {
            layers.push(RowAtlasLayer::new(
                records,
                layer_reservation,
                publication_revision,
            )?);
        }
        let row_count =
            u32::try_from(extents.len()).map_err(|_| AlignmentRowAtlasError::IndexOverflow)?;
        Self::from_parts(
            revision,
            evidence.revision(),
            self.source_revision,
            self.record_contract,
            self.layout_contract,
            target,
            layers,
            extents,
            vec![AlignmentRowGroup::all(row_count)],
            Vec::new(),
            self.record_count.saturating_add(novel.len()),
            metadata_reservation,
            publication_revision,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        revision: RowAtlasRevision,
        derived_from_evidence: EvidenceRevision,
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        layout_contract: LayoutContract,
        interval: GenomicInterval,
        mut layers: Vec<Arc<RowAtlasLayer>>,
        mut row_extents: Vec<RowExtent>,
        mut groups: Vec<AlignmentRowGroup>,
        mut connectors: Vec<AlignmentRowConnector>,
        record_count: usize,
        mut metadata_reservation: AllocationReservation,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentRowAtlasError> {
        layers.shrink_to_fit();
        row_extents.shrink_to_fit();
        groups.shrink_to_fit();
        connectors.shrink_to_fit();
        let assignment_fingerprint = assignment_fingerprint(layout_contract, &layers, record_count);
        let retained_bytes = atlas_metadata_bytes(row_extents.len(), layers.len())
            .saturating_add(row_group_bytes(&groups))
            .saturating_add(connector_bytes(connectors.len()));
        let metadata_allocation =
            metadata_reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            revision,
            derived_from_evidence,
            source_revision,
            record_contract,
            layout_contract,
            interval,
            layers: layers.into(),
            row_extents: row_extents.into(),
            groups: groups.into(),
            connectors: connectors.into(),
            record_count,
            assignment_fingerprint,
            metadata_allocation,
        }))
    }

    fn validate_extension(
        &self,
        evidence: &EvidenceRoot,
        target: GenomicInterval,
    ) -> Result<(), AlignmentRowAtlasError> {
        if evidence.source_revision() != self.source_revision {
            return Err(AlignmentRowAtlasError::SourceRevisionMismatch);
        }
        if evidence.record_contract() != self.record_contract {
            return Err(AlignmentRowAtlasError::RecordContractMismatch);
        }
        if target.contig() != self.interval.contig() || !target.contains_interval(self.interval) {
            return Err(AlignmentRowAtlasError::NonContainingExtension);
        }
        if !evidence.coverage().covers(target) {
            return Err(AlignmentRowAtlasError::UncertifiedInterval(target));
        }
        Ok(())
    }

    fn collect_novel(
        &self,
        evidence: &Arc<EvidenceRoot>,
        interval: GenomicInterval,
        target: &mut Vec<RowInput>,
    ) -> Result<(), AlignmentRowAtlasError> {
        let selection = evidence.select(interval)?;
        for (index, reference) in selection.records().iter().enumerate() {
            if self.record(reference.handle).is_some() {
                continue;
            }
            let view = selection
                .record_view(index)
                .ok_or(AlignmentRowAtlasError::InvalidEvidenceRecord)?;
            target.push(RowInput {
                handle: reference.handle,
                start: view.start(),
                end: view.end(),
            });
        }
        Ok(())
    }

    #[must_use]
    pub const fn revision(&self) -> RowAtlasRevision {
        self.revision
    }

    #[must_use]
    pub const fn derived_from_evidence(&self) -> EvidenceRevision {
        self.derived_from_evidence
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub const fn layout_contract(&self) -> LayoutContract {
        self.layout_contract
    }

    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub const fn record_count(&self) -> usize {
        self.record_count
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_extents.len()
    }

    #[must_use]
    pub fn row_extent(&self, row: u32) -> Option<RowExtent> {
        self.row_extents.get(row as usize).copied()
    }

    #[must_use]
    pub fn groups(&self) -> &[AlignmentRowGroup] {
        &self.groups
    }

    #[must_use]
    pub fn connectors_for_rows(
        &self,
        rows: Range<u32>,
        interval: GenomicInterval,
    ) -> Vec<AlignmentRowConnector> {
        if interval.contig() != self.interval.contig() {
            return Vec::new();
        }
        let mut connectors =
            Vec::with_capacity(self.connector_count_for_rows(rows.clone(), interval));
        connectors.extend(self.connectors.iter().filter(|connector| {
            rows.contains(&connector.row)
                && connector.start < interval.end()
                && connector.end > interval.start()
        }));
        connectors
    }

    #[must_use]
    pub fn connector_count_for_rows(&self, rows: Range<u32>, interval: GenomicInterval) -> usize {
        if interval.contig() != self.interval.contig() {
            return 0;
        }
        self.connectors
            .iter()
            .filter(|connector| {
                rows.contains(&connector.row)
                    && connector.start < interval.end()
                    && connector.end > interval.start()
            })
            .count()
    }

    #[must_use]
    pub const fn assignment_fingerprint(&self) -> ContractFingerprint {
        self.assignment_fingerprint
    }

    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    #[must_use]
    pub fn record(&self, handle: AlignmentHandle) -> Option<RowAtlasRecordHit> {
        self.layers
            .iter()
            .enumerate()
            .rev()
            .find_map(|(layer, records)| {
                let layer = u32::try_from(layer).ok()?;
                records
                    .record(handle)
                    .map(|(record_index, record)| RowAtlasRecordHit {
                        record,
                        layer,
                        record_index,
                    })
            })
    }

    /// Returns deterministic draw/pick order for one row and genomic interval without projecting
    /// the complete atlas into a query-local array.
    #[must_use]
    pub fn records_overlapping(
        &self,
        row: u32,
        interval: GenomicInterval,
    ) -> Vec<RowAtlasRecordHit> {
        if interval.contig() != self.interval.contig() {
            return Vec::new();
        }
        let maximum = self
            .layers
            .iter()
            .map(|records| records.overlapping_count(row, interval))
            .sum();
        let mut hits = Vec::with_capacity(maximum);
        for (layer, records) in self.layers.iter().enumerate() {
            if let Ok(layer) = u32::try_from(layer) {
                records.visit_overlapping(row, interval, layer, |hit| hits.push(hit));
            }
        }
        hits.sort_unstable_by_key(|hit| {
            (
                hit.record.start,
                hit.record.end,
                hit.record.handle,
                hit.layer,
            )
        });
        hits
    }

    #[must_use]
    pub fn records_at(&self, row: u32, position: u64) -> Vec<RowAtlasRecordHit> {
        let Some(end) = position.checked_add(1) else {
            return Vec::new();
        };
        let Ok(interval) = GenomicInterval::new(self.interval.contig(), position, end) else {
            return Vec::new();
        };
        self.records_overlapping(row, interval)
    }

    /// Selects only visible row blocks and genomic spans. This is the semantic-page input path;
    /// camera containment never changes row assignments.
    #[must_use]
    pub fn records_for_rows(
        &self,
        rows: Range<u32>,
        interval: GenomicInterval,
    ) -> Vec<RowAtlasRecordHit> {
        if interval.contig() != self.interval.contig() {
            return Vec::new();
        }
        let mut records = Vec::with_capacity(self.record_count_for_rows(rows.clone(), interval));
        for row in rows {
            for (layer, layer_records) in self.layers.iter().enumerate() {
                if let Ok(layer) = u32::try_from(layer) {
                    layer_records.visit_overlapping(row, interval, layer, |hit| records.push(hit));
                }
            }
        }
        records.sort_unstable_by_key(|hit| {
            (
                hit.record.row,
                hit.record.start,
                hit.record.end,
                hit.record.handle,
            )
        });
        records
    }

    #[must_use]
    pub fn record_count_for_rows(&self, rows: Range<u32>, interval: GenomicInterval) -> usize {
        if interval.contig() != self.interval.contig() {
            return 0;
        }
        rows.fold(0_usize, |total, row| {
            self.layers.iter().fold(total, |total, records| {
                total.saturating_add(records.overlapping_count(row, interval))
            })
        })
    }

    pub fn pin(&self) -> Result<AlignmentRowAtlasPins, LedgerError> {
        let mut allocations = Vec::with_capacity(self.layers.len().saturating_add(1));
        allocations.push(self.metadata_allocation.pin()?);
        for layer in self.layers.iter() {
            allocations.push(layer.allocation.pin()?);
        }
        Ok(AlignmentRowAtlasPins { allocations })
    }

    pub(crate) fn touch(
        &self,
        ledger: &AllocationLedger,
        revision: u64,
    ) -> Result<(), LedgerError> {
        ledger.touch(self.metadata_allocation.id(), revision)?;
        for layer in self.layers.iter() {
            ledger.touch(layer.allocation.id(), revision)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct AlignmentRowAtlasPins {
    allocations: Vec<PinnedAllocationLease>,
}

impl AlignmentRowAtlasPins {
    #[must_use]
    pub fn len(&self) -> usize {
        self.allocations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allocations.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
struct RowInput {
    handle: AlignmentHandle,
    start: u64,
    end: u64,
}

fn assign_extension_rows(
    records: &[RowInput],
    extents: &mut Vec<RowExtent>,
    incumbent: GenomicInterval,
) -> Result<Vec<RowAtlasRecord>, AlignmentRowAtlasError> {
    let split = records.partition_point(|record| record.start < incumbent.start());
    let (left, right) = records.split_at(split);
    let mut assigned = Vec::with_capacity(records.len());
    if !left.is_empty() {
        // A read retained by the incumbent atlas may begin far before the atlas interval (for
        // example, a long spliced RNA alignment). Preserve those incumbent assignments while
        // placing left-extension reads: a conflicting low-numbered row is skipped, not treated as
        // proof that the complete evidence is inconsistent. Keep the original incumbent starts
        // separate because `extents` is updated as new left records are appended.
        let mut incumbent_minimum_starts = extents
            .iter()
            .map(|extent| extent.minimum_start)
            .collect::<Vec<_>>();
        let mut active_by_end = BinaryHeap::<Reverse<(u64, u32)>>::new();
        let mut available_rows = (0..extents.len())
            .map(|row| {
                u32::try_from(row)
                    .map(Reverse)
                    .map_err(|_| AlignmentRowAtlasError::IndexOverflow)
            })
            .collect::<Result<BinaryHeap<_>, _>>()?;
        for record in left {
            validate_record(*record)?;
            while active_by_end
                .peek()
                .is_some_and(|Reverse((end, _))| *end <= record.start)
            {
                let Reverse((_, row)) = active_by_end
                    .pop()
                    .expect("a reusable stable left row was observed");
                available_rows.push(Reverse(row));
            }
            let mut blocked = Vec::new();
            let mut compatible = None;
            while let Some(Reverse(row)) = available_rows.pop() {
                if incumbent_minimum_starts[row as usize] >= record.end {
                    compatible = Some(row);
                    break;
                }
                blocked.push(Reverse(row));
            }
            available_rows.extend(blocked);
            let row = if let Some(row) = compatible {
                row
            } else {
                let row = u32::try_from(extents.len())
                    .map_err(|_| AlignmentRowAtlasError::IndexOverflow)?;
                extents.push(empty_extent());
                incumbent_minimum_starts.push(u64::MAX);
                row
            };
            update_extent(&mut extents[row as usize], *record)?;
            active_by_end.push(Reverse((record.end, row)));
            assigned.push(RowAtlasRecord {
                handle: record.handle,
                row,
                start: record.start,
                end: record.end,
            });
        }
    }
    if !right.is_empty() {
        let mut active_by_end = extents
            .iter()
            .enumerate()
            .map(|(row, extent)| {
                Ok(Reverse((
                    extent.maximum_end,
                    u32::try_from(row).map_err(|_| AlignmentRowAtlasError::IndexOverflow)?,
                )))
            })
            .collect::<Result<BinaryHeap<_>, AlignmentRowAtlasError>>()?;
        let mut available_rows = BinaryHeap::<Reverse<u32>>::new();
        for record in right {
            validate_record(*record)?;
            while active_by_end
                .peek()
                .is_some_and(|Reverse((end, _))| *end <= record.start)
            {
                let Reverse((_, row)) = active_by_end
                    .pop()
                    .expect("a reusable stable right row was observed");
                available_rows.push(Reverse(row));
            }
            let row = if let Some(Reverse(row)) = available_rows.pop() {
                row
            } else {
                let row = u32::try_from(extents.len())
                    .map_err(|_| AlignmentRowAtlasError::IndexOverflow)?;
                extents.push(empty_extent());
                row
            };
            update_extent(&mut extents[row as usize], *record)?;
            active_by_end.push(Reverse((record.end, row)));
            assigned.push(RowAtlasRecord {
                handle: record.handle,
                row,
                start: record.start,
                end: record.end,
            });
        }
    }
    Ok(assigned)
}

const fn empty_extent() -> RowExtent {
    RowExtent {
        minimum_start: u64::MAX,
        maximum_end: 0,
        record_count: 0,
    }
}

fn update_extent(extent: &mut RowExtent, record: RowInput) -> Result<(), AlignmentRowAtlasError> {
    extent.minimum_start = extent.minimum_start.min(record.start);
    extent.maximum_end = extent.maximum_end.max(record.end);
    extent.record_count = extent
        .record_count
        .checked_add(1)
        .ok_or(AlignmentRowAtlasError::IndexOverflow)?;
    Ok(())
}

fn validate_record(record: RowInput) -> Result<(), AlignmentRowAtlasError> {
    if record.start >= record.end {
        return Err(AlignmentRowAtlasError::InvalidRecordSpan {
            start: record.start,
            end: record.end,
        });
    }
    Ok(())
}

const fn supports_layered_extension(contract: LayoutContract) -> bool {
    matches!(contract.grouping, AlignmentGroupMode::None)
        && matches!(contract.group_order, AlignmentOrderDirection::Natural)
        && matches!(contract.sorting, AlignmentSortMode::Start)
        && matches!(contract.sort_order, AlignmentOrderDirection::Natural)
        && contract.sort_anchor.is_none()
        && !contract.view_as_pairs
        && contract.promoted_template.is_none()
}

fn maximum_selected_records(evidence: &EvidenceRoot, interval: GenomicInterval) -> usize {
    evidence
        .exact_filter_provenance(interval)
        .and_then(|provenance| usize::try_from(provenance.eligible_records).ok())
        .unwrap_or_else(|| evidence.record_count_upper_bound())
}

const fn layer_bytes(records: usize, row_order: usize, prefix: usize) -> usize {
    size_of::<RowAtlasLayer>()
        .saturating_add(records.saturating_mul(size_of::<RowAtlasRecord>()))
        .saturating_add(row_order.saturating_mul(size_of::<u32>()))
        .saturating_add(prefix.saturating_mul(size_of::<u64>()))
}

const fn atlas_metadata_bytes(rows: usize, layers: usize) -> usize {
    size_of::<AlignmentRowAtlas>()
        .saturating_add(rows.saturating_mul(size_of::<RowExtent>()))
        .saturating_add(layers.saturating_mul(size_of::<Arc<RowAtlasLayer>>()))
}

fn row_group_bytes(groups: &[AlignmentRowGroup]) -> usize {
    groups.iter().map(AlignmentRowGroup::retained_bytes).sum()
}

const fn connector_bytes(connectors: usize) -> usize {
    connectors.saturating_mul(size_of::<AlignmentRowConnector>())
}

fn assignment_fingerprint(
    contract: LayoutContract,
    layers: &[Arc<RowAtlasLayer>],
    record_count: usize,
) -> ContractFingerprint {
    let mut xor = 0_u64;
    let mut sum = 0_u64;
    for layer in layers {
        for record in layer.records.iter() {
            let mut hash = StableHasher::default();
            write_handle(&mut hash, record.handle);
            hash.write(&record.row.to_le_bytes());
            hash.write(&record.start.to_le_bytes());
            hash.write(&record.end.to_le_bytes());
            let value = hash.finish().get();
            xor ^= value.rotate_left((record.row % 63).saturating_add(1));
            sum = sum.wrapping_add(value.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        }
    }
    let mut hash = StableHasher::default();
    hash.write(b"alignment-row-atlas-v1\0");
    hash.write(&contract.fingerprint().get().to_le_bytes());
    hash.write(&record_count.to_le_bytes());
    hash.write(&xor.to_le_bytes());
    hash.write(&sum.to_le_bytes());
    hash.finish()
}

fn write_handle(hash: &mut StableHasher, handle: AlignmentHandle) {
    let source = handle.source_revision();
    hash.write(&source.source().fingerprint().high().to_le_bytes());
    hash.write(&source.source().fingerprint().low().to_le_bytes());
    hash.write(&source.revision().high().to_le_bytes());
    hash.write(&source.revision().low().to_le_bytes());
    match handle.source_position() {
        AlignmentSourcePosition::BamVirtualOffset(offset) => {
            hash.write(&[0]);
            hash.write(&offset.to_le_bytes());
        }
        AlignmentSourcePosition::CramRecord {
            container_offset,
            slice_landmark,
            record_index,
        } => {
            hash.write(&[1]);
            hash.write(&container_offset.to_le_bytes());
            hash.write(&slice_landmark.to_le_bytes());
            hash.write(&record_index.to_le_bytes());
        }
    }
}

#[derive(Debug, Error)]
pub enum AlignmentRowAtlasError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    EvidenceSelection(#[from] EvidenceSelectionError),
    #[error(transparent)]
    RowOracle(#[from] RowOracleError),
    #[error("row atlas evidence belongs to a different source revision")]
    SourceRevisionMismatch,
    #[error("row atlas evidence belongs to a different record contract")]
    RecordContractMismatch,
    #[error("row atlas target is not a containing same-contig extension")]
    NonContainingExtension,
    #[error("row atlas target {0:?} is not covered by complete evidence")]
    UncertifiedInterval(GenomicInterval),
    #[error("row atlas record {start}..{end} has an invalid span")]
    InvalidRecordSpan { start: u64, end: u64 },
    #[error("row atlas evidence contains an invalid record reference")]
    InvalidEvidenceRecord,
    #[error("row atlas compact index capacity was exceeded")]
    IndexOverflow,
    #[error("a physical record handle occurs more than once in one atlas layer")]
    DuplicateHandle,
    #[error(
        "new complete evidence introduced record {handle:?} at {start}..{end} inside incumbent atlas interval {incumbent:?}"
    )]
    NovelRecordInsideIncumbent {
        handle: AlignmentHandle,
        start: u64,
        end: u64,
        incumbent: GenomicInterval,
    },
    #[error("row atlas extension interval could not be represented")]
    InvalidExtensionInterval,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rgv_core::{ContigId, ReadStackingMode};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, SequenceData,
        TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AllocationClass, CanonicalRecordChunk, CapabilityRequirement, CompleteCoverage,
        EvidenceAcquisition, EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance,
        PayloadContract, PayloadSidecar, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(91);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn contract(stacking: ReadStackingMode) -> LayoutContract {
        LayoutContract {
            row_scale_class: 0,
            stacking,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        }
    }

    fn record(name: &[u8], start: u64, end: u64) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    fn evidence_chunk(
        ledger: &AllocationLedger,
        records: &[(&[u8], u64, u64, u64)],
    ) -> Arc<EvidenceChunk> {
        let mut batch = AlignmentBatch::default();
        for (name, start, end, in_block) in records {
            batch
                .push_with_source_position(
                    &record(name, *start, *end),
                    AlignmentSourcePosition::bam_virtual_offset((7 << 16) | in_block),
                )
                .unwrap();
        }
        let chunk = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(batch),
            RECORD_CONTRACT,
        )
        .unwrap();
        let footprint = EvidenceChunk::footprint(&chunk);
        let mut core = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core = core.register(footprint.core_bytes, 1).unwrap();
        let mut overlap = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap = overlap.register(footprint.overlap_index_bytes, 1).unwrap();
        Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap())
    }

    fn evidence_root(
        ledger: &AllocationLedger,
        chunk: Arc<EvidenceChunk>,
        interval: GenomicInterval,
        revision: u64,
    ) -> Arc<EvidenceRoot> {
        let eligible = chunk.overlapping_record_indices(interval).len() as u64;
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: eligible,
                eligible_records: eligible,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let payload = PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [],
            None,
        );
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(0, revision)),
            source_revision(),
            RECORD_CONTRACT,
            payload,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(revision).unwrap()
    }

    fn revision(value: u64) -> RowAtlasRevision {
        RowAtlasRevision::new(RevisionFingerprint::new(0, value))
    }

    fn rich_record(
        name: &[u8],
        start: u64,
        flags: u16,
        mate_start: Option<u64>,
        template_length: i64,
        group: &str,
        haplotype: i64,
        chimeric: bool,
    ) -> AlignmentRecord {
        let mut auxiliary_fields = vec![
            AuxField {
                tag: *b"RG",
                value: AuxValue::String(group.to_owned()),
            },
            AuxField {
                tag: *b"HP",
                value: AuxValue::Signed(haplotype),
            },
            AuxField {
                tag: *b"ZZ",
                value: AuxValue::String(format!("tag-{group}")),
            },
        ];
        if chimeric {
            auxiliary_fields.push(AuxField {
                tag: *b"SA",
                value: AuxValue::String("chr1,100,+,4M,60,0;".to_owned()),
            });
        }
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end: start + 4,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60_u8.saturating_sub(start as u8)),
            mate_contig: mate_start.map(|_| ContigId::new(0)),
            mate_start,
            template_length,
            read_name: name.to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGT".to_vec(),
                qualities: Some(vec![30; 4]),
            },
            auxiliary_fields,
        }
    }

    fn rich_evidence_root(ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let records = [
            rich_record(b"pair", 10, 0x63, Some(30), 24, "alpha", 1, false),
            rich_record(b"supp", 16, 0x810, None, 0, "beta", 2, true),
            rich_record(b"pair", 30, 0x93, Some(10), -24, "alpha", 1, false),
            rich_record(b"single", 50, 0, None, 0, "beta", 2, false),
        ];
        let mut decoded = AlignmentBatch::default();
        for (index, record) in records.iter().enumerate() {
            decoded
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (9 << 16) | u64::try_from(index + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 9,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let payload_allocation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(payload_bytes, 1)
            .unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, 1)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: 4,
                eligible_records: 4,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(0, 100)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(100).unwrap()
    }

    #[test]
    fn canonical_rows_and_interval_picker_use_stable_handles() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let chunk = evidence_chunk(
            &ledger,
            &[
                (b"a", 5, 15, 1),
                (b"b", 10, 20, 2),
                (b"c", 15, 25, 3),
                (b"long", 18, 60, 4),
            ],
        );
        let interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let evidence = evidence_root(&ledger, chunk, interval, 10);
        let atlas = AlignmentRowAtlas::build(
            revision(20),
            &evidence,
            interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();

        assert_eq!(atlas.record_count(), 4);
        assert_eq!(atlas.row_count(), 3);
        let row_zero = atlas.records_at(0, 19);
        assert_eq!(row_zero.len(), 1);
        let row_one = atlas.records_at(1, 19);
        assert_eq!(row_one.len(), 1);
        let row_two = atlas.records_at(2, 19);
        assert_eq!(row_two.len(), 1);
        assert_ne!(row_zero[0].record.handle(), row_one[0].record.handle());
        assert_eq!(atlas.record(row_zero[0].record.handle()), Some(row_zero[0]));
        assert_eq!(atlas.pin().unwrap().len(), 2);
    }

    #[test]
    fn contained_camera_selection_neither_repacks_nor_allocates_residency() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let chunk = evidence_chunk(
            &ledger,
            &[(b"a", 5, 15, 1), (b"b", 10, 20, 2), (b"c", 30, 35, 3)],
        );
        let interval = GenomicInterval::new(ContigId::new(0), 0, 40).unwrap();
        let evidence = evidence_root(&ledger, chunk, interval, 10);
        let atlas = AlignmentRowAtlas::build(
            revision(20),
            &evidence,
            interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let fingerprint = atlas.assignment_fingerprint();
        let before = ledger.snapshot().unwrap();
        let camera = GenomicInterval::new(ContigId::new(0), 8, 16).unwrap();
        let expected = atlas.records_for_rows(0..2, camera);
        assert_eq!(atlas.record_count_for_rows(0..2, camera), expected.len());
        assert_eq!(
            atlas.connector_count_for_rows(0..2, camera),
            atlas.connectors_for_rows(0..2, camera).len()
        );
        for _ in 0..10_000 {
            let _ = atlas.records_for_rows(0..2, camera);
        }
        let after = ledger.snapshot().unwrap();

        assert_eq!(atlas.assignment_fingerprint(), fingerprint);
        assert_eq!(atlas.layer_count(), 1);
        assert_eq!(after.charged, before.charged);
        assert_eq!(after.records, before.records);
    }

    #[test]
    fn incremental_right_extension_matches_clean_canonical_atlas() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let chunk = evidence_chunk(
            &ledger,
            &[
                (b"a", 5, 15, 1),
                (b"b", 10, 20, 2),
                (b"long", 25, 60, 3),
                (b"right-a", 42, 50, 4),
                (b"right-b", 50, 55, 5),
                (b"right-c", 61, 70, 6),
            ],
        );
        let narrow_interval = GenomicInterval::new(ContigId::new(0), 0, 40).unwrap();
        let wide_interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let narrow = evidence_root(&ledger, Arc::clone(&chunk), narrow_interval, 10);
        let wide = evidence_root(&ledger, chunk, wide_interval, 11);
        let narrow_atlas = AlignmentRowAtlas::build(
            revision(20),
            &narrow,
            narrow_interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let extended = narrow_atlas
            .extend(
                revision(21),
                &wide,
                wide_interval,
                &ledger,
                ReservationKind::Normal,
                21,
            )
            .unwrap();
        let clean = AlignmentRowAtlas::build(
            revision(22),
            &wide,
            wide_interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            22,
        )
        .unwrap();

        assert_eq!(extended.record_count(), 6);
        assert_eq!(extended.layer_count(), 2);
        assert_eq!(extended.row_count(), clean.row_count());
        assert_eq!(
            extended.assignment_fingerprint(),
            clean.assignment_fingerprint()
        );
        for reference in wide.select(wide_interval).unwrap().records() {
            assert_eq!(
                extended.record(reference.handle).unwrap().record.row(),
                clean.record(reference.handle).unwrap().record.row()
            );
        }
    }

    #[test]
    fn stable_left_extension_preserves_every_incumbent_assignment() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let chunk = evidence_chunk(
            &ledger,
            &[
                (b"left-a", 5, 15, 1),
                (b"left-b", 10, 20, 2),
                (b"middle-a", 45, 55, 3),
                (b"middle-b", 48, 60, 4),
            ],
        );
        let middle_interval = GenomicInterval::new(ContigId::new(0), 40, 80).unwrap();
        let wide_interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let middle = evidence_root(&ledger, Arc::clone(&chunk), middle_interval, 10);
        let wide = evidence_root(&ledger, chunk, wide_interval, 11);
        let incumbent = AlignmentRowAtlas::build(
            revision(20),
            &middle,
            middle_interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let incumbent_rows = middle
            .select(middle_interval)
            .unwrap()
            .records()
            .iter()
            .map(|record| {
                (
                    record.handle,
                    incumbent.record(record.handle).unwrap().record.row(),
                )
            })
            .collect::<Vec<_>>();
        let extended = incumbent
            .extend(
                revision(21),
                &wide,
                wide_interval,
                &ledger,
                ReservationKind::Normal,
                21,
            )
            .unwrap();

        for (handle, row) in incumbent_rows {
            assert_eq!(extended.record(handle).unwrap().record.row(), row);
        }
        assert_eq!(extended.layer_count(), 2);
    }

    #[test]
    fn stable_left_extension_allocates_around_long_incumbent_spans() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let chunk = evidence_chunk(
            &ledger,
            &[
                (b"incumbent-long", 0, 100, 1),
                (b"left-a", 5, 10, 2),
                (b"left-b", 15, 20, 3),
            ],
        );
        let middle_interval = GenomicInterval::new(ContigId::new(0), 40, 80).unwrap();
        let wide_interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let middle = evidence_root(&ledger, Arc::clone(&chunk), middle_interval, 10);
        let wide = evidence_root(&ledger, chunk, wide_interval, 11);
        let incumbent = AlignmentRowAtlas::build(
            revision(20),
            &middle,
            middle_interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let incumbent_handle = middle.select(middle_interval).unwrap().records()[0].handle;
        let incumbent_row = incumbent.record(incumbent_handle).unwrap().record.row();

        let extended = incumbent
            .extend(
                revision(21),
                &wide,
                wide_interval,
                &ledger,
                ReservationKind::Normal,
                21,
            )
            .unwrap();
        let wide_selection = wide.select(wide_interval).unwrap();
        let left_rows = wide_selection
            .records()
            .iter()
            .filter(|record| record.handle != incumbent_handle)
            .map(|record| extended.record(record.handle).unwrap().record.row())
            .collect::<Vec<_>>();

        assert_eq!(
            extended.record(incumbent_handle).unwrap().record.row(),
            incumbent_row
        );
        assert_eq!(left_rows, vec![1, 1]);
        assert_eq!(extended.row_count(), 2);
    }

    #[test]
    fn row_allocations_are_exactly_charged_and_shared_between_roots() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let chunk = evidence_chunk(&ledger, &[(b"a", 5, 15, 1), (b"b", 45, 55, 2)]);
        let narrow_interval = GenomicInterval::new(ContigId::new(0), 0, 40).unwrap();
        let wide_interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let narrow = evidence_root(&ledger, Arc::clone(&chunk), narrow_interval, 10);
        let wide = evidence_root(&ledger, chunk, wide_interval, 11);
        let incumbent = AlignmentRowAtlas::build(
            revision(20),
            &narrow,
            narrow_interval,
            contract(ReadStackingMode::Stable),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let extended = incumbent
            .extend(
                revision(21),
                &wide,
                wide_interval,
                &ledger,
                ReservationKind::Normal,
                21,
            )
            .unwrap();
        let pins = extended.pin().unwrap();
        let snapshot = ledger.snapshot().unwrap();
        let row_allocations = snapshot
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::RowAtlas)
            .collect::<Vec<_>>();

        assert_eq!(pins.len(), 3);
        assert_eq!(row_allocations.len(), 4);
        assert_eq!(
            row_allocations
                .iter()
                .filter(|record| record.pins == 1)
                .count(),
            3
        );
        assert!(
            row_allocations
                .iter()
                .all(|record| record.charged_capacity > 0)
        );
        assert_eq!(NonZeroUsize::new(extended.record_count()).unwrap().get(), 2);
    }

    #[test]
    fn every_group_and_sort_mode_is_scene_native_and_deterministic() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = rich_evidence_root(&ledger);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let group_modes = [
            AlignmentGroupMode::None,
            AlignmentGroupMode::Strand,
            AlignmentGroupMode::ReadGroup,
            AlignmentGroupMode::Haplotype,
            AlignmentGroupMode::Chimeric,
            AlignmentGroupMode::Supplementary,
            AlignmentGroupMode::LeftClip,
            AlignmentGroupMode::RightClip,
            AlignmentGroupMode::PairOrientation,
            AlignmentGroupMode::UnexpectedPair,
            AlignmentGroupMode::MateContig,
            AlignmentGroupMode::ReadOrder,
            AlignmentGroupMode::FirstOfPairStrand,
            AlignmentGroupMode::Tag(*b"ZZ"),
        ];
        let sort_modes = [
            AlignmentSortMode::Start,
            AlignmentSortMode::Strand,
            AlignmentSortMode::FirstOfPairStrand,
            AlignmentSortMode::MappingQuality,
            AlignmentSortMode::ReadName,
            AlignmentSortMode::ReadGroup,
            AlignmentSortMode::Haplotype,
            AlignmentSortMode::AlignedLength,
            AlignmentSortMode::LeftClip,
            AlignmentSortMode::RightClip,
            AlignmentSortMode::TemplateLength,
            AlignmentSortMode::PairOrientation,
            AlignmentSortMode::MateContig,
            AlignmentSortMode::PairStatus,
            AlignmentSortMode::ReadOrder,
            AlignmentSortMode::Base,
            AlignmentSortMode::Tag(*b"ZZ"),
        ];

        for (index, grouping) in group_modes.into_iter().enumerate() {
            let mut layout = contract(ReadStackingMode::Compact);
            layout.grouping = grouping;
            layout.group_order = if index % 2 == 0 {
                AlignmentOrderDirection::Natural
            } else {
                AlignmentOrderDirection::Reverse
            };
            let first = AlignmentRowAtlas::build(
                revision(200 + index as u64 * 2),
                &evidence,
                interval,
                layout,
                &ledger,
                ReservationKind::Normal,
                200 + index as u64 * 2,
            )
            .unwrap();
            let second = AlignmentRowAtlas::build(
                revision(201 + index as u64 * 2),
                &evidence,
                interval,
                layout,
                &ledger,
                ReservationKind::Normal,
                201 + index as u64 * 2,
            )
            .unwrap();
            assert_eq!(first.record_count(), 4, "grouping {grouping:?}");
            assert_eq!(
                first.assignment_fingerprint(),
                second.assignment_fingerprint(),
                "grouping {grouping:?}"
            );
            assert!(!first.groups().is_empty());
        }

        for (index, sorting) in sort_modes.into_iter().enumerate() {
            let mut layout = contract(ReadStackingMode::Compact);
            layout.sorting = sorting;
            layout.sort_order = if index % 2 == 0 {
                AlignmentOrderDirection::Natural
            } else {
                AlignmentOrderDirection::Reverse
            };
            layout.sort_anchor = (sorting == AlignmentSortMode::Base).then_some(12);
            let atlas = AlignmentRowAtlas::build(
                revision(300 + index as u64),
                &evidence,
                interval,
                layout,
                &ledger,
                ReservationKind::Normal,
                300 + index as u64,
            )
            .unwrap();
            assert_eq!(atlas.record_count(), 4, "sorting {sorting:?}");
            assert_eq!(
                atlas
                    .records_for_rows(0..u32::try_from(atlas.row_count()).unwrap(), interval)
                    .len(),
                4,
                "sorting {sorting:?}"
            );
        }
    }

    #[test]
    fn pair_view_shares_rows_and_promotion_is_a_typed_layout_decision() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let evidence = rich_evidence_root(&ledger);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let selection = evidence.select(interval).unwrap();
        let pair_handles = selection
            .records()
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                (selection.record_view(index).unwrap().read_name() == b"pair")
                    .then_some(record.handle)
            })
            .collect::<Vec<_>>();
        assert_eq!(pair_handles.len(), 2);

        let mut layout = contract(ReadStackingMode::Stable);
        layout.view_as_pairs = true;
        let paired = AlignmentRowAtlas::build(
            revision(400),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            400,
        )
        .unwrap();
        assert_eq!(
            paired.record(pair_handles[0]).unwrap().record.row(),
            paired.record(pair_handles[1]).unwrap().record.row()
        );
        let connectors =
            paired.connectors_for_rows(0..u32::try_from(paired.row_count()).unwrap(), interval);
        assert_eq!(connectors.len(), 1);
        assert_eq!((connectors[0].start(), connectors[0].end()), (14, 30));
        assert_eq!(connectors[0].right(), Some(pair_handles[1]));

        layout.promoted_template = Some(rgv_hts::template_id(b"supp").get());
        let promoted = AlignmentRowAtlas::build(
            revision(401),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            401,
        )
        .unwrap();
        let supplementary = selection
            .records()
            .iter()
            .enumerate()
            .find_map(|(index, record)| {
                (selection.record_view(index).unwrap().read_name() == b"supp")
                    .then_some(record.handle)
            })
            .unwrap();
        assert_eq!(promoted.record(supplementary).unwrap().record.row(), 0);
    }

    #[test]
    fn row_modes_refuse_missing_payload_instead_of_faking_missing_values() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 80).unwrap();
        let chunk = evidence_chunk(&ledger, &[(b"a", 5, 15, 1)]);
        let evidence = evidence_root(&ledger, chunk, interval, 10);
        let mut layout = contract(ReadStackingMode::Stable);
        layout.grouping = AlignmentGroupMode::ReadGroup;
        assert!(matches!(
            AlignmentRowAtlas::build(
                revision(500),
                &evidence,
                interval,
                layout,
                &ledger,
                ReservationKind::Normal,
                500,
            ),
            Err(AlignmentRowAtlasError::RowOracle(
                RowOracleError::AuxiliaryPayloadRequired(tag)
            )) if tag == *b"RG"
        ));
    }
}
