use super::*;

fn add_test_alignment(document: &mut SessionDocument) -> &mut SessionAlignmentTrack {
    document.alignment_tracks.push(SessionAlignmentTrack::new(
        TrackId::new(1),
        "reads.bam".to_owned(),
        TrackSource::Local {
            path: "reads.bam".to_owned(),
        },
    ));
    document.alignment_tracks.last_mut().unwrap()
}

#[test]
fn json_round_trip_preserves_schema_and_sources() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let alignment = add_test_alignment(&mut document);
    alignment.source = TrackSource::Remote {
        url: "https://example.org/sample.bam".to_owned(),
    };
    alignment.remote_index_url = Some("https://indexes.example.org/sample.custom.bai".to_owned());
    alignment.sample_key = Some("tumour".to_owned());
    let filters = &mut alignment.filters;
    filters.alignment_color_mode = AlignmentColorMode::Tag(*b"YC");
    filters.alignment_group_mode = AlignmentGroupMode::Chimeric;
    filters.alignment_group_order = AlignmentOrderDirection::Reverse;
    filters.alignment_sort_mode = AlignmentSortMode::Base;
    filters.alignment_sort_order = AlignmentOrderDirection::Reverse;
    filters.view_as_pairs = true;
    filters.minimum_template_length = Some(100);
    filters.maximum_template_length = Some(900);
    filters.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
    filters.minimum_base_quality = 20;
    filters.minimum_allele_fraction = 0.12;
    filters.quality_weighted_alleles = false;
    filters.show_reference = false;
    filters.show_coverage = false;
    filters.show_junctions = false;
    filters.minimum_junction_depth = 3;
    filters.minimum_junction_flank = 12;
    filters.junction_display_mode = JunctionDisplayMode::Expanded;
    filters.sashimi_scale_mode = SashimiScaleMode::Shared;
    filters.sashimi_fixed_scale_maximum = 250;
    filters.sashimi_count_mode = SashimiCountMode::CountsPerMillion;
    filters.show_junction_labels = false;
    filters.show_reads = false;
    filters.coverage_scale_maximum = Some(500);
    filters.coverage_scale_transform = CoverageScaleTransform::SquareRoot;
    filters.show_coverage_center_line = false;
    filters.alignment_density = 0.55;
    filters.inspector_activation = InspectorActivation::Hover;
    filters.read_base_display_mode = ReadBaseDisplayMode::All;
    filters.shade_read_bases_by_quality = false;
    filters.minimum_indel_size = 5;
    filters.minimum_event_label_size = 12;
    filters.show_modified_bases = false;
    filters.show_modification_frequency = false;
    filters.minimum_modification_probability = 128;
    filters.modified_base_color_mode = ModifiedBaseColorMode::ModifiedVsUnmodified;
    filters.hidden_modification_codes = vec!["m".to_owned(), "CHEBI:76792".to_owned()];
    document
        .contig_aliases
        .insert("1".to_owned(), "chr1".to_owned());
    document.reference_reverse_complement = true;
    document.reference_translation_frames = vec![
        ReferenceTranslationFrame::Forward1,
        ReferenceTranslationFrame::Reverse2,
    ];
    document.reference_genetic_code = GeneticCode::VertebrateMitochondrial;
    document.reference = Some(TrackSource::Remote {
        url: "https://example.org/reference.fa".to_owned(),
    });
    document.remote_reference_index_url =
        Some("https://indexes.example.org/reference.custom.fai".to_owned());
    document.sample_metadata =
        Some(SampleMetadataTable::from_tsv("sample\tcondition\ntumour\tcase\n").unwrap());
    assert_eq!(
        SessionDocument::from_json(&document.to_json().unwrap()).unwrap(),
        document
    );
}

#[test]
fn invalid_track_sample_associations_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document).sample_key = Some(" \n".to_owned());
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentTrack)
    ));
}

#[test]
fn custom_remote_indexes_require_bounded_http_urls_and_remote_sources() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let alignment = add_test_alignment(&mut document);
    alignment.remote_index_url = Some("https://example.org/sample.bai".to_owned());
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRemoteIndexUrl)
    ));

    document.alignment_tracks[0].source = TrackSource::Remote {
        url: "https://example.org/sample.bam".to_owned(),
    };
    document.alignment_tracks[0].remote_index_url = Some("ftp://example.org/sample.bai".to_owned());
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRemoteIndexUrl)
    ));

    document.alignment_tracks[0].remote_index_url =
        Some("https://example.org/sample.bai".to_owned());
    document.remote_reference_index_url = Some("https://example.org/reference.fai".to_owned());
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRemoteIndexUrl)
    ));

    document.reference = Some(TrackSource::Remote {
        url: "https://example.org/reference.fa".to_owned(),
    });
    assert!(document.to_json().is_ok());

    document.remote_reference_index_url = Some(format!(
        "https://example.org/{}",
        "x".repeat(MAXIMUM_REMOTE_INDEX_URL_BYTES)
    ));
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRemoteIndexUrl)
    ));
}

