use std::collections::BTreeMap;

use rgv_core::{GenomicInterval, OverlappingMatePolicy};

use crate::{
    AlignmentBatch, AlignmentError, AuxValue, CigarOpKind, ExactDepthTile, ExactDepthTileBuilder,
    JunctionStrand, PairSummary, PileupBin, PileupProvenance, PileupSummary,
    alignment::BatchOverflow,
    junction::{
        JunctionObservation, JunctionStrandInput, JunctionSummaryBuilder, RecordJunctionCollector,
    },
    pair::PairSummaryBuilder,
    pileup::{MateAlignment, MateOverlapMask, OverlapCollapser},
    template_id,
};

/// Canonical genomic width of one exact resident depth tile.
pub const ALIGNMENT_SUMMARY_TILE_BASES: u64 = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RangeEvent {
    start: u64,
    end: u64,
    reverse: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InsertionEvent {
    position: u64,
    reverse: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PairCandidate {
    score: u64,
    length: u64,
    eligible: bool,
}

/// Exact, viewport-independent summaries derived once from one immutable resident batch.
///
/// Depth is stored as compact constant-value runs inside canonical genomic tiles. Junction CIGAR
/// observations and pair candidates are retained by resident logical record index, so arbitrary
/// viewport evidence can be assembled without reparsing every selected CIGAR or rehashing every
/// selected template name.
#[derive(Clone, Debug)]
pub struct AlignmentSummaryIndex {
    interval: GenomicInterval,
    record_count: usize,
    overlapping_mate_policy: OverlappingMatePolicy,
    overlap_collapse_complete: bool,
    depth_tiles: Vec<ExactDepthTile>,
    deletions: Vec<RangeEvent>,
    insertions: Vec<InsertionEvent>,
    junction_offsets: Vec<u32>,
    junctions: Vec<JunctionObservation>,
    dropped_junctions: Vec<u32>,
    junction_strands: Vec<JunctionStrandInput>,
    alignment_flags: Vec<crate::AlignmentFlags>,
    spliced_record_indices: Vec<u32>,
    collapsed_offsets: Vec<u32>,
    collapsed_ranges: Vec<(u64, u64)>,
    pair_candidates: Vec<PairCandidate>,
    pair_candidate_indices: Vec<u32>,
    pair_candidate_indices_by_score: Vec<u32>,
    modification_tags_present: bool,
}

impl AlignmentSummaryIndex {
    pub fn build(
        batch: &AlignmentBatch,
        interval: GenomicInterval,
        overlapping_mate_policy: OverlappingMatePolicy,
    ) -> Result<Self, AlignmentError> {
        let mut depth_builders = BTreeMap::<u64, ExactDepthTileBuilder>::new();
        let mut deletions = Vec::new();
        let mut insertions = Vec::new();
        let mut junction_offsets = Vec::with_capacity(batch.len().saturating_add(1));
        let mut junctions = Vec::new();
        let mut dropped_junctions = Vec::with_capacity(batch.len());
        let mut junction_strands = Vec::with_capacity(batch.len());
        let mut alignment_flags = Vec::with_capacity(batch.len());
        let mut spliced_record_indices = Vec::new();
        let mut collapsed_offsets = Vec::with_capacity(batch.len().saturating_add(1));
        let mut collapsed_ranges = Vec::new();
        let mut pair_candidates = Vec::with_capacity(batch.len());
        let mut pair_candidate_indices = Vec::new();
        let mut modification_tags_present = false;
        let mut overlap_collapser = OverlapCollapser::new(overlapping_mate_policy);
        junction_offsets.push(0);
        collapsed_offsets.push(0);

        for index in 0..batch.len() {
            let record = batch
                .record_view(index)
                .ok_or(crate::PackedRecordError::InvalidIndex(index))?;
            let flags = record.flags();
            let reverse = flags.is_reverse();
            // Depth and indel summaries are independent of base quality, so every aligned base
            // of the earlier mate counts as contributed.
            let overlap_mask = overlap_collapser.mask_for_alignment(
                MateAlignment {
                    read_name: record.read_name(),
                    contig: record.contig(),
                    start: record.start(),
                    end: record.end(),
                    flags,
                    mate_contig: record.mate_contig(),
                    mate_start: record.mate_start(),
                },
                record.cigar().iter().copied(),
                |_| true,
            );
            let overlap_mask = overlap_mask.as_ref();
            let mut reference_position = record.start();
            let mut aligned_segments = Vec::new();
            let mut record_junctions = RecordJunctionCollector::default();
            for operation in record.cigar() {
                let end = reference_position.saturating_add(u64::from(operation.len));
                match operation.kind {
                    CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch => {
                        record_junctions.add_aligned(operation.len);
                        aligned_segments.push((reference_position, end));
                        reference_position = end;
                    }
                    CigarOpKind::Deletion => {
                        let mut add = |start, end| {
                            deletions.push(RangeEvent {
                                start,
                                end,
                                reverse,
                            });
                        };
                        if let Some(mask) = overlap_mask {
                            mask.add_unmasked_deletion_range(reference_position, end, add);
                        } else {
                            add(reference_position, end);
                        }
                        reference_position = end;
                    }
                    CigarOpKind::ReferenceSkip => {
                        record_junctions.add_skip(reference_position, end);
                        reference_position = end;
                    }
                    CigarOpKind::Insertion => {
                        if !overlap_mask.is_some_and(|mask| mask.masks_indel(reference_position)) {
                            insertions.push(InsertionEvent {
                                position: reference_position,
                                reverse,
                            });
                        }
                    }
                    CigarOpKind::SoftClip | CigarOpKind::HardClip | CigarOpKind::Padding => {}
                }
            }

            let mut merged = Vec::new();
            for (start, end) in aligned_segments {
                if let Some((_, merged_end)) = merged.last_mut()
                    && start <= *merged_end
                {
                    *merged_end = (*merged_end).max(end);
                } else {
                    merged.push((start, end));
                }
            }
            for (start, end) in merged {
                add_unmasked_ranges_with_overlap(
                    start,
                    end,
                    overlap_mask,
                    |start, end| {
                        add_depth_segment(
                            &mut depth_builders,
                            interval,
                            start,
                            end,
                            reverse,
                            record.mapping_quality(),
                        );
                    },
                    |start, end| collapsed_ranges.push((start, end)),
                );
            }
            collapsed_offsets.push(
                u32::try_from(collapsed_ranges.len())
                    .map_err(|_| AlignmentError::BatchOverflow(BatchOverflow))?,
            );

            let (observations, dropped) = record_junctions.finish();
            if !observations.is_empty() {
                spliced_record_indices.push(u32::try_from(index).unwrap_or(u32::MAX));
            }
            junctions.extend(observations);
            junction_offsets.push(
                u32::try_from(junctions.len())
                    .map_err(|_| AlignmentError::BatchOverflow(BatchOverflow))?,
            );
            dropped_junctions.push(u32::try_from(dropped).unwrap_or(u32::MAX));
            junction_strands.push(record_junction_strand(record)?);
            alignment_flags.push(record.flags());
            let pair_candidate = pair_candidate(record);
            if pair_candidate.eligible {
                pair_candidate_indices.push(u32::try_from(index).unwrap_or(u32::MAX));
            }
            pair_candidates.push(pair_candidate);
            modification_tags_present |=
                record.has_auxiliary_tag(*b"MM") || record.has_auxiliary_tag(*b"Mm");
        }

        deletions.sort_unstable_by_key(|event| (event.start, event.end));
        insertions.sort_unstable_by_key(|event| event.position);
        let depth_tiles = depth_builders
            .into_values()
            .map(ExactDepthTileBuilder::finish)
            .collect();
        let mut pair_candidate_indices_by_score = pair_candidate_indices.clone();
        pair_candidate_indices_by_score.sort_unstable_by_key(|index| {
            let candidate = pair_candidates[*index as usize];
            (candidate.score, *index)
        });
        Ok(Self {
            interval,
            record_count: batch.len(),
            overlapping_mate_policy,
            overlap_collapse_complete: overlap_collapser.complete(),
            depth_tiles,
            deletions,
            insertions,
            junction_offsets,
            junctions,
            dropped_junctions,
            junction_strands,
            alignment_flags,
            spliced_record_indices,
            collapsed_offsets,
            collapsed_ranges,
            pair_candidates,
            pair_candidate_indices,
            pair_candidate_indices_by_score,
            modification_tags_present,
        })
    }

    pub fn supports(
        &self,
        interval: GenomicInterval,
        record_count: usize,
        policy: OverlappingMatePolicy,
    ) -> bool {
        self.record_count == record_count
            && self.overlapping_mate_policy == policy
            && self.interval.contains_interval(interval)
    }

    pub(crate) fn pileup(
        &self,
        interval: GenomicInterval,
        bins: usize,
        minimum_base_quality: u8,
        selected: &[usize],
        rendered_records: usize,
    ) -> PileupSummary {
        let mut output = vec![PileupBin::default(); bins];
        for tile in self
            .depth_tiles
            .iter()
            .filter(|tile| tile.interval().overlaps(interval))
        {
            for run in tile.runs() {
                let start = run.start.max(interval.start());
                let end = run.end.min(interval.end());
                let Some((first, last)) = bin_range(interval, bins, start, end) else {
                    continue;
                };
                for bin in &mut output[first..last] {
                    if run.depth > bin.depth {
                        bin.depth = run.depth;
                        bin.forward_depth = run.forward_depth;
                        bin.reverse_depth = run.reverse_depth;
                        bin.mapping_quality_sum = run.mapping_quality_sum;
                        bin.mapping_quality_observations = run.mapping_quality_observations;
                    }
                }
            }
        }
        for event in self
            .deletions
            .iter()
            .filter(|event| event.start < interval.end() && event.end > interval.start())
        {
            let Some((first, last)) = bin_range(interval, bins, event.start, event.end) else {
                continue;
            };
            for bin in &mut output[first..last] {
                bin.alleles.deletion = bin.alleles.deletion.saturating_add(1);
                let strand = if event.reverse {
                    &mut bin.reverse_alleles
                } else {
                    &mut bin.forward_alleles
                };
                strand.deletion = strand.deletion.saturating_add(1);
            }
        }
        for event in self
            .insertions
            .iter()
            .filter(|event| event.position >= interval.start() && event.position <= interval.end())
        {
            let anchor = event.position.saturating_sub(1);
            let index = bin_index(interval, bins, anchor)
                .or_else(|| bin_index(interval, bins, event.position));
            if let Some(index) = index {
                let bin = &mut output[index];
                bin.alleles.insertion = bin.alleles.insertion.saturating_add(1);
                let strand = if event.reverse {
                    &mut bin.reverse_alleles
                } else {
                    &mut bin.forward_alleles
                };
                strand.insertion = strand.insertion.saturating_add(1);
            }
        }
        let collapsed_mate_bases = selected.iter().fold(0_u64, |total, index| {
            total.saturating_add(self.collapsed_bases_in(*index, interval))
        });
        PileupSummary {
            bins: output,
            minimum_base_quality,
            provenance: PileupProvenance {
                eligible_records: selected.len(),
                rendered_records,
                all_eligible_records: true,
                allele_observations_available: false,
                overlapping_mate_policy: self.overlapping_mate_policy,
                overlap_collapse_complete: self.overlap_collapse_complete,
                collapsed_mate_bases,
                exact_depth_complete: true,
                depth_segments_omitted: 0,
            },
            modifications: crate::ModificationSummary::default(),
        }
    }

    pub(crate) fn add_record_junctions(&self, index: usize, builder: &mut JunctionSummaryBuilder) {
        let Some((start, end)) = self.record_range(&self.junction_offsets, index) else {
            return;
        };
        builder.add_record_junctions(
            &self.junctions[start..end],
            self.dropped_junctions
                .get(index)
                .copied()
                .unwrap_or_default() as usize,
            self.alignment_flags
                .get(index)
                .copied()
                .unwrap_or(crate::AlignmentFlags::from_bits(0)),
            self.junction_strands
                .get(index)
                .copied()
                .unwrap_or(JunctionStrandInput::Missing),
        );
    }

    pub(crate) fn add_selected_junctions(
        &self,
        selected: &[usize],
        builder: &mut JunctionSummaryBuilder,
    ) {
        for_each_selected_candidate(selected, &self.spliced_record_indices, |index| {
            self.add_record_junctions(index, builder);
        });
    }

    pub(crate) const fn modification_tags_present(&self) -> bool {
        self.modification_tags_present
    }

    pub(crate) fn pair_summary(&self, selected: &[usize]) -> PairSummary {
        let mut builder = PairSummaryBuilder::new();
        let eligible = count_selected_candidates(selected, &self.pair_candidate_indices);
        builder.add_precomputed_eligible_count(eligible);
        for index in self
            .pair_candidate_indices_by_score
            .iter()
            .copied()
            .filter(|index| selected.binary_search(&(*index as usize)).is_ok())
            .take(crate::pair::MAX_TEMPLATE_LENGTH_SAMPLES)
        {
            let candidate = self.pair_candidates[index as usize];
            builder.add_precomputed_sample(candidate.score, candidate.length);
        }
        builder.finish(true)
    }

    pub fn estimated_bytes(&self) -> usize {
        self.depth_tiles
            .iter()
            .fold(0_usize, |total, tile| {
                total.saturating_add(tile.estimated_bytes())
            })
            .saturating_add(self.deletions.capacity() * std::mem::size_of::<RangeEvent>())
            .saturating_add(self.insertions.capacity() * std::mem::size_of::<InsertionEvent>())
            .saturating_add(self.junction_offsets.capacity() * std::mem::size_of::<u32>())
            .saturating_add(self.junctions.capacity() * std::mem::size_of::<JunctionObservation>())
            .saturating_add(self.dropped_junctions.capacity() * std::mem::size_of::<u32>())
            .saturating_add(
                self.junction_strands.capacity() * std::mem::size_of::<JunctionStrandInput>(),
            )
            .saturating_add(
                self.alignment_flags.capacity() * std::mem::size_of::<crate::AlignmentFlags>(),
            )
            .saturating_add(self.spliced_record_indices.capacity() * std::mem::size_of::<u32>())
            .saturating_add(self.collapsed_offsets.capacity() * std::mem::size_of::<u32>())
            .saturating_add(self.collapsed_ranges.capacity() * std::mem::size_of::<(u64, u64)>())
            .saturating_add(self.pair_candidates.capacity() * std::mem::size_of::<PairCandidate>())
            .saturating_add(self.pair_candidate_indices.capacity() * std::mem::size_of::<u32>())
            .saturating_add(
                self.pair_candidate_indices_by_score.capacity() * std::mem::size_of::<u32>(),
            )
    }

    pub fn tile_count_for(&self, interval: GenomicInterval) -> usize {
        self.depth_tiles
            .iter()
            .filter(|tile| tile.interval().overlaps(interval))
            .count()
    }

    fn collapsed_bases_in(&self, index: usize, interval: GenomicInterval) -> u64 {
        let Some((start, end)) = self.record_range(&self.collapsed_offsets, index) else {
            return 0;
        };
        self.collapsed_ranges[start..end]
            .iter()
            .map(|(start, end)| {
                (*end)
                    .min(interval.end())
                    .saturating_sub((*start).max(interval.start()))
            })
            .sum()
    }

    fn record_range(&self, offsets: &[u32], index: usize) -> Option<(usize, usize)> {
        Some((
            *offsets.get(index)? as usize,
            *offsets.get(index.saturating_add(1))? as usize,
        ))
    }
}

fn for_each_selected_candidate(
    selected: &[usize],
    candidates: &[u32],
    mut visit: impl FnMut(usize),
) {
    let mut selected_index = 0_usize;
    let mut candidate_index = 0_usize;
    while selected_index < selected.len() && candidate_index < candidates.len() {
        let selected_record = selected[selected_index];
        let candidate_record = candidates[candidate_index] as usize;
        match selected_record.cmp(&candidate_record) {
            std::cmp::Ordering::Less => selected_index = selected_index.saturating_add(1),
            std::cmp::Ordering::Greater => candidate_index = candidate_index.saturating_add(1),
            std::cmp::Ordering::Equal => {
                visit(selected_record);
                selected_index = selected_index.saturating_add(1);
                candidate_index = candidate_index.saturating_add(1);
            }
        }
    }
}

fn count_selected_candidates(selected: &[usize], candidates: &[u32]) -> usize {
    let mut count = 0_usize;
    for_each_selected_candidate(selected, candidates, |_| {
        count = count.saturating_add(1);
    });
    count
}

fn pair_candidate(record: crate::AlignmentRecordView<'_>) -> PairCandidate {
    let flags = record.flags();
    if record.read_name().is_empty()
        || !flags.is_paired()
        || !flags.is_proper_pair()
        || flags.is_unmapped()
        || flags.is_mate_unmapped()
        || flags.is_secondary()
        || flags.is_supplementary()
        || !flags.is_first_in_template()
        || record.mate_contig() != Some(record.contig())
        || record.template_length() == 0
    {
        return PairCandidate::default();
    }
    PairCandidate {
        score: template_id(record.read_name()).get(),
        length: record.template_length().unsigned_abs(),
        eligible: true,
    }
}

fn record_junction_strand(
    record: crate::AlignmentRecordView<'_>,
) -> Result<JunctionStrandInput, crate::PackedRecordError> {
    Ok(match record.auxiliary_field(*b"XS")? {
        Some(field) => match field.value {
            AuxValue::Character(b'+') => JunctionStrandInput::Tagged(JunctionStrand::Positive),
            AuxValue::Character(b'-') => JunctionStrandInput::Tagged(JunctionStrand::Negative),
            AuxValue::String(value) if value == "+" => {
                JunctionStrandInput::Tagged(JunctionStrand::Positive)
            }
            AuxValue::String(value) if value == "-" => {
                JunctionStrandInput::Tagged(JunctionStrand::Negative)
            }
            _ => JunctionStrandInput::Invalid,
        },
        None => match record.auxiliary_field(*b"TS")? {
            Some(field) => crate::backend_record::junction_strand_input(Some(&field.value)),
            None => match record.auxiliary_field(*b"ts")? {
                Some(field) => {
                    match crate::backend_record::junction_strand_input(Some(&field.value)) {
                        JunctionStrandInput::Tagged(strand) => {
                            JunctionStrandInput::ReadRelative(strand)
                        }
                        other => other,
                    }
                }
                None => JunctionStrandInput::Missing,
            },
        },
    })
}

fn add_depth_segment(
    tiles: &mut BTreeMap<u64, ExactDepthTileBuilder>,
    interval: GenomicInterval,
    start: u64,
    end: u64,
    reverse: bool,
    mapping_quality: Option<u8>,
) {
    let mut start = start.max(interval.start());
    let end = end.min(interval.end());
    while start < end {
        let coordinate = start / ALIGNMENT_SUMMARY_TILE_BASES;
        let tile_start = coordinate.saturating_mul(ALIGNMENT_SUMMARY_TILE_BASES);
        let tile_boundary = coordinate
            .saturating_add(1)
            .saturating_mul(ALIGNMENT_SUMMARY_TILE_BASES);
        let segment_end = tile_boundary.min(end);
        // The builder clips every segment to its interval, so that interval must be the whole
        // canonical tile. Ending it at the first segment seen would silently drop the depth of
        // every later segment that reaches further into the same tile.
        let tile_interval = GenomicInterval::new(interval.contig(), tile_start, tile_boundary)
            .expect("a non-empty depth segment creates a valid canonical tile interval");
        tiles
            .entry(coordinate)
            .or_insert_with(|| ExactDepthTileBuilder::new(tile_interval))
            .add_aligned_segment(start, segment_end, reverse, mapping_quality);
        start = segment_end;
    }
}

fn add_unmasked_ranges_with_overlap(
    start: u64,
    end: u64,
    mask: Option<&MateOverlapMask>,
    mut add: impl FnMut(u64, u64),
    overlap: impl FnMut(u64, u64),
) {
    if let Some(mask) = mask {
        mask.split_depth_range(start, end, add, overlap);
    } else {
        add(start, end);
    }
}

fn bin_index(interval: GenomicInterval, bins: usize, position: u64) -> Option<usize> {
    if bins == 0 || !interval.contains(position) {
        return None;
    }
    let offset = u128::from(position - interval.start());
    let index = offset.saturating_mul(bins as u128) / u128::from(interval.len());
    usize::try_from(index).ok().map(|index| index.min(bins - 1))
}

fn bin_range(
    interval: GenomicInterval,
    bins: usize,
    start: u64,
    end: u64,
) -> Option<(usize, usize)> {
    let start = start.max(interval.start());
    let end = end.min(interval.end());
    if start >= end || bins == 0 {
        return None;
    }
    let interval_length = u128::from(interval.len());
    let bins_u128 = bins as u128;
    let first = u128::from(start - interval.start()).saturating_mul(bins_u128) / interval_length;
    let last = u128::from(end - interval.start())
        .saturating_mul(bins_u128)
        .div_ceil(interval_length);
    let first = usize::try_from(first).ok()?.min(bins - 1);
    let last = usize::try_from(last).ok()?.clamp(first + 1, bins);
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rgv_core::ContigId;

    use super::*;
    use crate::{
        AlignmentFlags, AlignmentHeaderMetadata, AlignmentOverlapIndex, AlignmentQuery,
        AlignmentRecord, CigarOp, JunctionSummary, QueryDetail, QueryOptions, QueryStats,
        SequenceData, derive_alignment_query, derive_alignment_query_with_summary_index_and_phase,
    };
    use std::sync::Arc;

    fn record(
        name: &[u8],
        start: u64,
        cigar: &[(CigarOpKind, u32)],
        reverse: bool,
    ) -> AlignmentRecord {
        let end = cigar.iter().fold(start, |position, (kind, length)| {
            if matches!(
                kind,
                CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
                    | CigarOpKind::Deletion
                    | CigarOpKind::ReferenceSkip
            ) {
                position + u64::from(*length)
            } else {
                position
            }
        });
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(if reverse { 0x10 } else { 0 }),
            mapping_quality: Some(40),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: cigar
                .iter()
                .map(|(kind, len)| CigarOp {
                    kind: *kind,
                    len: *len,
                })
                .collect(),
            sequence: SequenceData::Missing,
            auxiliary_fields: Vec::new(),
        }
    }

    #[test]
    fn canonical_index_matches_fresh_depth_junction_and_indel_summaries() {
        let resident_interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 4_080, 4_160).unwrap();
        let mut batch = AlignmentBatch::default();
        batch
            .push(&record(b"left", 4_070, &[(CigarOpKind::Match, 70)], false))
            .unwrap();
        batch
            .push(&record(
                b"spliced",
                4_090,
                &[
                    (CigarOpKind::Match, 20),
                    (CigarOpKind::ReferenceSkip, 10),
                    (CigarOpKind::Match, 20),
                ],
                true,
            ))
            .unwrap();
        batch
            .push(&record(
                b"indel",
                4_100,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::Deletion, 5),
                    (CigarOpKind::Insertion, 2),
                    (CigarOpKind::Match, 15),
                ],
                false,
            ))
            .unwrap();
        let resident = AlignmentQuery {
            interval: resident_interval,
            header: Arc::new(AlignmentHeaderMetadata::default()),
            batch,
            pileup: PileupSummary::default(),
            junctions: JunctionSummary::default(),
            pairs: PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 3,
                records_eligible: 3,
                records_emitted: 3,
                complete_read_limit: 100,
                ..QueryStats::default()
            },
        };
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(100).unwrap(),
            coverage_bins: NonZeroUsize::new(37).unwrap(),
            collect_alleles: false,
            ..QueryOptions::default()
        };
        let overlap = AlignmentOverlapIndex::new(&resident.batch);
        let selected = overlap.select(&resident.batch, target);
        let index = AlignmentSummaryIndex::build(
            &resident.batch,
            resident.interval,
            options.overlapping_mate_policy,
        )
        .unwrap();
        let fresh =
            derive_alignment_query(&resident, &overlap, target, &options, None, &|| true).unwrap();
        let indexed_query = derive_alignment_query_with_summary_index_and_phase(
            &resident,
            &overlap,
            Some(&index),
            target,
            &options,
            None,
            &|| true,
            &mut |_| {},
        )
        .unwrap()
        .0;
        let indexed_pileup = index.pileup(
            target,
            options.coverage_bins.get(),
            options.minimum_base_quality,
            &selected,
            selected.len(),
        );
        let mut indexed_junctions =
            JunctionSummaryBuilder::new(target, options.minimum_junction_flank, None);
        for record_index in &selected {
            index.add_record_junctions(*record_index, &mut indexed_junctions);
        }
        assert_eq!(indexed_pileup, fresh.pileup);
        assert_eq!(
            indexed_junctions.finish(selected.len(), true),
            fresh.junctions
        );
        assert_eq!(indexed_query, fresh);
    }
}
