use std::collections::{BTreeMap, BTreeSet};

use rgv_core::GenomicInterval;

use crate::{
    AlignmentFlags, AlignmentRecord, AuxValue, CigarOp, CigarOpKind, ImplicitModificationMode,
    ModificationCode, SequenceData, aligned_position_for_sequencer_position,
    decode_modified_bases_parts, sequencer_base_matches,
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

/// One series' evidence at one site (or bin) of the summary.
///
/// A read is *called* at a site when its `MM` group assessed the base there: every base of the
/// canonical base under a `.` group, the listed bases under a `?` group. It counts as
/// *modified* for the code whose call is likeliest when that call is at least as likely as no
/// modification (the state the read stack draws), so a low-probability call is a called,
/// unmodified read. Reads without usable `MM`/`ML` are not counted at all.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModificationBin {
    /// Called reads whose likelier state is this series' modification.
    pub modified_calls: u32,
    /// Called reads: the denominator of the modified fraction.
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

/// One modification code's reads at a site or bin, merged across strands, haplotypes and
/// implicit modes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModificationCodeTally {
    pub code: Option<ModificationCode>,
    /// Reads modified with this code.
    pub modified: u32,
    /// Reads called for this code.
    pub called: u32,
    pub probability_sum: u64,
    pub probability_observations: u32,
}

impl ModificationCodeTally {
    /// Modified reads over called reads.
    #[must_use]
    pub fn fraction(self) -> Option<f64> {
        (self.called > 0).then(|| f64::from(self.modified) / f64::from(self.called))
    }

    /// Mean call probability (`ML / 255`) over called reads.
    #[must_use]
    pub fn weighted_fraction(self) -> Option<f64> {
        (self.called > 0)
            .then(|| self.probability_sum as f64 / (f64::from(self.called) * f64::from(u8::MAX)))
    }
}

/// Every code's reads at one site, or pooled over the sites of a bin: per code, the total
/// modified over the total called.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModificationSiteTally {
    /// Per code, in code order.
    pub codes: Vec<ModificationCodeTally>,
}

