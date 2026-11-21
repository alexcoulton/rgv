use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BinaryHeap, HashMap},
    sync::Arc,
};

use rgv_core::{AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, GenomicInterval};
use rgv_hts::{
    AlignmentFlags, AuxValue, CigarOp, CigarOpKind, PairClassification, PairEvidence,
    PairOrientation, ReadOrder, SequenceView, pair_evidence_view, template_id,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, CapabilityRequirement, EvidenceRoot, EvidenceSelection,
    EvidenceSelectionError, LayoutContract, ReadSampling,
    sampling::{SamplingCandidate, retained_by_sampling, sampling_priority},
};

/// Stable semantic identity for a contiguous row group. Dynamic tag values are retained as
/// immutable strings and are part of the row-atlas allocation, never camera-local UI labels.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AlignmentRowGroupKey {
    All,
    Forward,
    Reverse,
    Tagged(Arc<str>),
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentRowGroup {
    key: AlignmentRowGroupKey,
    row_start: u32,
    row_end: u32,
}

impl AlignmentRowGroup {
    pub(crate) fn all(row_end: u32) -> Self {
        Self {
            key: AlignmentRowGroupKey::All,
            row_start: 0,
            row_end,
        }
    }

    #[must_use]
    pub const fn key(&self) -> &AlignmentRowGroupKey {
        &self.key
    }

    #[must_use]
    pub const fn row_range(&self) -> std::ops::Range<u32> {
        self.row_start..self.row_end
    }

