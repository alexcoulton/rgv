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
    AlignmentColorMode, AlignmentGroupMode, BisulfiteContext, GENOME_CONFIGURATION_SCHEMA_VERSION,
    GenomeAnnotation, GenomeConfiguration, JunctionDisplayMode, PanelId, QuantitativeArithmetic,
    QuantitativeScaleMode, ReadBaseDisplayMode, ReferenceTranslationFrame, RegionOfInterest,
    ReviewItem, ReviewStatus, SashimiScaleMode, SbxDisplayOptions, SbxSimplexTailMode,
    SessionAlignmentTrack, SessionDocument, SessionLocus, SessionViewPanel, SmrtKineticsMode,
    TrackConfig, TrackFormat, TrackId, TrackKind, TrackSource, UltimaDisplayOptions,
    UltimaQualityMode,
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

#[derive(Clone, Copy)]
struct ProjectDefinition {
    id: &'static str,
    filename: &'static str,
    title: &'static str,
    purpose: &'static str,
    initial_locus: &'static str,
    scenario_id: &'static str,
}

const PROJECTS: &[ProjectDefinition] = &[
    ProjectDefinition {
        id: "mf-01-alignment-core",
        filename: "mf-01-alignment-core.rgv.json",
        title: "Core alignments and navigation",
        purpose: "paired, deep, structural, long-read, modification, grouping, sorting, and navigation review",
        initial_locus: "chr1:2590001-2650000",
        scenario_id: "scenario-mf-01",
    },
    ProjectDefinition {
        id: "mf-02-alignment-events",
        filename: "mf-02-alignment-events.rgv.json",
        title: "Alignment events and reference bases",
        purpose: "exact base, coverage, CIGAR, small-indel, and large-indel truth",
        initial_locus: "chrVariants:9401-12600",
        scenario_id: "scenario-mf-02",
    },
    ProjectDefinition {
        id: "mf-03-rna-sashimi",
        filename: "mf-03-rna-sashimi.rgv.json",
        title: "RNA and Sashimi",
        purpose: "spliced-read, strand-evidence, junction-count, scale, and export review",
        initial_locus: "chr1:2100001-2301000",
        scenario_id: "scenario-mf-03",
    },
    ProjectDefinition {
        id: "mf-04-bisulfite",
        filename: "mf-04-bisulfite.rgv.json",
        title: "Bisulfite contexts",
        purpose: "forward/reverse converted and protected cytosines in preselected contexts",
        initial_locus: "chrManual:901-1200",
        scenario_id: "scenario-mf-04",
    },
    ProjectDefinition {
        id: "mf-05-smrt-kinetics",
        filename: "mf-05-smrt-kinetics.rgv.json",
        title: "PacBio SMRT kinetics",
        purpose: "subread and CCS kinetics with the physical evidence mode preselected",
        initial_locus: "chr2:1-2000000",
        scenario_id: "scenario-mf-05",
    },
    ProjectDefinition {
        id: "mf-06-sbx",
        filename: "mf-06-sbx.rgv.json",
        title: "SBX insertion and tail evidence",
        purpose: "concordant/discordant insertions and simplex tail presentation",
        initial_locus: "chr2:1-2000000",
        scenario_id: "scenario-mf-06",
    },
    ProjectDefinition {
        id: "mf-07-ultima",
        filename: "mf-07-ultima.rgv.json",
        title: "Ultima flow-indel evidence",
        purpose: "directional and minimum Ultima evidence with a matching reference",
        initial_locus: "chrManual:49901-50600",
        scenario_id: "scenario-mf-07",
    },
    ProjectDefinition {
        id: "mf-08-single-cell",
        filename: "mf-08-single-cell.rgv.json",
        title: "Tagged single-cell alignments",
        purpose: "RG/CB/CR/UB/UR grouping, sorting, filtering, metadata, and persistence",
        initial_locus: "chrManual:4901-5400",
        scenario_id: "scenario-mf-08",
    },
    ProjectDefinition {
        id: "mf-09-cohort-variants",
        filename: "mf-09-cohort-variants.rgv.json",
        title: "Cohort variants and genotypes",
        purpose: "typed SNV/MNV/indel/SV/BND evidence, genotypes, samples, filters, and export",
        initial_locus: "chrVariants:14901-18100",
        scenario_id: "scenario-mf-09",
    },
    ProjectDefinition {
        id: "mf-10-mixed-tracks",
        filename: "mf-10-mixed-tracks.rgv.json",
        title: "Mixed auxiliary tracks",
        purpose: "all supported auxiliary formats, typed settings, overlays, arithmetic, and interaction",
        initial_locus: "chrVariants:8501-16000",
        scenario_id: "scenario-mf-10",
    },
    ProjectDefinition {
        id: "mf-11-genome-review",
        filename: "mf-11-genome-review.rgv.json",
        title: "Genome, search, translation, ROI, and review",
        purpose: "portable genome semantics, aliases, six-frame translation, ROI, and review workflow",
        initial_locus: "chrVariants:8501-12500",
        scenario_id: "scenario-mf-11",
    },
    ProjectDefinition {
        id: "mf-12-workspaces",
        filename: "mf-12-workspaces.rgv.json",
        title: "Multi-track and multi-panel workspace",
        purpose: "track identity, bulk actions, linked/independent panels, failure isolation, and relocation",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-14-reference-cram",
        filename: "mf-14-reference-cram.rgv.json",
        title: "Reference-dependent CRAM",
        purpose: "local CRAM decoding with matching and deliberately wrong reference behavior",
        initial_locus: "chrManual:901-5400",
        scenario_id: "scenario-mf-14",
    },
    ProjectDefinition {
        id: "mf-12-four-track",
        filename: "mf-12-four-track.rgv.json",
        title: "Four-track alignment workspace",
        purpose: "compact multi-sample identity, bulk-operation, and persistence review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-12-eight-track",
        filename: "mf-12-eight-track.rgv.json",
        title: "Eight-track alignment workspace",
        purpose: "larger multi-sample scrolling, ordering, metadata, and persistence review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-12-failure-isolation",
        filename: "mf-12-failure-isolation.rgv.json",
        title: "Workspace source failure isolation",
        purpose: "one deliberately missing alignment source alongside healthy tracks and panels",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-13-recovery",
        filename: "mf-13-recovery.rgv.json",
        title: "Sessions, imports, and recovery",
        purpose: "native/IGV compatibility, autosave discovery, partial workspace, and recovery review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-13",
    },
];

#[derive(Clone, Copy)]
struct ScenarioDefinition {
    id: &'static str,
    title: &'static str,
    project_id: Option<&'static str>,
    expected: &'static [&'static str],
}

const SCENARIOS: &[ScenarioDefinition] = &[
    scenario(
        "scenario-mf-01",
        "Core alignments and navigation",
        Some("mf-01-alignment-core"),
        &[
            "Structural pair classes are visible and inspectable.",
            "Horizontal navigation does not move the read stack vertically.",
            "Reference and coverage stay fixed while the read stack scrolls.",
        ],
    ),
    scenario(
        "scenario-mf-02",
        "Alignment events and reference bases",
        Some("mf-02-alignment-events"),
        &[
            "Small and large insertions/deletions remain anchored at their named positions.",
            "Reference and read letters remain legible and use stable event colours.",
            "I1000 denotes a 1,000-base insertion between reference bases.",
        ],
    ),
    scenario(
        "scenario-mf-03",
        "RNA and Sashimi",
        Some("mf-03-rna-sashimi"),
        &[
            "Sample A and B show exact shared, sample-specific, alternative, and multi-exon junction counts.",
            "Annotated and unannotated junctions plus valid, inferred, and invalid strand evidence remain distinct.",
        ],
    ),
    scenario(
        "scenario-mf-04",
        "Bisulfite contexts",
        Some("mf-04-bisulfite"),
        &[
            "The initial track is already in CG bisulfite mode.",
            "Converted and protected reads are distinguishable on both strands.",
        ],
    ),
    scenario(
        "scenario-mf-05",
        "PacBio SMRT kinetics",
        Some("mf-05-smrt-kinetics"),
        &[
            "The initial track is already in subread IPD mode.",
            "Kinetic values remain attached to bases through zoom and reverse orientation.",
        ],
    ),
    scenario(
        "scenario-mf-06",
        "SBX evidence",
        Some("mf-06-sbx"),
        &[
            "The initial track is already in SBX mode.",
            "Insertion categories and highlighted tails remain distinguishable.",
        ],
    ),
    scenario(
        "scenario-mf-07",
        "Ultima evidence",
        Some("mf-07-ultima"),
        &[
            "The initial track is already in directional Ultima mode.",
            "Valid forward/reverse tp, read-group mc, and t0 evidence plus three explicit error controls remain distinguishable.",
        ],
    ),
    scenario(
        "scenario-mf-08",
        "Tagged single-cell alignments",
        Some("mf-08-single-cell"),
        &[
            "Tracks are grouped by corrected cell barcode at startup.",
            "CB/CR/UB/UR tags and linked sample metadata are visible in inspection.",
        ],
    ),
    scenario(
        "scenario-mf-09",
        "Cohort variants and genotypes",
        Some("mf-09-cohort-variants"),
        &[
            "SNV, multi-allelic, insertion, deletion, inversion, symbolic deletion, and BND records are present.",
            "All 128 samples retain phased, unphased, missing, partial no-call, and DP fields.",
        ],
    ),
    scenario(
        "scenario-mf-10",
        "Mixed auxiliary tracks",
        Some("mf-10-mixed-tracks"),
        &[
            "Every supported generated auxiliary source queries non-empty evidence.",
            "Both TDF channels open independently and retain channel provenance.",
        ],
    ),
    scenario(
        "scenario-mf-11",
        "Genome and review",
        Some("mf-11-genome-review"),
        &[
            "Aliases, six translation frames, ROI entries, and review items restore.",
            "The portable genome bundle resolves its relative reference, cytobands, and annotation.",
        ],
    ),
    scenario(
        "scenario-mf-12",
        "Workspaces",
        Some("mf-12-workspaces"),
        &[
            "Four alignment tracks retain stable IDs across two panels.",
            "The intentionally missing source fails locally while healthy tracks remain usable.",
        ],
    ),
    scenario(
        "scenario-mf-13",
        "Sessions, IGV import, and recovery",
        Some("mf-13-recovery"),
        &[
            "Native schema versions migrate or reject exactly as declared.",
            "IGV import reports exact, approximated, and ignored items explicitly.",
        ],
    ),
    scenario(
        "scenario-mf-14",
        "Remote sources and failure isolation",
        None,
        &[
            "Healthy range requests are bounded and semantically match local sources.",
            "Auth, outage, corruption, validator, and retry profiles fail or recover deterministically.",
        ],
    ),
    scenario(
        "scenario-mf-15",
        "Automation, export, and accessibility",
        None,
        &[
            "Automation reports each command with stable identity and status.",
            "Keyboard focus and accessible names expose every common workflow.",
        ],
    ),
];

const fn scenario(
    id: &'static str,
    title: &'static str,
    project_id: Option<&'static str>,
    expected: &'static [&'static str],
) -> ScenarioDefinition {
    ScenarioDefinition {
        id,
        title,
        project_id,
        expected,
    }
}

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

struct QualificationOutput {
    suite: PathBuf,
    validation: PathBuf,
}

impl Drop for QualificationOutput {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.suite);
        let _ = fs::remove_dir_all(&self.validation);
    }
}

fn qualify_generation(root: &Path) -> Result<()> {
    let suffix = std::process::id();
    let output = QualificationOutput {
        suite: root.join(format!(
            "benchmark-data/manual-suite-qualification-{suffix}"
        )),
        validation: root.join(format!("validation/manual-suite-qualification-{suffix}")),
    };
    ensure!(
        !output.suite.exists() && !output.validation.exists(),
        "qualification output already exists; remove only the named stale qualification directories"
    );
    generate(root, &output.suite, &output.validation)?;
    let first = declared_tree_digest(root, &output.suite, &output.validation)?;
    generate(root, &output.suite, &output.validation)?;
    let second = declared_tree_digest(root, &output.suite, &output.validation)?;
    ensure!(first == second, "two clean fixture generations differed");

    let tampered = output.suite.join("data/tracks/default-annotations.bed");
    fs::OpenOptions::new()
        .append(true)
        .open(&tampered)?
        .write_all(b"# controlled tamper\n")?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted changed bytes"
    );
    generate(root, &output.suite, &output.validation)?;
    let undeclared = output.suite.join("failures/UNDECLARED.stale");
    write_text(&undeclared, "controlled stale output\n")?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted undeclared stale output"
    );
    fs::remove_file(&undeclared)?;
    fs::remove_file(&tampered)?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted a missing declared artifact"
    );
    println!(
        "manual fixture generation qualification passed: deterministic tree {first}, tamper/stale/missing rejection, relocation, production semantics, and size budget"
    );
    Ok(())
}

fn declared_tree_digest(root: &Path, suite: &Path, validation: &Path) -> Result<String> {
    let manifest: FixtureManifest = read_json(&suite.join("manifest.json"))?;
    let mut lines = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            format!(
                "artifact\t{}\t{}\t{}",
                artifact.id, artifact.path, artifact.sha256
            )
        })
        .chain(manifest.dependencies.iter().map(|dependency| {
            format!(
                "dependency\t{}\t{}\t{}",
                dependency.id, dependency.path, dependency.sha256
            )
        }))
        .collect::<Vec<_>>();
    lines.push(format!(
        "manifest\t{}",
        sha256_file(&suite.join("manifest.json"))?
    ));
    lines.push(format!(
        "catalog\t{}",
        sha256_file(&validation.join("coverage-catalog.json"))?
    ));
    lines.sort();
    ensure!(
        suite.starts_with(root) && validation.starts_with(root),
        "qualification outputs escaped the workspace"
    );
    Ok(sha256_bytes(lines.join("\n").as_bytes()))
}

fn generate(root: &Path, suite_root: &Path, validation_root: &Path) -> Result<()> {
    for directory in [
        suite_root.join("data/alignments"),
        suite_root.join("data/references"),
        suite_root.join("data/tracks"),
        suite_root.join("data/variants"),
        suite_root.join("genomes"),
        suite_root.join("metadata"),
        suite_root.join("imports"),
        suite_root.join("projects"),
        suite_root.join("expected"),
        suite_root.join("failures"),
        suite_root.join("recovery/autosaves"),
        suite_root.join("relocation/source/data"),
        validation_root.join("scenarios"),
        validation_root.join("batches"),
        validation_root.join("server-profiles"),
    ] {
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
    }

    write_specialist_reference(&suite_root.join("data/references/manual.fa"))?;
    write_wrong_reference(&suite_root.join("failures/wrong-reference.fa"))?;
    write_rna_bams(&suite_root.join("data/alignments"))?;
    write_smrt_bam(&suite_root.join("data/alignments/smrt-evidence.bam"))?;
    write_sbx_bam(&suite_root.join("data/alignments/sbx-evidence.bam"))?;
    write_ultima_bam(&suite_root.join("data/alignments/ultima-evidence.bam"))?;
    write_specialist_bam(&suite_root.join("data/alignments/specialist.bam"))?;
    write_specialist_cram(
        &suite_root.join("data/alignments/specialist.bam"),
        &suite_root.join("data/references/manual.fa"),
        &suite_root.join("data/alignments/reference-dependent.cram"),
    )?;
    write_cohort_variants(
        &suite_root.join("data/variants/cohort.vcf.gz"),
        &suite_root.join("data/variants/cohort.bcf"),
    )?;
    write_support_files(suite_root)?;
    write_projects(suite_root)?;
    write_relocation_bundle(suite_root)?;
    write_truth_documents(suite_root)?;
    write_automation_batches(validation_root)?;
    write_scenarios(root, suite_root, validation_root)?;
    write_server_profiles(validation_root)?;
    write_validation_index(validation_root)?;

    let mut dependencies = dependencies(root)?;
    dependencies.sort_by(|left, right| left.id.cmp(&right.id));
    let mut artifacts = generated_artifacts(root, suite_root, validation_root)?;
    artifacts.sort_by(|left, right| left.id.cmp(&right.id));
    let projects = project_manifest(root, suite_root)?;
    let mut manifest = FixtureManifest {
        schema_version: MANUAL_SUITE_SCHEMA_VERSION,
        generator: "rgv-manual-fixtures/1".to_owned(),
        corpus_size_budget_bytes: CORPUS_SIZE_BUDGET_BYTES,
        artifacts,
        dependencies,
        projects,
    };
    let catalog = coverage_catalog(root, &manifest)?;
    let catalog_path = validation_root.join("coverage-catalog.json");
    write_json(&catalog_path, &catalog)?;
    manifest.artifacts.push(artifact_for(
        root,
        &catalog_path,
        "coverage-catalog",
        "application/vnd.rgv.coverage+json",
        FixtureValidity::Valid,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )?);
    manifest
        .artifacts
        .sort_by(|left, right| left.id.cmp(&right.id));
    write_json(&suite_root.join("manifest.json"), &manifest)?;

    let summary = verify_manual_suite(
        root,
        &suite_root.join("manifest.json"),
        &catalog_path,
        false,
    )?;
    println!("{}", summarize(&summary));
    if !summary.incomplete_coverage_entries.is_empty() {
        println!(
            "foundation generated; {} coverage rows remain planned for semantic closure",
            summary.incomplete_coverage_entries.len()
        );
    }
    Ok(())
}

