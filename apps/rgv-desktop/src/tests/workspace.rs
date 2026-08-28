use super::super::*;

#[test]
fn overview_resolution_is_index_only_and_regional_resolution_requests_records() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap();
    let overview =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let regional =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_001.0).unwrap());

    assert_eq!(overview.detail, DetailLevel::Overview);
    assert!(!overview.requires_alignment_records());
    assert_eq!(regional.detail, DetailLevel::Regional);
    assert!(regional.requires_alignment_records());
}

#[test]
fn index_only_overview_settles_without_retaining_alignment_query_state() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap();
    let resolution =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let mut projection = AlignmentPanelProjection {
        pending_query: true,
        unavailable: Some("stale failure".to_owned()),
        query_stats: Some(rgv_hts::QueryStats::default()),
        ..AlignmentPanelProjection::default()
    };

    projection.settle_index_only_overview(resolution);

    assert!(!projection.pending_query);
    assert!(projection.current_query.is_none());
    assert!(projection.query_stats.is_none());
    assert!(projection.alignment_rows.is_none());
    assert!(projection.cached_layout.is_none());
    assert!(projection.unavailable.is_none());
    assert!(projection.index_only_overview);
    assert_eq!(projection.last_query_resolution, Some(resolution));
    assert_eq!(
        projection.settled_generation,
        Some(projection.clock.current().get())
    );
}

#[test]
fn shell_resize_refreshes_only_after_crossing_a_query_resolution_boundary() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
    let previous =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let same_bucket =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_020.0).unwrap());
    let next_bucket =
        AlignmentQueryResolution::for_viewport(Viewport::new(interval, 1_090.0).unwrap());

    assert!(!same_bucket.requires_refresh_from(previous));
    assert!(next_bucket.requires_refresh_from(previous));

    let read_interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
    let allele_exact =
        AlignmentQueryResolution::for_viewport(Viewport::new(read_interval, 600.0).unwrap());
    let aggregated =
        AlignmentQueryResolution::for_viewport(Viewport::new(read_interval, 400.0).unwrap());
    assert!(aggregated.requires_refresh_from(allele_exact));
}

#[test]
fn coverage_alleles_are_collected_at_read_detail_with_exact_bounded_bins() {
    let (bins, collect_alleles) = alignment_coverage_query_plan(1_060, 1_390.0, 0.763);
    assert_eq!(bins, 1_060);
    assert!(collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(
        MAXIMUM_COVERAGE_BINS + 1,
        5_000.0,
        MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL,
    );
    assert_eq!(bins, MAXIMUM_COVERAGE_BINS);
    assert!(!collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(1_930, 1_390.0, 1.39);
    assert_eq!(bins, 1_930);
    assert!(collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(2_800, 1_390.0, 2.01);
    assert_eq!(bins, 1_390);
    assert!(!collect_alleles);
}

#[test]
fn reference_aware_read_modes_request_context_before_base_detail() {
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Bases,
        0.125,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Reads,
        MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Reads,
        MAXIMUM_BASE_MARK_BASES_PER_PIXEL + 0.01,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        DetailLevel::Regional,
        10.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsForwardIpd),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
        DetailLevel::Regional,
        10.0,
    ));
}

#[test]
fn local_control_is_disabled_by_default_and_requires_a_secret_file() {
    let default = LaunchOptions::try_parse_from(["rgv-desktop"]).unwrap();
    assert!(default.control_port.is_none());
    assert!(LaunchOptions::try_parse_from(["rgv-desktop", "--control-port", "0"]).is_err());
    let enabled = LaunchOptions::try_parse_from([
        "rgv-desktop",
        "--control-port",
        "0",
        "--control-secret-file",
        "control.secret",
        "--control-root",
        "workspace",
    ])
    .unwrap();
    assert_eq!(enabled.control_port, Some(0));
}

#[test]
fn keyboard_evidence_navigation_wraps_both_directions_and_handles_empty_tracks() {
    assert_eq!(next_evidence_index(None, 0, true), None);
    assert_eq!(next_evidence_index(None, 3, true), Some(0));
    assert_eq!(next_evidence_index(Some(0), 3, true), Some(1));
    assert_eq!(next_evidence_index(Some(2), 3, true), Some(0));
    assert_eq!(next_evidence_index(None, 3, false), Some(2));
    assert_eq!(next_evidence_index(Some(0), 3, false), Some(2));
}

fn test_panel(id: u64) -> ViewPanelController {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    ViewPanelController::new(
        PanelId::new(id),
        Viewport::new(interval, 1_000.0).unwrap(),
        None,
        "chr1".to_owned(),
        1_000_000,
    )
}

fn test_alignment_query(start: u64) -> Arc<AlignmentQuery> {
    Arc::new(AlignmentQuery {
        interval: GenomicInterval::new(ContigId::new(0), start, start + 10).unwrap(),
        header: Arc::default(),
        batch: rgv_hts::AlignmentBatch::default(),
        pileup: rgv_hts::PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats::default(),
    })
}