#[test]
fn regions_and_review_queue_round_trip_and_reject_invalid_private_state() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr6".to_owned(),
        start: 100,
        end: 200,
    });
    document.regions_of_interest.push(RegionOfInterest {
        id: 1,
        name: "candidate interval".to_owned(),
        locus: SessionLocus {
            contig: "chr6".to_owned(),
            start: 110,
            end: 140,
        },
        color: [12, 34, 56, 48],
    });
    document.review_items.push(ReviewItem {
        id: 1,
        label: "candidate variant".to_owned(),
        locus: SessionLocus {
            contig: "chr6".to_owned(),
            start: 120,
            end: 121,
        },
        status: ReviewStatus::InProgress,
        notes: "Needs orthogonal review".to_owned(),
        assignment: Some("reviewer-a".to_owned()),
    });

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored.regions_of_interest, document.regions_of_interest);
    assert_eq!(restored.review_items, document.review_items);

    document.review_items[0].notes = "x".repeat(MAXIMUM_REVIEW_NOTE_BYTES + 1);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRegionsOrReview)
    ));
    document.review_items[0].notes.clear();
    document.regions_of_interest.push(RegionOfInterest {
        id: 1,
        name: "duplicate id".to_owned(),
        locus: SessionLocus {
            contig: "chr6".to_owned(),
            start: 150,
            end: 160,
        },
        color: [1, 2, 3, 4],
    });
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidRegionsOrReview)
    ));
}

#[test]
fn variant_track_review_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "cohort".to_owned(),
        TrackKind::Variant,
        TrackSource::Local {
            path: "cohort.bcf".to_owned(),
        },
    );
    track.variant = crate::VariantTrackSettings {
        minimum_quality: Some(30.0),
        pass_only: true,
        variant_types: vec![crate::VariantTypeGroup::Snv],
        genotype: crate::VariantGenotypeFilter::NonReference,
        sample_subset_enabled: true,
        sample_subset: vec!["proband".to_owned()],
        genotype_grouping: crate::VariantGenotypeGrouping::Genotype,
        color_mode: crate::VariantColorMode::Genotype,
        ..crate::VariantTrackSettings::default()
    };
    document.tracks.push(track);
    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored.tracks[0].variant, document.tracks[0].variant);

    document.tracks[0].variant.minimum_allele_frequency = Some(2.0);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn custom_bigbed_schema_round_trips_and_must_match_the_track_kind() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.tracks.push(
        TrackConfig::new(
            TrackId::new(1),
            "genes".to_owned(),
            TrackKind::GeneModel,
            TrackSource::Local {
                path: "genes.bb".to_owned(),
            },
        )
        .with_format(TrackFormat::BigGenePred),
    );
    document.tracks.push(
        TrackConfig::new(
            TrackId::new(2),
            "peaks".to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: "peaks.bb".to_owned(),
            },
        )
        .with_format(TrackFormat::BigNarrowPeak),
    );
    document.tracks[0].feature.display_mode = crate::FeatureDisplayMode::Squished;

    let json = document.to_json().unwrap();
    assert!(json.contains("\"format\": \"big_gene_pred\""));
    assert!(json.contains("\"format\": \"big_narrow_peak\""));
    let restored = SessionDocument::from_json(&json).unwrap();
    assert_eq!(restored.tracks[0].format, Some(TrackFormat::BigGenePred));
    assert_eq!(
        restored.tracks[0].feature.display_mode,
        crate::FeatureDisplayMode::Squished
    );
    assert_eq!(restored.tracks[1].format, Some(TrackFormat::BigNarrowPeak));

    document.tracks[1].kind = TrackKind::Quantitative;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn tdf_channel_identity_round_trips_and_requires_a_quantitative_track() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.tracks.push(
        TrackConfig::new(
            TrackId::new(3),
            "sample two".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: "archive.tdf".to_owned(),
            },
        )
        .with_format(TrackFormat::TdfChannel { index: 1 }),
    );
    let json = document.to_json().unwrap();
    let restored = SessionDocument::from_json(&json).unwrap();
    assert_eq!(
        restored.tracks[0].format,
        Some(TrackFormat::TdfChannel { index: 1 })
    );

    document.tracks[0].kind = TrackKind::Feature;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
    document.tracks[0].kind = TrackKind::Quantitative;
    document.tracks[0].format = Some(TrackFormat::TdfChannel {
        index: crate::MAXIMUM_TDF_CHANNELS,
    });
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn quantitative_track_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "signal".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Local {
            path: "signal.bw".to_owned(),
        },
    );
    track.quantitative = crate::QuantitativeTrackSettings {
        scale_mode: crate::QuantitativeScaleMode::Shared,
        fixed_minimum: -4.0,
        fixed_maximum: 8.0,
        baseline: 1.0,
        scale_transform: crate::QuantitativeScaleTransform::SignedLog10,
        display_mode: crate::QuantitativeDisplayMode::Heatmap,
        window_function: crate::QuantitativeWindowFunction::Maximum,
        missing_mode: crate::QuantitativeMissingMode::Marker,
        positive_color: [1, 2, 3, 4],
        negative_color: [5, 6, 7, 8],
        opacity: 0.5,
        arithmetic: crate::QuantitativeArithmetic::None,
        operand_track: None,
        show_range_labels: false,
        overlay_group: Some(7),
        scale_group: Some(7),
    };
    document.tracks.push(track);

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(
        restored.tracks[0].quantitative,
        document.tracks[0].quantitative
    );

    document.tracks.push(TrackConfig::new(
        TrackId::new(2),
        "control".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Local {
            path: "control.bw".to_owned(),
        },
    ));
    document.tracks[0].quantitative.arithmetic = crate::QuantitativeArithmetic::Subtract;
    document.tracks[0].quantitative.operand_track = Some(TrackId::new(2));
    assert!(document.to_json().is_ok());

    document.tracks[0].quantitative.operand_track = Some(TrackId::new(1));
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
    document.tracks[0].quantitative.arithmetic = crate::QuantitativeArithmetic::None;
    document.tracks[0].quantitative.operand_track = None;

    document.tracks[0].quantitative.fixed_minimum = 10.0;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
    document.tracks[0].quantitative.fixed_minimum = -10.0;
    document.tracks[0].quantitative.overlay_group = Some(0);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn interaction_track_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "loops".to_owned(),
        TrackKind::Interaction,
        TrackSource::Local {
            path: "loops.interact".to_owned(),
        },
    );
    track.interaction = crate::InteractionTrackSettings {
        display_mode: crate::InteractionDisplayMode::Blocks,
        minimum_score: Some(25.0),
        fixed_maximum_score: Some(1_000.0),
        show_labels: false,
    };
    document.tracks.push(track);
    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored, document);

    document.tracks[0].interaction.fixed_maximum_score = Some(0.0);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn gwas_track_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr6".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "association study".to_owned(),
        TrackKind::Gwas,
        TrackSource::Local {
            path: "study.assoc.gz".to_owned(),
        },
    );
    track.gwas = crate::GwasTrackSettings {
        color_mode: crate::GwasColorMode::Alternating,
        point_size: 6.0,
        maximum_score: 40.0,
        uniform_color: [1, 2, 3, 255],
        alternating_color_one: [4, 5, 6, 255],
        alternating_color_two: [7, 8, 9, 255],
    };
    document.tracks.push(track);
    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored, document);
    assert_eq!(infer_track_kind("study.assoc.gz"), TrackKind::Gwas);

    document.tracks[0].gwas.maximum_score = 0.0;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn copy_number_track_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr6".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "cohort".to_owned(),
        TrackKind::CopyNumber,
        TrackSource::Local {
            path: "cohort.seg".to_owned(),
        },
    );
    track.copy_number = crate::CopyNumberTrackSettings {
        value_mode: crate::CopyNumberValueMode::LogRatio,
        sort_mode: crate::CopyNumberSortMode::Amplification,
        sort_locus: Some(crate::CopyNumberSortLocus {
            contig: "chr6".to_owned(),
            start: 120,
            end: 180,
        }),
        row_height: 18.0,
        show_sample_names: false,
        show_values: true,
    };
    document.tracks.push(track);
    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored, document);

    document.tracks[0].copy_number.row_height = 2.0;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn mutation_track_state_round_trips_and_is_validated() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr17".to_owned(),
        start: 100,
        end: 200,
    });
    let mut track = TrackConfig::new(
        TrackId::new(1),
        "somatic mutations".to_owned(),
        TrackKind::Mutation,
        TrackSource::Local {
            path: "cohort.maf".to_owned(),
        },
    );
    track.mutation = crate::MutationTrackSettings {
        color_mode: crate::MutationColorMode::VariantType,
        row_height: 22.0,
        show_sample_names: false,
        show_gene_labels: false,
    };
    document.tracks.push(track);

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored, document);

    document.tracks[0].mutation.row_height = 2.0;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAuxiliaryTrack)
    ));
}