impl ModificationSiteTally {
    /// Merges the series evidence `entries` (any mix of series, sites and bins).
    pub fn from_bins<'a>(
        entries: impl IntoIterator<Item = (&'a ModificationSeriesKey, &'a ModificationBin)>,
    ) -> Self {
        let mut tally = Self::default();
        for (key, bin) in entries {
            tally.add(key.code, *bin);
        }
        tally
    }

    /// Adds one series' evidence for `code`.
    pub fn add(&mut self, code: ModificationCode, bin: ModificationBin) {
        let index = match self
            .codes
            .binary_search_by(|entry| entry.code.cmp(&Some(code)))
        {
            Ok(index) => index,
            Err(index) => {
                self.codes.insert(
                    index,
                    ModificationCodeTally {
                        code: Some(code),
                        ..ModificationCodeTally::default()
                    },
                );
                index
            }
        };
        let entry = &mut self.codes[index];
        entry.modified = entry.modified.saturating_add(bin.modified_calls);
        entry.called = entry.called.saturating_add(bin.canonical_observations);
        entry.probability_sum = entry.probability_sum.saturating_add(bin.probability_sum);
        entry.probability_observations = entry
            .probability_observations
            .saturating_add(bin.probability_observations);
    }

    /// Pools another site's tally into this one.
    pub fn merge(&mut self, other: &Self) {
        for entry in &other.codes {
            let Some(code) = entry.code else {
                continue;
            };
            self.add(
                code,
                ModificationBin {
                    modified_calls: entry.modified,
                    canonical_observations: entry.called,
                    probability_sum: entry.probability_sum,
                    probability_observations: entry.probability_observations,
                    ..ModificationBin::default()
                },
            );
        }
    }

    #[must_use]
    pub fn code(&self, code: ModificationCode) -> Option<ModificationCodeTally> {
        self.codes
            .iter()
            .copied()
            .find(|entry| entry.code == Some(code))
    }

    /// Reads called at the site: the most any code called (the codes of one base are usually
    /// called on the same reads).
    #[must_use]
    pub fn called(&self) -> u32 {
        self.codes
            .iter()
            .map(|entry| entry.called)
            .max()
            .unwrap_or(0)
    }

    /// Called reads modified by no code.
    #[must_use]
    pub fn unmodified(&self) -> u32 {
        self.called().saturating_sub(
            self.codes
                .iter()
                .map(|entry| entry.modified)
                .fold(0_u32, u32::saturating_add),
        )
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

    /// Every code's reads at bin `index`, merged across series.
    #[must_use]
    pub fn site_tally(&self, index: usize) -> ModificationSiteTally {
        ModificationSiteTally::from_bins(
            self.series
                .iter()
                .filter_map(|series| series.bins.get(index).map(|bin| (&series.key, bin))),
        )
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

pub struct ModificationSummaryBuilder {
    interval: GenomicInterval,
    bins: usize,
    series: Vec<ModificationSeries>,
    indices: BTreeMap<ModificationSeriesKey, usize>,
    dropped_keys: BTreeSet<ModificationSeriesKey>,
    provenance: ModificationProvenance,
}

impl ModificationSummaryBuilder {
    pub fn new(interval: GenomicInterval, bins: usize) -> Self {
        Self {
            interval,
            bins,
            series: Vec::new(),
            indices: BTreeMap::new(),
            dropped_keys: BTreeSet::new(),
            provenance: ModificationProvenance::default(),
        }
    }

    pub fn add_record(&mut self, record: &AlignmentRecord) {
        let mm = record
            .auxiliary_fields
            .iter()
            .find(|field| matches!(&field.tag, b"MM" | b"Mm"))
            .map(|field| &field.value);
        let ml = record
            .auxiliary_fields
            .iter()
            .find(|field| matches!(&field.tag, b"ML" | b"Ml"))
            .map(|field| &field.value);
        let haplotype = record
            .auxiliary_fields
            .iter()
            .find(|field| field.tag == *b"HP")
            .map(|field| &field.value);
        let sequence = match &record.sequence {
            SequenceData::Present { bases, .. } => Some(bases.as_slice()),
            SequenceData::NotLoaded | SequenceData::Missing => None,
        };
        let mn = record
            .auxiliary_fields
            .iter()
            .find(|field| field.tag == *b"MN")
            .map(|field| &field.value);
        self.add_record_parts_checked(
            record.start,
            &record.cigar,
            record.flags,
            sequence,
            mm,
            ml,
            mn,
            haplotype,
        );
    }

    /// Adds one packed record without materialising its sequence, CIGAR, or auxiliary fields.
    ///
    /// The `MN` tag is not consulted, so every hard-clipped record is treated as unresolvable.
    /// Prefer [`Self::add_record_parts_checked`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_record_parts(
        &mut self,
        alignment_start: u64,
        cigar: &[CigarOp],
        flags: AlignmentFlags,
        sequence: Option<&[u8]>,
        mm: Option<&AuxValue>,
        ml: Option<&AuxValue>,
        haplotype: Option<&AuxValue>,
    ) {
        self.add_record_parts_checked(
            alignment_start,
            cigar,
            flags,
            sequence,
            mm,
            ml,
            None,
            haplotype,
        );
    }

    /// Adds one packed record, using `MN` (when present) and hard clipping to decide whether the
    /// `MM` coordinates still describe the stored sequence. Unresolvable records are counted as
    /// unavailable instead of contributing misplaced calls.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn add_record_parts_checked(
        &mut self,
        alignment_start: u64,
        cigar: &[CigarOp],
        flags: AlignmentFlags,
        sequence: Option<&[u8]>,
        mm: Option<&AuxValue>,
        ml: Option<&AuxValue>,
        mn: Option<&AuxValue>,
        haplotype: Option<&AuxValue>,
    ) {
        let Some(mm) = mm else {
            return;
        };
        self.provenance.records_with_modification_tags = self
            .provenance
            .records_with_modification_tags
            .saturating_add(1);
        let Some(bases) = sequence else {
            self.provenance.unavailable_records =
                self.provenance.unavailable_records.saturating_add(1);
            return;
        };
        if !mm_resolvable(bases.len(), cigar, mn) {
            self.provenance.unavailable_records =
                self.provenance.unavailable_records.saturating_add(1);
            return;
        }
        let Ok(calls) = decode_modified_bases_parts(bases, flags, mm, ml) else {
            self.provenance.malformed_records = self.provenance.malformed_records.saturating_add(1);
            return;
        };
        self.provenance.decoded_calls = self.provenance.decoded_calls.saturating_add(calls.len());
        let haplotype = haplotype.and_then(haplotype_value);
        let key_of = |call: &crate::ModifiedBaseCall| ModificationSeriesKey {
            canonical_base: call.canonical_base,
            code: call.code,
            modification_strand: call.strand,
            implicit_mode: call.implicit_mode,
            haplotype,
        };
        let groups = calls.iter().map(key_of).collect::<BTreeSet<_>>();
        let place = |sequencer_position: usize, key: ModificationSeriesKey| {
            summary_position(
                alignment_start,
                cigar,
                bases,
                flags,
                sequencer_position,
                key.canonical_base,
                key.modification_strand,
            )
        };

        // Under a `.` group every base of the canonical base was assessed: each is a called
        // read at its site, unmodified unless one of the calls below says otherwise.
        for key in groups
            .iter()
            .copied()
            .filter(|key| key.implicit_mode == ImplicitModificationMode::Canonical)
        {
            let Some(series_index) = self.series_index(key) else {
                continue;
            };
            for sequencer_position in 0..bases.len() {
                if !sequencer_base_matches(bases, flags, sequencer_position, key.canonical_base) {
                    continue;
                }
                let Some(bin_index) =
                    place(sequencer_position, key).and_then(|position| self.bin_index(position))
                else {
                    continue;
                };
                let bin = &mut self.series[series_index].bins[bin_index];
                bin.canonical_observations = bin.canonical_observations.saturating_add(1);
            }
        }

        // Each base's calls decide its state as the read stack does: the likeliest modification
        // when it is at least as likely as no modification, else unmodified. The read counts as
        // modified for that code alone; every series that called the base counts it as called.
        let mut by_base = BTreeMap::<usize, Vec<&crate::ModifiedBaseCall>>::new();
        for call in &calls {
            by_base
                .entry(call.sequencer_position)
                .or_default()
                .push(call);
        }
        for (sequencer_position, base_calls) in by_base {
            let effective = |call: &crate::ModifiedBaseCall| call.probability.unwrap_or(u8::MAX);
            let total = base_calls
                .iter()
                .map(|call| u32::from(effective(call)))
                .sum::<u32>()
                .min(u32::from(u8::MAX));
            let unmodified = u8::MAX - u8::try_from(total).unwrap_or(u8::MAX);
            let mut winner: Option<usize> = None;
            for (index, call) in base_calls.iter().enumerate() {
                if winner.is_none_or(|best| effective(call) > effective(base_calls[best])) {
                    winner = Some(index);
                }
            }
            let winner = winner.filter(|best| effective(base_calls[*best]) >= unmodified);
            for (index, call) in base_calls.iter().enumerate() {
                let key = key_of(call);
                let Some(series_index) = self.series_index(key) else {
                    continue;
                };
                let Some(bin_index) =
                    place(sequencer_position, key).and_then(|position| self.bin_index(position))
                else {
                    continue;
                };
                let bin = &mut self.series[series_index].bins[bin_index];
                if key.implicit_mode == ImplicitModificationMode::Unknown {
                    // A `?` group assesses only the bases it lists.
                    bin.canonical_observations = bin.canonical_observations.saturating_add(1);
                }
                if winner == Some(index) {
                    bin.modified_calls = bin.modified_calls.saturating_add(1);
                    if flags.is_reverse() {
                        bin.reverse_modified_calls = bin.reverse_modified_calls.saturating_add(1);
                    } else {
                        bin.forward_modified_calls = bin.forward_modified_calls.saturating_add(1);
                    }
                }
                if let Some(probability) = call.probability {
                    bin.probability_sum =
                        bin.probability_sum.saturating_add(u64::from(probability));
                    bin.probability_observations = bin.probability_observations.saturating_add(1);
                } else {
                    bin.missing_probability_calls = bin.missing_probability_calls.saturating_add(1);
                }
            }
        }
    }

    pub fn finish(
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

fn haplotype_value(value: &AuxValue) -> Option<i32> {
    match value {
        AuxValue::Signed(value) => i32::try_from(*value).ok(),
        AuxValue::Unsigned(value) => i32::try_from(*value).ok(),
        AuxValue::String(value) => value.parse().ok(),
        _ => None,
    }
}

/// The reference position a call is summarised at. A cytosine call of a reverse-strand read
/// in a `CpG` (the read's own bases, stored `CG`) sits on the G of the forward strand; it is
/// moved onto the `CpG`'s C so both strands of a `CpG` are one site, as methylation callers
/// report it (`modkit pileup --combine-strands`).
fn summary_position(
    alignment_start: u64,
    cigar: &[CigarOp],
    bases: &[u8],
    flags: AlignmentFlags,
    sequencer_position: usize,
    canonical_base: u8,
    strand: crate::BaseModificationStrand,
) -> Option<u64> {
    let aligned = aligned_position_for_sequencer_position(bases.len(), flags, sequencer_position)?;
    let position = reference_position_for_aligned_position(alignment_start, cigar, aligned)?;
    let cpg_g = canonical_base.eq_ignore_ascii_case(&b'C')
        && flags.is_reverse()
        && strand == crate::BaseModificationStrand::Forward
        && aligned > 0
        && bases
            .get(aligned)
            .is_some_and(|base| base.eq_ignore_ascii_case(&b'G'))
        && bases[aligned - 1].eq_ignore_ascii_case(&b'C');
    if cpg_g
        && position > 0
        && reference_position_for_aligned_position(alignment_start, cigar, aligned - 1)
            == Some(position - 1)
    {
        return Some(position - 1);
    }
    Some(position)
}

fn reference_position_for_aligned_position(
    alignment_start: u64,
    cigar: &[CigarOp],
    aligned_position: usize,
) -> Option<u64> {
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

fn mm_resolvable(sequence_length: usize, cigar: &[CigarOp], mn: Option<&AuxValue>) -> bool {
    let hard_clipped = cigar
        .iter()
        .any(|operation| operation.kind == CigarOpKind::HardClip);
    crate::modified_bases_resolvable(sequence_length, mn, hard_clipped)
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
                bases: if flags & 0x10 == 0 {
                    b"ACCC".to_vec()
                } else {
                    // Original ACCC in reference-facing reverse-complement order.
                    b"GGGT".to_vec()
                },
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
    fn unknown_mode_counts_only_the_listed_bases_as_called() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("C+m?,1;", None, 0));
        let summary = builder.finish(1, true);
        let bin = summary.series[0].bins[2];
        assert_eq!(bin.modified_calls, 1);
        // The listed base is called; the unlisted cytosines are no-calls, not unmodified.
        assert_eq!(bin.canonical_observations, 1);
        assert_eq!(summary.series[0].bins[1].canonical_observations, 0);
        assert_eq!(summary.series[0].bins[3].canonical_observations, 0);
        assert_eq!(bin.missing_probability_calls, 1);
        assert_eq!(bin.raw_fraction(), Some(1.0));
        assert_eq!(bin.weighted_fraction(), Some(0.0));
    }

    /// A forward read `CGACGA` at 200 (`CpG`s at 200 and 203), with `MM`/`ML` as given.
    fn cpg_read(mm: Option<&str>, ml: Vec<u64>, flags: u16) -> AlignmentRecord {
        let mut record = record("C+m?;", None, flags);
        record.start = 200;
        record.end = 206;
        record.cigar = vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 6,
        }];
        // Stored forward-facing either way: the reverse read's original sequence is TCGTCG.
        record.sequence = SequenceData::Present {
            bases: b"CGACGA".to_vec(),
            qualities: None,
        };
        record.auxiliary_fields.clear();
        if let Some(mm) = mm {
            record.auxiliary_fields.push(AuxField {
                tag: *b"MM",
                value: AuxValue::String(mm.to_owned()),
            });
            record.auxiliary_fields.push(AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(ml),
            });
        }
        record
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn site_frequencies_count_called_reads_by_their_likelier_state() {
        const M: ModificationCode = ModificationCode::Letter(b'm');
        const H: ModificationCode = ModificationCode::Letter(b'h');
        let interval = GenomicInterval::new(ContigId::new(0), 200, 206).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 6);
        // Dorado-style explicit groups calling both CpG cytosines of each forward read.
        // 5mC at 200 and a no-call at 203 (unlisted).
        builder.add_record(&(cpg_read(Some("C+h?,0;C+m?,0;"), vec![5, 240], 0)));
        // 5mC 150 + 5hmC 20 at 200: modified (5mC 150 >= unmodified 85).
        builder.add_record(&(cpg_read(Some("C+h?,0;C+m?,0;"), vec![20, 150], 0)));
        // 5hmC wins at 200; at 203 both are low: unmodified.
        builder.add_record(&(cpg_read(Some("C+h?,0,0;C+m?,0,0;"), vec![200, 10, 30, 20], 0)));
        // A low-probability 5mC call (100 < unmodified 155): called, unmodified.
        builder.add_record(&(cpg_read(Some("C+m?,0;"), vec![100], 0)));
        // A tag-less read and an unresolvable one: never counted.
        builder.add_record(&(cpg_read(None, Vec::new(), 0)));
        let mut copied = cpg_read(Some("C+m?,0;"), vec![255], 0x800);
        copied.cigar.insert(
            0,
            CigarOp {
                kind: CigarOpKind::HardClip,
                len: 4,
            },
        );
        builder.add_record(&(copied));
        // A reverse read: its original TCGTCG has Cs at sequencer 1 and 4, stored G at 204 and
        // 201. Each is the G of a CpG and is summarised on the CpG's C (203 and 200).
        builder.add_record(&(cpg_read(Some("C+m?,0,0;"), vec![250, 250], 0x10)));
        let summary = builder.finish(7, true);
        assert_eq!(summary.provenance.records_with_modification_tags, 6);
        assert_eq!(summary.provenance.unavailable_records, 1);

        let site = summary.site_tally(0);
        assert_eq!(site.called(), 5);
        assert_eq!(
            site.code(M).map(|code| (code.modified, code.called)),
            Some((3, 5))
        );
        assert_eq!(
            site.code(H).map(|code| (code.modified, code.called)),
            Some((1, 3))
        );
        assert_eq!(site.unmodified(), 1);
        assert_eq!(
            site.code(M).and_then(ModificationCodeTally::fraction),
            Some(0.6)
        );

        let site = summary.site_tally(3);
        assert_eq!(site.called(), 2);
        assert_eq!(
            site.code(M).map(|code| (code.modified, code.called)),
            Some((1, 2))
        );
        assert_eq!(site.unmodified(), 1);
        // Nothing on the Gs: the reverse read's calls moved to the CpG's C.
        assert!(
            summary
                .site_tally(1)
                .codes
                .iter()
                .all(|code| code.called == 0)
        );
        assert!(
            summary
                .site_tally(4)
                .codes
                .iter()
                .all(|code| code.called == 0)
        );

        // Pooling two sites: per code, the total modified over the total called.
        let mut pooled = summary.site_tally(0);
        pooled.merge(&summary.site_tally(3));
        let m = pooled.code(M).unwrap();
        assert_eq!((m.modified, m.called), (4, 7));
    }

    #[test]
    fn reverse_alignment_maps_sequencer_oriented_calls_to_reference_positions() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&record("C+m,0;", Some(vec![255]), 0x10));
        let summary = builder.finish(1, true);
        assert_eq!(summary.series[0].bins[2].modified_calls, 1);
        assert_eq!(summary.series[0].bins[2].reverse_modified_calls, 1);
        assert_eq!(summary.series[0].bins[0].canonical_observations, 1);
        assert_eq!(summary.series[0].bins[1].canonical_observations, 1);
        assert_eq!(summary.series[0].bins[2].canonical_observations, 1);
        assert_eq!(summary.series[0].bins[3].canonical_observations, 0);
    }

    #[test]
    fn unresolvable_mm_is_reported_unavailable_instead_of_being_misplaced() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let hard_clipped = |mn: Option<u64>| {
            // 5H4M: MM written for the unclipped nine-base read would index the wrong bases.
            let mut record = record("C+m,1;", Some(vec![128]), 0);
            record.cigar.insert(
                0,
                CigarOp {
                    kind: CigarOpKind::HardClip,
                    len: 5,
                },
            );
            if let Some(length) = mn {
                record.auxiliary_fields.push(AuxField {
                    tag: *b"MN",
                    value: AuxValue::Unsigned(length),
                });
            }
            record
        };
        for unresolvable in [hard_clipped(None), hard_clipped(Some(9))] {
            let mut builder = ModificationSummaryBuilder::new(interval, 4);
            builder.add_record(&unresolvable);
            let summary = builder.finish(1, true);
            assert!(summary.series.is_empty());
            assert_eq!(summary.provenance.records_with_modification_tags, 1);
            assert_eq!(summary.provenance.unavailable_records, 1);
            assert_eq!(summary.provenance.malformed_records, 0);
            assert_eq!(summary.provenance.decoded_calls, 0);
        }
        // MN equal to the stored four bases proves MM was rewritten for the clipped record.
        // ACCC with `C+m,1`: the second C, read index 2, reference 102 (hard clips consume
        // neither read nor reference).
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&hard_clipped(Some(4)));
        let summary = builder.finish(1, true);
        assert_eq!(summary.series[0].bins[2].modified_calls, 1);
        assert_eq!(summary.provenance.unavailable_records, 0);
    }

    #[test]
    fn records_without_usable_tags_are_never_counted_as_unmodified() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        // A tag-less supplementary copy of the read, and one whose MM was copied onto its
        // hard-clipped sequence: neither adds a canonical observation (an "unmodified" vote).
        let mut tagless = record("C+m,1;", Some(vec![128]), 0x800);
        tagless.auxiliary_fields.clear();
        let mut copied = record("C+m,1;", Some(vec![128]), 0x800);
        copied.cigar.insert(
            0,
            CigarOp {
                kind: CigarOpKind::HardClip,
                len: 5,
            },
        );
        let mut alone = ModificationSummaryBuilder::new(interval, 4);
        alone.add_record(&record("C+m,1;", Some(vec![128]), 0));
        let alone = alone.finish(1, true);
        let mut builder = ModificationSummaryBuilder::new(interval, 4);
        builder.add_record(&tagless);
        builder.add_record(&record("C+m,1;", Some(vec![128]), 0));
        builder.add_record(&copied);
        let summary = builder.finish(3, true);
        assert_eq!(summary.series, alone.series);
        assert_eq!(summary.series[0].bins[1].canonical_observations, 1);
        assert_eq!(summary.series[0].bins[2].raw_fraction(), Some(1.0));
        assert_eq!(summary.provenance.records_with_modification_tags, 2);
        assert_eq!(summary.provenance.unavailable_records, 1);
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