#[test]
fn auxiliary_removal_clears_every_track_owned_panel_reference() {
    let removed_id = TrackId::new(5);
    let surviving_id = TrackId::new(6);
    let mut panel = test_panel(1);
    panel.track_snapshots = vec![
        TrackSnapshot {
            id: removed_id,
            name: "removed".to_owned(),
            data: TrackSnapshotData::Features(Vec::new()),
        },
        TrackSnapshot {
            id: surviving_id,
            name: "surviving".to_owned(),
            data: TrackSnapshotData::Features(Vec::new()),
        },
    ];
    for id in [removed_id, surviving_id] {
        panel.track_frames.push(TrackFrame {
            key: TrackFrameKey::Auxiliary(id),
            channel: ui::TrackPane::Annotation,
            name: format!("track-{id:?}"),
            kind: TrackKind::Feature,
            source_label: None,
            top: 0.0,
            bottom: 40.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        });
    }
    panel.selected_feature = Some(FeatureSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_variant = Some(VariantSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_gwas = Some(GwasSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_quantitative = Some(QuantitativeSelection {
        track_id: removed_id,
        record_index: 0,
        position: 100,
    });
    panel.selected_copy_number = Some(CopyNumberSelection {
        track_id: removed_id,
        segment_index: 0,
    });
    panel.selected_mutation = Some(MutationSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_interaction = Some(InteractionSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.hovered_record = Some((removed_id, 0));
    panel.hover_inspector_target = Some(HoverInspectorTarget::Record {
        track_id: removed_id,
        index: 0,
        operation_index: None,
        operation_offset: None,
    });
    panel.selected_track = Some(TrackFrameKey::Auxiliary(removed_id));
    panel.track_settled_generation = Some(panel.generation_clock.current().get());

    clear_removed_auxiliary_panel_state(&mut panel, &HashSet::from([removed_id]));

    assert_eq!(
        panel
            .track_snapshots
            .iter()
            .map(|snapshot| snapshot.id)
            .collect::<Vec<_>>(),
        [surviving_id]
    );
    assert_eq!(
        panel
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Auxiliary(surviving_id)]
    );
    assert!(panel.selected_feature.is_none());
    assert!(panel.selected_variant.is_none());
    assert!(panel.selected_gwas.is_none());
    assert!(panel.selected_quantitative.is_none());
    assert!(panel.selected_copy_number.is_none());
    assert!(panel.selected_mutation.is_none());
    assert!(panel.selected_interaction.is_none());
    assert!(panel.hovered_record.is_none());
    assert!(panel.hover_inspector_target.is_none());
    assert!(panel.selected_track.is_none());
    assert!(panel.track_settled_generation.is_none());
}

#[test]
fn auxiliary_removal_disables_arithmetic_that_depended_on_the_track() {
    let track = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bedGraph"),
            },
        )
    };
    let mut tracks = vec![track(5), track(6), track(7)];
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Add;
    tracks[1].quantitative.operand_track = Some(TrackId::new(5));
    tracks[2].quantitative.arithmetic = QuantitativeArithmetic::Subtract;
    tracks[2].quantitative.operand_track = Some(TrackId::new(6));

    assert_eq!(
        clear_removed_quantitative_dependencies(&mut tracks, &HashSet::from([TrackId::new(5)]),),
        1
    );
    assert_eq!(
        tracks[1].quantitative.arithmetic,
        QuantitativeArithmetic::None
    );
    assert_eq!(tracks[1].quantitative.operand_track, None);
    assert_eq!(
        tracks[2].quantitative.arithmetic,
        QuantitativeArithmetic::Subtract
    );
    assert_eq!(tracks[2].quantitative.operand_track, Some(TrackId::new(6)));
}

#[test]
fn auxiliary_queries_follow_source_lifecycle_not_presentation_eyes() {
    let quantitative = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bedGraph"),
            },
        )
    };
    let mut tracks = vec![
        quantitative(2),
        quantitative(1),
        TrackConfig::new(
            TrackId::new(3),
            "hidden features".to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: "hidden.bed".to_owned(),
            },
        ),
        TrackConfig::new(
            TrackId::new(4),
            "visible variants".to_owned(),
            TrackKind::Variant,
            TrackSource::Local {
                path: "visible.vcf".to_owned(),
            },
        ),
    ];
    tracks[0].visible = false;
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Add;
    tracks[1].quantitative.operand_track = Some(TrackId::new(2));
    tracks[2].visible = false;

    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );

    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::None;
    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );

    tracks[1].visible = false;
    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );
}

#[test]
fn alignment_removal_clears_deleted_identity_from_every_panel() {
    let removed = TrackId::new(5);
    let surviving = TrackId::new(6);
    let mut selected = test_panel(1);
    selected.selected_alignment_track = Some(removed);
    selected.selected_alignment_tracks = vec![surviving, removed];
    selected.selected_read_tracks = vec![surviving, removed];
    selected.read_selection_anchor = Some(removed);
    selected.selected_record = Some(3);
    selected.selected_read_event = Some(ReadEventSelection {
        record_index: 3,
        operation_index: 1,
        operation_offset: None,
    });
    selected.selected_pileup_position = Some(120);
    selected.selected_modification_position = Some(121);
    selected.selected_junction = Some((130, 150));
    selected.selected_chain = Some(SelectedSupplementaryChain {
        read_name: b"removed-read".to_vec(),
        segments: Vec::new(),
        active_segment: 0,
        malformed_entries: 0,
        truncated: false,
        invalid_tag_type: false,
    });
    selected.pending_read_selection = Some((removed, b"pending-read".to_vec()));
    selected.hovered_record = Some((removed, 3));
    selected.hover_inspector_target = Some(HoverInspectorTarget::Pileup(removed, 120));
    selected.selected_track = Some(TrackFrameKey::Alignment(removed));

    let mut unaffected = test_panel(2);
    unaffected.selected_alignment_track = Some(surviving);
    unaffected.selected_alignment_tracks = vec![removed, surviving];
    unaffected.selected_read_tracks = vec![removed, surviving];
    unaffected.read_selection_anchor = Some(surviving);
    unaffected.selected_record = Some(7);
    unaffected.selected_track = Some(TrackFrameKey::Alignment(surviving));

    for panel in [&mut selected, &mut unaffected] {
        for id in [removed, surviving] {
            panel.track_frames.push(TrackFrame {
                key: TrackFrameKey::Alignment(id),
                channel: ui::TrackPane::Annotation,
                name: format!("reads-{id:?}"),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 0.0,
                bottom: 80.0,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: Vec::new(),
            });
        }
        clear_removed_alignment_panel_state(panel, removed, true);
    }

    assert_eq!(selected.selected_alignment_track, None);
    assert_eq!(selected.selected_alignment_tracks, [surviving]);
    assert_eq!(selected.selected_read_tracks, [surviving]);
    assert_eq!(selected.read_selection_anchor, Some(surviving));
    assert!(selected.selected_record.is_none());
    assert!(selected.selected_read_event.is_none());
    assert!(selected.selected_pileup_position.is_none());
    assert!(selected.selected_modification_position.is_none());
    assert!(selected.selected_junction.is_none());
    assert!(selected.selected_chain.is_none());
    assert!(selected.pending_read_selection.is_none());
    assert!(selected.hovered_record.is_none());
    assert!(selected.hover_inspector_target.is_none());
    assert_eq!(
        selected.selected_track,
        Some(TrackFrameKey::Alignment(surviving))
    );
    assert_eq!(
        selected
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Alignment(surviving)]
    );

    assert_eq!(unaffected.selected_alignment_track, Some(surviving));
    assert_eq!(unaffected.selected_alignment_tracks, [surviving]);
    assert_eq!(unaffected.selected_read_tracks, [surviving]);
    assert_eq!(unaffected.read_selection_anchor, Some(surviving));
    assert_eq!(unaffected.selected_record, Some(7));
    assert_eq!(
        unaffected.selected_track,
        Some(TrackFrameKey::Alignment(surviving))
    );
    assert_eq!(
        unaffected
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Alignment(surviving)]
    );
}

