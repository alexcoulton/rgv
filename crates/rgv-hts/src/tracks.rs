use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read as _},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use bigtools::{BBIFileRead, BigBedRead, BigWigRead, utils::reopen::ReopenableFile};
use flate2::read::MultiGzDecoder;
use rgv_core::{
    CopyNumberSortMode, CopyNumberTrackSettings, InteractionTrackSettings, VariantFieldComparison,
    VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter, VariantMissingValueRule,
    VariantTrackSettings, VariantTypeGroup,
};
use rust_htslib::bcf::{
    self, Read as _,
    header::HeaderRecord,
    record::{GenotypeAllele, Numeric as _},
};
use thiserror::Error;

use crate::{
    HttpRangeConfig, RemoteCancellation, RemoteIoStats, RemoteTransferBudget,
    remote::{ControlledRangeReader, RemoteReaderControl},
};

pub use rgv_model::tracks::*;

const MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS: usize = 100_000;
const BIGBED_BACKWARD_NAVIGATION_WINDOW_BASES: u32 = 1_000_000;
const MAX_GWAS_SEARCH_SCANNED_ASSOCIATIONS: usize = 100_000;

mod remote_tabix;
mod text_feature;
#[cfg(test)]
use text_feature::FeatureIndex;
pub use text_feature::*;
use text_feature::{feature_search_terms, normalized_feature_search_term};

mod text_quantitative;
pub use text_quantitative::*;

mod gwas;
#[cfg(test)]
use gwas::bounded_gwas_records;
pub use gwas::*;

mod copy_number;
pub use copy_number::*;

mod mutations;
pub use mutations::*;

fn track_coordinate_mode(line: &str) -> Result<Option<u8>, String> {
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("coords="));
    match value {
        None => Ok(None),
        Some("0") => Ok(Some(0)),
        Some("1") => Ok(Some(1)),
        Some(value) => Err(format!("track coords must be 0 or 1, not {value}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WigState {
    Fixed {
        contig: String,
        next_one_based: u64,
        step: u64,
        span: u64,
    },
    Variable {
        contig: String,
        span: u64,
    },
}

fn parse_bedgraph(line: &str) -> Result<Option<(String, QuantitativeRecord)>, String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return Err("bedGraph requires chromosome, start, end, and value".to_owned());
    }
    let start = parse_u64(fields[1], "bedGraph start")?;
    let end = parse_u64(fields[2], "bedGraph end")?;
    if start >= end {
        return Err("bedGraph start must be smaller than end".to_owned());
    }
    let value = fields[3]
        .parse::<f32>()
        .map_err(|_| format!("invalid bedGraph value: {}", fields[3]))?;
    Ok(Some((
        fields[0].to_owned(),
        QuantitativeRecord { start, end, value },
    )))
}

