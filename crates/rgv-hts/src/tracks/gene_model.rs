//! Assembles the lines of a GTF or GFF3 file into transcript models, as IGV draws them.
//!
//! Each line of these formats is one part of a gene: a gene, a transcript, or one exon, CDS,
//! UTR or codon of a transcript. A transcript is drawn as one record carrying its exons as
//! `blocks` and its coding span as `coding_interval`, the same shape as a genePred record:
//!
//! * Lines are grouped by transcript: GTF by `transcript_id`; GFF3 by `Parent`, so a part with
//!   several parents (`Parent=tx1,tx2`) belongs to each. Only part lines are grouped (the types
//!   of [`SearchLevel::SubFeature`]: exon, CDS, UTR, start/stop codon, Selenocysteine, …).
//! * `blocks` are the exon lines, sorted and merged; without exon lines, the CDS, UTR and codon
//!   lines; without those, every other part except introns.
//! * `coding_interval` runs from the first to the last base of the CDS, start-codon and
//!   stop-codon lines (GTF keeps the stop codon outside the CDS).
//! * The span is the transcript line's, widened to the blocks; without a transcript line, the
//!   blocks'. Its attributes, strand and score are the transcript line's (without one, the
//!   first part's attributes less its own exon identifiers). A GFF3 transcript without a gene
//!   name takes its gene's (`gene` attribute, or the parent gene line's `gene_name`/`Name`).
//!   The record is named by gene name and typed `transcript`.
//! * A gene line (or any GFF3 parent) of a drawn transcript is not drawn. A transcript or gene
//!   line with no parts, and every line that belongs to no transcript, is drawn as before.

use std::collections::{HashMap, HashSet};

use super::FeatureRecord;
use super::feature_search::{SearchLevel, search_level};

/// What became of one input line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Placement {
    /// Drawn as output record `index`: unchanged, or as the transcript line of an assembled
    /// model.
    Drawn(usize),
    /// A gene (or other parent) line whose transcripts are drawn instead; `index` into
    /// [`Assembly::hidden`].
    Hidden(usize),
    /// A part folded into its transcript's model.
    Absorbed,
}

pub(super) struct Assembly {
    /// Records to draw, in no particular order.
    pub(super) records: Vec<FeatureRecord>,
    /// Container lines not drawn because their transcripts are.
    pub(super) hidden: Vec<FeatureRecord>,
    /// One per input line.
    pub(super) placements: Vec<Placement>,
}

/// Folded lines freed on another thread when at least this many.
const BACKGROUND_FREE_LINES: usize = 65_536;

/// Attributes describing one exon line rather than its transcript.
const PART_ONLY_ATTRIBUTES: [&str; 6] = [
    "exon_number",
    "exon_id",
    "exon_version",
    "ID",
    "Name",
    "rank",
];

fn attribute<'a>(record: &'a FeatureRecord, key: &str) -> Option<&'a str> {
    record
        .attributes
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty() && *value != ".")
}

fn has_attribute(record: &FeatureRecord, key: &str) -> bool {
    record.attributes.iter().any(|(name, _)| name == key)
}

/// Whether a line may stand for a transcript or gene whose parts can lie beyond it: the lines
/// whose full extent a windowed read must cover to assemble every model it touches.
fn may_have_parts(record: &FeatureRecord, gtf: bool) -> bool {
    match search_level(record) {
        SearchLevel::SubFeature => false,
        SearchLevel::Gene => true,
        SearchLevel::Other => {
            if gtf {
                return attribute(record, "transcript_id").is_some();
            }
            let feature_type = record
                .feature_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            has_attribute(record, "Parent")
                || feature_type.ends_with("rna")
                || feature_type.ends_with("transcript")
        }
    }
}

/// The parts of one transcript, by input index.
#[derive(Default)]
struct Group {
    line: Option<usize>,
    parts: Vec<usize>,
}

/// How the lines of one contig group into transcripts.
struct Grouping {
    groups: Vec<Group>,
    /// Whether each line is a part of some transcript.
    part: Vec<bool>,
    /// Whether each line is a container not drawn because its transcripts are.
    hide: Vec<bool>,
    /// A GFF3 transcript's parent gene line, by group.
    parent_gene: HashMap<usize, usize>,
}

