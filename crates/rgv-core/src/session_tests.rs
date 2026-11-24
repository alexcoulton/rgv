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
    filters.alignment_sort_anchor = Some(151);
    filters.read_stacking_mode = ReadStackingMode::Compact;
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
    filters.alignment_view_filter.all = vec![
        AlignmentViewPredicate::CigarEvent {
            event: AlignmentCigarEventPredicate::Insertion,
            scope: AlignmentPredicateScope::Position {
                contig: "chr1".to_owned(),
                position: 150,
            },
            minimum_length: Some(2),
            maximum_length: Some(20),
        },
        AlignmentViewPredicate::Auxiliary {
            tag: *b"HP",
            comparison: AlignmentAuxComparison::Equal,
            value: Some(AlignmentAuxFilterValue::Integer(1)),
        },
    ];
    filters.alignment_highlight_filter.all = vec![AlignmentViewPredicate::Substitution {
        scope: AlignmentPredicateScope::Interval {
            contig: "chr1".to_owned(),
            start: 140,
            end: 160,
        },
        observed_base: Some(b'T'),
        minimum_count: Some(1),
        maximum_count: Some(4),
    }];
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
    document.normalize_presentation();
    assert_eq!(
        SessionDocument::from_json(&document.to_json().unwrap()).unwrap(),
        document
    );
}

#[test]
fn builtin_landmark_identity_and_suppression_round_trip_portably() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    document.landmark_context = Some(crate::BuiltInLandmarkContext {
        bundle_id: "hg38".to_owned(),
        bundle_version: "1.0-rgv1".to_owned(),
    });
    document
        .suppressed_builtin_gene_bundles
        .insert("mm39".to_owned());
    document.tracks.push(
        TrackConfig::new(
            TrackId::new(9),
            "Genes".to_owned(),
            TrackKind::GeneModel,
            TrackSource::Local {
                path: "/developer/machine/genes.genePred.gz".to_owned(),
            },
        )
        .with_built_in(crate::BuiltInTrackIdentity {
            bundle_id: "hg38".to_owned(),
            bundle_version: "1.0-rgv1".to_owned(),
            asset_role: crate::LandmarkAssetRole::RepresentativeGenes,
            sha256: "a".repeat(64),
        }),
    );
    document.normalize_presentation();

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(restored.landmark_context, document.landmark_context);
    assert_eq!(
        restored.suppressed_builtin_gene_bundles,
        document.suppressed_builtin_gene_bundles
    );
    assert_eq!(restored.tracks[0].built_in, document.tracks[0].built_in);
    assert_eq!(restored.tracks[0].source, document.tracks[0].source);
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
fn legacy_alignment_group_mirrors_are_consumed_and_cleared_during_normalization() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let track = add_test_alignment(&mut document);
    track.track_group = Some(7);
    track.track_group_collapsed = true;
    track.read_track_group = Some(8);
    track.read_track_group_collapsed = true;

    document.normalize_presentation();

    let track = &document.alignment_tracks[0];
    assert_eq!(track.track_group, None);
    assert!(!track.track_group_collapsed);
    assert_eq!(track.read_track_group, None);
    assert!(!track.read_track_group_collapsed);
    assert!(document.to_json().is_ok());
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
    document.normalize_presentation();
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
    document.normalize_presentation();
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
    document.normalize_presentation();
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
    document.normalize_presentation();
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
    document.normalize_presentation();

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
    document.normalize_presentation();

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
fn channel_presentation_and_read_cameras_round_trip_independently() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document);
    document.alignment_tracks.push(SessionAlignmentTrack::new(
        TrackId::new(2),
        "normal.bam".to_owned(),
        TrackSource::Local {
            path: "normal.bam".to_owned(),
        },
    ));
    document.presentation =
        WorkspacePresentation::from_legacy(&document.alignment_tracks, &document.tracks);
    document.presentation.read_pane_height = 420.0;
    document.presentation.groups = vec![
        SessionPresentationGroup {
            id: 1,
            label: "Annotation comparison".to_owned(),
            channel: TrackChannel::Annotation,
            members: vec![TrackId::new(1), TrackId::new(2)],
            expanded: false,
            content_visible: true,
        },
        SessionPresentationGroup {
            id: 1,
            label: "Read comparison".to_owned(),
            channel: TrackChannel::Reads,
            members: vec![TrackId::new(1), TrackId::new(2)],
            expanded: true,
            content_visible: false,
        },
    ];
    document.presentation.next_group_id = 2;
    document.panels[0].read_panel_states = vec![SessionReadPanelState {
        track_id: TrackId::new(2),
        scroll_points: 120.0,
        content_origin_points: 20.0,
        latent_scroll_fraction: Some(0.4),
        content_bottom_points: 900.0,
        density: Some(2.5),
    }];

    let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();

    assert_eq!(restored.presentation, document.presentation);
    assert_eq!(
        restored.panels[0].read_panel_states,
        document.panels[0].read_panel_states
    );
    assert_ne!(
        restored.presentation.groups[0].channel,
        restored.presentation.groups[1].channel
    );
}

#[test]
fn schema_four_writes_only_normalized_presentation_state() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    add_test_alignment(&mut document);
    document.tracks.push(TrackConfig::new(
        TrackId::new(2),
        "features".to_owned(),
        TrackKind::Feature,
        TrackSource::Local {
            path: "features.bed".to_owned(),
        },
    ));
    document.normalize_presentation();

    let json = serde_json::from_str::<serde_json::Value>(&document.to_json().unwrap()).unwrap();
    let alignment = &json["alignment_tracks"][0];
    for legacy in [
        "visible",
        "collapsed",
        "track_group",
        "track_group_collapsed",
        "read_track_group",
        "read_track_group_collapsed",
        "height",
        "annotation_height",
        "read_group_height",
    ] {
        assert!(
            alignment.get(legacy).is_none(),
            "legacy field {legacy} leaked"
        );
    }
    assert!(alignment["filters"].get("show_reads").is_none());
    let auxiliary = &json["tracks"][0];
    for legacy in ["visible", "collapsed", "height"] {
        assert!(
            auxiliary.get(legacy).is_none(),
            "legacy field {legacy} leaked"
        );
    }
    assert!(json["presentation"]["pane_tracks"].is_array());
}