fn local_source(path: impl Into<String>) -> TrackSource {
    TrackSource::Local { path: path.into() }
}

fn locus(contig: &str, start: u64, end: u64) -> SessionLocus {
    SessionLocus {
        contig: contig.to_owned(),
        start,
        end,
    }
}

fn alignment_track(
    id: u64,
    name: &str,
    source: &str,
    color: AlignmentColorMode,
) -> SessionAlignmentTrack {
    let mut track =
        SessionAlignmentTrack::new(TrackId::new(id), name.to_owned(), local_source(source));
    track.height = 420.0;
    track.filters.alignment_color_mode = color;
    track
}

fn base_document(contig: &str, start: u64, end: u64, name: &str) -> SessionDocument {
    let mut document = SessionDocument::new(locus(contig, start, end));
    document.genome_name = Some(name.to_owned());
    document
}

fn write_projects(suite_root: &Path) -> Result<()> {
    let projects = suite_root.join("projects");

    let mut core = base_document("chr1", 2_590_000, 2_650_000, "RGV core alignment truth");
    let mut core_track = alignment_track(
        1,
        "Paired, structural, long, and modified reads",
        "../../mixed.bam",
        AlignmentColorMode::PairOrientation,
    );
    core_track.filters.view_as_pairs = true;
    core_track.filters.alignment_group_mode = AlignmentGroupMode::PairOrientation;
    core.alignment_tracks.push(core_track);
    core.review_items = vec![
        review_item(1, "Paired short reads", "chr1", 0, 200_000),
        review_item(2, "Deep coverage", "chr1", 900_000, 920_000),
        review_item(3, "Structural pairs", "chr1", 2_590_000, 2_650_000),
        review_item(4, "Long/supplementary/modifications", "chr2", 0, 2_000_000),
        review_item(5, "70,000-operation CIGAR", "chr2", 2_500_000, 2_550_000),
    ];
    write_session(&projects.join(PROJECTS[0].filename), &core)?;

    let mut events = base_document(
        "chrVariants",
        9_400,
        12_600,
        "RGV exact alignment event truth",
    );
    events.reference = Some(local_source("../../variants.fa"));
    let mut event_track = alignment_track(
        1,
        "Exact bases and small/large indels",
        "../../variants.bam",
        AlignmentColorMode::Strand,
    );
    event_track.filters.read_base_display_mode = ReadBaseDisplayMode::All;
    event_track.filters.minimum_event_label_size = 1;
    events.alignment_tracks.push(event_track);
    write_session(&projects.join(PROJECTS[1].filename), &events)?;

    let mut rna = base_document(
        MANUAL_CONTIG,
        19_900,
        21_100,
        "RGV two-sample RNA junction truth",
    );
    rna.reference = Some(local_source("../data/references/manual.fa"));
    let mut rna_track = alignment_track(
        1,
        "RNA sample A: shared and A-specific junctions",
        "../data/alignments/rna-sample-a.bam",
        AlignmentColorMode::Strand,
    );
    rna_track.filters.show_junctions = true;
    rna_track.filters.junction_display_mode = JunctionDisplayMode::Expanded;
    rna_track.filters.sashimi_scale_mode = SashimiScaleMode::Fixed;
    rna_track.filters.sashimi_fixed_scale_maximum = 100;
    rna.alignment_tracks.push(rna_track);
    let mut rna_track = alignment_track(
        2,
        "RNA sample B: shared and B-specific junctions",
        "../data/alignments/rna-sample-b.bam",
        AlignmentColorMode::Strand,
    );
    rna_track.filters.show_junctions = true;
    rna_track.filters.junction_display_mode = JunctionDisplayMode::Collapsed;
    rna_track.filters.sashimi_scale_mode = SashimiScaleMode::Automatic;
    rna.alignment_tracks.push(rna_track);
    rna.tracks.push(TrackConfig::new(
        TrackId::new(3),
        "Annotated RNA transcripts".to_owned(),
        TrackKind::GeneModel,
        local_source("../data/tracks/rna-transcripts.gff3"),
    ));
    write_session(&projects.join(PROJECTS[2].filename), &rna)?;

    let mut bisulfite = base_document(MANUAL_CONTIG, 900, 1_200, "RGV bisulfite truth");
    bisulfite.reference = Some(local_source("../data/references/manual.fa"));
    for (offset, context) in [
        BisulfiteContext::Cg,
        BisulfiteContext::Chg,
        BisulfiteContext::Chh,
        BisulfiteContext::Hcg,
        BisulfiteContext::Gch,
        BisulfiteContext::Wcg,
    ]
    .into_iter()
    .enumerate()
    {
        bisulfite.alignment_tracks.push(alignment_track(
            u64::try_from(offset)? + 1,
            &format!("{context:?} converted/protected evidence"),
            "../data/alignments/specialist.bam",
            AlignmentColorMode::Bisulfite(context),
        ));
    }
    write_session(&projects.join(PROJECTS[3].filename), &bisulfite)?;

    let mut smrt = base_document(
        MANUAL_CONTIG,
        29_900,
        30_600,
        "RGV focused SMRT kinetics truth",
    );
    smrt.reference = Some(local_source("../data/references/manual.fa"));
    for (offset, mode) in [
        SmrtKineticsMode::SubreadIpd,
        SmrtKineticsMode::SubreadPulseWidth,
        SmrtKineticsMode::CcsForwardIpd,
        SmrtKineticsMode::CcsForwardPulseWidth,
        SmrtKineticsMode::CcsReverseIpd,
        SmrtKineticsMode::CcsReversePulseWidth,
    ]
    .into_iter()
    .enumerate()
    {
        smrt.alignment_tracks.push(alignment_track(
            u64::try_from(offset)? + 1,
            &format!("SMRT {mode:?}"),
            "../data/alignments/smrt-evidence.bam",
            AlignmentColorMode::SmrtKinetics(mode),
        ));
    }
    write_session(&projects.join(PROJECTS[4].filename), &smrt)?;

    let mut sbx = base_document(MANUAL_CONTIG, 39_900, 40_600, "RGV focused SBX truth");
    sbx.reference = Some(local_source("../data/references/manual.fa"));
    for (offset, tail_mode) in [
        SbxSimplexTailMode::Highlight,
        SbxSimplexTailMode::Hide,
        SbxSimplexTailMode::Off,
    ]
    .into_iter()
    .enumerate()
    {
        sbx.alignment_tracks.push(alignment_track(
            u64::try_from(offset)? + 1,
            &format!("SBX {tail_mode:?} tails"),
            "../data/alignments/sbx-evidence.bam",
            AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: tail_mode,
                ..SbxDisplayOptions::default()
            }),
        ));
    }
    write_session(&projects.join(PROJECTS[5].filename), &sbx)?;

    let mut ultima = base_document(MANUAL_CONTIG, 49_900, 50_600, "RGV focused Ultima truth");
    ultima.reference = Some(local_source("../data/references/manual.fa"));
    for (offset, quality_mode) in [UltimaQualityMode::Directional, UltimaQualityMode::Minimum]
        .into_iter()
        .enumerate()
    {
        ultima.alignment_tracks.push(alignment_track(
            u64::try_from(offset)? + 1,
            &format!("Ultima {quality_mode:?} flow-indels"),
            "../data/alignments/ultima-evidence.bam",
            AlignmentColorMode::Ultima(UltimaDisplayOptions {
                quality_mode,
                maximum_homopolymer_length_override: None,
            }),
        ));
    }
    write_session(&projects.join(PROJECTS[6].filename), &ultima)?;

    let metadata = fs::read_to_string(suite_root.join("metadata/samples.tsv"))?;
    let mut single_cell =
        base_document(MANUAL_CONTIG, 4_900, 5_400, "RGV tagged single-cell truth");
    single_cell.reference = Some(local_source("../data/references/manual.fa"));
    single_cell.sample_metadata = Some(rgv_core::SampleMetadataTable::from_tsv(&metadata)?);
    let mut cell_track = alignment_track(
        1,
        "Cell and molecule barcodes",
        "../data/alignments/specialist.bam",
        AlignmentColorMode::Tag(*b"CB"),
    );
    cell_track.sample_key = Some("single-cell".to_owned());
    cell_track.filters.alignment_group_mode = AlignmentGroupMode::Tag(*b"CB");
    single_cell.alignment_tracks.push(cell_track);
    write_session(&projects.join(PROJECTS[7].filename), &single_cell)?;

    let mut variants = base_document("chrVariants", 14_900, 18_100, "RGV cohort variant truth");
    variants.reference = Some(local_source("../../variants.fa"));
    let mut variant_track = TrackConfig::new(
        TrackId::new(1),
        "Cohort VCF and 128-sample genotypes".to_owned(),
        TrackKind::Variant,
        local_source("../data/variants/cohort.vcf.gz"),
    );
    variant_track.sample_key = Some("child".to_owned());
    variants.tracks.push(variant_track);
    variants.tracks.push(TrackConfig::new(
        TrackId::new(2),
        "Equivalent cohort BCF".to_owned(),
        TrackKind::Variant,
        local_source("../data/variants/cohort.bcf"),
    ));
    variants.sample_metadata = Some(rgv_core::SampleMetadataTable::from_tsv(&metadata)?);
    write_session(&projects.join(PROJECTS[8].filename), &variants)?;

    let mixed = mixed_tracks_document(suite_root)?;
    write_session(&projects.join(PROJECTS[9].filename), &mixed)?;

    let mut genome = base_document("chrVariants", 8_500, 12_500, "RGV portable genome review");
    genome.reference = Some(local_source("../../variants.fa"));
    genome.cytobands = Some(local_source("../data/tracks/cytobands.txt"));
    genome.reference_translation_frames = ReferenceTranslationFrame::ALL.to_vec();
    genome.reference_reverse_complement = true;
    genome
        .contig_aliases
        .insert("variants".to_owned(), "chrVariants".to_owned());
    genome.regions_of_interest = vec![
        RegionOfInterest {
            id: 1,
            name: "large indels".to_owned(),
            locus: locus("chrVariants", 9_400, 12_600),
            color: [42, 112, 190, 180],
        },
        RegionOfInterest {
            id: 2,
            name: "Ultima evidence".to_owned(),
            locus: locus("chrVariants", 13_990, 15_120),
            color: [130, 70, 170, 180],
        },
    ];
    genome.review_items = vec![
        review_item(
            1,
            "Inspect translation and aliases",
            "chrVariants",
            8_500,
            9_200,
        ),
        review_item(2, "Inspect large indels", "chrVariants", 9_400, 12_600),
    ];
    genome.tracks.push(TrackConfig::new(
        TrackId::new(1),
        "Default transcript annotation".to_owned(),
        TrackKind::GeneModel,
        local_source("../../demo-tracks/transcripts.genePred"),
    ));
    write_session(&projects.join(PROJECTS[10].filename), &genome)?;

    let mut workspace = base_document("chr1", 900_000, 920_000, "RGV multi-track workspace truth");
    workspace.panels = vec![
        SessionViewPanel::new(PanelId::new(1), locus("chr1", 900_000, 920_000)),
        SessionViewPanel::new(PanelId::new(2), locus("chr2", 0, 200_000)),
    ];
    workspace.active_panel = PanelId::new(2);
    workspace.locus = locus("chr2", 0, 200_000);
    for (offset, (name, color)) in [
        ("Tumour paired", AlignmentColorMode::PairOrientation),
        ("Normal strand", AlignmentColorMode::Strand),
        ("Long reads", AlignmentColorMode::MappingQuality),
        ("Read groups", AlignmentColorMode::ReadGroup),
    ]
    .into_iter()
    .enumerate()
    {
        let mut track = alignment_track(u64::try_from(offset)? + 1, name, "../../mixed.bam", color);
        track.sample_key = Some(if offset % 2 == 0 { "tumour" } else { "normal" }.to_owned());
        workspace.alignment_tracks.push(track);
    }
    workspace.sample_metadata = Some(rgv_core::SampleMetadataTable::from_tsv(&metadata)?);
    write_session(&projects.join(PROJECTS[11].filename), &workspace)?;

    let four_track = workspace.clone();
    write_session(&projects.join("mf-12-four-track.rgv.json"), &four_track)?;
    let mut eight_track = workspace.clone();
    for (offset, (name, color)) in [
        ("Replicate A", AlignmentColorMode::TemplateLength),
        ("Replicate B", AlignmentColorMode::Haplotype),
        (
            "Replicate C",
            AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        ),
        ("Replicate D", AlignmentColorMode::Tag(*b"RG")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut track = alignment_track(u64::try_from(offset)? + 5, name, "../../mixed.bam", color);
        track.sample_key = Some(if offset % 2 == 0 { "tumour" } else { "normal" }.to_owned());
        eight_track.alignment_tracks.push(track);
    }
    write_session(&projects.join("mf-12-eight-track.rgv.json"), &eight_track)?;
    let mut failure_isolation = workspace.clone();
    failure_isolation.alignment_tracks.truncate(3);
    failure_isolation.alignment_tracks.push(alignment_track(
        4,
        "EXPECTED MISSING SOURCE (healthy tracks must remain)",
        "../failures/does-not-exist.bam",
        AlignmentColorMode::MappingQuality,
    ));
    write_session(
        &projects.join("mf-12-failure-isolation.rgv.json"),
        &failure_isolation,
    )?;
    write_session(&projects.join("mf-13-recovery.rgv.json"), &workspace)?;
    let mut autosave = workspace.clone();
    for track in &mut autosave.alignment_tracks {
        track.source = local_source("../../../mixed.bam");
    }
    write_session(
        &suite_root.join("recovery/autosaves/manual-00000000000000000001.autosave.json"),
        &autosave,
    )?;
    write_json(
        &suite_root.join("recovery/runtime-state.json"),
        &serde_json::json!({
            "schema_version": 1,
            "clean_shutdown": false,
            "autosave_enabled": true,
            "autosave_candidates": ["autosaves/manual-00000000000000000001.autosave.json"],
            "expected_choice": "recover-or-continue-without-recovery"
        }),
    )?;

    let mut cram = base_document(
        MANUAL_CONTIG,
        900,
        5_400,
        "RGV reference-dependent CRAM truth",
    );
    cram.reference = Some(local_source("../data/references/manual.fa"));
    cram.alignment_tracks.push(alignment_track(
        1,
        "Reference-dependent CRAM evidence",
        "../data/alignments/reference-dependent.cram",
        AlignmentColorMode::Strand,
    ));
    write_session(&projects.join(PROJECTS[12].filename), &cram)?;
    Ok(())
}

fn review_item(id: u64, label: &str, contig: &str, start: u64, end: u64) -> ReviewItem {
    ReviewItem {
        id,
        label: label.to_owned(),
        locus: locus(contig, start, end),
        status: ReviewStatus::Pending,
        notes: format!("Expected manual fixture truth for {label}."),
        assignment: None,
    }
}

fn write_session(path: &Path, document: &SessionDocument) -> Result<()> {
    write_text(path, &document.to_json()?)
}

fn write_relocation_bundle(suite_root: &Path) -> Result<()> {
    let bundle = suite_root.join("relocation/source");
    for (source, target) in [
        ("data/alignments/specialist.bam", "data/specialist.bam"),
        (
            "data/alignments/specialist.bam.bai",
            "data/specialist.bam.bai",
        ),
        ("data/references/manual.fa", "data/manual.fa"),
        ("data/references/manual.fa.fai", "data/manual.fa.fai"),
    ] {
        fs::copy(suite_root.join(source), bundle.join(target))?;
    }
    let mut document = base_document(
        MANUAL_CONTIG,
        900,
        5_400,
        "RGV self-contained relocation truth",
    );
    document.reference = Some(local_source("data/manual.fa"));
    document.alignment_tracks.push(alignment_track(
        1,
        "Relocatable specialist alignments",
        "data/specialist.bam",
        AlignmentColorMode::Strand,
    ));
    write_session(&bundle.join("relocatable.rgv.json"), &document)
}

