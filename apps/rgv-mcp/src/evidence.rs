//! Evidence queries answered from the viewer's own structured exports.
//!
//! The control protocol has no query commands for pileup columns or variant records, but its
//! `export` command writes both for the active panel's current interval. The query tools export
//! to a temporary artifact and reshape that text here into small, paginated JSON. Nothing in this
//! module talks to the viewer, so the reshaping is tested directly against export text.
//!
//! Export rows use one-based `start` and one-based inclusive `end` (stated in the export's own
//! `# coordinates` header). Everything returned from here is zero-based and half-open, like every
//! other numeric coordinate the MCP tools accept and return.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use thiserror::Error;

pub(crate) const DEFAULT_PILEUP_LIMIT: usize = 50;
pub(crate) const MAXIMUM_PILEUP_LIMIT: usize = 500;
pub(crate) const DEFAULT_VARIANT_LIMIT: usize = 25;
pub(crate) const MAXIMUM_VARIANT_LIMIT: usize = 200;
/// Longest free-text value (allele, INFO value, FORMAT string) returned for one field.
const MAXIMUM_FIELD_BYTES: usize = 256;
/// Most INFO fields and most genotypes returned for one variant record.
const MAXIMUM_NESTED_ENTRIES: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum EvidenceError {
    /// The export text does not have the shape this adapter understands.
    #[error("RGV returned an evidence export this adapter cannot interpret: {0}")]
    Malformed(String),
    /// The requested interval is not inside the interval the viewer exported.
    #[error("{0}")]
    NotInView(String),
    #[error("{0}")]
    InvalidInput(String),
}

impl EvidenceError {
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "invalid_response",
            Self::NotInView(_) => "interval_not_in_view",
            Self::InvalidInput(_) => "invalid_input",
        }
    }
}

