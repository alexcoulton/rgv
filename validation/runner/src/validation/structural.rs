#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(crate) fn validate_manifest(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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
pub(crate) fn validate_catalog(
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

pub(crate) fn validate_source_claims(root: &Path, catalog: &CoverageCatalog) -> Result<()> {
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

pub(crate) fn strip_markdown_emphasis(value: &str) -> String {
    value.replace(['`', '*'], "")
}

pub(crate) fn check_references(
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

pub(crate) fn unique_nonempty_ids<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    kind: &str,
) -> Result<()> {
    let mut unique = BTreeSet::new();
    for id in ids {
        ensure!(!id.trim().is_empty(), "{kind} ID is empty");
        ensure!(unique.insert(id), "duplicate {kind} ID {id}");
    }
    Ok(())
}

pub(crate) fn verify_declared_files(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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

pub(crate) fn verify_persisted_content(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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

pub(crate) fn verify_no_undeclared_files(
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

pub(crate) fn recursive_files(directory: &Path) -> Result<Vec<PathBuf>> {
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

pub(crate) fn verify_projects(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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
