use super::super::*;

#[test]
fn session_track_specs_restore_new_feature_formats_by_extension() {
    let source = |path: &str| TrackSource::Local {
        path: path.to_owned(),
    };
    assert_eq!(
        track_spec_kind(TrackKind::GeneModel, &source("genes.genePred.gz"), None),
        Some(TrackSpecKind::GenePred)
    );
    assert_eq!(
        track_spec_kind(TrackKind::GeneModel, &source("genes.refFlat"), None),
        Some(TrackSpecKind::RefFlat)
    );
    assert_eq!(
        track_spec_kind(TrackKind::Feature, &source("peaks.narrowPeak.gz"), None),
        Some(TrackSpecKind::NarrowPeak)
    );
    assert_eq!(
        track_spec_kind(TrackKind::Feature, &source("regions.broadPeak.gz"), None),
        Some(TrackSpecKind::BroadPeak)
    );
    assert_eq!(
        track_spec_kind(TrackKind::Feature, &source("alignments.psl"), None),
        Some(TrackSpecKind::Psl)
    );
    assert_eq!(
        track_spec_kind(TrackKind::Gwas, &source("study.logistic.gz"), None),
        Some(TrackSpecKind::Gwas)
    );
    assert_eq!(
        track_spec_kind(
            TrackKind::Feature,
            &source("ambiguous.bb"),
            Some(TrackFormat::BigNarrowPeak)
        ),
        Some(TrackSpecKind::BigNarrowPeak)
    );
    assert_eq!(
        track_spec_kind(TrackKind::GeneModel, &source("genes.bb"), None),
        Some(TrackSpecKind::BigGenePred)
    );
    assert_eq!(
        track_spec_kind(
            TrackKind::Quantitative,
            &TrackSource::Remote {
                url: "https://example.test/archive.tdf?signature=private#fragment".to_owned(),
            },
            None,
        ),
        Some(TrackSpecKind::Tdf)
    );
    assert_eq!(
        track_spec_kind(
            TrackKind::Variant,
            &TrackSource::Remote {
                url: "https://example.test/calls.vcf.gz?signature=private".to_owned(),
            },
            None,
        ),
        Some(TrackSpecKind::VcfOrBcf)
    );
}

#[test]
fn desktop_track_inference_covers_advertised_compressed_annotations() {
    for (path, expected_kind, expected_track_kind) in [
        ("features.bed.gz", TrackSpecKind::Bed, TrackKind::Feature),
        ("genes.gff3.gz", TrackSpecKind::Gff3, TrackKind::GeneModel),
        ("genes.gtf.gz", TrackSpecKind::Gtf, TrackKind::GeneModel),
        (
            "genes.genePred.gz",
            TrackSpecKind::GenePred,
            TrackKind::GeneModel,
        ),
        (
            "peaks.narrowPeak.gz",
            TrackSpecKind::NarrowPeak,
            TrackKind::Feature,
        ),
        (
            "regions.broadPeak.gz",
            TrackSpecKind::BroadPeak,
            TrackKind::Feature,
        ),
        ("alignments.psl.gz", TrackSpecKind::Psl, TrackKind::Feature),
        ("study.assoc.gz", TrackSpecKind::Gwas, TrackKind::Gwas),
        ("calls.vcf.gz", TrackSpecKind::VcfOrBcf, TrackKind::Variant),
        (
            "somatic.maf.gz",
            TrackSpecKind::MutationMaf,
            TrackKind::Mutation,
        ),
        (
            "somatic.maf.txt",
            TrackSpecKind::MutationMaf,
            TrackKind::Mutation,
        ),
    ] {
        assert_eq!(
            inferred_desktop_track_kind(Path::new(path)).unwrap(),
            (expected_kind, expected_track_kind),
            "{path}",
        );
    }
}

#[test]
fn desktop_track_inference_uses_the_path_of_signed_urls() {
    assert_eq!(
        inferred_desktop_track_kind(Path::new(
            "https://example.test/archive.tdf?token=private#ignored"
        ))
        .unwrap(),
        (TrackSpecKind::Tdf, TrackKind::Quantitative)
    );
    assert_eq!(
        inferred_desktop_track_kind(Path::new(
            "https://example.test/genes.bigGenePred?token=private"
        ))
        .unwrap(),
        (TrackSpecKind::BigGenePred, TrackKind::GeneModel)
    );
}