fn mixed_tracks_document(suite_root: &Path) -> Result<SessionDocument> {
    let text = fs::read_to_string(suite_root.join("../demo-tracks/rgv-demo-project.json"))?;
    let mut document = SessionDocument::from_json(&text)?;
    document.genome_name = Some("RGV complete mixed-format manual fixture".to_owned());
    for alignment in &mut document.alignment_tracks {
        alignment.source = local_source("../../variants.bam");
    }
    document.reference = Some(local_source("../../variants.fa"));
    for track in &mut document.tracks {
        let TrackSource::Local { path } = &mut track.source else {
            continue;
        };
        let filename = Path::new(path)
            .file_name()
            .context("demo track has no filename")?
            .to_string_lossy();
        *path = format!("../../demo-tracks/{filename}");
    }
    let signal_ids = document
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Quantitative)
        .map(|track| track.id)
        .collect::<Vec<_>>();
    if let Some(first) = signal_ids
        .first()
        .and_then(|id| document.tracks.iter_mut().find(|track| track.id == *id))
    {
        first.quantitative.scale_mode = QuantitativeScaleMode::Shared;
        first.quantitative.overlay_group = Some(1);
        first.quantitative.scale_group = Some(1);
    }
    if let Some(second) = signal_ids
        .get(1)
        .and_then(|id| document.tracks.iter_mut().find(|track| track.id == *id))
    {
        second.quantitative.scale_mode = QuantitativeScaleMode::Shared;
        second.quantitative.overlay_group = Some(1);
        second.quantitative.scale_group = Some(1);
    }
    if let (Some(derived_id), Some(operand_id)) = (signal_ids.get(2), signal_ids.get(1))
        && let Some(derived) = document
            .tracks
            .iter_mut()
            .find(|track| track.id == *derived_id)
    {
        "Arithmetic signal difference".clone_into(&mut derived.name);
        derived.quantitative.arithmetic = QuantitativeArithmetic::Subtract;
        derived.quantitative.operand_track = Some(*operand_id);
    }
    let mut next_id = document
        .tracks
        .iter()
        .map(|track| track.id.get())
        .max()
        .unwrap_or(1)
        + 1;
    for (name, path, kind) in [
        (
            "Plain BED annotations",
            "../data/tracks/default-annotations.bed",
            TrackKind::Feature,
        ),
        (
            "Plain GFF3 gene models",
            "../data/tracks/annotations.gff3",
            TrackKind::GeneModel,
        ),
        (
            "Compressed GFF3 gene models",
            "../data/tracks/annotations.gff3.gz",
            TrackKind::GeneModel,
        ),
        (
            "Plain GTF transcripts",
            "../data/tracks/annotations.gtf",
            TrackKind::GeneModel,
        ),
        (
            "refFlat transcripts",
            "../data/tracks/transcripts.refFlat",
            TrackKind::GeneModel,
        ),
        (
            "Fixed-step WIG",
            "../data/tracks/signal-fixed.wig",
            TrackKind::Quantitative,
        ),
        (
            "CBS copy number",
            "../data/tracks/cohort.cbs",
            TrackKind::CopyNumber,
        ),
        (
            "PLINK linear association",
            "../data/tracks/association.linear",
            TrackKind::Gwas,
        ),
        (
            "PLINK logistic association",
            "../data/tracks/association.logistic",
            TrackKind::Gwas,
        ),
        (
            "PLINK association",
            "../data/tracks/association.assoc",
            TrackKind::Gwas,
        ),
        (
            "PLINK quantitative association",
            "../data/tracks/association.qassoc",
            TrackKind::Gwas,
        ),
    ] {
        document.tracks.push(TrackConfig::new(
            TrackId::new(next_id),
            name.to_owned(),
            kind,
            local_source(path),
        ));
        next_id += 1;
    }
    Ok(document)
}

fn write_support_files(suite_root: &Path) -> Result<()> {
    write_text(
        &suite_root.join("metadata/samples.tsv"),
        "sample\tcondition\tbatch\tfamily_id\tfather\tmother\taffected\n\
         tumour\tcase\tB\tFAM1\tnormal-father\tnormal-mother\tyes\n\
         normal\tcontrol\tA\tFAM1\t0\t0\tno\n\
         normal-father\tcontrol\tA\tFAM1\t0\t0\tno\n\
         normal-mother\tcontrol\tA\tFAM1\t0\t0\tno\n\
         child\tcase\tC\tPED1\tfather\tmother\tyes\n\
         father\tcontrol\tC\tPED1\t0\t0\tno\n\
         mother\tcontrol\tC\tPED1\t0\t0\tno\n\
         single-cell\tcase\tSC\t.\t0\t0\tunknown\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/cytobands.txt"),
        "chrVariants\t0\t5000\tp11\tgneg\n\
         chrVariants\t5000\t10000\tp12\tgpos50\n\
         chrVariants\t10000\t11000\tcen\tacen\n\
         chrVariants\t11000\t20000\tq11\tgpos75\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/default-annotations.bed"),
        "chrVariants\t8500\t9200\tEXPECTED_SEARCH_FEATURE\t900\t+\n\
         chrVariants\t9400\t12600\tEXPECTED_LARGE_INDEL_REGION\t800\t-\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/annotations.gff3"),
        "##gff-version 3\n\
         chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tID=EXPECTED_GENE;Name=Expected gene\n\
         chrVariants\tRGV\tmRNA\t14501\t17500\t.\t+\t.\tID=EXPECTED_TX;Parent=EXPECTED_GENE\n\
         chrVariants\tRGV\texon\t14501\t15000\t.\t+\t.\tParent=EXPECTED_TX\n\
         chrVariants\tRGV\texon\t17001\t17500\t.\t+\t.\tParent=EXPECTED_TX\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/rna-transcripts.gff3"),
        "##gff-version 3\n\
         chrManual\tRGV\tgene\t19951\t20750\t.\t+\t.\tID=RNA_GENE;Name=RNA fixture gene\n\
         chrManual\tRGV\tmRNA\t19951\t20750\t.\t+\t.\tID=RNA_SHARED_TX;Parent=RNA_GENE\n\
         chrManual\tRGV\texon\t19951\t20040\t.\t+\t.\tParent=RNA_SHARED_TX\n\
         chrManual\tRGV\texon\t20141\t20250\t.\t+\t.\tParent=RNA_SHARED_TX\n\
         chrManual\tRGV\tmRNA\t20201\t20500\t.\t+\t.\tID=RNA_A_SPECIFIC_TX;Parent=RNA_GENE\n\
         chrManual\tRGV\texon\t20201\t20250\t.\t+\t.\tParent=RNA_A_SPECIFIC_TX\n\
         chrManual\tRGV\texon\t20371\t20500\t.\t+\t.\tParent=RNA_A_SPECIFIC_TX\n",
    )?;
    write_gzip(
        &suite_root.join("data/tracks/annotations.gff3.gz"),
        "##gff-version 3\n\
         chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tID=EXPECTED_GENE_GZ;Name=Expected compressed gene\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/annotations.gtf"),
        "chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; gene_name \"Expected GTF gene\";\n\
         chrVariants\tRGV\ttranscript\t14501\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\";\n\
         chrVariants\tRGV\texon\t14501\t15000\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\"; exon_number \"1\";\n\
         chrVariants\tRGV\texon\t17001\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\"; exon_number \"2\";\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/transcripts.refFlat"),
        "EXPECTED_REF_FLAT\tEXPECTED_REF_FLAT_TX\tchrVariants\t+\t14500\t17500\t14600\t17400\t2\t14500,17000,\t15000,17500,\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/signal-fixed.wig"),
        "fixedStep chrom=chrVariants start=14501 step=100 span=100\n1.0\n2.0\n4.0\n8.0\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/cohort.cbs"),
        "ID\tchrom\tloc.start\tloc.end\tnum.mark\tseg.mean\n\
         child\tchrVariants\t14501\t16000\t80\t0.75\n\
         father\tchrVariants\t16001\t17500\t75\t-0.50\n",
    )?;
    for extension in ["linear", "logistic", "assoc", "qassoc"] {
        write_text(
            &suite_root.join(format!("data/tracks/association.{extension}")),
            "CHR\tBP\tSNP\tP\tBETA\n\
             chrVariants\t15050\tEXPECTED_ASSOC_1\t1e-12\t0.8\n\
             chrVariants\t16600\tEXPECTED_ASSOC_2\t0.02\t-0.2\n",
        )?;
    }
    write_text(
        &suite_root.join("imports/regions.bed"),
        "track name=\"RGV manual regions\" itemRgb=On\n\
         chrVariants\t0\t1\tcontig-start\t0\t.\t0\t1\t45,92,175\n\
         chrVariants\t9400\t12600\tlarge-indels\t0\t.\t9400\t12600\t220,80,60\n\
         chrManual\t49900\t50600\tultima-evidence\t0\t.\t49900\t50600\t126,87,194\n\
         chrVariants\t19999\t20000\tcontig-end\t0\t.\t19999\t20000\t40,150,90\n",
    )?;
    write_text(
        &suite_root.join("imports/review-items.tsv"),
        "#RGV-review-queue-v1\n\
         contig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json\n\
         chrVariants\t0\t1\tpending\t\"Review contig start\"\t\"tester-a\"\t\"Boundary coordinate must remain zero-based internally.\"\n\
         chrVariants\t9400\t12600\tin_progress\t\"Review large indels\"\tnull\t\"Expected 1000 bp insertion and 2000 bp deletion\"\n\
         chrManual\t49900\t50600\tpending\t\"Review Ultima\"\t\"tester-b\"\t\"Expected tp, mc, t0, missing, and malformed evidence\"\n\
         chrVariants\t19999\t20000\tcomplete\t\"Review contig end\"\tnull\t\"Last valid base of chrVariants.\"\n",
    )?;
    write_text(
        &suite_root.join("imports/representative-igv-session.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Session genome=\"manual\" locus=\"chrVariants:9401-12600\" version=\"8\">\n\
           <Resources>\n\
             <Resource path=\"../../variants.bam\" index=\"../../variants.bam.bai\"/>\n\
             <Resource path=\"../../variants.fa\"/>\n\
             <Resource path=\"../../demo-tracks/features.bed.gz\"/>\n\
           </Resources>\n\
           <Panel name=\"DataPanel\"><Track id=\"../../variants.bam\" name=\"Imported alignment\" color=\"0,0,150\" height=\"320\" displayMode=\"EXPANDED\"/></Panel>\n\
           <Panel name=\"IgnoredPanel\"><Track id=\"unsupported.hic\" name=\"Unsupported Hi-C\"/></Panel>\n\
         </Session>\n",
    )?;
    let igv_xml = fs::read_to_string(suite_root.join("imports/representative-igv-session.xml"))?;
    let imported = SessionDocument::import_igv_with_report(&igv_xml)?;
    write_json(
        &suite_root.join("expected/igv-import-report.json"),
        &serde_json::json!({
            "schema_version": 1,
            "locus": imported.document.locus,
            "alignment_track_count": imported.document.alignment_tracks.len(),
            "track_count": imported.document.tracks.len(),
            "imported": imported.report.imported,
            "approximated": imported.report.approximated,
            "ignored": imported.report.ignored,
            "omitted_notice_count": imported.report.omitted_notice_count
        }),
    )?;
    write_text(
        &suite_root.join("failures/changed-source.bed"),
        "chrVariants\t10000\t10100\tCURRENT_CHANGED_RECORD\n",
    )?;
    write_json(
        &suite_root.join("expected/changed-source-state.json"),
        &serde_json::json!({
            "schema_version": 1,
            "path": "../failures/changed-source.bed",
            "saved_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
            "expected_behavior": "report-changed-and-keep-healthy-sources"
        }),
    )?;
    write_text(
        &suite_root.join("failures/invalid-session.json"),
        "{\"schema_version\":99,\"locus\":{\"contig\":\"chr1\",\"start\":0,\"end\":1}}\n",
    )?;
    write_text(
        &suite_root.join("failures/truncated.bed"),
        "chrVariants\t9000\n",
    )?;

    let genome = GenomeConfiguration {
        schema_version: GENOME_CONFIGURATION_SCHEMA_VERSION,
        name: "RGV portable manual genome".to_owned(),
        reference: local_source("../../variants.fa"),
        cytobands: Some(local_source("../data/tracks/cytobands.txt")),
        contig_aliases: BTreeMap::from([("variants".to_owned(), "chrVariants".to_owned())]),
        default_annotations: vec![GenomeAnnotation {
            name: "Manual search annotations".to_owned(),
            kind: TrackKind::Feature,
            source: local_source("../data/tracks/default-annotations.bed"),
        }],
    };
    write_json(&suite_root.join("genomes/manual.rgvgenome"), &genome)
}

fn write_gzip(path: &Path, contents: &str) -> Result<()> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(contents.as_bytes())?;
    fs::write(path, encoder.finish()?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn write_specialist_reference(path: &Path) -> Result<()> {
    let sequence = manual_reference_sequence();
    write_indexed_fasta(path, &sequence)
}

fn write_wrong_reference(path: &Path) -> Result<()> {
    let sequence = vec![b'T'; MANUAL_CONTIG_LENGTH];
    write_indexed_fasta(path, &sequence)
}

fn write_indexed_fasta(path: &Path, sequence: &[u8]) -> Result<()> {
    let mut fasta = format!(">{MANUAL_CONTIG}\n");
    for chunk in sequence.chunks(80) {
        fasta.push_str(std::str::from_utf8(chunk)?);
        fasta.push('\n');
    }
    write_text(path, &fasta)?;
    let offset = MANUAL_CONTIG.len() + 2;
    write_text(
        &PathBuf::from(format!("{}.fai", path.display())),
        &format!("{MANUAL_CONTIG}\t{MANUAL_CONTIG_LENGTH}\t{offset}\t80\t81\n"),
    )
}

#[derive(Clone, Copy)]
enum RnaStrandTag {
    Positive,
    Negative,
    Missing,
    Invalid,
}

fn write_rna_bams(directory: &Path) -> Result<()> {
    write_rna_bam(&directory.join("rna-sample-a.bam"), 'A')?;
    write_rna_bam(&directory.join("rna-sample-b.bam"), 'B')
}

fn write_rna_bam(path: &Path, sample: char) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    let read_group = format!("rna-{sample}");
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", &read_group)
            .push_tag(b"SM", format!("RNA sample {sample}"))
            .push_tag(b"PL", "ILLUMINA"),
    );

    let mut records = Vec::new();
    let shared_count = if sample == 'A' { 5 } else { 4 };
    for index in 0..shared_count {
        let tag = match index {
            0 => RnaStrandTag::Positive,
            1 => RnaStrandTag::Negative,
            2 => RnaStrandTag::Missing,
            _ => RnaStrandTag::Invalid,
        };
        records.push(rna_record(
            &format!("EXPECTED_RNA_{sample}_SHARED_{index}"),
            20_000,
            vec![Cigar::Equal(40), Cigar::RefSkip(100), Cigar::Equal(40)],
            index % 2 == 1,
            tag,
            &read_group,
        )?);
    }
    if sample == 'A' {
        for index in 0..3 {
            records.push(rna_record(
                &format!("EXPECTED_RNA_A_SPECIFIC_{index}"),
                20_200,
                vec![Cigar::Equal(50), Cigar::RefSkip(120), Cigar::Equal(50)],
                false,
                RnaStrandTag::Positive,
                &read_group,
            )?);
        }
    } else {
        for index in 0..2 {
            records.push(rna_record(
                &format!("EXPECTED_RNA_B_SPECIFIC_{index}"),
                20_400,
                vec![Cigar::Equal(35), Cigar::RefSkip(200), Cigar::Equal(35)],
                true,
                RnaStrandTag::Negative,
                &read_group,
            )?);
        }
        records.push(rna_record(
            "EXPECTED_RNA_B_ALTERNATIVE_DONOR",
            20_000,
            vec![Cigar::Equal(50), Cigar::RefSkip(90), Cigar::Equal(40)],
            false,
            RnaStrandTag::Positive,
            &read_group,
        )?);
        records.push(rna_record(
            "EXPECTED_RNA_B_ALTERNATIVE_ACCEPTOR",
            20_000,
            vec![Cigar::Equal(40), Cigar::RefSkip(110), Cigar::Equal(40)],
            false,
            RnaStrandTag::Positive,
            &read_group,
        )?);
    }
    records.push(rna_record(
        &format!("EXPECTED_RNA_{sample}_MULTI_EXON"),
        20_500,
        vec![
            Cigar::Equal(30),
            Cigar::RefSkip(80),
            Cigar::Equal(30),
            Cigar::RefSkip(90),
            Cigar::Equal(30),
        ],
        sample == 'B',
        RnaStrandTag::Missing,
        &read_group,
    )?);
    records.push(rna_record(
        &format!("EXPECTED_RNA_{sample}_RETAINED_INTRON"),
        20_000,
        vec![Cigar::Equal(180)],
        false,
        RnaStrandTag::Positive,
        &read_group,
    )?);
    records.sort_by_key(Record::pos);
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