#[test]
fn alignment_projections_keep_each_panel_query_independent() {
    let first_id = PanelId::new(1);
    let second_id = PanelId::new(2);
    let first_query = test_alignment_query(100);
    let second_query = test_alignment_query(200);
    let mut projections = alignment_panel_projections(&[first_id, second_id]);
    projections.get_mut(&first_id).unwrap().current_query = Some(Arc::clone(&first_query));
    projections.get_mut(&second_id).unwrap().current_query = Some(Arc::clone(&second_query));

    assert!(Arc::ptr_eq(
        projections
            .get(&first_id)
            .unwrap()
            .current_query
            .as_ref()
            .unwrap(),
        &first_query,
    ));
    assert!(Arc::ptr_eq(
        projections
            .get(&second_id)
            .unwrap()
            .current_query
            .as_ref()
            .unwrap(),
        &second_query,
    ));
}

fn two_panel_test_intervals(
    dictionary: &ReferenceDictionary,
    panel_ids: [PanelId; 2],
) -> [(PanelId, GenomicInterval); 2] {
    [
        (
            panel_ids[0],
            GenomicInterval::new(dictionary.id_by_name("chr1").unwrap(), 900_000, 920_000).unwrap(),
        ),
        (
            panel_ids[1],
            GenomicInterval::new(dictionary.id_by_name("chr2").unwrap(), 0, 200_000).unwrap(),
        ),
    ]
}

#[test]
fn panel_accepts_only_its_current_generation() {
    let first = test_panel(1);
    let second = test_panel(2);
    let current = first.request();

    assert!(first.accepts(current));
    assert!(!second.accepts(current));
    first.generation_clock.advance();
    assert!(!first.accepts(current));
    assert!(first.accepts(first.request()));
}

#[test]
fn review_navigation_follows_queue_order_wraps_and_honours_filter() {
    let item = |id, contig: &str, start, status| ReviewItem {
        id,
        label: format!("item-{id}"),
        locus: SessionLocus {
            contig: contig.to_owned(),
            start,
            end: start + 10,
        },
        status,
        notes: String::new(),
        assignment: None,
    };
    let items = vec![
        item(1, "chr1", 100, ReviewStatus::Pending),
        item(2, "chr2", 200, ReviewStatus::Complete),
        item(3, "chr3", 300, ReviewStatus::Pending),
    ];

    assert_eq!(
        adjacent_review_locus(&items, None, "chr1", 100, 110, true)
            .unwrap()
            .contig,
        "chr2"
    );
    assert_eq!(
        adjacent_review_locus(&items, None, "chr1", 100, 110, false)
            .unwrap()
            .contig,
        "chr3"
    );
    assert_eq!(
        adjacent_review_locus(&items, Some(ReviewStatus::Pending), "chr3", 300, 310, true,)
            .unwrap()
            .contig,
        "chr1"
    );
    assert!(
        adjacent_review_locus(&items, Some(ReviewStatus::Skipped), "chr1", 100, 110, true,)
            .is_none()
    );
}

#[test]
fn imported_regions_receive_unique_ids_and_respect_workspace_capacity() {
    let region = |id, start| RegionOfInterest {
        id,
        name: format!("region-{start}"),
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start,
            end: start + 10,
        },
        color: [1, 2, 3, 48],
    };
    let mut destination = vec![region(1, 0), region(3, 20)];
    let (added, omitted) =
        merge_imported_regions(&mut destination, vec![region(1, 40), region(2, 60)], 3);

    assert_eq!((added, omitted), (1, 1));
    assert_eq!(
        destination.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![1, 3, 2]
    );
}

#[test]
fn metadata_attribute_selection_spans_alignment_and_auxiliary_tracks() {
    let metadata = SampleMetadataTable::from_tsv(concat!(
        "sample\tcondition\n",
        "normal\tcontrol\n",
        "tumour\tcase\n",
    ))
    .unwrap();
    let tracks = vec![
        (
            TrackId::new(1),
            "reads".to_owned(),
            Some("tumour".to_owned()),
            TrackKind::Alignment,
        ),
        (
            TrackId::new(2),
            "signal".to_owned(),
            Some("tumour".to_owned()),
            TrackKind::Quantitative,
        ),
        (
            TrackId::new(3),
            "control".to_owned(),
            Some("normal".to_owned()),
            TrackKind::Variant,
        ),
        (
            TrackId::new(4),
            "unassigned".to_owned(),
            None,
            TrackKind::Feature,
        ),
    ];

    assert_eq!(
        metadata_matching_track_ids(&tracks, &metadata, 0, "case"),
        BTreeSet::from([TrackId::new(1), TrackId::new(2)])
    );
}

#[test]
fn quantitative_bulk_overlay_is_compatible_bounded_and_reversible() {
    let quantitative = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bw"),
            },
        )
    };
    let mut tracks = vec![
        quantitative(1),
        quantitative(2),
        TrackConfig::new(
            TrackId::new(3),
            "variants".to_owned(),
            TrackKind::Variant,
            TrackSource::Local {
                path: "variants.bcf".to_owned(),
            },
        ),
    ];
    let selected = BTreeSet::from([TrackId::new(1), TrackId::new(2), TrackId::new(3)]);

    assert!(apply_quantitative_bulk_action(
        &mut tracks,
        &selected,
        QuantitativeBulkAction::Overlay,
    ));
    assert_eq!(tracks[0].quantitative.overlay_group, Some(1));
    assert_eq!(tracks[1].quantitative.overlay_group, Some(1));
    assert_eq!(tracks[2].quantitative.overlay_group, None);
    tracks[0].quantitative.positive_color = [1, 2, 3, 4];
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Subtract;
    tracks[1].quantitative.operand_track = Some(TrackId::new(1));
    let quantitative_selected = BTreeSet::from([TrackId::new(1), TrackId::new(2)]);
    assert!(apply_compatible_auxiliary_appearance(
        &mut tracks,
        &quantitative_selected,
    ));
    assert_eq!(tracks[1].quantitative.positive_color, [1, 2, 3, 4]);
    assert_eq!(
        tracks[1].quantitative.arithmetic,
        QuantitativeArithmetic::Subtract
    );
    assert_eq!(tracks[1].quantitative.operand_track, Some(TrackId::new(1)));
    assert_eq!(tracks[1].quantitative.overlay_group, Some(1));
    assert!(apply_quantitative_bulk_action(
        &mut tracks,
        &selected,
        QuantitativeBulkAction::Separate,
    ));
    assert_eq!(tracks[0].quantitative.overlay_group, None);
    assert_eq!(
        tracks[0].quantitative.scale_mode,
        QuantitativeScaleMode::Automatic
    );
}

