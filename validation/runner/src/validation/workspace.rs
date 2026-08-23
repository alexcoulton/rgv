#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(crate) fn verify_genome_import_and_session_truth(
    root: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
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

pub(crate) fn verify_workspace_list_import_contracts(
    root: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
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

pub(crate) fn verify_workspace_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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

pub(crate) fn verify_relocation_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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
        query.batch.len() == 16,
        "MF-12 relocated project returned {} records instead of 16",
        query.batch.len()
    );
    Ok(())
}

pub(crate) fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
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