#[test]
fn version_three_round_trip_preserves_ordered_panels_and_active_locus() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr2".to_owned(),
        start: 20,
        end: 40,
    });
    document.panels = vec![
        SessionViewPanel::new(
            PanelId::new(8),
            SessionLocus {
                contig: "chr1".to_owned(),
                start: 100,
                end: 200,
            },
        ),
        SessionViewPanel::new(PanelId::new(3), document.locus.clone()),
    ];
    document.active_panel = PanelId::new(3);
    add_test_alignment(&mut document);

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();

    assert_eq!(restored, document);
    assert_eq!(
        restored
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>(),
        vec![PanelId::new(8), PanelId::new(3)]
    );
    assert_eq!(restored.active_panel, PanelId::new(3));
}

#[test]
fn version_two_migrates_to_one_active_version_three_panel() {
    let document = SessionDocument::from_json(include_str!(
        "../../../validation/four-track-session-v2.json"
    ))
    .unwrap();

    assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
    assert_eq!(document.panels.len(), 1);
    assert_eq!(document.panels[0].id, PanelId::new(1));
    assert_eq!(document.panels[0].locus, document.locus);
    assert_eq!(document.active_panel, PanelId::new(1));
}

#[test]
fn invalid_panel_collections_are_rejected() {
    let locus = SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 10,
    };
    let valid = SessionDocument::new(locus.clone());

    let mut empty = valid.clone();
    empty.panels.clear();
    assert!(matches!(empty.to_json(), Err(SessionError::InvalidPanels)));

    let mut zero_id = valid.clone();
    zero_id.panels[0].id = PanelId::new(0);
    zero_id.active_panel = PanelId::new(0);
    assert!(matches!(
        zero_id.to_json(),
        Err(SessionError::InvalidPanels)
    ));

    let mut duplicate = valid.clone();
    duplicate.panels.push(duplicate.panels[0].clone());
    assert!(matches!(
        duplicate.to_json(),
        Err(SessionError::InvalidPanels)
    ));

    let mut missing_active = valid.clone();
    missing_active.active_panel = PanelId::new(99);
    assert!(matches!(
        missing_active.to_json(),
        Err(SessionError::InvalidPanels)
    ));

    let mut mismatched_locus = valid.clone();
    mismatched_locus.locus = SessionLocus {
        contig: "chr2".to_owned(),
        start: 0,
        end: 10,
    };
    assert!(matches!(
        mismatched_locus.to_json(),
        Err(SessionError::InvalidPanels)
    ));

    let mut excessive = valid;
    excessive.panels = (1..=MAXIMUM_VIEW_PANELS + 1)
        .map(|value| {
            SessionViewPanel::new(PanelId::new(u64::try_from(value).unwrap()), locus.clone())
        })
        .collect();
    assert!(matches!(
        excessive.to_json(),
        Err(SessionError::InvalidPanels)
    ));
}