#[test]
fn region_overlay_clips_half_open_intervals_to_each_panel_viewport() {
    let viewport = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let region = RegionOfInterest {
        id: 1,
        name: "roi".to_owned(),
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start: 50,
            end: 120,
        },
        color: [1, 2, 3, 48],
    };
    assert_eq!(
        region_overlay_horizontal_span(&region, "chr1", viewport, 1.0, 40.0),
        Some((40.0, 60.0))
    );
    assert_eq!(
        region_overlay_horizontal_span(&region, "chr2", viewport, 1.0, 40.0),
        None
    );
    let touching = RegionOfInterest {
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start: 200,
            end: 210,
        },
        ..region
    };
    assert_eq!(
        region_overlay_horizontal_span(&touching, "chr1", viewport, 1.0, 40.0),
        None
    );
}

#[test]
fn panel_workspace_mutations_preserve_stable_identity_and_active_panel() {
    let mut workspace = PanelWorkspace::new(test_panel(7));
    assert!(workspace.push(test_panel(2)));
    assert!(workspace.push(test_panel(11)));
    assert!(!workspace.push(test_panel(2)));
    assert_eq!(workspace.next_id(), PanelId::new(12));

    assert!(workspace.activate(PanelId::new(2)));
    assert!(workspace.move_panel(PanelId::new(11), 0));
    assert_eq!(
        workspace.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        vec![PanelId::new(11), PanelId::new(7), PanelId::new(2)]
    );
    assert_eq!(workspace.active().id, PanelId::new(2));

    assert!(workspace.remove(PanelId::new(2)).is_some());
    assert_eq!(workspace.active().id, PanelId::new(7));
    assert!(workspace.remove(PanelId::new(11)).is_some());
    assert!(workspace.remove(PanelId::new(7)).is_none());
}

#[test]
fn session_panel_workspace_restores_order_active_locus_and_width() {
    let active_locus = SessionLocus {
        contig: "chr2".to_owned(),
        start: 500,
        end: 800,
    };
    let mut session = SessionDocument::new(active_locus.clone());
    session.panels = vec![
        SessionViewPanel::new(
            PanelId::new(9),
            SessionLocus {
                contig: "chr1".to_owned(),
                start: 100,
                end: 200,
            },
        ),
        SessionViewPanel::new(PanelId::new(4), active_locus),
    ];
    session.panels[0].vertical_scroll_points = 73.0;
    session.panels[1].vertical_scroll_points = 145.0;
    session.panels[0].annotation_scroll_points = 19.0;
    session.panels[1].annotation_scroll_points = 81.0;
    session.panels[0].vertical_content_origin_points = 33.0;
    session.panels[1].vertical_content_origin_points = 105.0;
    session.panels[1].read_panel_states = vec![SessionReadPanelState {
        track_id: TrackId::new(12),
        scroll_points: 210.0,
        content_origin_points: 40.0,
        latent_scroll_fraction: Some(0.6),
        content_bottom_points: 900.0,
        density: Some(3.0),
    }];
    session.active_panel = PanelId::new(4);

    let workspace = PanelWorkspace::from_session(&session, 400.0, 120.0).unwrap();

    assert_eq!(
        workspace.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        vec![PanelId::new(9), PanelId::new(4)]
    );
    assert_eq!(workspace.active().id, PanelId::new(4));
    assert!((workspace.panels[0].vertical_scroll_points - 73.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].vertical_scroll_points - 145.0).abs() < f32::EPSILON);
    assert!((workspace.annotation_scroll_points - 81.0).abs() < f32::EPSILON);
    assert!((workspace.panels[0].vertical_content_origin_points - 33.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].vertical_content_origin_points - 105.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].effective_vertical_scroll_points() - 40.0).abs() < f32::EPSILON);
    assert_eq!(
        workspace.panels[1]
            .read_panel_scrolls
            .get(&TrackId::new(12)),
        Some(&ReadPanelScrollState {
            scroll_points: 210.0,
            content_origin_points: 40.0,
            latent_scroll_fraction: Some(0.6),
            content_bottom_points: 900.0,
            density: Some(3.0),
        })
    );
    assert_eq!(workspace.active().contig_name, "chr2");
    assert_eq!(workspace.active().viewport.interval().start(), 500);
    assert_eq!(workspace.active().viewport.interval().end(), 800);
    assert!((workspace.active().viewport.pixel_width() - 400.0).abs() < f64::EPSILON);
    assert!(
        workspace
            .iter()
            .all(|panel| (panel.content_bottom_points - 120.0).abs() < f32::EPSILON)
    );
}

#[test]
fn every_alignment_controller_projection_is_created_for_every_panel() {
    let ids = [PanelId::new(9), PanelId::new(4), PanelId::new(20)];
    let projections = alignment_panel_projections(&ids);

    assert_eq!(projections.len(), ids.len());
    assert!(ids.iter().all(|id| projections.contains_key(id)));
    assert!(!projections.contains_key(&INITIAL_PANEL_ID));
}

#[test]
fn composite_display_keeps_gpu_identity_only_for_the_active_panel() {
    let mut first = test_panel(7);
    first.display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    first.display_list.rectangles[0].object_id = 41;
    let mut second = test_panel(2);
    second.display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    second.display_list.rectangles[0].object_id = 41;
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    let first_active = composite_panel_display_lists(&workspace);
    assert_eq!(
        first_active
            .rectangles
            .iter()
            .map(|rectangle| rectangle.object_id)
            .collect::<Vec<_>>(),
        vec![41, 0]
    );

    assert!(workspace.activate(PanelId::new(2)));
    let second_active = composite_panel_display_lists(&workspace);
    assert_eq!(
        second_active
            .rectangles
            .iter()
            .map(|rectangle| rectangle.object_id)
            .collect::<Vec<_>>(),
        vec![0, 41]
    );
}

