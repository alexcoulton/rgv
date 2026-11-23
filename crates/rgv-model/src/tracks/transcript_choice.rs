//! The representative transcript of each gene, for gene-model tracks drawn one transcript per
//! gene.
//!
//! Transcripts are grouped by gene: by `gene_id`, else by gene name and strand, always within one
//! contig, and only while their spans overlap or lie within [`GENE_GROUP_MAX_GAP`] of each other
//! ([`GENE_ID_GROUP_MAX_GAP`] for a `gene_id`), so genes that share a name but lie far apart stay
//! separate. Within a gene the
//! transcripts are ordered by the first rule that separates them:
//!
//! 1. a `MANE_Select` / `MANE Select` tag;
//! 2. an `Ensembl_canonical` or `RefSeq Select` tag;
//! 3. APPRIS principal level (`appris_principal_1` before `appris_principal_5`);
//! 4. a `CCDS` tag, a `ccdsid` attribute or a `CCDS:` database cross-reference;
//! 5. a `basic` tag;
//! 6. the longest coding sequence (exonic bases inside the coding interval);
//! 7. the longest spliced length (exonic bases);
//! 8. the smallest transcript identifier.
//!
//! The order depends only on each transcript's own data, so the first transcript is the same
//! whatever order the records arrive in. Sources write the result onto every transcript of a
//! gene with more than one ([`annotate_transcript_choices`]) when they load a whole file, or
//! when they read a window widened to the extent of the genes it touches
//! ([`transcript_group_read_extent`]), so the choice never depends on the window a record was
//! read in.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::{FeatureRecord, Strand};

/// Attribute carrying a transcript's place in its gene; see [`TranscriptChoice`].
pub const TRANSCRIPT_CHOICE_ATTRIBUTE: &str = "rgv_transcript_choice";

/// Transcripts of one gene name further apart than this are taken to be different genes.
pub const GENE_GROUP_MAX_GAP: u64 = 100_000;

/// The same for one `gene_id`, which names one locus in Ensembl and GENCODE but may be a plain
/// symbol in a bigGenePred: wide enough never to split a real gene.
pub const GENE_ID_GROUP_MAX_GAP: u64 = 2_500_000;

/// The rule that put a gene's representative transcript ahead of the next one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepresentativeReason {
    ManeSelect,
    Canonical,
    /// APPRIS principal level 1 to 5.
    AppRisPrincipal(u8),
    Ccds,
    Basic,
    LongestCoding,
    LongestSpliced,
    SmallestIdentifier,
}

impl RepresentativeReason {
    /// How the reason reads in the properties panel.
    #[must_use]
    pub fn description(self) -> String {
        match self {
            Self::ManeSelect => "MANE Select".to_owned(),
            Self::Canonical => "canonical (Ensembl canonical / RefSeq Select)".to_owned(),
            Self::AppRisPrincipal(level) => format!("APPRIS principal {level}"),
            Self::Ccds => "CCDS".to_owned(),
            Self::Basic => "basic".to_owned(),
            Self::LongestCoding => "longest CDS".to_owned(),
            Self::LongestSpliced => "longest spliced length".to_owned(),
            Self::SmallestIdentifier => "smallest transcript ID".to_owned(),
        }
    }

    fn code(self) -> String {
        match self {
            Self::ManeSelect => "mane".to_owned(),
            Self::Canonical => "canonical".to_owned(),
            Self::AppRisPrincipal(level) => format!("appris{level}"),
            Self::Ccds => "ccds".to_owned(),
            Self::Basic => "basic".to_owned(),
            Self::LongestCoding => "cds".to_owned(),
            Self::LongestSpliced => "spliced".to_owned(),
            Self::SmallestIdentifier => "id".to_owned(),
        }
    }

    fn parse(code: &str) -> Option<Self> {
        Some(match code {
            "mane" => Self::ManeSelect,
            "canonical" => Self::Canonical,
            "ccds" => Self::Ccds,
            "basic" => Self::Basic,
            "cds" => Self::LongestCoding,
            "spliced" => Self::LongestSpliced,
            "id" => Self::SmallestIdentifier,
            other => Self::AppRisPrincipal(
                other
                    .strip_prefix("appris")?
                    .parse()
                    .ok()
                    .filter(|level| (1..=5).contains(level))?,
            ),
        })
    }
}