fn rna_record(
    name: &str,
    position: i64,
    cigar: Vec<Cigar>,
    reverse: bool,
    strand_tag: RnaStrandTag,
    read_group: &str,
) -> Result<Record> {
    let sequence_length = cigar
        .iter()
        .map(|operation| match operation {
            Cigar::Match(length)
            | Cigar::Ins(length)
            | Cigar::SoftClip(length)
            | Cigar::Equal(length)
            | Cigar::Diff(length) => usize::try_from(*length).unwrap_or_default(),
            _ => 0,
        })
        .sum();
    let sequence = repeating_bases(sequence_length, name.as_bytes()[0]);
    let qualities = vec![35; sequence_length];
    let mut record = Record::new();
    record.set(
        name.as_bytes(),
        Some(&CigarString(cigar)),
        &sequence,
        &qualities,
    );
    record.set_tid(0);
    record.set_pos(position);
    record.set_mapq(60);
    if reverse {
        record.set_reverse();
    }
    record.push_aux(b"RG", Aux::String(read_group))?;
    match strand_tag {
        RnaStrandTag::Positive => record.push_aux(b"XS", Aux::Char(b'+'))?,
        RnaStrandTag::Negative => record.push_aux(b"XS", Aux::Char(b'-'))?,
        RnaStrandTag::Missing => {}
        RnaStrandTag::Invalid => record.push_aux(b"XS", Aux::Char(b'?'))?,
    }
    Ok(record)
}

#[derive(Clone, Copy)]
enum SmrtFixtureCase {
    Valid,
    Missing,
    WrongLength,
    WrongType,
    Negative,
}

fn write_smrt_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "smrt")
            .push_tag(b"SM", "smrt")
            .push_tag(b"PL", "PACBIO"),
    );
    let mut records = Vec::new();
    for (offset, (name, reverse, fixture_case)) in [
        ("EXPECTED_SMRT_VALID_FORWARD", false, SmrtFixtureCase::Valid),
        ("EXPECTED_SMRT_VALID_REVERSE", true, SmrtFixtureCase::Valid),
        (
            "EXPECTED_SMRT_MISSING_TAGS",
            false,
            SmrtFixtureCase::Missing,
        ),
        (
            "EXPECTED_SMRT_WRONG_LENGTH_IP",
            false,
            SmrtFixtureCase::WrongLength,
        ),
        (
            "EXPECTED_SMRT_WRONG_TYPE_IP",
            false,
            SmrtFixtureCase::WrongType,
        ),
        (
            "EXPECTED_SMRT_NEGATIVE_IP",
            false,
            SmrtFixtureCase::Negative,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut record = basic_record(
            name,
            30_000 + i64::try_from(offset)? * 10,
            &repeating_bases(100, u8::try_from(offset)?),
            reverse,
        )?;
        record.push_aux(b"RG", Aux::String("smrt"))?;
        push_smrt_fixture_tags(&mut record, fixture_case)?;
        records.push(record);
    }
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

fn push_smrt_fixture_tags(record: &mut Record, fixture_case: SmrtFixtureCase) -> Result<()> {
    if matches!(fixture_case, SmrtFixtureCase::Missing) {
        return Ok(());
    }
    let values = (0..100_u16)
        .map(|index| index.saturating_mul(75))
        .collect::<Vec<_>>();
    for tag in [b"ip", b"pw", b"fi", b"ri", b"fp", b"rp"] {
        if tag == b"ip" {
            match fixture_case {
                SmrtFixtureCase::WrongLength => {
                    record.push_aux(tag, Aux::ArrayU16((&values[..3]).into()))?;
                    continue;
                }
                SmrtFixtureCase::WrongType => {
                    record.push_aux(tag, Aux::String("not-an-array"))?;
                    continue;
                }
                SmrtFixtureCase::Negative => {
                    let mut signed = values
                        .iter()
                        .map(|value| i16::try_from(*value).unwrap_or(i16::MAX))
                        .collect::<Vec<_>>();
                    signed[5] = -1;
                    record.push_aux(tag, Aux::ArrayI16((&signed[..]).into()))?;
                    continue;
                }
                SmrtFixtureCase::Valid | SmrtFixtureCase::Missing => {}
            }
        }
        record.push_aux(tag, Aux::ArrayU16((&values[..]).into()))?;
    }
    Ok(())
}

fn write_sbx_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    let mut records = Vec::new();
    for (offset, (name, reverse, insertion_low_bases, all_tail, missing_qualities)) in [
        ("EXPECTED_SBX_CONCORDANT", false, 0_usize, false, false),
        ("EXPECTED_SBX_PARTIAL", false, 1, false, false),
        ("EXPECTED_SBX_FULL", false, 10, false, false),
        ("EXPECTED_SBX_REVERSE", true, 0, false, false),
        ("EXPECTED_SBX_ALL_TAIL", false, 10, true, false),
        ("EXPECTED_SBX_MISSING_QUALITIES", false, 0, false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let cigar = CigarString(vec![
            Cigar::SoftClip(20),
            Cigar::Equal(50),
            Cigar::Ins(10),
            Cigar::Equal(50),
            Cigar::SoftClip(20),
        ]);
        let sequence = repeating_bases(150, u8::try_from(offset)?);
        let mut qualities = vec![35_u8; 150];
        qualities[..20].fill(20);
        qualities[130..].fill(20);
        qualities[70..70 + insertion_low_bases].fill(5);
        if all_tail {
            qualities.fill(5);
        }
        if missing_qualities {
            qualities.fill(u8::MAX);
        }
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(40_000 + i64::try_from(offset)? * 10);
        record.set_mapq(60);
        if reverse {
            record.set_reverse();
        }
        records.push(record);
    }
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

fn write_ultima_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "ultima")
            .push_tag(b"SM", "ultima")
            .push_tag(b"PL", "ULTIMA")
            .push_tag(b"mc", 1),
    );

    let cases = [
        ("EXPECTED_ULTIMA_FORWARD_Q40", false, Some("valid")),
        ("EXPECTED_ULTIMA_REVERSE_Q0", true, Some("valid")),
        ("EXPECTED_ULTIMA_MISSING_TP", false, None),
        ("EXPECTED_ULTIMA_WRONG_LENGTH_TP", false, Some("short")),
        ("EXPECTED_ULTIMA_WRONG_TYPE_TP", false, Some("string")),
    ];
    let cigar = CigarString(vec![Cigar::Equal(3), Cigar::Ins(2), Cigar::Equal(3)]);
    let forward_sequence = b"ACGAAATC";
    let mut records = Vec::new();
    for (offset, (name, reverse, tp_case)) in cases.into_iter().enumerate() {
        let mut sequence = forward_sequence.to_vec();
        let mut qualities = vec![30_u8; sequence.len()];
        qualities[4] = if offset == 1 { 0 } else { 40 };
        let mut tp = vec![0_i32; sequence.len()];
        tp[4] = -2;
        if reverse {
            sequence = reverse_complement(&sequence);
            qualities.reverse();
            tp.reverse();
        }
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(50_000 + i64::try_from(offset)? * 10);
        record.set_mapq(60);
        record.push_aux(b"RG", Aux::String("ultima"))?;
        if reverse {
            record.set_reverse();
        }
        match tp_case {
            Some("valid") => record.push_aux(b"tp", Aux::ArrayI32((&tp[..]).into()))?,
            Some("short") => record.push_aux(b"tp", Aux::ArrayI32((&tp[..3]).into()))?,
            Some("string") => record.push_aux(b"tp", Aux::String("not-an-array"))?,
            None => {}
            Some(other) => anyhow::bail!("unknown Ultima fixture case {other}"),
        }
        records.push(record);
    }

    let reference = manual_reference_sequence();
    for (name, start, t0_quality) in [
        ("EXPECTED_ULTIMA_DELETION_MC_Q0", 50_109_usize, None),
        ("EXPECTED_ULTIMA_DELETION_T0_Q35", 50_130, Some(35_u8)),
    ] {
        let sequence = [
            &reference[start..start + 3],
            &reference[start + 4..start + 7],
        ]
        .concat();
        let qualities = vec![30_u8; sequence.len()];
        let cigar = CigarString(vec![Cigar::Equal(3), Cigar::Del(1), Cigar::Equal(3)]);
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(i64::try_from(start)?);
        record.set_mapq(60);
        record.push_aux(b"RG", Aux::String("ultima"))?;
        let tp = vec![0_i32; sequence.len()];
        record.push_aux(b"tp", Aux::ArrayI32((&tp[..]).into()))?;
        if let Some(quality) = t0_quality {
            let mut values = vec![20_u8; sequence.len()];
            values[2] = quality;
            values[3] = quality;
            let encoded = values
                .iter()
                .map(|value| char::from(value.saturating_add(33)))
                .collect::<String>();
            record.push_aux(b"t0", Aux::String(&encoded))?;
        }
        records.push(record);
    }
    records.sort_by_key(Record::pos);
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

fn write_specialist_bam(path: &Path) -> Result<()> {
    // MD5 of the deterministic 100,000-base `manual_reference_sequence`. CRAM requires the
    // sequence digest in SQ so it remains genuinely reference-dependent rather than embedding it.
    const REFERENCE_MD5: &str = "1bd65b2b27c9e6c1cb4ad93cbb50516d";
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH)
            .push_tag(b"M5", REFERENCE_MD5)
            .push_tag(b"UR", "manual.fa"),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "bisulfite")
            .push_tag(b"SM", "bisulfite")
            .push_tag(b"PL", "ILLUMINA"),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "single-cell")
            .push_tag(b"SM", "single-cell")
            .push_tag(b"PL", "ILLUMINA"),
    );
    let mut records = Vec::new();
    let reference = manual_reference_sequence();
    for (offset, (name, reverse, converted)) in [
        ("EXPECTED_CG_PROTECTED_FORWARD", false, false),
        ("EXPECTED_CG_CONVERTED_FORWARD", false, true),
        ("EXPECTED_CG_PROTECTED_REVERSE", true, false),
        ("EXPECTED_CG_CONVERTED_REVERSE", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let start = 1_000 + offset * 4;
        let mut sequence = reference[start..start + 160].to_vec();
        if reverse {
            sequence = reverse_complement(&sequence);
        }
        if converted {
            for base in &mut sequence {
                if *base == b'C' {
                    *base = b'T';
                }
            }
        }
        let mut record = basic_record(name, i64::try_from(start)?, &sequence, reverse)?;
        record.push_aux(b"RG", Aux::String("bisulfite"))?;
        records.push(record);
    }
    let mut low_quality = basic_record_with_quality(
        "EXPECTED_BISULFITE_LOW_QUALITY",
        1_020,
        &reference[1_020..1_180],
        false,
        5,
    )?;
    low_quality.push_aux(b"RG", Aux::String("bisulfite"))?;
    records.push(low_quality);
    let mut missing = basic_record(
        "EXPECTED_BISULFITE_MISSING_EVIDENCE",
        1_024,
        &[b'N'; 160],
        false,
    )?;
    missing.push_aux(b"RG", Aux::String("bisulfite"))?;
    records.push(missing);
    for (offset, (cell, raw_cell, umi, raw_umi)) in [
        ("CELL_A-1", "RAW_CELL_A", "UMI0001", "RAWUMI01"),
        ("CELL_A-1", "RAW_CELL_A", "UMI0001", "RAWUMI01"),
        ("CELL_A-1", "RAW_CELL_A", "UMI0002", "RAWUMI02"),
        ("CELL_B-1", "RAW_CELL_B", "UMI0001", "RAWUMI11"),
        ("CELL_B-1", "RAW_CELL_B", "UMI0003", "RAWUMI13"),
        ("CELL_C-1", "RAW_CELL_C", "UMI0009", "RAWUMI29"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("EXPECTED_{cell}_{umi}_{offset}");
        let mut record = basic_record(
            &name,
            5_000 + i64::try_from(offset)? * 5,
            &repeating_bases(100, offset as u8),
            offset % 2 == 1,
        )?;
        record.push_aux(b"RG", Aux::String("single-cell"))?;
        record.push_aux(b"CB", Aux::String(cell))?;
        record.push_aux(b"CR", Aux::String(raw_cell))?;
        record.push_aux(b"UB", Aux::String(umi))?;
        record.push_aux(b"UR", Aux::String(raw_umi))?;
        record.push_aux(b"HP", Aux::I32(i32::try_from(offset % 2 + 1)?))?;
        records.push(record);
    }
    let mut missing = basic_record(
        "EXPECTED_MISSING_CELL_TAGS",
        5_040,
        &repeating_bases(100, 9),
        false,
    )?;
    missing.push_aux(b"RG", Aux::String("single-cell"))?;
    records.push(missing);
    let mut malformed = basic_record(
        "EXPECTED_MALFORMED_CELL_TAG",
        5_045,
        &repeating_bases(100, 10),
        true,
    )?;
    malformed.push_aux(b"RG", Aux::String("single-cell"))?;
    malformed.push_aux(b"CB", Aux::I32(42))?;
    malformed.push_aux(b"CR", Aux::String("RAW_MALFORMED"))?;
    malformed.push_aux(b"UB", Aux::String("UMI_BAD"))?;
    malformed.push_aux(b"UR", Aux::String("RAW_UMI_BAD"))?;
    records.push(malformed);
    records.sort_by_key(Record::pos);

    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    let bai = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&bai), bam::index::Type::Bai, 1)?;
    let csi = PathBuf::from(format!("{}.csi", path.display()));
    bam::index::build(path, Some(&csi), bam::index::Type::Csi(14), 1)?;
    Ok(())
}

fn reverse_complement(sequence: &[u8]) -> Vec<u8> {
    sequence
        .iter()
        .rev()
        .map(|base| match base.to_ascii_uppercase() {
            b'A' => b'T',
            b'C' => b'G',
            b'G' => b'C',
            b'T' | b'U' => b'A',
            other => other,
        })
        .collect()
}

fn manual_reference_sequence() -> Vec<u8> {
    const MOTIF: &[u8] = b"ACGTCAGCTCGATCGGCH";
    (0..MANUAL_CONTIG_LENGTH)
        .map(|index| {
            let base = MOTIF[index % MOTIF.len()];
            if base == b'H' { b'A' } else { base }
        })
        .collect()
}

fn write_specialist_cram(bam_path: &Path, reference_path: &Path, cram_path: &Path) -> Result<()> {
    let mut reader = Reader::from_path(bam_path)?;
    let header = Header::from_template(reader.header());
    let mut writer = Writer::from_path(cram_path, &header, Format::Cram)?;
    writer.set_reference(reference_path)?;
    for result in reader.records() {
        writer.write(&result?)?;
    }
    drop(writer);
    let crai = PathBuf::from(format!("{}.crai", cram_path.display()));
    bam::index::build(cram_path, Some(&crai), bam::index::Type::Bai, 1)?;
    Ok(())
}

