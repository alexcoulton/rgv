#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn generate(root: &Path, suite_root: &Path, validation_root: &Path) -> Result<()> {
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

pub(crate) fn local_source(path: impl Into<String>) -> TrackSource {
    TrackSource::Local { path: path.into() }
}

pub(crate) fn locus(contig: &str, start: u64, end: u64) -> SessionLocus {
    SessionLocus {
        contig: contig.to_owned(),
        start,
        end,
    }
}

pub(crate) fn alignment_track(
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

pub(crate) fn base_document(contig: &str, start: u64, end: u64, name: &str) -> SessionDocument {
    let mut document = SessionDocument::new(locus(contig, start, end));
    document.genome_name = Some(name.to_owned());
    document
}

pub(crate) fn write_projects(suite_root: &Path) -> Result<()> {
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
    for (offset, (name, color, group, sort, density, pairs, reads)) in [
        (
            "Squished strand grouping",
            AlignmentColorMode::Strand,
            AlignmentGroupMode::Strand,
            AlignmentSortMode::Strand,
            0.55,
            false,
            true,
        ),
        (
            "Packed MAPQ sort",
            AlignmentColorMode::MappingQuality,
            AlignmentGroupMode::None,
            AlignmentSortMode::MappingQuality,
            0.35,
            false,
            true,
        ),
        (
            "Expanded read-group view",
            AlignmentColorMode::ReadGroup,
            AlignmentGroupMode::ReadGroup,
            AlignmentSortMode::ReadGroup,
            1.25,
            false,
            true,
        ),
        (
            "RG tag colour/group/sort",
            AlignmentColorMode::Tag(*b"RG"),
            AlignmentGroupMode::Tag(*b"RG"),
            AlignmentSortMode::Tag(*b"RG"),
            1.0,
            false,
            true,
        ),
        (
            "Paired insert-size view",
            AlignmentColorMode::TemplateLength,
            AlignmentGroupMode::UnexpectedPair,
            AlignmentSortMode::TemplateLength,
            1.0,
            true,
            true,
        ),
        (
            "Haplotype colour/group/sort",
            AlignmentColorMode::Haplotype,
            AlignmentGroupMode::Haplotype,
            AlignmentSortMode::Haplotype,
            1.0,
            false,
            true,
        ),
        (
            "Coverage density only",
            AlignmentColorMode::None,
            AlignmentGroupMode::None,
            AlignmentSortMode::Start,
            0.35,
            false,
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut track = alignment_track(u64::try_from(offset)? + 2, name, "../../mixed.bam", color);
        track.visible = false;
        track.filters.alignment_group_mode = group;
        track.filters.alignment_sort_mode = sort;
        track.filters.alignment_density = density;
        track.filters.view_as_pairs = pairs;
        track.filters.show_reads = reads;
        core.alignment_tracks.push(track);
    }
    core.review_items = vec![
        review_item(1, "Paired short reads", "chr1", 0, 200_000),
        review_item(2, "Deep coverage", "chr1", 900_000, 920_000),
        review_item(3, "Structural pairs", "chr1", 2_590_000, 2_650_000),
        review_item(4, "Long/supplementary/modifications", "chr2", 0, 30_000),
        review_item(5, "70,000-operation CIGAR", "chr2", 2_500_000, 2_510_000),
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
        let mut track = alignment_track(
            u64::try_from(offset)? + 1,
            &format!("{context:?} converted/protected evidence"),
            "../data/alignments/specialist.bam",
            AlignmentColorMode::Bisulfite(context),
        );
        // IGV counts both overlapping ends in this comparison fixture. Keep
        // the parity project explicit without changing RGV's safer default.
        track.filters.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
        bisulfite.alignment_tracks.push(track);
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

pub(crate) fn review_item(id: u64, label: &str, contig: &str, start: u64, end: u64) -> ReviewItem {
    ReviewItem {
        id,
        label: label.to_owned(),
        locus: locus(contig, start, end),
        status: ReviewStatus::Pending,
        notes: format!("Expected manual fixture truth for {label}."),
        assignment: None,
    }
}

pub(crate) fn write_session(path: &Path, document: &SessionDocument) -> Result<()> {
    write_text(path, &document.to_json()?)
}

pub(crate) fn write_relocation_bundle(suite_root: &Path) -> Result<()> {
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

pub(crate) fn mixed_tracks_document(suite_root: &Path) -> Result<SessionDocument> {
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