/// A transcript's place among the transcripts of its gene, as written by
/// [`annotate_transcript_choices`]. Only genes with more than one transcript carry one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptChoice<'a> {
    /// 0 for the representative transcript, then in order of preference.
    pub rank: usize,
    /// Transcripts of the gene, the representative included.
    pub count: usize,
    /// Why the representative comes first.
    pub reason: RepresentativeReason,
    /// The representative transcript's identifier.
    pub representative: &'a str,
    /// The gene: its `gene_id`, else `name@contig+` (strand `+`, `-` or `.`).
    pub gene_key: &'a str,
}

impl TranscriptChoice<'_> {
    #[must_use]
    pub const fn is_representative(&self) -> bool {
        self.rank == 0
    }

    /// Transcripts of the gene other than the representative.
    #[must_use]
    pub const fn alternatives(&self) -> usize {
        self.count.saturating_sub(1)
    }
}

impl FeatureRecord {
    /// This transcript's place in its gene, when its gene has more than one transcript.
    #[must_use]
    pub fn transcript_choice(&self) -> Option<TranscriptChoice<'_>> {
        let value = self
            .attributes
            .iter()
            .find(|(key, _)| key == TRANSCRIPT_CHOICE_ATTRIBUTE)
            .map(|(_, value)| value.as_str())?;
        let mut fields = value.splitn(5, '\t');
        let rank = fields.next()?.parse().ok()?;
        let count = fields.next()?.parse().ok()?;
        let reason = RepresentativeReason::parse(fields.next()?)?;
        let representative = fields.next()?;
        let gene_key = fields.next()?;
        (rank < count).then_some(TranscriptChoice {
            rank,
            count,
            reason,
            representative,
            gene_key,
        })
    }

    /// The identifier a transcript is listed and ordered by: `transcript_id`, else `ID`, else
    /// `Name`, else the record name.
    #[must_use]
    pub fn transcript_identifier(&self) -> Option<&str> {
        ["transcript_id", "ID", "Name"]
            .iter()
            .find_map(|key| attribute(self, key))
            .or_else(|| {
                self.name
                    .as_deref()
                    .filter(|value| !value.trim().is_empty() && *value != ".")
            })
    }

    /// Whether the record belongs to a protein-coding gene: by `gene_type`/`gene_biotype`, else
    /// by `transcript_type`/`transcript_biotype`/`biotype`, else by having a coding interval
    /// (genePred and bigGenePred `cdsStart != cdsEnd`).
    #[must_use]
    pub fn is_protein_coding(&self) -> bool {
        for keys in [
            &["gene_type", "gene_biotype"][..],
            &["transcript_type", "transcript_biotype", "biotype"][..],
        ] {
            if let Some(value) = keys.iter().find_map(|key| attribute(self, key)) {
                return value.eq_ignore_ascii_case("protein_coding");
            }
        }
        self.coding_interval.is_some_and(|(start, end)| start < end)
    }
}

fn attribute<'a>(record: &'a FeatureRecord, key: &str) -> Option<&'a str> {
    record
        .attributes
        .iter()
        .find(|(name, value)| name == key && !value.trim().is_empty() && value != ".")
        .map(|(_, value)| value.trim())
}

/// Whether a record is a transcript model that takes part in the choice: an assembled GTF/GFF3
/// transcript, a genePred/refFlat/bigGenePred row, or an untyped record.
fn is_transcript(record: &FeatureRecord) -> bool {
    record.feature_type.as_deref().is_none_or(|feature_type| {
        let feature_type = feature_type.to_ascii_lowercase();
        feature_type == "transcript"
            || feature_type == "primary_transcript"
            || feature_type.ends_with("rna")
    })
}

/// The gene a transcript is grouped under, before the span check: its `gene_id`, else its gene
/// name with contig and strand.
#[must_use]
pub fn transcript_gene_key(record: &FeatureRecord) -> Option<String> {
    gene_key(record).map(|(key, _)| key)
}

/// The gene key, and whether it is a `gene_id`.
fn gene_key(record: &FeatureRecord) -> Option<(String, bool)> {
    if !is_transcript(record) {
        return None;
    }
    if let Some(gene_id) = attribute(record, "gene_id") {
        return Some((gene_id.replace('\t', " "), true));
    }
    let name = ["gene_name", "gene", "name2", "gene_symbol"]
        .iter()
        .find_map(|key| attribute(record, key))?;
    let strand = match record.strand {
        Strand::Forward => '+',
        Strand::Reverse => '-',
        Strand::Unstranded => '.',
    };
    Some((
        format!("{name}@{}{strand}", record.contig).replace('\t', " "),
        false,
    ))
}

