use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs::File,
    io::{BufReader, Read},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_core::{
    BisulfiteContext, GenomicInterval, ReferenceDictionary, SmrtKineticsMode, UltimaQualityMode,
};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentQuery, AlignmentRecord, AlignmentSource, CigarOpKind,
    FastaReference, LocalAlignmentSource, QueryDetail, QueryOptions, SequenceView,
    analyze_ultima_record, smrt_kinetics_series, summarize_bisulfite_record, summarize_sbx_record,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const REQUIRED_CATEGORIES: &[&str] = &[
    "illumina-paired-end",
    "ont",
    "pacbio-hifi",
    "pacbio-clr",
    "pacbio-smrt-kinetics",
    "rna-seq",
    "amplicon",
    "bisulfite",
    "single-cell",
    "high-depth",
    "sv-heavy",
    "sbx",
    "ultima",
    "reference-dependent-cram",
];

#[derive(Debug, Parser)]
#[command(about = "Validate pinned alignment corpora through RGV's production normalization path")]
struct Arguments {
    manifest: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
    /// Require every technology/use-case category in the beta matrix.
    #[arg(long)]
    require_complete_matrix: bool,
}

#[derive(Debug, Deserialize)]
struct CorpusManifest {
    schema_version: u32,
    datasets: Vec<DatasetSpec>,
}

#[derive(Debug, Deserialize)]
struct DatasetSpec {
    id: String,
    categories: Vec<String>,
    alignment: PathBuf,
    alignment_sha256: String,
    index: PathBuf,
    index_sha256: String,
    reference: Option<PathBuf>,
    reference_sha256: Option<String>,
    loci: Vec<LocusSpec>,
}