#[test]
fn early_version_one_sessions_receive_pileup_defaults() {
    let json = r#"{"schema_version":1,"locus":{"contig":"chr1","start":0,"end":1},"alignment":{"Local":{"path":"reads.bam"}},"reference":null,"tracks":[],"filters":{"minimum_mapping_quality":null,"include_secondary":true,"include_supplementary":true,"auxiliary_predicates":[]},"maximum_alignments":1}"#;
    let document = SessionDocument::from_json(json).unwrap();
    assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
    assert_eq!(document.alignment_tracks.len(), 1);
    assert_eq!(document.alignment_tracks[0].maximum_alignments, 1);
    let filters = &document.alignment_tracks[0].filters;
    assert_eq!(filters.alignment_color_mode, AlignmentColorMode::None);
    assert_eq!(filters.alignment_group_mode, AlignmentGroupMode::None);
    assert_eq!(
        filters.alignment_group_order,
        AlignmentOrderDirection::Natural
    );
    assert_eq!(filters.alignment_sort_mode, AlignmentSortMode::Start);
    assert_eq!(
        filters.alignment_sort_order,
        AlignmentOrderDirection::Natural
    );
    assert!(!filters.view_as_pairs);
    assert_eq!(filters.minimum_template_length, None);
    assert_eq!(filters.maximum_template_length, None);
    assert_eq!(
        filters.overlapping_mate_policy,
        OverlappingMatePolicy::CountOnce
    );
    assert_eq!(filters.minimum_base_quality, 0);
    assert!((filters.minimum_allele_fraction - 0.20).abs() < f32::EPSILON);
    assert!(filters.quality_weighted_alleles);
    assert!(filters.show_reference);
    assert!(filters.show_coverage);
    assert!(filters.show_junctions);
    assert_eq!(filters.minimum_junction_depth, 1);
    assert_eq!(filters.minimum_junction_flank, 0);
    assert_eq!(
        filters.junction_display_mode,
        JunctionDisplayMode::Collapsed
    );
    assert_eq!(filters.sashimi_scale_mode, SashimiScaleMode::Automatic);
    assert_eq!(filters.sashimi_fixed_scale_maximum, 100);
    assert_eq!(filters.sashimi_count_mode, SashimiCountMode::Raw);
    assert!(filters.show_junction_labels);
    assert!(filters.show_reads);
    assert_eq!(filters.coverage_scale_maximum, None);
    assert_eq!(
        filters.coverage_scale_transform,
        CoverageScaleTransform::SquareRoot
    );
    assert!(filters.include_secondary);
    assert!(filters.include_supplementary);
    assert!(filters.show_coverage_center_line);
    assert!((filters.alignment_density - 1.0).abs() < f32::EPSILON);
    assert_eq!(filters.inspector_activation, InspectorActivation::Click);
    assert_eq!(
        filters.read_base_display_mode,
        ReadBaseDisplayMode::Mismatches
    );
    assert!(filters.shade_read_bases_by_quality);
    assert_eq!(filters.minimum_indel_size, 1);
    assert_eq!(filters.minimum_event_label_size, 1);
    assert!(filters.show_modified_bases);
    assert!(filters.show_modification_frequency);
    assert_eq!(filters.minimum_modification_probability, 0);
    assert_eq!(
        filters.modified_base_color_mode,
        ModifiedBaseColorMode::ByType
    );
    assert!(filters.hidden_modification_codes.is_empty());
}

#[test]
fn new_alignment_filters_use_igv_parity_defaults() {
    let filters = SessionFilters::default();
    assert!(!filters.include_secondary);
    assert!(filters.include_supplementary);
    assert_eq!(
        filters.coverage_scale_transform,
        CoverageScaleTransform::Linear
    );
    assert_eq!(filters.minimum_event_label_size, 10);
}