/// The tags of a transcript: every `tag`/`tags` attribute (a GTF line may carry many), split at
/// commas as GFF3 writes them, and the comma-separated values of bigBed extension fields.
fn tags(record: &FeatureRecord) -> impl Iterator<Item = &str> {
    record
        .attributes
        .iter()
        .filter(|(key, _)| {
            key.eq_ignore_ascii_case("tag")
                || key.eq_ignore_ascii_case("tags")
                || key.starts_with("extension_field_")
        })
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
}

/// What a transcript offers on each rule, best first within a field.
#[allow(clippy::struct_excessive_bools)] // One flag per tag rule, compared field by field.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Merit {
    mane: bool,
    canonical: bool,
    /// 5 for `appris_principal_1` down to 1 for `appris_principal_5`; 0 without.
    appris: u8,
    ccds: bool,
    basic: bool,
    coding: u64,
    spliced: u64,
}

fn merit(record: &FeatureRecord) -> Merit {
    let mut merit = Merit::default();
    for tag in tags(record) {
        let tag = tag.to_ascii_lowercase();
        match tag.as_str() {
            "mane_select" | "mane select" => merit.mane = true,
            "ensembl_canonical" | "refseq select" | "refseq_select" => merit.canonical = true,
            "ccds" => merit.ccds = true,
            "basic" => merit.basic = true,
            _ => {
                if let Some(level) = tag.strip_prefix("appris_principal") {
                    let level = level.trim_start_matches('_');
                    let level = if level.is_empty() {
                        Some(1)
                    } else {
                        level
                            .parse::<u8>()
                            .ok()
                            .filter(|level| (1..=5).contains(level))
                    };
                    if let Some(level) = level {
                        merit.appris = merit.appris.max(6 - level);
                    }
                }
            }
        }
    }
    merit.ccds |= attribute(record, "ccdsid").is_some()
        || attribute(record, "ccds_id").is_some()
        || record.attributes.iter().any(|(key, value)| {
            (key.eq_ignore_ascii_case("dbxref") || key.eq_ignore_ascii_case("db_xref"))
                && value
                    .split(',')
                    .any(|reference| reference.trim().starts_with("CCDS:"))
        });
    let blocks = if record.blocks.is_empty() {
        vec![(record.start, record.end)]
    } else {
        record.blocks.clone()
    };
    merit.spliced = blocks
        .iter()
        .map(|(start, end)| end.saturating_sub(*start))
        .sum();
    merit.coding = record
        .coding_interval
        .map_or(0, |(coding_start, coding_end)| {
            blocks
                .iter()
                .map(|(start, end)| {
                    (*end)
                        .min(coding_end)
                        .saturating_sub((*start).max(coding_start))
                })
                .sum()
        });
    merit
}

/// The first rule on which `first` is preferred over `second`.
fn separating_rule(first: Merit, second: Merit) -> RepresentativeReason {
    if first.mane != second.mane {
        RepresentativeReason::ManeSelect
    } else if first.canonical != second.canonical {
        RepresentativeReason::Canonical
    } else if first.appris != second.appris {
        RepresentativeReason::AppRisPrincipal(6 - first.appris)
    } else if first.ccds != second.ccds {
        RepresentativeReason::Ccds
    } else if first.basic != second.basic {
        RepresentativeReason::Basic
    } else if first.coding != second.coding {
        RepresentativeReason::LongestCoding
    } else if first.spliced != second.spliced {
        RepresentativeReason::LongestSpliced
    } else {
        RepresentativeReason::SmallestIdentifier
    }
}

struct Candidate<'a> {
    index: usize,
    merit: Merit,
    identifier: &'a str,
    record: &'a FeatureRecord,
}

/// Preferred first. Ties after every rule fall to position and shape, so the order is total
/// over distinct records and never depends on input order.
fn preference(left: &Candidate<'_>, right: &Candidate<'_>) -> Ordering {
    let (a, b) = (left.merit, right.merit);
    b.mane
        .cmp(&a.mane)
        .then(b.canonical.cmp(&a.canonical))
        .then(b.appris.cmp(&a.appris))
        .then(b.ccds.cmp(&a.ccds))
        .then(b.basic.cmp(&a.basic))
        .then(b.coding.cmp(&a.coding))
        .then(b.spliced.cmp(&a.spliced))
        .then_with(|| left.identifier.cmp(right.identifier))
        .then_with(|| {
            (left.record.start, left.record.end).cmp(&(right.record.start, right.record.end))
        })
        .then_with(|| left.record.blocks.cmp(&right.record.blocks))
        .then_with(|| {
            left.record
                .coding_interval
                .cmp(&right.record.coding_interval)
        })
        .then_with(|| left.record.name.cmp(&right.record.name))
        .then(left.index.cmp(&right.index))
}