fn three_track_presentation() -> WorkspacePresentation {
    let tracks = (1..=3)
        .map(|id| {
            SessionAlignmentTrack::new(
                TrackId::new(id),
                format!("sample-{id}"),
                TrackSource::Local {
                    path: format!("sample-{id}.bam"),
                },
            )
        })
        .collect::<Vec<_>>();
    WorkspacePresentation::from_legacy(&tracks, &[])
}

#[test]
fn presentation_reducer_is_atomic_and_channel_qualified() {
    let mut presentation = three_track_presentation();
    let original = presentation.clone();
    let invalid = presentation.apply_intents(&[
        TrackIntent::SetNodeVisibility {
            target: PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(1),
            },
            visible: false,
        },
        TrackIntent::ResizeReadPane { height: f32::NAN },
    ]);
    assert_eq!(invalid, Err(TrackIntentError::InvalidSize));
    assert_eq!(
        presentation, original,
        "a failed batch must not partially commit"
    );

    presentation
        .apply_intents(&[
            TrackIntent::GroupNodes {
                channel: TrackChannel::Annotation,
                members: vec![TrackId::new(1), TrackId::new(2)],
            },
            TrackIntent::GroupNodes {
                channel: TrackChannel::Reads,
                members: vec![TrackId::new(1), TrackId::new(2)],
            },
        ])
        .unwrap();
    assert_eq!(presentation.groups.len(), 2);
    assert_ne!(presentation.groups[0].id, presentation.groups[1].id);
    assert_ne!(
        presentation.groups[0].channel,
        presentation.groups[1].channel
    );
}

#[test]
fn explicit_read_pane_resize_leaves_annotation_auto_fit() {
    let mut presentation = three_track_presentation();
    assert!(presentation.annotation_pane_auto_fit);

    presentation
        .apply_intents(&[TrackIntent::ResizeReadPane { height: 420.0 }])
        .unwrap();

    assert_eq!(presentation.read_pane_height, 420.0);
    assert!(!presentation.annotation_pane_auto_fit);
}

#[test]
fn section_folding_is_a_nondestructive_workspace_gate() {
    let mut presentation = three_track_presentation();
    presentation.reference_content_visible = false;
    presentation.reference_preferred_height = Some(91.0);
    presentation
        .pane_tracks
        .iter_mut()
        .find(|node| node.channel == TrackChannel::Annotation && node.track_id == TrackId::new(1))
        .unwrap()
        .preferred_height = Some(137.0);
    presentation
        .apply_intents(&[
            TrackIntent::SetNodeVisibility {
                target: PresentationNodeId::Track {
                    channel: TrackChannel::Annotation,
                    track_id: TrackId::new(2),
                },
                visible: false,
            },
            TrackIntent::GroupNodes {
                channel: TrackChannel::Annotation,
                members: vec![TrackId::new(1), TrackId::new(2)],
            },
        ])
        .unwrap();
    let reference_state = (
        presentation.reference_content_visible,
        presentation.reference_preferred_height,
    );
    let pane_tracks = presentation.pane_tracks.clone();
    let groups = presentation.groups.clone();
    let composites = presentation.composites.clone();

    let changes = presentation
        .apply_intents(&[TrackIntent::SetSectionExpanded {
            channel: TrackChannel::Annotation,
            expanded: false,
        }])
        .unwrap();

    assert!(changes.presentation_changed);
    assert!(!changes.selection_changed);
    assert!(!presentation.annotation_section_expanded);
    assert!(presentation.read_section_expanded);
    assert_eq!(
        (
            presentation.reference_content_visible,
            presentation.reference_preferred_height,
        ),
        reference_state
    );
    assert_eq!(presentation.pane_tracks, pane_tracks);
    assert_eq!(presentation.groups, groups);
    assert_eq!(presentation.composites, composites);

    presentation
        .apply_intents(&[TrackIntent::SetSectionExpanded {
            channel: TrackChannel::Annotation,
            expanded: true,
        }])
        .unwrap();
    assert!(presentation.annotation_section_expanded);
    assert_eq!(presentation.pane_tracks, pane_tracks);
    assert_eq!(presentation.groups, groups);
}

#[test]
fn pane_bulk_controls_are_channel_scoped_and_reset_only_row_heights() {
    let mut presentation = three_track_presentation();
    presentation.reference_preferred_height = Some(88.0);
    for node in &mut presentation.pane_tracks {
        node.preferred_height = Some(if node.channel == TrackChannel::Annotation {
            120.0
        } else {
            240.0
        });
    }
    let reads_before = presentation
        .pane_tracks
        .iter()
        .filter(|node| node.channel == TrackChannel::Reads)
        .cloned()
        .collect::<Vec<_>>();

    presentation
        .apply_intents(&[
            TrackIntent::SetPaneRowsExpanded {
                channel: TrackChannel::Annotation,
                expanded: false,
            },
            TrackIntent::ResetPaneRowHeights {
                channel: TrackChannel::Annotation,
            },
        ])
        .unwrap();

    assert!(!presentation.reference_content_visible);
    assert_eq!(presentation.reference_preferred_height, None);
    assert!(
        presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == TrackChannel::Annotation)
            .all(|node| !node.content_visible && node.preferred_height.is_none())
    );
    assert_eq!(
        presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == TrackChannel::Reads)
            .cloned()
            .collect::<Vec<_>>(),
        reads_before
    );
}