#[test]
fn multi_panel_composite_export_matches_reviewed_golden_hash() {
    let mut first = test_panel(7);
    first.display_list = DisplayList::synthetic_coverage_and_reads_at(200.0, 12, 80, 8.0);
    let mut second = test_panel(2);
    second.display_list = DisplayList::synthetic_coverage_and_reads_at(200.0, 18, 140, 8.0);
    second
        .display_list
        .translate_and_clip_horizontal(200.0, 200.0, 400.0);
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));
    let composite = composite_panel_display_lists(&workspace);
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("multi-panel-first.png");
    let second_path = directory.path().join("multi-panel-second.png");
    export_png(&composite, 400, 200, &first_path).unwrap();
    export_png(&composite, 400, 200, &second_path).unwrap();
    let first_bytes = std::fs::read(&first_path).unwrap();
    assert_eq!(first_bytes, std::fs::read(&second_path).unwrap());
    if let Some(review_path) = std::env::var_os("RGV_MULTI_PANEL_VISUAL_REVIEW_PATH") {
        std::fs::copy(&first_path, review_path).unwrap();
    }
    let hash = first_bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    assert_eq!(
        hash, 561_290_389_687_490_317,
        "update only after reviewing an intentional multi-panel raster change"
    );
}

#[test]
fn multi_panel_annotation_geometry_uses_the_tallest_frame_for_every_row() {
    let alignment_id = TrackId::new(5);
    let auxiliary_id = TrackId::new(6);
    let add_layout = |panel: &mut ViewPanelController| {
        for frame in panel.track_frames.clone() {
            let track_id = match frame.key {
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => track_id,
                TrackFrameKey::Reference | TrackFrameKey::Group { .. } => unreachable!(),
            };
            let row_rect = egui::Rect::from_min_max(
                egui::pos2(0.0, frame.top),
                egui::pos2(100.0, frame.bottom),
            );
            panel.pane_layout.annotation.items.push(ui::PaneItemLayout {
                id: PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id,
                },
                frame,
                row_rect,
                header_rect: egui::Rect::from_min_max(
                    row_rect.min,
                    egui::pos2(row_rect.right(), row_rect.top() + 22.0),
                ),
                content_rect: Some(egui::Rect::from_min_max(
                    egui::pos2(row_rect.left(), row_rect.top() + 22.0),
                    row_rect.max,
                )),
                clip_rect: row_rect,
                desired_content_visible: true,
                effective_content_visible: true,
            });
        }
    };
    let mut first = test_panel(7);
    first.track_frames.push(TrackFrame {
        key: TrackFrameKey::Alignment(alignment_id),
        channel: ui::TrackPane::Annotation,
        name: "Reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 220.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    first.track_frames.push(TrackFrame {
        key: TrackFrameKey::Auxiliary(auxiliary_id),
        channel: ui::TrackPane::Annotation,
        name: "Genes".to_owned(),
        kind: TrackKind::GeneModel,
        source_label: None,
        top: 220.0,
        bottom: 520.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    add_layout(&mut first);
    let mut second = test_panel(2);
    second.track_frames.push(TrackFrame {
        key: TrackFrameKey::Alignment(alignment_id),
        channel: ui::TrackPane::Annotation,
        name: "Reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 460.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    second.track_frames.push(TrackFrame {
        key: TrackFrameKey::Auxiliary(auxiliary_id),
        channel: ui::TrackPane::Annotation,
        name: "Genes".to_owned(),
        kind: TrackKind::GeneModel,
        source_label: None,
        top: 460.0,
        bottom: 540.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    add_layout(&mut second);
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    let shared = maximum_annotation_frame_heights(&workspace);
    assert_eq!(
        shared.get(&PresentationNodeId::Track {
            channel: ui::TrackPane::Annotation,
            track_id: alignment_id,
        }),
        Some(&360.0)
    );
    assert_eq!(
        shared.get(&PresentationNodeId::Track {
            channel: ui::TrackPane::Annotation,
            track_id: auxiliary_id,
        }),
        Some(&300.0)
    );
}

#[test]
fn workspace_annotation_camera_uses_the_range_available_to_every_panel() {
    let cache = |content_bottom, clip_bottom| PanelScrollDisplayCache {
        annotations: DisplayList::default(),
        pinned: DisplayList::default(),
        annotation_overview: ui::ScrollbarOverview::default(),
        annotation_content_bottom: content_bottom,
        annotation_clip_top: 0.0,
        annotation_clip_bottom: clip_bottom,
        supports_fast_scroll: true,
        read_panels: Vec::new(),
    };
    let mut first = test_panel(7);
    first.scroll_display_cache = Some(cache(500.0, 300.0));
    let mut second = test_panel(2);
    second.scroll_display_cache = Some(cache(420.0, 300.0));
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    assert_eq!(workspace.maximum_annotation_scroll_points(), 120.0);
    workspace.annotation_scroll_points = 95.0;
    assert_eq!(workspace.annotation_scroll_points, 95.0);
}

#[test]
fn multi_locus_expression_is_ordered_bounded_and_atomic_to_parse() {
    assert_eq!(
        split_locus_expression("chr2:20-40  chr1:1-10\nchrX:5-8").unwrap(),
        vec!["chr2:20-40", "chr1:1-10", "chrX:5-8"]
    );
    assert!(split_locus_expression("   ").is_err());
    let excessive = (0..=MAXIMUM_VIEW_PANELS)
        .map(|index| format!("chr1:{}-{}", index + 1, index + 2))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(split_locus_expression(&excessive).is_err());
}

#[test]
fn mate_panel_locus_is_centered_bounded_and_requires_a_mapped_pair() {
    let dictionary =
        ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000), ("chr2".to_owned(), 2_000)]);
    let mut record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 150,
        flags: rgv_hts::AlignmentFlags::from_bits(0x1),
        mapping_quality: Some(60),
        mate_contig: Some(ContigId::new(1)),
        mate_start: Some(1_900),
        template_length: 0,
        read_name: b"pair".to_vec(),
        cigar: Vec::new(),
        sequence: SequenceData::Missing,
        auxiliary_fields: Vec::new(),
    };

    assert_eq!(
        mate_locus(&record, &dictionary, 400).as_deref(),
        Some("chr2:1601-2000")
    );
    record.flags = rgv_hts::AlignmentFlags::from_bits(0x9);
    assert!(mate_locus(&record, &dictionary, 400).is_none());
    record.flags = rgv_hts::AlignmentFlags::from_bits(0x1);
    record.mate_start = Some(2_000);
    assert!(mate_locus(&record, &dictionary, 400).is_none());
}

#[test]
fn locus_parser_converts_one_based_inclusive_input() {
    let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
    let (interval, name, length) =
        resolve_initial_locus(&dictionary, &ContigAliases::default(), Some("chr1:101-200"))
            .unwrap();
    assert_eq!(name, "chr1");
    assert_eq!(length, 1_000_000);
    assert_eq!(interval.start(), 100);
    assert_eq!(interval.end(), 200);
}