fn cohort_variant_header() -> BcfHeader {
    let mut header = BcfHeader::new();
    header.push_record(b"##contig=<ID=chrVariants,length=20000>");
    header.push_record(br#"##FILTER=<ID=q10,Description="Quality below ten">"#);
    header
        .push_record(br#"##INFO=<ID=END,Number=1,Type=Integer,Description="Structural endpoint">"#);
    header.push_record(
        br#"##INFO=<ID=SVTYPE,Number=1,Type=String,Description="Structural variant type">"#,
    );
    header.push_record(br#"##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency">"#);
    header.push_record(
        br#"##INFO=<ID=SOMATIC,Number=0,Type=Flag,Description="Somatic fixture record">"#,
    );
    header.push_record(
        br#"##INFO=<ID=AC,Number=A,Type=Integer,Description="Alternate allele counts">"#,
    );
    header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
    header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">"#);
    header.push_record(br#"##FORMAT=<ID=AD,Number=R,Type=Integer,Description="Allele depths">"#);
    header.push_record(br#"##FORMAT=<ID=GQ,Number=1,Type=Integer,Description="Genotype quality">"#);
    header.push_record(
        br#"##FORMAT=<ID=PL,Number=G,Type=Integer,Description="Phred genotype likelihoods">"#,
    );
    for sample in ["child", "father", "mother"] {
        header.push_sample(sample.as_bytes());
    }
    for index in 0..125 {
        header.push_sample(format!("cohort-{index:03}").as_bytes());
    }
    header
}

fn write_cohort_variants(vcf_path: &Path, bcf_path: &Path) -> Result<()> {
    write_cohort_variant_format(vcf_path, BcfFormat::Vcf)?;
    bcf_index::build(vcf_path, None, 1, bcf_index::Type::Tbx)?;
    write_cohort_variant_format(bcf_path, BcfFormat::Bcf)?;
    bcf_index::build(bcf_path, None, 1, bcf_index::Type::Csi(14))?;
    Ok(())
}

fn write_cohort_variant_format(path: &Path, format: BcfFormat) -> Result<()> {
    let header = cohort_variant_header();
    let mut writer = BcfWriter::from_path(path, &header, false, format)?;
    let definitions: &[(&str, i64, &[&[u8]], f32, Option<i32>, Option<&str>)] = &[
        ("EXPECTED_SNV", 15_049, &[b"A", b"T"], 60.0, None, None),
        (
            "EXPECTED_MULTI_ALLELIC",
            15_149,
            &[b"A", b"C", b"G"],
            55.0,
            None,
            None,
        ),
        ("EXPECTED_MNV", 15_199, &[b"AC", b"GT"], 52.0, None, None),
        (
            "EXPECTED_INSERTION",
            15_249,
            &[b"A", b"ATGC"],
            50.0,
            None,
            None,
        ),
        (
            "EXPECTED_DELETION",
            15_349,
            &[b"ATGC", b"A"],
            45.0,
            None,
            None,
        ),
        (
            "EXPECTED_INVERSION",
            15_449,
            &[b"N", b"<INV>"],
            40.0,
            Some(15_900),
            Some("INV"),
        ),
        (
            "EXPECTED_SYMBOLIC_DELETION",
            15_999,
            &[b"N", b"<DEL>"],
            9.0,
            Some(16_500),
            Some("DEL"),
        ),
        (
            "EXPECTED_BREAKEND",
            16_599,
            &[b"N", b"N]chrVariants:18001]"],
            48.0,
            None,
            Some("BND"),
        ),
    ];
    for (record_index, (id, position, alleles, quality, end, sv_type)) in
        definitions.iter().enumerate()
    {
        let mut record = writer.empty_record();
        record.set_rid(Some(0));
        record.set_pos(*position);
        record.set_id(id.as_bytes())?;
        record.set_alleles(alleles)?;
        record.set_qual(*quality);
        if *quality < 10.0 {
            record.set_filters(&[b"q10".as_slice()])?;
        }
        if let Some(end) = end {
            record.push_info_integer(b"END", &[*end])?;
        }
        if let Some(sv_type) = sv_type {
            record.push_info_string(b"SVTYPE", &[sv_type.as_bytes()])?;
        }
        let alternate_count = alleles.len().saturating_sub(1);
        let frequencies = (0..alternate_count)
            .map(|offset| 0.05_f32 + (offset as f32 * 0.10))
            .collect::<Vec<_>>();
        record.push_info_float(b"AF", &frequencies)?;
        let counts = (0..alternate_count)
            .map(|offset| i32::try_from(2 + offset + record_index).unwrap_or(i32::MAX))
            .collect::<Vec<_>>();
        record.push_info_integer(b"AC", &counts)?;
        if record_index % 2 == 0 {
            record.push_info_flag(b"SOMATIC")?;
        }
        let mut genotypes = Vec::with_capacity(128);
        let mut depths = Vec::with_capacity(128);
        let mut allele_depths = Vec::new();
        let mut genotype_qualities = Vec::with_capacity(128);
        let mut likelihoods = Vec::with_capacity(128 * 3);
        for sample_index in 0..128_usize {
            let alternate =
                i32::try_from(1 + (sample_index + record_index) % alternate_count.max(1))?;
            if record_index == definitions.len() - 1 && sample_index == 127 {
                genotypes.push(vec![
                    GenotypeAllele::Unphased(0),
                    GenotypeAllele::Unphased(i32::try_from(alternate_count + 7)?),
                ]);
            } else {
                match sample_index % 7 {
                    0 => genotypes.push(vec![
                        GenotypeAllele::Phased(0),
                        GenotypeAllele::Phased(alternate),
                    ]),
                    1 => genotypes.push(vec![
                        GenotypeAllele::Unphased(alternate),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                    2 => genotypes.push(vec![
                        GenotypeAllele::UnphasedMissing,
                        GenotypeAllele::UnphasedMissing,
                    ]),
                    3 => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::UnphasedMissing,
                    ]),
                    4 => genotypes.push(vec![GenotypeAllele::Unphased(alternate)]),
                    5 => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::Unphased(alternate),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                    _ => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                }
            }
            let depth = i32::try_from(20 + sample_index % 41)?;
            depths.push(depth);
            genotype_qualities.push(i32::try_from(10 + sample_index % 90)?);
            for allele_index in 0..=alternate_count {
                allele_depths.push(if allele_index == 0 {
                    depth / 2
                } else {
                    depth / i32::try_from(alternate_count + 1)?
                });
            }
            let ploidy = genotypes.last().map_or(2, Vec::len);
            let allele_count = alternate_count + 1;
            let likelihood_count = genotype_likelihood_count(allele_count, ploidy);
            let maximum_likelihood_count = genotype_likelihood_count(allele_count, 3);
            likelihoods.extend((0..likelihood_count).map(|index| {
                i32::try_from(index.saturating_mul(10).saturating_add(sample_index % 7))
                    .unwrap_or(i32::MAX)
            }));
            likelihoods.extend(std::iter::repeat_n(
                i32::MIN + 1,
                maximum_likelihood_count.saturating_sub(likelihood_count),
            ));
        }
        record.push_genotype_structured(&genotypes, 3)?;
        record.push_format_integer(b"DP", &depths)?;
        record.push_format_integer(b"AD", &allele_depths)?;
        record.push_format_integer(b"GQ", &genotype_qualities)?;
        record.push_format_integer(b"PL", &likelihoods)?;
        writer.write(&record)?;
    }
    Ok(())
}

fn genotype_likelihood_count(allele_count: usize, ploidy: usize) -> usize {
    match ploidy {
        0 => 0,
        1 => allele_count,
        2 => allele_count.saturating_mul(allele_count + 1) / 2,
        3 => {
            allele_count
                .saturating_mul(allele_count + 1)
                .saturating_mul(allele_count + 2)
                / 6
        }
        _ => 0,
    }
}

fn basic_record(name: &str, position: i64, sequence: &[u8], reverse: bool) -> Result<Record> {
    basic_record_with_quality(name, position, sequence, reverse, 35)
}

fn basic_record_with_quality(
    name: &str,
    position: i64,
    sequence: &[u8],
    reverse: bool,
    quality: u8,
) -> Result<Record> {
    let mut record = Record::new();
    let cigar = CigarString(vec![Cigar::Equal(u32::try_from(sequence.len())?)]);
    let qualities = vec![quality; sequence.len()];
    record.set(name.as_bytes(), Some(&cigar), sequence, &qualities);
    record.set_tid(0);
    record.set_pos(position);
    record.set_mapq(60);
    if reverse {
        record.set_reverse();
    }
    Ok(record)
}

fn repeating_bases(length: usize, seed: u8) -> Vec<u8> {
    const BASES: &[u8] = b"ACGTCAGCTCGATCGGCA";
    (0..length)
        .map(|offset| BASES[(offset + usize::from(seed)) % BASES.len()])
        .collect()
}

fn write_truth_documents(suite_root: &Path) -> Result<()> {
    let truths = [
        truth(
            "truth-mf-01",
            &["dep-mixed-bam"],
            "mixed-synthetic",
            vec![truth_locus(
                "structural-pairs",
                "chr1:2590001-2650000",
                "chr1",
                2_590_000,
                2_650_000,
                &[
                    "Long, short, same-strand, outward-facing, and inter-contig templates are present.",
                    "Deep coverage and long/supplementary loci are reachable through the review queue.",
                ],
            )],
        ),
        truth(
            "truth-mf-02",
            &["dep-variants-bam", "dep-variants-fasta"],
            "variants-synthetic",
            vec![truth_locus(
                "large-indels",
                "chrVariants:9401-12600",
                "chrVariants",
                9_400,
                12_600,
                &[
                    "A 1,000-base insertion and a 2,000-base deletion start from the shared 9,500 zero-based alignment position.",
                    "The base-level locus contains controls, SNVs, 1/6-base insertions, and 1/12-base deletions.",
                ],
            )],
        ),
        truth(
            "truth-mf-03",
            &[
                "manual-rna-a-bam",
                "manual-rna-b-bam",
                "manual-rna-annotation",
            ],
            "manual-synthetic",
            vec![truth_locus(
                "rna-junctions",
                "chrManual:19901-21100",
                MANUAL_CONTIG,
                19_900,
                21_100,
                &[
                    "Sample A contains five observations of the shared 20040-20140 junction and three of the annotated A-specific 20250-20370 junction.",
                    "Sample B contains four observations of the shared junction, two of the unannotated B-specific 20435-20635 junction, and alternative donor and acceptor events.",
                    "Both samples contain a two-junction multi-exon read and an unspliced retained-intron record.",
                    "Tagged positive, tagged negative, inferred missing, and invalid XS strand evidence are all present.",
                ],
            )],
        ),
        truth(
            "truth-mf-04",
            &["manual-specialist-bam", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "bisulfite",
                "chrManual:901-1200",
                MANUAL_CONTIG,
                900,
                1_200,
                &[
                    "Six named records cover forward/reverse converted/protected evidence, a low-quality call, and missing evidence.",
                    "Every CG, CHG, CHH, HCG, GCH, and WCG display mode has both protected and converted observations against the matching reference.",
                ],
            )],
        ),
        truth(
            "truth-mf-05",
            &["manual-smrt-bam"],
            "manual-synthetic",
            vec![truth_locus(
                "smrt",
                "chrManual:29901-30600",
                MANUAL_CONTIG,
                29_900,
                30_600,
                &[
                    "Forward and reverse records expose exact 100-value ip, pw, fi, ri, fp, and rp arrays in all six preconfigured modes.",
                    "Named controls expose missing tags, a three-value ip array, a string ip value, and a negative signed ip value without breaking valid reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-06",
            &["manual-sbx-bam"],
            "manual-synthetic",
            vec![truth_locus(
                "sbx",
                "chrManual:39901-40600",
                MANUAL_CONTIG,
                39_900,
                40_600,
                &[
                    "Named records expose concordant, partly discordant, and fully discordant ten-base insertions plus a reverse-strand control.",
                    "A named all-tail record has no retained center and a named missing-quality record reports unavailable evidence without hiding healthy reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-07",
            &["manual-ultima-bam", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "ultima",
                "chrManual:49901-50600",
                MANUAL_CONTIG,
                49_900,
                50_600,
                &[
                    "Forward Q40 and reverse Q0 insertion records expose exact signed tp direction arrays in directional and minimum modes.",
                    "A read-group mc=1 homopolymer deletion resolves to Q0 and a single-base non-homopolymer deletion resolves from t0 to Q35.",
                    "Named missing, wrong-length, and wrong-type tp controls remain local errors without hiding the valid reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-08",
            &["manual-specialist-bam", "manual-sample-metadata"],
            "manual-synthetic",
            vec![truth_locus(
                "single-cell",
                "chrManual:4901-5400",
                MANUAL_CONTIG,
                4_900,
                5_400,
                &[
                    "CELL_A-1, CELL_B-1, and CELL_C-1 records have corrected/raw cell and UMI tags.",
                    "One named record deliberately lacks cell and UMI tags.",
                    "One named record deliberately carries a numeric malformed CB tag.",
                ],
            )],
        ),
        truth(
            "truth-mf-09",
            &["manual-cohort-vcf", "manual-cohort-bcf"],
            "variants-synthetic",
            vec![truth_locus(
                "cohort-variants",
                "chrVariants:14901-18100",
                "chrVariants",
                14_900,
                18_100,
                &[
                    "The VCF and BCF contain SNV, MNV, multi-allelic, insertion, deletion, inversion, symbolic deletion, and breakend records.",
                    "All 128 samples retain phased, unphased, haploid, polyploid, missing, and partial no-call GT states plus DP, AD, GQ, and PL fields.",
                    "The final sample on EXPECTED_BREAKEND has one out-of-range allele index as a readable malformed-genotype boundary case.",
                ],
            )],
        ),
        truth(
            "truth-mf-10",
            &["dep-demo-project"],
            "variants-synthetic",
            vec![truth_locus(
                "mixed-tracks",
                "chrVariants:8501-16000",
                "chrVariants",
                8_500,
                16_000,
                &[
                    "All 19 declared auxiliary sources return non-empty production-engine evidence.",
                    "The TDF exposes two independent named channels.",
                ],
            )],
        ),
        truth(
            "truth-mf-11",
            &["manual-genome-config", "dep-variants-fasta"],
            "variants-synthetic",
            vec![truth_locus(
                "genome-review",
                "chrVariants:8501-12500",
                "chrVariants",
                8_500,
                12_500,
                &[
                    "Alias variants resolves to chrVariants.",
                    "All six translation frames, two ROIs, and two review items restore.",
                    "The chrVariants:8501-8560 reference is exact ACGT repetition, yielding stable forward/reverse codons including ACG, CGT, GTA, and TAC.",
                    "EXPECTED_SEARCH_FEATURE resolves exactly to chrVariants:8501-9200, and the import files contain valid coloured boundary ROIs and versioned review rows.",
                ],
            )],
        ),
        truth(
            "truth-mf-12",
            &["dep-mixed-bam"],
            "mixed-synthetic",
            vec![truth_locus(
                "workspace-deep",
                "chr1:900001-920000",
                "chr1",
                900_000,
                920_000,
                &[
                    "Four stable alignment tracks reopen in two ordered panels; separate eight-track and one-failure workspaces retain distinct identities.",
                    "The self-contained reference/BAM project reopens and queries after relocation to a different directory.",
                ],
            )],
        ),
        truth(
            "truth-mf-13",
            &[
                "manual-igv-import",
                "manual-igv-expected",
                "manual-recovery-autosave",
                "manual-recovery-state",
                "failure-changed-source",
                "manual-changed-source-state",
            ],
            "mixed-synthetic",
            vec![truth_locus(
                "session-recovery",
                "chr1:900001-920000",
                "chr1",
                900_000,
                920_000,
                &[
                    "The IGV import report exactly classifies imported, approximated, and ignored items.",
                    "A controlled unclean runtime state exposes exactly one valid autosave and an explicit recover-or-continue choice.",
                    "The changed-source fingerprint differs deliberately, while the partial workspace names exactly one missing BAM.",
                    "Native versions 1, 2, and 3 reopen and the deliberately future schema is rejected.",
                ],
            )],
        ),
        truth(
            "truth-mf-14",
            &["manual-reference-cram", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "reference-dependent-cram",
                "chrManual:901-5400",
                MANUAL_CONTIG,
                900,
                5_400,
                &[
                    "The CRAM returns the same fourteen named records as the source BAM when opened with the matching reference.",
                    "A reference with a different MD5 is rejected rather than displaying fabricated sequence.",
                ],
            )],
        ),
    ];
    for truth in truths {
        write_json(
            &suite_root.join(format!("expected/{}.json", truth.id)),
            &truth,
        )?;
    }
    Ok(())
}

fn truth(id: &str, fixture_ids: &[&str], assembly: &str, loci: Vec<TruthLocus>) -> TruthDocument {
    TruthDocument {
        schema_version: MANUAL_SUITE_SCHEMA_VERSION,
        id: id.to_owned(),
        fixture_ids: fixture_ids
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        assembly: assembly.to_owned(),
        loci,
    }
}

fn truth_locus(
    id: &str,
    displayed_locus: &str,
    contig: &str,
    start: u64,
    end: u64,
    facts: &[&str],
) -> TruthLocus {
    TruthLocus {
        id: id.to_owned(),
        displayed_locus: displayed_locus.to_owned(),
        contig: contig.to_owned(),
        start_zero_based: start,
        end_zero_based_exclusive: end,
        expected_facts: facts.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn write_scenarios(root: &Path, suite_root: &Path, validation_root: &Path) -> Result<()> {
    let relative_suite = suite_root.strip_prefix(root)?.to_string_lossy();
    for scenario in SCENARIOS {
        let launch = scenario.project_id.map_or_else(
            || operational_launch(scenario.id).to_owned(),
            |project_id| {
                let project = PROJECTS
                    .iter()
                    .find(|project| project.id == project_id)
                    .expect("scenario project IDs are static and valid");
                format!(
                    "cargo run --release -p rgv-desktop -- --session {relative_suite}/projects/{}",
                    project.filename
                )
            },
        );
        let ordinal = scenario.id.trim_start_matches("scenario-mf-");
        let project_line = scenario.project_id.unwrap_or("runtime-generated");
        let mut text = format!(
            "# {}: {}\n\n- **Status:** Generated and machine-verified; manual result not run\n- **Scenario ID:** `{}`\n- **Feature group:** `MF-{}`\n- **Project:** `{}`\n- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-{}.json`\n\n## Why this exists\n\n{}\n\n## Launch\n\n```bash\n{}\n```\n\n## Expected truth\n\n",
            scenario.id,
            scenario.title,
            scenario.id,
            ordinal,
            project_line,
            ordinal,
            scenario.title,
            launch
        );
        for expected in scenario.expected {
            let _ = writeln!(text, "- {expected}");
        }
        text.push_str("\n## Manual actions\n\n");
        for (index, action) in scenario_actions(scenario.id).iter().enumerate() {
            let _ = writeln!(text, "{}. {action}", index + 1);
        }
        text.push_str("\n## Common failure signatures\n\n");
        for failure in scenario_failures(scenario.id) {
            let _ = writeln!(text, "- {failure}");
        }
        text.push_str("\n## Result\n\n```text\ncommit:\nplatform/GPU/display scaling:\nresult: not run\nnotes:\nscreenshot or issue:\n```\n");
        write_text(
            &validation_root.join(format!("scenarios/{}.md", scenario.id)),
            &text,
        )?;
    }
    Ok(())
}

fn write_automation_batches(validation_root: &Path) -> Result<()> {
    write_json(
        &validation_root.join("batches/mf15-complete-exports.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/mixed.bam" } }, "kind": "Alignment", "name": "Automation alignments" },
                { "command": "goto", "loci": ["chr1:100001-120000"] },
                { "command": "add_panel", "locus": "chr1:110001-130000" },
                { "command": "set_option", "scope": { "scope": "workspace" }, "key": "show_junctions", "value": true },
                { "command": "group", "track_ids": [1], "mode": "strand" },
                { "command": "sort", "track_ids": [1], "mode": "mapping_quality", "direction": "natural" },
                { "command": "filter", "track_ids": [1], "expression": "NM>=0" },
                { "command": "wait_until_settled", "timeout_millis": 30000 },
                { "command": "select", "selection": { "selection": "position", "panel_id": 1, "track_id": 1, "position": 115_000 } },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/alignments.tsv", "kind": "alignments" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/pileup.tsv", "kind": "pileup" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/selected-evidence.tsv", "kind": "selected_evidence" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/modifications.tsv", "kind": "modifications" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/junctions.tsv", "kind": "junctions" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/track-names.tsv", "kind": "track_names" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/workspace.png", "scope": "visible_workspace", "format": "png" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/full-panel.svg", "scope": "full_active_panel", "format": "svg" },
                { "command": "save", "path": "../../../benchmark-results/manual-suite/automation-project.rgv.json" },
                { "command": "exit" }
            ]
        }),
    )?;
    write_json(
        &validation_root.join("batches/mf15-variant-track-exports.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "set_genome", "source": { "Local": { "path": "../../../benchmark-data/variants.fa" } }, "name": "Manual variants genome" },
                { "command": "goto", "loci": ["chrVariants:14901-18100"] },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/data/variants/cohort.bcf" } }, "kind": "Variant", "name": "Automation variants" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/demo-tracks/signal.bedGraph" } }, "kind": "Quantitative", "name": "Automation signal" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/data/tracks/default-annotations.bed" } }, "kind": "Feature", "name": "Automation regions" },
                { "command": "wait_until_settled", "timeout_millis": 30000 },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/variants.tsv", "kind": "variants" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/quantitative.tsv", "kind": "quantitative" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/regions.tsv", "kind": "regions" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/variant-track-names.tsv", "kind": "track_names" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/variant-workspace.svg", "scope": "visible_workspace", "format": "svg" },
                { "command": "save", "path": "../../../benchmark-results/manual-suite/variant-project.rgv.json" },
                { "command": "exit" }
            ]
        }),
    )?;
    write_json(
        &validation_root.join("batches/mf15-controlled-failure.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/failures/does-not-exist.bam" } }, "kind": "Alignment", "name": "EXPECTED missing source" },
                { "command": "goto", "loci": ["chr1:900001-920000"] },
                { "command": "wait_until_settled", "timeout_millis": 100 },
                { "command": "exit" }
            ]
        }),
    )
}

