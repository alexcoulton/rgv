//! IGV-compatible genome descriptions.
//!
//! IGV Desktop and igv.js describe a genome with a small JSON document (reference sequence,
//! cytobands, chromosome aliases and default annotation tracks) and publish a tab-delimited list
//! of hosted genomes. This module parses both, tolerantly: properties RGV does not use are
//! reported, never fatal, so that genome files written for IGV open unchanged.

use std::collections::BTreeSet;

use serde_json::Value;
use thiserror::Error;

use crate::{ContigAliases, MAXIMUM_CONTIG_ALIASES};

/// The hosted genome list read by IGV Desktop.
pub const IGV_HOSTED_GENOME_LIST_URL: &str = "https://igv.org/genomes/genomes.tsv";
pub const MAXIMUM_IGV_GENOME_TRACKS: usize = 256;
pub const MAXIMUM_IGV_GENOME_LIST_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgvGenomeTrack {
    pub name: String,
    pub url: String,
    pub index_url: Option<String>,
    /// Lower-cased IGV format name (`refgene`, `gff3`, `bigbed`, ...) when stated.
    pub format: Option<String>,
    pub order: Option<i64>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IgvGenomeDescription {
    pub id: String,
    pub name: String,
    pub fasta_url: Option<String>,
    pub index_url: Option<String>,
    pub two_bit_url: Option<String>,
    pub chrom_sizes_url: Option<String>,
    pub cytoband_url: Option<String>,
    pub alias_url: Option<String>,
    pub chromosome_order: Vec<String>,
    pub tracks: Vec<IgvGenomeTrack>,
    /// Properties present in the document which RGV does not act on.
    pub unused_properties: Vec<String>,
}

#[derive(Debug, Error)]
pub enum IgvGenomeError {
    #[error("the genome description is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the genome description is not a JSON object")]
    NotAnObject,
    #[error("the genome description names no reference sequence (twoBitURL or fastaURL)")]
    MissingReference,
    #[error("the genome description lists more than {MAXIMUM_IGV_GENOME_TRACKS} tracks")]
    TooManyTracks,
}

/// Description properties RGV acts on; every other property is reported as unused.
const USED_PROPERTIES: &[&str] = &[
    "id",
    "name",
    "fastaURL",
    "indexURL",
    "twoBitURL",
    "chromSizesURL",
    "cytobandURL",
    "aliasURL",
    "chromosomeOrder",
    "tracks",
];

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

impl IgvGenomeDescription {
    pub fn parse(document: &str) -> Result<Self, IgvGenomeError> {
        let value: Value = serde_json::from_str(document)?;
        let object = value.as_object().ok_or(IgvGenomeError::NotAnObject)?;
        let fasta_url = text(object.get("fastaURL"));
        let two_bit_url = text(object.get("twoBitURL"));
        if fasta_url.is_none() && two_bit_url.is_none() {
            return Err(IgvGenomeError::MissingReference);
        }
        let id = text(object.get("id"))
            .or_else(|| text(object.get("name")))
            .unwrap_or_else(|| "genome".to_owned());
        let name = text(object.get("name")).unwrap_or_else(|| id.clone());
        let chromosome_order = match object.get("chromosomeOrder") {
            Some(Value::String(order)) => order
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
            Some(Value::Array(order)) => order.iter().filter_map(|name| text(Some(name))).collect(),
            _ => Vec::new(),
        };
        let mut tracks = Vec::new();
        if let Some(Value::Array(entries)) = object.get("tracks") {
            if entries.len() > MAXIMUM_IGV_GENOME_TRACKS {
                return Err(IgvGenomeError::TooManyTracks);
            }
            for entry in entries {
                let Some(url) = text(entry.get("url")) else {
                    continue;
                };
                let name = text(entry.get("name"))
                    .or_else(|| text(entry.get("id")))
                    .unwrap_or_else(|| file_name(&url).to_owned());
                tracks.push(IgvGenomeTrack {
                    name,
                    index_url: text(entry.get("indexURL")),
                    format: text(entry.get("format"))
                        .or_else(|| text(entry.get("type")))
                        .map(|format| format.to_ascii_lowercase()),
                    order: entry.get("order").and_then(Value::as_i64),
                    hidden: entry.get("hidden").and_then(Value::as_bool) == Some(true)
                        || entry.get("visible").and_then(Value::as_bool) == Some(false),
                    url,
                });
            }
            tracks.sort_by_key(|track| track.order.unwrap_or(i64::MAX));
        }
        let unused_properties = object
            .keys()
            .filter(|key| !USED_PROPERTIES.contains(&key.as_str()))
            .cloned()
            .collect();
        Ok(Self {
            id,
            name,
            fasta_url,
            index_url: text(object.get("indexURL")),
            two_bit_url,
            chrom_sizes_url: text(object.get("chromSizesURL")),
            cytoband_url: text(object.get("cytobandURL")),
            alias_url: text(object.get("aliasURL")),
            chromosome_order,
            tracks,
            unused_properties,
        })
    }

    /// Resolves relative resource locations against the location the description was read from
    /// (a URL or a local path), as IGV does.
    pub fn resolve_relative_to(&mut self, description_location: &str) {
        let resolve = |location: &mut Option<String>| {
            if let Some(value) = location {
                *value = resolve_location(description_location, value);
            }
        };
        resolve(&mut self.fasta_url);
        resolve(&mut self.index_url);
        resolve(&mut self.two_bit_url);
        resolve(&mut self.chrom_sizes_url);
        resolve(&mut self.cytoband_url);
        resolve(&mut self.alias_url);
        for track in &mut self.tracks {
            track.url = resolve_location(description_location, &track.url);
            resolve(&mut track.index_url);
        }
    }
}

pub fn location_is_remote(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

/// Last path component of a URL or path, without any query string or fragment.
pub fn file_name(location: &str) -> &str {
    let path = if location_is_remote(location) {
        location.split(['?', '#']).next().unwrap_or(location)
    } else {
        location
    };
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn resolve_location(base: &str, location: &str) -> String {
    if location_is_remote(location)
        || location.starts_with('/')
        || location.starts_with("gs://")
        || location.starts_with("s3://")
        || std::path::Path::new(location).is_absolute()
    {
        return location.to_owned();
    }
    let directory = if location_is_remote(base) {
        let path_end = base.find(['?', '#']).unwrap_or(base.len());
        let scheme_end = base.find("://").map_or(0, |index| index + 3);
        match base[..path_end].rfind('/') {
            Some(index) if index >= scheme_end => &base[..=index],
            _ => return format!("{}/{location}", &base[..path_end]),
        }
    } else {
        match base.rfind(['/', '\\']) {
            Some(index) => &base[..=index],
            None => "",
        }
    };
    format!("{directory}{location}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgvGenomeListEntry {
    pub name: String,
    pub url: String,
    pub id: String,
}

/// Parses IGV's hosted genome list: `name<TAB>url<TAB>id` per line, with `<`/`#` header lines.
pub fn parse_igv_genome_list(document: &str) -> Vec<IgvGenomeListEntry> {
    let mut seen = BTreeSet::new();
    document
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', '<']))
        .filter_map(|line| {
            let mut fields = line.split('\t').map(str::trim);
            let name = fields.next().filter(|name| !name.is_empty())?;
            let url = fields.next().filter(|url| !url.is_empty())?;
            let id = fields.next().filter(|id| !id.is_empty())?;
            seen.insert(id.to_owned()).then(|| IgvGenomeListEntry {
                name: name.to_owned(),
                url: url.to_owned(),
                id: id.to_owned(),
            })
        })
        .take(MAXIMUM_IGV_GENOME_LIST_ENTRIES)
        .collect()
}

/// Converts an IGV chromosome alias table into explicit `alias -> canonical` entries.
///
/// Each line lists names for one sequence. The canonical name is whichever one the reference
/// itself uses; lines naming no reference sequence, and aliases which would shadow a canonical
/// name or disagree with an earlier line, are skipped.
pub fn parse_igv_alias_table<'a>(
    document: &str,
    canonical_names: impl IntoIterator<Item = &'a str>,
) -> ContigAliases {
    let canonical = canonical_names.into_iter().collect::<BTreeSet<_>>();
    let mut aliases = ContigAliases::new();
    for line in document.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let names = line
            .split('\t')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let Some(target) = names.iter().find(|name| canonical.contains(**name)) else {
            continue;
        };
        for alias in &names {
            if aliases.len() >= MAXIMUM_CONTIG_ALIASES {
                return aliases;
            }
            if canonical.contains(alias)
                || alias.len() > 256
                || alias.contains([':', ' ', '\n', '\r'])
            {
                continue;
            }
            aliases
                .entry((*alias).to_owned())
                .or_insert_with(|| (*target).to_owned());
        }
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;

    const HG38: &str = r#"{
      "id": "hg38",
      "name": "Human (GRCh38/hg38)",
      "twoBitURL": "https://example.org/hg38/hg38.2bit",
      "cytobandURL": "https://example.org/hg38/cytoBandIdeo.txt.gz",
      "aliasURL": "hg38_alias.tab",
      "chromSizesURL": "https://example.org/hg38/hg38.chrom.sizes",
      "chromosomeOrder": "chr1, chr2,chrX",
      "wholeGenomeView": true,
      "tracks": [
        {"id": "late", "name": "Late", "url": "late.bb", "order": 1000001},
        {"id": "genes", "name": "Refseq Genes", "url": "https://example.org/refGene.txt.gz",
         "format": "refGene", "order": 1000000, "html": "x"},
        {"name": "no url"}
      ],
      "hubs": ["https://example.org/hub.txt"]
    }"#;

    #[test]
    fn hosted_style_descriptions_parse_and_report_unused_properties() {
        let mut genome = IgvGenomeDescription::parse(HG38).unwrap();
        assert_eq!(genome.id, "hg38");
        assert_eq!(genome.name, "Human (GRCh38/hg38)");
        assert!(genome.fasta_url.is_none());
        assert_eq!(genome.chromosome_order, ["chr1", "chr2", "chrX"]);
        assert_eq!(genome.tracks.len(), 2);
        assert_eq!(genome.tracks[0].name, "Refseq Genes");
        assert_eq!(genome.tracks[0].format.as_deref(), Some("refgene"));
        assert_eq!(genome.unused_properties, ["hubs", "wholeGenomeView"]);

        genome.resolve_relative_to("https://igv.org/genomes/json/hg38.json?v=2");
        assert_eq!(
            genome.alias_url.as_deref(),
            Some("https://igv.org/genomes/json/hg38_alias.tab")
        );
        assert_eq!(genome.tracks[1].url, "https://igv.org/genomes/json/late.bb");
        assert_eq!(
            genome.two_bit_url.as_deref(),
            Some("https://example.org/hg38/hg38.2bit")
        );
    }

    #[test]
    fn fasta_descriptions_resolve_against_local_files() {
        let mut genome = IgvGenomeDescription::parse(
            r#"{"id":"mine","fastaURL":"seq/mine.fa","indexURL":"seq/mine.fa.fai",
                "chromosomeOrder":["a","b"]}"#,
        )
        .unwrap();
        genome.resolve_relative_to("/data/genomes/mine.json");
        assert_eq!(genome.name, "mine");
        assert_eq!(
            genome.fasta_url.as_deref(),
            Some("/data/genomes/seq/mine.fa")
        );
        assert_eq!(
            genome.index_url.as_deref(),
            Some("/data/genomes/seq/mine.fa.fai")
        );
        assert_eq!(genome.chromosome_order, ["a", "b"]);
    }

    #[test]
    fn descriptions_without_a_reference_are_rejected() {
        assert!(matches!(
            IgvGenomeDescription::parse(r#"{"id":"x"}"#),
            Err(IgvGenomeError::MissingReference)
        ));
        assert!(matches!(
            IgvGenomeDescription::parse("[]"),
            Err(IgvGenomeError::NotAnObject)
        ));
    }

    #[test]
    fn genome_lists_skip_headers_and_duplicates() {
        let list = parse_igv_genome_list(
            "<Server-Side Genome List>\nHuman (hg38)\thttps://x/hg38.json\thg38\n\
             # comment\nbroken line\nMouse\thttps://x/mm39.json\tmm39\n\
             Human again\thttps://x/other.json\thg38\n",
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "hg38");
        assert_eq!(list[0].name, "Human (hg38)");
        assert_eq!(list[1].url, "https://x/mm39.json");
    }

    #[test]
    fn alias_tables_target_the_reference_spelling() {
        let aliases = parse_igv_alias_table(
            "# header\nchr1\t1\tNC_000001.11\tCM000663.2\nMT\tchrM\tNC_012920.1\n\
             unplaced\tother\n2\tchr2\n",
            ["chr1", "chr2", "chrM"],
        );
        assert_eq!(aliases.get("1").map(String::as_str), Some("chr1"));
        assert_eq!(
            aliases.get("NC_000001.11").map(String::as_str),
            Some("chr1")
        );
        assert_eq!(aliases.get("MT").map(String::as_str), Some("chrM"));
        assert_eq!(aliases.get("2").map(String::as_str), Some("chr2"));
        assert!(!aliases.contains_key("chr1"));
        assert!(!aliases.contains_key("unplaced"));
    }
}