#[test]
fn locus_parser_rejects_unknown_contigs() {
    let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 100)]);
    assert!(
        resolve_initial_locus(&dictionary, &ContigAliases::default(), Some("chr2:1-10")).is_err()
    );
}

#[test]
fn locus_parser_uses_only_explicit_unambiguous_contig_aliases() {
    let dictionary =
        ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000), ("1".to_owned(), 1_000)]);
    let aliases = parse_contig_aliases(&["one=chr1".to_owned()]).unwrap();
    let (interval, canonical, _) =
        resolve_initial_locus(&dictionary, &aliases, Some("one:11-20")).unwrap();
    assert_eq!(canonical, "chr1");
    assert_eq!((interval.start(), interval.end()), (10, 20));

    let conflicting = parse_contig_aliases(&["1=chr1".to_owned()]).unwrap();
    assert!(resolve_initial_locus(&dictionary, &conflicting, Some("1:1-10")).is_err());
    assert!(parse_contig_aliases(&["one=chr1".to_owned(), "one=1".to_owned()]).is_err());
}

#[test]
fn annotation_search_requires_a_unique_exact_match_before_navigation() {
    let hit = |track_id, matched_term: &str, name: &str, start| TrackSearchHit {
        track_id: TrackId::new(track_id),
        track_name: "Genes".to_owned(),
        matched_term: matched_term.to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start,
            end: start + 100,
            name: Some(name.to_owned()),
            feature_type: Some("gene".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        },
    };
    let exact = hit(1, "brca1", "BRCA1", 100);
    let prefix = hit(1, "brca2", "BRCA2", 300);
    let results = TrackSearchResults {
        query: "brca1".to_owned(),
        hits: vec![exact.clone(), prefix],
        ..TrackSearchResults::default()
    };
    assert_eq!(
        preferred_annotation_search_hit(&results, "BRCA1"),
        Some(&exact)
    );
    assert_eq!(annotation_search_hit_locus(&exact), "chr1:101-200");

    let ambiguous = TrackSearchResults {
        query: "brca1".to_owned(),
        hits: vec![exact.clone(), hit(2, "brca1", "BRCA1", 500)],
        ..TrackSearchResults::default()
    };
    assert_eq!(preferred_annotation_search_hit(&ambiguous, "brca1"), None);
}

#[test]
fn standalone_locus_supports_track_only_sessions() {
    let (interval, name, length) = standalone_locus("chr7:1,001-2,000").unwrap();
    assert_eq!(name, "chr7");
    assert_eq!((interval.start(), interval.end()), (1_000, 2_000));
    assert!(length > interval.end());
}

#[test]
fn primary_alignment_upsert_preserves_deferred_session_tracks() {
    let mut alignment_tracks = vec![
        SessionAlignmentTrack::new(
            TrackId::new(10),
            "Tumour".to_owned(),
            TrackSource::Local {
                path: "tumour.bam".to_owned(),
            },
        ),
        SessionAlignmentTrack::new(
            TrackId::new(11),
            "Normal".to_owned(),
            TrackSource::Local {
                path: "normal.bam".to_owned(),
            },
        ),
    ];
    alignment_tracks[1].filters.minimum_mapping_quality = Some(30);
    let deferred = alignment_tracks[1].clone();
    let source = TrackSource::Local {
        path: "replacement.bam".to_owned(),
    };

    let primary =
        upsert_primary_alignment_config(&mut alignment_tracks, &[], source.clone(), "New").clone();

    assert_eq!(primary.id, TrackId::new(10));
    assert_eq!(primary.name, "New");
    assert_eq!(primary.source, source);
    assert_eq!(alignment_tracks, vec![primary, deferred]);
}

#[test]
fn primary_alignment_upsert_allocates_an_id_after_auxiliary_tracks() {
    let mut alignment_tracks = Vec::new();
    let auxiliary = vec![TrackConfig::new(
        TrackId::new(7),
        "Genes".to_owned(),
        TrackKind::GeneModel,
        TrackSource::Local {
            path: "genes.gtf".to_owned(),
        },
    )];

    let primary = upsert_primary_alignment_config(
        &mut alignment_tracks,
        &auxiliary,
        TrackSource::Local {
            path: "reads.bam".to_owned(),
        },
        "Reads",
    );

    assert_eq!(primary.id, TrackId::new(8));
}

#[test]
fn alignment_workspace_budgets_are_divided_across_live_tracks() {
    let cache_share = alignment_cache_share(320 * 1024 * 1024, 4).get();
    assert_eq!(cache_share, 80 * 1024 * 1024);
    assert!(cache_share.saturating_mul(4) <= 320 * 1024 * 1024);
    assert_eq!(
        allocate_workspace_records(50_000, &[50_000; 4]),
        vec![12_500; 4]
    );
    let constrained = allocate_workspace_records(2, &[10; 4]);
    assert_eq!(constrained, vec![1, 1, 0, 0]);
    assert_eq!(constrained.iter().sum::<usize>(), 2);
    assert_eq!(allocate_workspace_records(10, &[2, 20, 3]), vec![2, 5, 3]);
}

#[test]
fn alignment_ordering_can_promote_or_demote_the_primary_track() {
    let mut order = vec![TrackId::new(1), TrackId::new(2), TrackId::new(3)];

    assert!(move_ordered_item(&mut order, 0, 2));
    assert_eq!(
        order,
        vec![TrackId::new(2), TrackId::new(3), TrackId::new(1)]
    );
    assert!(move_ordered_item(&mut order, 2, 0));
    assert_eq!(
        order,
        vec![TrackId::new(1), TrackId::new(2), TrackId::new(3)]
    );
    assert!(!move_ordered_item(&mut order, 0, 0));
}

#[test]
fn additive_alignment_selection_toggles_stable_track_ids() {
    let first = TrackId::new(1);
    let second = TrackId::new(2);

    let selected = updated_alignment_selection(&[], first, false);
    let selected = updated_alignment_selection(&selected, second, true);
    assert_eq!(selected, vec![first, second]);
    let selected = updated_alignment_selection(&selected, first, true);
    assert_eq!(selected, vec![second]);
    assert_eq!(
        updated_alignment_selection(&selected, first, false),
        vec![first]
    );
}

#[test]
fn shift_alignment_selection_includes_the_complete_ordered_range() {
    let order = (1..=6).map(TrackId::new).collect::<Vec<_>>();

    assert_eq!(
        alignment_range_selection(&order, TrackId::new(2), TrackId::new(5)),
        order[1..=4]
    );
    assert_eq!(
        alignment_range_selection(&order, TrackId::new(5), TrackId::new(2)),
        order[1..=4]
    );
    assert_eq!(
        alignment_range_selection(&order, TrackId::new(99), TrackId::new(3)),
        [TrackId::new(3)]
    );
}