    #[must_use]
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(match &self.key {
            AlignmentRowGroupKey::Tagged(label) => label.len(),
            _ => 0,
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OracleGroupKey {
    All,
    Forward,
    Reverse,
    Tagged(String),
    Missing,
}

#[derive(Clone, Debug)]
pub(crate) struct OracleGroup {
    key: OracleGroupKey,
    row_start: u32,
    row_end: u32,
}

impl OracleGroup {
    #[must_use]
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<AlignmentRowGroup>().saturating_add(match &self.key {
            OracleGroupKey::Tagged(label) => label.len(),
            _ => 0,
        })
    }

    pub(crate) fn into_retained(self) -> AlignmentRowGroup {
        AlignmentRowGroup {
            key: match self.key {
                OracleGroupKey::All => AlignmentRowGroupKey::All,
                OracleGroupKey::Forward => AlignmentRowGroupKey::Forward,
                OracleGroupKey::Reverse => AlignmentRowGroupKey::Reverse,
                OracleGroupKey::Tagged(label) => AlignmentRowGroupKey::Tagged(Arc::from(label)),
                OracleGroupKey::Missing => AlignmentRowGroupKey::Missing,
            },
            row_start: self.row_start,
            row_end: self.row_end,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OracleAssignment {
    pub handle: AlignmentHandle,
    pub row: u32,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OracleRowExtent {
    pub minimum_start: u64,
    pub maximum_end: u64,
    pub record_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OraclePairConnector {
    pub template: u64,
    pub left: AlignmentHandle,
    pub right: Option<AlignmentHandle>,
    pub row: u32,
    pub start: u64,
    pub end: u64,
}

pub(crate) struct RowOracleOutput {
    pub assignments: Vec<OracleAssignment>,
    pub extents: Vec<OracleRowExtent>,
    pub groups: Vec<OracleGroup>,
    pub connectors: Vec<OraclePairConnector>,
    /// Eligible reads overlapping the interval before read sampling.
    pub eligible_records: usize,
}

#[derive(Clone, Debug)]
struct Candidate {
    handle: AlignmentHandle,
    start: u64,
    end: u64,
    contig: rgv_core::ContigId,
    flags: AlignmentFlags,
    mapping_quality: Option<u8>,
    mate_contig: Option<rgv_core::ContigId>,
    mate_start: Option<u64>,
    template_length: i64,
    read_name: Vec<u8>,
    cigar: Vec<CigarOp>,
    group_tag: Option<String>,
    sort_tag: Option<String>,
    has_sa: bool,
    pair: PairEvidence,
    base_sort: (u8, u8),
    position_group: Option<String>,
}

#[derive(Clone, Debug)]
struct RowUnit {
    representative: usize,
    members: Vec<usize>,
    start: u64,
    end: u64,
    paired: bool,
}

/// Runs the complete deterministic group/sort/pair oracle directly over immutable evidence.
/// Query-local indices are scratch only and never escape into the atlas.
pub(crate) fn build_row_oracle(
    evidence: &Arc<EvidenceRoot>,
    interval: GenomicInterval,
    contract: LayoutContract,
) -> Result<RowOracleOutput, RowOracleError> {
    validate_capabilities(evidence, contract)?;
    let selection = evidence.select(interval)?;
    let candidates = build_candidates(&selection, interval, contract)?;
    let units = build_units(&candidates, contract.view_as_pairs);
    let mut grouped = BTreeMap::<OracleGroupKey, Vec<usize>>::new();
    for (unit_index, unit) in units.iter().enumerate() {
        grouped
            .entry(group_key(
                &candidates[unit.representative],
                contract.grouping,
            ))
            .or_default()
            .push(unit_index);
    }

    if grouped.is_empty() {
        grouped.insert(OracleGroupKey::All, Vec::new());
    }
    for group in grouped.values_mut() {
        group.sort_by(|left, right| {
            compare_units(&units[*left], &units[*right], &candidates, contract.sorting)
        });
        if contract.sort_order == AlignmentOrderDirection::Reverse {
            group.reverse();
        }
        if let Some(promoted) = contract.promoted_template {
            group.sort_by_key(|unit| {
                template_id(&candidates[units[*unit].representative].read_name).get() != promoted
            });
        }
    }

    let ordered_groups = match contract.group_order {
        AlignmentOrderDirection::Natural => grouped.into_iter().collect::<Vec<_>>(),
        AlignmentOrderDirection::Reverse => grouped.into_iter().rev().collect::<Vec<_>>(),
    };
    let mut output = pack_groups(&candidates, &units, ordered_groups)?;
    output.eligible_records = selection.records().len();
    Ok(output)
}

fn build_candidates(
    selection: &EvidenceSelection,
    interval: GenomicInterval,
    contract: LayoutContract,
) -> Result<Vec<Candidate>, RowOracleError> {
    let group_tag = mode_tag(contract.grouping);
    let sort_tag = sort_tag(contract.sorting);
    let retained = sampled_selection(selection, contract.sampling)?;
    selection
        .records()
        .iter()
        .enumerate()
        .filter(|(index, _)| retained.as_ref().is_none_or(|retained| retained[*index]))
        .map(|(index, reference)| {
            let record = selection
                .record_view(index)
                .ok_or(RowOracleError::InvalidEvidenceRecord)?;
            let group_tag = group_tag
                .map(|tag| tag_label(selection, index, tag))
                .transpose()?
                .flatten();
            let sort_tag = sort_tag
                .map(|tag| tag_label(selection, index, tag))
                .transpose()?
                .flatten();
            let has_sa = if contract.grouping == AlignmentGroupMode::Chimeric {
                selection.auxiliary_field(index, *b"SA")?.is_some()
            } else {
                false
            };
            let position_base_key = if contract.sorting == AlignmentSortMode::Base
                || contract.grouping == AlignmentGroupMode::Base
            {
                alignment_base_sort_key(
                    record.start(),
                    record.end(),
                    record.cigar(),
                    selection.sequence(index),
                    contract.sort_anchor,
                )
            } else {
                (7, 0)
            };
            let position_group = match contract.grouping {
                AlignmentGroupMode::Base => {
                    Some(alignment_base_group_label(position_base_key).to_owned())
                }
                AlignmentGroupMode::Insertion => Some(alignment_insertion_group_label(
                    record.start(),
                    record.cigar(),
                    contract.sort_anchor,
                )),
                _ => None,
            };
            Ok(Candidate {
                handle: reference.handle,
                start: record.start(),
                end: record.end(),
                contig: record.contig(),
                flags: record.flags(),
                mapping_quality: record.mapping_quality(),
                mate_contig: record.mate_contig(),
                mate_start: record.mate_start(),
                template_length: record.template_length(),
                read_name: record.read_name().to_vec(),
                cigar: record.cigar().to_vec(),
                group_tag,
                sort_tag,
                has_sa,
                pair: pair_evidence_view(record, interval, contract.pair_length_bounds),
                base_sort: position_base_key,
                position_group,
            })
        })
        .collect()
}

/// Evaluates read sampling over the complete selection before any per-record layout scratch is
/// built, so a deep locus pays layout cost only for the reads that receive rows.
fn sampled_selection(
    selection: &EvidenceSelection,
    sampling: ReadSampling,
) -> Result<Option<Vec<bool>>, RowOracleError> {
    if !sampling.is_downsampled() {
        return Ok(None);
    }
    let candidates = selection
        .records()
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            let record = selection
                .record_view(index)
                .ok_or(RowOracleError::InvalidEvidenceRecord)?;
            Ok(SamplingCandidate {
                handle: reference.handle,
                start: record.start(),
                priority: sampling_priority(record.read_name()),
                decided: None,
            })
        })
        .collect::<Result<Vec<_>, RowOracleError>>()?;
    Ok(Some(retained_by_sampling(
        sampling,
        &candidates,
        &HashMap::new(),
    )))
}

fn validate_capabilities(
    evidence: &EvidenceRoot,
    contract: LayoutContract,
) -> Result<(), RowOracleError> {
    if (contract.sorting == AlignmentSortMode::Base
        || contract.grouping == AlignmentGroupMode::Base)
        && evidence.payload().sequence() != CapabilityRequirement::Required
    {
        return Err(RowOracleError::SequencePayloadRequired);
    }
    let mut tags = Vec::new();
    if let Some(tag) = mode_tag(contract.grouping) {
        tags.push(tag);
    }
    if contract.grouping == AlignmentGroupMode::Chimeric {
        tags.push(*b"SA");
    }
    if let Some(tag) = sort_tag(contract.sorting) {
        tags.push(tag);
    }
    tags.sort_unstable();
    tags.dedup();
    if !evidence.payload().all_auxiliary_tags() {
        for tag in tags {
            if evidence
                .payload()
                .auxiliary_tags()
                .binary_search(&tag)
                .is_err()
            {
                return Err(RowOracleError::AuxiliaryPayloadRequired(tag));
            }
        }
    }
    Ok(())
}

const fn mode_tag(mode: AlignmentGroupMode) -> Option<[u8; 2]> {
    match mode {
        AlignmentGroupMode::ReadGroup => Some(*b"RG"),
        AlignmentGroupMode::Haplotype => Some(*b"HP"),
        AlignmentGroupMode::Tag(tag) => Some(tag),
        _ => None,
    }
}

const fn sort_tag(mode: AlignmentSortMode) -> Option<[u8; 2]> {
    match mode {
        AlignmentSortMode::ReadGroup => Some(*b"RG"),
        AlignmentSortMode::Haplotype => Some(*b"HP"),
        AlignmentSortMode::Tag(tag) => Some(tag),
        _ => None,
    }
}

fn tag_label(
    selection: &EvidenceSelection,
    index: usize,
    tag: [u8; 2],
) -> Result<Option<String>, RowOracleError> {
    let Some(field) = selection.auxiliary_field(index, tag)? else {
        return Ok(None);
    };
    Ok(match field.value {
        AuxValue::Character(value) => Some(char::from(value).to_string()),
        AuxValue::Signed(value) => Some(value.to_string()),
        AuxValue::Unsigned(value) => Some(value.to_string()),
        AuxValue::Float(value) => Some(value.to_string()),
        AuxValue::Double(value) => Some(value.to_string()),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => Some(value),
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => None,
    })
}

fn build_units(candidates: &[Candidate], view_as_pairs: bool) -> Vec<RowUnit> {
    let mut units = Vec::<RowUnit>::new();
    let mut first_by_name = HashMap::<Vec<u8>, usize>::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let eligible_pair = view_as_pairs
            && candidate.flags.is_paired()
            && !candidate.flags.is_secondary()
            && !candidate.flags.is_supplementary()
            && !candidate.read_name.is_empty();
        let unit_index = if eligible_pair {
            if let Some(unit) = first_by_name.get(&candidate.read_name).copied() {
                unit
            } else {
                let unit = units.len();
                first_by_name.insert(candidate.read_name.clone(), unit);
                units.push(RowUnit {
                    representative: index,
                    members: Vec::new(),
                    start: candidate.start,
                    end: candidate.end,
                    paired: true,
                });
                unit
            }
        } else {
            let unit = units.len();
            units.push(RowUnit {
                representative: index,
                members: Vec::new(),
                start: candidate.start,
                end: candidate.end,
                paired: false,
            });
            unit
        };
        let unit = &mut units[unit_index];
        unit.members.push(index);
        unit.start = unit.start.min(candidate.start);
        unit.end = unit.end.max(candidate.end);
        if eligible_pair
            && !candidate.flags.is_mate_unmapped()
            && candidate.mate_contig == Some(candidate.contig)
            && let Some(mate_start) = candidate.mate_start
        {
            unit.start = unit.start.min(mate_start);
            unit.end = unit.end.max(mate_start.saturating_add(1));
        }
        if eligible_pair && candidate.template_length > 0 {
            unit.end = unit.end.max(
                candidate
                    .start
                    .saturating_add(candidate.template_length.unsigned_abs()),
            );
        } else if eligible_pair && candidate.template_length < 0 {
            unit.start = unit.start.min(
                candidate
                    .end
                    .saturating_sub(candidate.template_length.unsigned_abs()),
            );
        }
    }
    units
}

fn group_key(candidate: &Candidate, mode: AlignmentGroupMode) -> OracleGroupKey {
    match mode {
        AlignmentGroupMode::None => OracleGroupKey::All,
        AlignmentGroupMode::Strand => {
            if candidate.flags.is_reverse() {
                OracleGroupKey::Reverse
            } else {
                OracleGroupKey::Forward
            }
        }
        AlignmentGroupMode::ReadGroup
        | AlignmentGroupMode::Haplotype
        | AlignmentGroupMode::Tag(_) => candidate
            .group_tag
            .as_ref()
            .map_or(OracleGroupKey::Missing, |label| {
                OracleGroupKey::Tagged(label.clone())
            }),
        AlignmentGroupMode::Chimeric => tagged(if candidate.has_sa {
            "Chimeric"
        } else {
            "Not chimeric"
        }),
        AlignmentGroupMode::Supplementary => tagged(if candidate.flags.is_supplementary() {
            "Supplementary"
        } else {
            "Primary/secondary"
        }),
        AlignmentGroupMode::LeftClip => tagged(if left_clip(&candidate.cigar) > 0 {
            "Left clipped"
        } else {
            "Not left clipped"
        }),
        AlignmentGroupMode::RightClip => tagged(if right_clip(&candidate.cigar) > 0 {
            "Right clipped"
        } else {
            "Not right clipped"
        }),
        AlignmentGroupMode::PairOrientation => {
            tagged(orientation_label(candidate.pair.orientation))
        }
        AlignmentGroupMode::UnexpectedPair => tagged(match candidate.pair.classification {
            PairClassification::Proper | PairClassification::Concordant => "Expected",
            PairClassification::Unpaired => "Unpaired",
            _ => "Unexpected",
        }),
        AlignmentGroupMode::MateContig => candidate.mate_contig.map_or_else(
            || tagged("Missing"),
            |contig| tagged(&format!("tid {}", contig.get())),
        ),
        AlignmentGroupMode::ReadOrder => tagged(read_order_label(candidate.pair.read_order)),
        AlignmentGroupMode::FirstOfPairStrand => {
            tagged(match first_of_pair_reverse(candidate.flags) {
                Some(true) => "First reverse",
                Some(false) => "First forward",
                None => "Unknown",
            })
        }
        AlignmentGroupMode::Base | AlignmentGroupMode::Insertion => candidate
            .position_group
            .as_ref()
            .map_or(OracleGroupKey::Missing, |label| tagged(label)),
    }
}

fn tagged(label: &str) -> OracleGroupKey {
    OracleGroupKey::Tagged(label.to_owned())
}

fn compare_units(
    left: &RowUnit,
    right: &RowUnit,
    candidates: &[Candidate],
    mode: AlignmentSortMode,
) -> Ordering {
    let left_record = &candidates[left.representative];
    let right_record = &candidates[right.representative];
    let tie = || left_record.handle.cmp(&right_record.handle);
    let ordering = match mode {
        AlignmentSortMode::Start => (left.start, left.end).cmp(&(right.start, right.end)),
        AlignmentSortMode::Strand => (left_record.flags.is_reverse(), left.start)
            .cmp(&(right_record.flags.is_reverse(), right.start)),
        AlignmentSortMode::FirstOfPairStrand => (
            first_of_pair_reverse(left_record.flags).is_none(),
            first_of_pair_reverse(left_record.flags).unwrap_or_default(),
            left.start,
        )
            .cmp(&(
                first_of_pair_reverse(right_record.flags).is_none(),
                first_of_pair_reverse(right_record.flags).unwrap_or_default(),
                right.start,
            )),
        AlignmentSortMode::MappingQuality => (
            Reverse(left_record.mapping_quality.unwrap_or_default()),
            left.start,
        )
            .cmp(&(
                Reverse(right_record.mapping_quality.unwrap_or_default()),
                right.start,
            )),
        AlignmentSortMode::ReadName => left_record
            .read_name
            .cmp(&right_record.read_name)
            .then_with(|| left.start.cmp(&right.start)),
        AlignmentSortMode::ReadGroup | AlignmentSortMode::Haplotype | AlignmentSortMode::Tag(_) => {
            (
                left_record.sort_tag.is_none(),
                left_record.sort_tag.as_deref(),
                left.start,
            )
                .cmp(&(
                    right_record.sort_tag.is_none(),
                    right_record.sort_tag.as_deref(),
                    right.start,
                ))
        }
        AlignmentSortMode::AlignedLength => {
            (Reverse(aligned_length(&left_record.cigar)), left.start)
                .cmp(&(Reverse(aligned_length(&right_record.cigar)), right.start))
        }
        AlignmentSortMode::LeftClip => (Reverse(left_clip(&left_record.cigar)), left.start)
            .cmp(&(Reverse(left_clip(&right_record.cigar)), right.start)),
        AlignmentSortMode::RightClip => (Reverse(right_clip(&left_record.cigar)), left.start)
            .cmp(&(Reverse(right_clip(&right_record.cigar)), right.start)),
        AlignmentSortMode::TemplateLength => (
            Reverse(left_record.template_length.unsigned_abs()),
            left.start,
        )
            .cmp(&(
                Reverse(right_record.template_length.unsigned_abs()),
                right.start,
            )),
        AlignmentSortMode::PairOrientation => {
            (orientation_rank(left_record.pair.orientation), left.start)
                .cmp(&(orientation_rank(right_record.pair.orientation), right.start))
        }
        AlignmentSortMode::MateContig => {
            (left_record.mate_contig, left_record.mate_start, left.start).cmp(&(
                right_record.mate_contig,
                right_record.mate_start,
                right.start,
            ))
        }
        AlignmentSortMode::PairStatus => (
            classification_rank(left_record.pair.classification),
            left.start,
        )
            .cmp(&(
                classification_rank(right_record.pair.classification),
                right.start,
            )),
        AlignmentSortMode::ReadOrder => (read_order_rank(left_record.pair.read_order), left.start)
            .cmp(&(read_order_rank(right_record.pair.read_order), right.start)),
        AlignmentSortMode::Base => {
            (left_record.base_sort, left.start).cmp(&(right_record.base_sort, right.start))
        }
    };
    ordering.then_with(tie)
}

fn pack_groups(
    candidates: &[Candidate],
    units: &[RowUnit],
    groups: Vec<(OracleGroupKey, Vec<usize>)>,
) -> Result<RowOracleOutput, RowOracleError> {
    let mut rows_by_unit = vec![0_u32; units.len()];
    let mut extents = Vec::<OracleRowExtent>::new();
    let mut retained_groups = Vec::with_capacity(groups.len());
    for (key, priority_order) in groups {
        let row_start = u32::try_from(extents.len()).map_err(|_| RowOracleError::IndexOverflow)?;
        let mut coordinate_order = priority_order.clone();
        coordinate_order.sort_unstable_by_key(|unit| {
            (
                units[*unit].start,
                units[*unit].end,
                units[*unit].representative,
            )
        });
        let mut active = BinaryHeap::<Reverse<(u64, u32)>>::new();
        let mut available = BinaryHeap::<Reverse<u32>>::new();
        let mut local_extents = Vec::<OracleRowExtent>::new();
        for unit_index in coordinate_order {
            let unit = &units[unit_index];
            while active
                .peek()
                .is_some_and(|Reverse((end, _))| *end <= unit.start)
            {
                let Reverse((_, row)) = active.pop().expect("observed reusable row");
                available.push(Reverse(row));
            }
            let local_row = if let Some(Reverse(row)) = available.pop() {
                row
            } else {
                let row = u32::try_from(local_extents.len())
                    .map_err(|_| RowOracleError::IndexOverflow)?;
                local_extents.push(empty_extent());
                row
            };
            rows_by_unit[unit_index] = local_row;
            update_extent(&mut local_extents[local_row as usize], unit)?;
            active.push(Reverse((unit.end, local_row)));
        }

        // Interval colouring is performed in coordinate order for O(n log rows), then physical
        // lanes are deterministically permuted to preserve the requested sort priority.
        let mut priority_by_row = vec![usize::MAX; local_extents.len()];
        for (rank, unit) in priority_order.iter().copied().enumerate() {
            let row = rows_by_unit[unit] as usize;
            priority_by_row[row] = priority_by_row[row].min(rank);
        }
        let mut ordered_rows = (0..local_extents.len()).collect::<Vec<_>>();
        ordered_rows.sort_unstable_by_key(|row| (priority_by_row[*row], *row));
        let mut remap = vec![0_u32; local_extents.len()];
        let mut reordered_extents = Vec::with_capacity(local_extents.len());
        for (next, previous) in ordered_rows.into_iter().enumerate() {
            remap[previous] = u32::try_from(next).map_err(|_| RowOracleError::IndexOverflow)?;
            reordered_extents.push(local_extents[previous]);
        }
        for unit in &priority_order {
            rows_by_unit[*unit] = row_start.saturating_add(remap[rows_by_unit[*unit] as usize]);
        }
        extents.extend(reordered_extents);
        let row_end = u32::try_from(extents.len()).map_err(|_| RowOracleError::IndexOverflow)?;
        retained_groups.push(OracleGroup {
            key,
            row_start,
            row_end,
        });
    }

    let mut assignments = Vec::with_capacity(candidates.len());
    for (unit_index, unit) in units.iter().enumerate() {
        for member in &unit.members {
            let record = &candidates[*member];
            assignments.push(OracleAssignment {
                handle: record.handle,
                row: rows_by_unit[unit_index],
                start: record.start,
                end: record.end,
            });
        }
    }
    assignments.sort_unstable_by_key(|record| record.handle);
    let connectors = build_connectors(candidates, units, &rows_by_unit);
    Ok(RowOracleOutput {
        assignments,
        extents,
        groups: retained_groups,
        connectors,
        eligible_records: 0,
    })
}

fn build_connectors(
    candidates: &[Candidate],
    units: &[RowUnit],
    rows_by_unit: &[u32],
) -> Vec<OraclePairConnector> {
    let mut connectors = Vec::new();
    for (unit_index, unit) in units.iter().enumerate().filter(|(_, unit)| unit.paired) {
        let mut members = unit.members.clone();
        members.sort_unstable_by_key(|member| {
            let record = &candidates[*member];
            (record.start, record.end, record.handle)
        });
        if members.len() > 1 {
            for pair in members.windows(2) {
                let left = &candidates[pair[0]];
                let right = &candidates[pair[1]];
                if left.end >= right.start {
                    continue;
                }
                connectors.push(OraclePairConnector {
                    template: template_id(&left.read_name).get(),
                    left: left.handle,
                    right: Some(right.handle),
                    row: rows_by_unit[unit_index],
                    start: left.end,
                    end: right.start,
                });
            }
            continue;
        }
        let Some(member) = members.first().copied() else {
            continue;
        };
        let record = &candidates[member];
        if record.flags.is_mate_unmapped() || record.mate_contig != Some(record.contig) {
            continue;
        }
        let Some(mate_start) = record.mate_start else {
            continue;
        };
        let (start, end) = if mate_start >= record.end {
            (record.end, mate_start)
        } else if mate_start.saturating_add(1) <= record.start {
            (mate_start.saturating_add(1), record.start)
        } else {
            continue;
        };
        if start < end {
            connectors.push(OraclePairConnector {
                template: template_id(&record.read_name).get(),
                left: record.handle,
                right: None,
                row: rows_by_unit[unit_index],
                start,
                end,
            });
        }
    }
    connectors.sort_unstable_by_key(|connector| {
        (
            connector.row,
            connector.start,
            connector.end,
            connector.template,
            connector.left,
        )
    });
    connectors
}

const fn empty_extent() -> OracleRowExtent {
    OracleRowExtent {
        minimum_start: u64::MAX,
        maximum_end: 0,
        record_count: 0,
    }
}

fn update_extent(extent: &mut OracleRowExtent, unit: &RowUnit) -> Result<(), RowOracleError> {
    extent.minimum_start = extent.minimum_start.min(unit.start);
    extent.maximum_end = extent.maximum_end.max(unit.end);
    extent.record_count = extent
        .record_count
        .checked_add(u32::try_from(unit.members.len()).map_err(|_| RowOracleError::IndexOverflow)?)
        .ok_or(RowOracleError::IndexOverflow)?;
    Ok(())
}

fn alignment_base_sort_key(
    record_start: u64,
    record_end: u64,
    cigar: &[CigarOp],
    sequence: Option<SequenceView<'_>>,
    target: Option<u64>,
) -> (u8, u8) {
    let Some(target) = target else {
        return (7, 0);
    };
    if target < record_start || target >= record_end {
        return (7, 0);
    }
    let mut reference_position = record_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    let offset = usize::try_from(target - reference_position).unwrap_or(usize::MAX);
                    let index = read_position.saturating_add(offset);
                    let base = match sequence {
                        Some(SequenceView::Present { bases, .. }) => bases
                            .get(index)
                            .copied()
                            .map(|base| base.to_ascii_uppercase()),
                        Some(SequenceView::Missing | SequenceView::NotLoaded) | None => None,
                    };
                    return base.map_or((6, 0), |base| match base {
                        b'A' => (0, base),
                        b'C' => (1, base),
                        b'G' => (2, base),
                        b'T' | b'U' => (3, base),
                        b'N' => (4, base),
                        other => (5, other),
                    });
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_end;
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..end).contains(&target) {
                    return (5, b'-');
                }
                reference_position = end;
            }
            CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..end).contains(&target) {
                    return (5, b'~');
                }
                reference_position = end;
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    (7, 0)
}

const fn alignment_base_group_label(key: (u8, u8)) -> &'static str {
    match key {
        (0, _) => "A",
        (1, _) => "C",
        (2, _) => "G",
        (3, _) => "T",
        (4, _) => "N",
        (5, b'-') => "Deletion",
        (5, b'~') => "Reference skip",
        (5, _) => "Other base",
        (6, _) => "Sequence unavailable",
        _ => "Does not overlap position",
    }
}

