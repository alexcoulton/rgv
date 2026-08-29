use std::collections::BinaryHeap;

use rgv_core::{ContigId, GenomicInterval};

use crate::{AlignmentBatch, AlignmentFlags, AlignmentRecord};

pub const MAX_TEMPLATE_LENGTH_SAMPLES: usize = 4_096;
pub const MIN_TEMPLATE_LENGTH_SAMPLES: usize = 100;

/// Stable identity shared by records from the same SAM template.
///
/// SAM defines records with the same QNAME as belonging to one template. RGV retains the name in
/// its packed model and hashes it with a fixed algorithm so identity does not depend on process
/// randomization, record order, viewport, or individual-read admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TemplateId(u64);

impl TemplateId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOrder {
    First,
    Last,
    Unknown,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairOrientation {
    Inward,
    Outward,
    SameStrand,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairClassification {
    Unpaired,
    ReadUnmapped,
    MateUnmapped,
    MateMissing,
    InterContig,
    DiscordantOrientation,
    DiscordantTemplateLengthShort,
    DiscordantTemplateLengthLong,
    Proper,
    Concordant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MateVisibility {
    Missing,
    Unmapped,
    OtherContig,
    Visible,
    OutOfView,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TemplateLengthBounds {
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
}

impl TemplateLengthBounds {
    pub const fn contains(self, length: u64) -> bool {
        if let Some(minimum) = self.minimum
            && length < minimum
        {
            return false;
        }
        if let Some(maximum) = self.maximum
            && length > maximum
        {
            return false;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairEvidence {
    pub template_id: TemplateId,
    pub read_order: ReadOrder,
    pub orientation: PairOrientation,
    pub classification: PairClassification,
    pub mate_visibility: MateVisibility,
    pub absolute_template_length: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PairSummary {
    pub eligible_proper_templates: usize,
    pub sampled_templates: usize,
    pub median_template_length: Option<u64>,
    pub inferred_bounds: Option<TemplateLengthBounds>,
    pub all_eligible_records: bool,
}

impl PairSummary {
    pub const fn estimated_bytes(self) -> usize {
        std::mem::size_of::<Self>()
    }
}

pub(crate) struct PairSummaryBuilder {
    samples: BinaryHeap<(u64, u64)>,
    eligible_proper_templates: usize,
}

impl PairSummaryBuilder {
    pub(crate) fn new() -> Self {
        Self {
            samples: BinaryHeap::with_capacity(MAX_TEMPLATE_LENGTH_SAMPLES),
            eligible_proper_templates: 0,
        }
    }

    pub(crate) fn add_fields(
        &mut self,
        read_name: &[u8],
        contig: ContigId,
        flags: AlignmentFlags,
        mate_contig: Option<ContigId>,
        template_length: i64,
    ) {
        if read_name.is_empty()
            || !flags.is_paired()
            || !flags.is_proper_pair()
            || flags.is_unmapped()
            || flags.is_mate_unmapped()
            || flags.is_secondary()
            || flags.is_supplementary()
            || !flags.is_first_in_template()
            || mate_contig != Some(contig)
            || template_length == 0
        {
            return;
        }
        self.eligible_proper_templates = self.eligible_proper_templates.saturating_add(1);
        let length = template_length.unsigned_abs();
        let score = template_id(read_name).get();
        if self.samples.len() < MAX_TEMPLATE_LENGTH_SAMPLES {
            self.samples.push((score, length));
        } else if self.samples.peek().is_some_and(|&(worst, _)| score < worst) {
            self.samples.pop();
            self.samples.push((score, length));
        }
    }

    pub(crate) fn add_precomputed_eligible_count(&mut self, count: usize) {
        self.eligible_proper_templates = self.eligible_proper_templates.saturating_add(count);
    }

    pub(crate) fn add_precomputed_sample(&mut self, score: u64, length: u64) {
        if self.samples.len() < MAX_TEMPLATE_LENGTH_SAMPLES {
            self.samples.push((score, length));
        } else if self.samples.peek().is_some_and(|&(worst, _)| score < worst) {
            self.samples.pop();
            self.samples.push((score, length));
        }
    }

    pub(crate) fn finish(self, all_eligible_records: bool) -> PairSummary {
        let mut lengths = self
            .samples
            .into_iter()
            .map(|(_, length)| length)
            .collect::<Vec<_>>();
        lengths.sort_unstable();
        let sampled_templates = lengths.len();
        let median_template_length = quantile(&lengths, 1, 2);
        let inferred_bounds = (sampled_templates >= MIN_TEMPLATE_LENGTH_SAMPLES).then(|| {
            let lower_quartile = quantile(&lengths, 1, 4).unwrap_or_default();
            let upper_quartile = quantile(&lengths, 3, 4).unwrap_or(lower_quartile);
            let interquartile_range = upper_quartile.saturating_sub(lower_quartile);
            TemplateLengthBounds {
                minimum: Some(lower_quartile.saturating_sub(interquartile_range.saturating_mul(3))),
                maximum: Some(upper_quartile.saturating_add(interquartile_range.saturating_mul(3))),
            }
        });
        PairSummary {
            eligible_proper_templates: self.eligible_proper_templates,
            sampled_templates,
            median_template_length,
            inferred_bounds,
            all_eligible_records,
        }
    }
}

fn quantile(sorted: &[u64], numerator: usize, denominator: usize) -> Option<u64> {
    if sorted.is_empty() || denominator == 0 {
        return None;
    }
    let index = sorted.len().saturating_sub(1).saturating_mul(numerator) / denominator;
    sorted.get(index).copied()
}

pub fn template_id(read_name: &[u8]) -> TemplateId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in read_name {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    TemplateId(hash)
}

pub fn pair_evidence(
    record: &AlignmentRecord,
    visible_interval: GenomicInterval,
    bounds: TemplateLengthBounds,
) -> PairEvidence {
    classify_pair(
        &record.read_name,
        record.contig,
        record.start,
        record.flags,
        record.mate_contig,
        record.mate_start,
        record.template_length,
        visible_interval,
        bounds,
    )
}

pub fn pair_evidence_at(
    batch: &AlignmentBatch,
    index: usize,
    visible_interval: GenomicInterval,
    bounds: TemplateLengthBounds,
) -> Option<PairEvidence> {
    Some(classify_pair(
        batch.read_name(index)?,
        batch.contig(index)?,
        batch.start(index)?,
        batch.flags(index)?,
        batch.mate_contig(index)?,
        batch.mate_start(index)?,
        batch.template_length(index)?,
        visible_interval,
        bounds,
    ))
}

#[allow(clippy::too_many_arguments)]
fn classify_pair(
    read_name: &[u8],
    contig: rgv_core::ContigId,
    start: u64,
    flags: AlignmentFlags,
    mate_contig: Option<rgv_core::ContigId>,
    mate_start: Option<u64>,
    template_length: i64,
    visible_interval: GenomicInterval,
    bounds: TemplateLengthBounds,
) -> PairEvidence {
    let read_order = match (flags.is_first_in_template(), flags.is_last_in_template()) {
        (true, false) => ReadOrder::First,
        (false, true) => ReadOrder::Last,
        (false, false) => ReadOrder::Unknown,
        (true, true) => ReadOrder::Ambiguous,
    };
    let absolute_template_length = template_length.unsigned_abs();
    let orientation = pair_orientation(flags, start, mate_start);
    let mate_visibility = if !flags.is_paired() || mate_contig.is_none() || mate_start.is_none() {
        MateVisibility::Missing
    } else if flags.is_mate_unmapped() {
        MateVisibility::Unmapped
    } else if mate_contig != Some(visible_interval.contig()) {
        MateVisibility::OtherContig
    } else if mate_start.is_some_and(|position| visible_interval.contains(position)) {
        MateVisibility::Visible
    } else {
        MateVisibility::OutOfView
    };
    let classification = if !flags.is_paired() {
        PairClassification::Unpaired
    } else if flags.is_unmapped() {
        PairClassification::ReadUnmapped
    } else if flags.is_mate_unmapped() {
        PairClassification::MateUnmapped
    } else if mate_contig.is_none() || mate_start.is_none() {
        PairClassification::MateMissing
    } else if mate_contig != Some(contig) {
        PairClassification::InterContig
    } else if !matches!(orientation, PairOrientation::Inward) {
        PairClassification::DiscordantOrientation
    } else if bounds
        .minimum
        .is_some_and(|minimum| absolute_template_length < minimum)
    {
        PairClassification::DiscordantTemplateLengthShort
    } else if bounds
        .maximum
        .is_some_and(|maximum| absolute_template_length > maximum)
    {
        PairClassification::DiscordantTemplateLengthLong
    } else if flags.is_proper_pair() {
        PairClassification::Proper
    } else {
        PairClassification::Concordant
    };
    PairEvidence {
        template_id: template_id(read_name),
        read_order,
        orientation,
        classification,
        mate_visibility,
        absolute_template_length,
    }
}

fn pair_orientation(flags: AlignmentFlags, start: u64, mate_start: Option<u64>) -> PairOrientation {
    let Some(mate_start) = mate_start else {
        return PairOrientation::Unknown;
    };
    if flags.is_reverse() == flags.is_mate_reverse() {
        return PairOrientation::SameStrand;
    }
    let inward = if start <= mate_start {
        !flags.is_reverse() && flags.is_mate_reverse()
    } else {
        flags.is_reverse() && !flags.is_mate_reverse()
    };
    if inward {
        PairOrientation::Inward
    } else {
        PairOrientation::Outward
    }
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;
    use crate::{AlignmentRecord, SequenceData};

    fn record(flags: u16) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 150,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(300),
            template_length: 250,
            read_name: b"template-1".to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::Missing,
            auxiliary_fields: Vec::new(),
        }
    }

    fn visible() -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), 0, 500).unwrap()
    }

    #[test]
    fn stable_template_identity_depends_only_on_read_name() {
        let mut left = record(0x63);
        let mut right = record(0x93);
        right.start = 300;
        right.mate_start = Some(100);
        assert_eq!(
            pair_evidence(&left, visible(), TemplateLengthBounds::default()).template_id,
            pair_evidence(&right, visible(), TemplateLengthBounds::default()).template_id
        );
        left.read_name = b"different".to_vec();
        assert_ne!(template_id(&left.read_name), template_id(&right.read_name));
    }

    #[test]
    fn proper_inward_pair_retains_order_orientation_and_visibility() {
        let evidence = pair_evidence(&record(0x63), visible(), TemplateLengthBounds::default());
        assert_eq!(evidence.read_order, ReadOrder::First);
        assert_eq!(evidence.orientation, PairOrientation::Inward);
        assert_eq!(evidence.classification, PairClassification::Proper);
        assert_eq!(evidence.mate_visibility, MateVisibility::Visible);
        assert_eq!(evidence.absolute_template_length, 250);
    }

    #[test]
    fn explicit_bounds_override_a_proper_pair_flag() {
        let evidence = pair_evidence(
            &record(0x63),
            visible(),
            TemplateLengthBounds {
                minimum: None,
                maximum: Some(200),
            },
        );
        assert_eq!(
            evidence.classification,
            PairClassification::DiscordantTemplateLengthLong
        );
    }

    #[test]
    fn reports_orientation_intercontig_unmapped_missing_and_out_of_view_states() {
        let mut outward = record(0x73);
        assert_eq!(
            pair_evidence(&outward, visible(), TemplateLengthBounds::default()).classification,
            PairClassification::DiscordantOrientation
        );

        outward.mate_contig = Some(ContigId::new(1));
        let intercontig = pair_evidence(&outward, visible(), TemplateLengthBounds::default());
        assert_eq!(intercontig.classification, PairClassification::InterContig);
        assert_eq!(intercontig.mate_visibility, MateVisibility::OtherContig);

        let mate_unmapped =
            pair_evidence(&record(0x6b), visible(), TemplateLengthBounds::default());
        assert_eq!(
            mate_unmapped.classification,
            PairClassification::MateUnmapped
        );
        assert_eq!(mate_unmapped.mate_visibility, MateVisibility::Unmapped);

        let mut missing = record(0x61);
        missing.mate_start = None;
        assert_eq!(
            pair_evidence(&missing, visible(), TemplateLengthBounds::default()).classification,
            PairClassification::MateMissing
        );

        let narrow = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
        assert_eq!(
            pair_evidence(&record(0x63), narrow, TemplateLengthBounds::default()).mate_visibility,
            MateVisibility::OutOfView
        );
    }

    #[test]
    fn packed_batch_classification_matches_materialized_records() {
        let record = record(0x63);
        let expected = pair_evidence(&record, visible(), TemplateLengthBounds::default());
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        assert_eq!(
            pair_evidence_at(&batch, 0, visible(), TemplateLengthBounds::default()),
            Some(expected)
        );
    }

    #[test]
    fn pair_summary_is_bounded_and_infers_a_robust_envelope() {
        let mut builder = PairSummaryBuilder::new();
        for index in 0..(MAX_TEMPLATE_LENGTH_SAMPLES + 1_000) {
            let length = if index == 0 {
                50_000
            } else {
                300 + i64::try_from(index % 11).unwrap()
            };
            builder.add_fields(
                format!("pair-{index}").as_bytes(),
                ContigId::new(0),
                AlignmentFlags::from_bits(0x43),
                Some(ContigId::new(0)),
                length,
            );
        }
        let summary = builder.finish(true);
        assert_eq!(
            summary.eligible_proper_templates,
            MAX_TEMPLATE_LENGTH_SAMPLES + 1_000
        );
        assert_eq!(summary.sampled_templates, MAX_TEMPLATE_LENGTH_SAMPLES);
        assert!(summary.all_eligible_records);
        let bounds = summary.inferred_bounds.unwrap();
        assert!(bounds.minimum.unwrap() < 300);
        assert!(bounds.maximum.unwrap() < 1_000);
    }

    #[test]
    fn pair_summary_requires_enough_evidence_and_ignores_non_primary_records() {
        let mut builder = PairSummaryBuilder::new();
        builder.add_fields(
            b"secondary",
            ContigId::new(0),
            AlignmentFlags::from_bits(0x143),
            Some(ContigId::new(0)),
            300,
        );
        for index in 0..(MIN_TEMPLATE_LENGTH_SAMPLES - 1) {
            builder.add_fields(
                format!("pair-{index}").as_bytes(),
                ContigId::new(0),
                AlignmentFlags::from_bits(0x43),
                Some(ContigId::new(0)),
                300,
            );
        }
        let summary = builder.finish(false);
        assert_eq!(
            summary.eligible_proper_templates,
            MIN_TEMPLATE_LENGTH_SAMPLES - 1
        );
        assert!(summary.inferred_bounds.is_none());
        assert!(!summary.all_eligible_records);
    }
}
