use std::collections::BTreeMap;

use rgv_core::GenomicInterval;

/// Hard bound on distinct splice junctions retained by one regional query.
pub const MAX_JUNCTIONS: usize = 100_000;
/// Hard bound on splice operations retained from one alignment record.
pub const MAX_JUNCTIONS_PER_RECORD: usize = 4_096;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JunctionStrand {
    Positive,
    Negative,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JunctionStrandInput {
    Tagged(JunctionStrand),
    Missing,
    Invalid,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JunctionEvidence {
    pub start: u64,
    pub end: u64,
    pub depth: u32,
    pub minimum_left_flank: u32,
    pub minimum_right_flank: u32,
    pub maximum_left_flank: u32,
    pub maximum_right_flank: u32,
    pub positive_depth: u32,
    pub negative_depth: u32,
    pub unknown_depth: u32,
    pub tagged_positive_depth: u32,
    pub tagged_negative_depth: u32,
    pub tagged_unknown_depth: u32,
    pub fallback_positive_depth: u32,
    pub fallback_negative_depth: u32,
    pub invalid_strand_depth: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JunctionProvenance {
    pub eligible_records: usize,
    pub spliced_records: usize,
    pub supporting_spliced_records: usize,
    pub junction_observations_examined: usize,
    pub junction_observations_retained: usize,
    pub junction_observations_filtered_by_flank: usize,
    pub minimum_flank: u32,
    /// Whole-file mapped alignment count from index metadata, when available.
    pub normalization_mapped_records: Option<u64>,
    pub xs_tagged_observations: usize,
    pub alignment_strand_fallback_observations: usize,
    pub invalid_xs_observations: usize,
    pub dropped_record_junction_observations: usize,
    pub dropped_distinct_junctions: usize,
    pub all_eligible_records: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JunctionSummary {
    pub junctions: Vec<JunctionEvidence>,
    pub provenance: JunctionProvenance,
}

impl JunctionSummary {
    pub fn estimated_bytes(&self) -> usize {
        self.junctions
            .capacity()
            .saturating_mul(std::mem::size_of::<JunctionEvidence>())
    }

    pub fn maximum_depth(&self) -> u32 {
        self.junctions
            .iter()
            .map(|junction| junction.depth)
            .max()
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JunctionObservation {
    pub start: u64,
    pub end: u64,
    pub left_flank: u32,
    pub right_flank: u32,
}

/// Collects splice observations while a CIGAR is decoded.
///
/// A flank is the number of aligned `M`, `=` or `X` bases in the adjacent exon block. Insertions,
/// deletions and clipping do not add support, while another reference skip starts the next block.
pub struct RecordJunctionCollector {
    observations: Vec<JunctionObservation>,
    aligned_flank: u32,
    pending_observation: Option<usize>,
    dropped_observations: usize,
    unbounded: bool,
}

impl Default for RecordJunctionCollector {
    fn default() -> Self {
        Self {
            observations: Vec::new(),
            aligned_flank: 0,
            pending_observation: None,
            dropped_observations: 0,
            unbounded: false,
        }
    }
}

impl RecordJunctionCollector {
    /// Exact collector used after a resource reservation has bounded the source CIGAR work.
    #[must_use]
    pub fn exact() -> Self {
        Self {
            unbounded: true,
            ..Self::default()
        }
    }

    pub fn add_aligned(&mut self, length: u32) {
        self.aligned_flank = self.aligned_flank.saturating_add(length);
    }

    pub fn add_skip(&mut self, start: u64, end: u64) {
        if let Some(index) = self.pending_observation {
            self.observations[index].right_flank = self.aligned_flank;
        }
        if self.unbounded || self.observations.len() < MAX_JUNCTIONS_PER_RECORD {
            self.observations.push(JunctionObservation {
                start,
                end,
                left_flank: self.aligned_flank,
                right_flank: 0,
            });
            self.pending_observation = Some(self.observations.len() - 1);
        } else {
            self.pending_observation = None;
            self.dropped_observations = self.dropped_observations.saturating_add(1);
        }
        self.aligned_flank = 0;
    }

    pub fn finish(mut self) -> (Vec<JunctionObservation>, usize) {
        if let Some(index) = self.pending_observation {
            self.observations[index].right_flank = self.aligned_flank;
        }
        (self.observations, self.dropped_observations)
    }
}

#[derive(Default)]
pub struct JunctionSummaryBuilder {
    interval: Option<GenomicInterval>,
    minimum_flank: u32,
    normalization_mapped_records: Option<u64>,
    junctions: BTreeMap<(u64, u64), JunctionEvidence>,
    spliced_records: usize,
    supporting_spliced_records: usize,
    junction_observations_examined: usize,
    junction_observations_retained: usize,
    junction_observations_filtered_by_flank: usize,
    xs_tagged_observations: usize,
    alignment_strand_fallback_observations: usize,
    invalid_xs_observations: usize,
    dropped_record_junction_observations: usize,
    dropped_distinct_junctions: usize,
    unbounded: bool,
}

impl JunctionSummaryBuilder {
    pub fn new(
        interval: GenomicInterval,
        minimum_flank: u32,
        normalization_mapped_records: Option<u64>,
    ) -> Self {
        Self {
            interval: Some(interval),
            minimum_flank,
            normalization_mapped_records,
            ..Self::default()
        }
    }

    /// Exact aggregator used after the caller has reserved capacity for all candidate events.
    #[must_use]
    pub fn exact(
        interval: GenomicInterval,
        minimum_flank: u32,
        normalization_mapped_records: Option<u64>,
    ) -> Self {
        Self {
            interval: Some(interval),
            minimum_flank,
            normalization_mapped_records,
            unbounded: true,
            ..Self::default()
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn add_record_junctions(
        &mut self,
        junctions: &[JunctionObservation],
        dropped_record_junctions: usize,
        alignment_reverse: bool,
        strand_input: JunctionStrandInput,
    ) {
        self.dropped_record_junction_observations = self
            .dropped_record_junction_observations
            .saturating_add(dropped_record_junctions);
        if junctions.is_empty() {
            return;
        }
        self.spliced_records = self.spliced_records.saturating_add(1);
        let mut record_supports = false;
        for observation in junctions {
            if observation.start >= observation.end
                || !self.interval.is_some_and(|interval| {
                    observation.start < interval.end() && observation.end > interval.start()
                })
            {
                continue;
            }
            self.junction_observations_examined =
                self.junction_observations_examined.saturating_add(1);
            if observation.left_flank < self.minimum_flank
                || observation.right_flank < self.minimum_flank
            {
                self.junction_observations_filtered_by_flank = self
                    .junction_observations_filtered_by_flank
                    .saturating_add(1);
                continue;
            }
            if !self.unbounded
                && !self
                    .junctions
                    .contains_key(&(observation.start, observation.end))
                && self.junctions.len() >= MAX_JUNCTIONS
            {
                self.dropped_distinct_junctions = self.dropped_distinct_junctions.saturating_add(1);
                continue;
            }
            let strand = match strand_input {
                JunctionStrandInput::Tagged(strand) => {
                    self.xs_tagged_observations = self.xs_tagged_observations.saturating_add(1);
                    strand
                }
                JunctionStrandInput::Missing => {
                    self.alignment_strand_fallback_observations = self
                        .alignment_strand_fallback_observations
                        .saturating_add(1);
                    if alignment_reverse {
                        JunctionStrand::Negative
                    } else {
                        JunctionStrand::Positive
                    }
                }
                JunctionStrandInput::Invalid => {
                    self.invalid_xs_observations = self.invalid_xs_observations.saturating_add(1);
                    JunctionStrand::Unknown
                }
            };
            let evidence = self
                .junctions
                .entry((observation.start, observation.end))
                .or_insert(JunctionEvidence {
                    start: observation.start,
                    end: observation.end,
                    minimum_left_flank: observation.left_flank,
                    minimum_right_flank: observation.right_flank,
                    maximum_left_flank: observation.left_flank,
                    maximum_right_flank: observation.right_flank,
                    ..JunctionEvidence::default()
                });
            evidence.depth = evidence.depth.saturating_add(1);
            evidence.minimum_left_flank = evidence.minimum_left_flank.min(observation.left_flank);
            evidence.minimum_right_flank =
                evidence.minimum_right_flank.min(observation.right_flank);
            evidence.maximum_left_flank = evidence.maximum_left_flank.max(observation.left_flank);
            evidence.maximum_right_flank =
                evidence.maximum_right_flank.max(observation.right_flank);
            let strand_depth = match strand {
                JunctionStrand::Positive => &mut evidence.positive_depth,
                JunctionStrand::Negative => &mut evidence.negative_depth,
                JunctionStrand::Unknown => &mut evidence.unknown_depth,
            };
            *strand_depth = strand_depth.saturating_add(1);
            let source_depth = match strand_input {
                JunctionStrandInput::Tagged(JunctionStrand::Positive) => {
                    &mut evidence.tagged_positive_depth
                }
                JunctionStrandInput::Tagged(JunctionStrand::Negative) => {
                    &mut evidence.tagged_negative_depth
                }
                JunctionStrandInput::Tagged(JunctionStrand::Unknown) => {
                    &mut evidence.tagged_unknown_depth
                }
                JunctionStrandInput::Missing if alignment_reverse => {
                    &mut evidence.fallback_negative_depth
                }
                JunctionStrandInput::Missing => &mut evidence.fallback_positive_depth,
                JunctionStrandInput::Invalid => &mut evidence.invalid_strand_depth,
            };
            *source_depth = source_depth.saturating_add(1);
            self.junction_observations_retained =
                self.junction_observations_retained.saturating_add(1);
            record_supports = true;
        }
        if record_supports {
            self.supporting_spliced_records = self.supporting_spliced_records.saturating_add(1);
        }
    }

    pub fn finish(self, eligible_records: usize, all_eligible_records: bool) -> JunctionSummary {
        JunctionSummary {
            junctions: self.junctions.into_values().collect(),
            provenance: JunctionProvenance {
                eligible_records,
                spliced_records: self.spliced_records,
                supporting_spliced_records: self.supporting_spliced_records,
                junction_observations_examined: self.junction_observations_examined,
                junction_observations_retained: self.junction_observations_retained,
                junction_observations_filtered_by_flank: self
                    .junction_observations_filtered_by_flank,
                minimum_flank: self.minimum_flank,
                normalization_mapped_records: self.normalization_mapped_records,
                xs_tagged_observations: self.xs_tagged_observations,
                alignment_strand_fallback_observations: self.alignment_strand_fallback_observations,
                invalid_xs_observations: self.invalid_xs_observations,
                dropped_record_junction_observations: self.dropped_record_junction_observations,
                dropped_distinct_junctions: self.dropped_distinct_junctions,
                all_eligible_records,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;

    fn observation(start: u64, end: u64, left_flank: u32, right_flank: u32) -> JunctionObservation {
        JunctionObservation {
            start,
            end,
            left_flank,
            right_flank,
        }
    }

    #[test]
    fn cigar_collector_assigns_adjacent_exon_flanks() {
        let mut collector = RecordJunctionCollector::default();
        collector.add_aligned(10);
        collector.add_skip(110, 210);
        collector.add_aligned(20);
        collector.add_skip(230, 330);
        collector.add_aligned(30);
        let (observations, dropped) = collector.finish();
        assert_eq!(dropped, 0);
        assert_eq!(
            observations,
            vec![observation(110, 210, 10, 20), observation(230, 330, 20, 30)]
        );
    }

    #[test]
    fn aggregation_counts_junction_depth_and_declared_strand_sources() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 500).unwrap();
        let mut builder = JunctionSummaryBuilder::new(interval, 0, Some(1_000_000));
        builder.add_record_junctions(
            &[observation(150, 250, 12, 18), observation(300, 400, 20, 25)],
            0,
            false,
            JunctionStrandInput::Tagged(JunctionStrand::Negative),
        );
        builder.add_record_junctions(
            &[observation(150, 250, 8, 30)],
            0,
            false,
            JunctionStrandInput::Missing,
        );
        builder.add_record_junctions(
            &[observation(150, 250, 40, 6)],
            0,
            true,
            JunctionStrandInput::Invalid,
        );
        let summary = builder.finish(10, true);
        assert_eq!(summary.junctions.len(), 2);
        assert_eq!(summary.junctions[0].depth, 3);
        assert_eq!(summary.junctions[0].positive_depth, 1);
        assert_eq!(summary.junctions[0].negative_depth, 1);
        assert_eq!(summary.junctions[0].unknown_depth, 1);
        assert_eq!(summary.junctions[0].minimum_left_flank, 8);
        assert_eq!(summary.junctions[0].minimum_right_flank, 6);
        assert_eq!(summary.junctions[0].maximum_left_flank, 40);
        assert_eq!(summary.junctions[0].maximum_right_flank, 30);
        assert_eq!(summary.junctions[0].tagged_negative_depth, 1);
        assert_eq!(summary.junctions[0].fallback_positive_depth, 1);
        assert_eq!(summary.junctions[0].invalid_strand_depth, 1);
        assert_eq!(summary.provenance.spliced_records, 3);
        assert_eq!(summary.provenance.supporting_spliced_records, 3);
        assert_eq!(summary.provenance.junction_observations_examined, 4);
        assert_eq!(summary.provenance.junction_observations_retained, 4);
        assert_eq!(
            summary.provenance.normalization_mapped_records,
            Some(1_000_000)
        );
        assert_eq!(summary.provenance.xs_tagged_observations, 2);
        assert_eq!(summary.provenance.alignment_strand_fallback_observations, 1);
        assert_eq!(summary.provenance.invalid_xs_observations, 1);
        assert!(summary.provenance.all_eligible_records);
    }

    #[test]
    fn out_of_query_junctions_are_not_retained() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
        let mut builder = JunctionSummaryBuilder::new(interval, 0, None);
        builder.add_record_junctions(
            &[observation(0, 50, 10, 10), observation(250, 300, 10, 10)],
            0,
            false,
            JunctionStrandInput::Missing,
        );
        assert!(builder.finish(1, true).junctions.is_empty());
    }

    #[test]
    fn minimum_flank_filters_observations_before_exact_aggregation() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 500).unwrap();
        let mut builder = JunctionSummaryBuilder::new(interval, 10, None);
        builder.add_record_junctions(
            &[observation(150, 250, 9, 100)],
            0,
            false,
            JunctionStrandInput::Missing,
        );
        builder.add_record_junctions(
            &[observation(150, 250, 10, 10)],
            0,
            true,
            JunctionStrandInput::Tagged(JunctionStrand::Negative),
        );
        let summary = builder.finish(2, true);
        assert_eq!(summary.junctions.len(), 1);
        assert_eq!(summary.junctions[0].depth, 1);
        assert_eq!(summary.junctions[0].tagged_negative_depth, 1);
        assert_eq!(summary.provenance.spliced_records, 2);
        assert_eq!(summary.provenance.supporting_spliced_records, 1);
        assert_eq!(summary.provenance.junction_observations_examined, 2);
        assert_eq!(summary.provenance.junction_observations_retained, 1);
        assert_eq!(
            summary.provenance.junction_observations_filtered_by_flank,
            1
        );
        assert_eq!(summary.provenance.minimum_flank, 10);
    }
}