#[test]
fn version_one_migration_avoids_auxiliary_track_id_collisions() {
    let document =
        SessionDocument::from_json(include_str!("../../../validation/session-v1.json")).unwrap();
    assert_eq!(document.alignment_tracks[0].id, TrackId::new(2));
    assert_eq!(document.alignment_tracks[0].maximum_alignments, 1234);
    assert_eq!(
        document.alignment_tracks[0].filters.minimum_mapping_quality,
        Some(20)
    );
    assert_eq!(document.maximum_alignments, 1234);
}

#[test]
fn multiple_alignment_tracks_preserve_order_and_independent_settings() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    });
    let mut tumour = SessionAlignmentTrack::new(
        TrackId::new(10),
        "Tumour".to_owned(),
        TrackSource::Local {
            path: "tumour.bam".to_owned(),
        },
    );
    tumour.filters.minimum_mapping_quality = Some(30);
    tumour.filters.alignment_color_mode = AlignmentColorMode::Haplotype;
    let mut normal = SessionAlignmentTrack::new(
        TrackId::new(11),
        "Normal".to_owned(),
        TrackSource::Local {
            path: "normal.bam".to_owned(),
        },
    );
    normal.filters.minimum_mapping_quality = Some(10);
    normal.filters.show_modification_frequency = false;
    document.alignment_tracks = vec![tumour.clone(), normal.clone()];

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored.alignment_tracks, vec![tumour, normal]);
}

#[test]
fn duplicate_workspace_track_ids_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    add_test_alignment(&mut document);
    document.tracks.push(TrackConfig::new(
        TrackId::new(1),
        "genes".to_owned(),
        TrackKind::GeneModel,
        TrackSource::Local {
            path: "genes.gtf".to_owned(),
        },
    ));
    assert!(matches!(
        document.to_json(),
        Err(SessionError::DuplicateTrackId)
    ));
}

#[test]
fn malformed_or_excessive_hidden_modification_codes_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    add_test_alignment(&mut document);
    document.alignment_tracks[0]
        .filters
        .hidden_modification_codes = vec!["not-a-code".to_owned()];
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidModificationCodes)
    ));

    document.alignment_tracks[0]
        .filters
        .hidden_modification_codes = (0..9).map(|_| "m".to_owned()).collect();
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidModificationCodes)
    ));
}

#[test]
fn invalid_coverage_scale_and_alignment_density_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    add_test_alignment(&mut document);
    document.alignment_tracks[0].filters.coverage_scale_maximum = Some(0);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidCoverageScale)
    ));

    document.alignment_tracks[0].filters.coverage_scale_maximum = None;
    document.alignment_tracks[0].filters.alignment_density = f32::NAN;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentDensity)
    ));

    document.alignment_tracks[0].filters.alignment_density = 0.34;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentDensity)
    ));
}

#[test]
fn invalid_resized_read_group_height_is_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    add_test_alignment(&mut document);
    document.alignment_tracks[0].read_group_height = Some(39.0);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentTrack)
    ));

    document.alignment_tracks[0].read_group_height = Some(240.0);
    assert!(document.to_json().is_ok());
}

#[test]
fn invalid_alignment_style_tags_are_rejected_for_every_style_mode() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    add_test_alignment(&mut document);

    document.alignment_tracks[0].filters.alignment_sort_mode = AlignmentSortMode::Tag([0, b'1']);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentStyleTag)
    ));

    document.alignment_tracks[0].filters.alignment_sort_mode = AlignmentSortMode::Start;
    document.alignment_tracks[0].filters.alignment_group_mode = AlignmentGroupMode::Tag(*b"1X");
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentStyleTag)
    ));
}

#[test]
fn reversed_template_length_bounds_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    let alignment = add_test_alignment(&mut document);
    alignment.filters.minimum_template_length = Some(500);
    alignment.filters.maximum_template_length = Some(100);
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidTemplateLengthBounds)
    ));
}

#[test]
fn per_track_alignment_limit_cannot_exceed_the_workspace_ceiling() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    document.maximum_alignments = 1_000;
    add_test_alignment(&mut document).maximum_alignments = 1_001;

    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentLimit)
    ));
}

#[test]
fn session_rejects_more_alignment_engines_than_the_workspace_limit() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
    });
    document.alignment_tracks = (0..=MAXIMUM_ALIGNMENT_TRACKS)
        .map(|index| {
            SessionAlignmentTrack::new(
                TrackId::new(u64::try_from(index).unwrap().saturating_add(1)),
                format!("reads-{index}.bam"),
                TrackSource::Local {
                    path: format!("reads-{index}.bam"),
                },
            )
        })
        .collect();

    assert!(matches!(
        document.to_json(),
        Err(SessionError::TooManyAlignmentTracks)
    ));
}

#[test]
fn four_track_failure_isolation_fixture_is_a_valid_v2_session() {
    let document = SessionDocument::from_json(include_str!(
        "../../../validation/four-track-session-v2.json"
    ))
    .unwrap();

    assert_eq!(document.alignment_tracks.len(), 4);
    assert_eq!(document.alignment_tracks[0].id, TrackId::new(1));
    assert_eq!(document.alignment_tracks[3].id, TrackId::new(4));
    assert!(
        document.alignment_tracks[3]
            .source
            .redacted_label()
            .contains("missing")
    );
    assert_eq!(document.maximum_alignments, 50_000);
}

