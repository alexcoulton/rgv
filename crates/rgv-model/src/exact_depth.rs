use std::mem::size_of;

use rgv_core::GenomicInterval;

use crate::PileupBin;

/// One maximal genomic span with constant exact aligned-base depth.
///
/// Reference skips and deletions do not contribute aligned-base depth. Mapping-quality fields
/// describe the same records that establish `depth` over this entire half-open span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactDepthRun {
    pub start: u64,
    pub end: u64,
    pub depth: u32,
    pub forward_depth: u32,
    pub reverse_depth: u32,
    pub mapping_quality_sum: u64,
    pub mapping_quality_observations: u32,
}

/// Exact viewport-independent depth for one genomic ownership tile.
///
/// Runs retain only non-zero spans. A compact segment tree provides exact maxima for arbitrary
/// contained or intersecting ranges without rescanning all runs or source records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactDepthTile {
    interval: GenomicInterval,
    runs: Vec<ExactDepthRun>,
    maximum_tree: Vec<u32>,
    leaf_base: usize,
}

impl ExactDepthTile {
    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub fn runs(&self) -> &[ExactDepthRun] {
        &self.runs
    }

    /// Returns the exact maximum depth in the intersection with this tile.
    ///
    /// `None` means the requested interval is on another contig or does not intersect the tile;
    /// `Some(0)` means it intersects but contains no aligned base.
    #[must_use]
    pub fn maximum_depth(&self, interval: GenomicInterval) -> Option<u32> {
        if !self.interval.overlaps(interval) {
            return None;
        }
        let start = self.interval.start().max(interval.start());
        let end = self.interval.end().min(interval.end());
        let first = self.runs.partition_point(|run| run.end <= start);
        let last = self.runs.partition_point(|run| run.start < end);
        Some(self.maximum_over_run_indices(first, last))
    }

    /// Projects the exact runs into arbitrary display bins using the current maximum-in-bin
    /// semantics. This is a derived view; the tile itself remains camera independent.
    #[must_use]
    pub fn project_bins(&self, interval: GenomicInterval, bins: usize) -> Vec<PileupBin> {
        let mut output = vec![PileupBin::default(); bins];
        if bins == 0 || !self.interval.overlaps(interval) {
            return output;
        }
        for run in self
            .runs
            .iter()
            .filter(|run| run.start < interval.end() && run.end > interval.start())
        {
            let Some((first, last)) = bin_range(interval, bins, run.start, run.end) else {
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
        output
    }

    /// Heap and object capacity retained exclusively by this tile.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.owned_heap_bytes())
    }

    /// Heap capacity owned by the tile, excluding the inline [`ExactDepthTile`] value.
    #[must_use]
    pub fn owned_heap_bytes(&self) -> usize {
        self.runs
            .capacity()
            .saturating_mul(size_of::<ExactDepthRun>())
            .saturating_add(
                self.maximum_tree
                    .capacity()
                    .saturating_mul(size_of::<u32>()),
            )
    }

    fn maximum_over_run_indices(&self, mut start: usize, mut end: usize) -> u32 {
        if start >= end || self.maximum_tree.is_empty() {
            return 0;
        }
        start = start.saturating_add(self.leaf_base);
        end = end.saturating_add(self.leaf_base);
        let mut maximum = 0_u32;
        while start < end {
            if !start.is_multiple_of(2) {
                maximum = maximum.max(self.maximum_tree[start]);
                start = start.saturating_add(1);
            }
            if !end.is_multiple_of(2) {
                end = end.saturating_sub(1);
                maximum = maximum.max(self.maximum_tree[end]);
            }
            start /= 2;
            end /= 2;
        }
        maximum
    }
}

/// Private-work builder for an [`ExactDepthTile`].
///
/// Workers may abandon this value freely; only `finish` creates publishable immutable data.
#[derive(Debug)]
pub struct ExactDepthTileBuilder {
    interval: GenomicInterval,
    events: Vec<DepthEvent>,
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

impl ExactDepthTileBuilder {
    #[must_use]
    pub const fn new(interval: GenomicInterval) -> Self {
        Self {
            interval,
            events: Vec::new(),
        }
    }

    pub fn add_aligned_segment(
        &mut self,
        start: u64,
        end: u64,
        reverse: bool,
        mapping_quality: Option<u8>,
    ) {
        let start = start.max(self.interval.start());
        let end = end.min(self.interval.end());
        if start >= end {
            return;
        }
        let forward = i64::from(!reverse);
        let reverse = i64::from(reverse);
        let mapping_quality_sum = mapping_quality.map_or(0, i64::from);
        let mapping_quality_count = i64::from(mapping_quality.is_some());
        self.events.push(DepthEvent {
            position: start,
            depth: 1,
            forward,
            reverse,
            mapping_quality_sum,
            mapping_quality_count,
        });
        self.events.push(DepthEvent {
            position: end,
            depth: -1,
            forward: -forward,
            reverse: -reverse,
            mapping_quality_sum: -mapping_quality_sum,
            mapping_quality_count: -mapping_quality_count,
        });
    }