#[test]
fn desktop_track_inference_rejects_unimplemented_compressed_text_sources() {
    for path in ["signal.bedGraph.gz", "loops.bedpe.gz", "segments.seg.gz"] {
        assert!(
            inferred_desktop_track_kind(Path::new(path)).is_err(),
            "{path} must not be accepted until its source actually decodes gzip"
        );
    }
}

#[test]
fn remote_source_editor_validates_alignment_companions_without_losing_signed_urls() {
    let editor = RemoteSourceEditor {
        source_url: " https://example.test/reads.cram?signature=private ".to_owned(),
        index_url: "https://index.example.test/reads.custom.crai?signature=index".to_owned(),
        reference_url: "https://example.test/reference.fa?signature=reference".to_owned(),
        reference_index_url: "https://index.example.test/reference.custom.fai".to_owned(),
        ..RemoteSourceEditor::default()
    };
    let request = validate_remote_source_editor(&editor).unwrap();
    assert_eq!(request.kind, RemoteSourceKind::Alignment);
    assert_eq!(
        request.source_url,
        "https://example.test/reads.cram?signature=private"
    );
    assert_eq!(
        request.index_url.as_deref(),
        Some("https://index.example.test/reads.custom.crai?signature=index")
    );
    assert_eq!(
        request.reference_url.as_deref(),
        Some("https://example.test/reference.fa?signature=reference")
    );
}

#[test]
fn remote_source_editor_accepts_only_range_indexed_auxiliary_formats() {
    for path in [
        "calls.vcf.gz?signature=private",
        "calls.bcf",
        "signal.bw",
        "archive.tdf?signature=private",
        "features.bigBed",
        "genes.bigGenePred",
        "peaks.bigNarrowPeak",
    ] {
        let editor = RemoteSourceEditor {
            kind: RemoteSourceKind::IndexedTrack,
            source_url: format!("https://example.test/{path}"),
            ..RemoteSourceEditor::default()
        };
        assert!(validate_remote_source_editor(&editor).is_ok(), "{path}");
    }
    for path in ["features.bed.gz", "signal.bedGraph"] {
        let editor = RemoteSourceEditor {
            kind: RemoteSourceKind::IndexedTrack,
            source_url: format!("https://example.test/{path}"),
            ..RemoteSourceEditor::default()
        };
        assert!(validate_remote_source_editor(&editor).is_err(), "{path}");
    }
}

#[test]
fn remote_source_editor_requires_complete_reference_companions() {
    let editor = RemoteSourceEditor {
        source_url: "https://example.test/reads.cram".to_owned(),
        reference_index_url: "https://example.test/reference.custom.fai".to_owned(),
        ..RemoteSourceEditor::default()
    };
    assert!(validate_remote_source_editor(&editor).is_err());
}

#[test]
fn remote_source_editor_ignores_fields_hidden_after_changing_source_type() {
    let editor = RemoteSourceEditor {
        kind: RemoteSourceKind::IndexedTrack,
        source_url: "https://example.test/archive.tdf".to_owned(),
        index_url: "not a visible field".to_owned(),
        reference_url: "not a visible field".to_owned(),
        reference_index_url: "not a visible field".to_owned(),
        error: None,
    };
    let request = validate_remote_source_editor(&editor).unwrap();
    assert_eq!(request.index_url, None);
    assert_eq!(request.reference_url, None);
    assert_eq!(request.reference_index_url, None);
}

#[test]
fn gwas_inspector_preserves_exact_source_p_value_and_all_columns() {
    let inspector = GwasInspectorSnapshot {
        track_name: "association study".to_owned(),
        query: Arc::new(rgv_hts::GwasQuery {
            records: vec![rgv_hts::GwasRecord {
                contig: "chr6".to_owned(),
                position: 99,
                identifier: "rs-test".to_owned(),
                p_value: 1e-300,
                minus_log10_p: 300.0,
                fields: vec![
                    ("CHR".to_owned(), "chr6".to_owned()),
                    ("BP".to_owned(), "100".to_owned()),
                    ("SNP".to_owned(), "rs-test".to_owned()),
                    ("P".to_owned(), "1.0000e-300".to_owned()),
                    ("BETA".to_owned(), "-0.125".to_owned()),
                ],
            }],
            provenance: rgv_hts::GwasQueryProvenance {
                records_examined: 500_000,
                records_retained: 250_000,
                source_rows_ignored: 2,
                records_truncated: true,
            },
        }),
        record_index: 0,
        settings: GwasTrackSettings {
            maximum_score: 25.0,
            ..GwasTrackSettings::default()
        },
    };

    let fields = gwas_inspector_fields(&inspector);
    assert!(fields.contains(&("Exact source p-value".to_owned(), "1.0000e-300".to_owned())));
    assert!(fields.contains(&("Source column · BETA".to_owned(), "-0.125".to_owned())));
    assert!(fields.iter().any(|(label, value)| {
        label == "Query provenance"
            && value.contains("250000 retained / 500000 examined")
            && value.contains("cap applied")
    }));
}

