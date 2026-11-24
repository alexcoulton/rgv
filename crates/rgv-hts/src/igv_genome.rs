//! Loads IGV-compatible genome descriptions and the hosted genome list.
//!
//! The reference sequence itself stays where it is (it is read by range request). The small
//! companions of a genome — its description, chromosome sizes, cytobands, alias table and text
//! annotation tracks — are downloaded once into a cache directory, so a genome which has been
//! opened before can be opened again without a network connection.

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use flate2::read::MultiGzDecoder;
use rgv_core::{
    AssemblyIdentityConfidence, ContigAliases, GENOME_CONFIGURATION_SCHEMA_VERSION,
    GenomeAnnotation, GenomeAssemblyIdentity, GenomeConfiguration, GenomeConfigurationError,
    GenomicSourceFormat, IgvGenomeDescription, IgvGenomeError, IgvGenomeListEntry, IgvGenomeTrack,
    TrackFormat, TrackKind, TrackSource, classify_genomic_source, parse_igv_alias_table,
    parse_igv_genome_list,
};
use thiserror::Error;

use crate::{HttpRangeConfig, RemoteError, read_remote_object_bounded};

const MAXIMUM_DESCRIPTION_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_COMPANION_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_ANNOTATION_BYTES: usize = 512 * 1024 * 1024;
const MAXIMUM_DECOMPRESSED_COMPANION_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum IgvGenomeLoadError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Description(#[from] IgvGenomeError),
    #[error(transparent)]
    Configuration(#[from] GenomeConfigurationError),
    #[error("{0} is not valid UTF-8 text")]
    NotText(String),
    #[error("no hosted genome has the id '{0}'")]
    UnknownGenome(String),
}

/// A genome description converted to RGV's genome configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct IgvGenomeImport {
    pub configuration: GenomeConfiguration,
    /// Parts of the description which were skipped or could not be honoured.
    pub notices: Vec<String>,
}