#[derive(Debug, Deserialize)]
struct LocusSpec {
    region: String,
    #[serde(default)]
    expected: ExpectedSemantics,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct ExpectedSemantics {
    minimum_records_seen: usize,
    minimum_records_emitted: usize,
    required: Vec<SemanticFeature>,
    junctions: Option<ExpectedJunctionSemantics>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct ExpectedJunctionSemantics {
    distinct: usize,
    total_depth: u64,
    xs_tagged: usize,
    alignment_strand_fallback: usize,
    invalid_xs: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum SemanticFeature {
    Spliced,
    Secondary,
    Supplementary,
    Paired,
    LongRead,
    LongCigar,
    ModifiedBases,
    MissingSequence,
    MissingQuality,
    BisulfiteEvidence,
    SmrtKinetics,
    SingleCellTags,
    SbxEvidence,
    UltimaEvidence,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct ObservedSemantics {
    records_seen: usize,
    records_emitted: usize,
    spliced: usize,
    secondary: usize,
    supplementary: usize,
    paired: usize,
    long_read: usize,
    long_cigar: usize,
    modified_bases: usize,
    missing_sequence: usize,
    missing_quality: usize,
    bisulfite_evidence: usize,
    smrt_kinetics: usize,
    single_cell_tags: usize,
    sbx_evidence: usize,
    ultima_evidence: usize,
    junctions_distinct: usize,
    junction_depth: u64,
    junction_xs_tagged: usize,
    junction_alignment_strand_fallback: usize,
    junction_invalid_xs: usize,
}

#[derive(Debug, Serialize)]
struct ValidationReport {
    manifest_schema_version: u32,
    categories: BTreeSet<String>,
    datasets: Vec<DatasetReport>,
}

#[derive(Debug, Serialize)]
struct DatasetReport {
    id: String,
    categories: Vec<String>,
    alignment_bytes: u64,
    open_ms: f64,
    loci: Vec<LocusReport>,
}

#[derive(Debug, Serialize)]
struct LocusReport {
    region: String,
    query_ms: f64,
    observed: ObservedSemantics,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let manifest_json = std::fs::read_to_string(&arguments.manifest)
        .with_context(|| format!("could not read {}", arguments.manifest.display()))?;
    let manifest: CorpusManifest = serde_json::from_str(&manifest_json)?;
    anyhow::ensure!(
        manifest.schema_version == MANIFEST_SCHEMA_VERSION,
        "unsupported corpus manifest schema {}",
        manifest.schema_version
    );
    anyhow::ensure!(!manifest.datasets.is_empty(), "corpus contains no datasets");
    if arguments.require_complete_matrix {
        validate_complete_matrix(&manifest.datasets)?;
    }
    let base = arguments
        .manifest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut categories = BTreeSet::new();
    let mut dataset_reports = Vec::with_capacity(manifest.datasets.len());
    for dataset in manifest.datasets {
        categories.extend(dataset.categories.iter().cloned());
        dataset_reports.push(validate_dataset(base, dataset)?);
    }
    let report = ValidationReport {
        manifest_schema_version: MANIFEST_SCHEMA_VERSION,
        categories,
        datasets: dataset_reports,
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = arguments.output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, format!("{json}\n"))?;
        println!("wrote {}", path.display());
    } else {
        println!("{json}");
    }
    Ok(())
}

fn validate_complete_matrix(datasets: &[DatasetSpec]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for dataset in datasets {
        anyhow::ensure!(
            ids.insert(dataset.id.as_str()),
            "validation corpus contains duplicate dataset id {}",
            dataset.id
        );
        anyhow::ensure!(
            !dataset.categories.is_empty(),
            "dataset {} has no qualification categories",
            dataset.id
        );
        let distinct = dataset.categories.iter().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            distinct.len() == dataset.categories.len(),
            "dataset {} contains duplicate qualification categories",
            dataset.id
        );
    }
    for category in REQUIRED_CATEGORIES {
        let candidates = datasets
            .iter()
            .filter(|dataset| dataset.categories.iter().any(|value| value == category))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !candidates.is_empty(),
            "validation corpus is missing required category {category}"
        );
        anyhow::ensure!(
            candidates
                .iter()
                .any(|dataset| dataset_qualifies_for_category(dataset, category)),
            "validation category {category} has no dataset declaring its required semantic evidence"
        );
    }
    Ok(())
}

fn dataset_qualifies_for_category(dataset: &DatasetSpec, category: &str) -> bool {
    let has = |feature| {
        dataset
            .loci
            .iter()
            .any(|locus| locus.expected.required.contains(&feature))
    };
    let maximum_minimum_records = dataset
        .loci
        .iter()
        .map(|locus| locus.expected.minimum_records_seen)
        .max()
        .unwrap_or_default();
    match category {
        "illumina-paired-end" => has(SemanticFeature::Paired),
        "ont" => has(SemanticFeature::LongRead) && has(SemanticFeature::ModifiedBases),
        "pacbio-hifi" | "pacbio-clr" => has(SemanticFeature::LongRead),
        "pacbio-smrt-kinetics" => has(SemanticFeature::SmrtKinetics),
        "rna-seq" => has(SemanticFeature::Spliced),
        "amplicon" => maximum_minimum_records >= 1_000,
        "bisulfite" => dataset.reference.is_some() && has(SemanticFeature::BisulfiteEvidence),
        "single-cell" => has(SemanticFeature::SingleCellTags),
        "high-depth" => maximum_minimum_records >= 100_000,
        "sv-heavy" => has(SemanticFeature::Supplementary),
        "sbx" => has(SemanticFeature::SbxEvidence),
        "ultima" => has(SemanticFeature::UltimaEvidence),
        "reference-dependent-cram" => {
            dataset.reference.is_some()
                && dataset.reference_sha256.is_some()
                && dataset
                    .alignment
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("cram"))
        }
        _ => false,
    }
}

fn validate_dataset(base: &Path, dataset: DatasetSpec) -> Result<DatasetReport> {
    anyhow::ensure!(!dataset.id.trim().is_empty(), "dataset id cannot be empty");
    anyhow::ensure!(
        !dataset.loci.is_empty(),
        "dataset {} has no loci",
        dataset.id
    );
    let alignment = base.join(&dataset.alignment);
    let index = base.join(&dataset.index);
    verify_sha256(&alignment, &dataset.alignment_sha256)?;
    verify_sha256(&index, &dataset.index_sha256)?;
    let reference = dataset.reference.map(|path| base.join(path));
    let include_reference = reference.is_some();
    match (&reference, &dataset.reference_sha256) {
        (Some(path), Some(expected)) => verify_sha256(path, expected)?,
        (Some(_), None) => anyhow::bail!("dataset {} reference has no SHA-256", dataset.id),
        (None, Some(_)) => anyhow::bail!("dataset {} has a reference hash but no path", dataset.id),
        (None, None) => {}
    }
    let opened = Instant::now();
    let mut source = LocalAlignmentSource::open(
        &alignment,
        &AlignmentOpenOptions {
            index_path: Some(index),
            reference_path: reference.clone(),
            ..AlignmentOpenOptions::default()
        },
    )?;
    let reference_reader = reference.as_ref().map(FastaReference::open).transpose()?;
    let open_ms = milliseconds(opened);
    let mut loci = Vec::with_capacity(dataset.loci.len());
    for locus in dataset.loci {
        let interval = parse_locus(source.reference_dictionary(), &locus.region)?;
        let started = Instant::now();
        let contig_name = source
            .reference_dictionary()
            .get(interval.contig())
            .context("alignment reference dictionary is inconsistent")?
            .name()
            .to_owned();
        let mut query = source.query(
            interval,
            QueryOptions {
                detail: QueryDetail::Full,
                max_records: NonZeroUsize::new(250_000).unwrap(),
                include_reference,
                ..QueryOptions::default()
            },
            &|| true,
        )?;
        if let Some(reference_reader) = &reference_reader {
            query.reference = reference_reader.fetch_slice_by_name(&contig_name, interval)?;
        }
        let observed = observe(&query);
        check_expectations(&dataset.id, &locus.region, locus.expected, observed)?;
        loci.push(LocusReport {
            region: locus.region,
            query_ms: milliseconds(started),
            observed,
        });
    }
    Ok(DatasetReport {
        id: dataset.id,
        categories: dataset.categories,
        alignment_bytes: std::fs::metadata(alignment)?.len(),
        open_ms,
        loci,
    })
}