fn scenario_actions(id: &str) -> &'static [&'static str] {
    match id {
        "scenario-mf-01" => &[
            "Confirm paired, structural, deep, long-read, supplementary, modified-base, and pathological-CIGAR review loci are present.",
            "At each locus pan and zoom with the top navigator, then vertically scroll without moving reference or coverage tracks.",
            "Switch grouping, sorting, pairing, color, density, and base-display settings and inspect named reads.",
            "Use mate and supplementary-chain navigation, then return through locus history and the review queue.",
            "Save, reopen, and export the selected read/evidence; confirm track identity and settings persist.",
        ],
        "scenario-mf-02" => &[
            "Inspect each named match, SNV, small-indel, 1,000-base insertion, and 2,000-base deletion record.",
            "Zoom across base-letter, event-label, and aggregate thresholds without changing event identity or colour.",
            "Select insertion and deletion markers and compare positions, CIGAR, sequence, qualities, and pileup with truth-mf-02.",
            "Pan slightly while zoomed in and confirm reads do not jump away or restack vertically.",
            "Export the selected evidence and reopen the project to confirm the exact locus and base-display settings.",
        ],
        "scenario-mf-03" => &[
            "Compare RNA sample A and B at the shared and sample-specific junctions.",
            "Confirm annotated A-specific and unannotated B-specific links plus alternative donor/acceptor and multi-exon reads.",
            "Switch strand handling and inspect positive, negative, inferred-missing, and invalid XS evidence.",
            "Compare fixed and per-track automatic Sashimi scaling while panning and zooming both panels.",
            "Filter junction counts, select links, and export vector evidence without changing the exact support counts.",
        ],
        "scenario-mf-04" => &[
            "Inspect all six preconfigured CG, CHG, CHH, HCG, GCH, and WCG tracks.",
            "For every context confirm protected and converted observations against the matching reference.",
            "Compare forward and reverse reads, then inspect the low-quality and missing-evidence controls.",
            "Toggle pairing, grouping, and zoom thresholds without falling back to ordinary mismatch colouring.",
            "Save and reopen; confirm every track retains its distinct bisulfite context.",
        ],
        "scenario-mf-05" => &[
            "Open each of the six subread/CCS IPD and pulse-width modes.",
            "Inspect forward and reverse records and compare exact per-base values with truth-mf-05.",
            "Check missing, wrong-length, wrong-type, and malformed evidence is reported locally rather than crashing the track.",
            "Pan and zoom through thresholds and confirm kinetic colour scales do not reinterpret the same value.",
            "Save/reopen all modes and export one selected kinetic series.",
        ],
        "scenario-mf-06" => &[
            "Inspect concordant, partial, full, reverse, and all-tail SBX records.",
            "Compare Highlight, Hide, and Off simplex-tail tracks at the same reads.",
            "Check left/right tails, insertion components, and quality boundaries against truth-mf-06.",
            "Inspect missing or malformed evidence and confirm neighboring healthy reads remain visible.",
            "Save/reopen and export a selected SBX read in every tail mode.",
        ],
        "scenario-mf-07" => &[
            "Inspect the four valid and three invalid named Ultima records against the matching reference.",
            "Compare directional and minimum-evidence tracks for forward Q40/reverse Q0 tp, read-group mc Q0, and t0 Q35 calls.",
            "Check missing, wrong-length, and wrong-type tp records are reported without hiding valid evidence.",
            "Pan and zoom through base/event thresholds without changing the decoded event.",
            "Save/reopen and export the same selected event from both quality modes.",
        ],
        "scenario-mf-08" => &[
            "Group, sort, colour, and filter by RG, CB, CR, UB, and UR.",
            "Confirm repeated and distinct molecules for CELL_A-1, CELL_B-1, and CELL_C-1 remain obvious.",
            "Inspect and copy corrected/raw cell and UMI tags from named reads.",
            "Check the missing-tag and numeric malformed-CB records remain local, explicit failures.",
            "Apply bulk actions, save, reopen, and confirm tag expressions and metadata links persist.",
        ],
        "scenario-mf-09" => &[
            "Compare the same eight records in bgzip/tabix VCF and BCF/CSI tracks.",
            "Inspect SNV, MNV, multi-allelic, indel, inversion, symbolic deletion, and breakend spans.",
            "Filter and scroll 128 samples; verify pedigree order and phased, unphased, haploid, polyploid, and no-call genotypes.",
            "Inspect INFO, FILTER, GT, DP, AD, GQ, and PL fields and use next/previous variant navigation.",
            "Export selected records/genotypes and confirm the two source formats remain semantically identical.",
        ],
        "scenario-mf-10" => &[
            "Open every declared auxiliary track and inspect its named exact record.",
            "Exercise search, labels, exon/strand structure, inspectors, and next/previous feature navigation.",
            "Navigate both endpoints of BEDPE/interact records and compare SEG/CBS/MAF/GWAS behavior.",
            "Compare independent/shared signal scales, overlay groups, arithmetic output, and both TDF channels.",
            "Export SVG and tables, save/reopen, and confirm every typed format setting persists.",
        ],
        "scenario-mf-11" => &[
            "Open the portable genome and confirm reference, lengths, cytobands, aliases, and default annotation.",
            "Search coordinates, aliases, genes, and variants, including ambiguous and no-result cases.",
            "Compare forward/reverse reference and all six translation frames at known start/stop/partial codons.",
            "Import, edit, navigate, and export regions of interest and review items at their boundary coordinates.",
            "Use locus history, multiple loci, whole-contig, and previous/next navigation, then save/reopen.",
        ],
        "scenario-mf-12" => &[
            "Open the four-track, eight-track, two-panel, and failure-isolation projects in turn.",
            "Add, remove, reorder, hide, collapse, resize, and bulk-edit tracks while preserving stable identities.",
            "Group by metadata and compare linked versus independent panel navigation and reference persistence.",
            "Open failure isolation and confirm exactly one missing source does not blank the three healthy tracks.",
            "Save/reopen, relocate the portable project bundle, and compare the exported track manifest.",
        ],
        "scenario-mf-13" => &[
            "Open native schema versions 1, 2, and 3, then confirm the future version is rejected explicitly.",
            "Import the representative IGV XML and compare every imported, approximated, and ignored notice with the expected report.",
            "Open missing-source, changed-source, partial-workspace, and invalid-setting cases and confirm failures remain isolated.",
            "Materialize the controlled runtime state in a disposable settings directory and verify one autosave recovery choice appears.",
            "Recover the autosave and also test continue-without-recovery; use Save project as… to preserve only the chosen result.",
        ],
        "scenario-mf-14" => &[
            "Start the loopback server, materialize runtime projects, and compare remote sources with their local equivalents.",
            "Exercise HEAD, ranges, adjacent/custom indexes, redirects, signed queries, validators, and bearer authorization.",
            "Switch through fail-first, outage, slow, no-range, short-body, changed-ETag, corrupt, and invalid-content-type profiles.",
            "Test missing/corrupt indexes, wrong CRAM reference, cancellation, retry, and recovery while healthy panels remain usable.",
            "Stop the server and confirm runtime credentials were absent from projects, logs, reports, and workspace files.",
        ],
        "scenario-mf-15" => &[
            "Run every supplied automation batch and inspect stable command IDs, settled-state waits, results, timeouts, and errors.",
            "Exercise locus, panel, track, selection, and settings operations through authenticated local control.",
            "Generate screenshots and every claimed structured, vector, semantic, and tabular export and validate each output.",
            "Complete the common workflow using only the keyboard; check focus order, shortcuts, names, high zoom, high DPI, and reduced motion.",
            "Save/reopen and inspect logs/reports to confirm runtime credentials and sensitive query values are redacted.",
        ],
        _ => &[],
    }
}

fn scenario_failures(id: &str) -> &'static [&'static str] {
    match id {
        "scenario-mf-14" => &[
            "A failed remote source blanks or destabilizes healthy panels.",
            "Credentials or signed query values appear in a saved project, report, or log.",
        ],
        "scenario-mf-13" => &[
            "Compatibility is silently approximated or a future schema is accepted.",
            "Recovery overwrites the original project or offers an invalid autosave.",
        ],
        "scenario-mf-15" => &[
            "An operation reports settled before the requested state is visible.",
            "Keyboard focus becomes lost, hidden, unnamed, or trapped.",
        ],
        _ => &[
            "Named truth changes, disappears, or moves after pan/zoom/save/reopen.",
            "A malformed record crashes the app or hides unrelated healthy evidence.",
        ],
    }
}

fn operational_launch(id: &str) -> &'static str {
    match id {
        "scenario-mf-13" => {
            "cargo run --release -p rgv-desktop -- --session validation/multi-panel-four-track-session-v3.json"
        }
        "scenario-mf-14" => {
            "cargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy"
        }
        "scenario-mf-15" => {
            "cargo run --release -p rgv-desktop -- --batch validation/automation-smoke.json --automation-report benchmark-results/manual-suite-automation.json"
        }
        _ => "cargo run --release -p rgv-manual-fixtures -- list",
    }
}

fn write_validation_index(validation_root: &Path) -> Result<()> {
    let mut text = String::from(
        "# RGV manual fixture suite\n\nThe suite is fixture-ready: its hashes, portable projects, exact semantic truth, production readers, relocation, coverage catalog, and controlled failure inputs pass mechanically. Manual scenario results remain `not run` until a tester records them; synthetic readiness does not replace real-data or platform qualification.\n\n## Generate and verify\n\n```bash\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- verify --require-complete\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list\n```\n\n`cargo run` builds changed binaries automatically; a separate `cargo build` is optional. Use release mode for visual testing. The expected CRAM MD5 diagnostic during verification is the deliberate wrong-reference rejection.\n\n## Short pass\n\nRun MF-01, MF-02, MF-03, MF-09, MF-10, MF-12, and one MF-14 outage/recovery profile. This covers ordinary alignments, base events, RNA, cohorts, formats, workspaces, and isolation without attempting every specialist mode.\n\n## Remote lab\n\n```bash\ncargo build --release -p rgv-validation-runner --bins\ntarget/release/rgv-fixture-server-check --suite benchmark-data/manual-suite\ncargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy --write-endpoint /tmp/rgv-fixture-endpoint.json\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- materialize-remote --endpoint /tmp/rgv-fixture-endpoint.json --output /tmp/rgv-remote-projects\n```\n\nThe endpoint document contains runtime-only credentials and must remain outside the repository.\n\n## Full pass\n\n",
    );
    for scenario in SCENARIOS {
        let _ = writeln!(
            text,
            "- [{}: {}](scenarios/{}.md)",
            scenario.id, scenario.title, scenario.id
        );
    }
    text.push_str(
        "\nSynthetic truth proves deterministic mechanics; it does not replace the representative private-technology matrix in `../MANUAL_QUALIFICATION.md`.\n",
    );
    write_text(&validation_root.join("README.md"), &text)
}