fn is_remote(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

fn file_name(location: &str) -> &str {
    let path = if is_remote(location) {
        location.split(['?', '#']).next().unwrap_or(location)
    } else {
        location
    };
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn cache_key(location: &str) -> String {
    use std::fmt::Write as _;

    crate::source_revision::digest_bytes(location.as_bytes())[..8]
        .iter()
        .fold(String::with_capacity(16), |mut key, byte| {
            let _ = write!(key, "{byte:02x}");
            key
        })
}

fn safe_name(name: &str) -> String {
    let cleaned = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() {
        "resource".to_owned()
    } else {
        cleaned.chars().take(96).collect()
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("partial-{}", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Freshness {
    /// Use the cached copy when there is one.
    PreferCache,
    /// Ask the server first and fall back to the cached copy when it cannot be reached.
    PreferNetwork,
}

/// Returns a local path holding the bytes of `location`, downloading into the cache if needed.
/// A local `location` is returned unchanged.
pub(crate) fn local_copy(
    location: &str,
    cached_name: &str,
    cache: &Path,
    remote: &HttpRangeConfig,
    maximum_bytes: usize,
    freshness: Freshness,
) -> Result<PathBuf, IgvGenomeLoadError> {
    if !is_remote(location) {
        return Ok(PathBuf::from(location));
    }
    let path = cache.join(format!(
        "{}-{}",
        cache_key(location),
        safe_name(cached_name)
    ));
    if freshness == Freshness::PreferCache && path.is_file() {
        return Ok(path);
    }
    match read_remote_object_bounded(location, remote.clone(), maximum_bytes) {
        Ok((bytes, _)) => {
            write_atomically(&path, &bytes)?;
            Ok(path)
        }
        Err(_) if path.is_file() => Ok(path),
        Err(error) => Err(error.into()),
    }
}

fn read_text(path: &Path, label: &str) -> Result<String, IgvGenomeLoadError> {
    let bytes = fs::read(path)?;
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoded = Vec::new();
        MultiGzDecoder::new(bytes.as_slice())
            .take(MAXIMUM_DECOMPRESSED_COMPANION_BYTES)
            .read_to_end(&mut decoded)?;
        decoded
    } else {
        bytes
    };
    String::from_utf8(bytes).map_err(|_| IgvGenomeLoadError::NotText(label.to_owned()))
}

/// Reads IGV's hosted genome list, refreshing the cached copy when the server is reachable.
/// The flag is true when the list came from the cache because the server could not be reached.
pub fn load_igv_genome_list(
    url: &str,
    cache: &Path,
    remote: &HttpRangeConfig,
) -> Result<(Vec<IgvGenomeListEntry>, bool), IgvGenomeLoadError> {
    let cached = cache.join(format!("{}-genomes.tsv", cache_key(url)));
    let (path, from_cache) = if is_remote(url) {
        match read_remote_object_bounded(url, remote.clone(), MAXIMUM_DESCRIPTION_BYTES) {
            Ok((bytes, _)) => {
                write_atomically(&cached, &bytes)?;
                (cached, false)
            }
            Err(_) if cached.is_file() => (cached, true),
            Err(error) => return Err(error.into()),
        }
    } else {
        (PathBuf::from(url), false)
    };
    Ok((
        parse_igv_genome_list(&read_text(&path, "the genome list")?),
        from_cache,
    ))
}

/// Resolves a hosted genome id such as `hg38` through the genome list and loads it.
pub fn load_hosted_igv_genome(
    id: &str,
    list_url: &str,
    cache: &Path,
    remote: &HttpRangeConfig,
) -> Result<IgvGenomeImport, IgvGenomeLoadError> {
    let (entries, _) = load_igv_genome_list(list_url, cache, remote)?;
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.id.eq_ignore_ascii_case(id))
        })
        .ok_or_else(|| IgvGenomeLoadError::UnknownGenome(id.to_owned()))?;
    let mut url = entry.url.clone();
    if !is_remote(&url) && is_remote(list_url) {
        // The hosted list may name descriptions relative to itself.
        let mut description = IgvGenomeDescription {
            alias_url: Some(url),
            ..IgvGenomeDescription::default()
        };
        description.resolve_relative_to(list_url);
        url = description.alias_url.unwrap_or_default();
    }
    load_igv_genome(&url, cache, remote)
}

/// Loads an IGV genome description from a local path or URL.
pub fn load_igv_genome(
    location: &str,
    cache: &Path,
    remote: &HttpRangeConfig,
) -> Result<IgvGenomeImport, IgvGenomeLoadError> {
    let description_path = local_copy(
        location,
        file_name(location),
        cache,
        remote,
        MAXIMUM_DESCRIPTION_BYTES,
        Freshness::PreferNetwork,
    )?;
    let mut description =
        IgvGenomeDescription::parse(&read_text(&description_path, "the genome description")?)?;
    description.resolve_relative_to(location);
    import_description(&description, location, cache, remote)
}

fn source(location: &str) -> TrackSource {
    if is_remote(location) {
        TrackSource::Remote {
            url: location.to_owned(),
        }
    } else {
        TrackSource::Local {
            path: location.to_owned(),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn import_description(
    description: &IgvGenomeDescription,
    location: &str,
    cache: &Path,
    remote: &HttpRangeConfig,
) -> Result<IgvGenomeImport, IgvGenomeLoadError> {
    let mut notices = Vec::new();
    let companion = |url: &str, name: &str| {
        local_copy(
            url,
            name,
            cache,
            remote,
            MAXIMUM_COMPANION_BYTES,
            Freshness::PreferCache,
        )
    };

    // Chromosome sizes give the reference's own sequence names and, for 2bit, every length in
    // one read.
    let chrom_sizes = match &description.chrom_sizes_url {
        Some(url) => match companion(url, file_name(url)) {
            Ok(path) => Some(path),
            Err(error) => {
                notices.push(format!("chromosome sizes could not be read: {error}"));
                None
            }
        },
        None => None,
    };

    let (reference, reference_index) = if let Some(two_bit) = &description.two_bit_url {
        (
            source(two_bit),
            chrom_sizes
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        )
    } else {
        let fasta = description
            .fasta_url
            .as_ref()
            .ok_or(IgvGenomeError::MissingReference)?;
        (source(fasta), description.index_url.clone())
    };

    let canonical_names = canonical_sequence_names(
        description,
        chrom_sizes.as_deref(),
        cache,
        remote,
        &mut notices,
    );

    let cytobands = match &description.cytoband_url {
        Some(url) => match companion(url, file_name(url))
            .and_then(|path| read_text(&path, "the cytoband table"))
        {
            Ok(text) => {
                // Stored decompressed: the ideogram loader reads plain text.
                let path = cache.join(format!("{}-cytobands.txt", cache_key(url)));
                write_atomically(&path, text.as_bytes())?;
                Some(TrackSource::Local {
                    path: path.to_string_lossy().into_owned(),
                })
            }
            Err(error) => {
                notices.push(format!("cytobands could not be read: {error}"));
                None
            }
        },
        None => None,
    };

    let contig_aliases = match (&description.alias_url, &canonical_names) {
        (Some(url), Some(names)) => match companion(url, file_name(url))
            .and_then(|path| read_text(&path, "the chromosome alias table"))
        {
            Ok(text) => parse_igv_alias_table(&text, names.iter().map(String::as_str)),
            Err(error) => {
                notices.push(format!("chromosome aliases could not be read: {error}"));
                ContigAliases::new()
            }
        },
        (Some(_), None) => {
            notices.push(
                "chromosome aliases were skipped because the reference's sequence names are not \
                 known before it is opened (no chromSizesURL or FASTA index)"
                    .to_owned(),
            );
            ContigAliases::new()
        }
        (None, _) => ContigAliases::new(),
    };

    let mut default_annotations = Vec::new();
    for track in &description.tracks {
        if track.hidden {
            continue;
        }
        match import_track(track, cache, remote) {
            Ok(annotation) => default_annotations.push(annotation),
            Err(reason) => notices.push(format!("track '{}' was skipped: {reason}", track.name)),
        }
    }

    for property in &description.unused_properties {
        if matches!(
            property.as_str(),
            "hubs" | "twoBitBptURL" | "blat" | "blatDB"
        ) {
            notices.push(format!("'{property}' is not used by RGV"));
        }
    }

    let configuration = GenomeConfiguration {
        schema_version: GENOME_CONFIGURATION_SCHEMA_VERSION,
        name: description.name.chars().take(256).collect(),
        assembly_identity: Some(GenomeAssemblyIdentity {
            stable_id: description.id.chars().take(256).collect(),
            display_name: description.name.chars().take(256).collect(),
            confidence: AssemblyIdentityConfidence::Explicit,
            provenance: format!("IGV genome description {}", file_name(location)),
            matched_fingerprints: 0,
            expected_primary_contigs: 0,
        }),
        reference,
        reference_index,
        cytobands,
        contig_aliases,
        default_annotations,
        asset_provenance: Vec::new(),
    };
    configuration.validate()?;
    Ok(IgvGenomeImport {
        configuration,
        notices,
    })
}

fn canonical_sequence_names(
    description: &IgvGenomeDescription,
    chrom_sizes: Option<&Path>,
    cache: &Path,
    remote: &HttpRangeConfig,
    notices: &mut Vec<String>,
) -> Option<Vec<String>> {
    let first_columns = |text: String| {
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    if let Some(path) = chrom_sizes {
        return read_text(path, "the chromosome sizes table")
            .ok()
            .map(first_columns);
    }
    let index = description.index_url.clone().or_else(|| {
        description
            .fasta_url
            .as_ref()
            .filter(|fasta| !is_remote(fasta))
            .map(|fasta| format!("{fasta}.fai"))
    })?;
    match local_copy(
        &index,
        file_name(&index),
        cache,
        remote,
        MAXIMUM_COMPANION_BYTES,
        Freshness::PreferCache,
    )
    .and_then(|path| read_text(&path, "the FASTA index"))
    {
        Ok(text) => Some(first_columns(text)),
        Err(error) => {
            notices.push(format!("the FASTA index could not be read: {error}"));
            None
        }
    }
}

/// How a text annotation is named in the cache so that it is opened with the right parser.
fn text_annotation(format: &str) -> Option<(TrackKind, &'static str)> {
    Some(match format {
        "refgene" | "genepred" | "genepredext" | "ncbirefseq" | "ensgene" | "knowngene"
        | "refseq" => (TrackKind::GeneModel, "genePred"),
        "refflat" => (TrackKind::GeneModel, "refflat"),
        "gff3" | "gff" => (TrackKind::GeneModel, "gff3"),
        "gtf" => (TrackKind::GeneModel, "gtf"),
        "bed" => (TrackKind::Feature, "bed"),
        "narrowpeak" => (TrackKind::Feature, "narrowPeak"),
        "broadpeak" => (TrackKind::Feature, "broadPeak"),
        _ => return None,
    })
}

fn import_track(
    track: &IgvGenomeTrack,
    cache: &Path,
    remote: &HttpRangeConfig,
) -> Result<GenomeAnnotation, String> {
    let classified = classify_genomic_source(&track.url);
    let stated = track.format.as_deref().unwrap_or_default();
    let inferred = classified.map(|classified| classified.format);

    // Indexed binary formats are read in place by range request.
    let binary = match (stated, inferred) {
        ("biggenepred", _) | (_, Some(GenomicSourceFormat::BigGenePred)) => {
            Some((TrackKind::GeneModel, Some(TrackFormat::BigGenePred)))
        }
        ("bignarrowpeak", _) | (_, Some(GenomicSourceFormat::BigNarrowPeak)) => {
            Some((TrackKind::Feature, Some(TrackFormat::BigNarrowPeak)))
        }
        ("bigbed" | "bb", _) | ("", Some(GenomicSourceFormat::BigBed)) => {
            Some((TrackKind::Feature, None))
        }
        ("bigwig" | "bw", _)
        | ("", Some(GenomicSourceFormat::BigWig | GenomicSourceFormat::Tdf)) => {
            Some((TrackKind::Quantitative, None))
        }
        _ => None,
    };
    if let Some((kind, format)) = binary {
        return Ok(GenomeAnnotation {
            name: track.name.chars().take(256).collect(),
            kind,
            source: source(&track.url),
            format,
        });
    }

    let (kind, extension) = text_annotation(stated)
        .or_else(|| {
            stated.is_empty().then_some(()).and_then(|()| {
                text_annotation(match inferred? {
                    GenomicSourceFormat::GenePred => "genepred",
                    GenomicSourceFormat::RefFlat => "refflat",
                    GenomicSourceFormat::Gff3 => "gff3",
                    GenomicSourceFormat::Gtf => "gtf",
                    GenomicSourceFormat::Bed => "bed",
                    GenomicSourceFormat::NarrowPeak => "narrowpeak",
                    GenomicSourceFormat::BroadPeak => "broadpeak",
                    _ => return None,
                })
            })
        })
        .ok_or_else(|| {
            if stated.is_empty() {
                "its format could not be determined from its name".to_owned()
            } else {
                format!("format '{stated}' is not supported as a default genome track")
            }
        })?;

    if !is_remote(&track.url) {
        // A local file is used in place when its name already selects the right parser.
        let already_named = classified.is_some_and(|classified| {
            classified.is_supported()
                && text_annotation(stated).is_none_or(|(expected, _)| classified.kind() == expected)
                && classified.kind() == kind
        });
        if already_named {
            return Ok(GenomeAnnotation {
                name: track.name.chars().take(256).collect(),
                kind,
                source: source(&track.url),
                format: None,
            });
        }
    }

    // Text annotations are cached under a name whose extension states the format, because IGV
    // genome files commonly name them ambiguously (for example `refGene.txt.gz`).
    let original = file_name(&track.url);
    let compressed = original.to_ascii_lowercase().ends_with(".gz");
    let stem = original
        .trim_end_matches(".gz")
        .trim_end_matches(".GZ")
        .rsplit_once('.')
        .map_or(original, |(stem, _)| stem);
    let cached_name = format!("{stem}.{extension}{}", if compressed { ".gz" } else { "" });
    let path = if is_remote(&track.url) {
        local_copy(
            &track.url,
            &cached_name,
            cache,
            remote,
            MAXIMUM_ANNOTATION_BYTES,
            Freshness::PreferCache,
        )
        .map_err(|error| error.to_string())?
    } else {
        let path = cache.join(format!(
            "{}-{}",
            cache_key(&track.url),
            safe_name(&cached_name)
        ));
        let bytes = fs::read(&track.url).map_err(|error| error.to_string())?;
        write_atomically(&path, &bytes).map_err(|error| error.to_string())?;
        path
    };
    Ok(GenomeAnnotation {
        name: track.name.chars().take(256).collect(),
        kind,
        source: TrackSource::Local {
            path: path.to_string_lossy().into_owned(),
        },
        format: None,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::{Compression, write::GzEncoder};
    use tempfile::tempdir;

    use super::*;
    use crate::{FastaOpenOptions, FastaReference};

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn a_local_igv_genome_opens_with_sequence_cytobands_aliases_and_genes() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let cache = root.join("cache");
        fs::write(
            root.join("tiny.2bit"),
            crate::two_bit::tests::encode_two_bit(
                &[
                    ("chr1", "ACGTACGTACGTACGTACGTACGTACGTACGT"),
                    ("chrM", "GATTACAGATTACA"),
                ],
                false,
            ),
        )
        .unwrap();
        fs::write(root.join("tiny.chrom.sizes"), "chr1\t32\nchrM\t14\n").unwrap();
        fs::write(
            root.join("cytoBandIdeo.txt.gz"),
            gzip(b"chr1\t0\t16\tp11\tgneg\nchr1\t16\t32\tq11\tgpos50\n"),
        )
        .unwrap();
        fs::write(root.join("alias.tab"), "chr1\t1\tNC_1\nchrM\tMT\n").unwrap();
        // UCSC refGene layout: a leading bin column and an ambiguous `.txt.gz` name.
        fs::write(
            root.join("refGene.txt.gz"),
            gzip(
                b"585\tNM_1\tchr1\t+\t2\t30\t4\t28\t2\t2,20,\t10,30,\t0\tGENE1\tcmpl\tcmpl\t0,0,\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join("tiny.json"),
            r#"{
              "id": "tiny", "name": "Tiny genome",
              "twoBitURL": "tiny.2bit", "chromSizesURL": "tiny.chrom.sizes",
              "cytobandURL": "cytoBandIdeo.txt.gz", "aliasURL": "alias.tab",
              "tracks": [
                {"name": "Genes", "url": "refGene.txt.gz", "format": "refgene", "order": 1},
                {"name": "Hidden", "url": "hidden.bed", "hidden": true},
                {"name": "Odd", "url": "odd.xyz", "format": "mystery"}
              ],
              "hubs": ["hub.txt"]
            }"#,
        )
        .unwrap();

        let location = root.join("tiny.json").to_string_lossy().into_owned();
        let import = load_igv_genome(&location, &cache, &HttpRangeConfig::default()).unwrap();
        let configuration = &import.configuration;
        assert_eq!(configuration.name, "Tiny genome");
        assert_eq!(
            configuration.assembly_identity.as_ref().unwrap().stable_id,
            "tiny"
        );
        assert_eq!(
            configuration.contig_aliases.get("1").map(String::as_str),
            Some("chr1")
        );
        assert_eq!(
            configuration.contig_aliases.get("MT").map(String::as_str),
            Some("chrM")
        );
        assert!(import.notices.iter().any(|notice| notice.contains("Odd")));
        assert!(import.notices.iter().any(|notice| notice.contains("hubs")));

        // The reference opens through the ordinary reference reader using the sizes companion.
        let TrackSource::Local { path: reference } = &configuration.reference else {
            panic!("a local genome keeps a local reference");
        };
        let reference = FastaReference::open_with_options(
            reference,
            &FastaOpenOptions {
                remote_index_url: configuration.reference_index.clone(),
                ..FastaOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(reference.reference_dictionary().len(), 2);

        // Cytobands are stored decompressed and parse against the reference dictionary.
        let Some(TrackSource::Local { path: cytobands }) = &configuration.cytobands else {
            panic!("cytobands are cached locally");
        };
        let cytobands = rgv_core::parse_ucsc_cytobands(
            &fs::read_to_string(cytobands).unwrap(),
            reference.reference_dictionary(),
        )
        .unwrap();
        assert_eq!(cytobands.len(), 2);

        // The gene track is cached under a name which selects the genePred parser.
        assert_eq!(configuration.default_annotations.len(), 1);
        let genes = &configuration.default_annotations[0];
        assert_eq!(genes.kind, TrackKind::GeneModel);
        let TrackSource::Local { path: genes_path } = &genes.source else {
            panic!("text annotations are cached locally");
        };
        assert!(genes_path.ends_with("refGene.genePred.gz"), "{genes_path}");
        let classified = classify_genomic_source(genes_path).unwrap();
        assert_eq!(classified.format, GenomicSourceFormat::GenePred);
        assert!(classified.is_supported());
        let features =
            crate::TextFeatureSource::open(genes_path, crate::FeatureFormat::GenePred).unwrap();
        assert_eq!(features.contigs().collect::<Vec<_>>(), ["chr1"]);
    }

    /// Opens the real hosted hg38 genome. Needs network access; run with `--ignored`.
    #[test]
    #[ignore = "requires network access to GitHub, igv.org and UCSC"]
    fn hosted_hg38_opens_from_the_igv_genome_list() {
        let directory = tempdir().unwrap();
        let remote = HttpRangeConfig::default();
        let started = std::time::Instant::now();
        let import = load_hosted_igv_genome(
            "hg38",
            rgv_core::IGV_HOSTED_GENOME_LIST_URL,
            directory.path(),
            &remote,
        )
        .unwrap();
        eprintln!("loaded description in {:?}", started.elapsed());
        eprintln!("notices: {:#?}", import.notices);
        eprintln!(
            "annotations: {:#?}",
            import.configuration.default_annotations
        );
        let configuration = &import.configuration;
        assert_eq!(
            configuration.contig_aliases.get("1").map(String::as_str),
            Some("chr1")
        );
        assert!(configuration.cytobands.is_some());
        let TrackSource::Remote { url } = &configuration.reference else {
            panic!("the hosted reference is remote");
        };
        let started = std::time::Instant::now();
        let reference = FastaReference::open_with_options(
            url,
            &FastaOpenOptions {
                remote_index_url: configuration.reference_index.clone(),
                ..FastaOpenOptions::default()
            },
        )
        .unwrap();
        eprintln!("opened reference in {:?}", started.elapsed());
        let chr1 = reference.reference_dictionary().id_by_name("chr1").unwrap();
        assert_eq!(
            reference.reference_dictionary().get(chr1).unwrap().length(),
            248_956_422
        );
        // GRCh38 chr17:7,676,000-7,676,020 lies inside TP53.
        let chr17 = reference
            .reference_dictionary()
            .id_by_name("chr17")
            .unwrap();
        let started = std::time::Instant::now();
        let bases = reference
            .fetch(rgv_core::GenomicInterval::new(chr17, 7_675_999, 7_676_020).unwrap())
            .unwrap();
        eprintln!(
            "fetched {} in {:?}",
            String::from_utf8_lossy(&bases),
            started.elapsed()
        );
        assert_eq!(bases.len(), 21);
        assert!(bases.iter().all(|base| b"ACGT".contains(base)));
        // Unknown bases at the chromosome start are reported as N.
        let telomere = reference
            .fetch(rgv_core::GenomicInterval::new(chr1, 0, 10).unwrap())
            .unwrap();
        assert_eq!(telomere, b"NNNNNNNNNN");
    }

    #[test]
    fn genome_lists_resolve_ids_to_descriptions() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(
            root.join("mini.2bit"),
            crate::two_bit::tests::encode_two_bit(&[("chrA", "ACGTACGT")], false),
        )
        .unwrap();
        let description = root.join("mini.json");
        fs::write(&description, r#"{"id":"mini","twoBitURL":"mini.2bit"}"#).unwrap();
        let list = root.join("genomes.tsv");
        fs::write(
            &list,
            format!(
                "<Server-Side Genome List>\nMini genome\t{}\tmini\n",
                description.display()
            ),
        )
        .unwrap();
        let cache = root.join("cache");
        let list = list.to_string_lossy().into_owned();
        let (entries, from_cache) =
            load_igv_genome_list(&list, &cache, &HttpRangeConfig::default()).unwrap();
        assert!(!from_cache);
        assert_eq!(entries.len(), 1);
        let import =
            load_hosted_igv_genome("MINI", &list, &cache, &HttpRangeConfig::default()).unwrap();
        assert_eq!(import.configuration.name, "mini");
        assert!(import.configuration.reference_index.is_none());
        assert!(matches!(
            load_hosted_igv_genome("absent", &list, &cache, &HttpRangeConfig::default()),
            Err(IgvGenomeLoadError::UnknownGenome(_))
        ));
    }
}