fn parse_wig_line(
    line: &str,
    state: &mut Option<WigState>,
) -> Result<Option<(String, QuantitativeRecord)>, String> {
    if line.starts_with("fixedStep") {
        let fields = wig_declaration_fields(line, "fixedStep")?;
        let contig = fields
            .get("chrom")
            .ok_or_else(|| "fixedStep requires chrom".to_owned())?
            .clone();
        let start = parse_required_positive(&fields, "start", "fixedStep")?;
        let step = parse_required_positive(&fields, "step", "fixedStep")?;
        let span = parse_optional_positive(&fields, "span", 1, "fixedStep")?;
        *state = Some(WigState::Fixed {
            contig,
            next_one_based: start,
            step,
            span,
        });
        return Ok(None);
    }
    if line.starts_with("variableStep") {
        let fields = wig_declaration_fields(line, "variableStep")?;
        let contig = fields
            .get("chrom")
            .ok_or_else(|| "variableStep requires chrom".to_owned())?
            .clone();
        let span = parse_optional_positive(&fields, "span", 1, "variableStep")?;
        *state = Some(WigState::Variable { contig, span });
        return Ok(None);
    }
    match state {
        Some(WigState::Fixed {
            contig,
            next_one_based,
            step,
            span,
        }) => {
            let value = line
                .parse::<f32>()
                .map_err(|_| format!("invalid fixedStep value: {line}"))?;
            let start = next_one_based
                .checked_sub(1)
                .ok_or_else(|| "fixedStep start must be one or greater".to_owned())?;
            let end = start
                .checked_add(*span)
                .ok_or_else(|| "fixedStep interval overflow".to_owned())?;
            *next_one_based = next_one_based
                .checked_add(*step)
                .ok_or_else(|| "fixedStep coordinate overflow".to_owned())?;
            Ok(Some((
                contig.clone(),
                QuantitativeRecord { start, end, value },
            )))
        }
        Some(WigState::Variable { contig, span }) => {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err("variableStep data requires position and value".to_owned());
            }
            let one_based = parse_u64(fields[0], "variableStep position")?;
            let start = one_based
                .checked_sub(1)
                .ok_or_else(|| "variableStep position must be one or greater".to_owned())?;
            let end = start
                .checked_add(*span)
                .ok_or_else(|| "variableStep interval overflow".to_owned())?;
            let value = fields[1]
                .parse::<f32>()
                .map_err(|_| format!("invalid variableStep value: {}", fields[1]))?;
            Ok(Some((
                contig.clone(),
                QuantitativeRecord { start, end, value },
            )))
        }
        None => Err("WIG data appears before a fixedStep or variableStep declaration".to_owned()),
    }
}

fn wig_declaration_fields(line: &str, kind: &str) -> Result<HashMap<String, String>, String> {
    let mut fields = HashMap::new();
    let mut words = line.split_whitespace();
    if words.next() != Some(kind) {
        return Err(format!("invalid {kind} declaration"));
    }
    for word in words {
        let (key, value) = word
            .split_once('=')
            .ok_or_else(|| format!("invalid {kind} field: {word}"))?;
        if key.is_empty()
            || value.is_empty()
            || fields.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return Err(format!("invalid or duplicate {kind} field: {word}"));
        }
    }
    Ok(fields)
}

fn parse_required_positive(
    fields: &HashMap<String, String>,
    key: &str,
    kind: &str,
) -> Result<u64, String> {
    let value = fields
        .get(key)
        .ok_or_else(|| format!("{kind} requires {key}"))?;
    parse_optional_positive(fields, key, 0, kind)
        .map_err(|_| format!("invalid {kind} {key}: {value}"))
}

fn parse_optional_positive(
    fields: &HashMap<String, String>,
    key: &str,
    default: u64,
    kind: &str,
) -> Result<u64, String> {
    let value = fields.get(key).map_or(Ok(default), |raw| {
        raw.parse::<u64>()
            .map_err(|_| format!("invalid {kind} {key}: {raw}"))
    })?;
    if value == 0 {
        return Err(format!("{kind} {key} must be positive"));
    }
    Ok(value)
}

mod interactions;
pub use interactions::*;

mod variants;
pub use variants::*;
#[cfg(test)]
use variants::{
    classify_variant, parse_breakend_partner, parse_breakpoint_orientations,
    selected_sample_indices,
};

mod bbi;
pub use bbi::*;
#[cfg(test)]
use bbi::{
    ensure_bigbed_query_capacity, parse_big_gene_pred_record, parse_bigbed_record,
    retain_quantitative_record,
};