impl Grouping {
    fn new(count: usize) -> Self {
        Self {
            groups: Vec::new(),
            part: vec![false; count],
            hide: vec![false; count],
            parent_gene: HashMap::new(),
        }
    }
}

/// Transcript keys to groups.
#[derive(Default)]
struct Keys<'a> {
    groups: HashMap<&'a str, usize>,
    /// Lines of one transcript are usually adjacent: the last key is checked first.
    last: Option<(&'a str, usize)>,
}

impl<'a> Keys<'a> {
    /// The group of `key`, and whether it was created now.
    fn group(&mut self, key: &'a str, groups: &mut Vec<Group>) -> (usize, bool) {
        if let Some((last, group)) = self.last
            && last == key
        {
            return (group, false);
        }
        let mut created = false;
        let group = *self.groups.entry(key).or_insert_with(|| {
            created = true;
            groups.push(Group::default());
            groups.len() - 1
        });
        self.last = Some((key, group));
        (group, created)
    }
}

/// GTF: parts by `transcript_id`; a gene line is hidden when a transcript of its `gene_id` is
/// drawn.
fn group_gtf(records: &[FeatureRecord]) -> Grouping {
    let mut grouping = Grouping::new(records.len());
    let mut keys = Keys::default();
    let mut genes = Vec::new();
    for (index, record) in records.iter().enumerate() {
        match (search_level(record), attribute(record, "transcript_id")) {
            (SearchLevel::SubFeature, Some(key)) => {
                let (group, _) = keys.group(key, &mut grouping.groups);
                grouping.groups[group].parts.push(index);
                grouping.part[index] = true;
            }
            (SearchLevel::Other, Some(key)) => {
                let (group, _) = keys.group(key, &mut grouping.groups);
                grouping.groups[group].line.get_or_insert(index);
            }
            (SearchLevel::Gene, _) => genes.push(index),
            _ => {}
        }
    }
    let drawn_genes = grouping
        .groups
        .iter()
        .filter(|group| !group.parts.is_empty())
        .filter_map(|group| {
            group
                .line
                .into_iter()
                .chain(group.parts.iter().copied())
                .find_map(|index| attribute(&records[index], "gene_id"))
        })
        .collect::<HashSet<_>>();
    for index in genes {
        grouping.hide[index] =
            attribute(&records[index], "gene_id").is_some_and(|gene| drawn_genes.contains(gene));
    }
    grouping
}

/// GFF3: parts by each of their `Parent`s; the line with that `ID` is the transcript line, and
/// its own parents are hidden.
fn group_gff3(records: &[FeatureRecord]) -> Grouping {
    let mut grouping = Grouping::new(records.len());
    // Parent lines by ID; parts are never parents.
    let mut lines = HashMap::<&str, usize>::new();
    for (index, record) in records.iter().enumerate() {
        if search_level(record) != SearchLevel::SubFeature
            && let Some(id) = attribute(record, "ID")
        {
            lines.entry(id).or_insert(index);
        }
    }
    let mut keys = Keys::default();
    for (index, record) in records.iter().enumerate() {
        if search_level(record) != SearchLevel::SubFeature {
            continue;
        }
        let Some(parents) = attribute(record, "Parent") else {
            continue;
        };
        for parent in parents
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            let (group, created) = keys.group(parent, &mut grouping.groups);
            let group = &mut grouping.groups[group];
            if created {
                group.line = lines.get(parent).copied();
            }
            // `Parent=tx1,tx1` names one parent.
            if group.parts.last() != Some(&index) {
                group.parts.push(index);
            }
            grouping.part[index] = true;
        }
    }
    let transcript_lines = grouping
        .groups
        .iter()
        .filter_map(|group| group.line)
        .collect::<HashSet<_>>();
    for (group_index, group) in grouping.groups.iter().enumerate() {
        let Some(parents) = group
            .line
            .and_then(|line| attribute(&records[line], "Parent"))
        else {
            continue;
        };
        for parent in parents.split(',').map(str::trim) {
            if let Some(&container) = lines.get(parent)
                && !transcript_lines.contains(&container)
            {
                grouping.hide[container] = true;
                grouping.parent_gene.entry(group_index).or_insert(container);
            }
        }
    }
    grouping
}

