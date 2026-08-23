use std::{fmt::Write as _, fs::File, io::Read, path::Path};

use rgv_core::{
    MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS, MAXIMUM_REVIEW_NOTE_BYTES, RegionOfInterest,
    ReviewItem, ReviewStatus, SampleMetadataTable, SessionLocus,
};

const MAXIMUM_LIST_IMPORT_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_LABEL_BYTES: usize = 256;
const DEFAULT_REGION_COLOR: [u8; 4] = [45, 92, 175, 48];
const REVIEW_HEADER: &str = "#RGV-review-queue-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceListKind {
    Regions,
    Review,
    SampleMetadata,
}

#[derive(Debug)]
pub(crate) enum WorkspaceListImport {
    Regions(Vec<RegionOfInterest>),
    Review(Vec<ReviewItem>),
    SampleMetadata(SampleMetadataTable),
}

pub(crate) fn import_workspace_list(
    path: &Path,
    kind: WorkspaceListKind,
) -> Result<WorkspaceListImport, String> {
    let text = read_bounded_utf8(path)?;
    match kind {
        WorkspaceListKind::Regions => parse_region_bed(&text).map(WorkspaceListImport::Regions),
        WorkspaceListKind::Review => parse_review_tsv(&text).map(WorkspaceListImport::Review),
        WorkspaceListKind::SampleMetadata => SampleMetadataTable::from_tsv(&text)
            .map(WorkspaceListImport::SampleMetadata)
            .map_err(|error| error.to_string()),
    }
}

pub(crate) fn region_bed_export(regions: &[RegionOfInterest]) -> String {
    let mut output = String::from("track name=\"RGV regions of interest\" itemRgb=\"On\"\n");
    for region in regions.iter().take(MAXIMUM_REGIONS_OF_INTEREST) {
        let name = single_line(&region.name, MAXIMUM_LABEL_BYTES);
        let [red, green, blue, _] = region.color;
        let _ = writeln!(
            output,
            "{}\t{}\t{}\t{}\t0\t.\t{}\t{}\t{red},{green},{blue}",
            region.locus.contig,
            region.locus.start,
            region.locus.end,
            name,
            region.locus.start,
            region.locus.end,
        );
    }
    output
}

pub(crate) fn review_tsv_export(items: &[ReviewItem]) -> String {
    let mut output = format!(
        "{REVIEW_HEADER}\ncontig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json\n"
    );
    for item in items.iter().take(MAXIMUM_REVIEW_ITEMS) {
        let label = serde_json::to_string(&single_line(&item.label, MAXIMUM_LABEL_BYTES))
            .expect("serializing a string cannot fail");
        let assignment = serde_json::to_string(&item.assignment)
            .expect("serializing an optional string cannot fail");
        let notes = serde_json::to_string(&bounded_text(&item.notes, MAXIMUM_REVIEW_NOTE_BYTES))
            .expect("serializing a string cannot fail");
        let _ = writeln!(
            output,
            "{}\t{}\t{}\t{}\t{label}\t{assignment}\t{notes}",
            item.locus.contig,
            item.locus.start,
            item.locus.end,
            review_status_name(item.status),
        );
    }
    output
}

fn read_bounded_utf8(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut text = String::new();
    file.take((MAXIMUM_LIST_IMPORT_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|error| {
            format!(
                "{} is not valid bounded UTF-8 text: {error}",
                path.display()
            )
        })?;
    if text.len() > MAXIMUM_LIST_IMPORT_BYTES {
        return Err(format!(
            "{} exceeds the {} MiB workspace-list import limit",
            path.display(),
            MAXIMUM_LIST_IMPORT_BYTES / (1024 * 1024)
        ));
    }
    Ok(text)
}

fn parse_region_bed(text: &str) -> Result<Vec<RegionOfInterest>, String> {
    let mut regions = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty()
            || line.starts_with('#')
            || line.starts_with("track ")
            || line.starts_with("browser ")
        {
            continue;
        }
        if regions.len() >= MAXIMUM_REGIONS_OF_INTEREST {
            return Err(format!(
                "line {line_number}: more than {MAXIMUM_REGIONS_OF_INTEREST} regions"
            ));
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 3 {
            return Err(format!(
                "line {line_number}: BED requires at least 3 columns"
            ));
        }
        let locus = parse_locus(fields[0], fields[1], fields[2], line_number)?;
        let fallback = format!("Region {}", regions.len() + 1);
        let name = fields.get(3).map_or_else(
            || fallback.clone(),
            |name| single_line(name, MAXIMUM_LABEL_BYTES),
        );
        let name = if name.trim().is_empty() {
            fallback
        } else {
            name
        };
        let color = fields
            .get(8)
            .filter(|value| !value.is_empty() && **value != "0")
            .map_or(Ok(DEFAULT_REGION_COLOR), |value| {
                parse_bed_color(value, line_number)
            })?;
        regions.push(RegionOfInterest {
            id: u64::try_from(regions.len() + 1).map_err(|error| error.to_string())?,
            name,
            locus,
            color,
        });
    }
    Ok(regions)
}

fn parse_review_tsv(text: &str) -> Result<Vec<ReviewItem>, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some(REVIEW_HEADER) {
        return Err(format!("review queue must begin with {REVIEW_HEADER}"));
    }
    let Some(header) = lines.next() else {
        return Err("review queue is missing its column header".to_owned());
    };
    if header.trim_end_matches('\r')
        != "contig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json"
    {
        return Err("review queue has an unsupported column layout".to_owned());
    }
    let mut items = Vec::new();
    for (offset, line) in lines.enumerate() {
        let line_number = offset + 3;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if items.len() >= MAXIMUM_REVIEW_ITEMS {
            return Err(format!(
                "line {line_number}: more than {MAXIMUM_REVIEW_ITEMS} review items"
            ));
        }
        let fields = line.splitn(7, '\t').collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err(format!(
                "line {line_number}: expected 7 tab-separated columns"
            ));
        }
        let locus = parse_locus(fields[0], fields[1], fields[2], line_number)?;
        let status = parse_review_status(fields[3])
            .ok_or_else(|| format!("line {line_number}: unknown review status"))?;
        let mut label: String = serde_json::from_str(fields[4])
            .map_err(|error| format!("line {line_number}: invalid label: {error}"))?;
        let mut assignment: Option<String> = serde_json::from_str(fields[5])
            .map_err(|error| format!("line {line_number}: invalid assignment: {error}"))?;
        let mut notes: String = serde_json::from_str(fields[6])
            .map_err(|error| format!("line {line_number}: invalid notes: {error}"))?;
        label = single_line(&label, MAXIMUM_LABEL_BYTES);
        if label.trim().is_empty() {
            label = format!("{}:{}-{}", locus.contig, locus.start + 1, locus.end);
        }
        if let Some(value) = &mut assignment {
            *value = single_line(value, MAXIMUM_LABEL_BYTES);
            if value.trim().is_empty() {
                assignment = None;
            }
        }
        notes = bounded_text(&notes, MAXIMUM_REVIEW_NOTE_BYTES);
        items.push(ReviewItem {
            id: u64::try_from(items.len() + 1).map_err(|error| error.to_string())?,
            label,
            locus,
            status,
            notes,
            assignment,
        });
    }
    Ok(items)
}

