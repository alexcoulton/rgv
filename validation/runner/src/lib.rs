use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use rgv_core::{
    AlignmentColorMode, AutomationBatch, AutomationCommand, AutomationExportKind,
    AutomationSnapshotFormat, BisulfiteContext, GenerationClock, GenomeConfiguration,
    GenomicInterval, PanelGeneration, PanelId, QuantitativeArithmetic, ReferenceTranslationFrame,
    SashimiScaleMode, SbxSimplexTailMode, SessionDocument, SmrtKineticsMode, TrackFormat,
    TrackKind, TrackSource, UltimaQualityMode,
};
use rgv_engine::{TrackEngine, TrackEngineEvent, TrackSnapshotData, TrackSpec, TrackSpecKind};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentQuery, AlignmentSource, AuxValue, FastaReference, FeatureFormat,
    LocalAlignmentSource, LocalVariantSource, QueryDetail, QueryOptions, TextFeatureSource,
    UltimaQualitySource, VariantType, analyze_ultima_record, smrt_kinetics_series,
    summarize_bisulfite_record, summarize_sbx_record,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANUAL_SUITE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_SUITE_ROOT: &str = "benchmark-data/manual-suite";
pub const DEFAULT_VALIDATION_ROOT: &str = "validation/manual-suite";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageDisposition {
    Planned,
    Ready,
    FailureReady,
    OperationalReady,
    RealDataOnly,
    Deferred,
}