fn alignment_insertion_group_label(
    record_start: u64,
    cigar: &[CigarOp],
    target: Option<u64>,
) -> String {
    let Some(target) = target else {
        return "No position anchor".to_owned();
    };
    let mut reference_position = record_start;
    for operation in cigar {
        match operation.kind {
            CigarOpKind::Insertion if reference_position == target => {
                return format!("Insertion {} bp", operation.len);
            }
            CigarOpKind::Match
            | CigarOpKind::SequenceMatch
            | CigarOpKind::SequenceMismatch
            | CigarOpKind::Deletion
            | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    "No insertion".to_owned()
}

fn aligned_length(cigar: &[CigarOp]) -> u64 {
    cigar.iter().fold(0_u64, |length, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Insertion
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            length.saturating_add(u64::from(operation.len))
        } else {
            length
        }
    })
}

fn left_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .first()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}

fn right_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .last()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}

fn first_of_pair_reverse(flags: AlignmentFlags) -> Option<bool> {
    if flags.is_first_in_template() {
        Some(flags.is_reverse())
    } else if flags.is_last_in_template() {
        Some(flags.is_mate_reverse())
    } else {
        None
    }
}

const fn orientation_rank(value: PairOrientation) -> u8 {
    match value {
        PairOrientation::Inward => 0,
        PairOrientation::Outward => 1,
        PairOrientation::SameStrand => 2,
        PairOrientation::Unknown => 3,
    }
}