fn parse_locus(contig: &str, start: &str, end: &str, line: usize) -> Result<SessionLocus, String> {
    if contig.is_empty()
        || contig.len() > MAXIMUM_LABEL_BYTES
        || contig.chars().any(char::is_whitespace)
    {
        return Err(format!("line {line}: invalid contig name"));
    }
    let start = start
        .parse::<u64>()
        .map_err(|error| format!("line {line}: invalid zero-based start: {error}"))?;
    let end = end
        .parse::<u64>()
        .map_err(|error| format!("line {line}: invalid half-open end: {error}"))?;
    if start >= end {
        return Err(format!("line {line}: start must be smaller than end"));
    }
    Ok(SessionLocus {
        contig: contig.to_owned(),
        start,
        end,
    })
}

fn parse_bed_color(value: &str, line: usize) -> Result<[u8; 4], String> {
    let channels = value
        .split(',')
        .map(str::parse::<u8>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("line {line}: invalid BED itemRgb color: {error}"))?;
    let [red, green, blue] = channels.as_slice() else {
        return Err(format!(
            "line {line}: BED itemRgb must contain red,green,blue"
        ));
    };
    Ok([*red, *green, *blue, DEFAULT_REGION_COLOR[3]])
}

fn review_status_name(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Pending => "pending",
        ReviewStatus::InProgress => "in_progress",
        ReviewStatus::Complete => "complete",
        ReviewStatus::Skipped => "skipped",
    }
}

fn parse_review_status(value: &str) -> Option<ReviewStatus> {
    match value {
        "pending" => Some(ReviewStatus::Pending),
        "in_progress" => Some(ReviewStatus::InProgress),
        "complete" => Some(ReviewStatus::Complete),
        "skipped" => Some(ReviewStatus::Skipped),
        _ => None,
    }
}

fn single_line(value: &str, maximum_bytes: usize) -> String {
    bounded_text(&value.replace(['\t', '\r', '\n'], " "), maximum_bytes)
}

fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bed_round_trip_preserves_coordinates_names_and_rgb() {
        let input = "track itemRgb=On\nchr1\t99\t200\ttumour locus\t0\t.\t99\t200\t12,34,56\n";
        let parsed = parse_region_bed(input).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].locus.start, 99);
        assert_eq!(parsed[0].locus.end, 200);
        assert_eq!(parsed[0].name, "tumour locus");
        assert_eq!(parsed[0].color, [12, 34, 56, 48]);

        let reparsed = parse_region_bed(&region_bed_export(&parsed)).unwrap();
        assert_eq!(reparsed[0].locus, parsed[0].locus);
        assert_eq!(reparsed[0].name, parsed[0].name);
        assert_eq!(reparsed[0].color, parsed[0].color);
    }

    #[test]
    fn bed_rejects_invalid_intervals_and_colors() {
        assert!(parse_region_bed("chr1\t200\t100\n").is_err());
        assert!(parse_region_bed("chr1\t100\t200\tx\t0\t.\t100\t200\t300,0,0\n").is_err());
    }

    #[test]
    fn review_round_trip_preserves_multiline_notes_and_assignment() {
        let item = ReviewItem {
            id: 7,
            label: "candidate\tone".to_owned(),
            locus: SessionLocus {
                contig: "chr6".to_owned(),
                start: 100,
                end: 200,
            },
            status: ReviewStatus::InProgress,
            notes: "first line\nsecond line".to_owned(),
            assignment: Some("Alex".to_owned()),
        };
        let parsed = parse_review_tsv(&review_tsv_export(&[item])).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].label, "candidate one");
        assert_eq!(parsed[0].status, ReviewStatus::InProgress);
        assert_eq!(parsed[0].notes, "first line\nsecond line");
        assert_eq!(parsed[0].assignment.as_deref(), Some("Alex"));
    }

    #[test]
    fn review_import_requires_version_and_known_status() {
        assert!(parse_review_tsv("contig\tstart\tend\n").is_err());
        let input = concat!(
            "#RGV-review-queue-v1\n",
            "contig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json\n",
            "chr1\t0\t1\tunknown\t\"x\"\tnull\t\"\"\n"
        );
        assert!(parse_review_tsv(input).is_err());
    }
}