#[test]
fn presentation_reducer_moves_groups_atomically_and_rejects_member_escape() {
    let mut presentation = three_track_presentation();
    presentation
        .apply_intents(&[TrackIntent::GroupNodes {
            channel: TrackChannel::Reads,
            members: vec![TrackId::new(1), TrackId::new(2)],
        }])
        .unwrap();
    let before = presentation.clone();
    assert_eq!(
        presentation.apply_intents(&[TrackIntent::MoveNode {
            target: PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(1),
            },
            before: None,
        }]),
        Err(TrackIntentError::GroupMemberMove)
    );
    assert_eq!(presentation, before);

    presentation
        .apply_intents(&[TrackIntent::MoveNode {
            target: PresentationNodeId::Group {
                channel: TrackChannel::Reads,
                group_id: 1,
            },
            before: None,
        }])
        .unwrap();
    let read_order = presentation
        .pane_tracks
        .iter()
        .filter(|node| node.channel == TrackChannel::Reads)
        .map(|node| node.track_id)
        .collect::<Vec<_>>();
    assert_eq!(
        read_order,
        vec![TrackId::new(3), TrackId::new(1), TrackId::new(2)]
    );
}

#[test]
fn move_before_a_group_member_targets_the_complete_group_anchor() {
    let mut presentation = six_track_presentation();
    presentation
        .apply_intents(&[TrackIntent::GroupNodes {
            channel: TrackChannel::Reads,
            members: vec![TrackId::new(3), TrackId::new(4)],
        }])
        .unwrap();

    presentation
        .apply_intents(&[TrackIntent::MoveNode {
            target: PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(1),
            },
            before: Some(PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(4),
            }),
        }])
        .unwrap();

    let order = presentation
        .pane_tracks
        .iter()
        .filter(|node| node.channel == TrackChannel::Reads)
        .map(|node| node.track_id)
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        (vec![2, 1, 3, 4, 5, 6])
            .into_iter()
            .map(TrackId::new)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        presentation.groups[0].members,
        vec![TrackId::new(3), TrackId::new(4)]
    );
}

fn six_track_presentation() -> WorkspacePresentation {
    let tracks = (1..=6)
        .map(|id| {
            SessionAlignmentTrack::new(
                TrackId::new(id),
                format!("sample-{id}"),
                TrackSource::Local {
                    path: format!("sample-{id}.bam"),
                },
            )
        })
        .collect::<Vec<_>>();
    WorkspacePresentation::from_legacy(&tracks, &[])
}

#[test]
fn hiding_the_ungrouped_read_does_not_resurrect_or_mutate_the_hidden_group() {
    let mut presentation = six_track_presentation();
    presentation
        .apply_intents(&[TrackIntent::GroupNodes {
            channel: TrackChannel::Reads,
            members: (2..=6).map(TrackId::new).collect(),
        }])
        .unwrap();
    let group_id = presentation.groups[0].id;
    let group = PresentationNodeId::Group {
        channel: TrackChannel::Reads,
        group_id,
    };
    presentation
        .apply_intents(&[
            TrackIntent::SetGroupExpanded {
                target: group,
                expanded: false,
            },
            TrackIntent::SetGroupVisibility {
                target: group,
                visible: false,
            },
        ])
        .unwrap();
    let before = presentation.groups[0].clone();

    presentation
        .apply_intents(&[TrackIntent::SetNodeVisibility {
            target: PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(1),
            },
            visible: false,
        }])
        .unwrap();

    assert_eq!(presentation.groups[0], before);
    assert!(!presentation.groups[0].expanded);
    assert!(!presentation.groups[0].content_visible);
    assert_eq!(
        presentation.pane_root_nodes(TrackChannel::Reads),
        vec![
            PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id: TrackId::new(1),
            },
            group,
        ]
    );
}

#[test]
fn group_visibility_is_a_parent_gate_and_preserves_mixed_child_eye_states() {
    let mut presentation = three_track_presentation();
    presentation
        .apply_intents(&[
            TrackIntent::SetNodeVisibility {
                target: PresentationNodeId::Track {
                    channel: TrackChannel::Reads,
                    track_id: TrackId::new(2),
                },
                visible: false,
            },
            TrackIntent::GroupNodes {
                channel: TrackChannel::Reads,
                members: vec![TrackId::new(1), TrackId::new(2)],
            },
        ])
        .unwrap();
    let group = PresentationNodeId::Group {
        channel: TrackChannel::Reads,
        group_id: presentation.groups[0].id,
    };

    presentation
        .apply_intents(&[TrackIntent::SetGroupVisibility {
            target: group,
            visible: false,
        }])
        .unwrap();
    assert_eq!(
        presentation.effective_track_visibility(TrackChannel::Reads, TrackId::new(1)),
        Some(false)
    );
    presentation
        .apply_intents(&[TrackIntent::SetGroupVisibility {
            target: group,
            visible: true,
        }])
        .unwrap();

    assert_eq!(
        presentation.effective_track_visibility(TrackChannel::Reads, TrackId::new(1)),
        Some(true)
    );
    assert_eq!(
        presentation.effective_track_visibility(TrackChannel::Reads, TrackId::new(2)),
        Some(false)
    );
    assert!(
        presentation
            .pane_track_state(TrackChannel::Reads, TrackId::new(1))
            .unwrap()
            .content_visible
    );
    assert!(
        !presentation
            .pane_track_state(TrackChannel::Reads, TrackId::new(2))
            .unwrap()
            .content_visible
    );
}