fn observe(query: &AlignmentQuery) -> ObservedSemantics {
    let batch = &query.batch;
    let junctions = &query.junctions;
    let mut observed = ObservedSemantics {
        records_seen: query.stats.records_seen,
        records_emitted: query.stats.records_emitted,
        junctions_distinct: junctions.junctions.len(),
        junction_depth: junctions
            .junctions
            .iter()
            .map(|junction| u64::from(junction.depth))
            .sum(),
        junction_xs_tagged: junctions.provenance.xs_tagged_observations,
        junction_alignment_strand_fallback: junctions
            .provenance
            .alignment_strand_fallback_observations,
        junction_invalid_xs: junctions.provenance.invalid_xs_observations,
        ..ObservedSemantics::default()
    };
    for index in 0..batch.len() {
        let cigar = batch.cigar(index).unwrap_or_default();
        observed.spliced += usize::from(
            cigar
                .iter()
                .any(|operation| operation.kind == CigarOpKind::ReferenceSkip),
        );
        observed.secondary += usize::from(batch.flags[index].is_secondary());
        observed.supplementary += usize::from(batch.flags[index].is_supplementary());
        observed.paired += usize::from(batch.flags[index].bits() & 0x1 != 0);
        observed.long_cigar += usize::from(cigar.len() > u16::MAX as usize);
        observed.modified_bases += usize::from(
            batch.has_auxiliary_tag(index, *b"MM") || batch.has_auxiliary_tag(index, *b"Mm"),
        );
        let has_cell_barcode =
            batch.has_auxiliary_tag(index, *b"CB") || batch.has_auxiliary_tag(index, *b"CR");
        let has_umi =
            batch.has_auxiliary_tag(index, *b"UB") || batch.has_auxiliary_tag(index, *b"UR");
        observed.single_cell_tags += usize::from(has_cell_barcode && has_umi);
        match batch.sequence(index) {
            Some(SequenceView::Missing) => {
                observed.missing_sequence += 1;
                observed.missing_quality += 1;
            }
            Some(SequenceView::Present { bases, qualities }) => {
                observed.long_read += usize::from(bases.len() >= 10_000);
                observed.missing_quality += usize::from(qualities.is_none());
            }
            Some(SequenceView::NotLoaded) | None => {}
        }
        if let Ok(record) = batch.materialize_record(index) {
            observe_specialist_record(&record, query, index, &mut observed);
        }
    }
    observed
}