#[test]
fn gwas_picking_uses_both_point_position_and_transformed_height() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let records = vec![
        rgv_hts::GwasRecord {
            contig: "chr1".to_owned(),
            position: 150,
            identifier: "low".to_owned(),
            p_value: 0.1,
            minus_log10_p: 1.0,
            fields: Vec::new(),
        },
        rgv_hts::GwasRecord {
            contig: "chr1".to_owned(),
            position: 150,
            identifier: "high".to_owned(),
            p_value: 1e-20,
            minus_log10_p: 20.0,
            fields: Vec::new(),
        },
    ];
    let settings = GwasTrackSettings::default();
    let x = viewport.pixel_at_genomic(150) as f32;
    let high_y = gwas_point_y(20.0, 100.0, 200.0, settings.maximum_score);
    assert_eq!(
        gwas_record_index_at_point(&records, viewport, x, high_y, 100.0, 200.0, &settings),
        Some(1)
    );
    assert_eq!(
        gwas_record_index_at_point(
            &records,
            viewport,
            x + 50.0,
            high_y,
            100.0,
            200.0,
            &settings
        ),
        None
    );
}

#[test]
fn typed_bigbed_cli_and_session_restore_preserve_the_exact_schema() {
    let options = LaunchOptions::try_parse_from([
        "rgv-desktop",
        "--big-gene-pred",
        "genes.bb",
        "--big-narrow-peak",
        "peaks.bb",
    ])
    .unwrap();
    let (specifications, tracks) = build_track_specs(&options);
    assert_eq!(specifications.len(), 2);
    assert_eq!(specifications[0].kind, TrackSpecKind::BigGenePred);
    assert_eq!(specifications[1].kind, TrackSpecKind::BigNarrowPeak);
    assert_eq!(tracks.tracks()[0].kind, TrackKind::GeneModel);
    assert_eq!(tracks.tracks()[0].format, Some(TrackFormat::BigGenePred));
    assert_eq!(tracks.tracks()[1].kind, TrackKind::Feature);
    assert_eq!(tracks.tracks()[1].format, Some(TrackFormat::BigNarrowPeak));

    let mut session = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    });
    for track in tracks.tracks() {
        session.tracks.push(track.clone());
    }
    let (restored, _) = session_track_specs(&session);
    assert_eq!(restored[0].kind, TrackSpecKind::BigGenePred);
    assert_eq!(restored[1].kind, TrackSpecKind::BigNarrowPeak);
}

#[test]
fn broad_peak_and_psl_cli_use_typed_feature_sources() {
    let options = LaunchOptions::try_parse_from([
        "rgv-desktop",
        "--broad-peak",
        "regions.broadPeak",
        "--psl",
        "alignments.psl",
    ])
    .unwrap();
    let (specifications, tracks) = build_track_specs(&options);
    assert_eq!(specifications.len(), 2);
    assert_eq!(specifications[0].kind, TrackSpecKind::BroadPeak);
    assert_eq!(specifications[1].kind, TrackSpecKind::Psl);
    assert!(
        tracks
            .tracks()
            .iter()
            .all(|track| track.kind == TrackKind::Feature)
    );
}

#[test]
fn gwas_cli_and_session_restore_use_the_first_class_track() {
    let options = LaunchOptions::try_parse_from([
        "rgv-desktop",
        "--gwas",
        "study.assoc.gz",
        "--gwas",
        "replication.gwas",
    ])
    .unwrap();
    let (specifications, tracks) = build_track_specs(&options);
    assert_eq!(specifications.len(), 2);
    assert!(
        specifications
            .iter()
            .all(|specification| specification.kind == TrackSpecKind::Gwas)
    );
    assert!(
        tracks
            .tracks()
            .iter()
            .all(|track| track.kind == TrackKind::Gwas)
    );

    let mut session = SessionDocument::new(SessionLocus {
        contig: "chr6".to_owned(),
        start: 0,
        end: 100,
    });
    session.tracks.push(tracks.tracks()[0].clone());
    let (restored, _) = session_track_specs(&session);
    assert_eq!(restored[0].kind, TrackSpecKind::Gwas);
}