#[test]
fn composite_visibility_is_a_parent_gate_and_preserves_member_eyes() {
    let mut members = [
        TrackConfig::new(
            TrackId::new(1),
            "signal-a".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: "a.bw".to_owned(),
            },
        ),
        TrackConfig::new(
            TrackId::new(2),
            "signal-b".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: "b.bw".to_owned(),
            },
        ),
    ];
    for member in &mut members {
        member.quantitative.overlay_group = Some(9);
        member.quantitative.scale_group = Some(9);
        member.quantitative.scale_mode = crate::QuantitativeScaleMode::Shared;
    }
    let mut presentation = WorkspacePresentation::from_legacy(&[], &members);
    presentation.reconcile_composites(TrackChannel::Annotation, &BTreeSet::from([9]));
    presentation
        .apply_intents(&[TrackIntent::SetNodeVisibility {
            target: PresentationNodeId::Track {
                channel: TrackChannel::Annotation,
                track_id: TrackId::new(2),
            },
            visible: false,
        }])
        .unwrap();

    presentation
        .apply_intents(&[TrackIntent::SetCompositeVisibility {
            channel: TrackChannel::Annotation,
            composite_id: 9,
            visible: false,
        }])
        .unwrap();
    presentation
        .apply_intents(&[TrackIntent::SetCompositeVisibility {
            channel: TrackChannel::Annotation,
            composite_id: 9,
            visible: true,
        }])
        .unwrap();

    assert!(
        presentation
            .pane_track_state(TrackChannel::Annotation, TrackId::new(1))
            .unwrap()
            .content_visible
    );
    assert!(
        !presentation
            .pane_track_state(TrackChannel::Annotation, TrackId::new(2))
            .unwrap()
            .content_visible
    );
}

#[test]
fn presentation_group_ids_are_monotonic_and_never_reused() {
    let mut presentation = three_track_presentation();
    presentation
        .apply_intents(&[TrackIntent::GroupNodes {
            channel: TrackChannel::Reads,
            members: vec![TrackId::new(1), TrackId::new(2)],
        }])
        .unwrap();
    let first_id = presentation.groups[0].id;
    presentation
        .apply_intents(&[TrackIntent::Ungroup {
            target: PresentationNodeId::Group {
                channel: TrackChannel::Reads,
                group_id: first_id,
            },
        }])
        .unwrap();
    presentation
        .apply_intents(&[TrackIntent::GroupNodes {
            channel: TrackChannel::Reads,
            members: vec![TrackId::new(1), TrackId::new(2)],
        }])
        .unwrap();

    assert!(presentation.groups[0].id > first_id);
}