#[test]
fn multi_panel_four_track_fixture_is_a_valid_v3_session() {
    let document = SessionDocument::from_json(include_str!(
        "../../../validation/multi-panel-four-track-session-v3.json"
    ))
    .unwrap();

    assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
    assert_eq!(document.alignment_tracks.len(), 4);
    assert_eq!(document.panels.len(), 2);
    assert_eq!(document.panels[0].id, PanelId::new(7));
    assert_eq!(document.panels[1].id, PanelId::new(3));
    assert_eq!(document.active_panel, PanelId::new(3));
    assert_eq!(document.locus, document.panels[1].locus);
}

#[test]
fn partial_igv_import_extracts_locus_and_resources() {
    let xml = r#"<Session genome="hg38" locus="chr1:101-200"><Resources><Resource path="reads.bam"/><Resource path="genes.gff3"/><Resource path="https://example.org/signal.bw"/></Resources></Session>"#;
    let document = SessionDocument::import_igv(xml).unwrap();
    assert_eq!((document.locus.start, document.locus.end), (100, 200));
    assert_eq!(
        document.alignment_tracks[0].source,
        TrackSource::Local {
            path: "reads.bam".to_owned()
        }
    );
    assert_eq!(document.tracks.len(), 2);
    assert_eq!(document.tracks[0].kind, TrackKind::GeneModel);
    assert_eq!(document.tracks[1].kind, TrackKind::Quantitative);
}

#[test]
fn track_kind_inference_recognizes_gene_models_and_narrow_peaks() {
    for path in [
        "genes.genePred",
        "genes.genePred.gz",
        "genes.refFlat",
        "genes.refFlat.gz",
    ] {
        assert_eq!(infer_track_kind(path), TrackKind::GeneModel, "{path}");
    }
    assert_eq!(infer_track_kind("calls.narrowPeak"), TrackKind::Feature);
    assert_eq!(infer_track_kind("calls.narrowPeak.gz"), TrackKind::Feature);
    assert_eq!(
        infer_track_kind("archived-signal.tdf"),
        TrackKind::Quantitative
    );
}

#[test]
fn igv_import_preserves_every_alignment_resource_in_order() {
    let xml = r#"<Session locus="chr1:1-10"><Resources><Resource path="tumour.bam"/><Resource path="normal.cram"/><Resource path="genes.gtf"/></Resources></Session>"#;

    let document = SessionDocument::import_igv(xml).unwrap();

    assert_eq!(document.alignment_tracks.len(), 2);
    assert_eq!(document.alignment_tracks[0].name, "tumour.bam");
    assert_eq!(document.alignment_tracks[1].name, "normal.cram");
    assert_eq!(document.tracks.len(), 1);
    assert_eq!(document.tracks[0].id, TrackId::new(3));
}

#[test]
fn igv_import_applies_supported_track_state_and_reports_every_semantic_category() {
    let xml = r#"
            <Session genome="hg38" locus="chr1:101-200" version="8" custom="x">
              <Resources>
                <Resource path="tumour.bam" name="Tumour resource" index="tumour.bam.bai" url="https://example.org/details" type="bam"/>
              </Resources>
              <Panel height="900" name="DataPanel" width="1200">
                <Track id="tumour.bam" name="Tumour reads" visible="false" height="17" renderer="ALIGNMENTS" expand="false">
                  <DataRange minimum="0" maximum="100"/>
                </Track>
                <Track id="tumour.bam_coverage" visible="false" autoScale="true"/>
              </Panel>
              <Filter name="not-supported"/>
            </Session>
        "#;

    let imported = SessionDocument::import_igv_with_report(xml).unwrap();
    let track = &imported.document.alignment_tracks[0];
    assert_eq!(imported.document.genome_name.as_deref(), Some("hg38"));
    assert_eq!(track.name, "Tumour reads");
    assert!(!track.visible);
    assert!((track.height - 26.0).abs() < f32::EPSILON);
    assert!(!track.filters.show_coverage);
    assert!(
        imported
            .report
            .imported
            .iter()
            .any(|notice| notice.contains("Track.name"))
    );
    assert!(
        imported
            .report
            .approximated
            .iter()
            .any(|notice| notice.contains("panel placement"))
    );
    for ignored in [
        "Session.custom",
        "Resource.index",
        "Resource.url",
        "Resource.type",
        "Panel.height",
        "Track.renderer",
        "DataRange.minimum",
        "Filter.name",
    ] {
        assert!(
            imported
                .report
                .ignored
                .iter()
                .any(|notice| notice.contains(ignored)),
            "missing report for {ignored}: {:?}",
            imported.report
        );
    }
    assert_eq!(imported.report.omitted_notice_count, 0);
}

#[test]
fn future_session_versions_are_rejected() {
    assert!(matches!(
        SessionDocument::from_json(include_str!("../../../validation/future-session-v99.json")),
        Err(SessionError::UnsupportedVersion(99))
    ));
}

