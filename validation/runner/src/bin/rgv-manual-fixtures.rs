#![allow(
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::type_complexity
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use flate2::{Compression, write::GzEncoder};
use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentSortMode, BisulfiteContext,
    GENOME_CONFIGURATION_SCHEMA_VERSION, GenomeAnnotation, GenomeConfiguration,
    JunctionDisplayMode, PanelId, QuantitativeArithmetic, QuantitativeScaleMode,
    ReadBaseDisplayMode, ReferenceTranslationFrame, RegionOfInterest, ReviewItem, ReviewStatus,
    SashimiScaleMode, SbxDisplayOptions, SbxSimplexTailMode, SessionAlignmentTrack,
    SessionDocument, SessionLocus, SessionViewPanel, SmrtKineticsMode, TrackConfig, TrackFormat,
    TrackId, TrackKind, TrackSource, UltimaDisplayOptions, UltimaQualityMode,
};
use rgv_validation_runner::{
    CoverageCatalog, CoverageDisposition, CoverageEntry, CoverageSource, DEFAULT_SUITE_ROOT,
    DEFAULT_VALIDATION_ROOT, FixtureArtifact, FixtureDependency, FixtureEndpointDocument,
    FixtureManifest, FixtureProject, FixtureValidity, MANUAL_SUITE_SCHEMA_VERSION, TruthDocument,
    TruthLocus, artifact_for, dependency_for, markdown_table_claims, read_json, sha256_bytes,
    sha256_file, summarize, verify_manual_suite, workspace_root, write_json, write_text,
};
use rust_htslib::bam::{
    self, Format, Header, Read as BamRead, Reader, Record, Writer,
    header::HeaderRecord,
    record::{Aux, Cigar, CigarString},
};
use rust_htslib::bcf::{
    Format as BcfFormat, Header as BcfHeader, Writer as BcfWriter, index as bcf_index,
    record::GenotypeAllele,
};

const CORPUS_SIZE_BUDGET_BYTES: u64 = 50 * 1024 * 1024;
const MANUAL_CONTIG: &str = "chrManual";
const MANUAL_CONTIG_LENGTH: usize = 100_000;

#[derive(Debug, Parser)]
#[command(about = "Generate, verify, and list the complete RGV manual fixture suite")]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, default_value = DEFAULT_SUITE_ROOT)]
    suite_root: PathBuf,
    #[arg(long, default_value = DEFAULT_VALIDATION_ROOT)]
    validation_root: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate every declared manual fixture and run the structural verifier.
    Generate,
    /// Verify the current suite without rewriting it.
    Verify {
        /// Reject any coverage row that remains planned.
        #[arg(long)]
        require_complete: bool,
    },
    /// Print the focused projects and copyable release-mode launch commands.
    List,
    /// Generate twice in disposable workspace directories and exercise determinism/tamper gates.
    Qualify,
    /// Materialize runtime remote projects from a running fixture-server endpoint document.
    MaterializeRemote {
        #[arg(long)]
        endpoint: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[path = "rgv-manual-fixtures/definitions.rs"]
mod definitions;
pub(crate) use definitions::{PROJECTS, SCENARIOS};
#[path = "rgv-manual-fixtures/qualification.rs"]
mod qualification;
use qualification::qualify_generation;
#[path = "rgv-manual-fixtures/projects.rs"]
mod projects;
pub(crate) use projects::{base_document, generate, local_source, write_session};
#[path = "rgv-manual-fixtures/support.rs"]
mod support;
pub(crate) use support::{
    write_rna_bams, write_sbx_bam, write_smrt_bam, write_specialist_bam, write_specialist_cram,
    write_specialist_reference, write_support_files, write_ultima_bam, write_wrong_reference,
};
#[path = "rgv-manual-fixtures/variants.rs"]
mod variants;
pub(crate) use variants::{
    basic_record, basic_record_with_quality, repeating_bases, write_cohort_variants,
};
#[path = "rgv-manual-fixtures/truth.rs"]
mod truth;
pub(crate) use truth::write_truth_documents;
#[path = "rgv-manual-fixtures/scenarios.rs"]
mod scenarios;
pub(crate) use scenarios::{
    write_automation_batches, write_scenarios, write_server_profiles, write_validation_index,
};
#[path = "rgv-manual-fixtures/catalog.rs"]
mod catalog;
pub(crate) use catalog::{coverage_catalog, dependencies, generated_artifacts, project_manifest};

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let root = workspace_root();
    let suite_root = root.join(&arguments.suite_root);
    let validation_root = root.join(&arguments.validation_root);
    match arguments.command.unwrap_or(Command::Generate) {
        Command::Generate => generate(&root, &suite_root, &validation_root),
        Command::Verify { require_complete } => {
            let summary = verify_manual_suite(
                &root,
                &suite_root.join("manifest.json"),
                &validation_root.join("coverage-catalog.json"),
                require_complete,
            )?;
            println!("{}", summarize(&summary));
            if !summary.incomplete_coverage_entries.is_empty() {
                println!(
                    "planned coverage: {}",
                    summary.incomplete_coverage_entries.join(", ")
                );
            }
            Ok(())
        }
        Command::List => list_projects(&root, &suite_root),
        Command::Qualify => qualify_generation(&root),
        Command::MaterializeRemote { endpoint, output } => {
            materialize_remote_projects(&endpoint, &output)
        }
    }
}

