use std::collections::{BTreeMap, BTreeSet};

use rgv_core::GenomicInterval;

use crate::{
    AlignmentFlags, AlignmentRecord, AuxValue, CigarOp, CigarOpKind, ImplicitModificationMode,
    ModificationCode, SequenceData, decode_modified_bases,
};

/// Hard bound on independently rendered modification/canonical/strand/haplotype series.
pub const MAX_MODIFICATION_SERIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModificationSeriesKey {
    pub canonical_base: u8,
    pub code: ModificationCode,
    pub modification_strand: crate::BaseModificationStrand,
    pub implicit_mode: ImplicitModificationMode,
    pub haplotype: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModificationBin {
    pub modified_calls: u32,
    pub canonical_observations: u32,
    pub forward_modified_calls: u32,
    pub reverse_modified_calls: u32,
    pub probability_sum: u64,
    pub probability_observations: u32,
    pub missing_probability_calls: u32,
}

impl ModificationBin {
    pub fn raw_fraction(self) -> Option<f64> {
        (self.canonical_observations > 0)
            .then(|| f64::from(self.modified_calls) / f64::from(self.canonical_observations))
    }

    /// Probability-weighted modified fraction, using `ML / 255` as call support.
    pub fn weighted_fraction(self) -> Option<f64> {
        (self.canonical_observations > 0).then(|| {
            self.probability_sum as f64
                / (f64::from(self.canonical_observations) * f64::from(u8::MAX))
        })
    }

    pub fn mean_probability(self) -> Option<f64> {
        (self.probability_observations > 0)
            .then(|| self.probability_sum as f64 / f64::from(self.probability_observations))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModificationSeries {
    pub key: ModificationSeriesKey,
    pub bins: Vec<ModificationBin>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModificationProvenance {
    pub eligible_records: usize,
    pub records_with_modification_tags: usize,
    pub decoded_calls: usize,
    pub malformed_records: usize,
    pub unavailable_records: usize,
    pub dropped_series: usize,
    pub all_eligible_records: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModificationSummary {
    pub bin_count: usize,
    pub series: Vec<ModificationSeries>,
    pub provenance: ModificationProvenance,
}

impl ModificationSummary {
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }

    pub fn bin_count(&self) -> usize {
        self.bin_count
    }

    pub fn estimated_bytes(&self) -> usize {
        self.series.iter().fold(0_usize, |total, series| {
            total
                .saturating_add(std::mem::size_of::<ModificationSeries>())
                .saturating_add(
                    series
                        .bins
                        .len()
                        .saturating_mul(std::mem::size_of::<ModificationBin>()),
                )
        })
    }

    pub fn bin_index_at(&self, query_interval: GenomicInterval, position: u64) -> Option<usize> {
        let bins = self.bin_count();
        if bins == 0 || !query_interval.contains(position) {
            return None;
        }
        let offset = u128::from(position - query_interval.start());
        let index = offset.saturating_mul(bins as u128) / u128::from(query_interval.len());
        usize::try_from(index).ok().map(|index| index.min(bins - 1))
    }

    pub fn bin_interval(
        &self,
        query_interval: GenomicInterval,
        index: usize,
    ) -> Option<GenomicInterval> {
        let bins = self.bin_count();
        if bins == 0 || index >= bins {
            return None;
        }
        let length = u128::from(query_interval.len());
        let count = bins as u128;
        let start_offset = (index as u128).saturating_mul(length) / count;
        let end_offset = (index as u128 + 1).saturating_mul(length).div_ceil(count);
        GenomicInterval::new(
            query_interval.contig(),
            query_interval
                .start()
                .saturating_add(u64::try_from(start_offset).ok()?),
            query_interval
                .start()
                .saturating_add(u64::try_from(end_offset).ok()?)
                .min(query_interval.end()),
        )
        .ok()
    }
}

pub(crate) struct ModificationSummaryBuilder {
    interval: GenomicInterval,
    bins: usize,
    series: Vec<ModificationSeries>,
    indices: BTreeMap<ModificationSeriesKey, usize>,
    dropped_keys: BTreeSet<ModificationSeriesKey>,
    provenance: ModificationProvenance,
}

impl ModificationSummaryBuilder {
    pub(crate) fn new(interval: GenomicInterval, bins: usize) -> Self {
        Self {
            interval,
            bins,
            series: Vec::new(),
            indices: BTreeMap::new(),
            dropped_keys: BTreeSet::new(),
            provenance: ModificationProvenance::default(),
        }
    }

    pub(crate) fn add_record(&mut self, record: &AlignmentRecord) {
        if !has_modification_tag(record) {
            return;
        }
        self.provenance.records_with_modification_tags = self
            .provenance
            .records_with_modification_tags
            .saturating_add(1);
        let SequenceData::Present { bases, .. } = &record.sequence else {
            self.provenance.unavailable_records =
                self.provenance.unavailable_records.saturating_add(1);
            return;
        };
        let Ok(calls) = decode_modified_bases(record) else {
            self.provenance.malformed_records = self.provenance.malformed_records.saturating_add(1);
            return;
        };
        self.provenance.decoded_calls = self.provenance.decoded_calls.saturating_add(calls.len());
        let haplotype = record_haplotype(record);
        let groups = calls
            .iter()
            .map(|call| ModificationSeriesKey {
                canonical_base: call.canonical_base,
                code: call.code,
                modification_strand: call.strand,
                implicit_mode: call.implicit_mode,
                haplotype,
            })
            .collect::<BTreeSet<_>>();

        for key in groups
            .iter()
            .copied()
            .filter(|key| key.implicit_mode == ImplicitModificationMode::Canonical)
        {
            let Some(series_index) = self.series_index(key) else {
                continue;
            };
            for (stored_position, base) in bases.iter().copied().enumerate() {
                if base.to_ascii_uppercase() != key.canonical_base {
                    continue;
                }
                let Some(position) = reference_position_for_stored_position(
                    record.start,
                    &record.cigar,
                    bases.len(),
                    record.flags,
                    stored_position,
                ) else {
                    continue;
                };
                let Some(bin_index) = self.bin_index(position) else {
                    continue;
                };
                let bin = &mut self.series[series_index].bins[bin_index];
                bin.canonical_observations = bin.canonical_observations.saturating_add(1);
            }
        }

        for call in calls {
            let key = ModificationSeriesKey {
                canonical_base: call.canonical_base,
                code: call.code,
                modification_strand: call.strand,
                implicit_mode: call.implicit_mode,
                haplotype,
            };
            let Some(series_index) = self.series_index(key) else {
                continue;
            };
            let Some(position) = reference_position_for_stored_position(
                record.start,
                &record.cigar,
                bases.len(),
                record.flags,
                call.read_position,
            ) else {
                continue;
            };
            let Some(bin_index) = self.bin_index(position) else {
                continue;
            };
            let bin = &mut self.series[series_index].bins[bin_index];
            bin.modified_calls = bin.modified_calls.saturating_add(1);
            if record.flags.is_reverse() {
                bin.reverse_modified_calls = bin.reverse_modified_calls.saturating_add(1);
            } else {
                bin.forward_modified_calls = bin.forward_modified_calls.saturating_add(1);
            }
            if let Some(probability) = call.probability {
                bin.probability_sum = bin.probability_sum.saturating_add(u64::from(probability));
                bin.probability_observations = bin.probability_observations.saturating_add(1);
            } else {
                bin.missing_probability_calls = bin.missing_probability_calls.saturating_add(1);
            }
        }
    }

    pub(crate) fn finish(
        mut self,
        eligible_records: usize,
        all_eligible_records: bool,
    ) -> ModificationSummary {
        self.provenance.eligible_records = eligible_records;
        self.provenance.all_eligible_records = all_eligible_records;
        self.provenance.dropped_series = self.dropped_keys.len();
        ModificationSummary {
            bin_count: self.bins,
            series: self.series,
            provenance: self.provenance,
        }
    }

    fn series_index(&mut self, key: ModificationSeriesKey) -> Option<usize> {
        if let Some(index) = self.indices.get(&key) {
            return Some(*index);
        }
        if self.series.len() >= MAX_MODIFICATION_SERIES {
            self.dropped_keys.insert(key);
            return None;
        }
        let index = self.series.len();
        self.series.push(ModificationSeries {
            key,
            bins: vec![ModificationBin::default(); self.bins],
        });
        self.indices.insert(key, index);
        Some(index)
    }

    fn bin_index(&self, position: u64) -> Option<usize> {
        if self.bins == 0 || !self.interval.contains(position) {
            return None;
        }
        let offset = u128::from(position - self.interval.start());
        let index = offset.saturating_mul(self.bins as u128) / u128::from(self.interval.len());
        usize::try_from(index)
            .ok()
            .map(|index| index.min(self.bins - 1))
    }
}

fn has_modification_tag(record: &AlignmentRecord) -> bool {
    record
        .auxiliary_fields
        .iter()
        .any(|field| matches!(&field.tag, b"MM" | b"Mm"))
}

fn record_haplotype(record: &AlignmentRecord) -> Option<i32> {
    let value = &record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"HP")?
        .value;
    match value {
        AuxValue::Signed(value) => i32::try_from(*value).ok(),
        AuxValue::Unsigned(value) => i32::try_from(*value).ok(),
        AuxValue::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn reference_position_for_stored_position(
    alignment_start: u64,
    cigar: &[CigarOp],
    sequence_length: usize,
    flags: AlignmentFlags,
    stored_position: usize,
) -> Option<u64> {
    let aligned_position = if flags.is_reverse() {
        sequence_length.checked_sub(stored_position.saturating_add(1))?
    } else {
        stored_position
    };
    let mut reference_position = alignment_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if (read_position..read_position.saturating_add(length)).contains(&aligned_position)
                {
                    return Some(
                        reference_position
                            .saturating_add(aligned_position.saturating_sub(read_position) as u64),
                    );
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use rgv_core::ContigId;

    use super::*;
    use crate::{AuxField, BaseModificationStrand};

    fn record(mm: &str, ml: Option<Vec<u64>>, flags: u16) -> AlignmentRecord {
        let mut auxiliary_fields = vec![AuxField {
            tag: *b"MM",
            value: AuxValue::String(mm.to_owned()),
        }];
        if let Some(ml) = ml {
            auxiliary_fields.push(AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(ml),
            });
        }
        auxiliary_fields.push(AuxField {
            tag: *b"HP",
            value: AuxValue::Unsigned(2),
        });
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 104,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"modified".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACCC".to_vec(),
                qualities: None,
            },
            auxiliary_fields,
        }
    }

    #[test]
    fn exact_bins_count_modified_and_implicit_canonical_evidence() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("C+m,1;", Some(vec![128]), 0));
        let summary = builder.finish(1, true);
        assert_eq!(summary.series.len(), 1);
        let series = &summary.series[0];
        assert_eq!(series.key.canonical_base, b'C');
        assert_eq!(series.key.code, ModificationCode::Letter(b'm'));
        assert_eq!(
            series.key.modification_strand,
            BaseModificationStrand::Forward
        );
        assert_eq!(series.key.haplotype, Some(2));
        assert_eq!(series.bins[1].canonical_observations, 1);
        assert_eq!(series.bins[2].canonical_observations, 1);
        assert_eq!(series.bins[2].modified_calls, 1);
        assert_eq!(series.bins[2].probability_sum, 128);
        assert_eq!(series.bins[3].canonical_observations, 1);
        assert_eq!(summary.provenance.decoded_calls, 1);
    }

    #[test]
    fn unknown_mode_does_not_invent_an_unmodified_denominator() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("C+m?,1;", None, 0));
        let summary = builder.finish(1, true);
        let bin = summary.series[0].bins[2];
        assert_eq!(bin.modified_calls, 1);
        assert_eq!(bin.canonical_observations, 0);
        assert_eq!(bin.missing_probability_calls, 1);
        assert_eq!(bin.raw_fraction(), None);
        assert_eq!(bin.weighted_fraction(), None);
    }

    #[test]
    fn reverse_alignment_maps_stored_calls_to_reverse_reference_positions() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("C+m,0;", Some(vec![255]), 0x10));
        let summary = builder.finish(1, true);
        assert_eq!(summary.series[0].bins[2].modified_calls, 1);
        assert_eq!(summary.series[0].bins[2].reverse_modified_calls, 1);
    }

    #[test]
    fn malformed_records_are_reported_without_failing_the_summary() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("broken", None, 0));
        let summary = builder.finish(1, true);
        assert!(summary.series.is_empty());
        assert_eq!(summary.provenance.records_with_modification_tags, 1);
        assert_eq!(summary.provenance.malformed_records, 1);
        assert_eq!(summary.bin_count(), 4);
    }

    #[test]
    fn distinct_series_are_hard_capped_and_reported() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mm = (0..MAX_MODIFICATION_SERIES + 8).fold(String::new(), |mut mm, index| {
            write!(mm, "C+{},0;", 10_000 + index).unwrap();
            mm
        });
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record(
            &mm,
            Some(vec![255; MAX_MODIFICATION_SERIES + 8]),
            0,
        ));
        let summary = builder.finish(1, true);
        assert_eq!(summary.series.len(), MAX_MODIFICATION_SERIES);
        assert_eq!(summary.provenance.dropped_series, 8);
    }
}