impl CoverageDisposition {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        !matches!(self, Self::Planned)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoverageSource {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub id: String,
    pub capability: String,
    pub source_path: String,
    pub source_claim: String,
    pub disposition: CoverageDisposition,
    pub preflight_status: String,
    pub manual_qualification_status: String,
    #[serde(default)]
    pub fixture_ids: Vec<String>,
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    #[serde(default)]
    pub batch_ids: Vec<String>,
    #[serde(default)]
    pub truth_ids: Vec<String>,
    #[serde(default)]
    pub real_data_follow_up: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoverageCatalog {
    pub schema_version: u32,
    pub sources: Vec<CoverageSource>,
    pub entries: Vec<CoverageEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureValidity {
    Valid,
    IntentionallyInvalid,
    RuntimeGenerated,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FixtureArtifact {
    pub id: String,
    pub generator_schema_version: u32,
    /// Workspace-relative path. Paths are portable and cannot escape the workspace.
    pub path: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
    pub validity: FixtureValidity,
    #[serde(default)]
    pub source_fixture_ids: Vec<String>,
    #[serde(default)]
    pub assembly: Option<String>,
    #[serde(default)]
    pub contigs: BTreeMap<String, u64>,
    #[serde(default)]
    pub truth_ids: Vec<String>,
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    pub privacy: String,
    pub redistributable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FixtureDependency {
    pub id: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub purpose: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FixtureProject {
    pub id: String,
    pub title: String,
    pub path: String,
    pub purpose: String,
    pub initial_locus: String,
    pub scenario_id: String,
    /// Basenames of deliberately unavailable local sources used to prove partial-workspace recovery.
    #[serde(default)]
    pub expected_unresolved_source_basenames: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FixtureManifest {
    pub schema_version: u32,
    pub generator: String,
    pub corpus_size_budget_bytes: u64,
    pub artifacts: Vec<FixtureArtifact>,
    pub dependencies: Vec<FixtureDependency>,
    pub projects: Vec<FixtureProject>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FixtureEndpointDocument {
    pub schema_version: u32,
    pub process_id: u32,
    pub base_url: String,
    pub profile: String,
    pub bearer_token: String,
    pub control_secret: String,
    pub fixture_urls: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TruthLocus {
    pub id: String,
    pub displayed_locus: String,
    pub contig: String,
    pub start_zero_based: u64,
    pub end_zero_based_exclusive: u64,
    pub expected_facts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TruthDocument {
    pub schema_version: u32,
    pub id: String,
    pub fixture_ids: Vec<String>,
    pub assembly: String,
    pub loci: Vec<TruthLocus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationSummary {
    pub artifacts: usize,
    pub dependencies: usize,
    pub projects: usize,
    pub coverage_entries: usize,
    pub incomplete_coverage_entries: Vec<String>,
    pub declared_bytes: u64,
    pub semantic_checks: usize,
}

#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .components()
        .collect()
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to hash {}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

pub fn relative_workspace_path(path: &Path, root: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "fixture path {} is outside workspace {}",
            path.display(),
            root.display()
        )
    })?;
    ensure_portable_relative_path(relative)?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

pub fn ensure_portable_relative_path(path: &Path) -> Result<()> {
    ensure!(!path.as_os_str().is_empty(), "fixture path is empty");
    ensure!(
        !path.is_absolute(),
        "fixture path {} is absolute",
        path.display()
    );
    for component in path.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "fixture path {} contains a non-portable component",
            path.display()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn artifact_for(
    root: &Path,
    path: &Path,
    id: impl Into<String>,
    media_type: impl Into<String>,
    validity: FixtureValidity,
    truth_ids: Vec<String>,
    project_ids: Vec<String>,
    scenario_ids: Vec<String>,
) -> Result<FixtureArtifact> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat fixture artifact {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "fixture artifact {} is not a file",
        path.display()
    );
    Ok(FixtureArtifact {
        id: id.into(),
        generator_schema_version: MANUAL_SUITE_SCHEMA_VERSION,
        path: relative_workspace_path(path, root)?,
        media_type: media_type.into(),
        bytes: metadata.len(),
        sha256: sha256_file(path)?,
        validity,
        source_fixture_ids: Vec::new(),
        assembly: None,
        contigs: BTreeMap::new(),
        truth_ids,
        project_ids,
        scenario_ids,
        privacy: "deterministic-synthetic-no-personal-data".to_owned(),
        redistributable: true,
    })
}

pub fn dependency_for(
    root: &Path,
    path: &Path,
    id: impl Into<String>,
    purpose: impl Into<String>,
) -> Result<FixtureDependency> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat fixture dependency {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "fixture dependency {} is not a file",
        path.display()
    );
    Ok(FixtureDependency {
        id: id.into(),
        path: relative_workspace_path(path, root)?,
        bytes: metadata.len(),
        sha256: sha256_file(path)?,
        purpose: purpose.into(),
    })
}

pub fn verify_manual_suite(
    root: &Path,
    manifest_path: &Path,
    catalog_path: &Path,
    require_complete: bool,
) -> Result<VerificationSummary> {
    let manifest: FixtureManifest = read_json(manifest_path)?;
    let catalog: CoverageCatalog = read_json(catalog_path)?;
    validate_manifest(root, &manifest)?;
    validate_catalog(root, &catalog, &manifest, require_complete)?;
    verify_declared_files(root, &manifest)?;
    verify_persisted_content(root, &manifest)?;
    verify_no_undeclared_files(root, manifest_path, catalog_path, &manifest)?;
    verify_projects(root, &manifest)?;
    verify_truth_documents(root, &manifest)?;
    let semantic_checks = verify_semantic_truth(root, &manifest)?;

    let incomplete_coverage_entries = catalog
        .entries
        .iter()
        .filter(|entry| !entry.disposition.is_complete())
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    Ok(VerificationSummary {
        artifacts: manifest.artifacts.len(),
        dependencies: manifest.dependencies.len(),
        projects: manifest.projects.len(),
        coverage_entries: catalog.entries.len(),
        incomplete_coverage_entries,
        declared_bytes: manifest
            .artifacts
            .iter()
            .map(|artifact| artifact.bytes)
            .sum(),
        semantic_checks,
    })
}

#[allow(clippy::too_many_lines)]
fn validate_manifest(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    ensure!(
        manifest.schema_version == MANUAL_SUITE_SCHEMA_VERSION,
        "manual fixture manifest schema {} is unsupported",
        manifest.schema_version
    );
    ensure!(
        !manifest.generator.trim().is_empty(),
        "manifest generator is empty"
    );
    ensure!(
        manifest.corpus_size_budget_bytes > 0,
        "manifest corpus size budget is zero"
    );

    unique_nonempty_ids(
        manifest
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str()),
        "artifact",
    )?;
    unique_nonempty_ids(
        manifest
            .dependencies
            .iter()
            .map(|dependency| dependency.id.as_str()),
        "dependency",
    )?;
    unique_nonempty_ids(
        manifest.projects.iter().map(|project| project.id.as_str()),
        "project",
    )?;
    let fixture_ids = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.id.as_str())
        .chain(
            manifest
                .dependencies
                .iter()
                .map(|dependency| dependency.id.as_str()),
        )
        .collect::<BTreeSet<_>>();

    let mut paths = BTreeSet::new();
    for artifact in &manifest.artifacts {
        ensure!(
            artifact.generator_schema_version == MANUAL_SUITE_SCHEMA_VERSION,
            "artifact {} has unsupported generator schema {}",
            artifact.id,
            artifact.generator_schema_version
        );
        ensure_portable_relative_path(Path::new(&artifact.path))?;
        ensure!(
            paths.insert(&artifact.path),
            "duplicate artifact path {}",
            artifact.path
        );
        ensure!(
            !artifact.media_type.trim().is_empty(),
            "artifact {} has no media type",
            artifact.id
        );
        ensure!(
            artifact.sha256.len() == 64,
            "artifact {} has an invalid SHA-256",
            artifact.id
        );
        for source in &artifact.source_fixture_ids {
            ensure!(
                fixture_ids.contains(source.as_str()) && source != &artifact.id,
                "artifact {} references unknown or self source fixture {}",
                artifact.id,
                source
            );
        }
        ensure!(
            artifact.contigs.values().all(|length| *length > 0),
            "artifact {} has an invalid contig length",
            artifact.id
        );
        ensure!(
            !artifact.privacy.trim().is_empty(),
            "artifact {} has no privacy declaration",
            artifact.id
        );
        ensure!(
            artifact.redistributable,
            "artifact {} is not declared redistributable",
            artifact.id
        );
    }
    for dependency in &manifest.dependencies {
        ensure_portable_relative_path(Path::new(&dependency.path))?;
        ensure!(
            paths.insert(&dependency.path),
            "fixture path {} is declared more than once",
            dependency.path
        );
        ensure!(
            dependency.sha256.len() == 64,
            "dependency {} has an invalid SHA-256",
            dependency.id
        );
    }
    let artifact_paths = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect::<BTreeSet<_>>();
    for project in &manifest.projects {
        ensure_portable_relative_path(Path::new(&project.path))?;
        ensure!(
            artifact_paths.contains(project.path.as_str()),
            "project {} path {} is not a declared artifact",
            project.id,
            project.path
        );
        ensure!(
            !project.title.trim().is_empty(),
            "project {} has no title",
            project.id
        );
        ensure!(
            !project.purpose.trim().is_empty(),
            "project {} has no purpose",
            project.id
        );
        ensure!(
            !project.initial_locus.trim().is_empty(),
            "project {} has no initial locus",
            project.id
        );
        ensure!(
            !project.scenario_id.trim().is_empty(),
            "project {} has no scenario",
            project.id
        );
    }
    let declared_bytes = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.bytes)
        .sum::<u64>();
    ensure!(
        declared_bytes <= manifest.corpus_size_budget_bytes,
        "manual suite declares {declared_bytes} bytes, exceeding its {} byte budget",
        manifest.corpus_size_budget_bytes
    );
    ensure!(
        root.is_dir(),
        "workspace root {} is not a directory",
        root.display()
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_catalog(
    root: &Path,
    catalog: &CoverageCatalog,
    manifest: &FixtureManifest,
    require_complete: bool,
) -> Result<()> {
    ensure!(
        catalog.schema_version == MANUAL_SUITE_SCHEMA_VERSION,
        "coverage catalog schema {} is unsupported",
        catalog.schema_version
    );
    unique_nonempty_ids(
        catalog.entries.iter().map(|entry| entry.id.as_str()),
        "coverage",
    )?;
    unique_nonempty_ids(
        catalog.sources.iter().map(|source| source.path.as_str()),
        "coverage source",
    )?;

    let source_paths = catalog
        .sources
        .iter()
        .map(|source| source.path.as_str())
        .collect::<BTreeSet<_>>();
    for source in &catalog.sources {
        ensure_portable_relative_path(Path::new(&source.path))?;
        let path = root.join(&source.path);
        ensure!(path.is_file(), "coverage source {} is missing", source.path);
        ensure!(
            sha256_file(&path)? == source.sha256,
            "coverage source {} changed; regenerate and review the catalog",
            source.path
        );
    }

    let fixture_ids = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.id.as_str())
        .chain(
            manifest
                .dependencies
                .iter()
                .map(|dependency| dependency.id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let project_ids = manifest
        .projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<BTreeSet<_>>();
    let truth_ids = manifest
        .artifacts
        .iter()
        .flat_map(|artifact| artifact.truth_ids.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let scenario_ids = manifest
        .artifacts
        .iter()
        .flat_map(|artifact| artifact.scenario_ids.iter().map(String::as_str))
        .chain(
            manifest
                .projects
                .iter()
                .map(|project| project.scenario_id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let batch_ids = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.media_type == "application/vnd.rgv.automation-batch+json")
        .map(|artifact| artifact.id.as_str())
        .chain(
            manifest
                .dependencies
                .iter()
                .filter(|dependency| dependency.id.contains("automation"))
                .map(|dependency| dependency.id.as_str()),
        )
        .collect::<BTreeSet<_>>();

    for entry in &catalog.entries {
        ensure!(
            !entry.capability.trim().is_empty(),
            "coverage {} has no capability",
            entry.id
        );
        ensure!(
            source_paths.contains(entry.source_path.as_str()),
            "coverage {} references undeclared source {}",
            entry.id,
            entry.source_path
        );
        ensure!(
            !entry.source_claim.trim().is_empty(),
            "coverage {} has no source claim",
            entry.id
        );
        ensure!(
            entry.preflight_status == "passed",
            "coverage {} preflight has not passed",
            entry.id
        );
        ensure!(
            entry.manual_qualification_status == "not-run",
            "coverage {} must distinguish fixture readiness from manual qualification",
            entry.id
        );
        check_references(&entry.fixture_ids, &fixture_ids, &entry.id, "fixture")?;
        check_references(&entry.project_ids, &project_ids, &entry.id, "project")?;
        check_references(&entry.truth_ids, &truth_ids, &entry.id, "truth")?;
        check_references(&entry.scenario_ids, &scenario_ids, &entry.id, "scenario")?;
        check_references(&entry.batch_ids, &batch_ids, &entry.id, "batch")?;
        match entry.disposition {
            CoverageDisposition::Planned => {
                ensure!(!require_complete, "coverage {} remains planned", entry.id);
            }
            CoverageDisposition::Ready => ensure!(
                !entry.fixture_ids.is_empty()
                    && !entry.project_ids.is_empty()
                    && !entry.scenario_ids.is_empty()
                    && !entry.truth_ids.is_empty(),
                "ready coverage {} lacks a fixture, project, scenario, or truth reference",
                entry.id
            ),
            CoverageDisposition::FailureReady => ensure!(
                !entry.fixture_ids.is_empty() && !entry.scenario_ids.is_empty(),
                "failure-ready coverage {} lacks a fixture or scenario",
                entry.id
            ),
            CoverageDisposition::OperationalReady => ensure!(
                !entry.scenario_ids.is_empty(),
                "operational coverage {} lacks a scenario",
                entry.id
            ),
            CoverageDisposition::RealDataOnly => ensure!(
                entry
                    .real_data_follow_up
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty()),
                "real-data-only coverage {} lacks a reason",
                entry.id
            ),
            CoverageDisposition::Deferred => ensure!(
                entry.source_path.ends_with("FORMAT_SUPPORT.md"),
                "deferred coverage {} is not tied to the documented format boundary",
                entry.id
            ),
        }
    }

    validate_source_claims(root, catalog)
}

fn validate_source_claims(root: &Path, catalog: &CoverageCatalog) -> Result<()> {
    let catalog_claims = catalog
        .entries
        .iter()
        .map(|entry| (entry.source_path.as_str(), entry.source_claim.as_str()))
        .collect::<BTreeSet<_>>();
    for source in &catalog.sources {
        let text = fs::read_to_string(root.join(&source.path))?;
        for claim in markdown_table_claims(&text) {
            ensure!(
                catalog_claims.contains(&(source.path.as_str(), claim.as_str())),
                "advertised claim {claim:?} from {} has no coverage entry",
                source.path
            );
        }
    }
    Ok(())
}

#[must_use]
pub fn markdown_table_claims(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with('|') || !line.ends_with('|') {
                return None;
            }
            let first = line
                .trim_matches('|')
                .split('|')
                .next()
                .map(str::trim)
                .unwrap_or_default();
            if first.is_empty()
                || matches!(first, "Capability" | "Workflow" | "Data family")
                || first
                    .chars()
                    .all(|character| matches!(character, '-' | ':' | ' '))
            {
                None
            } else {
                Some(strip_markdown_emphasis(first))
            }
        })
        .collect()
}

fn strip_markdown_emphasis(value: &str) -> String {
    value.replace(['`', '*'], "")
}

fn check_references(
    references: &[String],
    known: &BTreeSet<&str>,
    coverage_id: &str,
    kind: &str,
) -> Result<()> {
    for reference in references {
        ensure!(
            known.contains(reference.as_str()),
            "coverage {coverage_id} references unknown {kind} {reference}"
        );
    }
    Ok(())
}

fn unique_nonempty_ids<'a>(ids: impl IntoIterator<Item = &'a str>, kind: &str) -> Result<()> {
    let mut unique = BTreeSet::new();
    for id in ids {
        ensure!(!id.trim().is_empty(), "{kind} ID is empty");
        ensure!(unique.insert(id), "duplicate {kind} ID {id}");
    }
    Ok(())
}

fn verify_declared_files(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    for (id, relative, bytes, sha256) in manifest
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.id.as_str(),
                artifact.path.as_str(),
                artifact.bytes,
                artifact.sha256.as_str(),
            )
        })
        .chain(manifest.dependencies.iter().map(|dependency| {
            (
                dependency.id.as_str(),
                dependency.path.as_str(),
                dependency.bytes,
                dependency.sha256.as_str(),
            )
        }))
    {
        let path = root.join(relative);
        let metadata = fs::metadata(&path)
            .with_context(|| format!("declared fixture {id} is missing at {}", path.display()))?;
        ensure!(metadata.is_file(), "declared fixture {id} is not a file");
        ensure!(
            metadata.len() == bytes,
            "declared fixture {id} byte length changed"
        );
        ensure!(
            sha256_file(&path)? == sha256,
            "declared fixture {id} SHA-256 changed"
        );
    }
    Ok(())
}

fn verify_persisted_content(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let workspace = root.display().to_string();
    for artifact in &manifest.artifacts {
        let path = root.join(&artifact.path);
        let bytes = fs::read(&path)?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        ensure!(
            !text.contains('\r') && !text.contains('\0'),
            "text fixture {} contains non-portable CR or NUL bytes",
            artifact.id
        );
        for forbidden in [
            workspace.as_str(),
            "\"bearer_token\"",
            "\"control_secret\"",
            "http://127.0.0.1:",
            "/Users/",
            "C:\\Users\\",
        ] {
            ensure!(
                !text.contains(forbidden),
                "persisted fixture {} contains forbidden developer path or runtime credential material",
                artifact.id
            );
        }
    }
    Ok(())
}

fn verify_no_undeclared_files(
    root: &Path,
    manifest_path: &Path,
    catalog_path: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
    let mut declared = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect::<BTreeSet<_>>();
    declared.insert(relative_workspace_path(manifest_path, root)?);
    declared.insert(relative_workspace_path(catalog_path, root)?);
    for directory in [
        manifest_path
            .parent()
            .context("manual fixture manifest has no parent directory")?,
        catalog_path
            .parent()
            .context("manual fixture catalog has no parent directory")?,
    ] {
        for path in recursive_files(directory)? {
            let relative = relative_workspace_path(&path, root)?;
            ensure!(
                declared.contains(&relative),
                "manual fixture output contains undeclared stale file {relative}"
            );
        }
    }
    Ok(())
}

fn recursive_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![directory.to_path_buf()];
    let mut files = Vec::new();
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)
            .with_context(|| format!("failed to scan {}", current.display()))?
        {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "manual fixture output may not contain symlink {}",
                path.display()
            );
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn verify_projects(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    for project in &manifest.projects {
        let path = root.join(&project.path);
        if path
            .extension()
            .is_some_and(|extension| extension == "rgvgenome")
        {
            GenomeConfiguration::read(&path)
                .with_context(|| format!("failed to reopen genome project {}", project.id))?;
        } else {
            let document = SessionDocument::read(&path)
                .with_context(|| format!("failed to reopen project {}", project.id))?;
            ensure!(
                !document.panels.is_empty(),
                "project {} reopened without a panel",
                project.id
            );
            let unresolved = document.unresolved_local_sources();
            let observed_unresolved = unresolved
                .iter()
                .filter_map(|source| source.path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<BTreeSet<_>>();
            let expected_unresolved = project
                .expected_unresolved_source_basenames
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            ensure!(
                observed_unresolved == expected_unresolved,
                "project {} unresolved sources differ (expected {:?}, observed {:?}): {}",
                project.id,
                expected_unresolved,
                observed_unresolved,
                unresolved
                    .iter()
                    .map(|source| source.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

fn verify_truth_documents(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let truth_artifacts = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.media_type == "application/vnd.rgv.truth+json")
        .collect::<Vec<_>>();
    let mut truth_ids = BTreeMap::new();
    for artifact in truth_artifacts {
        let truth: TruthDocument = read_json(&root.join(&artifact.path))?;
        ensure!(
            truth.schema_version == MANUAL_SUITE_SCHEMA_VERSION,
            "truth {} has unsupported schema {}",
            truth.id,
            truth.schema_version
        );
        ensure!(
            !truth.id.trim().is_empty(),
            "truth document has an empty ID"
        );
        ensure!(
            !truth.fixture_ids.is_empty(),
            "truth {} has no fixture IDs",
            truth.id
        );
        ensure!(!truth.loci.is_empty(), "truth {} has no loci", truth.id);
        for locus in &truth.loci {
            ensure!(
                locus.start_zero_based < locus.end_zero_based_exclusive,
                "truth locus {} is empty",
                locus.id
            );
            ensure!(
                !locus.expected_facts.is_empty(),
                "truth locus {} has no expected facts",
                locus.id
            );
        }
        ensure!(
            truth_ids
                .insert(truth.id.clone(), artifact.id.as_str())
                .is_none(),
            "duplicate truth document ID {}",
            truth.id
        );
    }
    let referenced_truth = manifest
        .artifacts
        .iter()
        .flat_map(|artifact| artifact.truth_ids.iter())
        .collect::<BTreeSet<_>>();
    for truth_id in referenced_truth {
        ensure!(
            truth_ids.contains_key(truth_id),
            "referenced truth {truth_id} has no truth document"
        );
    }
    Ok(())
}

fn verify_semantic_truth(root: &Path, manifest: &FixtureManifest) -> Result<usize> {
    verify_mixed_alignment_truth(root, manifest)?;
    verify_focused_rna_truth(root, manifest)?;
    verify_event_and_ultima_truth(root, manifest)?;
    verify_specialist_alignment_truth(root, manifest)?;
    verify_reference_dependent_cram(root, manifest)?;
    verify_variant_truth(root, manifest)?;
    verify_genome_import_and_session_truth(root, manifest)?;
    verify_workspace_truth(root, manifest)?;
    verify_relocation_truth(root, manifest)?;
    verify_automation_truth(root, manifest)?;
    verify_preconfigured_specialist_projects(root, manifest)?;
    verify_mixed_track_truth(root, manifest)?;
    Ok(34)
}

fn fixture_path(root: &Path, manifest: &FixtureManifest, id: &str) -> Result<PathBuf> {
    let relative = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.id == id)
        .map(|artifact| artifact.path.as_str())
        .or_else(|| {
            manifest
                .dependencies
                .iter()
                .find(|dependency| dependency.id == id)
                .map(|dependency| dependency.path.as_str())
        })
        .with_context(|| format!("semantic verifier references unknown fixture {id}"))?;
    Ok(root.join(relative))
}

fn project_document(root: &Path, manifest: &FixtureManifest, id: &str) -> Result<SessionDocument> {
    let project = manifest
        .projects
        .iter()
        .find(|project| project.id == id)
        .with_context(|| format!("semantic verifier references unknown project {id}"))?;
    SessionDocument::read(root.join(&project.path))
        .with_context(|| format!("failed to reopen semantic project {id}"))
}

fn query_alignment(
    path: &Path,
    reference: Option<&Path>,
    contig: &str,
    start: u64,
    end: u64,
) -> Result<AlignmentQuery> {
    let mut source = LocalAlignmentSource::open(
        path,
        &AlignmentOpenOptions {
            reference_path: reference.map(Path::to_path_buf),
            ..AlignmentOpenOptions::default()
        },
    )
    .with_context(|| {
        format!(
            "failed to open alignment semantic fixture {}",
            path.display()
        )
    })?;
    let contig_id = source
        .reference_dictionary()
        .id_by_name(contig)
        .with_context(|| format!("alignment semantic fixture has no contig {contig}"))?;
    let mut query = source
        .query(
            GenomicInterval::new(contig_id, start, end)?,
            QueryOptions {
                detail: QueryDetail::Full,
                collect_modifications: true,
                ..QueryOptions::default()
            },
            &|| true,
        )
        .with_context(|| {
            format!(
                "failed to query alignment semantic fixture {} at {contig}:{start}-{end}",
                path.display()
            )
        })?;
    if let Some(reference_path) = reference {
        let reference = FastaReference::open(reference_path)?;
        query.reference = reference.fetch_slice_by_name(contig, query.interval)?;
    }
    Ok(query)
}

fn query_read_names(query: &AlignmentQuery) -> Result<BTreeSet<String>> {
    (0..query.batch.len())
        .map(|index| {
            let record = query.materialize_record(index)?;
            Ok(String::from_utf8_lossy(&record.read_name).into_owned())
        })
        .collect()
}

fn verify_mixed_alignment_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let path = fixture_path(root, manifest, "dep-mixed-bam")?;
    let paired = query_alignment(&path, None, "chr1", 0, 200_000)?;
    ensure!(
        paired.pairs.eligible_proper_templates > 0
            && paired.batch.flags.iter().any(|flags| flags.is_paired()),
        "MF-01 has no proper paired templates"
    );
    let deep = query_alignment(&path, None, "chr1", 900_000, 920_000)?;
    ensure!(
        deep.stats.records_seen >= 150_000,
        "MF-01 deep locus saw {} records instead of at least 150,000",
        deep.stats.records_seen
    );
    let rna = query_alignment(&path, None, "chr1", 2_100_000, 2_301_000)?;
    ensure!(
        rna.junctions.junctions.len() == 10_000
            && rna.junctions.provenance.spliced_records == 10_000,
        "MF-03 does not retain the exact 10,000-junction truth"
    );
    let long = query_alignment(&path, None, "chr2", 0, 2_000_000)?;
    ensure!(
        long.batch
            .flags
            .iter()
            .any(|flags| flags.is_supplementary())
            && (0..long.batch.len()).any(|index| long.batch.has_auxiliary_tag(index, *b"MM")),
        "MF-01 long-read locus lacks supplementary or modified-base evidence"
    );
    ensure!(
        (0..long.batch.len()).any(|index| {
            long.materialize_record(index).is_ok_and(|record| {
                smrt_kinetics_series(&record, SmrtKineticsMode::SubreadIpd)
                    .is_ok_and(|series| !series.is_empty())
            })
        }),
        "MF-05 has no decodable SMRT IPD series"
    );
    ensure!(
        (0..long.batch.len()).any(|index| {
            long.materialize_record(index).is_ok_and(|record| {
                summarize_sbx_record(&record, 10, 30).is_ok_and(|summary| {
                    summary
                        .concordant_insertions
                        .saturating_add(summary.partly_discordant_insertions)
                        .saturating_add(summary.fully_discordant_insertions)
                        > 0
                })
            })
        }),
        "MF-06 has no decodable SBX insertion evidence"
    );
    let pathological = query_alignment(&path, None, "chr2", 2_500_000, 2_550_000)?;
    ensure!(
        (0..pathological.batch.len()).any(|index| pathological
            .batch
            .cigar(index)
            .is_some_and(|cigar| cigar.len() >= 70_000)),
        "MF-01 lacks the 70,000-operation CIGAR"
    );
    Ok(())
}

fn verify_focused_rna_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let reference = fixture_path(root, manifest, "manual-reference-fasta")?;
    let sample_a = query_alignment(
        &fixture_path(root, manifest, "manual-rna-a-bam")?,
        Some(&reference),
        "chrManual",
        19_900,
        21_100,
    )?;
    let sample_b = query_alignment(
        &fixture_path(root, manifest, "manual-rna-b-bam")?,
        Some(&reference),
        "chrManual",
        19_900,
        21_100,
    )?;
    let expected_a = BTreeMap::from([
        ((20_040, 20_140), 5),
        ((20_250, 20_370), 3),
        ((20_530, 20_610), 1),
        ((20_640, 20_730), 1),
    ]);
    let expected_b = BTreeMap::from([
        ((20_040, 20_140), 4),
        ((20_050, 20_140), 1),
        ((20_040, 20_150), 1),
        ((20_435, 20_635), 2),
        ((20_530, 20_610), 1),
        ((20_640, 20_730), 1),
    ]);
    for (label, query, expected, tagged, fallback, invalid) in [
        ("A", &sample_a, expected_a, 5, 3, 2),
        ("B", &sample_b, expected_b, 6, 3, 1),
    ] {
        let observed = query
            .junctions
            .junctions
            .iter()
            .map(|junction| ((junction.start, junction.end), junction.depth))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            observed == expected,
            "MF-03 sample {label} junction truth changed: {observed:?}"
        );
        ensure!(
            query.junctions.provenance.spliced_records == 9
                && query.junctions.provenance.junction_observations_retained == 10
                && query.junctions.provenance.xs_tagged_observations == tagged
                && query
                    .junctions
                    .provenance
                    .alignment_strand_fallback_observations
                    == fallback
                && query.junctions.provenance.invalid_xs_observations == invalid,
            "MF-03 sample {label} strand or splice provenance changed: {:?}",
            query.junctions.provenance
        );
    }
    let project = project_document(root, manifest, "mf-03-rna-sashimi")?;
    ensure!(
        project.alignment_tracks.len() == 2
            && project.tracks.len() == 1
            && project.alignment_tracks[0].filters.show_junctions
            && project.alignment_tracks[1].filters.show_junctions
            && project.alignment_tracks[0].filters.sashimi_scale_mode == SashimiScaleMode::Fixed
            && project.alignment_tracks[1].filters.sashimi_scale_mode
                == SashimiScaleMode::Automatic,
        "MF-03 project lost its two RNA tracks, transcript context, or distinct Sashimi modes"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_event_and_ultima_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bam = fixture_path(root, manifest, "dep-variants-bam")?;
    let fasta = fixture_path(root, manifest, "dep-variants-fasta")?;
    FastaReference::open(&fasta)
        .with_context(|| format!("failed to open event reference {}", fasta.display()))?;
    let events = query_alignment(&bam, Some(&fasta), "chrVariants", 9_400, 12_600)?;
    let names = query_read_names(&events)?;
    for expected in [
        "long_insertion_1000bp",
        "long_deletion_2000bp",
        "small_insertion_1bp",
        "small_insertion_6bp",
        "small_deletion_1bp",
        "small_deletion_12bp",
        "snv_single",
    ] {
        ensure!(names.contains(expected), "MF-02 is missing {expected}");
    }
    ensure!(
        events.pileup.provenance.rendered_records > 0 && events.reference.is_some(),
        "MF-02 did not produce pileup and reference truth"
    );

    let ultima_bam = fixture_path(root, manifest, "manual-ultima-bam")?;
    let ultima_fasta = fixture_path(root, manifest, "manual-reference-fasta")?;
    let ultima = query_alignment(
        &ultima_bam,
        Some(&ultima_fasta),
        "chrManual",
        49_900,
        50_600,
    )?;
    ensure!(
        ultima.batch.len() == 7,
        "MF-07 has {} records instead of 7",
        ultima.batch.len()
    );
    let mut valid = BTreeMap::new();
    let mut invalid = BTreeSet::new();
    for index in 0..ultima.batch.len() {
        let record = ultima.materialize_record(index)?;
        let name = String::from_utf8_lossy(&record.read_name).into_owned();
        match analyze_ultima_record(
            &record,
            ultima.reference.as_ref(),
            UltimaQualityMode::Directional,
            ultima
                .read_group(index)
                .and_then(|group| group.maximum_homopolymer_length),
        ) {
            Ok(analysis) if !analysis.events.is_empty() => {
                let event = &analysis.events[0];
                valid.insert(name, (event.quality, event.quality_source));
            }
            Ok(_) => anyhow::bail!("MF-07 record {name} decoded without an event"),
            Err(_) => {
                invalid.insert(name);
            }
        }
    }
    ensure!(
        invalid
            == BTreeSet::from([
                "EXPECTED_ULTIMA_MISSING_TP".to_owned(),
                "EXPECTED_ULTIMA_WRONG_LENGTH_TP".to_owned(),
                "EXPECTED_ULTIMA_WRONG_TYPE_TP".to_owned(),
            ]),
        "MF-07 malformed record set changed: {invalid:?}"
    );
    for (name, quality, source) in [
        (
            "EXPECTED_ULTIMA_FORWARD_Q40",
            40.0,
            UltimaQualitySource::TpDirection,
        ),
        (
            "EXPECTED_ULTIMA_REVERSE_Q0",
            0.0,
            UltimaQualitySource::TpDirection,
        ),
        (
            "EXPECTED_ULTIMA_DELETION_MC_Q0",
            0.0,
            UltimaQualitySource::MaximumHomopolymerLimit,
        ),
        (
            "EXPECTED_ULTIMA_DELETION_T0_Q35",
            35.0,
            UltimaQualitySource::T0,
        ),
    ] {
        let observed = valid
            .get(name)
            .with_context(|| format!("MF-07 is missing valid record {name}"))?;
        ensure!(
            observed.0 == Some(quality) && observed.1 == Some(source),
            "MF-07 {name} decoded as {observed:?} instead of Q{quality} from {source:?}"
        );
    }
    ensure!(
        valid.len() == 4,
        "MF-07 decoded {} valid records instead of 4",
        valid.len()
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_specialist_alignment_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bam = fixture_path(root, manifest, "manual-specialist-bam")?;
    let fasta = fixture_path(root, manifest, "manual-reference-fasta")?;
    let bisulfite = query_alignment(&bam, Some(&fasta), "chrManual", 900, 1_200)?;
    let bisulfite_names = query_read_names(&bisulfite)?;
    ensure!(
        bisulfite_names.len() == 6,
        "MF-04 has {} records instead of 6",
        bisulfite_names.len()
    );
    for expected in [
        "EXPECTED_CG_PROTECTED_FORWARD",
        "EXPECTED_CG_CONVERTED_FORWARD",
        "EXPECTED_CG_PROTECTED_REVERSE",
        "EXPECTED_CG_CONVERTED_REVERSE",
        "EXPECTED_BISULFITE_LOW_QUALITY",
        "EXPECTED_BISULFITE_MISSING_EVIDENCE",
    ] {
        ensure!(
            bisulfite_names.contains(expected),
            "MF-04 is missing {expected}"
        );
    }
    let reference = bisulfite
        .reference
        .as_ref()
        .context("MF-04 query has no reference slice")?;
    for context in [
        BisulfiteContext::Cg,
        BisulfiteContext::Chg,
        BisulfiteContext::Chh,
        BisulfiteContext::Hcg,
        BisulfiteContext::Gch,
        BisulfiteContext::Wcg,
    ] {
        let mut protected = 0_u64;
        let mut converted = 0_u64;
        for index in 0..bisulfite.batch.len() {
            let record = bisulfite.materialize_record(index)?;
            let summary = summarize_bisulfite_record(&record, reference, context);
            protected = protected.saturating_add(summary.protected);
            converted = converted.saturating_add(summary.converted);
        }
        ensure!(
            protected > 0 && converted > 0,
            "MF-04 {context:?} lacks protected or converted observations ({protected}/{converted})"
        );
    }

    let smrt_path = fixture_path(root, manifest, "manual-smrt-bam")?;
    let smrt = query_alignment(&smrt_path, Some(&fasta), "chrManual", 29_900, 30_600)?;
    ensure!(
        smrt.batch.len() == 6,
        "MF-05 has {} records instead of 6",
        smrt.batch.len()
    );
    let modes = [
        SmrtKineticsMode::SubreadIpd,
        SmrtKineticsMode::SubreadPulseWidth,
        SmrtKineticsMode::CcsForwardIpd,
        SmrtKineticsMode::CcsForwardPulseWidth,
        SmrtKineticsMode::CcsReverseIpd,
        SmrtKineticsMode::CcsReversePulseWidth,
    ];
    let mut valid_records = 0_usize;
    let mut invalid_ip_records = BTreeSet::new();
    for index in 0..smrt.batch.len() {
        let record = smrt.materialize_record(index)?;
        let name = String::from_utf8_lossy(&record.read_name).into_owned();
        if name.starts_with("EXPECTED_SMRT_VALID_") {
            ensure!(
                modes.iter().all(|mode| smrt_kinetics_series(&record, *mode)
                    .is_ok_and(|series| series.len() == 100)),
                "MF-05 valid record {name} lost a 100-value kinetic mode"
            );
            valid_records += 1;
        } else if name == "EXPECTED_SMRT_MISSING_TAGS" {
            ensure!(
                modes
                    .iter()
                    .all(|mode| smrt_kinetics_series(&record, *mode).is_err()),
                "MF-05 missing-tag record unexpectedly decoded"
            );
        } else {
            ensure!(
                smrt_kinetics_series(&record, SmrtKineticsMode::SubreadIpd).is_err(),
                "MF-05 malformed ip record {name} unexpectedly decoded"
            );
            invalid_ip_records.insert(name);
        }
    }
    ensure!(
        valid_records == 2
            && invalid_ip_records
                == BTreeSet::from([
                    "EXPECTED_SMRT_NEGATIVE_IP".to_owned(),
                    "EXPECTED_SMRT_WRONG_LENGTH_IP".to_owned(),
                    "EXPECTED_SMRT_WRONG_TYPE_IP".to_owned(),
                ]),
        "MF-05 valid or malformed record set changed"
    );

    let sbx_path = fixture_path(root, manifest, "manual-sbx-bam")?;
    let sbx = query_alignment(&sbx_path, Some(&fasta), "chrManual", 39_900, 40_600)?;
    ensure!(
        sbx.batch.len() == 6,
        "MF-06 has {} records instead of 6",
        sbx.batch.len()
    );
    let mut observed_sbx = BTreeMap::new();
    for index in 0..sbx.batch.len() {
        let record = sbx.materialize_record(index)?;
        let name = String::from_utf8_lossy(&record.read_name).into_owned();
        if name == "EXPECTED_SBX_MISSING_QUALITIES" {
            ensure!(
                summarize_sbx_record(&record, 10, 30).is_err(),
                "MF-06 missing-quality record unexpectedly decoded"
            );
            observed_sbx.insert(name, "missing".to_owned());
            continue;
        }
        let summary = summarize_sbx_record(&record, 10, 30)?;
        let state = if summary.concordant_insertions == 1 {
            "concordant"
        } else if summary.partly_discordant_insertions == 1 {
            "partial"
        } else if summary.fully_discordant_insertions == 1 {
            "full"
        } else {
            "unexpected"
        };
        if name == "EXPECTED_SBX_ALL_TAIL" {
            ensure!(
                summary.retained_bases == 0 && summary.left_tail_bases == 150,
                "MF-06 all-tail bounds changed: {summary:?}"
            );
        } else {
            ensure!(
                summary.left_tail_bases == 20 && summary.right_tail_bases == 20,
                "MF-06 named tail bounds changed for {name}: {summary:?}"
            );
        }
        observed_sbx.insert(name, state.to_owned());
    }
    ensure!(
        observed_sbx
            == BTreeMap::from([
                ("EXPECTED_SBX_ALL_TAIL".to_owned(), "full".to_owned()),
                (
                    "EXPECTED_SBX_CONCORDANT".to_owned(),
                    "concordant".to_owned()
                ),
                ("EXPECTED_SBX_FULL".to_owned(), "full".to_owned()),
                (
                    "EXPECTED_SBX_MISSING_QUALITIES".to_owned(),
                    "missing".to_owned()
                ),
                ("EXPECTED_SBX_PARTIAL".to_owned(), "partial".to_owned()),
                ("EXPECTED_SBX_REVERSE".to_owned(), "concordant".to_owned()),
            ]),
        "MF-06 named insertion-state truth changed: {observed_sbx:?}"
    );

    let cells = query_alignment(&bam, Some(&fasta), "chrManual", 4_900, 5_400)?;
    ensure!(
        cells.batch.len() == 8,
        "MF-08 has {} records instead of 8",
        cells.batch.len()
    );
    let mut corrected_cells = BTreeSet::new();
    let mut missing = 0_usize;
    let mut malformed = 0_usize;
    for index in 0..cells.batch.len() {
        let record = cells.materialize_record(index)?;
        let tags = record
            .auxiliary_fields
            .iter()
            .map(|field| (field.tag, &field.value))
            .collect::<BTreeMap<_, _>>();
        match tags.get(b"CB") {
            Some(AuxValue::String(cell)) => {
                corrected_cells.insert(cell.clone());
                for tag in [*b"CR", *b"UB", *b"UR"] {
                    ensure!(
                        tags.contains_key(&tag),
                        "MF-08 record has CB but lacks a companion barcode/UMI tag"
                    );
                }
            }
            None => missing += 1,
            Some(_) => malformed += 1,
        }
    }
    ensure!(
        corrected_cells
            == BTreeSet::from([
                "CELL_A-1".to_owned(),
                "CELL_B-1".to_owned(),
                "CELL_C-1".to_owned(),
            ]),
        "MF-08 corrected cell set changed"
    );
    ensure!(
        missing == 1,
        "MF-08 expected one missing-tag record, found {missing}"
    );
    ensure!(
        malformed == 1,
        "MF-08 expected one malformed-tag record, found {malformed}"
    );
    Ok(())
}

fn verify_reference_dependent_cram(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bam = fixture_path(root, manifest, "manual-specialist-bam")?;
    let cram = fixture_path(root, manifest, "manual-reference-cram")?;
    let reference = fixture_path(root, manifest, "manual-reference-fasta")?;
    let wrong_reference = fixture_path(root, manifest, "failure-wrong-reference-fasta")?;
    let bam_query = query_alignment(&bam, Some(&reference), "chrManual", 900, 5_400)?;
    let cram_query = query_alignment(&cram, Some(&reference), "chrManual", 900, 5_400)?;
    ensure!(
        query_read_names(&bam_query)? == query_read_names(&cram_query)?,
        "MF-14 CRAM is not semantically equivalent to its source BAM"
    );
    let wrong = query_alignment(&cram, Some(&wrong_reference), "chrManual", 900, 5_400);
    ensure!(
        wrong.is_err(),
        "MF-14 accepted the deliberately wrong CRAM reference"
    );
    Ok(())
}

fn verify_variant_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bcf = fixture_path(root, manifest, "manual-cohort-bcf")?;
    let vcf = fixture_path(root, manifest, "manual-cohort-vcf")?;
    let mut source = LocalVariantSource::open(&bcf)?;
    let query = source.query("chrVariants", 14_900, 18_100, 100)?;
    ensure!(
        query.header.samples.len() == 128
            && query.header.samples[..3] == ["child", "father", "mother"],
        "MF-09 sample count or pedigree-leading order changed"
    );
    ensure!(
        query.records.len() == 8,
        "MF-09 has {} variant records instead of 8",
        query.records.len()
    );
    let types = query
        .records
        .iter()
        .flat_map(|record| &record.variant_types)
        .collect::<Vec<_>>();
    for expected in [
        VariantType::Snv,
        VariantType::Mnv,
        VariantType::Insertion,
        VariantType::Deletion,
        VariantType::Inversion,
        VariantType::StructuralDeletion,
        VariantType::Breakend,
    ] {
        ensure!(
            types.contains(&&expected),
            "MF-09 lacks variant type {expected:?}"
        );
    }
    ensure!(
        query
            .records
            .iter()
            .any(|record| record.alternates.len() == 2),
        "MF-09 lacks its multi-allelic record"
    );
    ensure!(
        query
            .records
            .iter()
            .flat_map(|record| &record.genotypes)
            .any(|genotype| genotype.alleles.len() == 1)
            && query
                .records
                .iter()
                .flat_map(|record| &record.genotypes)
                .any(|genotype| genotype.alleles.len() == 3)
            && query.header.formats.iter().any(|field| field.id == "AD")
            && query.header.formats.iter().any(|field| field.id == "GQ")
            && query.header.formats.iter().any(|field| field.id == "PL"),
        "MF-09 lacks haploid/polyploid genotypes or AD/GQ/PL FORMAT truth"
    );
    let malformed = query
        .records
        .iter()
        .find(|record| record.id.as_deref() == Some("EXPECTED_BREAKEND"))
        .context("MF-09 lacks EXPECTED_BREAKEND")?;
    ensure!(
        malformed
            .genotypes
            .last()
            .is_some_and(|genotype| genotype.alleles.iter().any(|allele| {
                allele
                    .index
                    .is_some_and(|index| index as usize > malformed.alternates.len())
            })),
        "MF-09 lost its readable out-of-range malformed genotype boundary"
    );
    let mut vcf_source = LocalVariantSource::open(&vcf)?;
    let vcf_query = vcf_source.query("chrVariants", 14_900, 18_100, 100)?;
    ensure!(
        vcf_query.header == query.header && vcf_query.records == query.records,
        "MF-09 VCF and BCF semantic truth differ"
    );
    let project = project_document(root, manifest, "mf-09-cohort-variants")?;
    let pedigree_order = project
        .sample_metadata
        .as_ref()
        .context("MF-09 project has no sample metadata")?
        .pedigree_sample_order(&query.header.samples)
        .context("MF-09 metadata did not produce a pedigree order")?;
    ensure!(
        pedigree_order.starts_with(&[0, 1, 2]),
        "MF-09 pedigree order does not lead with child, father, and mother"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_genome_import_and_session_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let genome_path = fixture_path(root, manifest, "manual-genome-config")?;
    let genome = GenomeConfiguration::read(&genome_path)?;
    ensure!(
        genome
            .contig_aliases
            .get("variants")
            .is_some_and(|target| target == "chrVariants")
            && genome.default_annotations.len() == 1,
        "MF-11 portable genome aliases or annotations changed"
    );
    let genome_project = project_document(root, manifest, "mf-11-genome-review")?;
    ensure!(
        genome_project.reference_translation_frames == ReferenceTranslationFrame::ALL.to_vec()
            && genome_project.reference_reverse_complement
            && genome_project.regions_of_interest.len() == 2
            && genome_project.review_items.len() == 2,
        "MF-11 lost six-frame translation, reverse reference, ROI, or review state"
    );
    let reference_path = fixture_path(root, manifest, "dep-variants-fasta")?;
    let reference = FastaReference::open(&reference_path)?;
    let dictionary = reference.reference_dictionary();
    let contig = dictionary
        .id_by_name("chrVariants")
        .context("MF-11 reference lacks chrVariants")?;
    let translation_slice = reference.fetch_slice(
        GenomicInterval::new(contig, 8_500, 8_560).context("invalid MF-11 translation locus")?,
    )?;
    ensure!(
        translation_slice.bases == b"ACGTACGTACGTACGTACGTACGTACGTACGTACGTACGTACGTACGTACGTACGTACGT",
        "MF-11 translation locus reference sequence changed"
    );
    let annotation = fixture_path(root, manifest, "manual-default-annotations")?;
    let feature_source = TextFeatureSource::open(&annotation, FeatureFormat::Bed)?;
    let feature_search = feature_source.search("EXPECTED_SEARCH_FEATURE", 10);
    ensure!(
        feature_search.hits.len() == 1
            && feature_search.hits[0].record.start == 8_500
            && feature_search.hits[0].record.end == 9_200,
        "MF-11 exact feature search no longer resolves EXPECTED_SEARCH_FEATURE"
    );
    verify_workspace_list_import_contracts(root, manifest)?;
    let igv_path = fixture_path(root, manifest, "manual-igv-import")?;
    let xml = fs::read_to_string(&igv_path)?;
    let imported = SessionDocument::import_igv_with_report(&xml)?;
    ensure!(
        !imported.report.imported.is_empty(),
        "MF-13 IGV import produced no explicit imported entries"
    );
    ensure!(
        !imported.report.approximated.is_empty() || !imported.report.ignored.is_empty(),
        "MF-13 IGV import did not disclose any approximation or ignored setting"
    );
    let expected_import: serde_json::Value =
        read_json(&fixture_path(root, manifest, "manual-igv-expected")?)?;
    let observed_import = serde_json::json!({
        "schema_version": 1,
        "locus": imported.document.locus,
        "alignment_track_count": imported.document.alignment_tracks.len(),
        "track_count": imported.document.tracks.len(),
        "imported": imported.report.imported,
        "approximated": imported.report.approximated,
        "ignored": imported.report.ignored,
        "omitted_notice_count": imported.report.omitted_notice_count
    });
    ensure!(
        observed_import == expected_import,
        "MF-13 IGV import differs from its exact expected report"
    );
    for id in ["dep-session-v1", "dep-session-v2", "dep-session-v3"] {
        let path = fixture_path(root, manifest, id)?;
        SessionDocument::read(&path)
            .with_context(|| format!("MF-13 failed to read native compatibility fixture {id}"))?;
    }
    let future = fixture_path(root, manifest, "dep-session-future")?;
    ensure!(
        SessionDocument::read(&future).is_err(),
        "MF-13 accepted an unsupported future session"
    );
    let autosave = fixture_path(root, manifest, "manual-recovery-autosave")?;
    let recovered = SessionDocument::read(&autosave)?;
    ensure!(
        recovered.alignment_tracks.len() == 4 && recovered.unresolved_local_sources().is_empty(),
        "MF-13 recovery autosave is not a complete four-track workspace"
    );
    let recovery_state: serde_json::Value =
        read_json(&fixture_path(root, manifest, "manual-recovery-state")?)?;
    ensure!(
        recovery_state["clean_shutdown"] == false
            && recovery_state["autosave_candidates"]
                .as_array()
                .is_some_and(|candidates| candidates.len() == 1)
            && recovery_state["expected_choice"] == "recover-or-continue-without-recovery",
        "MF-13 controlled unclean recovery state changed"
    );
    let changed_state: serde_json::Value = read_json(&fixture_path(
        root,
        manifest,
        "manual-changed-source-state",
    )?)?;
    let changed_source = fixture_path(root, manifest, "failure-changed-source")?;
    ensure!(
        changed_state["saved_sha256"]
            .as_str()
            .is_some_and(|saved| saved != sha256_file(&changed_source).unwrap_or_default())
            && changed_state["expected_behavior"] == "report-changed-and-keep-healthy-sources",
        "MF-13 changed-source fingerprint no longer represents a controlled mismatch"
    );
    Ok(())
}

fn verify_workspace_list_import_contracts(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let regions = fs::read_to_string(fixture_path(root, manifest, "manual-roi-import")?)?;
    let region_rows = regions
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("track "))
        .collect::<Vec<_>>();
    ensure!(
        region_rows.len() == 4
            && region_rows[0].starts_with("chrVariants\t0\t1\tcontig-start")
            && region_rows[3].starts_with("chrVariants\t19999\t20000\tcontig-end")
            && region_rows.iter().all(|row| row.split('\t').count() >= 9),
        "MF-11 ROI import lost exact colours or contig-boundary rows"
    );

    let review = fs::read_to_string(fixture_path(root, manifest, "manual-review-import")?)?;
    let mut lines = review.lines();
    ensure!(
        lines.next() == Some("#RGV-review-queue-v1")
            && lines.next()
                == Some("contig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json"),
        "MF-11 review import does not use the desktop's versioned export header"
    );
    let rows = lines.collect::<Vec<_>>();
    ensure!(
        rows.len() == 4,
        "MF-11 review import must contain four rows"
    );
    for row in &rows {
        let fields = row.splitn(7, '\t').collect::<Vec<_>>();
        ensure!(
            fields.len() == 7
                && matches!(
                    fields[3],
                    "pending" | "in_progress" | "complete" | "skipped"
                )
                && serde_json::from_str::<String>(fields[4]).is_ok()
                && serde_json::from_str::<Option<String>>(fields[5]).is_ok()
                && serde_json::from_str::<String>(fields[6]).is_ok(),
            "MF-11 review import row is not accepted version-1 syntax: {row}"
        );
    }
    ensure!(
        rows[0].starts_with("chrVariants\t0\t1\t")
            && rows[3].starts_with("chrVariants\t19999\t20000\t"),
        "MF-11 review import lost boundary-coordinate records"
    );
    Ok(())
}

fn verify_workspace_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let four = project_document(root, manifest, "mf-12-four-track")?;
    let eight = project_document(root, manifest, "mf-12-eight-track")?;
    let failure = project_document(root, manifest, "mf-12-failure-isolation")?;
    ensure!(
        four.alignment_tracks.len() == 4
            && eight.alignment_tracks.len() == 8
            && four.panels.len() == 2
            && eight.panels.len() == 2,
        "MF-12 lost the exact four/eight-track or two-panel workspaces"
    );
    let identities = eight
        .alignment_tracks
        .iter()
        .map(|track| (track.id, track.name.as_str()))
        .collect::<BTreeSet<_>>();
    ensure!(
        identities.len() == 8
            && eight
                .alignment_tracks
                .iter()
                .all(|track| track.sample_key.is_some()),
        "MF-12 eight-track identities or sample metadata links are not distinct"
    );
    let missing_sources = failure
        .alignment_tracks
        .iter()
        .filter(|track| match &track.source {
            TrackSource::Local { path } => path.contains("does-not-exist"),
            TrackSource::Remote { .. } => false,
        })
        .count();
    ensure!(
        failure.alignment_tracks.len() == 4 && missing_sources == 1,
        "MF-12 failure-isolation project must contain exactly one missing source among four tracks"
    );
    Ok(())
}

struct TemporaryRelocation(PathBuf);

impl Drop for TemporaryRelocation {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn verify_relocation_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let source_project = fixture_path(root, manifest, "manual-relocation-project")?;
    let source_bundle = source_project
        .parent()
        .context("MF-12 relocation project has no bundle directory")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let destination = std::env::temp_dir().join(format!(
        "rgv-manual-relocation-{}-{nonce}",
        std::process::id()
    ));
    let guard = TemporaryRelocation(destination.clone());
    copy_directory(source_bundle, &guard.0)?;
    let moved_project = guard.0.join("relocatable.rgv.json");
    let document = SessionDocument::read(&moved_project)?;
    ensure!(
        document.unresolved_local_sources().is_empty(),
        "MF-12 relocated project has unresolved sources"
    );
    let bam = match &document.alignment_tracks[0].source {
        TrackSource::Local { path } => PathBuf::from(path),
        TrackSource::Remote { .. } => anyhow::bail!("MF-12 relocation BAM became remote"),
    };
    let reference = match document.reference.as_ref() {
        Some(TrackSource::Local { path }) => PathBuf::from(path),
        _ => anyhow::bail!("MF-12 relocation reference is missing or remote"),
    };
    let query = query_alignment(&bam, Some(&reference), "chrManual", 900, 5_400)?;
    ensure!(
        query.batch.len() == 14,
        "MF-12 relocated project returned {} records instead of 14",
        query.batch.len()
    );
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn verify_automation_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let batch_artifacts = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.media_type == "application/vnd.rgv.automation-batch+json")
        .collect::<Vec<_>>();
    ensure!(
        batch_artifacts.len() == 3,
        "MF-15 expected three generated automation batches"
    );
    let mut export_kinds = Vec::new();
    let mut snapshot_formats = Vec::new();
    let mut controlled_missing_source = false;
    for artifact in batch_artifacts {
        let batch = AutomationBatch::read(root.join(&artifact.path))?;
        for command in batch.commands {
            match command {
                AutomationCommand::Export { kind, .. } => {
                    export_kinds.push(kind);
                }
                AutomationCommand::Snapshot { format, .. } => {
                    snapshot_formats.push(format);
                }
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local { path },
                    ..
                } if path.contains("does-not-exist.bam") => controlled_missing_source = true,
                _ => {}
            }
        }
    }
    ensure!(
        [
            AutomationExportKind::Alignments,
            AutomationExportKind::Pileup,
            AutomationExportKind::SelectedEvidence,
            AutomationExportKind::Modifications,
            AutomationExportKind::Junctions,
            AutomationExportKind::Variants,
            AutomationExportKind::Quantitative,
            AutomationExportKind::Regions,
            AutomationExportKind::TrackNames,
        ]
        .iter()
        .all(|kind| export_kinds.contains(kind))
            && snapshot_formats.contains(&AutomationSnapshotFormat::Png)
            && snapshot_formats.contains(&AutomationSnapshotFormat::Svg)
            && controlled_missing_source,
        "MF-15 batches do not cover every export, both snapshot formats, and a controlled failure"
    );
    Ok(())
}

fn verify_preconfigured_specialist_projects(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bisulfite = project_document(root, manifest, "mf-04-bisulfite")?;
    let bisulfite_modes = bisulfite
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Bisulfite(context) => Some(context),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        bisulfite_modes.len() == 6
            && [
                BisulfiteContext::Cg,
                BisulfiteContext::Chg,
                BisulfiteContext::Chh,
                BisulfiteContext::Hcg,
                BisulfiteContext::Gch,
                BisulfiteContext::Wcg,
            ]
            .iter()
            .all(|mode| bisulfite_modes.contains(mode)),
        "MF-04 does not preconfigure all six bisulfite contexts"
    );

