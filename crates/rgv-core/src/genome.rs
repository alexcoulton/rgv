use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContigAliases, ContigId, GenomicInterval, ReferenceDictionary, TrackKind, TrackSource,
};

pub const GENOME_CONFIGURATION_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_DEFAULT_ANNOTATIONS: usize = 256;
pub const MAXIMUM_GENOME_ASSET_PROVENANCE: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyIdentityConfidence {
    #[default]
    Unknown,
    Explicit,
    Fingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeAssemblyIdentity {
    pub stable_id: String,
    pub display_name: String,
    pub confidence: AssemblyIdentityConfidence,
    pub provenance: String,
    #[serde(default)]
    pub matched_fingerprints: usize,
    #[serde(default)]
    pub expected_primary_contigs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeAssetProvenance {
    pub role: String,
    pub source: String,
    pub version: String,
    pub sha256: String,
    pub license: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum GeneticCode {
    #[default]
    Standard,
    VertebrateMitochondrial,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum ReferenceTranslationFrame {
    Forward1,
    Forward2,
    Forward3,
    Reverse1,
    Reverse2,
    Reverse3,
}

impl ReferenceTranslationFrame {
    pub const ALL: [Self; 6] = [
        Self::Forward1,
        Self::Forward2,
        Self::Forward3,
        Self::Reverse1,
        Self::Reverse2,
        Self::Reverse3,
    ];

    pub const fn phase(self) -> u64 {
        match self {
            Self::Forward1 | Self::Reverse1 => 0,
            Self::Forward2 | Self::Reverse2 => 1,
            Self::Forward3 | Self::Reverse3 => 2,
        }
    }

    pub const fn is_reverse(self) -> bool {
        matches!(self, Self::Reverse1 | Self::Reverse2 | Self::Reverse3)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Forward1 => "+1",
            Self::Forward2 => "+2",
            Self::Forward3 => "+3",
            Self::Reverse1 => "−1",
            Self::Reverse2 => "−2",
            Self::Reverse3 => "−3",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenomeAnnotation {
    pub name: String,
    pub kind: TrackKind,
    pub source: TrackSource,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenomeConfiguration {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub assembly_identity: Option<GenomeAssemblyIdentity>,
    pub reference: TrackSource,
    #[serde(default)]
    pub cytobands: Option<TrackSource>,
    #[serde(default)]
    pub contig_aliases: ContigAliases,
    #[serde(default)]
    pub default_annotations: Vec<GenomeAnnotation>,
    #[serde(default)]
    pub asset_provenance: Vec<GenomeAssetProvenance>,
}

impl GenomeConfiguration {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, GenomeConfigurationError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let mut configuration: Self = serde_json::from_str(&text)?;
        configuration.resolve_relative_sources(path.parent().unwrap_or_else(|| Path::new(".")));
        configuration.validate()?;
        Ok(configuration)
    }

    pub fn validate(&self) -> Result<(), GenomeConfigurationError> {
        if self.schema_version != GENOME_CONFIGURATION_SCHEMA_VERSION {
            return Err(GenomeConfigurationError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        if self.name.trim().is_empty() || self.name.len() > 256 {
            return Err(GenomeConfigurationError::InvalidName);
        }
        if self.assembly_identity.as_ref().is_some_and(|identity| {
            identity.stable_id.trim().is_empty()
                || identity.stable_id.len() > 256
                || identity.display_name.trim().is_empty()
                || identity.display_name.len() > 256
                || identity.provenance.trim().is_empty()
                || identity.provenance.len() > 4_096
        }) {
            return Err(GenomeConfigurationError::InvalidIdentity);
        }
        if self.contig_aliases.len() > crate::MAXIMUM_CONTIG_ALIASES
            || self
                .contig_aliases
                .iter()
                .any(|(alias, target)| !valid_contig_name(alias) || !valid_contig_name(target))
        {
            return Err(GenomeConfigurationError::InvalidAliases);
        }
        if self.default_annotations.len() > MAXIMUM_DEFAULT_ANNOTATIONS
            || self.default_annotations.iter().any(|annotation| {
                annotation.name.trim().is_empty()
                    || annotation.name.len() > 256
                    || matches!(annotation.kind, TrackKind::Alignment | TrackKind::Reference)
            })
        {
            return Err(GenomeConfigurationError::InvalidAnnotations);
        }
        if self.asset_provenance.len() > MAXIMUM_GENOME_ASSET_PROVENANCE
            || self.asset_provenance.iter().any(|asset| {
                asset.role.trim().is_empty()
                    || asset.role.len() > 256
                    || asset.source.trim().is_empty()
                    || asset.source.len() > 4_096
                    || asset.version.len() > 256
                    || asset.sha256.len() != 64
                    || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || asset.license.trim().is_empty()
                    || asset.license.len() > 4_096
            })
        {
            return Err(GenomeConfigurationError::InvalidAssetProvenance);
        }
        Ok(())
    }

    fn resolve_relative_sources(&mut self, base: &Path) {
        resolve_relative_source(&mut self.reference, base);
        if let Some(cytobands) = &mut self.cytobands {
            resolve_relative_source(cytobands, base);
        }
        for annotation in &mut self.default_annotations {
            resolve_relative_source(&mut annotation.source, base);
        }
    }
}

fn valid_contig_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.contains([':', ' ', '\t', '\n', '\r'])
}

fn resolve_relative_source(source: &mut TrackSource, base: &Path) {
    let TrackSource::Local { path } = source else {
        return;
    };
    let local = Path::new(path);
    if local.is_relative() {
        *path = base.join(local).to_string_lossy().into_owned();
    }
}

#[derive(Debug, Error)]
pub enum GenomeConfigurationError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("genome configuration schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("genome configuration name is invalid")]
    InvalidName,
    #[error("genome configuration assembly identity is invalid")]
    InvalidIdentity,
    #[error("genome configuration contig aliases are invalid")]
    InvalidAliases,
    #[error(
        "genome configuration default annotations are invalid or exceed {MAXIMUM_DEFAULT_ANNOTATIONS} entries"
    )]
    InvalidAnnotations,
    #[error("genome configuration asset provenance is invalid")]
    InvalidAssetProvenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CytobandStain {
    Negative,
    Positive25,
    Positive50,
    Positive75,
    Positive100,
    Centromere,
    Variable,
    Stalk,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cytoband {
    pub interval: GenomicInterval,
    pub name: String,
    pub stain: CytobandStain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeAssembly {
    pub name: String,
    pub identity: Option<GenomeAssemblyIdentity>,
    /// Number of contigs with configured reference sequence. This can be
    /// smaller than the alignment dictionary used for navigation.
    pub reference_contig_count: usize,
    pub default_annotation_count: usize,
    pub dictionary: ReferenceDictionary,
    pub cytobands: Vec<Cytoband>,
}

impl GenomeAssembly {
    pub fn from_dictionary(name: impl Into<String>, dictionary: ReferenceDictionary) -> Self {
        let reference_contig_count = dictionary.len();
        Self {
            name: name.into(),
            identity: None,
            reference_contig_count,
            default_annotation_count: 0,
            dictionary,
            cytobands: Vec::new(),
        }
    }

    pub fn context_label(&self) -> String {
        let Some(identity) = &self.identity else {
            return self.name.clone();
        };
        if identity.expected_primary_contigs > 0
            && self.reference_contig_count < identity.expected_primary_contigs
        {
            format!(
                "{} · {}-contig subset",
                identity.display_name, self.reference_contig_count
            )
        } else {
            identity.display_name.clone()
        }
    }

    pub fn has_cytobands(&self) -> bool {
        !self.cytobands.is_empty()
    }

    pub fn cytobands_for(&self, contig: ContigId) -> impl Iterator<Item = &Cytoband> {
        self.cytobands
            .iter()
            .filter(move |band| band.interval.contig() == contig)
    }
}

pub fn parse_ucsc_cytobands(
    text: &str,
    dictionary: &ReferenceDictionary,
) -> Result<Vec<Cytoband>, CytobandParseError> {
    let mut bands = Vec::new();
    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // UCSC represents unnamed bands as an empty fourth TSV column. Preserve
        // that column instead of collapsing it with `split_whitespace`.
        let fields = if line.contains('\t') {
            line.split('\t').collect::<Vec<_>>()
        } else {
            line.split_whitespace().collect::<Vec<_>>()
        };
        if fields.len() < 5 {
            return Err(CytobandParseError::FieldCount { line: line_number });
        }
        let Some(contig) = dictionary.id_by_name(fields[0]) else {
            continue;
        };
        let start = fields[1]
            .parse::<u64>()
            .map_err(|_| CytobandParseError::Coordinate { line: line_number })?;
        let end = fields[2]
            .parse::<u64>()
            .map_err(|_| CytobandParseError::Coordinate { line: line_number })?;
        let contig_length = dictionary
            .get(contig)
            .expect("a dictionary lookup returned a valid contig id")
            .length();
        let interval = GenomicInterval::bounded(contig, start, end, contig_length)
            .map_err(|_| CytobandParseError::Coordinate { line: line_number })?;
        bands.push(Cytoband {
            interval,
            name: fields[3].to_owned(),
            stain: parse_stain(fields[4], line_number)?,
        });
    }
    bands.sort_by_key(|band| (band.interval.contig(), band.interval.start()));
    Ok(bands)
}

fn parse_stain(value: &str, line: usize) -> Result<CytobandStain, CytobandParseError> {
    match value.to_ascii_lowercase().as_str() {
        "gneg" => Ok(CytobandStain::Negative),
        "gpos25" => Ok(CytobandStain::Positive25),
        "gpos50" => Ok(CytobandStain::Positive50),
        "gpos75" => Ok(CytobandStain::Positive75),
        "gpos100" => Ok(CytobandStain::Positive100),
        "acen" => Ok(CytobandStain::Centromere),
        "gvar" => Ok(CytobandStain::Variable),
        "stalk" => Ok(CytobandStain::Stalk),
        stain => Err(CytobandParseError::Stain {
            line,
            stain: stain.to_owned(),
        }),
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CytobandParseError {
    #[error("cytoband line {line} must contain chromosome, start, end, name, and stain")]
    FieldCount { line: usize },
    #[error("cytoband line {line} has invalid or out-of-range coordinates")]
    Coordinate { line: usize },
    #[error("cytoband line {line} has unsupported stain {stain}")]
    Stain { line: usize, stain: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cytobands_are_filtered_by_contig() {
        let dictionary =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("chr2".to_owned(), 200)]);
        let mut assembly = GenomeAssembly::from_dictionary("test", dictionary);
        assembly.cytobands = vec![
            Cytoband {
                interval: GenomicInterval::new(ContigId::new(0), 0, 50).unwrap(),
                name: "p1".to_owned(),
                stain: CytobandStain::Negative,
            },
            Cytoband {
                interval: GenomicInterval::new(ContigId::new(1), 0, 100).unwrap(),
                name: "q1".to_owned(),
                stain: CytobandStain::Positive50,
            },
        ];

        assert_eq!(assembly.cytobands_for(ContigId::new(0)).count(), 1);
    }

    #[test]
    fn parses_ucsc_cytobands_and_ignores_unknown_contigs() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000)]);
        let bands = parse_ucsc_cytobands(
            "chr1\t0\t400\tp11\tgneg\nchr1\t400\t500\tp12\tacen\nchr1\t500\t600\t\tgneg\nchrUn\t0\t5\tx\tgneg",
            &dictionary,
        )
        .unwrap();
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[1].stain, CytobandStain::Centromere);
        assert!(bands[2].name.is_empty());
    }

    #[test]
    fn portable_genome_configuration_resolves_local_bundle_paths() {
        let configuration = GenomeConfiguration {
            schema_version: GENOME_CONFIGURATION_SCHEMA_VERSION,
            name: "test".to_owned(),
            assembly_identity: None,
            reference: TrackSource::Local {
                path: "reference/genome.fa".to_owned(),
            },
            cytobands: Some(TrackSource::Local {
                path: "metadata/cytobands.tsv".to_owned(),
            }),
            contig_aliases: ContigAliases::from([("1".to_owned(), "chr1".to_owned())]),
            default_annotations: vec![GenomeAnnotation {
                name: "Genes".to_owned(),
                kind: TrackKind::GeneModel,
                source: TrackSource::Local {
                    path: "annotations/genes.gtf.gz".to_owned(),
                },
            }],
            asset_provenance: Vec::new(),
        };
        configuration.validate().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let configuration_path = directory.path().join("test.rgvgenome");
        std::fs::write(
            &configuration_path,
            serde_json::to_string_pretty(&configuration).unwrap(),
        )
        .unwrap();
        let configuration = GenomeConfiguration::read(&configuration_path).unwrap();

        assert_eq!(
            configuration.reference,
            TrackSource::Local {
                path: directory
                    .path()
                    .join("reference/genome.fa")
                    .to_string_lossy()
                    .into_owned()
            }
        );
        assert_eq!(
            configuration.default_annotations[0].source,
            TrackSource::Local {
                path: directory
                    .path()
                    .join("annotations/genes.gtf.gz")
                    .to_string_lossy()
                    .into_owned()
            }
        );
    }
}
