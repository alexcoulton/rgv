use std::{collections::HashMap, sync::OnceLock};

use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};

use crate::reference::ReferenceSlice;
use crate::{AlignmentFlags, CigarOp, CigarOpKind, ModificationSummary, TemplateId, template_id};

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

#[doc(hidden)]
pub const MAX_PENDING_PAIR_OVERLAPS: usize = 100_000;

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

/// Reference ranges that the later-arriving mate of an overlapping pair must not count again.
///
/// Unlike [`OverlapMask`], this follows what the earlier mate actually contributed: a deletion
/// or `N` skip in the earlier mate does not hide the later mate's aligned bases from depth, and
/// an earlier base that failed the base-quality threshold does not hide the later mate's base.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MateOverlapMask {
    /// Positions where the earlier mate has an aligned base (`M`, `=`, `X`).
    aligned: Vec<(u64, u64)>,
    /// Positions the earlier mate spans with an aligned base or a deletion.
    spanned: Vec<(u64, u64)>,
    /// Positions where the earlier mate contributed an allele observation: a base passing the
    /// caller's base-quality eligibility, or a deletion.
    observed: Vec<(u64, u64)>,
}

impl MateOverlapMask {
    /// Whether the later mate's aligned base at `position` is already counted in depth.
    pub fn masks_depth(&self, position: u64) -> bool {
        ranges_contain(&self.aligned, position)
    }

    /// Whether the later mate's base observation at `position` duplicates an earlier-mate
    /// base or deletion observation.
    pub fn masks_base(&self, position: u64) -> bool {
        ranges_contain(&self.observed, position)
    }

    /// Whether an indel event of the later mate at `position` lies where the earlier mate
    /// already describes the template (aligned base or deletion).
    pub fn masks_indel(&self, position: u64) -> bool {
        ranges_contain(&self.spanned, position)
    }

    /// Emits the parts of an aligned segment not already counted by the earlier mate and
    /// returns the number of collapsed reference bases.
    pub fn add_unmasked_depth_range(&self, start: u64, end: u64, add: impl FnMut(u64, u64)) -> u64 {
        add_outside_ranges(&self.aligned, start, end, add, |_, _| {})
    }

    /// Emits the parts of a deletion not already described by the earlier mate and returns the
    /// number of collapsed reference bases.
    pub fn add_unmasked_deletion_range(
        &self,
        start: u64,
        end: u64,
        add: impl FnMut(u64, u64),
    ) -> u64 {
        add_outside_ranges(&self.spanned, start, end, add, |_, _| {})
    }

    /// Like [`Self::add_unmasked_depth_range`], additionally reporting each collapsed range.
    pub fn split_depth_range(
        &self,
        start: u64,
        end: u64,
        add: impl FnMut(u64, u64),
        collapsed: impl FnMut(u64, u64),
    ) -> u64 {
        add_outside_ranges(&self.aligned, start, end, add, collapsed)
    }

    fn is_empty(&self) -> bool {
        self.spanned.is_empty()
    }

    fn single_range(start: u64, end: u64) -> Self {
        Self {
            aligned: vec![(start, end)],
            spanned: vec![(start, end)],
            observed: vec![(start, end)],
        }
    }