#[test]
fn project_sessions_store_inside_sources_relatively_and_resolve_them_on_read() {
    let project = tempfile::tempdir().unwrap();
    let data_directory = project.path().join("data");
    std::fs::create_dir(&data_directory).unwrap();
    let reads = data_directory.join("reads.bam");
    let reference = data_directory.join("reference.fa");
    std::fs::write(&reads, b"reads").unwrap();
    std::fs::write(&reference, b"reference").unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();

    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.reference = Some(TrackSource::Local {
        path: reference.display().to_string(),
    });
    document.alignment_tracks.push(SessionAlignmentTrack::new(
        TrackId::new(1),
        "reads".to_owned(),
        TrackSource::Local {
            path: reads.display().to_string(),
        },
    ));
    document.tracks.push(TrackConfig::new(
        TrackId::new(2),
        "outside".to_owned(),
        TrackKind::Feature,
        TrackSource::Local {
            path: outside.path().display().to_string(),
        },
    ));
    let session_path = project.path().join("analysis.json");

    document.write(&session_path).unwrap();

    let canonical_reads = reads.canonicalize().unwrap();
    let canonical_reference = reference.canonicalize().unwrap();
    let canonical_outside = outside.path().canonicalize().unwrap();
    let stored = std::fs::read_to_string(&session_path).unwrap();
    assert!(stored.contains(&format!("data{}reads.bam", std::path::MAIN_SEPARATOR)));
    assert!(stored.contains(&format!("data{}reference.fa", std::path::MAIN_SEPARATOR)));
    assert!(stored.contains(&canonical_outside.display().to_string()));

    let reopened = SessionDocument::read(&session_path).unwrap();
    assert_eq!(
        reopened.alignment_tracks[0].source,
        TrackSource::Local {
            path: canonical_reads.display().to_string()
        }
    );
    assert_eq!(
        reopened.reference,
        Some(TrackSource::Local {
            path: canonical_reference.display().to_string()
        })
    );
    assert!(reopened.unresolved_local_sources().is_empty());
    let mut expected = document.clone();
    expected.alignment_tracks[0].source = TrackSource::Local {
        path: canonical_reads.display().to_string(),
    };
    expected.reference = Some(TrackSource::Local {
        path: canonical_reference.display().to_string(),
    });
    expected.tracks[0].source = TrackSource::Local {
        path: canonical_outside.display().to_string(),
    };
    assert_eq!(reopened, expected);

    let relocated = tempfile::tempdir().unwrap();
    std::fs::create_dir(relocated.path().join("data")).unwrap();
    std::fs::copy(&session_path, relocated.path().join("analysis.json")).unwrap();
    std::fs::copy(&reads, relocated.path().join("data/reads.bam")).unwrap();
    std::fs::copy(&reference, relocated.path().join("data/reference.fa")).unwrap();
    let moved = SessionDocument::read(relocated.path().join("analysis.json")).unwrap();
    let relocated_reads = relocated
        .path()
        .join("data/reads.bam")
        .canonicalize()
        .unwrap();
    assert_eq!(
        moved.alignment_tracks[0].source,
        TrackSource::Local {
            path: relocated_reads.display().to_string()
        }
    );
    assert!(moved.unresolved_local_sources().is_empty());
}

#[test]
fn unresolved_project_sources_are_reported_individually() {
    let project = tempfile::tempdir().unwrap();
    let session_path = project.path().join("analysis.json");
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.reference = Some(TrackSource::Local {
        path: project.path().join("missing.fa").display().to_string(),
    });
    document.alignment_tracks.push(SessionAlignmentTrack::new(
        TrackId::new(1),
        "tumour".to_owned(),
        TrackSource::Local {
            path: project.path().join("missing.bam").display().to_string(),
        },
    ));
    document.write(&session_path).unwrap();

    let reopened = SessionDocument::read(&session_path).unwrap();
    let issues = reopened.unresolved_local_sources();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].label, "reference");
    assert_eq!(issues[1].label, "alignment track 1 (tumour)");
    assert!(issues.iter().all(|issue| issue.path.is_absolute()));
}

#[test]
fn repeated_atomic_session_saves_replace_the_complete_document() {
    let project = tempfile::tempdir().unwrap();
    let session_path = project.path().join("analysis.json");
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.write(&session_path).unwrap();
    document.locus.start = 300;
    document.locus.end = 400;
    document.panels[0].locus = document.locus.clone();
    document.write(&session_path).unwrap();

    let reopened = SessionDocument::read(&session_path).unwrap();
    assert_eq!(reopened.locus.start, 300);
    assert_eq!(reopened.locus.end, 400);
}

#[test]
fn failed_session_save_leaves_the_last_good_project_untouched() {
    let project = tempfile::tempdir().unwrap();
    let session_path = project.path().join("analysis.json");
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.write(&session_path).unwrap();
    document.locus.start = 500;
    document.locus.end = 400;
    assert!(matches!(
        document.write(&session_path),
        Err(SessionError::InvalidLocus)
    ));

    let reopened = SessionDocument::read(&session_path).unwrap();
    assert_eq!(reopened.locus.start, 100);
    assert_eq!(reopened.locus.end, 200);
}