const fn orientation_label(value: PairOrientation) -> &'static str {
    match value {
        PairOrientation::Inward => "Inward",
        PairOrientation::Outward => "Outward",
        PairOrientation::SameStrand => "Same strand",
        PairOrientation::Unknown => "Unknown",
    }
}

const fn classification_rank(value: PairClassification) -> u8 {
    match value {
        PairClassification::Proper => 0,
        PairClassification::Concordant => 1,
        PairClassification::DiscordantTemplateLengthShort => 2,
        PairClassification::DiscordantTemplateLengthLong => 3,
        PairClassification::DiscordantOrientation => 4,
        PairClassification::InterContig => 5,
        PairClassification::MateUnmapped => 6,
        PairClassification::MateMissing => 7,
        PairClassification::ReadUnmapped => 8,
        PairClassification::Unpaired => 9,
    }
}

const fn read_order_rank(value: ReadOrder) -> u8 {
    match value {
        ReadOrder::First => 0,
        ReadOrder::Last => 1,
        ReadOrder::Unknown => 2,
        ReadOrder::Ambiguous => 3,
    }
}

const fn read_order_label(value: ReadOrder) -> &'static str {
    match value {
        ReadOrder::First => "First",
        ReadOrder::Last => "Last",
        ReadOrder::Unknown => "Unknown",
        ReadOrder::Ambiguous => "Ambiguous",
    }
}

#[derive(Debug, Error)]
pub enum RowOracleError {
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    PackedRecord(#[from] rgv_hts::PackedRecordError),
    #[error("row layout mode requires retained sequence payload")]
    SequencePayloadRequired,
    #[error("row layout mode requires retained auxiliary tag {0:?}")]
    AuxiliaryPayloadRequired([u8; 2]),
    #[error("row oracle encountered an invalid evidence record")]
    InvalidEvidenceRecord,
    #[error("row oracle index capacity was exceeded")]
    IndexOverflow,
}