    /// Builds the earlier mate's contribution inside `window` from its CIGAR.
    fn from_alignment(
        alignment_start: u64,
        window: (u64, u64),
        cigar: impl IntoIterator<Item = CigarOp>,
        mut base_is_observed: impl FnMut(usize) -> bool,
    ) -> Self {
        let mut mask = Self::default();
        let mut reference_position = alignment_start;
        let mut read_position = 0_usize;
        for operation in cigar {
            let length = u64::from(operation.len);
            match operation.kind {
                CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                    let end = reference_position.saturating_add(length);
                    let clipped_start = reference_position.max(window.0);
                    let clipped_end = end.min(window.1);
                    if clipped_start < clipped_end {
                        push_range(&mut mask.aligned, clipped_start, clipped_end);
                        push_range(&mut mask.spanned, clipped_start, clipped_end);
                        for position in clipped_start..clipped_end {
                            let offset = usize::try_from(position - reference_position)
                                .unwrap_or(usize::MAX);
                            if base_is_observed(read_position.saturating_add(offset)) {
                                push_range(&mut mask.observed, position, position + 1);
                            }
                        }
                    }
                    read_position = read_position.saturating_add(operation.len as usize);
                    reference_position = end;
                }
                CigarOpKind::Deletion => {
                    let end = reference_position.saturating_add(length);
                    let clipped_start = reference_position.max(window.0);
                    let clipped_end = end.min(window.1);
                    if clipped_start < clipped_end {
                        push_range(&mut mask.spanned, clipped_start, clipped_end);
                        push_range(&mut mask.observed, clipped_start, clipped_end);
                    }
                    reference_position = end;
                }
                CigarOpKind::ReferenceSkip => {
                    reference_position = reference_position.saturating_add(length);
                }
                CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                    read_position = read_position.saturating_add(operation.len as usize);
                }
                CigarOpKind::HardClip | CigarOpKind::Padding => {}
            }
            if reference_position >= window.1 {
                break;
            }
        }
        mask
    }
}

fn push_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if let Some((_, last_end)) = ranges.last_mut()
        && *last_end >= start
    {
        *last_end = (*last_end).max(end);
    } else {
        ranges.push((start, end));
    }
}

fn ranges_contain(ranges: &[(u64, u64)], position: u64) -> bool {
    let index = ranges.partition_point(|(_, end)| *end <= position);
    ranges
        .get(index)
        .is_some_and(|(start, _)| *start <= position)
}

/// Splits `[start, end)` against sorted disjoint `ranges`; returns the covered base count.
fn add_outside_ranges(
    ranges: &[(u64, u64)],
    start: u64,
    end: u64,
    mut add: impl FnMut(u64, u64),
    mut covered: impl FnMut(u64, u64),
) -> u64 {
    let mut cursor = start;
    let mut total = 0_u64;
    let first = ranges.partition_point(|(_, range_end)| *range_end <= start);
    for (range_start, range_end) in &ranges[first..] {
        if *range_start >= end {
            break;
        }
        let overlap_start = (*range_start).max(cursor);
        let overlap_end = (*range_end).min(end);
        if overlap_start >= overlap_end {
            continue;
        }
        if cursor < overlap_start {
            add(cursor, overlap_start);
        }
        covered(overlap_start, overlap_end);
        total = total.saturating_add(overlap_end - overlap_start);
        cursor = overlap_end;
    }
    if cursor < end {
        add(cursor, end);
    }
    total
}

/// Identity and placement of one alignment as needed for mate-overlap collapsing.
#[derive(Clone, Copy, Debug)]
pub struct MateAlignment<'a> {
    pub read_name: &'a [u8],
    pub contig: ContigId,
    pub start: u64,
    pub end: u64,
    pub flags: AlignmentFlags,
    pub mate_contig: Option<ContigId>,
    pub mate_start: Option<u64>,
}

struct PendingOverlap {
    contig: ContigId,
    mate_start: u64,
    mask: MateOverlapMask,
}

/// Pending-entry count at which stale entries are first swept.
const PENDING_SWEEP_FLOOR: usize = 4_096;

pub struct OverlapCollapser {
    policy: OverlappingMatePolicy,
    pending: HashMap<TemplateId, PendingOverlap>,
    complete: bool,
    collapsed_mate_bases: u64,
    /// Placement of the most recent record; used to prove coordinate order.
    last_placement: Option<(ContigId, u64)>,
    /// True while every record arrived in non-decreasing `(contig, start)` order.
    coordinate_ordered: bool,
    /// True once a sweep discarded entries on the strength of coordinate order.
    swept_entries: bool,
    next_sweep: usize,
}