    #[must_use]
    pub fn finish(mut self) -> ExactDepthTile {
        self.events.sort_unstable_by_key(|event| event.position);
        let mut runs = Vec::<ExactDepthRun>::new();
        let mut depth = 0_i64;
        let mut forward = 0_i64;
        let mut reverse = 0_i64;
        let mut mapping_quality_sum = 0_i64;
        let mut mapping_quality_count = 0_i64;
        let mut cursor = 0;
        while cursor < self.events.len() {
            let position = self.events[cursor].position;
            while cursor < self.events.len() && self.events[cursor].position == position {
                let event = self.events[cursor];
                depth = depth.saturating_add(event.depth);
                forward = forward.saturating_add(event.forward);
                reverse = reverse.saturating_add(event.reverse);
                mapping_quality_sum = mapping_quality_sum.saturating_add(event.mapping_quality_sum);
                mapping_quality_count =
                    mapping_quality_count.saturating_add(event.mapping_quality_count);
                cursor = cursor.saturating_add(1);
            }
            let Some(next_position) = self.events.get(cursor).map(|event| event.position) else {
                continue;
            };
            if depth <= 0 || position >= next_position {
                continue;
            }
            let run = ExactDepthRun {
                start: position,
                end: next_position,
                depth: nonnegative_u32(depth),
                forward_depth: nonnegative_u32(forward),
                reverse_depth: nonnegative_u32(reverse),
                mapping_quality_sum: u64::try_from(mapping_quality_sum.max(0)).unwrap_or(u64::MAX),
                mapping_quality_observations: nonnegative_u32(mapping_quality_count),
            };
            if let Some(previous) = runs.last_mut()
                && previous.end == run.start
                && previous.depth == run.depth
                && previous.forward_depth == run.forward_depth
                && previous.reverse_depth == run.reverse_depth
                && previous.mapping_quality_sum == run.mapping_quality_sum
                && previous.mapping_quality_observations == run.mapping_quality_observations
            {
                previous.end = run.end;
            } else {
                runs.push(run);
            }
        }
        runs.shrink_to_fit();
        let (maximum_tree, leaf_base) = build_maximum_tree(&runs);
        ExactDepthTile {
            interval: self.interval,
            runs,
            maximum_tree,
            leaf_base,
        }
    }
}

fn build_maximum_tree(runs: &[ExactDepthRun]) -> (Vec<u32>, usize) {
    if runs.is_empty() {
        return (Vec::new(), 0);
    }
    let leaf_base = runs.len().next_power_of_two();
    let mut tree = vec![0_u32; leaf_base.saturating_mul(2)];
    for (index, run) in runs.iter().enumerate() {
        tree[leaf_base + index] = run.depth;
    }
    for index in (1..leaf_base).rev() {
        tree[index] = tree[index.saturating_mul(2)].max(tree[index.saturating_mul(2) + 1]);
    }
    (tree, leaf_base)
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

fn nonnegative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;

    use super::*;

    fn interval(start: u64, end: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), start, end).unwrap()
    }

    #[test]
    fn exact_runs_merge_constant_spans_and_preserve_strands() {
        let mut builder = ExactDepthTileBuilder::new(interval(100, 200));
        builder.add_aligned_segment(90, 150, false, Some(60));
        builder.add_aligned_segment(125, 175, true, Some(20));
        let tile = builder.finish();

        assert_eq!(
            tile.runs(),
            [
                ExactDepthRun {
                    start: 100,
                    end: 125,
                    depth: 1,
                    forward_depth: 1,
                    reverse_depth: 0,
                    mapping_quality_sum: 60,
                    mapping_quality_observations: 1,
                },
                ExactDepthRun {
                    start: 125,
                    end: 150,
                    depth: 2,
                    forward_depth: 1,
                    reverse_depth: 1,
                    mapping_quality_sum: 80,
                    mapping_quality_observations: 2,
                },
                ExactDepthRun {
                    start: 150,
                    end: 175,
                    depth: 1,
                    forward_depth: 0,
                    reverse_depth: 1,
                    mapping_quality_sum: 20,
                    mapping_quality_observations: 1,
                },
            ]
        );
    }

    #[test]
    fn range_maximum_is_exact_for_partial_runs_and_gaps() {
        let mut builder = ExactDepthTileBuilder::new(interval(0, 100));
        builder.add_aligned_segment(0, 10, false, None);
        builder.add_aligned_segment(30, 60, false, None);
        builder.add_aligned_segment(40, 50, true, None);
        let tile = builder.finish();

        assert_eq!(tile.maximum_depth(interval(5, 35)), Some(1));
        assert_eq!(tile.maximum_depth(interval(40, 45)), Some(2));
        assert_eq!(tile.maximum_depth(interval(15, 20)), Some(0));
        assert_eq!(tile.maximum_depth(interval(100, 110)), None);
        assert_eq!(
            tile.maximum_depth(GenomicInterval::new(ContigId::new(1), 0, 10).unwrap()),
            None
        );
    }

    #[test]
    fn arbitrary_bin_projection_uses_exact_maximum_depth() {
        let mut builder = ExactDepthTileBuilder::new(interval(0, 100));
        builder.add_aligned_segment(0, 100, false, Some(30));
        builder.add_aligned_segment(49, 51, true, Some(50));
        let bins = builder.finish().project_bins(interval(0, 100), 10);

        assert_eq!(
            bins.iter().map(|bin| bin.depth).collect::<Vec<_>>(),
            [1, 1, 1, 1, 2, 2, 1, 1, 1, 1]
        );
        assert_eq!(bins[4].mapping_quality_sum, 80);
        assert_eq!(bins[4].mapping_quality_observations, 2);
    }
}