fn write_server_profiles(validation_root: &Path) -> Result<()> {
    for (name, status, behavior) in [
        (
            "healthy",
            206,
            "HEAD and bounded GET byte ranges succeed with stable validators",
        ),
        (
            "auth-required",
            401,
            "requests without the runtime bearer token are rejected",
        ),
        (
            "redirect",
            307,
            "the first request redirects while preserving signed query keys",
        ),
        (
            "fail-first-n",
            503,
            "the first configured number of data requests fail, then recover",
        ),
        (
            "outage",
            503,
            "all data requests fail until the profile is changed",
        ),
        (
            "slow",
            206,
            "responses are delayed by the configured bounded duration",
        ),
        (
            "no-range",
            200,
            "Range is deliberately ignored and Accept-Ranges is omitted",
        ),
        (
            "short-body",
            206,
            "the response body is shorter than its requested range",
        ),
        (
            "changed-etag",
            206,
            "the validator changes after the first data request",
        ),
        (
            "corrupt",
            206,
            "one deterministic byte in the response body is changed",
        ),
        (
            "invalid-content-type",
            206,
            "the bytes are valid but the response declares an incompatible media type",
        ),
    ] {
        write_json(
            &validation_root.join(format!("server-profiles/{name}.json")),
            &serde_json::json!({
                "schema_version": 1,
                "profile": name,
                "representative_status": status,
                "expected_behavior": behavior,
                "external_network_required": false
            }),
        )?;
    }
    write_json(
        &validation_root.join("server-profiles/remote-source-map.json"),
        &serde_json::json!({
            "schema_version": 1,
            "bam": { "source": "dep-mixed-bam", "index": "dep-mixed-bai" },
            "cram": { "source": "manual-reference-cram", "index": "manual-reference-crai", "reference": "manual-reference-fasta", "reference_index": "manual-reference-fai" },
            "reference": { "source": "dep-variants-fasta", "index": "dep-variants-fai" },
            "variants": { "source": "manual-cohort-bcf", "index": "manual-cohort-csi" },
            "bigwig": "dep-demo-signal.bigWig",
            "bigbed": "dep-demo-features.bigBed",
            "big_gene_pred": "dep-demo-genes.bigGenePred",
            "big_narrow_peak": "dep-demo-peaks.bigNarrowPeak",
            "tdf": "dep-demo-archive.tdf"
        }),
    )
}

fn dependencies(root: &Path) -> Result<Vec<FixtureDependency>> {
    let entries = [
        (
            "dep-mixed-bam",
            "benchmark-data/mixed.bam",
            "mixed short/long/RNA/specialist alignment corpus",
        ),
        (
            "dep-mixed-bai",
            "benchmark-data/mixed.bam.bai",
            "BAI for the mixed alignment corpus",
        ),
        (
            "dep-variants-bam",
            "benchmark-data/variants.bam",
            "exact base/indel/Ultima alignment truth",
        ),
        (
            "dep-variants-bai",
            "benchmark-data/variants.bam.bai",
            "BAI for exact base/indel truth",
        ),
        (
            "dep-variants-fasta",
            "benchmark-data/variants.fa",
            "matching exact-event reference",
        ),
        (
            "dep-variants-fai",
            "benchmark-data/variants.fa.fai",
            "FAI for exact-event reference",
        ),
        (
            "dep-demo-project",
            "benchmark-data/demo-tracks/rgv-demo-project.json",
            "existing verified mixed-format workspace",
        ),
        (
            "dep-demo-bcf",
            "benchmark-data/demo-tracks/variants.bcf",
            "typed variants and genotypes",
        ),
        (
            "dep-demo-bcf-csi",
            "benchmark-data/demo-tracks/variants.bcf.csi",
            "CSI for typed variants",
        ),
        (
            "dep-session-v1",
            "validation/session-v1.json",
            "native schema-v1 migration",
        ),
        (
            "dep-session-v2",
            "validation/four-track-session-v2.json",
            "native schema-v2 multi-track migration",
        ),
        (
            "dep-session-v3",
            "validation/multi-panel-four-track-session-v3.json",
            "native schema-v3 multi-panel session",
        ),
        (
            "dep-session-future",
            "validation/future-session-v99.json",
            "unsupported future-schema rejection",
        ),
        (
            "dep-automation-batch",
            "validation/automation-smoke.json",
            "structured automation and export smoke",
        ),
        (
            "dep-workspace-batch",
            "validation/demo-workspace-smoke.json",
            "mixed-workspace settled-state smoke",
        ),
    ];
    let mut dependencies = entries
        .into_iter()
        .map(|(id, path, purpose)| dependency_for(root, &root.join(path), id, purpose))
        .collect::<Result<Vec<_>>>()?;
    for entry in fs::read_dir(root.join("benchmark-data/demo-tracks"))? {
        let path = entry?.path();
        if path.is_file()
            && path.file_name().is_some_and(|name| {
                !matches!(
                    name.to_string_lossy().as_ref(),
                    "rgv-demo-project.json" | "variants.bcf" | "variants.bcf.csi"
                )
            })
        {
            let name = path.file_name().unwrap().to_string_lossy();
            dependencies.push(dependency_for(
                root,
                &path,
                format!("dep-demo-{name}"),
                format!("existing mixed-format source {name}"),
            )?);
        }
    }
    Ok(dependencies)
}

fn generated_artifacts(
    root: &Path,
    suite_root: &Path,
    validation_root: &Path,
) -> Result<Vec<FixtureArtifact>> {
    let mut artifacts = Vec::new();
    let files = [
        (
            "manual-smrt-bam",
            "data/alignments/smrt-evidence.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-smrt-bai",
            "data/alignments/smrt-evidence.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-sbx-bam",
            "data/alignments/sbx-evidence.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-sbx-bai",
            "data/alignments/sbx-evidence.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-ultima-bam",
            "data/alignments/ultima-evidence.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-ultima-bai",
            "data/alignments/ultima-evidence.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-rna-a-bam",
            "data/alignments/rna-sample-a.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-rna-a-bai",
            "data/alignments/rna-sample-a.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-rna-b-bam",
            "data/alignments/rna-sample-b.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-rna-b-bai",
            "data/alignments/rna-sample-b.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-rna-annotation",
            "data/tracks/rna-transcripts.gff3",
            "text/vnd.rgv.gff3",
            FixtureValidity::Valid,
        ),
        (
            "manual-specialist-bam",
            "data/alignments/specialist.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-specialist-bai",
            "data/alignments/specialist.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-specialist-csi",
            "data/alignments/specialist.bam.csi",
            "application/vnd.rgv.csi",
            FixtureValidity::Valid,
        ),
        (
            "manual-reference-cram",
            "data/alignments/reference-dependent.cram",
            "application/vnd.rgv.cram",
            FixtureValidity::Valid,
        ),
        (
            "manual-reference-crai",
            "data/alignments/reference-dependent.cram.crai",
            "application/vnd.rgv.crai",
            FixtureValidity::Valid,
        ),
        (
            "manual-reference-fasta",
            "data/references/manual.fa",
            "application/vnd.rgv.fasta",
            FixtureValidity::Valid,
        ),
        (
            "manual-reference-fai",
            "data/references/manual.fa.fai",
            "application/vnd.rgv.fai",
            FixtureValidity::Valid,
        ),
        (
            "manual-cohort-vcf",
            "data/variants/cohort.vcf.gz",
            "application/vnd.rgv.vcf+bgzip",
            FixtureValidity::Valid,
        ),
        (
            "manual-cohort-tbi",
            "data/variants/cohort.vcf.gz.tbi",
            "application/vnd.rgv.tabix",
            FixtureValidity::Valid,
        ),
        (
            "manual-cohort-bcf",
            "data/variants/cohort.bcf",
            "application/vnd.rgv.bcf",
            FixtureValidity::Valid,
        ),
        (
            "manual-cohort-csi",
            "data/variants/cohort.bcf.csi",
            "application/vnd.rgv.csi",
            FixtureValidity::Valid,
        ),
        (
            "manual-cytobands",
            "data/tracks/cytobands.txt",
            "text/tab-separated-values",
            FixtureValidity::Valid,
        ),
        (
            "manual-default-annotations",
            "data/tracks/default-annotations.bed",
            "text/vnd.rgv.bed",
            FixtureValidity::Valid,
        ),
        (
            "manual-gff3",
            "data/tracks/annotations.gff3",
            "text/vnd.rgv.gff3",
            FixtureValidity::Valid,
        ),
        (
            "manual-gff3-gzip",
            "data/tracks/annotations.gff3.gz",
            "application/vnd.rgv.gff3+gzip",
            FixtureValidity::Valid,
        ),
        (
            "manual-gtf",
            "data/tracks/annotations.gtf",
            "text/vnd.rgv.gtf",
            FixtureValidity::Valid,
        ),
        (
            "manual-ref-flat",
            "data/tracks/transcripts.refFlat",
            "text/vnd.rgv.ref-flat",
            FixtureValidity::Valid,
        ),
        (
            "manual-fixed-wig",
            "data/tracks/signal-fixed.wig",
            "text/vnd.rgv.wig",
            FixtureValidity::Valid,
        ),
        (
            "manual-cbs",
            "data/tracks/cohort.cbs",
            "text/vnd.rgv.cbs",
            FixtureValidity::Valid,
        ),
        (
            "manual-association-linear",
            "data/tracks/association.linear",
            "text/vnd.rgv.gwas",
            FixtureValidity::Valid,
        ),
        (
            "manual-association-logistic",
            "data/tracks/association.logistic",
            "text/vnd.rgv.gwas",
            FixtureValidity::Valid,
        ),
        (
            "manual-association-assoc",
            "data/tracks/association.assoc",
            "text/vnd.rgv.gwas",
            FixtureValidity::Valid,
        ),
        (
            "manual-association-qassoc",
            "data/tracks/association.qassoc",
            "text/vnd.rgv.gwas",
            FixtureValidity::Valid,
        ),
        (
            "manual-genome-config",
            "genomes/manual.rgvgenome",
            "application/vnd.rgv.genome+json",
            FixtureValidity::Valid,
        ),
        (
            "manual-sample-metadata",
            "metadata/samples.tsv",
            "text/tab-separated-values",
            FixtureValidity::Valid,
        ),
        (
            "manual-roi-import",
            "imports/regions.bed",
            "text/vnd.rgv.regions",
            FixtureValidity::Valid,
        ),
        (
            "manual-review-import",
            "imports/review-items.tsv",
            "text/vnd.rgv.review-items",
            FixtureValidity::Valid,
        ),
        (
            "manual-igv-import",
            "imports/representative-igv-session.xml",
            "application/vnd.igv.session+xml",
            FixtureValidity::Valid,
        ),
        (
            "manual-igv-expected",
            "expected/igv-import-report.json",
            "application/vnd.rgv.igv-import-report+json",
            FixtureValidity::Valid,
        ),
        (
            "manual-recovery-autosave",
            "recovery/autosaves/manual-00000000000000000001.autosave.json",
            "application/vnd.rgv.session+json",
            FixtureValidity::Valid,
        ),
        (
            "manual-recovery-state",
            "recovery/runtime-state.json",
            "application/vnd.rgv.recovery-state+json",
            FixtureValidity::Valid,
        ),
        (
            "manual-relocation-project",
            "relocation/source/relocatable.rgv.json",
            "application/vnd.rgv.session+json",
            FixtureValidity::Valid,
        ),
        (
            "manual-relocation-bam",
            "relocation/source/data/specialist.bam",
            "application/vnd.rgv.bam",
            FixtureValidity::Valid,
        ),
        (
            "manual-relocation-bai",
            "relocation/source/data/specialist.bam.bai",
            "application/vnd.rgv.bai",
            FixtureValidity::Valid,
        ),
        (
            "manual-relocation-fasta",
            "relocation/source/data/manual.fa",
            "application/vnd.rgv.fasta",
            FixtureValidity::Valid,
        ),
        (
            "manual-relocation-fai",
            "relocation/source/data/manual.fa.fai",
            "application/vnd.rgv.fai",
            FixtureValidity::Valid,
        ),
        (
            "manual-changed-source-state",
            "expected/changed-source-state.json",
            "application/vnd.rgv.source-state+json",
            FixtureValidity::Valid,
        ),
        (
            "failure-changed-source",
            "failures/changed-source.bed",
            "text/vnd.rgv.bed",
            FixtureValidity::IntentionallyInvalid,
        ),
        (
            "failure-invalid-session",
            "failures/invalid-session.json",
            "application/vnd.rgv.session+json",
            FixtureValidity::IntentionallyInvalid,
        ),
        (
            "failure-truncated-bed",
            "failures/truncated.bed",
            "text/vnd.rgv.bed",
            FixtureValidity::IntentionallyInvalid,
        ),
        (
            "failure-wrong-reference-fasta",
            "failures/wrong-reference.fa",
            "application/vnd.rgv.fasta",
            FixtureValidity::IntentionallyInvalid,
        ),
        (
            "failure-wrong-reference-fai",
            "failures/wrong-reference.fa.fai",
            "application/vnd.rgv.fai",
            FixtureValidity::IntentionallyInvalid,
        ),
    ];
    for (id, relative, media_type, validity) in files {
        artifacts.push(artifact_for(
            root,
            &suite_root.join(relative),
            id,
            media_type,
            validity,
            truth_ids_for_fixture(id),
            projects_for_fixture(id),
            scenarios_for_fixture(id),
        )?);
    }
    for project in PROJECTS {
        artifacts.push(artifact_for(
            root,
            &suite_root.join("projects").join(project.filename),
            format!("project-artifact-{}", project.id),
            "application/vnd.rgv.session+json",
            FixtureValidity::Valid,
            vec![format!(
                "truth-mf-{}",
                project
                    .id
                    .trim_start_matches("mf-")
                    .split('-')
                    .next()
                    .expect("project IDs contain an ordinal")
            )],
            vec![project.id.to_owned()],
            vec![project.scenario_id.to_owned()],
        )?);
    }
    for index in 1..=14 {
        let truth_id = format!("truth-mf-{index:02}");
        let project = PROJECTS
            .iter()
            .find(|project| project.id.starts_with(&format!("mf-{index:02}-")))
            .context("truth document has no matching project")?;
        artifacts.push(artifact_for(
            root,
            &suite_root.join(format!("expected/{truth_id}.json")),
            format!("truth-document-mf-{index:02}"),
            "application/vnd.rgv.truth+json",
            FixtureValidity::Valid,
            vec![truth_id],
            vec![project.id.to_owned()],
            vec![format!("scenario-mf-{index:02}")],
        )?);
    }
    for scenario in SCENARIOS {
        artifacts.push(artifact_for(
            root,
            &validation_root.join(format!("scenarios/{}.md", scenario.id)),
            format!("scenario-card-{}", scenario.id),
            "text/markdown",
            FixtureValidity::Valid,
            Vec::new(),
            scenario
                .project_id
                .map(|id| vec![id.to_owned()])
                .unwrap_or_default(),
            vec![scenario.id.to_owned()],
        )?);
    }
    for entry in fs::read_dir(validation_root.join("batches"))? {
        let path = entry?.path();
        if path.is_file() {
            let filename = path
                .file_name()
                .context("automation batch has no filename")?
                .to_string_lossy();
            artifacts.push(artifact_for(
                root,
                &path,
                format!("manual-batch-{filename}"),
                "application/vnd.rgv.automation-batch+json",
                FixtureValidity::Valid,
                Vec::new(),
                Vec::new(),
                vec!["scenario-mf-15".to_owned()],
            )?);
        }
    }
    for entry in fs::read_dir(validation_root.join("server-profiles"))? {
        let path = entry?.path();
        if path.is_file() {
            let filename = path
                .file_name()
                .context("server profile has no filename")?
                .to_string_lossy();
            artifacts.push(artifact_for(
                root,
                &path,
                format!("server-profile-{filename}"),
                "application/vnd.rgv.fixture-server-profile+json",
                FixtureValidity::Valid,
                Vec::new(),
                Vec::new(),
                vec!["scenario-mf-14".to_owned()],
            )?);
        }
    }
    artifacts.push(artifact_for(
        root,
        &validation_root.join("README.md"),
        "manual-suite-index",
        "text/markdown",
        FixtureValidity::Valid,
        Vec::new(),
        Vec::new(),
        SCENARIOS
            .iter()
            .map(|scenario| scenario.id.to_owned())
            .collect(),
    )?);
    for artifact in &mut artifacts {
        let truth = artifact.truth_ids.join(",");
        if [
            "truth-mf-03",
            "truth-mf-04",
            "truth-mf-05",
            "truth-mf-08",
            "truth-mf-14",
        ]
        .iter()
        .any(|id| truth.contains(id))
        {
            artifact.assembly = Some("manual-synthetic".to_owned());
            artifact
                .contigs
                .insert(MANUAL_CONTIG.to_owned(), MANUAL_CONTIG_LENGTH as u64);
        } else if [
            "truth-mf-02",
            "truth-mf-07",
            "truth-mf-09",
            "truth-mf-10",
            "truth-mf-11",
        ]
        .iter()
        .any(|id| truth.contains(id))
        {
            artifact.assembly = Some("variants-synthetic".to_owned());
            artifact.contigs.insert("chrVariants".to_owned(), 20_000);
        } else if artifact
            .truth_ids
            .iter()
            .any(|id| matches!(id.as_str(), "truth-mf-01" | "truth-mf-12" | "truth-mf-13"))
        {
            artifact.assembly = Some("mixed-synthetic".to_owned());
            artifact.contigs.insert("chr1".to_owned(), 3_000_000);
            artifact.contigs.insert("chr2".to_owned(), 3_000_000);
        }
        if let Some(project_id) = artifact.id.strip_prefix("project-artifact-") {
            artifact.source_fixture_ids = project_source_fixture_ids(project_id);
        }
    }
    Ok(artifacts)
}