impl OverlapCollapser {
    pub fn new(policy: OverlappingMatePolicy) -> Self {
        Self {
            policy,
            pending: HashMap::new(),
            complete: true,
            collapsed_mate_bases: 0,
            last_placement: None,
            coordinate_ordered: true,
            swept_entries: false,
            next_sweep: PENDING_SWEEP_FLOOR,
        }
    }

    /// Legacy single-range mask: the later mate is masked over `[mate_start, earlier.end)`
    /// regardless of the earlier mate's CIGAR or base qualities. Prefer
    /// [`Self::mask_for_alignment`], which masks only what the earlier mate contributed.
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
        let mate = MateAlignment {
            read_name,
            contig,
            start,
            end,
            flags,
            mate_contig,
            mate_start,
        };
        let mate_start = self.eligible_mate_start(&mate)?;
        self.note_placement(contig, start);
        if start == mate_start {
            return (!flags.is_first_in_template()).then_some(OverlapMask { start, end });
        }
        let identity = template_id(read_name);
        if start < mate_start {
            if end > mate_start {
                self.insert_pending(
                    identity,
                    contig,
                    mate_start,
                    MateOverlapMask::single_range(mate_start, end),
                );
            }
            None
        } else {
            let pending = self.pending.remove(&identity)?;
            let (start, _) = *pending.mask.spanned.first()?;
            let (_, end) = *pending.mask.spanned.last()?;
            Some(OverlapMask { start, end })
        }
    }

    /// Returns the ranges this alignment must not count because its mate, seen earlier, already
    /// contributed them.
    ///
    /// `cigar` is this alignment's CIGAR and `base_is_observed(read_index)` reports whether the
    /// aligned base at that `SEQ` index is counted as an allele observation (present and passing
    /// the base-quality threshold). Pass `|_| true` when base observations are not collected.
    /// When both mates start at the same position the first to arrive is counted in full.
    pub fn mask_for_alignment(
        &mut self,
        mate: MateAlignment<'_>,
        cigar: impl IntoIterator<Item = CigarOp>,
        base_is_observed: impl FnMut(usize) -> bool,
    ) -> Option<MateOverlapMask> {
        let mate_start = self.eligible_mate_start(&mate)?;
        self.note_placement(mate.contig, mate.start);
        let identity = template_id(mate.read_name);
        let arrives_second = mate.start > mate_start
            || (mate.start == mate_start
                && self.pending.get(&identity).is_some_and(|pending| {
                    pending.contig == mate.contig && pending.mate_start == mate_start
                }));
        if arrives_second {
            return self.pending.remove(&identity).map(|pending| pending.mask);
        }
        if mate.end > mate_start {
            let mask = MateOverlapMask::from_alignment(
                mate.start,
                (mate_start, mate.end),
                cigar,
                base_is_observed,
            );
            if !mask.is_empty() {
                self.insert_pending(identity, mate.contig, mate_start, mask);
            }
        }
        None
    }

    fn eligible_mate_start(&self, mate: &MateAlignment<'_>) -> Option<u64> {
        if self.policy == OverlappingMatePolicy::CountBoth
            || mate.read_name.is_empty()
            || !mate.flags.is_paired()
            || mate.flags.is_unmapped()
            || mate.flags.is_mate_unmapped()
            || mate.flags.is_secondary()
            || mate.flags.is_supplementary()
            || mate.mate_contig != Some(mate.contig)
        {
            return None;
        }
        mate.mate_start
    }

    fn note_placement(&mut self, contig: ContigId, start: u64) {
        let placement = (contig, start);
        if self.last_placement.is_some_and(|last| placement < last) && self.coordinate_ordered {
            self.coordinate_ordered = false;
            // Entries were discarded assuming coordinate order; their mates may still arrive.
            if self.swept_entries {
                self.complete = false;
            }
        }
        self.last_placement = Some(placement);
    }

    fn insert_pending(
        &mut self,
        identity: TemplateId,
        contig: ContigId,
        mate_start: u64,
        mask: MateOverlapMask,
    ) {
        if self.pending.len() >= self.next_sweep.min(MAX_PENDING_PAIR_OVERLAPS) {
            self.sweep_stale();
        }
        if self.pending.len() < MAX_PENDING_PAIR_OVERLAPS || self.pending.contains_key(&identity) {
            self.pending.insert(
                identity,
                PendingOverlap {
                    contig,
                    mate_start,
                    mask,
                },
            );
        } else {
            self.complete = false;
        }
    }

    /// Drops entries whose mate can no longer arrive: in coordinate-ordered input a mate that
    /// starts before the current record was either already consumed or never admitted
    /// (filtered out, or outside the query).
    fn sweep_stale(&mut self) {
        let Some(current) = self.last_placement.filter(|_| self.coordinate_ordered) else {
            return;
        };
        let before = self.pending.len();
        self.pending
            .retain(|_, pending| (pending.contig, pending.mate_start) >= current);
        self.swept_entries |= self.pending.len() != before;
        self.next_sweep = self
            .pending
            .len()
            .saturating_mul(2)
            .max(PENDING_SWEEP_FLOOR);
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

    /// Earlier mates currently remembered for the count-once rule.
    #[doc(hidden)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
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

    /// Base-quality threshold below which base observations are not counted.
    pub const fn minimum_base_quality(&self) -> u8 {
        self.minimum_base_quality
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
    #[doc(hidden)]
    pub fn add_base(&mut self, position: u64, base: u8, quality: Option<u8>, reverse: bool) {
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

/// Whether the aligned base at `read_position` is counted as a base observation: the base is
/// present and its quality is either missing or at least `minimum_base_quality`.
///
/// This is the eligibility rule [`aligned_observation`] and
/// [`PileupBuilder::add_base_with_bisulfite_strand`] apply together.
pub fn base_observation_is_counted(
    base_count: usize,
    qualities: &[u8],
    read_position: usize,
    minimum_base_quality: u8,
) -> bool {
    read_position < base_count
        && qualities
            .get(read_position)
            .is_none_or(|quality| *quality == u8::MAX || *quality >= minimum_base_quality)
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

    const PAIRED_FIRST: u16 = 0x1 | 0x20 | 0x40;
    const PAIRED_LAST_REVERSE: u16 = 0x1 | 0x10 | 0x80;

    fn mate(name: &[u8], start: u64, end: u64, flags: u16, mate_start: u64) -> MateAlignment<'_> {
        MateAlignment {
            read_name: name,
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(flags),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(mate_start),
        }
    }

    fn op(kind: CigarOpKind, len: u32) -> CigarOp {
        CigarOp { kind, len }
    }

    #[test]
    fn mate_mask_follows_the_earlier_mates_cigar_and_base_quality() {
        // Earlier mate: start 100, 4M2D4M -> bases at 100..104 and 106..110, deletion 104..106.
        // Read index 4 (reference 106) fails base quality. Later mate: 102..112, all aligned.
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        let qualities = [30_u8, 30, 30, 30, 5, 30, 30, 30];
        let earlier = collapser.mask_for_alignment(
            mate(b"pair", 100, 110, PAIRED_FIRST, 102),
            [
                op(CigarOpKind::Match, 4),
                op(CigarOpKind::Deletion, 2),
                op(CigarOpKind::Match, 4),
            ],
            |index| base_observation_is_counted(8, &qualities, index, 10),
        );
        assert_eq!(earlier, None);
        let mask = collapser
            .mask_for_alignment(
                mate(b"pair", 102, 112, PAIRED_LAST_REVERSE, 100),
                [op(CigarOpKind::Match, 10)],
                |_| true,
            )
            .expect("the later mate receives the earlier mate's contribution");
        assert_eq!(collapser.pending_len(), 0);

        let depth_masked = (100..114)
            .filter(|position| mask.masks_depth(*position))
            .collect::<Vec<_>>();
        assert_eq!(depth_masked, [102, 103, 106, 107, 108, 109]);
        let base_masked = (100..114)
            .filter(|position| mask.masks_base(*position))
            .collect::<Vec<_>>();
        // 104/105: the earlier mate's deletion is the template's observation there.
        // 106: the earlier base failed quality, so the later mate's base must be counted.
        assert_eq!(base_masked, [102, 103, 104, 105, 107, 108, 109]);
        let indel_masked = (100..114)
            .filter(|position| mask.masks_indel(*position))
            .collect::<Vec<_>>();
        assert_eq!(indel_masked, [102, 103, 104, 105, 106, 107, 108, 109]);

        let mut kept = Vec::new();
        let masked_bases = mask.add_unmasked_depth_range(102, 112, |start, end| {
            kept.push((start, end));
        });
        assert_eq!(kept, [(104, 106), (110, 112)]);
        assert_eq!(masked_bases, 6);
        let mut kept = Vec::new();
        let mut removed = Vec::new();
        mask.split_depth_range(
            100,
            108,
            |start, end| kept.push((start, end)),
            |start, end| removed.push((start, end)),
        );
        assert_eq!(kept, [(100, 102), (104, 106)]);
        assert_eq!(removed, [(102, 104), (106, 108)]);
        let mut kept = Vec::new();
        assert_eq!(
            mask.add_unmasked_deletion_range(108, 113, |start, end| kept.push((start, end))),
            2
        );
        assert_eq!(kept, [(110, 113)]);
    }

    #[test]
    fn mate_mask_leaves_a_reference_skip_of_the_earlier_mate_uncollapsed() {
        // Earlier mate: start 200, 3M4N3M -> bases at 200..203 and 207..210. Later mate: 203..208.
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        assert_eq!(
            collapser.mask_for_alignment(
                mate(b"spliced", 200, 210, PAIRED_FIRST, 203),
                [
                    op(CigarOpKind::Match, 3),
                    op(CigarOpKind::ReferenceSkip, 4),
                    op(CigarOpKind::Match, 3),
                ],
                |_| true,
            ),
            None
        );
        let mask = collapser
            .mask_for_alignment(
                mate(b"spliced", 203, 208, PAIRED_LAST_REVERSE, 200),
                [op(CigarOpKind::Match, 5)],
                |_| true,
            )
            .unwrap();
        let mut kept = Vec::new();
        let masked_bases = mask.add_unmasked_depth_range(203, 208, |start, end| {
            kept.push((start, end));
        });
        assert_eq!(kept, [(203, 207)]);
        assert_eq!(masked_bases, 1);
        assert!(!mask.masks_base(206) && mask.masks_base(207));
    }

    #[test]
    fn mates_starting_together_count_the_first_arrival_in_full() {
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        // The second-in-template record arrives first; nothing may be masked without proof that
        // its mate was counted.
        assert_eq!(
            collapser.mask_for_alignment(
                mate(b"same", 300, 304, PAIRED_LAST_REVERSE, 300),
                [op(CigarOpKind::Match, 4)],
                |_| true,
            ),
            None
        );
        let mask = collapser
            .mask_for_alignment(
                mate(b"same", 300, 306, PAIRED_FIRST, 300),
                [op(CigarOpKind::Match, 6)],
                |_| true,
            )
            .unwrap();
        let mut kept = Vec::new();
        assert_eq!(
            mask.add_unmasked_depth_range(300, 306, |start, end| kept.push((start, end))),
            4
        );
        assert_eq!(kept, [(304, 306)]);
    }

    #[test]
    fn pending_overlaps_drain_when_the_later_mate_never_arrives() {
        // Every earlier mate overlaps a later mate that is never offered (filtered out). In
        // coordinate order those entries are provably dead once the scan passes the mate start.
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        let total = MAX_PENDING_PAIR_OVERLAPS as u64 * 2 + 10;
        for index in 0..total {
            let name = format!("orphan-{index}");
            let start = index * 10;
            assert_eq!(
                collapser.mask_for_alignment(
                    mate(name.as_bytes(), start, start + 8, PAIRED_FIRST, start + 4),
                    [op(CigarOpKind::Match, 8)],
                    |_| true,
                ),
                None
            );
        }
        assert!(collapser.complete());
        assert!(collapser.pending_len() <= 2 * PENDING_SWEEP_FLOOR);

        // A live pair is still collapsed after the sweeps.
        let start = total * 10;
        collapser.mask_for_alignment(
            mate(b"live", start, start + 8, PAIRED_FIRST, start + 4),
            [op(CigarOpKind::Match, 8)],
            |_| true,
        );
        let mask = collapser
            .mask_for_alignment(
                mate(b"live", start + 4, start + 12, PAIRED_LAST_REVERSE, start),
                [op(CigarOpKind::Match, 8)],
                |_| true,
            )
            .unwrap();
        assert!(mask.masks_depth(start + 4) && mask.masks_depth(start + 7));
        assert!(!mask.masks_depth(start + 8));
    }

    #[test]
    fn legacy_single_range_mask_also_drains_stale_entries() {
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        let contig = ContigId::new(0);
        let first = AlignmentFlags::from_bits(PAIRED_FIRST);
        let last = AlignmentFlags::from_bits(PAIRED_LAST_REVERSE);
        for index in 0..MAX_PENDING_PAIR_OVERLAPS as u64 + 10 {
            let name = format!("orphan-{index}");
            let start = index * 10;
            assert_eq!(
                collapser.mask_for(
                    name.as_bytes(),
                    contig,
                    start,
                    start + 8,
                    first,
                    Some(contig),
                    Some(start + 4),
                ),
                None
            );
        }
        assert!(collapser.complete());
        let start = 5_000_000;
        collapser.mask_for(
            b"live",
            contig,
            start,
            start + 8,
            first,
            Some(contig),
            Some(start + 4),
        );
        assert_eq!(
            collapser.mask_for(
                b"live",
                contig,
                start + 4,
                start + 12,
                last,
                Some(contig),
                Some(start),
            ),
            Some(OverlapMask {
                start: start + 4,
                end: start + 8,
            })
        );
    }

    #[test]
    fn unordered_input_never_passes_off_a_sweep_as_complete() {
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        for index in 0..PENDING_SWEEP_FLOOR as u64 + 2 {
            let name = format!("orphan-{index}");
            let start = 1_000 + index * 10;
            collapser.mask_for_alignment(
                mate(name.as_bytes(), start, start + 8, PAIRED_FIRST, start + 4),
                [op(CigarOpKind::Match, 8)],
                |_| true,
            );
        }
        assert!(collapser.complete());
        // A record behind the scan position proves the input was not coordinate ordered, so
        // the entries already swept may have been needed.
        collapser.mask_for_alignment(
            mate(b"behind", 10, 18, PAIRED_FIRST, 14),
            [op(CigarOpKind::Match, 8)],
            |_| true,
        );
        assert!(!collapser.complete());
    }

    #[test]
    fn unordered_input_without_a_sweep_keeps_pending_overlaps() {
        let mut collapser = OverlapCollapser::new(OverlappingMatePolicy::CountOnce);
        collapser.mask_for_alignment(
            mate(b"late-left", 500, 510, PAIRED_FIRST, 505),
            [op(CigarOpKind::Match, 10)],
            |_| true,
        );
        collapser.mask_for_alignment(
            mate(b"other", 100, 110, PAIRED_FIRST, 400),
            [op(CigarOpKind::Match, 10)],
            |_| true,
        );
        let mask = collapser
            .mask_for_alignment(
                mate(b"late-left", 505, 515, PAIRED_LAST_REVERSE, 500),
                [op(CigarOpKind::Match, 10)],
                |_| true,
            )
            .unwrap();
        assert!(mask.masks_depth(505) && mask.masks_depth(509) && !mask.masks_depth(510));
        assert!(collapser.complete());
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
