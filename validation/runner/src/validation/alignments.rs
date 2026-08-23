#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn verify_truth_documents(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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

pub(crate) fn verify_semantic_truth(root: &Path, manifest: &FixtureManifest) -> Result<usize> {
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

pub(crate) fn fixture_path(root: &Path, manifest: &FixtureManifest, id: &str) -> Result<PathBuf> {
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

pub(crate) fn project_document(
    root: &Path,
    manifest: &FixtureManifest,
    id: &str,
) -> Result<SessionDocument> {
    let project = manifest
        .projects
        .iter()
        .find(|project| project.id == id)
        .with_context(|| format!("semantic verifier references unknown project {id}"))?;
    SessionDocument::read(root.join(&project.path))
        .with_context(|| format!("failed to reopen semantic project {id}"))
}

pub(crate) fn query_alignment(
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

pub(crate) fn query_read_names(query: &AlignmentQuery) -> Result<BTreeSet<String>> {
    (0..query.batch.len())
        .map(|index| {
            let record = query.materialize_record(index)?;
            Ok(String::from_utf8_lossy(&record.read_name).into_owned())
        })
        .collect()
}

pub(crate) fn named_alignment_record(
    query: &AlignmentQuery,
    name: &[u8],
    fixture_locus: &str,
) -> Result<AlignmentRecord> {
    (0..query.batch.len())
        .find_map(|index| {
            let record = query.materialize_record(index).ok()?;
            (record.read_name == name).then_some(record)
        })
        .with_context(|| format!("{fixture_locus} lost {}", String::from_utf8_lossy(name)))
}

pub(crate) fn verify_mixed_sparse_controls(path: &Path) -> Result<()> {
    let query = query_alignment(path, None, "chr1", 2_650_000, 2_652_150)?;
    let duplicate = named_alignment_record(&query, b"Control-000000", "MF-01 controls")?;
    let qc_fail = named_alignment_record(&query, b"Control-000001", "MF-01 controls")?;
    let low_mapq = named_alignment_record(&query, b"Control-000002", "MF-01 controls")?;
    let missing_quality = named_alignment_record(&query, b"Control-000003", "MF-01 controls")?;
    let secondary = named_alignment_record(&query, b"Control-000004", "MF-01 controls")?;
    ensure!(
        duplicate.flags.bits() & 0x400 != 0
            && qc_fail.flags.bits() & 0x200 != 0
            && low_mapq.mapping_quality == Some(0)
            && matches!(
                missing_quality.sequence,
                SequenceData::Present {
                    qualities: None,
                    ..
                }
            )
            && secondary.flags.is_secondary()
            && duplicate
                .auxiliary_fields
                .iter()
                .any(|field| field.tag == *b"RG")
            && duplicate
                .auxiliary_fields
                .iter()
                .any(|field| field.tag == *b"NM"),
        "MF-01 duplicate/QC/MAPQ/quality/secondary/RG/NM controls changed"
    );
    Ok(())
}

pub(crate) fn verify_mixed_long_truth(path: &Path) -> Result<()> {
    let long = query_alignment(path, None, "chr2", 0, 2_000_000)?;
    ensure!(
        long.batch
            .flags
            .iter()
            .any(|flags| flags.is_supplementary())
            && (0..long.batch.len()).any(|index| long.batch.has_auxiliary_tag(index, *b"MM")),
        "MF-01 long-read locus lacks supplementary or modified-base evidence"
    );
    let missing = named_alignment_record(&long, b"Long-001490", "MF-01 long-read locus")?;
    ensure!(
        decode_modified_bases(&missing)?.is_empty(),
        "MF-01 missing-MM record unexpectedly decoded calls"
    );
    for name in [b"Long-001491", b"Long-001492", b"Long-001494"] {
        let record = named_alignment_record(&long, name, "MF-01 long-read locus")?;
        ensure!(
            decode_modified_bases(&record).is_err(),
            "MF-01 malformed modified-base record {} unexpectedly decoded",
            String::from_utf8_lossy(name)
        );
    }
    let multi = named_alignment_record(&long, b"Long-001493", "MF-01 long-read locus")?;
    let calls = decode_modified_bases(&multi)?;
    ensure!(
        calls.len() == 2
            && calls[0].strand == BaseModificationStrand::Forward
            && calls[0].code == ModificationCode::Letter(b'm')
            && calls[0].probability == Some(255)
            && calls[1].strand == BaseModificationStrand::Reverse
            && calls[1].code == ModificationCode::Letter(b'h')
            && calls[1].probability == Some(64),
        "MF-01 multi-code/strand/probability modified-base truth changed"
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
    Ok(())
}

pub(crate) fn verify_mixed_pair_truth(path: &Path) -> Result<()> {
    let structural = query_alignment(path, None, "chr1", 2_590_000, 2_650_000)?;
    let pair_truth = (0..structural.batch.len())
        .map(|index| {
            let record = structural.materialize_record(index)?;
            Ok(pair_evidence(
                &record,
                structural.interval,
                TemplateLengthBounds {
                    minimum: Some(100),
                    maximum: Some(1_000),
                },
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    for orientation in [
        PairOrientation::Inward,
        PairOrientation::Outward,
        PairOrientation::SameStrand,
    ] {
        ensure!(
            pair_truth
                .iter()
                .any(|evidence| evidence.orientation == orientation),
            "MF-01 structural locus lacks {orientation:?} pair evidence"
        );
    }
    for classification in [
        PairClassification::InterContig,
        PairClassification::DiscordantOrientation,
        PairClassification::DiscordantTemplateLengthShort,
        PairClassification::DiscordantTemplateLengthLong,
        PairClassification::MateUnmapped,
    ] {
        ensure!(
            pair_truth
                .iter()
                .any(|evidence| evidence.classification == classification),
            "MF-01 structural locus lacks {classification:?} pair evidence"
        );
    }
    Ok(())
}

pub(crate) fn verify_mixed_alignment_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let path = fixture_path(root, manifest, "dep-mixed-bam")?;
    let paired = query_alignment(&path, None, "chr1", 0, 200_000)?;
    ensure!(
        paired.pairs.eligible_proper_templates > 0
            && paired.batch.flags.iter().any(|flags| flags.is_paired()),
        "MF-01 has no proper paired templates"
    );
    let project = project_document(root, manifest, "mf-01-alignment-core")?;
    ensure!(
        project.alignment_tracks.len() == 8
            && project
                .alignment_tracks
                .iter()
                .filter(|track| track.visible)
                .count()
                == 1
            && project
                .alignment_tracks
                .iter()
                .any(|track| !track.filters.show_reads)
            && project
                .alignment_tracks
                .iter()
                .map(|track| track.filters.alignment_density.to_bits())
                .collect::<BTreeSet<_>>()
                .len()
                >= 3,
        "MF-01 lost its eight focused pair/group/sort/density track configurations"
    );
    let deep = query_alignment(&path, None, "chr1", 900_000, 920_000)?;
    ensure!(
        deep.stats.records_seen >= 150_000,
        "MF-01 deep locus saw {} records instead of at least 150,000",
        deep.stats.records_seen
    );
    verify_mixed_sparse_controls(&path)?;
    let rna = query_alignment(&path, None, "chr1", 2_100_000, 2_301_000)?;
    ensure!(
        rna.junctions.junctions.len() == 10_000
            && rna.junctions.provenance.spliced_records == 10_000,
        "MF-03 does not retain the exact 10,000-junction truth"
    );
    verify_mixed_long_truth(&path)?;
    let pathological = query_alignment(&path, None, "chr2", 2_500_000, 2_550_000)?;
    ensure!(
        (0..pathological.batch.len()).any(|index| pathological
            .batch
            .cigar(index)
            .is_some_and(|cigar| cigar.len() >= 70_000)),
        "MF-01 lacks the 70,000-operation CIGAR"
    );
    verify_mixed_pair_truth(&path)?;
    Ok(())
}

pub(crate) fn verify_focused_rna_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
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
pub(crate) fn verify_event_and_ultima_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bam = fixture_path(root, manifest, "dep-variants-bam")?;
    let fasta = fixture_path(root, manifest, "dep-variants-fasta")?;
    FastaReference::open(&fasta)
        .with_context(|| format!("failed to open event reference {}", fasta.display()))?;
    let events = query_alignment(&bam, Some(&fasta), "chrVariants", 9_400, 12_600)?;
    let names = query_read_names(&events)?;
    for expected in [
        "long_insertion_1000bp",
        "long_deletion_2000bp",
        "reference_control_a",
        "reference_control_b",
        "small_insertion_1bp",
        "small_insertion_2bp",
        "small_insertion_6bp",
        "small_insertion_10bp",
        "large_insertion_100bp",
        "small_deletion_1bp",
        "small_deletion_2bp",
        "small_deletion_10bp",
        "small_deletion_12bp",
        "large_deletion_100bp",
        "large_deletion_500bp",
        "large_deletion_1000bp",
        "snv_single",
        "snv_cluster_3",
        "mnv_3bp",
        "ambiguous_bases",
        "low_quality_snv_q5",
        "reverse_strand_snv",
        "compound_snv_ins_del",
        "mixed_m_soft_hard_clip",
        "skip_padding_adjacent_events",
    ] {
        ensure!(names.contains(expected), "MF-02 is missing {expected}");
    }
    ensure!(
        events.batch.len() == 25,
        "MF-02 has {} event records instead of 25",
        events.batch.len()
    );
    let mut cigar_lengths: Vec<(CigarOpKind, BTreeSet<u32>)> = Vec::new();
    let mut special_records = BTreeMap::new();
    for index in 0..events.batch.len() {
        let record = events.materialize_record(index)?;
        for operation in &record.cigar {
            if let Some((_, lengths)) = cigar_lengths
                .iter_mut()
                .find(|(kind, _)| *kind == operation.kind)
            {
                lengths.insert(operation.len);
            } else {
                cigar_lengths.push((operation.kind, BTreeSet::from([operation.len])));
            }
        }
        special_records.insert(
            String::from_utf8_lossy(&record.read_name).into_owned(),
            record,
        );
    }
    for (kind, expected) in [
        (CigarOpKind::Insertion, &[1, 2, 3, 6, 10, 100, 1_000][..]),
        (
            CigarOpKind::Deletion,
            &[1, 2, 10, 12, 100, 500, 1_000, 2_000][..],
        ),
        (CigarOpKind::Match, &[20, 38, 40, 49, 50][..]),
        (CigarOpKind::SequenceMismatch, &[1, 3][..]),
        (CigarOpKind::SoftClip, &[5][..]),
        (CigarOpKind::HardClip, &[5][..]),
        (CigarOpKind::ReferenceSkip, &[25][..]),
        (CigarOpKind::Padding, &[3][..]),
    ] {
        let observed = cigar_lengths
            .iter()
            .find_map(|(observed_kind, lengths)| (*observed_kind == kind).then_some(lengths))
            .with_context(|| format!("MF-02 lacks {kind:?}"))?;
        ensure!(
            expected.iter().all(|length| observed.contains(length)),
            "MF-02 {kind:?} lengths changed: {observed:?}"
        );
    }
    let ambiguous = special_records
        .get("ambiguous_bases")
        .context("MF-02 lacks ambiguous base control")?;
    ensure!(
        matches!(&ambiguous.sequence, SequenceData::Present { bases, .. } if bases[48..52] == [b'N'; 4]),
        "MF-02 ambiguous base control lost its four Ns"
    );
    let low_quality = special_records
        .get("low_quality_snv_q5")
        .context("MF-02 lacks low-quality SNV control")?;
    ensure!(
        matches!(&low_quality.sequence, SequenceData::Present { qualities: Some(qualities), .. } if qualities[49] == 5),
        "MF-02 low-quality SNV no longer has Q5 at its mismatch"
    );
    ensure!(
        special_records
            .get("reverse_strand_snv")
            .is_some_and(|record| record.flags.is_reverse()),
        "MF-02 reverse-strand control is no longer reverse"
    );
    let (_, pileup) = events
        .pileup
        .bin_at(events.interval, 10_289)
        .context("MF-02 lacks the exact pileup bin at chrVariants:10290")?;
    ensure!(
        pileup.depth == 7
            && pileup.forward_depth == 6
            && pileup.reverse_depth == 1
            && pileup.alleles.deletion == 2,
        "MF-02 chrVariants:10290 pileup changed: {pileup:?}"
    );
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
pub(crate) fn verify_specialist_alignment_truth(
    root: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
    let bam = fixture_path(root, manifest, "manual-specialist-bam")?;
    let fasta = fixture_path(root, manifest, "manual-reference-fasta")?;
    let bisulfite = query_alignment(&bam, Some(&fasta), "chrManual", 900, 1_200)?;
    let bisulfite_names = query_read_names(&bisulfite)?;
    ensure!(
        bisulfite.batch.len() == 8 && bisulfite_names.len() == 7,
        "MF-04 has {} records/{} names instead of 8/7",
        bisulfite.batch.len(),
        bisulfite_names.len(),
    );
    for expected in [
        "EXPECTED_CG_PROTECTED_FORWARD",
        "EXPECTED_CG_CONVERTED_FORWARD",
        "EXPECTED_CG_PROTECTED_REVERSE",
        "EXPECTED_CG_CONVERTED_REVERSE",
        "EXPECTED_BISULFITE_LOW_QUALITY",
        "EXPECTED_BISULFITE_MISSING_EVIDENCE",
        "EXPECTED_BISULFITE_PAIRED_ORIENTATION",
    ] {
        ensure!(
            bisulfite_names.contains(expected),
            "MF-04 is missing {expected}"
        );
    }
    let paired_bisulfite = (0..bisulfite.batch.len())
        .filter_map(|index| {
            let record = bisulfite.materialize_record(index).ok()?;
            (record.read_name == b"EXPECTED_BISULFITE_PAIRED_ORIENTATION").then(|| {
                pair_evidence(&record, bisulfite.interval, TemplateLengthBounds::default())
            })
        })
        .collect::<Vec<_>>();
    ensure!(
        paired_bisulfite.len() == 2
            && paired_bisulfite.iter().all(|evidence| {
                evidence.orientation == PairOrientation::Inward
                    && evidence.classification == PairClassification::Proper
            }),
        "MF-04 paired orientation control is not an inward proper pair"
    );
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
        sbx.batch.len() == 7,
        "MF-06 has {} records instead of 7",
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
        if name == "EXPECTED_SBX_QUALITY_BOUNDARY" {
            ensure!(
                matches!(&record.sequence, SequenceData::Present { qualities: Some(qualities), .. } if qualities[70..73] == [9, 10, 30]),
                "MF-06 quality-boundary record lost Q9/Q10/Q30 insertion bases"
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
                (
                    "EXPECTED_SBX_QUALITY_BOUNDARY".to_owned(),
                    "partial".to_owned()
                ),
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

pub(crate) fn verify_reference_dependent_cram(
    root: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
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