/// The interval a query asks about, zero-based and half-open, with its paging window.
#[derive(Clone, Debug, Default)]
pub(crate) struct EvidenceQuery {
    pub(crate) contig: Option<String>,
    pub(crate) start: Option<u64>,
    pub(crate) end: Option<u64>,
    pub(crate) track_ids: Option<Vec<u64>>,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VariantDetail {
    pub(crate) include_info: bool,
    pub(crate) include_genotypes: bool,
    pub(crate) include_sample_names: bool,
}

/// What the export's `#` header lines say about the interval it covers.
#[derive(Debug, PartialEq, Eq)]
struct ExportScope {
    genome: String,
    contig: String,
    /// Zero-based inclusive start of the exported interval.
    start: u64,
    /// Zero-based exclusive end of the exported interval.
    end: u64,
    /// The viewer stopped writing at its export byte limit.
    truncated: bool,
}

fn export_scope(text: &str) -> Result<ExportScope, EvidenceError> {
    let mut genome = None;
    let mut locus = None;
    let mut truncated = false;
    for line in text.lines().filter(|line| line.starts_with('#')) {
        if let Some(value) = line.strip_prefix("# genome\t") {
            genome = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("# locus\t") {
            locus = Some(value);
        } else if line.starts_with("# export_truncated") {
            truncated = true;
        }
    }
    let malformed = || EvidenceError::Malformed("the `# locus` header is missing".to_owned());
    // The contig may itself contain colons and hyphens; the range is everything after the last
    // colon and is always `<one-based start>-<one-based inclusive end>`.
    let (contig, range) = locus
        .ok_or_else(malformed)?
        .rsplit_once(':')
        .ok_or_else(malformed)?;
    let (start, end) = range.split_once('-').ok_or_else(malformed)?;
    let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
        return Err(malformed());
    };
    if start == 0 || end < start {
        return Err(malformed());
    }
    Ok(ExportScope {
        genome: genome.unwrap_or_default(),
        contig: contig.to_owned(),
        start: start - 1,
        end,
        truncated,
    })
}

/// Resolves the requested interval against the exported one. The tools never navigate, so an
/// interval the viewer did not export is an error rather than a silently partial answer.
fn resolve_interval(
    query: &EvidenceQuery,
    scope: &ExportScope,
) -> Result<(u64, u64), EvidenceError> {
    if let Some(contig) = &query.contig
        && contig != &scope.contig
    {
        return Err(EvidenceError::NotInView(format!(
            "the active panel shows contig {}, not {contig}; navigate there first with rgv_navigate",
            scope.contig
        )));
    }
    let start = query.start.unwrap_or(scope.start);
    let end = query.end.unwrap_or_else(|| {
        query
            .start
            .map_or(scope.end, |start| start.saturating_add(1))
    });
    if start >= end {
        return Err(EvidenceError::InvalidInput(
            "end must be greater than start (zero-based, half-open interval)".to_owned(),
        ));
    }
    if start < scope.start || end > scope.end {
        return Err(EvidenceError::NotInView(format!(
            "the requested interval {}:{start}-{end} (zero-based, half-open) is not inside the \
             active panel's current view {}:{}-{}; navigate there first with rgv_navigate",
            scope.contig, scope.contig, scope.start, scope.end
        )));
    }
    Ok((start, end))
}

fn check_page(query: &EvidenceQuery, maximum: usize) -> Result<(), EvidenceError> {
    if (1..=maximum).contains(&query.limit) {
        Ok(())
    } else {
        Err(EvidenceError::InvalidInput(format!(
            "limit must be between 1 and {maximum}"
        )))
    }
}

/// Parses a `# track_provenance\t<id>\t<name>\tkey=value...` line.
fn track_provenance(line: &str) -> Option<(u64, Value)> {
    let mut cells = line.strip_prefix("# track_provenance\t")?.split('\t');
    let track_id = cells.next()?.parse::<u64>().ok()?;
    let mut track = Map::new();
    track.insert("track_id".to_owned(), json!(track_id));
    track.insert("track_name".to_owned(), json!(cells.next()?));
    for cell in cells {
        if let Some((key, value)) = cell.split_once('=') {
            let value = value
                .parse::<u64>()
                .map(Value::from)
                .or_else(|_| value.parse::<bool>().map(Value::from))
                .unwrap_or_else(|_| json!(value));
            track.insert(key.to_owned(), value);
        }
    }
    Some((track_id, Value::Object(track)))
}

fn wanted(track_ids: Option<&Vec<u64>>, track_id: u64) -> bool {
    track_ids.is_none_or(|ids| ids.contains(&track_id))
}

/// Shared paging fields. `truncated` is true whenever matching rows exist beyond this page.
fn page(total: usize, offset: usize, returned: usize) -> Value {
    let next = offset.saturating_add(returned);
    json!({
        "total_matching": total,
        "offset": offset,
        "returned": returned,
        "truncated": next < total,
        "next_offset": (next < total).then_some(next),
    })
}

fn bounded(text: &str) -> (String, bool) {
    if text.len() <= MAXIMUM_FIELD_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAXIMUM_FIELD_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

fn cell<'a>(
    cells: &[&'a str],
    columns: &HashMap<&str, usize>,
    name: &str,
) -> Result<&'a str, EvidenceError> {
    columns
        .get(name)
        .and_then(|index| cells.get(*index))
        .copied()
        .ok_or_else(|| EvidenceError::Malformed(format!("a row has no `{name}` column")))
}

fn number(
    cells: &[&str],
    columns: &HashMap<&str, usize>,
    name: &str,
) -> Result<u64, EvidenceError> {
    cell(cells, columns, name)?
        .parse()
        .map_err(|_| EvidenceError::Malformed(format!("`{name}` is not an unsigned integer")))
}

/// A numeric cell that the viewer writes as `.` when it has no value.
fn optional_decimal(text: &str) -> Value {
    text.parse::<f64>().map_or(Value::Null, Value::from)
}

/// Reshapes a `pileup` export into per-base sites for the requested interval.
pub(crate) fn pileup_result(text: &str, query: &EvidenceQuery) -> Result<Value, EvidenceError> {
    check_page(query, MAXIMUM_PILEUP_LIMIT)?;
    let scope = export_scope(text)?;
    let (start, end) = resolve_interval(query, &scope)?;
    let mut columns = HashMap::new();
    let mut tracks = Vec::new();
    let mut sites = Vec::new();
    let mut total = 0_usize;
    for line in text.lines() {
        if let Some((track_id, track)) = track_provenance(line) {
            if wanted(query.track_ids.as_ref(), track_id) {
                tracks.push(track);
            }
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let cells = line.split('\t').collect::<Vec<_>>();
        if columns.is_empty() {
            columns = cells
                .iter()
                .enumerate()
                .map(|(index, name)| (*name, index))
                .collect();
            continue;
        }
        let track_id = number(&cells, &columns, "track_id")?;
        let one_based = number(&cells, &columns, "start")?;
        let position = one_based
            .checked_sub(1)
            .ok_or_else(|| EvidenceError::Malformed("a pileup row starts at 0".to_owned()))?;
        if !wanted(query.track_ids.as_ref(), track_id) || position < start || position >= end {
            continue;
        }
        total += 1;
        if total <= query.offset || sites.len() >= query.limit {
            continue;
        }
        let count = |name| number(&cells, &columns, name);
        sites.push(json!({
            "track_id": track_id,
            "position": position,
            "depth": count("depth")?,
            "forward_depth": count("forward_depth")?,
            "reverse_depth": count("reverse_depth")?,
            "counts": {
                "A": count("A")?,
                "C": count("C")?,
                "G": count("G")?,
                "T": count("T")?,
                "N": count("N")?,
                "insertion": count("insertion")?,
                "deletion": count("deletion")?,
            },
            "mean_base_quality": optional_decimal(cell(&cells, &columns, "mean_base_quality")?),
            "mean_mapping_quality":
                optional_decimal(cell(&cells, &columns, "mean_mapping_quality")?),
        }));
    }
    if columns.is_empty() {
        return Err(EvidenceError::Malformed(
            "the pileup column header is missing".to_owned(),
        ));
    }
    let returned = sites.len();
    Ok(json!({
        "genome": scope.genome,
        "contig": scope.contig,
        "coordinates": "zero-based; intervals are half-open [start, end)",
        "query_start": start,
        "query_end": end,
        "view_start": scope.start,
        "view_end": scope.end,
        "zero_depth_sites_omitted": true,
        "tracks": tracks,
        "sites": sites,
        "page": page(total, query.offset, returned),
        "source_export_truncated": scope.truncated,
    }))
}

/// Replaces sample names by `sample_1`, `sample_2`, ... in first-appearance order over the whole
/// export, so a sample keeps its pseudonym on every page of the same query.
#[derive(Default)]
struct SamplePseudonyms {
    assigned: HashMap<String, usize>,
}

impl SamplePseudonyms {
    fn name(&mut self, sample: &str) -> String {
        let next = self.assigned.len() + 1;
        let index = *self.assigned.entry(sample.to_owned()).or_insert(next);
        format!("sample_{index}")
    }
}

fn list(text: &str, separator: char) -> Vec<Value> {
    text.split(separator)
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| json!(bounded(part).0))
        .collect()
}

/// Appends to a bounded per-record list, flagging the record when entries are dropped.
fn push_nested(record: &mut Value, list: &str, flag: &str, entry: Value) {
    let Some(entries) = record.get_mut(list).and_then(Value::as_array_mut) else {
        return;
    };
    if entries.len() < MAXIMUM_NESTED_ENTRIES {
        entries.push(entry);
    } else {
        record[flag] = json!(true);
    }
}

/// Reshapes a `variants` export into records overlapping the requested interval.
#[allow(clippy::too_many_lines)]
pub(crate) fn variants_result(
    text: &str,
    query: &EvidenceQuery,
    detail: VariantDetail,
) -> Result<Value, EvidenceError> {
    check_page(query, MAXIMUM_VARIANT_LIMIT)?;
    let scope = export_scope(text)?;
    let (start, end) = resolve_interval(query, &scope)?;
    let mut pseudonyms = SamplePseudonyms::default();
    let mut tracks = Vec::new();
    let mut records = Vec::<Value>::new();
    let mut total = 0_usize;
    // True while the `info`/`genotype` rows being read belong to a record on this page.
    let mut on_page = false;
    for line in text.lines() {
        if let Some((track_id, track)) = track_provenance(line) {
            if wanted(query.track_ids.as_ref(), track_id) {
                tracks.push(track);
            }
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let cells = line.split('\t').collect::<Vec<_>>();
        // The three column-header rows repeat their row kind and then name `track_id`.
        if cells.get(1) == Some(&"track_id") {
            continue;
        }
        let short = || EvidenceError::Malformed(format!("a `{}` row is too short", cells[0]));
        match cells[0] {
            "record" => {
                let [
                    _,
                    track_id,
                    _,
                    contig,
                    row_start,
                    row_end,
                    id,
                    reference,
                    alternates,
                    quality,
                    filters,
                    types,
                    ..,
                ] = cells.as_slice()
                else {
                    return Err(short());
                };
                let malformed =
                    || EvidenceError::Malformed("a variant row has invalid coordinates".to_owned());
                let track_id = track_id.parse::<u64>().map_err(|_| malformed())?;
                let record_start = row_start
                    .parse::<u64>()
                    .ok()
                    .and_then(|value| value.checked_sub(1))
                    .ok_or_else(malformed)?;
                let record_end = row_end.parse::<u64>().map_err(|_| malformed())?;
                // A record with an empty reference span still occupies its anchor base.
                let overlaps =
                    record_start < end && record_end.max(record_start.saturating_add(1)) > start;
                on_page = false;
                if !wanted(query.track_ids.as_ref(), track_id) || !overlaps {
                    continue;
                }
                total += 1;
                if total <= query.offset || records.len() >= query.limit {
                    continue;
                }
                on_page = true;
                let (reference, reference_truncated) = bounded(reference);
                let mut record = json!({
                    "track_id": track_id,
                    "contig": contig,
                    "start": record_start,
                    "end": record_end,
                    "id": (*id != ".").then_some(*id),
                    "ref": reference,
                    "ref_truncated": reference_truncated,
                    "alt": list(alternates, ','),
                    "qual": optional_decimal(quality),
                    "filters": list(filters, ';'),
                    "types": list(types, ','),
                });
                if detail.include_info {
                    record["info"] = json!([]);
                    record["info_truncated"] = json!(false);
                }
                if detail.include_genotypes {
                    record["genotypes"] = json!([]);
                    record["genotypes_truncated"] = json!(false);
                }
                records.push(record);
            }
            "info" => {
                let [_, _, _, key, value, field_truncated, ..] = cells.as_slice() else {
                    return Err(short());
                };
                if on_page
                    && detail.include_info
                    && let Some(record) = records.last_mut()
                {
                    let (value, shortened) = bounded(value);
                    push_nested(
                        record,
                        "info",
                        "info_truncated",
                        json!({
                            "key": key,
                            "value": value,
                            "truncated": shortened || *field_truncated == "true",
                        }),
                    );
                }
            }
            "genotype" => {
                let [_, _, _, sample, genotype, group, format, ..] = cells.as_slice() else {
                    return Err(short());
                };
                // Assign pseudonyms for every row so they do not depend on the page.
                let pseudonym = pseudonyms.name(sample);
                if on_page
                    && detail.include_genotypes
                    && let Some(record) = records.last_mut()
                {
                    let (format, shortened) = bounded(format);
                    let sample = if detail.include_sample_names {
                        (*sample).to_owned()
                    } else {
                        pseudonym
                    };
                    push_nested(
                        record,
                        "genotypes",
                        "genotypes_truncated",
                        json!({
                            "sample": sample,
                            "genotype": genotype.strip_prefix("GT=").unwrap_or(genotype),
                            "group": group,
                            "format": format,
                            "format_truncated": shortened,
                        }),
                    );
                }
            }
            other => {
                return Err(EvidenceError::Malformed(format!(
                    "unknown variant row kind `{}`",
                    bounded(other).0
                )));
            }
        }
    }
    let returned = records.len();
    let sample_names = match (detail.include_genotypes, detail.include_sample_names) {
        (false, _) => "not_requested",
        (true, true) => "included",
        (true, false) => "pseudonymized",
    };
    Ok(json!({
        "genome": scope.genome,
        "contig": scope.contig,
        "coordinates": "zero-based; intervals are half-open [start, end)",
        "query_start": start,
        "query_end": end,
        "view_start": scope.start,
        "view_end": scope.end,
        "tracks": tracks,
        "variants": records,
        "sample_names": sample_names,
        "page": page(total, query.offset, returned),
        "source_export_truncated": scope.truncated,
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const PILEUP: &str = "# RGV 0.1.0 pileup export v1\n# genome\tGRCh38\n# locus\tchr1:101-110\n# coordinates\tstart is one-based; end is one-based inclusive; internal intervals are zero-based half-open\n# byte_limit\t67108864\ntrack_id\ttrack_name\tcontig\tstart\tend\tdepth\tforward_depth\treverse_depth\tA\tC\tG\tT\tN\tinsertion\tdeletion\tmean_base_quality\tmean_mapping_quality\n# track_provenance\t3\ttumour\tauthority=committed_scene\tresolution=single_base\tzero_depth_sites=omitted\teligible_records=12\tall_eligible_records=true\talleles_available=true\toverlap_collapse_complete=true\toverlapping_mate_policy=CountOnce\n3\ttumour\tchr1\t101\t101\t10\t6\t4\t10\t0\t0\t0\t0\t0\t0\t35.5000\t60.0000\n3\ttumour\tchr1\t102\t102\t12\t7\t5\t1\t11\t0\t0\t0\t2\t1\t.\t59.2500\n3\ttumour\tchr1\t105\t105\t9\t5\t4\t0\t0\t9\t0\t0\t0\t0\t30.0000\t60.0000\n# track_provenance\t4\tnormal\tauthority=committed_scene\tresolution=single_base\tzero_depth_sites=omitted\teligible_records=7\tall_eligible_records=true\talleles_available=false\toverlap_collapse_complete=true\toverlapping_mate_policy=CountOnce\n4\tnormal\tchr1\t102\t102\t7\t3\t4\t0\t0\t0\t0\t0\t0\t0\t.\t.\n";

    pub(crate) const VARIANTS: &str = "# RGV 0.1.0 variant export v1\n# genome\tGRCh38\n# locus\tHLA-A*01:01:201-400\n# coordinates\tstart is one-based; end is one-based inclusive; internal intervals are zero-based half-open\n# byte_limit\t67108864\nrecord\ttrack_id\ttrack_name\tcontig\tstart\tend\tid\tref\talt\tqual\tfilters\ttypes\ninfo\ttrack_id\trecord_ordinal\tkey\tvalue\tfield_truncated\ngenotype\ttrack_id\trecord_ordinal\tsample\tgenotype\tgroup\tformat_fields\n# track_provenance\t7\tcalls\trecords_examined=3\trecords_filtered=0\trecords_truncated=false\ttotal_samples=2\tselected_samples=2\tgenotype_cells_omitted=0\nrecord\t7\tcalls\tHLA-A*01:01\t210\t210\trs1\tA\tG,T\t50.5\tPASS\tSNV\ninfo\t7\t0\tDP\t40\tfalse\ngenotype\t7\t0\tPATIENT_A\tGT=0/1\tHet\tDP=20\ngenotype\t7\t0\tPATIENT_B\tGT=0|0\tHom-ref\tDP=20\nrecord\t7\tcalls\tHLA-A*01:01\t300\t304\t.\tACGTA\tA\t.\tq10;lowdp\tDeletion\ngenotype\t7\t1\tPATIENT_B\tGT=1/1\tHom-alt\t\nrecord\t7\tcalls\tHLA-A*01:01\t390\t390\t.\tC\tCT\t12\tPASS\tInsertion\ngenotype\t7\t2\tPATIENT_A\tGT=./.\tMissing\t\n# export_truncated\tbyte_limit_reached\n";

    fn query(start: Option<u64>, end: Option<u64>) -> EvidenceQuery {
        EvidenceQuery {
            start,
            end,
            limit: 50,
            ..EvidenceQuery::default()
        }
    }

    #[test]
    fn pileup_sites_are_zero_based_and_keep_the_viewers_counts() {
        // One-based 102 is zero-based 101; a lone `start` asks about exactly that base.
        let result = pileup_result(PILEUP, &query(Some(101), None)).unwrap();
        assert_eq!(result["contig"], "chr1");
        assert_eq!(result["genome"], "GRCh38");
        assert_eq!(
            (&result["view_start"], &result["view_end"]),
            (&json!(100), &json!(110))
        );
        assert_eq!(
            (&result["query_start"], &result["query_end"]),
            (&json!(101), &json!(102))
        );
        let sites = result["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["track_id"], 3);
        assert_eq!(sites[0]["position"], 101);
        assert_eq!(sites[0]["depth"], 12);
        assert_eq!(sites[0]["forward_depth"], 7);
        assert_eq!(sites[0]["reverse_depth"], 5);
        assert_eq!(
            sites[0]["counts"],
            json!({"A": 1, "C": 11, "G": 0, "T": 0, "N": 0, "insertion": 2, "deletion": 1})
        );
        assert_eq!(sites[0]["mean_base_quality"], Value::Null);
        assert_eq!(sites[0]["mean_mapping_quality"], 59.25);
        assert_eq!(sites[1]["track_id"], 4);
        assert_eq!(result["tracks"][0]["alleles_available"], true);
        assert_eq!(result["tracks"][1]["alleles_available"], false);
        assert_eq!(result["tracks"][0]["eligible_records"], 12);
        assert_eq!(result["page"]["truncated"], false);
        assert_eq!(result["page"]["next_offset"], Value::Null);
        assert_eq!(result["source_export_truncated"], false);
    }

    #[test]
    fn pileup_defaults_to_the_whole_view_and_pages_with_an_explicit_truncation_flag() {
        let mut first = query(None, None);
        first.limit = 2;
        let result = pileup_result(PILEUP, &first).unwrap();
        assert_eq!(result["sites"].as_array().unwrap().len(), 2);
        assert_eq!(result["page"]["total_matching"], 4);
        assert_eq!(result["page"]["truncated"], true);
        assert_eq!(result["page"]["next_offset"], 2);

        let mut second = first.clone();
        second.offset = 2;
        let result = pileup_result(PILEUP, &second).unwrap();
        let sites = result["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["position"], 104);
        assert_eq!(result["page"]["truncated"], false);

        let mut filtered = query(None, None);
        filtered.track_ids = Some(vec![4]);
        let result = pileup_result(PILEUP, &filtered).unwrap();
        assert_eq!(result["page"]["total_matching"], 1);
        assert_eq!(result["tracks"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_interval_outside_the_exported_view_is_refused_not_answered_partially() {
        for (start, end) in [
            (Some(99), Some(101)),
            (Some(105), Some(111)),
            (Some(500), None),
        ] {
            let error = pileup_result(PILEUP, &query(start, end)).unwrap_err();
            assert_eq!(error.code(), "interval_not_in_view");
            assert!(error.to_string().contains("chr1:100-110"));
        }
        let mut other_contig = query(None, None);
        other_contig.contig = Some("chr2".to_owned());
        let error = pileup_result(PILEUP, &other_contig).unwrap_err();
        assert_eq!(error.code(), "interval_not_in_view");
        assert!(error.to_string().contains("shows contig chr1, not chr2"));

        let error = pileup_result(PILEUP, &query(Some(105), Some(105))).unwrap_err();
        assert_eq!(error.code(), "invalid_input");
        let mut unbounded = query(None, None);
        unbounded.limit = MAXIMUM_PILEUP_LIMIT + 1;
        assert_eq!(
            pileup_result(PILEUP, &unbounded).unwrap_err().code(),
            "invalid_input"
        );
        unbounded.limit = 0;
        assert_eq!(
            pileup_result(PILEUP, &unbounded).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn malformed_exports_are_reported_as_invalid_responses() {
        for text in [
            "",
            "# RGV 0.1.0 pileup export v1\n# locus\tchr1\n",
            "# locus\tchr1:0-10\n",
            "# locus\tchr1:20-10\n",
            "# locus\tchr1:101-110\n",
            "# locus\tchr1:101-110\ntrack_id\tstart\n3\tnot-a-number\n",
            "# locus\tchr1:101-110\ntrack_id\tstart\n3\t0\n",
        ] {
            let error = pileup_result(text, &query(None, None)).unwrap_err();
            assert_eq!(error.code(), "invalid_response", "{text:?}");
        }
        let error = variants_result(
            "# locus\tchr1:101-110\nsurprise\t1\t2\n",
            &query(None, None),
            VariantDetail::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_response");
        let error = variants_result(
            "# locus\tchr1:101-110\nrecord\t7\tcalls\n",
            &query(None, None),
            VariantDetail::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_response");
    }

    #[test]
    fn variants_overlapping_the_interval_are_returned_zero_based_half_open() {
        // The contig name itself contains colons; the view is one-based 201-400.
        let result =
            variants_result(VARIANTS, &query(None, None), VariantDetail::default()).unwrap();
        assert_eq!(result["contig"], "HLA-A*01:01");
        assert_eq!(
            (&result["view_start"], &result["view_end"]),
            (&json!(200), &json!(400))
        );
        let variants = result["variants"].as_array().unwrap();
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0]["start"], 209);
        assert_eq!(variants[0]["end"], 210);
        assert_eq!(variants[0]["id"], "rs1");
        assert_eq!(variants[0]["ref"], "A");
        assert_eq!(variants[0]["alt"], json!(["G", "T"]));
        assert_eq!(variants[0]["qual"], 50.5);
        assert_eq!(variants[0]["filters"], json!(["PASS"]));
        assert_eq!(variants[0]["types"], json!(["SNV"]));
        assert_eq!(variants[1]["id"], Value::Null);
        assert_eq!(variants[1]["qual"], Value::Null);
        assert_eq!(variants[1]["filters"], json!(["q10", "lowdp"]));
        assert_eq!(
            (&variants[1]["start"], &variants[1]["end"]),
            (&json!(299), &json!(304))
        );
        // INFO, genotypes and sample names are absent unless asked for.
        assert!(variants[0].get("info").is_none());
        assert!(variants[0].get("genotypes").is_none());
        assert_eq!(result["sample_names"], "not_requested");
        assert!(!result.to_string().contains("PATIENT"));
        assert_eq!(result["tracks"][0]["records_examined"], 3);
        assert_eq!(result["tracks"][0]["records_truncated"], false);
        assert_eq!(result["source_export_truncated"], true);

        // The deletion spans zero-based [299, 304): a query touching its last base finds it,
        // one starting at its exclusive end does not.
        let overlapping = variants_result(
            VARIANTS,
            &query(Some(303), Some(304)),
            VariantDetail::default(),
        )
        .unwrap();
        assert_eq!(overlapping["page"]["total_matching"], 1);
        let adjacent = variants_result(
            VARIANTS,
            &query(Some(304), Some(389)),
            VariantDetail::default(),
        )
        .unwrap();
        assert_eq!(adjacent["page"]["total_matching"], 0);
        assert_eq!(adjacent["variants"], json!([]));
    }

    #[test]
    fn genotype_sample_names_are_pseudonymized_consistently_unless_opted_in() {
        let detail = VariantDetail {
            include_info: true,
            include_genotypes: true,
            include_sample_names: false,
        };
        let mut second_page = query(None, None);
        second_page.offset = 1;
        second_page.limit = 1;
        let result = variants_result(VARIANTS, &second_page, detail).unwrap();
        assert!(!result.to_string().contains("PATIENT"));
        assert_eq!(result["sample_names"], "pseudonymized");
        assert_eq!(result["page"]["truncated"], true);
        assert_eq!(result["page"]["next_offset"], 2);
        let variant = &result["variants"][0];
        // PATIENT_B is `sample_2` on every page because PATIENT_A appeared first in the export.
        assert_eq!(variant["genotypes"][0]["sample"], "sample_2");
        assert_eq!(variant["genotypes"][0]["genotype"], "1/1");
        assert_eq!(variant["genotypes"][0]["group"], "Hom-alt");
        assert_eq!(variant["info"], json!([]));

        let first = variants_result(VARIANTS, &query(None, None), detail).unwrap();
        let variant = &first["variants"][0];
        assert_eq!(
            variant["info"][0],
            json!({"key": "DP", "value": "40", "truncated": false})
        );
        assert_eq!(variant["genotypes"][0]["sample"], "sample_1");
        assert_eq!(variant["genotypes"][1]["sample"], "sample_2");
        assert_eq!(variant["genotypes"][1]["genotype"], "0|0");
        assert_eq!(variant["genotypes"][0]["format"], "DP=20");

        let named = variants_result(
            VARIANTS,
            &query(None, None),
            VariantDetail {
                include_sample_names: true,
                ..detail
            },
        )
        .unwrap();
        assert_eq!(named["sample_names"], "included");
        assert_eq!(named["variants"][0]["genotypes"][0]["sample"], "PATIENT_A");
    }

    #[test]
    fn oversized_fields_and_nested_lists_are_bounded_with_flags() {
        let long_value = "é".repeat(MAXIMUM_FIELD_BYTES);
        let mut text = String::from(
            "# locus\tchr1:1-100\nrecord\t7\tcalls\tchr1\t5\t5\t.\tA\tG\t.\tPASS\tSNV\n",
        );
        for index in 0..=MAXIMUM_NESTED_ENTRIES {
            use std::fmt::Write as _;
            writeln!(text, "info\t7\t0\tK{index}\t{long_value}\tfalse").unwrap();
            writeln!(text, "genotype\t7\t0\tS{index}\tGT=0/1\tHet\t").unwrap();
        }
        let detail = VariantDetail {
            include_info: true,
            include_genotypes: true,
            include_sample_names: false,
        };
        let result = variants_result(&text, &query(None, None), detail).unwrap();
        let variant = &result["variants"][0];
        assert_eq!(
            variant["info"].as_array().unwrap().len(),
            MAXIMUM_NESTED_ENTRIES
        );
        assert_eq!(variant["info_truncated"], true);
        assert_eq!(
            variant["genotypes"].as_array().unwrap().len(),
            MAXIMUM_NESTED_ENTRIES
        );
        assert_eq!(variant["genotypes_truncated"], true);
        let value = variant["info"][0]["value"].as_str().unwrap();
        assert!(value.len() <= MAXIMUM_FIELD_BYTES);
        assert!(value.chars().all(|character| character == 'é'));
        assert_eq!(variant["info"][0]["truncated"], true);
    }
}