/// Groups `records` into genes: indices per gene, each list in genomic order.
fn gene_groups(records: &[FeatureRecord]) -> Vec<(String, Vec<usize>)> {
    let mut by_key = HashMap::<(String, String, bool), Vec<usize>>::new();
    for (index, record) in records.iter().enumerate() {
        if let Some((key, by_id)) = gene_key(record) {
            by_key
                .entry((record.contig.clone(), key, by_id))
                .or_default()
                .push(index);
        }
    }
    let mut groups = Vec::new();
    for ((_, key, by_id), mut members) in by_key {
        let gap = if by_id {
            GENE_ID_GROUP_MAX_GAP
        } else {
            GENE_GROUP_MAX_GAP
        };
        members.sort_by_key(|&index| (records[index].start, records[index].end, index));
        let mut current: Vec<usize> = Vec::new();
        let mut current_end = 0_u64;
        for index in members {
            let record = &records[index];
            if !current.is_empty() && record.start > current_end.saturating_add(gap) {
                groups.push((key.clone(), std::mem::take(&mut current)));
                current_end = 0;
            }
            current_end = current_end.max(record.end);
            current.push(index);
        }
        if !current.is_empty() {
            groups.push((key, current));
        }
    }
    groups
}

/// Writes [`TRANSCRIPT_CHOICE_ATTRIBUTE`] onto every transcript of every gene in `records` that
/// has more than one, replacing any written before. Records keep their order.
pub fn annotate_transcript_choices(records: &mut [FeatureRecord]) {
    for record in records.iter_mut() {
        record
            .attributes
            .retain(|(key, _)| key != TRANSCRIPT_CHOICE_ATTRIBUTE);
    }
    let mut written = Vec::new();
    for (gene_key, members) in gene_groups(records) {
        if members.len() < 2 {
            continue;
        }
        let mut candidates = members
            .iter()
            .map(|&index| Candidate {
                index,
                merit: merit(&records[index]),
                identifier: records[index].transcript_identifier().unwrap_or_default(),
                record: &records[index],
            })
            .collect::<Vec<_>>();
        candidates.sort_by(preference);
        let reason = separating_rule(candidates[0].merit, candidates[1].merit);
        let representative = candidates[0].identifier.replace('\t', " ");
        let count = candidates.len();
        for (rank, candidate) in candidates.iter().enumerate() {
            written.push((
                candidate.index,
                format!(
                    "{rank}\t{count}\t{}\t{representative}\t{gene_key}",
                    reason.code()
                ),
            ));
        }
    }
    for (index, value) in written {
        records[index]
            .attributes
            .push((TRANSCRIPT_CHOICE_ATTRIBUTE.to_owned(), value));
    }
}

/// The interval a windowed read of `[start, end)` must cover so every gene with a transcript
/// in the window has all its transcripts read: the window widened to the extent of each such
/// gene's transcripts in `records` (a read of at least the window), by at most `reach` bases on
/// either side. A transcript lying beyond every one of `records` is found by reading again.
#[must_use]
pub fn transcript_group_read_extent(
    records: &[FeatureRecord],
    start: u64,
    end: u64,
    reach: u64,
) -> (u64, u64) {
    let mut extents = HashMap::<(&str, String), (u64, u64, bool)>::new();
    for record in records {
        if let Some(key) = transcript_gene_key(record) {
            let in_window = record.start < end && record.end > start;
            let extent = extents.entry((record.contig.as_str(), key)).or_insert((
                record.start,
                record.end,
                in_window,
            ));
            extent.0 = extent.0.min(record.start);
            extent.1 = extent.1.max(record.end);
            extent.2 |= in_window;
        }
    }
    let (mut low, mut high) = (start, end);
    for (extent_start, extent_end, _) in extents.values().filter(|extent| extent.2) {
        low = low.min(*extent_start);
        high = high.max(*extent_end);
    }
    (
        low.max(start.saturating_sub(reach)),
        high.min(end.saturating_add(reach)),
    )
}

#[cfg(test)]
#[path = "transcript_choice_tests.rs"]
mod tests;