fn list_projects(root: &Path, suite_root: &Path) -> Result<()> {
    let manifest: FixtureManifest = read_json(&suite_root.join("manifest.json"))?;
    for project in manifest.projects {
        println!("{} — {}", project.id, project.title);
        println!(
            "  cargo run --release -p rgv-desktop -- --session {}",
            root.join(&project.path).strip_prefix(root)?.display()
        );
        println!("  initial locus: {}", project.initial_locus);
    }
    Ok(())
}

fn materialize_remote_projects(endpoint_path: &Path, output: &Path) -> Result<()> {
    let endpoint: FixtureEndpointDocument = read_json(endpoint_path)?;
    ensure!(
        endpoint.schema_version == 1,
        "unsupported fixture endpoint schema"
    );
    fs::create_dir_all(output)?;
    let url = |id: &str| -> Result<String> {
        endpoint
            .fixture_urls
            .get(id)
            .cloned()
            .with_context(|| format!("runtime endpoint has no fixture URL for {id}"))
    };
    let remote = |id: &str| -> Result<TrackSource> { Ok(TrackSource::Remote { url: url(id)? }) };

    let mut bam = base_document("chr1", 900_000, 920_000, "RGV loopback remote BAM");
    let mut bam_track = SessionAlignmentTrack::new(
        TrackId::new(1),
        "Loopback remote BAM".to_owned(),
        remote("dep-mixed-bam")?,
    );
    bam_track.remote_index_url = Some(url("dep-mixed-bai")?);
    bam.alignment_tracks.push(bam_track);
    write_session(&output.join("mf-14-remote-bam.rgv.json"), &bam)?;

    let mut cram = base_document(
        MANUAL_CONTIG,
        900,
        5_400,
        "RGV loopback reference-dependent CRAM",
    );
    cram.reference = Some(remote("manual-reference-fasta")?);
    cram.remote_reference_index_url = Some(url("manual-reference-fai")?);
    let mut cram_track = SessionAlignmentTrack::new(
        TrackId::new(1),
        "Loopback remote CRAM".to_owned(),
        remote("manual-reference-cram")?,
    );
    cram_track.remote_index_url = Some(url("manual-reference-crai")?);
    cram.alignment_tracks.push(cram_track);
    write_session(&output.join("mf-14-remote-cram.rgv.json"), &cram)?;

    let mut indexed = base_document(
        "chrVariants",
        8_500,
        18_100,
        "RGV loopback remote indexed tracks",
    );
    indexed.reference = Some(remote("dep-variants-fasta")?);
    indexed.remote_reference_index_url = Some(url("dep-variants-fai")?);
    for (offset, (name, fixture, kind, format)) in [
        (
            "Remote cohort BCF",
            "manual-cohort-bcf",
            TrackKind::Variant,
            None,
        ),
        (
            "Remote BigWig",
            "dep-demo-signal.bigWig",
            TrackKind::Quantitative,
            None,
        ),
        (
            "Remote BigBed",
            "dep-demo-features.bigBed",
            TrackKind::Feature,
            None,
        ),
        (
            "Remote bigGenePred",
            "dep-demo-genes.bigGenePred",
            TrackKind::GeneModel,
            Some(TrackFormat::BigGenePred),
        ),
        (
            "Remote bigNarrowPeak",
            "dep-demo-peaks.bigNarrowPeak",
            TrackKind::Feature,
            Some(TrackFormat::BigNarrowPeak),
        ),
        (
            "Remote TDF",
            "dep-demo-archive.tdf",
            TrackKind::Quantitative,
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut track = TrackConfig::new(
            TrackId::new(u64::try_from(offset)? + 1),
            name.to_owned(),
            kind,
            remote(fixture)?,
        );
        track.format = format;
        indexed.tracks.push(track);
    }
    write_session(&output.join("mf-14-remote-indexed.rgv.json"), &indexed)?;

    let mut missing_index = bam.clone();
    missing_index.genome_name = Some("RGV expected missing remote BAM index".to_owned());
    missing_index.alignment_tracks[0].remote_index_url = Some(format!(
        "{}/fixture/not-declared/missing.bai",
        endpoint.base_url
    ));
    write_session(
        &output.join("mf-14-failure-missing-index.rgv.json"),
        &missing_index,
    )?;
    let mut corrupt_index = bam.clone();
    corrupt_index.genome_name = Some("RGV expected corrupt remote BAM index".to_owned());
    corrupt_index.alignment_tracks[0].remote_index_url = Some(url("failure-truncated-bed")?);
    write_session(
        &output.join("mf-14-failure-corrupt-index.rgv.json"),
        &corrupt_index,
    )?;
    let mut wrong_reference = cram.clone();
    wrong_reference.genome_name = Some("RGV expected wrong remote CRAM reference".to_owned());
    wrong_reference.reference = Some(remote("failure-wrong-reference-fasta")?);
    wrong_reference.remote_reference_index_url = Some(url("failure-wrong-reference-fai")?);
    write_session(
        &output.join("mf-14-failure-wrong-reference.rgv.json"),
        &wrong_reference,
    )?;
    let mut isolation = bam.clone();
    let mut broken = isolation.alignment_tracks[0].clone();
    broken.id = TrackId::new(2);
    "EXPECTED broken remote index".clone_into(&mut broken.name);
    broken.remote_index_url = Some(format!(
        "{}/fixture/not-declared/missing.bai",
        endpoint.base_url
    ));
    isolation.alignment_tracks.push(broken);
    write_session(&output.join("mf-14-failure-isolation.rgv.json"), &isolation)?;
    println!("materialized remote projects in {}", output.display());
    for filename in [
        "mf-14-remote-bam.rgv.json",
        "mf-14-remote-cram.rgv.json",
        "mf-14-remote-indexed.rgv.json",
        "mf-14-failure-missing-index.rgv.json",
        "mf-14-failure-corrupt-index.rgv.json",
        "mf-14-failure-wrong-reference.rgv.json",
        "mf-14-failure-isolation.rgv.json",
    ] {
        let text = fs::read_to_string(output.join(filename))?;
        ensure!(
            !text.contains(&endpoint.bearer_token) && !text.contains(&endpoint.control_secret),
            "runtime project {filename} persisted a fixture-server credential"
        );
        SessionDocument::read(output.join(filename))?;
        println!(
            "cargo run --release -p rgv-desktop -- --session {}",
            output.join(filename).display()
        );
    }
    Ok(())
}