#[test]
fn tdf_cli_and_session_restore_use_the_indexed_quantitative_source() {
    let options =
        LaunchOptions::try_parse_from(["rgv-desktop", "--tdf", "archived-signal.tdf"]).unwrap();
    let (specifications, tracks) = build_track_specs(&options);
    assert_eq!(specifications.len(), 1);
    assert_eq!(specifications[0].kind, TrackSpecKind::Tdf);
    assert_eq!(tracks.tracks()[0].kind, TrackKind::Quantitative);

    let mut session = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    });
    let mut first = tracks.tracks()[0].clone();
    first.name = "sample one".to_owned();
    first.format = Some(TrackFormat::TdfChannel { index: 0 });
    let mut second = first.clone();
    second.id = TrackId::new(2);
    second.name = "sample two".to_owned();
    second.format = Some(TrackFormat::TdfChannel { index: 1 });
    session.tracks.extend([first, second]);
    let (restored, _) = session_track_specs(&session);
    assert_eq!(restored[0].kind, TrackSpecKind::TdfChannel { index: 0 });
    assert_eq!(restored[1].kind, TrackSpecKind::TdfChannel { index: 1 });
    assert_eq!(
        tdf_channel_display_name("archive.tdf", "", 2),
        "archive.tdf · channel 3"
    );
}

#[test]
fn tdf_channel_expansion_is_source_ordered_and_preserves_settings() {
    let mut parent = TrackConfig::new(
        TrackId::new(5),
        "archive.tdf".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Local {
            path: "archive.tdf".to_owned(),
        },
    );
    parent.quantitative.display_mode = QuantitativeDisplayMode::Line;
    let names = vec![
        "first".to_owned(),
        "selected".to_owned(),
        "third".to_owned(),
    ];
    let expansion =
        build_tdf_channel_expansion(&parent, Path::new("archive.tdf"), &names, 1, 10).unwrap();
    assert_eq!(expansion.selected_name, "selected");
    assert_eq!(
        expansion
            .specifications
            .iter()
            .map(|specification| (specification.id, specification.kind))
            .collect::<Vec<_>>(),
        [
            (TrackId::new(10), TrackSpecKind::TdfChannel { index: 0 }),
            (TrackId::new(11), TrackSpecKind::TdfChannel { index: 2 })
        ]
    );
    assert!(expansion.children.iter().all(|track| {
        track.quantitative.display_mode == QuantitativeDisplayMode::Line
            && matches!(track.format, Some(TrackFormat::TdfChannel { .. }))
    }));

    let mut collection = TrackCollection::default();
    collection.push(parent);
    collection.insert_after(TrackId::new(5), expansion.children);
    assert!(collection.move_track(TrackId::new(5), 1));
    assert_eq!(
        collection
            .tracks()
            .iter()
            .map(|track| track.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "archive.tdf", "third"]
    );
}

#[test]
fn quantitative_inspector_exposes_tdf_channel_and_resolution() {
    let inspector = QuantitativeInspectorSnapshot {
        track_name: "archive".to_owned(),
        query: Arc::new(rgv_hts::QuantitativeQuery {
            records: vec![rgv_hts::QuantitativeRecord {
                start: 100,
                end: 110,
                value: 4.5,
            }],
            provenance: rgv_hts::QuantitativeQueryProvenance {
                records_examined: 1,
                records_retained: 1,
                ..rgv_hts::QuantitativeQueryProvenance::default()
            },
            source_metadata: Some(rgv_hts::QuantitativeSourceMetadata::Tdf(
                rgv_hts::TdfQueryMetadata {
                    version: 4,
                    genome_id: Some("hg38".to_owned()),
                    data_type: "OTHER".to_owned(),
                    selected_track_name: "sample one".to_owned(),
                    selected_track_index: 0,
                    track_count: 2,
                    dataset: "/chr1/z3/mean".to_owned(),
                    zoom: Some(3),
                    window_function: "mean".to_owned(),
                    compressed: true,
                },
            )),
        }),
        record_index: 0,
        selected_position: 105,
        operand: None,
        settings: QuantitativeTrackSettings::default(),
    };
    let fields = quantitative_inspector_fields(&inspector);
    assert!(fields.contains(&("TDF channel".to_owned(), "sample one".to_owned())));
    assert!(fields.iter().any(|(label, value)| {
        label == "TDF resolution" && value.contains("zoom 3") && value.contains("mean")
    }));
    assert!(fields.contains(&("TDF genome".to_owned(), "hg38".to_owned())));
}