fn is_remote_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn parse_bed(line: &str) -> Result<FeatureRecord, String> {
    // BED columns are whitespace-separated. Tabs win when present so names containing spaces
    // survive; a file written with spaces only is still read.
    let fields = if line.contains('\t') {
        line.split('\t').map(str::trim).collect::<Vec<_>>()
    } else {
        line.split_whitespace().collect::<Vec<_>>()
    };
    if fields.len() < 3 {
        return Err("BED requires at least three whitespace-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "BED start")?;
    let end = parse_u64(fields[2], "BED end")?;
    if start >= end {
        return Err("BED start must be less than end".to_owned());
    }
    let blocks = parse_bed_blocks(&fields, start)?;
    let coding_interval = if fields.len() >= 8 {
        let coding_start = parse_u64(fields[6], "BED thick start")?;
        let coding_end = parse_u64(fields[7], "BED thick end")?;
        // An empty thick interval is how BED says "no thick part"; writers put it anywhere,
        // commonly at 0/0 or at the feature end, so its position is not validated.
        if coding_start >= coding_end {
            None
        } else if coding_start < start || coding_end > end {
            return Err("BED thick interval must be contained within the feature".to_owned());
        } else {
            Some((coding_start, coding_end))
        }
    } else {
        None
    };
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start,
        end,
        name: fields
            .get(3)
            .filter(|value| **value != ".")
            .map(ToString::to_string),
        score: fields.get(4).and_then(|value| value.parse().ok()),
        strand: fields
            .get(5)
            .map_or(Strand::Unstranded, |value| parse_strand(value)),
        feature_type: None,
        blocks,
        coding_interval,
        peak: None,
        attributes: Vec::new(),
    })
}

fn parse_bed_blocks(fields: &[&str], feature_start: u64) -> Result<Vec<(u64, u64)>, String> {
    if fields.len() < 12 {
        return Ok(Vec::new());
    }
    let count = fields[9]
        .parse::<usize>()
        .map_err(|_| "invalid BED block count".to_owned())?;
    if count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "BED block count exceeds the {MAX_FEATURE_BLOCKS_PER_RECORD}-block limit"
        ));
    }
    let sizes = comma_numbers(fields[10], "BED block size")?;
    let starts = comma_numbers(fields[11], "BED block start")?;
    if count != sizes.len() || count != starts.len() {
        return Err("BED block count does not match block arrays".to_owned());
    }
    Ok(sizes
        .into_iter()
        .zip(starts)
        .map(|(size, start)| {
            let start = feature_start.saturating_add(start);
            (start, start.saturating_add(size))
        })
        .collect())
}

fn parse_gff(line: &str, gtf: bool) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 9 {
        return Err("GFF/GTF requires nine tab-separated columns".to_owned());
    }
    let one_based_start = parse_u64(fields[3], "GFF start")?;
    let end = parse_u64(fields[4], "GFF end")?;
    if one_based_start == 0 || one_based_start > end {
        return Err("GFF coordinates must be one-based and forward".to_owned());
    }
    let attributes = parse_attributes(fields[8], gtf);
    let name = attribute_value(&attributes, &["Name", "gene_name", "gene_id", "ID"]);
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start: one_based_start - 1,
        end,
        name,
        feature_type: Some(fields[2].to_owned()),
        score: (fields[5] != ".").then(|| fields[5].parse().ok()).flatten(),
        strand: parse_strand(fields[6]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes,
    })
}