#[test]
fn workspace_locus_is_resolved_against_each_alignment_dictionary() {
    let dictionary =
        ReferenceDictionary::from_contigs([("chrM".to_owned(), 100), ("chr6".to_owned(), 1_000)]);
    let primary_interval = GenomicInterval::new(ContigId::new(9), 100, 200).unwrap();

    let interval = interval_for_track(&dictionary, "chr6", primary_interval).unwrap();
    assert_eq!(interval.contig(), ContigId::new(1));
    assert_eq!(interval.start(), 100);
    assert_eq!(interval.end(), 200);
    assert!(interval_for_track(&dictionary, "chr1", primary_interval).is_none());
}

#[test]
fn every_alignment_track_filter_is_parsed_independently() {
    let tumour = SessionFilters {
        minimum_mapping_quality: Some(30),
        auxiliary_predicates: vec!["RG=tumour".to_owned()],
        ..SessionFilters::default()
    };
    let normal = SessionFilters {
        minimum_mapping_quality: Some(10),
        auxiliary_predicates: vec!["HP=1".to_owned()],
        ..SessionFilters::default()
    };

    let tumour = active_filters_from_session(&tumour).unwrap();
    let normal = active_filters_from_session(&normal).unwrap();

    assert_eq!(tumour.alignment.minimum_mapping_quality, Some(30));
    assert_eq!(normal.alignment.minimum_mapping_quality, Some(10));
    assert_ne!(
        tumour.alignment.fingerprint(),
        normal.alignment.fingerprint()
    );

    let invalid = SessionFilters {
        auxiliary_predicates: vec!["not a predicate".to_owned()],
        ..SessionFilters::default()
    };
    assert!(active_filters_from_session(&invalid).is_err());
}

#[test]
fn full_settings_update_changes_only_the_target_alignment_config() {
    let source = TrackSource::Local {
        path: "normal.bam".to_owned(),
    };
    let mut normal = SessionAlignmentTrack::new(TrackId::new(2), "Normal".to_owned(), source);
    let untouched = SessionAlignmentTrack::new(
        TrackId::new(1),
        "Tumour".to_owned(),
        TrackSource::Local {
            path: "tumour.bam".to_owned(),
        },
    );
    let (mut editor, mut active) = filter_state_from_session(&normal.filters).unwrap();
    let mut filters = normal.filters.clone();
    filters.minimum_allele_fraction = 0.07;
    filters.alignment_color_mode = AlignmentColorMode::Haplotype;

    let outcome = commit_alignment_settings_draft(
        &mut normal,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 30,
                include_secondary: false,
                include_supplementary: true,
                expression: "RG=normal;HP=1".to_owned(),
                error: None,
            },
            maximum_alignments: 80_000,
            apply_filter: true,
        },
        50_000,
    );

    assert_eq!(
        outcome,
        AlignmentSettingsOutcome {
            configuration_changed: true,
            query_changed: true,
        }
    );
    assert_eq!(normal.filters.minimum_mapping_quality, Some(30));
    assert!((normal.filters.minimum_allele_fraction - 0.07).abs() < f32::EPSILON);
    assert_eq!(
        normal.filters.alignment_color_mode,
        AlignmentColorMode::Haplotype
    );
    assert_eq!(
        normal.filters.auxiliary_predicates,
        vec!["RG=normal", "HP=1"]
    );
    assert!(!normal.filters.include_secondary);
    assert_eq!(normal.maximum_alignments, 50_000);
    assert_eq!(untouched.filters, SessionFilters::default());
}

#[test]
fn ultima_mode_transition_refetches_reference_but_quality_method_only_relayouts() {
    let make_track = || {
        SessionAlignmentTrack::new(
            TrackId::new(2),
            "Ultima".to_owned(),
            TrackSource::Local {
                path: "ultima.bam".to_owned(),
            },
        )
    };
    let mut entering = make_track();
    let (mut editor, mut active) = filter_state_from_session(&entering.filters).unwrap();
    let mut filters = entering.filters.clone();
    filters.alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions::default());
    let outcome = commit_alignment_settings_draft(
        &mut entering,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 0,
                include_secondary: false,
                include_supplementary: true,
                expression: String::new(),
                error: None,
            },
            maximum_alignments: 50_000,
            apply_filter: false,
        },
        50_000,
    );
    assert!(outcome.query_changed);

    let mut changing_method = make_track();
    changing_method.filters.alignment_color_mode =
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default());
    let (mut editor, mut active) = filter_state_from_session(&changing_method.filters).unwrap();
    let mut filters = changing_method.filters.clone();
    filters.alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions {
        quality_mode: UltimaQualityMode::Minimum,
        ..UltimaDisplayOptions::default()
    });
    let outcome = commit_alignment_settings_draft(
        &mut changing_method,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 0,
                include_secondary: false,
                include_supplementary: true,
                expression: String::new(),
                error: None,
            },
            maximum_alignments: 50_000,
            apply_filter: false,
        },
        50_000,
    );
    assert!(outcome.configuration_changed);
    assert!(!outcome.query_changed);
}