fn observe_specialist_record(
    record: &AlignmentRecord,
    query: &AlignmentQuery,
    index: usize,
    observed: &mut ObservedSemantics,
) {
    if let Ok(summary) = summarize_sbx_record(record, 10, 30)
        && summary
            .left_tail_bases
            .saturating_add(summary.right_tail_bases)
            > 0
        && summary
            .concordant_insertions
            .saturating_add(summary.partly_discordant_insertions)
            .saturating_add(summary.fully_discordant_insertions)
            > 0
    {
        observed.sbx_evidence += 1;
    }
    let kinetic_modes = [
        SmrtKineticsMode::SubreadIpd,
        SmrtKineticsMode::SubreadPulseWidth,
        SmrtKineticsMode::CcsForwardIpd,
        SmrtKineticsMode::CcsForwardPulseWidth,
        SmrtKineticsMode::CcsReverseIpd,
        SmrtKineticsMode::CcsReversePulseWidth,
    ];
    observed.smrt_kinetics += usize::from(
        kinetic_modes
            .into_iter()
            .any(|mode| smrt_kinetics_series(record, mode).is_ok_and(|series| !series.is_empty())),
    );
    if let Some(reference) = query.reference.as_ref() {
        let contexts = [
            BisulfiteContext::Cg,
            BisulfiteContext::Chg,
            BisulfiteContext::Chh,
            BisulfiteContext::Hcg,
            BisulfiteContext::Gch,
            BisulfiteContext::Wcg,
        ];
        observed.bisulfite_evidence += usize::from(contexts.into_iter().any(|context| {
            summarize_bisulfite_record(record, reference, context).context_observations() > 0
        }));
    }
    let maximum_homopolymer_length = query
        .read_group(index)
        .and_then(|group| group.maximum_homopolymer_length);
    if let Ok(analysis) = analyze_ultima_record(
        record,
        query.reference.as_ref(),
        UltimaQualityMode::Directional,
        maximum_homopolymer_length,
    ) && analysis.summary.quality_events > 0
    {
        observed.ultima_evidence += 1;
    }
}

fn check_expectations(
    dataset: &str,
    locus: &str,
    expected: ExpectedSemantics,
    observed: ObservedSemantics,
) -> Result<()> {
    anyhow::ensure!(
        observed.records_seen >= expected.minimum_records_seen,
        "{dataset} {locus}: saw {} records, expected at least {}",
        observed.records_seen,
        expected.minimum_records_seen
    );
    anyhow::ensure!(
        observed.records_emitted >= expected.minimum_records_emitted,
        "{dataset} {locus}: emitted {} records, expected at least {}",
        observed.records_emitted,
        expected.minimum_records_emitted
    );
    for feature in expected.required {
        let (count, name) = match feature {
            SemanticFeature::Spliced => (observed.spliced, "spliced alignment"),
            SemanticFeature::Secondary => (observed.secondary, "secondary alignment"),
            SemanticFeature::Supplementary => (observed.supplementary, "supplementary alignment"),
            SemanticFeature::Paired => (observed.paired, "paired alignment"),
            SemanticFeature::LongRead => (observed.long_read, "long read"),
            SemanticFeature::LongCigar => (observed.long_cigar, "long CIGAR"),
            SemanticFeature::ModifiedBases => (observed.modified_bases, "modified-base tags"),
            SemanticFeature::MissingSequence => (observed.missing_sequence, "missing sequence"),
            SemanticFeature::MissingQuality => (observed.missing_quality, "missing quality"),
            SemanticFeature::BisulfiteEvidence => {
                (observed.bisulfite_evidence, "bisulfite context evidence")
            }
            SemanticFeature::SmrtKinetics => (observed.smrt_kinetics, "PacBio SMRT kinetic arrays"),
            SemanticFeature::SingleCellTags => {
                (observed.single_cell_tags, "cell-barcode and UMI tags")
            }
            SemanticFeature::SbxEvidence => (observed.sbx_evidence, "SBX quality evidence"),
            SemanticFeature::UltimaEvidence => {
                (observed.ultima_evidence, "Ultima flow-indel evidence")
            }
        };
        anyhow::ensure!(count > 0, "{dataset} {locus}: missing {name}");
    }
    if let Some(junctions) = expected.junctions {
        anyhow::ensure!(
            observed.junctions_distinct == junctions.distinct,
            "{dataset} {locus}: observed {} distinct junctions, expected {}",
            observed.junctions_distinct,
            junctions.distinct
        );
        anyhow::ensure!(
            observed.junction_depth == junctions.total_depth,
            "{dataset} {locus}: observed total junction depth {}, expected {}",
            observed.junction_depth,
            junctions.total_depth
        );
        anyhow::ensure!(
            observed.junction_xs_tagged == junctions.xs_tagged
                && observed.junction_alignment_strand_fallback
                    == junctions.alignment_strand_fallback
                && observed.junction_invalid_xs == junctions.invalid_xs,
            "{dataset} {locus}: junction strand provenance was XS {}, fallback {}, invalid {}; expected XS {}, fallback {}, invalid {}",
            observed.junction_xs_tagged,
            observed.junction_alignment_strand_fallback,
            observed.junction_invalid_xs,
            junctions.xs_tagged,
            junctions.alignment_strand_fallback,
            junctions.invalid_xs,
        );
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    anyhow::ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid SHA-256 in manifest for {}",
        path.display()
    );
    let mut reader = BufReader::new(
        File::open(path).with_context(|| format!("could not open {}", path.display()))?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = hasher.finalize();
    let mut actual = String::with_capacity(64);
    for byte in digest {
        write!(actual, "{byte:02x}").expect("writing to a String cannot fail");
    }
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(expected),
        "SHA-256 mismatch for {}: expected {expected}, found {actual}",
        path.display()
    );
    Ok(())
}