/// Assembles the lines of one contig (in any order). Linear in the number of lines.
pub(super) fn assemble(mut records: Vec<FeatureRecord>, gtf: bool) -> Assembly {
    let count = records.len();
    let Grouping {
        groups,
        part,
        hide,
        parent_gene,
    } = if gtf {
        group_gtf(&records)
    } else {
        group_gff3(&records)
    };
    let mut placements = vec![Placement::Absorbed; count];
    // Moves a line out, leaving an empty record (no allocation) in its place.
    let take = |records: &mut Vec<FeatureRecord>, index: usize| {
        std::mem::replace(&mut records[index], vacant())
    };
    let mut output = Vec::with_capacity(groups.len());
    for (group_index, group) in groups.iter().enumerate() {
        if group.parts.is_empty() {
            // A transcript line with no parts is drawn as it is, below.
            continue;
        }
        // The transcript line is moved into its model; parts and genes are only read.
        let line = group.line.map(|line| take(&mut records, line));
        let gene = parent_gene.get(&group_index).map(|&gene| &records[gene]);
        let parts = group
            .parts
            .iter()
            .map(|&index| &records[index])
            .collect::<Vec<_>>();
        let model = transcript_model(line, &parts, gene, gtf);
        if let Some(index) = group.line {
            placements[index] = Placement::Drawn(output.len());
        }
        output.push(model);
    }
    let mut hidden = Vec::new();
    for index in 0..count {
        if part[index] || placements[index] != Placement::Absorbed {
            // A part, or a transcript line already moved into its model.
            continue;
        }
        if hide[index] {
            placements[index] = Placement::Hidden(hidden.len());
            hidden.push(take(&mut records, index));
        } else {
            placements[index] = Placement::Drawn(output.len());
            output.push(take(&mut records, index));
        }
    }
    // What is left is the folded parts. Freeing them is a noticeable part of a large file's
    // load, so it is done off the loading thread (inline if no thread can be started).
    if count >= BACKGROUND_FREE_LINES {
        let _ = std::thread::Builder::new()
            .name("rgv-free-annotation-lines".to_owned())
            .spawn(move || drop(records));
    }
    Assembly {
        records: output,
        hidden,
        placements,
    }
}

const fn vacant() -> FeatureRecord {
    FeatureRecord {
        contig: String::new(),
        start: 0,
        end: 0,
        name: None,
        feature_type: None,
        score: None,
        strand: super::Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    }
}

fn merged(mut intervals: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    intervals.sort_unstable();
    let mut blocks: Vec<(u64, u64)> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        match blocks.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => blocks.push((start, end)),
        }
    }
    blocks
}

/// Zero-based, half-open.
type Interval = (u64, u64);

/// A transcript's blocks (merged) and coding interval (unclipped) from its parts.
fn blocks_and_coding(parts: &[&FeatureRecord]) -> (Vec<Interval>, Option<Interval>) {
    let mut exons = Vec::new();
    let mut coding_parts = Vec::new();
    let mut others = Vec::new();
    let mut coding: Option<Interval> = None;
    for part in parts {
        let interval = (part.start, part.end);
        let feature_type = part.feature_type.as_deref().unwrap_or_default();
        let is = |types: &[&str]| {
            types
                .iter()
                .any(|candidate| feature_type.eq_ignore_ascii_case(candidate))
        };
        if is(&["exon", "coding_exon", "noncoding_exon"]) {
            exons.push(interval);
        } else if is(&["CDS", "start_codon", "stop_codon"]) {
            coding_parts.push(interval);
            coding = Some(coding.map_or(interval, |(start, end)| {
                (start.min(part.start), end.max(part.end))
            }));
        } else if is(&["UTR", "five_prime_UTR", "three_prime_UTR", "5UTR", "3UTR"]) {
            coding_parts.push(interval);
        } else if !is(&["intron"]) {
            others.push(interval);
        }
    }
    let blocks = merged(if !exons.is_empty() {
        exons
    } else if !coding_parts.is_empty() {
        coding_parts
    } else {
        others
    });
    (blocks, coding)
}

