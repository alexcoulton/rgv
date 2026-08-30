use std::{collections::HashMap, sync::OnceLock};

use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};

use crate::reference::ReferenceSlice;
use crate::{AlignmentFlags, ModificationSummary, TemplateId, template_id};

/// Fixed-point scale used for base-quality-weighted allele support.
pub const QUALITY_WEIGHT_SCALE: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlleleSupport {
    pub a: u32,
    pub c: u32,
    pub g: u32,
    pub t: u32,
    pub n: u32,
    pub deletion: u32,
    pub insertion: u32,
}

impl AlleleSupport {
    pub const fn base_observations(self) -> u32 {
        self.a
            .saturating_add(self.c)
            .saturating_add(self.g)
            .saturating_add(self.t)
            .saturating_add(self.n)
    }

    pub const fn reference_observations(self) -> u32 {
        self.base_observations().saturating_add(self.deletion)
    }

    pub const fn for_base(self, base: u8) -> u32 {
        match base.to_ascii_uppercase() {
            b'A' => self.a,
            b'C' => self.c,
            b'G' => self.g,
            b'T' => self.t,
            _ => self.n,
        }
    }

    fn increment_base(&mut self, base: u8) {
        let value = match base.to_ascii_uppercase() {
            b'A' => &mut self.a,
            b'C' => &mut self.c,
            b'G' => &mut self.g,
            b'T' => &mut self.t,
            _ => &mut self.n,
        };
        *value = value.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QualityWeightedAlleleSupport {
    pub a: u64,
    pub c: u64,
    pub g: u64,
    pub t: u64,
    pub n: u64,
}

impl QualityWeightedAlleleSupport {
    pub const fn for_base(self, base: u8) -> u64 {
        match base.to_ascii_uppercase() {
            b'A' => self.a,
            b'C' => self.c,
            b'G' => self.g,
            b'T' => self.t,
            _ => self.n,
        }
    }

    pub const fn total(self) -> u64 {
        self.a
            .saturating_add(self.c)
            .saturating_add(self.g)
            .saturating_add(self.t)
            .saturating_add(self.n)
    }

    fn add_base(&mut self, base: u8, weight: u64) {
        let value = match base.to_ascii_uppercase() {
            b'A' => &mut self.a,
            b'C' => &mut self.c,
            b'G' => &mut self.g,
            b'T' => &mut self.t,
            _ => &mut self.n,
        };
        *value = value.saturating_add(weight);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PileupBin {
    /// Maximum concurrent eligible aligned-base depth at any position in this bin.
    pub depth: u32,
    pub forward_depth: u32,
    pub reverse_depth: u32,
    /// Base observations passing the configured base-quality threshold.
    pub alleles: AlleleSupport,
    pub forward_alleles: AlleleSupport,
    pub reverse_alleles: AlleleSupport,
    /// Alleles partitioned by the conversion strand used by bisulfite display.
    /// Paired records inherit the first end's strand so both mates contribute
    /// to the same biological strand.
    pub bisulfite_forward_alleles: AlleleSupport,
    pub bisulfite_reverse_alleles: AlleleSupport,
    pub quality_weighted: QualityWeightedAlleleSupport,
    pub bisulfite_forward_quality_weighted: QualityWeightedAlleleSupport,
    pub bisulfite_reverse_quality_weighted: QualityWeightedAlleleSupport,
    pub base_quality_sum: u64,
    pub base_quality_observations: u32,
    pub missing_base_quality_observations: u32,
    pub mapping_quality_sum: u64,
    pub mapping_quality_observations: u32,
}

impl PileupBin {
    pub fn mean_base_quality(self) -> Option<f64> {
        (self.base_quality_observations > 0)
            .then(|| self.base_quality_sum as f64 / f64::from(self.base_quality_observations))
    }

    pub fn mean_mapping_quality(self) -> Option<f64> {
        (self.mapping_quality_observations > 0)
            .then(|| self.mapping_quality_sum as f64 / f64::from(self.mapping_quality_observations))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PileupProvenance {
    pub eligible_records: usize,
    pub rendered_records: usize,
    /// False only when cancellation or a future bounded approximation prevents a complete pileup.
    pub all_eligible_records: bool,
    pub allele_observations_available: bool,
    pub overlapping_mate_policy: OverlappingMatePolicy,
    pub overlap_collapse_complete: bool,
    pub collapsed_mate_bases: u64,
    /// True when every eligible aligned segment was included in the exact
    /// interval sweep used to calculate per-bin maximum depth.
    pub exact_depth_complete: bool,
    /// Aligned segments omitted because the bounded event budget was exhausted.
    pub depth_segments_omitted: u64,
}

/// Each retained aligned segment consumes two sweep events. The bound keeps a
/// malformed or adversarial CIGAR from turning a viewport query into unbounded
/// memory growth; incomplete results are explicitly reported in provenance.
pub const MAX_PILEUP_DEPTH_EVENTS: usize = 2_000_000;

pub(crate) const MAX_PENDING_PAIR_OVERLAPS: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlapMask {
    pub start: u64,
    pub end: u64,
}

impl OverlapMask {
    pub const fn contains(self, position: u64) -> bool {
        position >= self.start && position < self.end
    }
}

pub fn add_unmasked_range(
    start: u64,
    end: u64,
    mask: Option<OverlapMask>,
    mut add: impl FnMut(u64, u64),
) -> u64 {
    let Some(mask) = mask else {
        add(start, end);
        return 0;
    };
    let overlap_start = start.max(mask.start);
    let overlap_end = end.min(mask.end);
    if overlap_start >= overlap_end {
        add(start, end);
        return 0;
    }
    if start < overlap_start {
        add(start, overlap_start);
    }
    if overlap_end < end {
        add(overlap_end, end);
    }
    overlap_end - overlap_start
}

pub struct OverlapCollapser {
    policy: OverlappingMatePolicy,
    pending: HashMap<TemplateId, OverlapMask>,
    complete: bool,
    collapsed_mate_bases: u64,
}

impl OverlapCollapser {
    pub fn new(policy: OverlappingMatePolicy) -> Self {
        Self {
            policy,
            pending: HashMap::new(),
            complete: true,
            collapsed_mate_bases: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mask_for(
        &mut self,
        read_name: &[u8],
        contig: ContigId,
        start: u64,
        end: u64,
        flags: AlignmentFlags,
        mate_contig: Option<ContigId>,
        mate_start: Option<u64>,
    ) -> Option<OverlapMask> {
        if self.policy == OverlappingMatePolicy::CountBoth
            || read_name.is_empty()
            || !flags.is_paired()
            || flags.is_unmapped()
            || flags.is_mate_unmapped()
            || flags.is_secondary()
            || flags.is_supplementary()
            || mate_contig != Some(contig)
        {
            return None;
        }
        let mate_start = mate_start?;
        let identity = template_id(read_name);
        if start == mate_start {
            return (!flags.is_first_in_template()).then_some(OverlapMask { start, end });
        }
        let is_left = start < mate_start;
        if is_left {
            if end > mate_start {
                if self.pending.len() < MAX_PENDING_PAIR_OVERLAPS {
                    self.pending.insert(
                        identity,
                        OverlapMask {
                            start: mate_start,
                            end,
                        },
                    );
                } else {
                    self.complete = false;
                }
            }
            None
        } else {
            self.pending.remove(&identity)
        }
    }

    pub fn note_collapsed(&mut self, bases: u64) {
        self.collapsed_mate_bases = self.collapsed_mate_bases.saturating_add(bases);
    }

    pub const fn policy(&self) -> OverlappingMatePolicy {
        self.policy
    }

    pub const fn complete(&self) -> bool {
        self.complete
    }

    pub const fn collapsed_mate_bases(&self) -> u64 {
        self.collapsed_mate_bases
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PileupSummary {
    pub bins: Vec<PileupBin>,
    pub minimum_base_quality: u8,
    pub provenance: PileupProvenance,
    pub modifications: ModificationSummary,
}

impl PileupSummary {
    /// Builds a depth-only summary for synthetic workloads and compatibility fixtures.
    pub fn from_depths(depths: impl IntoIterator<Item = u32>) -> Self {
        Self {
            bins: depths
                .into_iter()
                .map(|depth| PileupBin {
                    depth,
                    ..PileupBin::default()
                })
                .collect(),
            ..Self::default()
        }
    }

    pub fn estimated_bytes(&self) -> usize {
        self.bins
            .len()
            .saturating_mul(std::mem::size_of::<PileupBin>())
            .saturating_add(self.modifications.estimated_bytes())
    }

    pub fn maximum_depth(&self) -> u32 {
        self.bins
            .iter()
            .map(|bin| bin.depth)
            .max()
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.bins.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bins.len()
    }

    pub fn bin_at(
        &self,
        query_interval: GenomicInterval,
        position: u64,
    ) -> Option<(usize, PileupBin)> {
        if self.bins.is_empty() || !query_interval.contains(position) {
            return None;
        }
        let offset = u128::from(position - query_interval.start());
        let index =
            offset.saturating_mul(self.bins.len() as u128) / u128::from(query_interval.len());
        let index = usize::try_from(index).ok()?.min(self.bins.len() - 1);
        Some((index, self.bins[index]))
    }

    pub fn bin_interval(
        &self,
        query_interval: GenomicInterval,
        index: usize,
    ) -> Option<GenomicInterval> {
        if self.bins.is_empty() || index >= self.bins.len() {
            return None;
        }
        let query_length = u128::from(query_interval.len());
        let bins = self.bins.len() as u128;
        let start_offset = (index as u128).saturating_mul(query_length) / bins;
        let end_offset = (index as u128 + 1)
            .saturating_mul(query_length)
            .div_ceil(bins);
        let start = query_interval
            .start()
            .saturating_add(u64::try_from(start_offset).ok()?);
        let end = query_interval
            .start()
            .saturating_add(u64::try_from(end_offset).ok()?)
            .min(query_interval.end());
        GenomicInterval::new(query_interval.contig(), start, end).ok()
    }

    /// Returns a base-only consensus when the summary has exact one-bin-per-base resolution.
    /// Indels are intentionally reported separately and are not incorporated into this sequence.
    pub fn consensus(
        &self,
        query_interval: GenomicInterval,
        requested: GenomicInterval,
        reference: Option<&ReferenceSlice>,
    ) -> Option<Vec<u8>> {
        if query_interval.contig() != requested.contig()
            || !query_interval.contains_interval(requested)
            || self.bins.len() != usize::try_from(query_interval.len()).ok()?
        {
            return None;
        }
        let mut consensus = Vec::with_capacity(usize::try_from(requested.len()).ok()?);
        for position in requested.start()..requested.end() {
            let index = usize::try_from(position - query_interval.start()).ok()?;
            let support = self.bins[index].quality_weighted;
            let candidates = [
                (b'A', support.a),
                (b'C', support.c),
                (b'G', support.g),
                (b'T', support.t),
                (b'N', support.n),
            ];
            let total = support.total();
            let base = if total == 0 {
                reference
                    .and_then(|reference| reference.base_at(position))
                    .map_or(b'N', |base| base.to_ascii_uppercase())
            } else {
                let (base, maximum) = candidates
                    .into_iter()
                    .max_by_key(|(_, value)| *value)
                    .unwrap();
                if maximum.saturating_mul(2) > total {
                    base
                } else {
                    b'N'
                }
            };
            consensus.push(base);
        }
        Some(consensus)
    }
}

pub struct PileupBuilder {
    interval: GenomicInterval,
    bins: Vec<PileupBin>,
    minimum_base_quality: u8,
    depth_events: Vec<DepthEvent>,
    depth_events_complete: bool,
    depth_segments_omitted: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DepthEvent {
    position: u64,
    depth: i64,
    forward: i64,
    reverse: i64,
    mapping_quality_sum: i64,
    mapping_quality_count: i64,
}

impl PileupBuilder {
    pub fn new(interval: GenomicInterval, bins: usize, minimum_base_quality: u8) -> Self {
        Self {
            interval,
            bins: vec![PileupBin::default(); bins],
            minimum_base_quality,
            depth_events: Vec::new(),
            depth_events_complete: true,
            depth_segments_omitted: 0,
        }
    }

    pub fn add_aligned_record(
        &mut self,
        segments: impl IntoIterator<Item = (u64, u64)>,
        reverse: bool,
        mapping_quality: Option<u8>,
    ) {
        let mut merged: Option<(u64, u64)> = None;
        for (start, end) in segments {
            let start = start.max(self.interval.start());
            let end = end.min(self.interval.end());
            if start >= end {
                continue;
            }
            if let Some((merged_start, merged_end)) = merged {
                if start <= merged_end {
                    merged = Some((merged_start, merged_end.max(end)));
                } else {
                    self.add_aligned_segment(merged_start, merged_end, reverse, mapping_quality);
                    merged = Some((start, end));
                }
            } else {
                merged = Some((start, end));
            }
        }
        if let Some((start, end)) = merged {
            self.add_aligned_segment(start, end, reverse, mapping_quality);
        }
    }

    fn add_aligned_segment(
        &mut self,
        start: u64,
        end: u64,
        reverse: bool,
        mapping_quality: Option<u8>,
    ) {
        if self.depth_events.len().saturating_add(2) > MAX_PILEUP_DEPTH_EVENTS {
            self.depth_events_complete = false;
            self.depth_segments_omitted = self.depth_segments_omitted.saturating_add(1);
            return;
        }
        let forward = i64::from(!reverse);
        let reverse = i64::from(reverse);
        let mapping_quality_sum = mapping_quality.map_or(0, i64::from);
        let mapping_quality_count = i64::from(mapping_quality.is_some());
        self.depth_events.push(DepthEvent {
            position: start,
            depth: 1,
            forward,
            reverse,
            mapping_quality_sum,
            mapping_quality_count,
        });
        self.depth_events.push(DepthEvent {
            position: end,
            depth: -1,
            forward: -forward,
            reverse: -reverse,
            mapping_quality_sum: -mapping_quality_sum,
            mapping_quality_count: -mapping_quality_count,
        });
    }

    #[cfg(test)]
    pub(crate) fn add_base(&mut self, position: u64, base: u8, quality: Option<u8>, reverse: bool) {
        self.add_base_with_bisulfite_strand(position, base, quality, reverse, reverse);
    }

    pub fn add_base_with_bisulfite_strand(
        &mut self,
        position: u64,
        base: u8,
        quality: Option<u8>,
        reverse: bool,
        bisulfite_reverse: bool,
    ) {
        let Some(index) = self.bin_index(position) else {
            return;
        };
        if quality.is_some_and(|quality| quality < self.minimum_base_quality) {
            return;
        }
        let bin = &mut self.bins[index];
        bin.alleles.increment_base(base);
        if reverse {
            bin.reverse_alleles.increment_base(base);
        } else {
            bin.forward_alleles.increment_base(base);
        }
        if bisulfite_reverse {
            bin.bisulfite_reverse_alleles.increment_base(base);
        } else {
            bin.bisulfite_forward_alleles.increment_base(base);
        }
        let weight = quality.map_or(QUALITY_WEIGHT_SCALE, quality_weight);
        bin.quality_weighted.add_base(base, weight);
        if bisulfite_reverse {
            bin.bisulfite_reverse_quality_weighted
                .add_base(base, weight);
        } else {
            bin.bisulfite_forward_quality_weighted
                .add_base(base, weight);
        }
        if let Some(quality) = quality {
            bin.base_quality_sum = bin.base_quality_sum.saturating_add(u64::from(quality));
            bin.base_quality_observations = bin.base_quality_observations.saturating_add(1);
        } else {
            bin.missing_base_quality_observations =
                bin.missing_base_quality_observations.saturating_add(1);
        }
    }

    pub fn add_deletion(&mut self, start: u64, end: u64, reverse: bool) {
        let Some((first, last)) = self.bin_range(start, end) else {
            return;
        };
        for bin in &mut self.bins[first..last] {
            bin.alleles.deletion = bin.alleles.deletion.saturating_add(1);
            let strand = if reverse {
                &mut bin.reverse_alleles
            } else {
                &mut bin.forward_alleles
            };
            strand.deletion = strand.deletion.saturating_add(1);
        }
    }

    pub fn add_insertion(&mut self, reference_position: u64, reverse: bool) {
        let anchor = reference_position.saturating_sub(1);
        let index = self
            .bin_index(anchor)
            .or_else(|| self.bin_index(reference_position));
        if let Some(index) = index {
            let bin = &mut self.bins[index];
            bin.alleles.insertion = bin.alleles.insertion.saturating_add(1);
            let strand = if reverse {
                &mut bin.reverse_alleles
            } else {
                &mut bin.forward_alleles
            };
            strand.insertion = strand.insertion.saturating_add(1);
        }
    }

    /// Returns exact allele/indel bins without deriving aligned-depth events.
    ///
    /// This is used by independently owned allele tiles whose depth lives in a different
    /// allocation and contract. Callers must not add aligned-depth segments to this builder.
    #[must_use]
    pub fn finish_alleles(mut self) -> Vec<PileupBin> {
        debug_assert!(self.depth_events.is_empty());
        self.bins.shrink_to_fit();
        self.bins
    }

    pub fn finish(mut self, mut provenance: PileupProvenance) -> PileupSummary {
        let mut depth = 0_i64;
        let mut forward = 0_i64;
        let mut reverse = 0_i64;
        let mut mapping_quality_sum = 0_i64;
        let mut mapping_quality_count = 0_i64;
        self.depth_events
            .sort_unstable_by_key(|event| event.position);
        let mut cursor = 0;
        while cursor < self.depth_events.len() {
            let position = self.depth_events[cursor].position;
            while cursor < self.depth_events.len() && self.depth_events[cursor].position == position
            {
                let event = self.depth_events[cursor];
                depth = depth.saturating_add(event.depth);
                forward = forward.saturating_add(event.forward);
                reverse = reverse.saturating_add(event.reverse);
                mapping_quality_sum = mapping_quality_sum.saturating_add(event.mapping_quality_sum);
                mapping_quality_count =
                    mapping_quality_count.saturating_add(event.mapping_quality_count);
                cursor += 1;
            }
            let next_position = self
                .depth_events
                .get(cursor)
                .map_or(self.interval.end(), |event| event.position)
                .min(self.interval.end());
            if depth <= 0 || position >= next_position {
                continue;
            }
            if let Some((first, last)) = self.bin_range(position, next_position) {
                let candidate_depth = nonnegative_u32(depth);
                for bin in &mut self.bins[first..last] {
                    if candidate_depth > bin.depth {
                        bin.depth = candidate_depth;
                        bin.forward_depth = nonnegative_u32(forward);
                        bin.reverse_depth = nonnegative_u32(reverse);
                        bin.mapping_quality_sum =
                            u64::try_from(mapping_quality_sum.max(0)).unwrap_or(u64::MAX);
                        bin.mapping_quality_observations = nonnegative_u32(mapping_quality_count);
                    }
                }
            }
        }
        provenance.exact_depth_complete =
            self.depth_events_complete && provenance.all_eligible_records;
        provenance.depth_segments_omitted = self.depth_segments_omitted;
        PileupSummary {
            bins: self.bins,
            minimum_base_quality: self.minimum_base_quality,
            provenance,
            modifications: ModificationSummary::default(),
        }
    }

    fn bin_index(&self, position: u64) -> Option<usize> {
        if self.bins.is_empty() || !self.interval.contains(position) {
            return None;
        }
        let offset = u128::from(position - self.interval.start());
        let index =
            offset.saturating_mul(self.bins.len() as u128) / u128::from(self.interval.len());
        usize::try_from(index)
            .ok()
            .map(|index| index.min(self.bins.len() - 1))
    }

    fn bin_range(&self, start: u64, end: u64) -> Option<(usize, usize)> {
        let start = start.max(self.interval.start());
        let end = end.min(self.interval.end());
        if start >= end || self.bins.is_empty() {
            return None;
        }
        let interval_length = u128::from(self.interval.len());
        let bins = self.bins.len() as u128;
        let first =
            u128::from(start - self.interval.start()).saturating_mul(bins) / interval_length;
        let last = u128::from(end - self.interval.start())
            .saturating_mul(bins)
            .div_ceil(interval_length);
        let first = usize::try_from(first).ok()?.min(self.bins.len() - 1);
        let last = usize::try_from(last)
            .ok()?
            .clamp(first + 1, self.bins.len());
        Some((first, last))
    }
}

fn nonnegative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn quality_weight(quality: u8) -> u64 {
    static WEIGHTS: OnceLock<[u64; 256]> = OnceLock::new();
    WEIGHTS.get_or_init(|| {
        std::array::from_fn(|quality| {
            let confidence = 1.0 - 10_f64.powf(-(quality as f64) / 10.0);
            (confidence * QUALITY_WEIGHT_SCALE as f64).round() as u64
        })
    })[usize::from(quality)]
}

pub fn aligned_observation(
    bases: &[u8],
    qualities: &[u8],
    read_position: usize,
) -> Option<(u8, Option<u8>)> {
    let base = *bases.get(read_position)?;
    let quality = qualities
        .get(read_position)
        .copied()
        .filter(|quality| *quality != u8::MAX);
    Some((base, quality))
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;

    use super::*;

    fn interval() -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), 100, 104).unwrap()
    }

    #[test]
    fn pileup_tracks_alleles_strands_qualities_and_indels() {
        let mut builder = PileupBuilder::new(interval(), 4, 10);
        builder.add_aligned_record([(100, 104)], false, Some(60));
        builder.add_aligned_record([(101, 103)], true, Some(20));
        builder.add_base(100, b'A', Some(30), false);
        builder.add_base(101, b'C', Some(9), false);
        builder.add_base(101, b'T', Some(20), true);
        builder.add_base(102, b'G', None, true);
        builder.add_deletion(102, 104, true);
        builder.add_insertion(103, false);
        let summary = builder.finish(PileupProvenance {
            eligible_records: 2,
            rendered_records: 1,
            all_eligible_records: true,
            allele_observations_available: true,
            ..PileupProvenance::default()
        });

        assert_eq!(summary.bins[0].depth, 1);
        assert_eq!(summary.bins[1].depth, 2);
        assert_eq!(summary.bins[1].forward_depth, 1);
        assert_eq!(summary.bins[1].reverse_depth, 1);
        assert_eq!(summary.bins[0].alleles.a, 1);
        assert_eq!(summary.bins[1].alleles.c, 0);
        assert_eq!(summary.bins[1].alleles.t, 1);
        assert_eq!(summary.bins[1].forward_alleles.t, 0);
        assert_eq!(summary.bins[1].reverse_alleles.t, 1);
        assert_eq!(summary.bins[2].alleles.g, 1);
        assert_eq!(summary.bins[2].alleles.deletion, 1);
        assert_eq!(summary.bins[2].reverse_alleles.deletion, 1);
        assert_eq!(summary.bins[2].alleles.insertion, 1);
        assert_eq!(summary.bins[2].forward_alleles.insertion, 1);
        assert_eq!(summary.bins[1].mapping_quality_sum, 80);
        assert_eq!(summary.bins[1].mapping_quality_observations, 2);
        assert_eq!(summary.bins[2].missing_base_quality_observations, 1);
        assert!(summary.bins[0].quality_weighted.a < QUALITY_WEIGHT_SCALE);
        assert!(summary.provenance.all_eligible_records);
    }

    #[test]
    fn pileup_keeps_bisulfite_template_strand_separate_from_alignment_strand() {
        let mut builder = PileupBuilder::new(interval(), 4, 0);
        builder.add_base_with_bisulfite_strand(100, b'T', Some(30), true, false);
        let summary = builder.finish(PileupProvenance::default());
        let bin = summary.bins[0];

        assert_eq!(bin.reverse_alleles.t, 1);
        assert_eq!(bin.forward_alleles.t, 0);
        assert_eq!(bin.bisulfite_forward_alleles.t, 1);
        assert_eq!(bin.bisulfite_reverse_alleles.t, 0);
        assert_eq!(
            bin.bisulfite_forward_quality_weighted.t,
            bin.quality_weighted.t
        );
    }

    #[test]
    fn aggregated_bins_count_each_overlapping_read_once_for_depth() {
        let mut builder = PileupBuilder::new(interval(), 2, 0);
        builder.add_aligned_record(
            [(100, 101), (101, 102), (102, 103), (103, 104)],
            false,
            Some(60),
        );
        for (offset, base) in b"ACGT".iter().copied().enumerate() {
            builder.add_base(100 + offset as u64, base, Some(30), false);
        }
        let summary = builder.finish(PileupProvenance::default());
        assert_eq!(summary.bins[0].depth, 1);
        assert_eq!(summary.bins[1].depth, 1);
        assert_eq!(summary.bins[0].mapping_quality_sum, 60);
        assert_eq!(summary.bins[0].mapping_quality_observations, 1);
        assert_eq!(summary.bins[1].mapping_quality_sum, 60);
        assert_eq!(summary.bins[1].mapping_quality_observations, 1);
        assert_eq!(summary.bins[0].alleles.base_observations(), 2);
        assert_eq!(summary.bins[1].alleles.base_observations(), 2);
    }

    #[test]
    fn aggregated_bin_reports_maximum_concurrent_depth_not_touching_reads() {
        let mut builder = PileupBuilder::new(interval(), 1, 0);
        builder.add_aligned_record([(100, 101)], false, Some(60));
        builder.add_aligned_record([(103, 104)], true, Some(20));
        let summary = builder.finish(PileupProvenance {
            all_eligible_records: true,
            ..PileupProvenance::default()
        });

        assert_eq!(summary.bins[0].depth, 1);
        assert_eq!(summary.bins[0].forward_depth, 1);
        assert_eq!(summary.bins[0].reverse_depth, 0);
        assert_eq!(summary.bins[0].mapping_quality_sum, 60);
        assert_eq!(summary.bins[0].mapping_quality_observations, 1);
        assert!(summary.provenance.exact_depth_complete);
    }

    #[test]
    fn exact_pileup_builds_quality_weighted_base_consensus() {
        let mut builder = PileupBuilder::new(interval(), 4, 0);
        for _ in 0..3 {
            builder.add_base(100, b'A', Some(30), false);
        }
        builder.add_base(100, b'C', Some(10), false);
        builder.add_base(101, b'C', Some(30), false);
        builder.add_base(101, b'G', Some(30), true);
        let summary = builder.finish(PileupProvenance::default());
        let reference = ReferenceSlice {
            interval: interval(),
            bases: b"ACGT".to_vec(),
        };
        assert_eq!(
            summary.consensus(interval(), interval(), Some(&reference)),
            Some(b"ANGT".to_vec())
        );
        let aggregated = PileupSummary::from_depths([1; 2]);
        assert_eq!(
            aggregated.consensus(interval(), interval(), Some(&reference)),
            None
        );
    }

    #[test]
    fn reverse_observations_use_sam_alignment_order_without_a_second_reverse_complement() {
        assert_eq!(
            aligned_observation(b"AACG", &[10, 20, 30, 40], 0),
            Some((b'A', Some(10)))
        );
        assert_eq!(
            aligned_observation(b"AACG", &[10, 20, 30, 40], 3),
            Some((b'G', Some(40)))
        );
    }
}