fn assert_presentation_invariants(presentation: &WorkspacePresentation) {
    let mut qualified_group_ids = BTreeSet::new();
    for channel in [TrackChannel::Annotation, TrackChannel::Reads] {
        let pane_tracks = presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == channel)
            .collect::<Vec<_>>();
        let pane_ids = pane_tracks
            .iter()
            .map(|node| node.track_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(pane_ids.len(), pane_tracks.len());

        let mut grouped = BTreeSet::new();
        for group in presentation
            .groups
            .iter()
            .filter(|group| group.channel == channel)
        {
            assert!(qualified_group_ids.insert((channel, group.id)));
            assert!(group.members.len() >= 2);
            let members = group.members.iter().copied().collect::<BTreeSet<_>>();
            assert_eq!(members.len(), group.members.len());
            assert!(members.iter().all(|member| pane_ids.contains(member)));
            assert!(members.iter().all(|member| grouped.insert(*member)));
            let positions = group
                .members
                .iter()
                .map(|member| {
                    pane_tracks
                        .iter()
                        .position(|node| node.track_id == *member)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert!(positions.windows(2).all(|pair| pair[1] == pair[0] + 1));
        }

        let roots = presentation.pane_root_nodes(channel);
        assert_eq!(
            roots.iter().copied().collect::<BTreeSet<_>>().len(),
            roots.len()
        );
        assert!(roots.iter().all(|root| {
            match root {
                PresentationNodeId::Track { track_id, .. } => !grouped.contains(track_id),
                PresentationNodeId::Group { group_id, .. } => presentation
                    .groups
                    .iter()
                    .any(|group| group.channel == channel && group.id == *group_id),
                PresentationNodeId::Reference => false,
            }
        }));
        let interactive = presentation.pane_interactive_nodes(channel);
        assert_eq!(
            interactive.iter().copied().collect::<BTreeSet<_>>().len(),
            interactive.len()
        );
        for node in pane_tracks {
            let effective = presentation
                .effective_track_visibility(channel, node.track_id)
                .unwrap();
            let exposed = presentation
                .track_content_exposed(channel, node.track_id)
                .unwrap();
            assert!(!exposed || effective);
        }
    }
    assert!(
        presentation
            .groups
            .iter()
            .all(|group| group.id < presentation.next_group_id)
    );
}

#[test]
fn presentation_reducer_preserves_invariants_across_deterministic_random_sequences() {
    for seed in 1..=24_u64 {
        let mut state = six_track_presentation();
        let mut random = seed;
        for _ in 0..160 {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let channel = if random & 1 == 0 {
                TrackChannel::Annotation
            } else {
                TrackChannel::Reads
            };
            let pane_ids = state
                .pane_tracks
                .iter()
                .filter(|node| node.channel == channel)
                .map(|node| node.track_id)
                .collect::<Vec<_>>();
            if pane_ids.is_empty() {
                break;
            }
            let pick = pane_ids[(random as usize >> 8) % pane_ids.len()];
            match (random >> 2) % 8 {
                0 => {
                    state
                        .apply_intents(&[TrackIntent::SetNodeVisibility {
                            target: PresentationNodeId::Track {
                                channel,
                                track_id: pick,
                            },
                            visible: random & 0x20 == 0,
                        }])
                        .unwrap();
                }
                1 => {
                    state
                        .apply_intents(&[TrackIntent::ResizeNode {
                            target: PresentationNodeId::Track {
                                channel,
                                track_id: pick,
                            },
                            preferred_height: match channel {
                                TrackChannel::Annotation => 80.0 + (random % 300) as f32,
                                TrackChannel::Reads => 40.0 + (random % 500) as f32,
                            },
                        }])
                        .unwrap();
                }
                2 => {
                    let ungrouped = pane_ids
                        .windows(2)
                        .find(|pair| {
                            pair.iter()
                                .all(|track_id| state.group_for_track(channel, *track_id).is_none())
                        })
                        .map(|pair| pair.to_vec());
                    if let Some(members) = ungrouped {
                        state
                            .apply_intents(&[TrackIntent::GroupNodes { channel, members }])
                            .unwrap();
                    }
                }
                3 | 4 => {
                    if let Some(group) = state
                        .groups
                        .iter()
                        .find(|group| group.channel == channel)
                        .cloned()
                    {
                        let target = PresentationNodeId::Group {
                            channel,
                            group_id: group.id,
                        };
                        let intent = if (random >> 2) % 8 == 3 {
                            TrackIntent::SetGroupExpanded {
                                target,
                                expanded: !group.expanded,
                            }
                        } else {
                            TrackIntent::SetGroupVisibility {
                                target,
                                visible: !group.content_visible,
                            }
                        };
                        state.apply_intents(&[intent]).unwrap();
                    }
                }
                5 => {
                    if let Some(group) = state
                        .groups
                        .iter()
                        .find(|group| group.channel == channel)
                        .cloned()
                    {
                        state
                            .apply_intents(&[TrackIntent::Ungroup {
                                target: PresentationNodeId::Group {
                                    channel,
                                    group_id: group.id,
                                },
                            }])
                            .unwrap();
                    }
                }
                6 => {
                    let roots = state.pane_root_nodes(channel);
                    if roots.len() > 1 {
                        let target = roots[(random as usize >> 12) % roots.len()];
                        let before = roots.iter().copied().find(|candidate| *candidate != target);
                        state
                            .apply_intents(&[TrackIntent::MoveNode { target, before }])
                            .unwrap();
                    }
                }
                _ => {
                    let loaded = state
                        .pane_tracks
                        .iter()
                        .map(|node| node.track_id)
                        .collect::<BTreeSet<_>>();
                    if loaded.len() > 3 {
                        state.remove_source(pick);
                    }
                }
            }
            assert_presentation_invariants(&state);
        }
    }
}

#[test]
fn schema_four_serializes_only_normalized_presentation_fields() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    });
    add_test_alignment(&mut document);
    document.alignment_tracks[0].collapsed = true;
    document.alignment_tracks[0].track_group = Some(7);
    document.alignment_tracks[0].read_track_group = Some(8);
    document.alignment_tracks[0].annotation_height = Some(222.0);
    document.alignment_tracks[0].read_group_height = Some(333.0);
    document.normalize_presentation();

    let json = document.to_json().unwrap();

    assert!(json.contains("\"presentation\""));
    assert!(!json.contains("\"track_group\""));
    assert!(!json.contains("\"read_track_group\""));
    assert!(!json.contains("\"annotation_height\""));
    assert!(!json.contains("\"read_group_height\""));
    assert!(!json.contains("\"collapsed\":"));
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
fn version_three_migration_qualifies_equal_group_ids_by_channel() {
    let mut legacy = serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../validation/multi-panel-four-track-session-v3.json"
    ))
    .unwrap();
    for track in legacy["alignment_tracks"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .take(2)
    {
        track["track_group"] = serde_json::json!(7);
        track["read_track_group"] = serde_json::json!(7);
    }
    legacy["alignment_tracks"][0]["track_group_collapsed"] = serde_json::json!(false);
    legacy["alignment_tracks"][1]["track_group_collapsed"] = serde_json::json!(true);

    let document = SessionDocument::from_json(&serde_json::to_string(&legacy).unwrap()).unwrap();
    let annotation = document
        .presentation
        .groups
        .iter()
        .find(|group| group.channel == TrackChannel::Annotation && group.id == 7)
        .unwrap();
    let reads = document
        .presentation
        .groups
        .iter()
        .find(|group| group.channel == TrackChannel::Reads && group.id == 7)
        .unwrap();

    assert_eq!(annotation.members, reads.members);
    assert!(!annotation.expanded, "any collapsed legacy member wins");
    assert!(reads.expanded);
    assert_eq!(document.migration_notices.len(), 1);
    assert!(document.migration_notices[0].contains("inconsistent legacy Annotation group 7"));
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
    assert_eq!(filters.alignment_sort_anchor, None);
    assert_eq!(filters.read_stacking_mode, ReadStackingMode::Stable);
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
    // Weak single-read junctions are hidden by default since the Sashimi redesign.
    assert_eq!(filters.minimum_junction_depth, 2);
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
fn invalid_alignment_predicate_contracts_are_rejected() {
    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    });
    let filters = &mut add_test_alignment(&mut document).filters;
    filters.alignment_view_filter.version = 2;
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentPredicate)
    ));

    {
        let filters = &mut document.alignment_tracks[0].filters;
        filters.alignment_view_filter.version = 1;
        filters.alignment_view_filter.all = vec![AlignmentViewPredicate::CigarEvent {
            event: AlignmentCigarEventPredicate::Deletion,
            scope: AlignmentPredicateScope::Interval {
                contig: "chr1".to_owned(),
                start: 20,
                end: 20,
            },
            minimum_length: Some(10),
            maximum_length: Some(2),
        }];
    }
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentPredicate)
    ));

    document.alignment_tracks[0]
        .filters
        .alignment_view_filter
        .all = vec![AlignmentViewPredicate::Substitution {
        scope: AlignmentPredicateScope::Anywhere,
        observed_base: Some(b'!'),
        minimum_count: None,
        maximum_count: None,
    }];
    assert!(matches!(
        document.to_json(),
        Err(SessionError::InvalidAlignmentPredicate)
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
    assert_eq!(document.presentation.pane_tracks.len(), 8);
    assert_eq!(document.presentation.read_pane_height, 300.0);
    assert_eq!(document.alignment_tracks[0].height, 360.0);
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
    assert!(
        track.visible,
        "loaded source availability is independent of its eye"
    );
    assert!(
        !imported
            .document
            .presentation
            .pane_track_state(TrackChannel::Annotation, track.id)
            .unwrap()
            .content_visible
    );
    assert!((track.height - 360.0).abs() < f32::EPSILON);
    assert!((imported.document.presentation.read_pane_height - 100.0).abs() < f32::EPSILON);
    let read_node = imported
        .document
        .presentation
        .pane_track_state(TrackChannel::Reads, track.id)
        .unwrap();
    assert_eq!(read_node.preferred_height, Some(40.0));
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
    let error =
        SessionDocument::from_json(include_str!("../../../validation/future-session-v99.json"))
            .unwrap_err();
    assert!(matches!(
        error,
        SessionError::WrittenByNewerVersion {
            schema_version: 99,
            detail: None,
            ..
        }
    ));
    assert!(error.to_string().contains("written by a newer RGV"));
    assert!(matches!(
        SessionDocument::from_json(r#"{"schema_version":0}"#),
        Err(SessionError::UnsupportedVersion(0))
    ));
}

fn minimal_session() -> SessionDocument {
    SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 0,
        end: 100,
    })
}