fn project_source_fixture_ids(project_id: &str) -> Vec<String> {
    let ids: &[&str] = match project_id {
        "mf-01-alignment-core" => &["dep-mixed-bam", "dep-mixed-bai"],
        "mf-02-alignment-events" => &["dep-variants-bam", "dep-variants-fasta"],
        "mf-03-rna-sashimi" => &[
            "manual-rna-a-bam",
            "manual-rna-b-bam",
            "manual-reference-fasta",
            "manual-rna-annotation",
        ],
        "mf-04-bisulfite" | "mf-08-single-cell" => {
            &["manual-specialist-bam", "manual-reference-fasta"]
        }
        "mf-05-smrt-kinetics" => &["manual-smrt-bam", "manual-reference-fasta"],
        "mf-06-sbx" => &["manual-sbx-bam", "manual-reference-fasta"],
        "mf-07-ultima" => &["manual-ultima-bam", "manual-reference-fasta"],
        "mf-09-cohort-variants" => &[
            "manual-cohort-vcf",
            "manual-cohort-bcf",
            "manual-sample-metadata",
        ],
        "mf-10-mixed-tracks" => &["dep-demo-project"],
        "mf-11-genome-review" => &["manual-genome-config", "dep-variants-fasta"],
        "mf-12-workspaces"
        | "mf-12-four-track"
        | "mf-12-eight-track"
        | "mf-12-failure-isolation" => &["dep-mixed-bam", "manual-sample-metadata"],
        "mf-13-recovery" => &[
            "manual-recovery-autosave",
            "manual-recovery-state",
            "manual-igv-import",
            "manual-igv-expected",
        ],
        "mf-14-reference-cram" => &["manual-reference-cram", "manual-reference-fasta"],
        _ => &[],
    };
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn truth_ids_for_fixture(id: &str) -> Vec<String> {
    match id {
        "manual-smrt-bam" => vec!["truth-mf-05".to_owned()],
        "manual-sbx-bam" => vec!["truth-mf-06".to_owned()],
        "manual-ultima-bam" => vec!["truth-mf-07".to_owned()],
        "manual-relocation-project" | "manual-relocation-bam" | "manual-relocation-fasta" => {
            vec!["truth-mf-12".to_owned()]
        }
        "manual-igv-import"
        | "manual-igv-expected"
        | "manual-recovery-autosave"
        | "manual-recovery-state"
        | "manual-changed-source-state"
        | "failure-changed-source" => vec!["truth-mf-13".to_owned()],
        "manual-rna-a-bam" | "manual-rna-b-bam" | "manual-rna-annotation" => {
            vec!["truth-mf-03".to_owned()]
        }
        "manual-specialist-bam" | "manual-reference-fasta" => {
            vec![
                "truth-mf-04".to_owned(),
                "truth-mf-05".to_owned(),
                "truth-mf-06".to_owned(),
                "truth-mf-08".to_owned(),
            ]
        }
        "manual-genome-config" | "manual-roi-import" | "manual-review-import" => {
            vec!["truth-mf-11".to_owned()]
        }
        "manual-sample-metadata" => vec!["truth-mf-08".to_owned(), "truth-mf-09".to_owned()],
        "manual-cohort-vcf" | "manual-cohort-bcf" => vec!["truth-mf-09".to_owned()],
        "manual-gff3"
        | "manual-gff3-gzip"
        | "manual-gtf"
        | "manual-ref-flat"
        | "manual-fixed-wig"
        | "manual-cbs"
        | "manual-association-linear"
        | "manual-association-logistic"
        | "manual-association-assoc"
        | "manual-association-qassoc" => vec!["truth-mf-10".to_owned()],
        _ => Vec::new(),
    }
}

fn projects_for_fixture(id: &str) -> Vec<String> {
    match id {
        "manual-smrt-bam" => vec!["mf-05-smrt-kinetics".to_owned()],
        "manual-sbx-bam" => vec!["mf-06-sbx".to_owned()],
        "manual-ultima-bam" => vec!["mf-07-ultima".to_owned()],
        "manual-relocation-project" | "manual-relocation-bam" | "manual-relocation-fasta" => {
            vec!["mf-12-workspaces".to_owned()]
        }
        "manual-igv-import"
        | "manual-igv-expected"
        | "manual-recovery-autosave"
        | "manual-recovery-state"
        | "manual-changed-source-state"
        | "failure-changed-source" => vec!["mf-13-recovery".to_owned()],
        "manual-rna-a-bam" | "manual-rna-b-bam" | "manual-rna-annotation" => {
            vec!["mf-03-rna-sashimi".to_owned()]
        }
        "manual-specialist-bam" | "manual-reference-fasta" => {
            vec![
                "mf-04-bisulfite".to_owned(),
                "mf-05-smrt-kinetics".to_owned(),
                "mf-06-sbx".to_owned(),
                "mf-08-single-cell".to_owned(),
            ]
        }
        "manual-genome-config" | "manual-roi-import" | "manual-review-import" => {
            vec!["mf-11-genome-review".to_owned()]
        }
        "manual-cohort-vcf" | "manual-cohort-bcf" => {
            vec!["mf-09-cohort-variants".to_owned()]
        }
        "manual-gff3"
        | "manual-gff3-gzip"
        | "manual-gtf"
        | "manual-ref-flat"
        | "manual-fixed-wig"
        | "manual-cbs"
        | "manual-association-linear"
        | "manual-association-logistic"
        | "manual-association-assoc"
        | "manual-association-qassoc" => vec!["mf-10-mixed-tracks".to_owned()],
        "manual-sample-metadata" => vec![
            "mf-08-single-cell".to_owned(),
            "mf-09-cohort-variants".to_owned(),
            "mf-12-workspaces".to_owned(),
        ],
        _ => Vec::new(),
    }
}

fn scenarios_for_fixture(id: &str) -> Vec<String> {
    projects_for_fixture(id)
        .into_iter()
        .filter_map(|project| {
            PROJECTS
                .iter()
                .find(|definition| definition.id == project)
                .map(|definition| definition.scenario_id.to_owned())
        })
        .collect()
}

fn project_manifest(root: &Path, suite_root: &Path) -> Result<Vec<FixtureProject>> {
    PROJECTS
        .iter()
        .map(|project| {
            Ok(FixtureProject {
                id: project.id.to_owned(),
                title: project.title.to_owned(),
                path: rgv_validation_runner::relative_workspace_path(
                    &suite_root.join("projects").join(project.filename),
                    root,
                )?,
                purpose: project.purpose.to_owned(),
                initial_locus: project.initial_locus.to_owned(),
                scenario_id: project.scenario_id.to_owned(),
                expected_unresolved_source_basenames: if project.id == "mf-12-failure-isolation" {
                    vec!["does-not-exist.bam".to_owned()]
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

fn coverage_catalog(root: &Path, manifest: &FixtureManifest) -> Result<CoverageCatalog> {
    let source_paths = ["docs/DESKTOP_FEATURE_ACCESS.md", "docs/FORMAT_SUPPORT.md"];
    let sources = source_paths
        .iter()
        .map(|path| {
            Ok(CoverageSource {
                path: (*path).to_owned(),
                sha256: sha256_file(&root.join(path))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = Vec::new();
    let mut ordinal = 1_u32;
    for source_path in source_paths {
        let text = fs::read_to_string(root.join(source_path))?;
        for claim in markdown_table_claims(&text) {
            let mapping = map_claim(&claim)?;
            entries.push(CoverageEntry {
                id: format!("coverage-{ordinal:03}"),
                capability: claim.clone(),
                source_path: source_path.to_owned(),
                source_claim: claim,
                disposition: if mapping.project_ids.is_empty() {
                    CoverageDisposition::OperationalReady
                } else {
                    CoverageDisposition::Ready
                },
                preflight_status: "passed".to_owned(),
                manual_qualification_status: "not-run".to_owned(),
                fixture_ids: mapping.fixture_ids,
                project_ids: mapping.project_ids,
                scenario_ids: mapping.scenario_ids,
                batch_ids: mapping.batch_ids,
                truth_ids: mapping.truth_ids,
                real_data_follow_up: mapping.real_data_follow_up,
            });
            ordinal += 1;
        }
    }
    for (capability, scenario_id) in [
        ("Deterministic batch automation", "scenario-mf-15"),
        ("Authenticated local external control", "scenario-mf-15"),
        ("Structured evidence and snapshot exports", "scenario-mf-15"),
        (
            "Bearer authentication without persistence",
            "scenario-mf-14",
        ),
    ] {
        entries.push(CoverageEntry {
            id: format!("coverage-{ordinal:03}"),
            capability: capability.to_owned(),
            source_path: "docs/DESKTOP_FEATURE_ACCESS.md".to_owned(),
            source_claim: capability.to_owned(),
            disposition: CoverageDisposition::OperationalReady,
            preflight_status: "passed".to_owned(),
            manual_qualification_status: "not-run".to_owned(),
            fixture_ids: Vec::new(),
            project_ids: Vec::new(),
            scenario_ids: vec![scenario_id.to_owned()],
            batch_ids: if scenario_id == "scenario-mf-15" {
                vec![
                    "manual-batch-mf15-complete-exports.json".to_owned(),
                    "manual-batch-mf15-variant-track-exports.json".to_owned(),
                    "manual-batch-mf15-controlled-failure.json".to_owned(),
                    "dep-automation-batch".to_owned(),
                ]
            } else {
                Vec::new()
            },
            truth_ids: Vec::new(),
            real_data_follow_up: None,
        });
        ordinal += 1;
    }
    let known_projects = manifest
        .projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        entries
            .iter()
            .flat_map(|entry| &entry.project_ids)
            .all(|id| known_projects.contains(id.as_str())),
        "coverage mapping references an unknown project"
    );
    Ok(CoverageCatalog {
        schema_version: MANUAL_SUITE_SCHEMA_VERSION,
        sources,
        entries,
    })
}

struct ClaimMapping {
    fixture_ids: Vec<String>,
    project_ids: Vec<String>,
    scenario_ids: Vec<String>,
    batch_ids: Vec<String>,
    truth_ids: Vec<String>,
    real_data_follow_up: Option<String>,
}

fn map_claim(claim: &str) -> Result<ClaimMapping> {
    let lower = claim.to_ascii_lowercase();
    let projects: &[&str] = match lower.as_str() {
        "accessibility preferences and keyboard operation" => &[],
        "arithmetic, shared scales, overlays"
        | "bigwig/tdf/bedgraph/wig"
        | "bed/gff/gtf/genepred/refflat/peaks/psl/bigbed"
        | "bedpe/interact"
        | "gwas association summary"
        | "multi-channel tdf"
        | "seg/cbs copy number and maf mutations"
        | "local auxiliary track" => &["mf-10-mixed-tracks"],
        "autosave and recovery" | "native or igv project" => &["mf-13-recovery"],
        "base display, quality shading, indel/label thresholds, modified-base visibility and legend" => {
            &["mf-01-alignment-core", "mf-02-alignment-events"]
        }
        "bisulfite contexts, pacbio kinetics, sbx, and ultima evidence" => &[
            "mf-04-bisulfite",
            "mf-05-smrt-kinetics",
            "mf-06-sbx",
            "mf-07-ultima",
        ],
        "cram plus remote reference" => &["mf-14-reference-cram"],
        "consensus, reference, read, cigar, tags, and event sequence copy" => {
            &["mf-01-alignment-core", "mf-02-alignment-events"]
        }
        "coordinate, annotation-name, and variant-id search" => &[
            "mf-09-cohort-variants",
            "mf-10-mixed-tracks",
            "mf-11-genome-review",
        ],
        "expanded/squished/collapsed and continuous vertical density"
        | "mate and supplementary-chain navigation" => &["mf-01-alignment-core"],
        "grouping, sorting, direction, pairing, template bounds, and tag expressions" => {
            &["mf-01-alignment-core", "mf-08-single-cell"]
        }
        "local bam/cram" => &["mf-01-alignment-core", "mf-14-reference-cram"],
        "local fasta"
        | "portable genome"
        | "reverse reference and six-frame translation"
        | "regions of interest"
        | "review queue" => &["mf-11-genome-review"],
        "multiple loci" => &["mf-11-genome-review", "mf-12-workspaces"],
        "png and semantic evidence exports" => &["mf-10-mixed-tracks"],
        "previous/next annotation or loaded evidence" => &[
            "mf-09-cohort-variants",
            "mf-10-mixed-tracks",
            "mf-11-genome-review",
        ],
        "rna and sashimi" => &["mf-03-rna-sashimi"],
        "read, cigar event, pileup, modification, and junction inspection" => &[
            "mf-01-alignment-core",
            "mf-02-alignment-events",
            "mf-03-rna-sashimi",
        ],
        "reference, coverage, junction/sashimi, and read subtracks" => {
            &["mf-01-alignment-core", "mf-03-rna-sashimi"]
        }
        "remote bam/cram" | "remote fasta" | "remote indexed auxiliary track" => {
            &["mf-14-reference-cram"]
        }
        "sample metadata and bulk track actions" => &[
            "mf-08-single-cell",
            "mf-09-cohort-variants",
            "mf-12-workspaces",
        ],
        "save/save-as and portable relocation"
        | "several alignment samples"
        | "track visibility, collapse, order, height, appearance, and removal" => {
            &["mf-12-workspaces"]
        }
        "vcf/bcf and genotypes" => &["mf-09-cohort-variants"],
        "additional annotation"
        | "archived quantitative"
        | "association summary"
        | "copy number"
        | "general annotation"
        | "indexed annotation"
        | "indexed peaks"
        | "indexed transcripts"
        | "interactions"
        | "peaks"
        | "quantitative"
        | "somatic mutations"
        | "transcript annotation" => &["mf-10-mixed-tracks"],
        "alignments" => &[
            "mf-01-alignment-core",
            "mf-03-rna-sashimi",
            "mf-04-bisulfite",
            "mf-05-smrt-kinetics",
            "mf-06-sbx",
            "mf-07-ultima",
            "mf-08-single-cell",
            "mf-14-reference-cram",
        ],
        "reference" => &["mf-11-genome-review", "mf-14-reference-cram"],
        "variants" => &["mf-09-cohort-variants"],
        _ => anyhow::bail!("manual fixture coverage has no exact mapping for claim {claim:?}"),
    };
    if projects.is_empty() {
        return Ok(ClaimMapping {
            fixture_ids: Vec::new(),
            project_ids: Vec::new(),
            scenario_ids: vec!["scenario-mf-15".to_owned()],
            batch_ids: vec![
                "manual-batch-mf15-complete-exports.json".to_owned(),
                "manual-batch-mf15-variant-track-exports.json".to_owned(),
                "manual-batch-mf15-controlled-failure.json".to_owned(),
                "dep-automation-batch".to_owned(),
            ],
            truth_ids: Vec::new(),
            real_data_follow_up: Some(
                "Repeat accessibility checks on each supported desktop platform.".to_owned(),
            ),
        });
    }
    let definitions = projects
        .iter()
        .map(|id| {
            PROJECTS
                .iter()
                .find(|project| project.id == *id)
                .with_context(|| format!("coverage mapping references unknown project {id}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let scenario_ids = definitions
        .iter()
        .map(|project| project.scenario_id.to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let truth_ids = definitions
        .iter()
        .map(|project| {
            let ordinal = project
                .id
                .trim_start_matches("mf-")
                .split('-')
                .next()
                .expect("static project ID has an ordinal");
            format!("truth-mf-{ordinal}")
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let real_data_follow_up = projects
        .iter()
        .any(|id| matches!(*id, "mf-05-smrt-kinetics" | "mf-06-sbx" | "mf-07-ultima"))
        .then(|| {
            "Repeat specialist evidence checks with representative private real-world data."
                .to_owned()
        });
    Ok(ClaimMapping {
        fixture_ids: definitions
            .iter()
            .map(|project| format!("project-artifact-{}", project.id))
            .collect(),
        project_ids: projects.iter().map(|id| (*id).to_owned()).collect(),
        scenario_ids,
        batch_ids: Vec::new(),
        truth_ids,
        real_data_follow_up,
    })
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