/// Gives a GFF3 transcript its gene's name and identifier when it carries neither: from its
/// own `gene` attribute (as NCBI writes it), or from its parent gene line.
fn add_gene_identity(attributes: &mut Vec<(String, String)>, gene: Option<&FeatureRecord>) {
    let has = |attributes: &[(String, String)], key: &str| {
        attributes
            .iter()
            .any(|(name, value)| name == key && !value.trim().is_empty())
    };
    let gene_name = attributes
        .iter()
        .find(|(key, value)| key == "gene" && !value.trim().is_empty())
        .map(|(_, value)| value.clone())
        .or_else(|| {
            gene.and_then(|gene| {
                attribute(gene, "gene_name")
                    .or_else(|| attribute(gene, "Name"))
                    .map(str::to_owned)
            })
        });
    // Added first: the label is the first naming attribute, and `Name` is usually the
    // transcript's.
    if !has(attributes, "gene_id")
        && let Some(gene_id) = gene.and_then(|gene| attribute(gene, "gene_id"))
    {
        attributes.insert(0, ("gene_id".to_owned(), gene_id.to_owned()));
    }
    if !has(attributes, "gene_name")
        && let Some(gene_name) = gene_name
    {
        attributes.insert(0, ("gene_name".to_owned(), gene_name));
    }
}

fn transcript_model(
    line: Option<FeatureRecord>,
    parts: &[&FeatureRecord],
    gene: Option<&FeatureRecord>,
    gtf: bool,
) -> FeatureRecord {
    let (blocks, coding) = blocks_and_coding(parts);
    let first = parts[0];
    let (mut start, mut end) = line
        .as_ref()
        .map_or((u64::MAX, 0), |line| (line.start, line.end));
    for &(block_start, block_end) in blocks.first().into_iter().chain(blocks.last()) {
        start = start.min(block_start);
        end = end.max(block_end);
    }
    if start >= end {
        // Only introns: the parts' own extent.
        for part in parts {
            start = start.min(part.start);
            end = end.max(part.end);
        }
    }
    let coding_interval = coding
        .map(|(coding_start, coding_end)| (coding_start.max(start), coding_end.min(end)))
        .filter(|(coding_start, coding_end)| coding_start < coding_end);
    let (contig, line_name, score, strand, mut attributes) = match line {
        Some(line) => (
            line.contig,
            line.name,
            line.score,
            line.strand,
            line.attributes,
        ),
        None => (
            first.contig.clone(),
            None,
            parts.iter().find_map(|part| part.score),
            first.strand,
            first
                .attributes
                .iter()
                .filter(|(key, _)| !PART_ONLY_ATTRIBUTES.contains(&key.as_str()))
                .cloned()
                .collect(),
        ),
    };
    if !gtf {
        add_gene_identity(&mut attributes, gene);
    }
    let name = attributes
        .iter()
        .find(|(key, value)| key == "gene_name" && !value.trim().is_empty() && value != ".")
        .map(|(_, value)| value.clone())
        .or(line_name)
        .or_else(|| {
            ["transcript_name", "transcript_id", "Name", "ID", "Parent"]
                .iter()
                .find_map(|key| {
                    attributes
                        .iter()
                        .find(|(name, value)| name == key && !value.trim().is_empty())
                        .map(|(_, value)| value.clone())
                })
        });
    FeatureRecord {
        contig,
        start,
        end,
        name,
        feature_type: Some("transcript".to_owned()),
        score,
        strand,
        blocks,
        coding_interval,
        peak: None,
        attributes,
    }
}

/// The interval a windowed read of `[start, end)` must cover so every transcript model
/// touching the window is complete: the window, widened to the lines that may have parts
/// beyond it, by at most `reach` bases on either side.
pub(super) fn assembly_read_extent(
    records: &[FeatureRecord],
    start: u64,
    end: u64,
    reach: u64,
    gtf: bool,
) -> (u64, u64) {
    let (mut low, mut high) = (start, end);
    for record in records.iter().filter(|record| may_have_parts(record, gtf)) {
        low = low.min(record.start);
        high = high.max(record.end);
    }
    (
        low.max(start.saturating_sub(reach)),
        high.min(end.saturating_add(reach)),
    )
}

#[cfg(test)]
#[path = "gene_model_tests.rs"]
mod tests;