#[test]
fn sessions_record_their_writer_and_explain_newer_revisions() {
    let json = minimal_session().to_json().unwrap();
    let value = serde_json::from_str::<serde_json::Value>(&json).unwrap();
    assert_eq!(value["writer"]["application"], "rgv");
    assert_eq!(value["writer"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        value["writer"]["schema_minor_version"],
        SESSION_SCHEMA_MINOR_VERSION
    );
    assert_eq!(value["schema_version"], SESSION_SCHEMA_VERSION);
    // The document's own keys still lead the file and it round-trips cleanly.
    assert!(json.starts_with("{\n  \"schema_version\""));
    let reread = SessionDocument::from_json(&json).unwrap();
    assert!(reread.migration_notices.is_empty());
    assert_eq!(reread.to_json().unwrap(), json);

    // Files from before writer metadata existed still open silently.
    let mut legacy = value.clone();
    legacy.as_object_mut().unwrap().remove("writer");
    let legacy = SessionDocument::from_json(&legacy.to_string()).unwrap();
    assert!(legacy.migration_notices.is_empty());

    // Same schema, newer revision, parseable: opened with a data-loss notice.
    let mut newer = value.clone();
    newer["writer"]["schema_minor_version"] = serde_json::json!(SESSION_SCHEMA_MINOR_VERSION + 1);
    newer["writer"]["version"] = serde_json::json!("9.9.9");
    newer["a_field_from_the_future"] = serde_json::json!({"x": 1});
    let opened = SessionDocument::from_json(&newer.to_string()).unwrap();
    assert_eq!(opened.migration_notices.len(), 1);
    assert!(opened.migration_notices[0].contains("newer RGV (rgv 9.9.9"));
    assert_eq!(
        opened.newer_writer_notice(),
        Some(opened.migration_notices[0].as_str())
    );
    assert_eq!(minimal_session().newer_writer_notice(), None);
    assert_eq!(
        crate::newer_writer_notice(&["Presentation was migrated".to_owned()]),
        None
    );
    assert!(opened.migration_notices[0].contains("lost if the project is saved"));

    // Same schema, newer revision, unknown enum variant: a named error, not raw serde.
    newer["reference_genetic_code"] = serde_json::json!("a_code_from_the_future");
    let error = SessionDocument::from_json(&newer.to_string()).unwrap_err();
    assert!(
        matches!(
            &error,
            SessionError::WrittenByNewerVersion {
                schema_version: SESSION_SCHEMA_VERSION,
                detail: Some(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("rgv 9.9.9"));

    // The same damage without a newer writer is still reported as JSON damage.
    newer["writer"]["schema_minor_version"] = serde_json::json!(SESSION_SCHEMA_MINOR_VERSION);
    assert!(matches!(
        SessionDocument::from_json(&newer.to_string()),
        Err(SessionError::Json(_))
    ));
}

#[test]
fn legacy_groups_with_separated_members_are_made_contiguous() {
    let mut legacy = serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../validation/multi-panel-four-track-session-v3.json"
    ))
    .unwrap();
    // Tracks 0 and 2 share a group in both panes; 3 and 1 share another (reads only).
    for (index, group, read_group) in [
        (0, Some(7), Some(7)),
        (2, Some(7), Some(7)),
        (3, None, Some(9)),
        (1, None, Some(9)),
    ] {
        let track = &mut legacy["alignment_tracks"][index];
        track["track_group"] = serde_json::json!(group);
        track["read_track_group"] = serde_json::json!(read_group);
    }
    let ids = legacy["alignment_tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| TrackId::new(track["id"].as_u64().unwrap()))
        .collect::<Vec<_>>();

    let document = SessionDocument::from_json(&serde_json::to_string(&legacy).unwrap()).unwrap();
    let pane = |channel| {
        document
            .presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == channel)
            .map(|node| node.track_id)
            .take(4)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pane(TrackChannel::Annotation),
        [ids[0], ids[2], ids[1], ids[3]]
    );
    assert_eq!(pane(TrackChannel::Reads), [ids[0], ids[2], ids[1], ids[3]]);
    assert_eq!(document.presentation.groups.len(), 3);
    for group in &document.presentation.groups {
        assert_eq!(group.members.len(), 2);
    }
    assert_eq!(
        document
            .migration_notices
            .iter()
            .filter(|notice| notice.contains("were not adjacent"))
            .count(),
        3,
        "{:?}",
        document.migration_notices
    );
    // The repaired document is stable under a save and reload.
    let reread = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(reread.presentation, document.presentation);
}

#[test]
fn session_tracks_are_capped_and_aliases_may_contain_colons() {
    let mut document = minimal_session();
    document
        .contig_aliases
        .insert("HLA-A:01".to_owned(), "HLA-A*01:01:01:01".to_owned());
    let reread = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    assert_eq!(reread.contig_aliases, document.contig_aliases);
    for invalid in ["has space", "tab\tbed", "line\nfeed", "bell\u{7}"] {
        let mut document = minimal_session();
        document
            .contig_aliases
            .insert(invalid.to_owned(), "chr1".to_owned());
        assert!(
            matches!(document.to_json(), Err(SessionError::InvalidContigAliases)),
            "{invalid:?}"
        );
    }

    let track = |id: u64| {
        TrackConfig::new(
            TrackId::new(id),
            format!("track {id}"),
            TrackKind::Feature,
            TrackSource::Local {
                path: format!("{id}.bed"),
            },
        )
    };
    let mut document = minimal_session();
    document.tracks = (1..=MAXIMUM_SESSION_TRACKS as u64).map(track).collect();
    assert!(document.to_json().is_ok());
    document
        .tracks
        .push(track(MAXIMUM_SESSION_TRACKS as u64 + 1));
    assert!(matches!(
        document.to_json(),
        Err(SessionError::TooManyTracks)
    ));
}

#[test]
fn igv_import_tolerates_loci_rgv_cannot_place() {
    let import = |locus: &str| {
        SessionDocument::import_igv_with_report(&format!(
            r#"<Session genome="hg38" {locus}><Resources><Resource path="reads.bam"/></Resources></Session>"#
        ))
        .unwrap()
    };
    let locus = |import: &IgvSessionImport| {
        let locus = &import.document.locus;
        (locus.contig.clone(), locus.start, locus.end)
    };

    for (attribute, requested) in [
        (r#"locus="All""#, "All"),
        (r#"locus="all""#, "all"),
        ("", ""),
        (r#"locus="chr1:0-10""#, "chr1:0-10"),
    ] {
        let imported = import(attribute);
        assert_eq!(
            imported.locus_resolution,
            IgvLocusResolution::Unresolved {
                requested: requested.to_owned()
            },
            "{attribute}"
        );
        assert_eq!(imported.document.alignment_tracks.len(), 1);
        assert!(
            imported
                .report
                .approximated
                .iter()
                .chain(&imported.report.ignored)
                .any(|notice| notice.contains("default locus")),
            "{attribute}: {:?}",
            imported.report
        );
    }

    for name in ["chr1", "BRCA1", "HLA-DRB1*15:01:01:01N"] {
        let imported = import(&format!(r#"locus="{name}""#));
        assert_eq!(
            imported.locus_resolution,
            IgvLocusResolution::Named {
                name: name.to_owned()
            }
        );
        assert_eq!(locus(&imported), (name.to_owned(), 0, 1));
    }

    for (text, expected) in [
        ("chr1:101-200", ("chr1", 100, 200)),
        ("chr1:1,001-2,000", ("chr1", 1_000, 2_000)),
        ("HLA-A*01:01:01:01:101-200", ("HLA-A*01:01:01:01", 100, 200)),
        (
            "chr1:5,000,000,001-5,000,000,100",
            ("chr1", 5_000_000_000, 5_000_000_100),
        ),
        ("chr2:1000", ("chr2", 979, 1_020)),
        ("chr3 11 20", ("chr3", 10, 20)),
    ] {
        let imported = import(&format!(r#"locus="{text}""#));
        assert_eq!(
            imported.locus_resolution,
            IgvLocusResolution::Interval,
            "{text}"
        );
        assert_eq!(
            locus(&imported),
            (expected.0.to_owned(), expected.1, expected.2),
            "{text}"
        );
    }

    let several = import(r#"locus="chr1:101-200 chr2:301-400""#);
    assert_eq!(several.locus_resolution, IgvLocusResolution::Interval);
    assert_eq!(locus(&several), ("chr1".to_owned(), 100, 200));
    assert!(
        several
            .report
            .approximated
            .iter()
            .any(|notice| notice.contains("several loci"))
    );
}

#[test]
fn igv_import_does_not_treat_url_schemes_as_local_paths() {
    let xml = r#"<Session locus="chr1:1-10"><Resources>
        <Resource path="s3://bucket/a.bam"/>
        <Resource path="gs://bucket/b.cram"/>
        <Resource path="ftp://example.org/c.bw"/>
        <Resource path="file:///data/with%20space/d.bed"/>
        <Resource path="https://example.org/e.bw"/>
        <Resource path="C:\data\f.bed"/>
        <Resource path="relative/g.bed"/>
    </Resources></Session>"#;
    let imported = SessionDocument::import_igv_with_report(xml).unwrap();
    let document = &imported.document;
    let remote = |url: &str| TrackSource::Remote {
        url: url.to_owned(),
    };
    let local = |path: &str| TrackSource::Local {
        path: path.to_owned(),
    };
    assert_eq!(
        document.alignment_tracks[0].source,
        remote("s3://bucket/a.bam")
    );
    assert_eq!(
        document.alignment_tracks[1].source,
        remote("gs://bucket/b.cram")
    );
    assert_eq!(document.tracks[0].source, remote("ftp://example.org/c.bw"));
    #[cfg(unix)]
    assert_eq!(document.tracks[1].source, local("/data/with space/d.bed"));
    assert_eq!(
        document.tracks[2].source,
        remote("https://example.org/e.bw")
    );
    assert_eq!(document.tracks[3].source, local("C:\\data\\f.bed"));
    assert_eq!(document.tracks[4].source, local("relative/g.bed"));
    assert!(
        imported
            .report
            .approximated
            .iter()
            .any(|notice| notice.contains("remote scheme RGV cannot open yet"))
    );

    // Remote sources are never rebased onto the project directory.
    let mut resolved = document.clone();
    resolved.resolve_project_paths("/project");
    assert_eq!(
        resolved.alignment_tracks[0].source,
        remote("s3://bucket/a.bam")
    );
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

    document.normalize_presentation();
    document.write(&session_path).unwrap();

    let canonical_reads = reads.canonicalize().unwrap();
    let canonical_reference = reference.canonicalize().unwrap();
    let canonical_outside = outside.path().canonicalize().unwrap();
    let stored = std::fs::read_to_string(&session_path).unwrap();
    // The session is JSON, where a Windows path's backslashes are escaped.
    let in_json = |text: &str| {
        let quoted = serde_json::to_string(text).unwrap();
        quoted[1..quoted.len() - 1].to_owned()
    };
    let separator = std::path::MAIN_SEPARATOR;
    assert!(stored.contains(&in_json(&format!("data{separator}reads.bam"))));
    assert!(stored.contains(&in_json(&format!("data{separator}reference.fa"))));
    assert!(stored.contains(&in_json(&canonical_outside.display().to_string())));

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
fn modification_display_defaults_to_modified_only_and_round_trips() {
    // A session saved before the setting existed, with the old by-type default: it loads
    // modified-only, plotting 5mC in the lane.
    let json = r#"{"schema_version":1,"locus":{"contig":"chr1","start":0,"end":1},"alignment":{"Local":{"path":"reads.bam"}},"reference":null,"tracks":[],"filters":{"minimum_mapping_quality":null,"include_secondary":true,"include_supplementary":true,"auxiliary_predicates":[],"show_modified_bases":true,"modified_base_color_mode":"by_type"},"maximum_alignments":1}"#;
    let document = SessionDocument::from_json(json).unwrap();
    let filters = &document.alignment_tracks[0].filters;
    assert!(!filters.show_unmodified_calls);
    assert!(!filters.draws_unmodified_calls());
    assert_eq!(filters.modification_lane_code, None);
    assert!(!SessionFilters::default().draws_unmodified_calls());

    let mut document = SessionDocument::new(SessionLocus {
        contig: "chr1".to_owned(),
        start: 100,
        end: 200,
    });
    let filters = &mut add_test_alignment(&mut document).filters;
    filters.set_draws_unmodified_calls(true);
    filters.modification_lane_code = Some("h".to_owned());
    let reopened = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
    let filters = &reopened.alignment_tracks[0].filters;
    assert!(filters.show_unmodified_calls);
    assert!(filters.draws_unmodified_calls());
    assert_eq!(filters.modification_lane_code.as_deref(), Some("h"));

    // The legacy modified-versus-unmodified colouring always drew unmodified calls; switching
    // the 2-colour display off leaves it for the single colour.
    let mut legacy = SessionFilters {
        modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
        ..SessionFilters::default()
    };
    assert!(legacy.draws_unmodified_calls());
    legacy.set_draws_unmodified_calls(false);
    assert!(!legacy.draws_unmodified_calls());
    assert_eq!(legacy.modified_base_color_mode, ModifiedBaseColorMode::Mono);
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
#[test]
fn igv_notices_truncate_at_utf8_boundaries_and_still_deduplicate() {
    let mut notices = std::collections::BTreeSet::new();
    let mut omitted = 0;
    for text in [
        "a".repeat(512),
        "a".repeat(600),
        format!("a{}", "é".repeat(300)),
    ] {
        super::insert_igv_notice(&mut notices, &mut omitted, text.clone());
        super::insert_igv_notice(&mut notices, &mut omitted, text);
    }
    assert_eq!(notices.len(), 2);
    assert_eq!(omitted, 0);
    assert!(notices.iter().all(|notice| notice.len() <= 512));
    let xml = format!(
        "<Session locus=\"chr1:1-100\"><{}/></Session>",
        "é".repeat(300)
    );
    let imported = super::SessionDocument::import_igv_with_report(&xml).unwrap();
    assert!(!imported.report.ignored.is_empty());
    assert!(
        imported
            .report
            .ignored
            .iter()
            .all(|notice| notice.len() <= 512)
    );
}

#[test]
fn sashimi_presentation_settings_round_trip_and_default_for_older_projects() {
    let filters = SessionFilters {
        minimum_junction_depth: 5,
        junction_relative_threshold_percent: 2.5,
        show_all_junctions: true,
        junction_library_strandedness: LibraryStrandedness::FirstOfPairAntisense,
        sashimi_label_mode: SashimiLabelMode::Psi,
        ..SessionFilters::default()
    };
    let json = serde_json::to_value(&filters).unwrap();
    assert_eq!(
        json["junction_library_strandedness"],
        "first_of_pair_antisense"
    );
    assert_eq!(json["sashimi_label_mode"], "psi");
    let restored: SessionFilters = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(restored, filters);
    assert!(validate_filters(&restored).is_ok());

    // A project written before these settings existed opens with the defaults: weak junctions
    // (fewer than 2 reads, or under 1 % of a shared site) hidden, unstranded, count labels.
    let mut legacy = json;
    for key in [
        "minimum_junction_depth",
        "junction_relative_threshold_percent",
        "show_all_junctions",
        "junction_library_strandedness",
        "sashimi_label_mode",
    ] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let legacy: SessionFilters = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.minimum_junction_depth, 2);
    assert!((legacy.junction_relative_threshold_percent - 1.0).abs() < f32::EPSILON);
    assert!(!legacy.show_all_junctions);
    assert_eq!(
        legacy.junction_library_strandedness,
        LibraryStrandedness::Unstranded
    );
    assert_eq!(legacy.sashimi_label_mode, SashimiLabelMode::Counts);

    let invalid = SessionFilters {
        junction_relative_threshold_percent: 150.0,
        ..SessionFilters::default()
    };
    assert!(validate_filters(&invalid).is_err());
}
