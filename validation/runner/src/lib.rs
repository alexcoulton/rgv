use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use rgv_automation::{
    AutomationBatch, AutomationCommand, AutomationExportKind, AutomationSnapshotFormat,
};
use rgv_core::{
    AlignmentColorMode, BisulfiteContext, GenerationClock, GenomeConfiguration, GenomicInterval,
    PanelGeneration, PanelId, QuantitativeArithmetic, ReferenceTranslationFrame, SashimiScaleMode,
    SbxSimplexTailMode, SessionDocument, SmrtKineticsMode, TrackFormat, TrackKind, TrackSource,
    UltimaQualityMode,
};
use rgv_engine::{TrackEngine, TrackEngineEvent, TrackSnapshotData, TrackSpec, TrackSpecKind};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentQuery, AlignmentRecord, AlignmentSource, AuxValue,
    BaseModificationStrand, CigarOpKind, FastaReference, FeatureFormat, LocalAlignmentSource,
    LocalVariantSource, ModificationCode, PairClassification, PairOrientation, QueryDetail,
    QueryOptions, SequenceData, TemplateLengthBounds, TextFeatureSource, UltimaQualitySource,
    VariantType, analyze_ultima_record, decode_modified_bases, pair_evidence, smrt_kinetics_series,
    summarize_bisulfite_record, summarize_sbx_record,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANUAL_SUITE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_SUITE_ROOT: &str = "benchmark-data/manual-suite";
pub const DEFAULT_VALIDATION_ROOT: &str = "validation/manual-suite";

#[path = "validation/structural.rs"]
mod structural;
pub use structural::markdown_table_claims;
use structural::{
    validate_catalog, validate_manifest, verify_declared_files, verify_no_undeclared_files,
    verify_persisted_content, verify_projects,
};
#[path = "validation/alignments.rs"]
mod alignments;
pub(crate) use alignments::{fixture_path, project_document, query_alignment};
use alignments::{verify_semantic_truth, verify_truth_documents};
#[path = "validation/variants.rs"]
mod variants;
pub(crate) use variants::verify_variant_truth;
#[path = "validation/workspace.rs"]
mod workspace;
pub(crate) use workspace::{
    verify_genome_import_and_session_truth, verify_relocation_truth, verify_workspace_truth,
};
#[path = "validation/automation.rs"]
mod automation;
pub(crate) use automation::{verify_automation_truth, verify_preconfigured_specialist_projects};
#[path = "validation/tracks.rs"]
mod tracks;
pub(crate) use tracks::verify_mixed_track_truth;

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