fn parse_locus(dictionary: &ReferenceDictionary, locus: &str) -> Result<GenomicInterval> {
    let (name, range) = locus
        .split_once(':')
        .with_context(|| format!("locus must use name:start-end, got {locus}"))?;
    let (start, end) = range
        .replace(',', "")
        .split_once('-')
        .map(|(start, end)| (start.to_owned(), end.to_owned()))
        .with_context(|| format!("locus must use name:start-end, got {locus}"))?;
    let start: u64 = start.parse()?;
    let end: u64 = end.parse()?;
    anyhow::ensure!(start > 0 && end >= start, "invalid locus {locus}");
    let contig = dictionary
        .id_by_name(name)
        .with_context(|| format!("unknown contig {name}"))?;
    let length = dictionary
        .get(contig)
        .context("reference dictionary is inconsistent")?
        .length();
    Ok(GenomicInterval::bounded(contig, start - 1, end, length)?)
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantics_expectations_report_missing_features() {
        let result = check_expectations(
            "fixture",
            "chr1:1-10",
            ExpectedSemantics {
                required: vec![SemanticFeature::Spliced],
                ..ExpectedSemantics::default()
            },
            ObservedSemantics::default(),
        );
        assert!(result.unwrap_err().to_string().contains("spliced"));
    }

    #[test]
    fn complete_matrix_has_all_documented_categories() {
        assert_eq!(REQUIRED_CATEGORIES.len(), 14);
        assert!(REQUIRED_CATEGORIES.contains(&"ont"));
        assert!(REQUIRED_CATEGORIES.contains(&"pacbio-smrt-kinetics"));
        assert!(REQUIRED_CATEGORIES.contains(&"sbx"));
        assert!(REQUIRED_CATEGORIES.contains(&"ultima"));
        assert!(REQUIRED_CATEGORIES.contains(&"reference-dependent-cram"));
        assert!(REQUIRED_CATEGORIES.contains(&"sv-heavy"));
    }

    #[test]
    fn complete_matrix_requires_specialist_semantic_evidence_not_only_labels() {
        let mut dataset = complete_matrix_dataset();
        dataset.loci[0]
            .expected
            .required
            .retain(|feature| *feature != SemanticFeature::SmrtKinetics);

        let error = validate_complete_matrix(&[dataset]).unwrap_err();

        assert!(error.to_string().contains("pacbio-smrt-kinetics"));
    }

    #[test]
    fn complete_matrix_accepts_every_declared_semantic_and_cram_requirement() {
        validate_complete_matrix(&[complete_matrix_dataset()]).unwrap();
    }

    #[test]
    fn reference_dependent_cram_category_requires_a_cram_source() {
        let mut dataset = complete_matrix_dataset();
        dataset.alignment = PathBuf::from("representative.bam");

        let error = validate_complete_matrix(&[dataset]).unwrap_err();

        assert!(error.to_string().contains("reference-dependent-cram"));
    }

    fn complete_matrix_dataset() -> DatasetSpec {
        DatasetSpec {
            id: "complete".to_owned(),
            categories: REQUIRED_CATEGORIES
                .iter()
                .map(|category| (*category).to_owned())
                .collect(),
            alignment: PathBuf::from("representative.cram"),
            alignment_sha256: "0".repeat(64),
            index: PathBuf::from("representative.cram.crai"),
            index_sha256: "0".repeat(64),
            reference: Some(PathBuf::from("reference.fa")),
            reference_sha256: Some("0".repeat(64)),
            loci: vec![LocusSpec {
                region: "chr1:1-100000".to_owned(),
                expected: ExpectedSemantics {
                    minimum_records_seen: 100_000,
                    minimum_records_emitted: 1,
                    required: vec![
                        SemanticFeature::Paired,
                        SemanticFeature::LongRead,
                        SemanticFeature::ModifiedBases,
                        SemanticFeature::SmrtKinetics,
                        SemanticFeature::Spliced,
                        SemanticFeature::BisulfiteEvidence,
                        SemanticFeature::SingleCellTags,
                        SemanticFeature::Supplementary,
                        SemanticFeature::SbxEvidence,
                        SemanticFeature::UltimaEvidence,
                    ],
                    junctions: None,
                },
            }],
        }
    }
}