    let smrt = project_document(root, manifest, "mf-05-smrt-kinetics")?;
    let smrt_modes = smrt
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => Some(mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        smrt_modes.len() == 6
            && [
                SmrtKineticsMode::SubreadIpd,
                SmrtKineticsMode::SubreadPulseWidth,
                SmrtKineticsMode::CcsForwardIpd,
                SmrtKineticsMode::CcsForwardPulseWidth,
                SmrtKineticsMode::CcsReverseIpd,
                SmrtKineticsMode::CcsReversePulseWidth,
            ]
            .iter()
            .all(|mode| smrt_modes.contains(mode)),
        "MF-05 does not preconfigure all six SMRT kinetics modes"
    );

    let sbx = project_document(root, manifest, "mf-06-sbx")?;
    let sbx_tail_modes = sbx
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Sbx(options) => Some(options.simplex_tail_mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        sbx_tail_modes.len() == 3
            && [
                SbxSimplexTailMode::Off,
                SbxSimplexTailMode::Highlight,
                SbxSimplexTailMode::Hide,
            ]
            .iter()
            .all(|mode| sbx_tail_modes.contains(mode)),
        "MF-06 does not preconfigure all SBX tail modes"
    );

    let ultima = project_document(root, manifest, "mf-07-ultima")?;
    let ultima_modes = ultima
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Ultima(options) => Some(options.quality_mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        ultima_modes.len() == 2
            && [UltimaQualityMode::Directional, UltimaQualityMode::Minimum]
                .iter()
                .all(|mode| ultima_modes.contains(mode)),
        "MF-07 does not preconfigure both Ultima quality modes"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_mixed_track_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let document = project_document(root, manifest, "mf-10-mixed-tracks")?;
    ensure!(
        document.tracks.iter().any(|track| {
            track.quantitative.arithmetic == QuantitativeArithmetic::Subtract
                && track.quantitative.operand_track.is_some()
        }) && document
            .tracks
            .iter()
            .filter(|track| track.quantitative.overlay_group == Some(1))
            .count()
            >= 2,
        "MF-10 lost its arithmetic track or two-track overlay group"
    );
    let specs = document
        .tracks
        .iter()
        .map(|track| {
            let TrackSource::Local { path } = &track.source else {
                anyhow::bail!("MF-10 unexpectedly contains a remote source");
            };
            Ok(TrackSpec {
                id: track.id,
                name: track.name.clone(),
                path: PathBuf::from(path),
                kind: fixture_track_spec_kind(track.kind, track.format, Path::new(path))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let engine = TrackEngine::spawn(specs.clone(), 128);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ready = None;
    let mut errors = Vec::new();
    let mut tdf_channels = None;
    while Instant::now() < deadline && ready.is_none() {
        match engine.try_receive()? {
            Some(TrackEngineEvent::Ready { opened, failed }) => ready = Some((opened, failed)),
            Some(TrackEngineEvent::TdfChannelsDiscovered { track_names, .. }) => {
                tdf_channels = Some(track_names);
            }
            Some(TrackEngineEvent::Error { message, .. }) => errors.push(message),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let (opened, failed) = ready.context("MF-10 timed out while opening its production sources")?;
    ensure!(
        errors.is_empty(),
        "MF-10 source errors: {}",
        errors.join("; ")
    );
    ensure!(
        opened == specs.len() && failed == 0,
        "MF-10 opened {opened} sources with {failed} failures instead of {} healthy sources",
        specs.len()
    );
    ensure!(
        tdf_channels.as_deref().is_some_and(|names| {
            names
                == [
                    "archive sample one".to_owned(),
                    "archive sample two".to_owned(),
                ]
        }),
        "MF-10 did not discover both exact TDF channel names"
    );

    let clock = GenerationClock::default();
    let request = PanelGeneration::new(PanelId::new(1), clock.current());
    engine.try_query(request, clock, "chrVariants".to_owned(), 8_500, 18_100)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshots = loop {
        ensure!(
            Instant::now() < deadline,
            "MF-10 timed out querying its sources"
        );
        match engine.try_receive()? {
            Some(TrackEngineEvent::QueryCompleted { tracks, .. }) => break tracks,
            Some(TrackEngineEvent::Error { message, .. }) => {
                anyhow::bail!("MF-10 query failed: {message}");
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    ensure!(
        snapshots.len() == specs.len(),
        "MF-10 returned {} snapshots for {} sources",
        snapshots.len(),
        specs.len()
    );
    for snapshot in snapshots {
        verify_mixed_snapshot_exact(&snapshot)?;
        let has_data = match snapshot.data {
            TrackSnapshotData::Features(records) => !records.is_empty(),
            TrackSnapshotData::Variants(query) => !query.records.is_empty(),
            TrackSnapshotData::Quantitative(query) => !query.records.is_empty(),
            TrackSnapshotData::CopyNumber(query) => !query.segments.is_empty(),
            TrackSnapshotData::Mutations(query) => !query.records.is_empty(),
            TrackSnapshotData::Interactions(query) => !query.records.is_empty(),
            TrackSnapshotData::Gwas(query) => !query.records.is_empty(),
        };
        ensure!(
            has_data,
            "MF-10 source {} returned no expected evidence",
            snapshot.name
        );
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_mixed_snapshot_exact(snapshot: &rgv_engine::TrackSnapshot) -> Result<()> {
    let feature_truth = match snapshot.name.as_str() {
        "Features (gzip BED)" => Some(("review-region", 8_800, 9_600)),
        "Genes (gzip GTF)" => Some(("Demo gene 1", 9_000, 10_100)),
        "Transcripts" => Some(("DEMO2-T1", 10_500, 12_000)),
        "Narrow peaks" => Some(("peak-with-summit", 9_200, 9_500)),
        "Broad peaks" => Some(("broad-region", 10_800, 11_400)),
        "PSL target alignments" => Some(("query-demo", 9_800, 9_890)),
        "Generic BigBed" => Some(("indexed-feature", 8_700, 9_000)),
        "Indexed transcripts" => Some(("DEMO-BIG", 9_000, 10_200)),
        "Indexed peaks" => Some(("indexed-peak", 10_600, 11_100)),
        "Plain BED annotations" => Some(("EXPECTED_SEARCH_FEATURE", 8_500, 9_200)),
        "Plain GFF3 gene models" => Some(("Expected gene", 14_500, 17_500)),
        "Compressed GFF3 gene models" => Some(("Expected compressed gene", 14_500, 17_500)),
        "Plain GTF transcripts" => Some(("Expected GTF gene", 14_500, 17_500)),
        "refFlat transcripts" => Some(("EXPECTED_REF_FLAT", 14_500, 17_500)),
        _ => None,
    };
    if let Some((name, start, end)) = feature_truth {
        let TrackSnapshotData::Features(records) = &snapshot.data else {
            anyhow::bail!("MF-10 {} did not decode as features", snapshot.name);
        };
        ensure!(
            records.iter().any(|record| {
                record.name.as_deref() == Some(name) && record.start == start && record.end == end
            }),
            "MF-10 {} lost exact feature {name} at {start}-{end}",
            snapshot.name
        );
        return Ok(());
    }

    let quantitative_truth = match snapshot.name.as_str() {
        "Signal BigWig" | "Arithmetic signal difference" => Some((8_500, 9_300, 2.5)),
        "Archived TDF" => Some((9_000, 9_500, 1.0)),
        "Signal WIG" => Some((8_800, 8_900, 1.0)),
        "Fixed-step WIG" => Some((14_500, 14_600, 1.0)),
        _ => None,
    };
    if let Some((start, end, value)) = quantitative_truth {
        let TrackSnapshotData::Quantitative(query) = &snapshot.data else {
            anyhow::bail!("MF-10 {} did not decode as quantitative", snapshot.name);
        };
        ensure!(
            query.records.iter().any(|record| {
                record.start == start
                    && record.end == end
                    && (record.value - value).abs() <= f32::EPSILON
            }),
            "MF-10 {} lost exact signal {value} at {start}-{end}",
            snapshot.name
        );
        return Ok(());
    }

    match (&*snapshot.name, &snapshot.data) {
        ("Association study", TrackSnapshotData::Gwas(query)) => ensure!(
            query.records.iter().any(|record| {
                record.identifier == "rs-demo-1"
                    && record.position == 9_099
                    && (record.p_value - 1e-8).abs() <= 1e-16
            }),
            "MF-10 lost exact demo association"
        ),
        (
            "PLINK linear association"
            | "PLINK logistic association"
            | "PLINK association"
            | "PLINK quantitative association",
            TrackSnapshotData::Gwas(query),
        ) => ensure!(
            query.records.iter().any(|record| {
                record.identifier == "EXPECTED_ASSOC_1"
                    && record.position == 15_049
                    && (record.p_value - 1e-12).abs() <= 1e-20
            }),
            "MF-10 {} lost exact PLINK association",
            snapshot.name
        ),
        ("Variants and genotypes", TrackSnapshotData::Variants(query)) => ensure!(
            query.header.samples == ["tumour", "normal"]
                && query.records.iter().any(|record| {
                    record.id.as_deref() == Some("demo-snv")
                        && record.start == 9_949
                        && record.quality == Some(60.0)
                }),
            "MF-10 lost exact demo variant/genotype truth"
        ),
        ("BEDPE loops", TrackSnapshotData::Interactions(query)) => ensure!(
            query.records.iter().any(|record| {
                record.name.as_deref() == Some("loop-A")
                    && record.source.start == 9_000
                    && record.target.start == 11_200
            }),
            "MF-10 lost exact BEDPE loop"
        ),
        ("UCSC interactions", TrackSnapshotData::Interactions(query)) => ensure!(
            query.records.iter().any(|record| {
                record.name.as_deref() == Some("variant/gene/demo")
                    && record.value == Some(0.625)
                    && record.directional
            }),
            "MF-10 lost exact interact record"
        ),
        ("Copy number", TrackSnapshotData::CopyNumber(query)) => ensure!(
            query.samples == ["tumour", "normal"]
                && query.segments.iter().any(|segment| {
                    segment.sample_index == 0
                        && segment.start == 8_500
                        && segment.end == 10_000
                        && (segment.value - 0.85).abs() <= f32::EPSILON
                }),
            "MF-10 lost exact SEG record"
        ),
        ("CBS copy number", TrackSnapshotData::CopyNumber(query)) => ensure!(
            query.samples == ["child", "father"]
                && query.segments.iter().any(|segment| {
                    segment.sample_index == 0
                        && segment.start == 14_500
                        && segment.end == 16_000
                        && (segment.value - 0.75).abs() <= f32::EPSILON
                }),
            "MF-10 lost exact CBS record"
        ),
        ("Somatic mutations", TrackSnapshotData::Mutations(query)) => ensure!(
            query.samples == ["tumour"]
                && query.records.iter().any(|record| {
                    record.gene == "DEMO1"
                        && record.start == 9_950
                        && record.classification == "Missense_Mutation"
                }),
            "MF-10 lost exact MAF record"
        ),
        _ => anyhow::bail!(
            "MF-10 has no exact semantic oracle for track {} ({:?})",
            snapshot.name,
            snapshot.data
        ),
    }
    Ok(())
}

fn fixture_track_spec_kind(
    kind: TrackKind,
    format: Option<TrackFormat>,
    path: &Path,
) -> Result<TrackSpecKind> {
    if let Some(format) = format {
        return Ok(match format {
            TrackFormat::BigGenePred => TrackSpecKind::BigGenePred,
            TrackFormat::BigNarrowPeak => TrackSpecKind::BigNarrowPeak,
            TrackFormat::TdfChannel { index } => TrackSpecKind::TdfChannel { index },
        });
    }
    let filename = path
        .file_name()
        .context("manual track source has no filename")?
        .to_string_lossy()
        .to_ascii_lowercase();
    let uncompressed = filename
        .strip_suffix(".gz")
        .or_else(|| filename.strip_suffix(".bgz"))
        .unwrap_or(&filename);
    let extension = Path::new(uncompressed)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    Ok(match kind {
        TrackKind::Feature => match extension {
            "bignarrowpeak" => TrackSpecKind::BigNarrowPeak,
            "bb" | "bigbed" => TrackSpecKind::BigBed,
            "narrowpeak" => TrackSpecKind::NarrowPeak,
            "broadpeak" => TrackSpecKind::BroadPeak,
            "psl" => TrackSpecKind::Psl,
            _ => TrackSpecKind::Bed,
        },
        TrackKind::GeneModel => match extension {
            "biggenepred" | "bb" | "bigbed" => TrackSpecKind::BigGenePred,
            "gtf" => TrackSpecKind::Gtf,
            "genepred" | "gp" => TrackSpecKind::GenePred,
            "refflat" => TrackSpecKind::RefFlat,
            _ => TrackSpecKind::Gff3,
        },
        TrackKind::Variant => TrackSpecKind::VcfOrBcf,
        TrackKind::Mutation => TrackSpecKind::MutationMaf,
        TrackKind::Gwas => TrackSpecKind::Gwas,
        TrackKind::Quantitative => match extension {
            "bedgraph" | "bdg" => TrackSpecKind::BedGraph,
            "wig" | "wiggle" => TrackSpecKind::Wig,
            "tdf" => TrackSpecKind::Tdf,
            _ => TrackSpecKind::BigWig,
        },
        TrackKind::CopyNumber => TrackSpecKind::Seg,
        TrackKind::Interaction => {
            if extension == "interact" {
                TrackSpecKind::Interact
            } else {
                TrackSpecKind::BedPe
            }
        }
        TrackKind::Alignment | TrackKind::Reference => {
            anyhow::bail!("manual auxiliary project contains a non-auxiliary track kind")
        }
    })
}

pub fn summarize(summary: &VerificationSummary) -> String {
    format!(
        "verified {} artifacts, {} reused dependencies, {} projects, {} semantic checks, and {} coverage rows ({} generated bytes; {} planned rows)",
        summary.artifacts,
        summary.dependencies,
        summary.projects,
        summary.semantic_checks,
        summary.coverage_entries,
        summary.declared_bytes,
        summary.incomplete_coverage_entries.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_claims_extract_all_table_rows_without_headers() {
        let claims = markdown_table_claims(
            "| Capability | Result |\n| --- | --- |\n| Local **BAM** | yes |\n| `VCF`/BCF | yes |\n",
        );
        assert_eq!(
            claims,
            BTreeSet::from(["Local BAM".to_owned(), "VCF/BCF".to_owned()])
        );
    }

    #[test]
    fn portable_paths_reject_absolute_and_parent_components() {
        assert!(ensure_portable_relative_path(Path::new("validation/file.json")).is_ok());
        assert!(ensure_portable_relative_path(Path::new("../outside.json")).is_err());
        assert!(ensure_portable_relative_path(Path::new("/absolute.json")).is_err());
    }

    #[test]
    fn planned_is_the_only_incomplete_disposition() {
        assert!(!CoverageDisposition::Planned.is_complete());
        for disposition in [
            CoverageDisposition::Ready,
            CoverageDisposition::FailureReady,
            CoverageDisposition::OperationalReady,
            CoverageDisposition::RealDataOnly,
            CoverageDisposition::Deferred,
        ] {
            assert!(disposition.is_complete());
        }
    }
}