#[test]
fn session_json_never_contains_remote_credentials() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let alignment = add_test_alignment(&mut document);
    alignment.source = TrackSource::Remote {
        url: "https://alice:secret@example.org/sample.bam?token=private#fragment".to_owned(),
    };
    alignment.remote_index_url = Some(
        "https://index-user:index-secret@example.org/sample.bai?access_token=index-private"
            .to_owned(),
    );
    document.tracks.push(TrackConfig::new(
        TrackId::new(2),
        "signal".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Remote {
            url: "https://example.org/signal.bw?X-Amz-Signature=private".to_owned(),
        },
    ));
    document.cytobands = Some(TrackSource::Remote {
        url: "https://example.org/cytobands.txt?access_token=private".to_owned(),
    });
    document.reference = Some(TrackSource::Remote {
        url: "https://example.org/reference.fa".to_owned(),
    });
    document.remote_reference_index_url =
        Some("https://ref-user:ref-secret@example.org/reference.fai?token=private".to_owned());

    let json = document.to_json().unwrap();
    assert!(!json.contains("alice"));
    assert!(!json.contains("secret"));
    assert!(!json.contains("token"));
    assert!(!json.contains("X-Amz"));
    assert!(!json.contains("index-user"));
    assert!(!json.contains("index-secret"));
    assert!(!json.contains("ref-user"));
    assert!(!json.contains("ref-secret"));
    assert!(json.contains("https://example.org/sample.bam"));
    assert!(json.contains("https://example.org/sample.bai"));
    assert!(json.contains("https://example.org/reference.fai"));
    assert!(json.contains("https://example.org/signal.bw"));
    assert!(json.contains("https://example.org/cytobands.txt"));
}

#[test]
fn bisulfite_context_round_trips_without_changing_older_defaults() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document)
        .filters
        .alignment_color_mode = AlignmentColorMode::Bisulfite(BisulfiteContext::Wcg);

    let json = document.to_json().unwrap();
    assert!(json.contains("bisulfite"));
    assert!(json.contains("wcg"));
    let reopened = SessionDocument::from_json(&json).unwrap();
    assert_eq!(
        reopened.alignment_tracks[0].filters.alignment_color_mode,
        AlignmentColorMode::Bisulfite(BisulfiteContext::Wcg)
    );
    assert_eq!(
        SessionFilters::default().alignment_color_mode,
        AlignmentColorMode::None
    );
}

#[test]
fn smrt_kinetics_mode_round_trips_without_changing_older_defaults() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document)
        .filters
        .alignment_color_mode =
        AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsReversePulseWidth);

    let json = document.to_json().unwrap();
    assert!(json.contains("smrt_kinetics"));
    assert!(json.contains("ccs_reverse_pulse_width"));
    let reopened = SessionDocument::from_json(&json).unwrap();
    assert_eq!(
        reopened.alignment_tracks[0].filters.alignment_color_mode,
        AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsReversePulseWidth)
    );
    assert_eq!(
        SessionFilters::default().alignment_color_mode,
        AlignmentColorMode::None
    );
}

#[test]
fn ultima_display_options_round_trip_and_reject_zero_override() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document)
        .filters
        .alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions {
        quality_mode: UltimaQualityMode::Minimum,
        maximum_homopolymer_length_override: Some(14),
    });

    let json = document.to_json().unwrap();
    assert!(json.contains("ultima"));
    assert!(json.contains("minimum"));
    let reopened = SessionDocument::from_json(&json).unwrap();
    assert_eq!(
        reopened.alignment_tracks[0].filters.alignment_color_mode,
        document.alignment_tracks[0].filters.alignment_color_mode
    );

    if let AlignmentColorMode::Ultima(options) =
        &mut document.alignment_tracks[0].filters.alignment_color_mode
    {
        options.maximum_homopolymer_length_override = Some(0);
    }
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentDisplayThreshold)
    ));
}

#[test]
fn sbx_display_options_round_trip_and_reject_reversed_thresholds() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document)
        .filters
        .alignment_color_mode = AlignmentColorMode::Sbx(SbxDisplayOptions {
        simplex_tail_mode: SbxSimplexTailMode::Hide,
        discordant_quality_maximum: 12,
        simplex_tail_quality_maximum: 32,
    });

    let json = document.to_json().unwrap();
    let reopened = SessionDocument::from_json(&json).unwrap();
    assert_eq!(
        reopened.alignment_tracks[0].filters.alignment_color_mode,
        document.alignment_tracks[0].filters.alignment_color_mode
    );

    if let AlignmentColorMode::Sbx(options) =
        &mut document.alignment_tracks[0].filters.alignment_color_mode
    {
        options.simplex_tail_quality_maximum = options.discordant_quality_maximum;
    }
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentDisplayThreshold)
    ));

    if let AlignmentColorMode::Sbx(options) =
        &mut document.alignment_tracks[0].filters.alignment_color_mode
    {
        options.discordant_quality_maximum = 10;
        options.simplex_tail_quality_maximum = 94;
    }
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentDisplayThreshold)
    ));
}
