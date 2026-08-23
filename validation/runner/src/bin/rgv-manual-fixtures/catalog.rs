#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn dependencies(root: &Path) -> Result<Vec<FixtureDependency>> {
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

pub(crate) fn generated_artifacts(
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

pub(crate) fn project_source_fixture_ids(project_id: &str) -> Vec<String> {
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

pub(crate) fn truth_ids_for_fixture(id: &str) -> Vec<String> {
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

pub(crate) fn projects_for_fixture(id: &str) -> Vec<String> {
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

pub(crate) fn scenarios_for_fixture(id: &str) -> Vec<String> {
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

pub(crate) fn project_manifest(root: &Path, suite_root: &Path) -> Result<Vec<FixtureProject>> {
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

pub(crate) fn coverage_catalog(root: &Path, manifest: &FixtureManifest) -> Result<CoverageCatalog> {
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

pub(crate) struct ClaimMapping {
    fixture_ids: Vec<String>,
    project_ids: Vec<String>,
    scenario_ids: Vec<String>,
    batch_ids: Vec<String>,
    truth_ids: Vec<String>,
    real_data_follow_up: Option<String>,
}

pub(crate) fn map_claim(claim: &str) -> Result<ClaimMapping> {
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