#[test]
fn bulk_appearance_preserves_sample_specific_filters() {
    let source = SessionFilters {
        alignment_color_mode: AlignmentColorMode::Haplotype,
        alignment_group_mode: AlignmentGroupMode::ReadGroup,
        alignment_group_order: AlignmentOrderDirection::Reverse,
        alignment_sort_mode: AlignmentSortMode::Base,
        alignment_sort_order: AlignmentOrderDirection::Reverse,
        alignment_density: 2.5,
        show_junctions: false,
        minimum_junction_flank: 14,
        junction_display_mode: JunctionDisplayMode::Expanded,
        sashimi_scale_mode: SashimiScaleMode::Shared,
        sashimi_fixed_scale_maximum: 250,
        sashimi_count_mode: SashimiCountMode::CountsPerMillion,
        show_junction_labels: false,
        show_modified_bases: false,
        minimum_mapping_quality: Some(5),
        minimum_base_quality: 7,
        auxiliary_predicates: vec!["RG=source".to_owned()],
        include_secondary: false,
        ..SessionFilters::default()
    };
    let mut target = SessionFilters {
        minimum_mapping_quality: Some(30),
        minimum_base_quality: 20,
        auxiliary_predicates: vec!["RG=target".to_owned()],
        include_secondary: true,
        ..SessionFilters::default()
    };

    copy_compatible_alignment_appearance(&mut target, &source);

    assert_eq!(target.alignment_color_mode, AlignmentColorMode::Haplotype);
    assert_eq!(target.alignment_group_mode, AlignmentGroupMode::ReadGroup);
    assert_eq!(
        target.alignment_group_order,
        AlignmentOrderDirection::Reverse
    );
    assert_eq!(target.alignment_sort_mode, AlignmentSortMode::Base);
    assert_eq!(
        target.alignment_sort_order,
        AlignmentOrderDirection::Reverse
    );
    assert!((target.alignment_density - 2.5).abs() < f32::EPSILON);
    assert!(!target.show_junctions);
    assert_eq!(target.minimum_junction_flank, 0);
    assert_eq!(target.junction_display_mode, JunctionDisplayMode::Expanded);
    assert_eq!(target.sashimi_scale_mode, SashimiScaleMode::Shared);
    assert_eq!(target.sashimi_fixed_scale_maximum, 250);
    assert_eq!(
        target.sashimi_count_mode,
        SashimiCountMode::CountsPerMillion
    );
    assert!(!target.show_junction_labels);
    assert!(!target.show_modified_bases);
    assert_eq!(target.minimum_mapping_quality, Some(30));
    assert_eq!(target.minimum_base_quality, 20);
    assert_eq!(target.auxiliary_predicates, vec!["RG=target"]);
    assert!(target.include_secondary);
}

#[test]
fn base_sort_position_tracks_each_panel_centre_only_in_base_mode() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();

    assert_eq!(
        alignment_sort_position(AlignmentSortMode::Base, viewport),
        Some(150)
    );
    assert_eq!(
        alignment_sort_position(AlignmentSortMode::MappingQuality, viewport),
        None
    );
}

#[test]
fn failed_fourth_controller_does_not_block_three_healthy_queries() {
    let mut session = SessionDocument::from_json(include_str!(
        "../../../../validation/four-track-session-v2.json"
    ))
    .unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for track in &mut session.alignment_tracks {
        if let TrackSource::Local { path } = &mut track.source {
            *path = repository.join(&*path).display().to_string();
        }
    }
    let cache = NonZeroUsize::new(16 * 1024 * 1024).unwrap();
    let mut controllers = session
        .alignment_tracks
        .into_iter()
        .map(|config| {
            spawn_alignment_track_controller(config, cache, None, None, &[INITIAL_PANEL_ID])
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut opened = Vec::new();
    let mut failures = 0_usize;
    for track in &mut controllers {
        match track
            .engine
            .receive_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Some(AlignmentEngineEvent::Opened { dictionary, .. }) => opened.push(dictionary),
            Some(AlignmentEngineEvent::Error { .. }) => failures += 1,
            event => panic!("unexpected controller-open event: {event:?}"),
        }
    }
    assert_eq!(opened.len(), 3);
    assert_eq!(failures, 1);

    for (track, dictionary) in controllers.iter().take(3).zip(&opened) {
        let contig = dictionary.id_by_name("chr1").unwrap();
        let interval = GenomicInterval::new(contig, 900_000, 920_000).unwrap();
        let clock = &track.projection(INITIAL_PANEL_ID).unwrap().clock;
        let generation = clock.advance();
        let request = PanelGeneration::new(PanelId::new(1), generation);
        track
            .engine
            .try_query(
                request,
                clock.clone(),
                interval,
                QueryOptions {
                    max_records: NonZeroUsize::new(100).unwrap(),
                    coverage_bins: NonZeroUsize::new(64).unwrap(),
                    collect_modifications: false,
                    ..QueryOptions::default()
                },
            )
            .unwrap();
    }
    for track in controllers.iter().take(3) {
        let generation = track.projection(INITIAL_PANEL_ID).unwrap().clock.current();
        let expected = PanelGeneration::new(PanelId::new(1), generation);
        match track
            .engine
            .receive_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Some(AlignmentEngineEvent::QueryCompleted {
                request: completed,
                query,
                ..
            }) => {
                assert_eq!(completed, expected);
                assert!(query.stats.records_seen > 0);
                assert!(query.stats.records_emitted <= 100);
            }
            event => panic!("unexpected healthy query event: {event:?}"),
        }
    }
}

#[test]
fn two_alignment_controllers_route_two_panel_queries_independently() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = repository.join("benchmark-data/mixed.bam");
    let panel_ids = [PanelId::new(7), PanelId::new(3)];
    let controllers = (1..=2)
        .map(|id| {
            let config = SessionAlignmentTrack::new(
                TrackId::new(id),
                format!("Mixed {id}"),
                TrackSource::Local {
                    path: source.display().to_string(),
                },
            );
            spawn_alignment_track_controller(
                config,
                NonZeroUsize::new(16 * 1024 * 1024).unwrap(),
                None,
                None,
                &panel_ids,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let dictionaries = controllers
        .iter()
        .map(|controller| {
            match controller
                .engine
                .receive_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => dictionary,
                event => panic!("unexpected controller-open event: {event:?}"),
            }
        })
        .collect::<Vec<_>>();

    for (controller, dictionary) in controllers.iter().zip(&dictionaries) {
        for (panel_id, interval) in two_panel_test_intervals(dictionary, panel_ids) {
            let clock = &controller.projection(panel_id).unwrap().clock;
            let request = PanelGeneration::new(panel_id, clock.advance());
            controller
                .engine
                .try_query(
                    request,
                    clock.clone(),
                    interval,
                    QueryOptions {
                        max_records: NonZeroUsize::new(100).unwrap(),
                        coverage_bins: NonZeroUsize::new(64).unwrap(),
                        collect_modifications: false,
                        ..QueryOptions::default()
                    },
                )
                .unwrap();
        }
    }

    for (controller, dictionary) in controllers.iter().zip(&dictionaries) {
        let expected = two_panel_test_intervals(dictionary, panel_ids);
        let mut completed = HashMap::new();
        for _ in 0..expected.len() {
            match controller
                .engine
                .receive_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Some(AlignmentEngineEvent::QueryCompleted { request, query, .. }) => {
                    completed.insert(request.panel_id(), query);
                }
                event => panic!("unexpected two-panel query event: {event:?}"),
            }
        }
        assert_eq!(completed.len(), 2);
        for (panel_id, interval) in expected {
            let query = completed.get(&panel_id).unwrap();
            assert_eq!(query.interval.contig(), interval.contig());
            assert!(query.interval.contains_interval(interval));
            assert!(query.stats.records_seen > 0);
        }
    }
}