#[allow(clippy::too_many_lines)]
fn parse_gene_pred(line: &str, ref_flat: bool) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    let minimum_fields = if ref_flat { 11 } else { 10 };
    let extended_fields = if ref_flat { None } else { Some(15) };
    let has_bin = fields.len() == minimum_fields + 1
        || extended_fields.is_some_and(|count| fields.len() == count + 1);
    let offset = usize::from(has_bin);
    let available = fields.len().saturating_sub(offset);
    if available != minimum_fields && extended_fields != Some(available) {
        return Err(if ref_flat {
            "refFlat requires 11 tab-separated columns, with an optional leading bin".to_owned()
        } else {
            "genePred requires 10 or 15 tab-separated columns, with an optional leading bin"
                .to_owned()
        });
    }
    if has_bin {
        parse_u64(fields[0], "genePred bin")?;
    }
    let gene_name = ref_flat.then_some(fields[offset]);
    let transcript_column = offset + usize::from(ref_flat);
    let transcript_name = fields[transcript_column];
    let contig = fields[transcript_column + 1];
    let strand = parse_strand(fields[transcript_column + 2]);
    if strand == Strand::Unstranded {
        return Err("genePred strand must be + or -".to_owned());
    }
    let start = parse_u64(fields[transcript_column + 3], "genePred transcript start")?;
    let end = parse_u64(fields[transcript_column + 4], "genePred transcript end")?;
    let coding_start = parse_u64(fields[transcript_column + 5], "genePred CDS start")?;
    let coding_end = parse_u64(fields[transcript_column + 6], "genePred CDS end")?;
    if start >= end {
        return Err("genePred transcript start must be less than end".to_owned());
    }
    if coding_start < start || coding_end > end || coding_start > coding_end {
        return Err("genePred CDS interval must be contained within the transcript".to_owned());
    }
    let exon_count = fields[transcript_column + 7]
        .parse::<usize>()
        .map_err(|_| "invalid genePred exon count".to_owned())?;
    if exon_count == 0 || exon_count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "genePred exon count must be between 1 and {MAX_FEATURE_BLOCKS_PER_RECORD}"
        ));
    }
    let exon_starts = comma_numbers(fields[transcript_column + 8], "genePred exon start")?;
    let exon_ends = comma_numbers(fields[transcript_column + 9], "genePred exon end")?;
    if exon_starts.len() != exon_count || exon_ends.len() != exon_count {
        return Err("genePred exon count does not match exon arrays".to_owned());
    }
    let mut blocks = Vec::with_capacity(exon_count);
    for (exon_start, exon_end) in exon_starts.into_iter().zip(exon_ends) {
        if exon_start < start || exon_start >= exon_end || exon_end > end {
            return Err(
                "genePred exon must be non-empty and contained within the transcript".to_owned(),
            );
        }
        if blocks
            .last()
            .is_some_and(|(_, previous_end)| exon_start < *previous_end)
        {
            return Err("genePred exons must be sorted and non-overlapping".to_owned());
        }
        blocks.push((exon_start, exon_end));
    }
    let mut attributes = vec![
        ("transcript_id".to_owned(), transcript_name.to_owned()),
        (
            "source_format".to_owned(),
            if ref_flat { "refFlat" } else { "genePred" }.to_owned(),
        ),
    ];
    if let Some(gene_name) = gene_name {
        attributes.push(("gene_name".to_owned(), gene_name.to_owned()));
    }
    let mut extended_gene_name = None;
    let score = if !ref_flat && available == 15 {
        let score_column = transcript_column + 10;
        let score = fields[score_column]
            .parse::<i32>()
            .map_err(|_| "invalid extended genePred score".to_owned())?;
        for (label, value) in [
            ("CDS start status", fields[score_column + 2]),
            ("CDS end status", fields[score_column + 3]),
        ] {
            if !matches!(value, "none" | "unk" | "incmpl" | "cmpl") {
                return Err(format!("invalid extended genePred {label}: {value}"));
            }
        }
        let exon_frames = fields[score_column + 4]
            .trim_end_matches(',')
            .split(',')
            .map(|value| {
                value
                    .parse::<i8>()
                    .map_err(|_| format!("invalid extended genePred exon frame: {value}"))
                    .and_then(|value| {
                        matches!(value, -1..=2)
                            .then_some(value)
                            .ok_or_else(|| format!("invalid extended genePred exon frame: {value}"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if exon_frames.len() != exon_count {
            return Err("extended genePred exon frame count does not match exon count".to_owned());
        }
        let parsed_gene_name = fields[score_column + 1];
        if !parsed_gene_name.is_empty() && parsed_gene_name != "." {
            extended_gene_name = Some(parsed_gene_name);
        }
        attributes.extend([
            ("gene_name".to_owned(), parsed_gene_name.to_owned()),
            (
                "cds_start_status".to_owned(),
                fields[score_column + 2].to_owned(),
            ),
            (
                "cds_end_status".to_owned(),
                fields[score_column + 3].to_owned(),
            ),
            (
                "exon_frames".to_owned(),
                fields[score_column + 4].to_owned(),
            ),
        ]);
        Some(score as f32)
    } else {
        None
    };
    Ok(FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: Some(
            gene_name
                .filter(|value| !value.is_empty() && *value != ".")
                .or(extended_gene_name)
                .unwrap_or(transcript_name)
                .to_owned(),
        ),
        feature_type: Some("transcript".to_owned()),
        score,
        strand,
        blocks,
        coding_interval: (coding_start < coding_end).then_some((coding_start, coding_end)),
        peak: None,
        attributes,
    })
}

fn parse_narrow_peak(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 10 {
        return Err("narrowPeak requires exactly 10 tab-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "narrowPeak start")?;
    let end = parse_u64(fields[2], "narrowPeak end")?;
    if start >= end {
        return Err("narrowPeak start must be less than end".to_owned());
    }
    parse_narrow_peak_fields(fields[0], start, end, &fields[3..], "narrowPeak")
}

fn parse_broad_peak(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 9 {
        return Err("broadPeak requires exactly 9 tab-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "broadPeak start")?;
    let end = parse_u64(fields[2], "broadPeak end")?;
    if start >= end {
        return Err("broadPeak start must be less than end".to_owned());
    }
    let score = fields[4]
        .parse::<u16>()
        .ok()
        .filter(|score| *score <= 1000)
        .ok_or_else(|| "broadPeak score must be an integer between 0 and 1000".to_owned())?;
    if !matches!(fields[5], "+" | "-" | ".") {
        return Err("broadPeak strand must be +, -, or .".to_owned());
    }
    let signal_value = parse_finite_f32(fields[6], "broadPeak signal value")?;
    let p_value = parse_optional_peak_value(fields[7], "broadPeak p-value")?;
    let q_value = parse_optional_peak_value(fields[8], "broadPeak q-value")?;
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start,
        end,
        name: (fields[3] != ".").then(|| fields[3].to_owned()),
        feature_type: Some("broadPeak".to_owned()),
        score: Some(f32::from(score)),
        strand: parse_strand(fields[5]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: Some(PeakMetrics {
            signal_value,
            p_value,
            q_value,
            summit: None,
        }),
        attributes: vec![("source_format".to_owned(), "broadPeak".to_owned())],
    })
}

fn is_psl_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("psLayout ")
        || trimmed.starts_with("match ")
        || trimmed.chars().all(|character| {
            character.is_ascii_whitespace() || matches!(character, '-' | '+' | '|')
        })
}

#[allow(clippy::too_many_lines)]
fn parse_psl(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let has_bin = fields.len() == 22;
    if fields.len() != 21 && !has_bin {
        return Err(
            "PSL requires exactly 21 whitespace-separated columns, with an optional leading bin"
                .to_owned(),
        );
    }
    let offset = usize::from(has_bin);
    if has_bin {
        parse_u64(fields[0], "PSL bin")?;
    }
    let number = |column: usize, label: &str| parse_u64(fields[offset + column], label);
    let matches = number(0, "PSL matches")?;
    let mismatches = number(1, "PSL mismatches")?;
    let repeat_matches = number(2, "PSL repeat matches")?;
    let n_count = number(3, "PSL N count")?;
    let query_insertions = number(4, "PSL query insertion count")?;
    let query_inserted_bases = number(5, "PSL query inserted bases")?;
    let target_insertions = number(6, "PSL target insertion count")?;
    let target_inserted_bases = number(7, "PSL target inserted bases")?;
    let raw_strand = fields[offset + 8];
    if raw_strand.is_empty()
        || raw_strand.len() > 2
        || !raw_strand.bytes().all(|value| matches!(value, b'+' | b'-'))
    {
        return Err("PSL strand must contain one or two + or - characters".to_owned());
    }
    let query_reverse = raw_strand.as_bytes()[0] == b'-';
    let target_reverse = raw_strand.as_bytes().get(1) == Some(&b'-');
    let query_name = fields[offset + 9];
    if query_name.is_empty() || query_name == "." {
        return Err("PSL query name must not be empty".to_owned());
    }
    let query_size = number(10, "PSL query size")?;
    let query_start = number(11, "PSL query start")?;
    let query_end = number(12, "PSL query end")?;
    let target_name = fields[offset + 13];
    let target_size = number(14, "PSL target size")?;
    let target_start = number(15, "PSL target start")?;
    let target_end = number(16, "PSL target end")?;
    if query_start >= query_end || query_end > query_size {
        return Err("PSL query interval must be non-empty and contained by qSize".to_owned());
    }
    if target_start >= target_end || target_end > target_size {
        return Err("PSL target interval must be non-empty and contained by tSize".to_owned());
    }
    let block_count = usize::try_from(number(17, "PSL block count")?)
        .map_err(|_| "PSL block count cannot be represented".to_owned())?;
    if block_count == 0 || block_count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "PSL block count must be between 1 and {MAX_FEATURE_BLOCKS_PER_RECORD}"
        ));
    }
    let block_sizes = comma_numbers_exact(fields[offset + 18], "PSL block size", block_count)?;
    let query_starts =
        comma_numbers_exact(fields[offset + 19], "PSL query block start", block_count)?;
    let target_starts =
        comma_numbers_exact(fields[offset + 20], "PSL target block start", block_count)?;
    let block_bases = block_sizes.iter().try_fold(0_u64, |total, size| {
        total
            .checked_add(*size)
            .ok_or_else(|| "PSL block bases overflow their coordinate range".to_owned())
    })?;
    let query_span = query_end - query_start;
    let target_span = target_end - target_start;
    let nucleotide_query_span = block_bases
        .checked_add(query_inserted_bases)
        .ok_or_else(|| "PSL query span overflows its coordinate range".to_owned())?;
    let nucleotide_target_span = block_bases
        .checked_add(target_inserted_bases)
        .ok_or_else(|| "PSL target span overflows its coordinate range".to_owned())?;
    if query_span != nucleotide_query_span || target_span != nucleotide_target_span {
        let translated_block_bases = block_bases.checked_mul(3).ok_or_else(|| {
            "PSL translated block bases overflow their coordinate range".to_owned()
        })?;
        let translated_query_span = translated_block_bases
            .checked_add(query_inserted_bases)
            .ok_or_else(|| "PSL translated query span overflows".to_owned())?;
        let translated_target_span = translated_block_bases
            .checked_add(target_inserted_bases)
            .ok_or_else(|| "PSL translated target span overflows".to_owned())?;
        if query_span == translated_query_span || target_span == translated_target_span {
            return Err(
                "translated protein PSL is not supported by the nucleotide block model".to_owned(),
            );
        }
        return Err("PSL block and insertion sizes do not match the declared spans".to_owned());
    }
    let mut blocks = Vec::with_capacity(block_count);
    for ((size, query_block_start), target_block_start) in block_sizes
        .iter()
        .copied()
        .zip(query_starts.iter().copied())
        .zip(target_starts.iter().copied())
    {
        if size == 0 {
            return Err("PSL blocks must be non-empty".to_owned());
        }
        let query_block_end = query_block_start
            .checked_add(size)
            .ok_or_else(|| "PSL query block overflows its coordinate range".to_owned())?;
        let positive_query_start = if query_reverse {
            query_size.checked_sub(query_block_end).ok_or_else(|| {
                "PSL reverse query block falls outside the query sequence".to_owned()
            })?
        } else {
            query_block_start
        };
        if positive_query_start < query_start
            || positive_query_start
                .checked_add(size)
                .is_none_or(|end| end > query_end)
        {
            return Err("PSL query block must be contained by the query interval".to_owned());
        }
        let target_block_end = target_block_start
            .checked_add(size)
            .ok_or_else(|| "PSL target block overflows its coordinate range".to_owned())?;
        let positive_target_start = if target_reverse {
            target_size.checked_sub(target_block_end).ok_or_else(|| {
                "PSL reverse target block falls outside the target sequence".to_owned()
            })?
        } else {
            target_block_start
        };
        let positive_target_end = positive_target_start
            .checked_add(size)
            .ok_or_else(|| "PSL target block overflows its coordinate range".to_owned())?;
        if positive_target_start < target_start || positive_target_end > target_end {
            return Err(
                "PSL target block must be contained by the declared target interval; translated protein PSL is not supported"
                    .to_owned(),
            );
        }
        blocks.push((positive_target_start, positive_target_end));
    }
    blocks.sort_unstable();
    if blocks
        .windows(2)
        .any(|adjacent| adjacent[1].0 < adjacent[0].1)
    {
        return Err("PSL target blocks must not overlap".to_owned());
    }
    let total_compared = matches
        .saturating_add(mismatches)
        .saturating_add(repeat_matches)
        .saturating_add(n_count);
    let score = (total_compared > 0).then(|| {
        (matches.saturating_add(repeat_matches) as f64 * 1000.0 / total_compared as f64) as f32
    });
    let strand = if query_reverse ^ target_reverse {
        Strand::Reverse
    } else {
        Strand::Forward
    };
    let attributes = [
        ("source_format", "PSL".to_owned()),
        ("matches", matches.to_string()),
        ("mismatches", mismatches.to_string()),
        ("repeat_matches", repeat_matches.to_string()),
        ("n_count", n_count.to_string()),
        ("query_insertions", query_insertions.to_string()),
        ("query_inserted_bases", query_inserted_bases.to_string()),
        ("target_insertions", target_insertions.to_string()),
        ("target_inserted_bases", target_inserted_bases.to_string()),
        ("query_name", query_name.to_owned()),
        ("query_size", query_size.to_string()),
        ("query_start", query_start.to_string()),
        ("query_end", query_end.to_string()),
        ("target_size", target_size.to_string()),
        ("raw_strand", raw_strand.to_owned()),
        ("block_sizes", fields[offset + 18].to_owned()),
        ("query_starts", fields[offset + 19].to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    Ok(FeatureRecord {
        contig: target_name.to_owned(),
        start: target_start,
        end: target_end,
        name: Some(query_name.to_owned()),
        feature_type: Some("PSL alignment".to_owned()),
        score,
        strand,
        blocks,
        coding_interval: None,
        peak: None,
        attributes,
    })
}

fn parse_narrow_peak_fields(
    contig: &str,
    start: u64,
    end: u64,
    fields: &[&str],
    source_format: &str,
) -> Result<FeatureRecord, String> {
    if fields.len() != 7 {
        return Err(format!(
            "{source_format} requires exactly seven fields after its coordinates"
        ));
    }
    let score = fields[1]
        .parse::<f32>()
        .map_err(|_| format!("invalid {source_format} score"))?;
    if !score.is_finite() || !(0.0..=1000.0).contains(&score) {
        return Err(format!(
            "{source_format} score must be finite and between 0 and 1000"
        ));
    }
    let signal_value = parse_finite_f32(fields[3], &format!("{source_format} signal value"))?;
    let p_value = parse_optional_peak_value(fields[4], &format!("{source_format} p-value"))?;
    let q_value = parse_optional_peak_value(fields[5], &format!("{source_format} q-value"))?;
    let peak_offset = fields[6]
        .parse::<i64>()
        .map_err(|_| format!("invalid {source_format} summit offset"))?;
    let summit = if peak_offset == -1 {
        None
    } else {
        let peak_offset = u64::try_from(peak_offset)
            .map_err(|_| format!("{source_format} summit offset must be -1 or non-negative"))?;
        if peak_offset >= end - start {
            return Err(format!(
                "{source_format} summit offset must fall inside the peak interval"
            ));
        }
        Some(start + peak_offset)
    };
    Ok(FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: (fields[0] != ".").then(|| fields[0].to_owned()),
        feature_type: Some(source_format.to_owned()),
        score: Some(score),
        strand: parse_strand(fields[2]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: Some(PeakMetrics {
            signal_value,
            p_value,
            q_value,
            summit,
        }),
        attributes: if source_format == "narrowPeak" {
            Vec::new()
        } else {
            vec![("source_format".to_owned(), source_format.to_owned())]
        },
    })
}

fn parse_finite_f32(raw: &str, label: &str) -> Result<f32, String> {
    let value = raw
        .parse::<f32>()
        .map_err(|_| format!("invalid {label}: {raw}"))?;
    if !value.is_finite() {
        return Err(format!("{label} must be finite"));
    }
    Ok(value)
}

fn parse_optional_peak_value(raw: &str, label: &str) -> Result<Option<f32>, String> {
    let value = parse_finite_f32(raw, label)?;
    if value.to_bits() == (-1.0_f32).to_bits() {
        Ok(None)
    } else if value >= 0.0 {
        Ok(Some(value))
    } else {
        Err(format!("{label} must be -1 or non-negative"))
    }
}

fn parse_attributes(raw: &str, gtf: bool) -> Vec<(String, String)> {
    raw.split(';')
        .filter_map(|field| {
            let field = field.trim();
            if field.is_empty() {
                return None;
            }
            if gtf {
                let pair = field.split_once(char::is_whitespace)?;
                return Some((
                    pair.0.trim().to_owned(),
                    pair.1.trim().trim_matches('"').to_owned(),
                ));
            }
            // GFF3 escapes `;`, `=`, `,`, `%`, tabs and control characters as `%XX` in both
            // tags and values. Decoding happens after the `;`/`=` split, as the format requires.
            let pair = field.split_once('=')?;
            Some((
                percent_decode_gff3(pair.0.trim()),
                percent_decode_gff3(pair.1.trim().trim_matches('"')),
            ))
        })
        .collect()
}

/// Decodes GFF3 `%XX` escapes. Malformed escapes and sequences that do not form UTF-8 are
/// kept verbatim, so a stray `%` never loses data.
fn percent_decode_gff3(raw: &str) -> String {
    if !raw.contains('%') {
        return raw.to_owned();
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        if let Some(byte) = escape {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| raw.to_owned())
}

fn attribute_value(attributes: &[(String, String)], keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        attributes
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    })
}

fn comma_numbers(raw: &str, label: &str) -> Result<Vec<u64>, String> {
    raw.trim_end_matches(',')
        .split(',')
        .map(|value| parse_u64(value, label))
        .collect()
}

fn comma_numbers_exact(raw: &str, label: &str, expected: usize) -> Result<Vec<u64>, String> {
    let mut values = Vec::with_capacity(expected);
    for raw_value in raw.trim_end_matches(',').split(',') {
        if values.len() == expected {
            return Err(format!("{label} count does not match the declared count"));
        }
        values.push(parse_u64(raw_value, label)?);
    }
    if values.len() != expected {
        return Err(format!("{label} count does not match the declared count"));
    }
    Ok(values)
}

fn parse_u64(raw: &str, label: &str) -> Result<u64, String> {
    raw.parse().map_err(|_| format!("invalid {label}: {raw}"))
}

fn parse_strand(raw: &str) -> Strand {
    match raw {
        "+" => Strand::Forward,
        "-" => Strand::Reverse,
        _ => Strand::Unstranded,
    }
}

#[derive(Debug, Error)]
pub enum TrackError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("malformed track line {line}: {message}")]
    MalformedLine { line: usize, message: String },
    #[error("malformed indexed track record at {contig}:{start}: {message}")]
    MalformedRecord {
        contig: String,
        start: u64,
        message: String,
    },
    #[error("invalid track schema: {0}")]
    InvalidSchema(String),
    #[error("track coordinate cannot be represented by the backend")]
    Coordinate,
    #[error("track backend error: {0}")]
    Backend(String),
    #[error("track resource limit: {0}")]
    ResourceLimit(String),
}

#[cfg(test)]
#[path = "tracks_tests.rs"]
mod tests;
