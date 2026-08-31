use super::super::*;
use crate::display::alignment::{
    INDEX_ONLY_READ_NOTICE, ReadPanelKey, ReadPanelSpec, alignment_scene_annotation_frame,
    index_only_read_display, index_only_read_notice, materialized_alignment_row_range,
    read_pane_layout, read_panel_bounds, read_state_display,
};
use crate::display::auxiliary::{
    feature_layout_row_count, feature_record_index_at_display_point,
    pinned_gene_layout_maximum_rows,
};
use crate::display::{
    ANNOTATION_PANE_BACKGROUND, PINNED_ALIGNMENT_COVERAGE_HEIGHT,
    PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT, PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT,
    PINNED_ALIGNMENT_MODIFICATION_HEIGHT, READ_PANE_HEADER_HEIGHT, SYNTHETIC_READ_ROW_STRIDE,
    alignment_activity_overview_from_counts, alignment_annotation_lane_heights,
    alignment_scene_read_extent, annotation_frame_content_height,
    append_annotation_pane_background, ensure_annotation_presentation_frames, pane_layout_snapshot,
    pinned_gene_track_height, primary_pinned_gene_id, read_panel_scrollbar_maximum,
    reconcile_read_panel_scroll_for_authority, reflow_annotation_presentation,
    reflow_annotation_presentation_with_heights, resolve_read_panel_overview,
    synthetic_read_rows_for_bounds, synthetic_section_display, track_section_geometry,
};

#[test]
fn alignment_row_materialization_tracks_vertical_camera_with_overscan() {
    assert_eq!(
        materialized_alignment_row_range(220.0, 10.0, 220.0, 320.0, 100.0, 1_000, 2),
        (8, 22)
    );
    assert_eq!(
        materialized_alignment_row_range(220.0, 10.0, 220.0, 320.0, 0.0, 15, 32),
        (0, 15)
    );
    assert_eq!(
        materialized_alignment_row_range(220.0, 10.0, 320.0, 320.0, 0.0, 15, 2),
        (0, 0)
    );
}

#[test]
fn scene_row_activity_is_the_overview_authority_when_the_cpu_read_list_is_empty() {
    let scene = alignment_activity_overview_from_counts([0, 3, 99, 1]);
    let cpu_scrollable = DisplayList::default();
    let resolved = resolve_read_panel_overview(Some(&scene), || {
        ui::scrollbar_overview(&[], &cpu_scrollable, 0.0, 400.0, 0.0, 0.0, 800.0, 128)
    });

    assert_eq!(resolved, scene);
    assert_eq!(resolved.bins.len(), 4);
    assert_eq!(resolved.bins[0].kind, None);
    assert!(resolved.bins[1].density > 0.0);
    assert_eq!(resolved.bins[2].density, 1.0);
    assert_eq!(resolved.bins[2].kind, Some(TrackKind::Alignment));
}

#[test]
fn an_authoritative_empty_scene_overview_cannot_be_replaced_by_legacy_geometry() {
    let empty = ui::ScrollbarOverview::default();
    let resolved = resolve_read_panel_overview(Some(&empty), || {
        alignment_activity_overview_from_counts([10, 20, 30])
    });

    assert_eq!(resolved, empty);
}

#[test]
fn scene_active_row_end_controls_scroll_extent_and_shrink_clamps_the_camera() {
    let (rows, bottom) = alignment_scene_read_extent(37, 200.0, 7.0, 400.0);
    assert_eq!(rows, 37);
    assert!((bottom - 465.0).abs() < f32::EPSILON);

    let (scroll, _) = reconcile_vertical_scroll(500.0, 600.0, 100.0, Some(5.0 / 6.0), None);
    assert!((scroll - 100.0).abs() < f32::EPSILON);
}

#[test]
fn pending_row_authority_presents_retained_thumb_as_provisional_without_moving_camera() {
    let lineage =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let coordinate_space = lineage;
    let mut state = ReadPanelScrollState {
        scroll_points: 353.5,
        content_origin_points: 3.5,
        latent_scroll_fraction: Some(0.5),
        content_bottom_points: 1_100.0,
        density: Some(1.0),
        stable_row_anchor: capture_stable_row_camera(350.0, 7.0, coordinate_space),
        scene_contig: Some(ContigId::new(2)),
    };
    let settled = state;

    assert_eq!(
        reconcile_read_panel_scroll_for_authority(
            &mut state,
            ReadRowCoordinateAuthority::Pending,
            Some(7.0),
            400.0,
            500.0,
            0.0,
            None,
        ),
        None
    );
    assert_eq!(state, settled, "pending work must remain purely latent");
    assert_eq!(
        read_panel_scrollbar_maximum(state, 500.0, ReadRowCoordinateAuthority::Pending),
        600.0,
        "pending activity must not flash a full-height zero-maximum thumb"
    );
    assert_eq!(
        read_panel_scrollbar_maximum(state, 500.0, ReadRowCoordinateAuthority::Absent),
        0.0,
        "missing semantic authority keeps the live rail but cannot expose an old thumb extent"
    );

    assert_eq!(
        reconcile_read_panel_scroll_for_authority(
            &mut state,
            ReadRowCoordinateAuthority::Exact(coordinate_space),
            Some(7.0),
            900.0,
            500.0,
            0.0,
            None,
        ),
        Some((7.0, coordinate_space))
    );
    assert!((state.scroll_points - 353.5).abs() < f32::EPSILON);
    assert_eq!(state.stable_row_anchor, settled.stable_row_anchor);
    assert_eq!(
        read_panel_scrollbar_maximum(
            state,
            500.0,
            ReadRowCoordinateAuthority::Exact(coordinate_space),
        ),
        400.0
    );
}

#[test]
fn stale_exact_scroll_cache_is_provisional_until_the_committed_target_rebuilds_it() {
    let lineage =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(13, 17));
    let exact = ReadRowCoordinateAuthority::Exact(lineage);
    let current = current_read_row_authority(4, 5, false, exact);
    let state = ReadPanelScrollState {
        scroll_points: 240.0,
        content_bottom_points: 1_100.0,
        ..ReadPanelScrollState::default()
    };

    assert_eq!(current, ReadRowCoordinateAuthority::Pending);
    assert!(
        !current.is_exact(),
        "stale metrics must not remain interactive"
    );
    assert_eq!(
        read_panel_scrollbar_maximum(state, 500.0, current),
        600.0,
        "the provisional waypoint must not collapse to a full-height zero-maximum thumb"
    );
}

#[test]
fn vertical_only_target_change_keeps_exact_scroll_authority() {
    let lineage =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(13, 17));
    let exact = ReadRowCoordinateAuthority::Exact(lineage);

    assert_eq!(current_read_row_authority(4, 5, true, exact), exact);
}

#[test]
fn same_target_new_row_atlas_revokes_old_exact_scroll_authority() {
    let coordinate_space =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(19, 23));
    let old_atlas = RowAtlasRevision::new(rgv_alignment_scene::RevisionFingerprint::new(29, 31));
    let new_atlas = RowAtlasRevision::new(rgv_alignment_scene::RevisionFingerprint::new(37, 41));
    let exact = ReadRowCoordinateAuthority::Exact(coordinate_space);

    assert!(read_row_cache_matches_committed_activity(
        7,
        exact,
        Some(old_atlas),
        Some(800),
        7,
        Some((coordinate_space, old_atlas, 800)),
    ));
    assert!(!read_row_cache_matches_committed_activity(
        7,
        exact,
        Some(old_atlas),
        Some(800),
        7,
        Some((coordinate_space, new_atlas, 800)),
    ));
    assert!(read_row_cache_matches_committed_activity(
        7,
        ReadRowCoordinateAuthority::Pending,
        None,
        Some(800),
        7,
        None,
    ));
}

#[test]
fn read_panels_tile_the_read_pane_with_independent_viewport_heights() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let bounds = read_panel_bounds(
        &[
            ReadPanelSpec::expanded(ReadPanelKey::Track(first), Some(300.0)),
            ReadPanelSpec::expanded(ReadPanelKey::Track(second), Some(100.0)),
        ],
        200.0,
        600.0,
    );

    assert_eq!(bounds.len(), 2);
    assert_eq!(bounds[0].key, ReadPanelKey::Track(first));
    assert_eq!((bounds[0].top, bounds[0].bottom), (200.0, 500.0));
    assert_eq!(bounds[1].key, ReadPanelKey::Track(second));
    assert_eq!((bounds[1].top, bounds[1].bottom), (500.0, 600.0));
    assert_eq!(bounds[0].bottom, bounds[1].top);
}

#[test]
fn a_single_read_panel_occupies_the_pane_without_a_saved_bottom_bound() {
    let track = TrackId::new(101);
    let layout = read_pane_layout(
        &[ReadPanelSpec::expanded(
            ReadPanelKey::Track(track),
            Some(100.0),
        )],
        200.0,
        600.0,
    );

    assert_eq!(layout.bounds.len(), 1);
    assert_eq!(layout.bounds[0].key, ReadPanelKey::Track(track));
    assert_eq!(
        (layout.bounds[0].top, layout.bounds[0].bottom),
        (200.0, 600.0)
    );
    assert_eq!(layout.content_bottom, 600.0);
}

#[test]
fn synthetic_single_read_panel_tracks_the_viewport_bottom() {
    assert_eq!(synthetic_read_rows_for_bounds(202.0, 902.0), 100);
    assert_eq!(synthetic_read_rows_for_bounds(202.0, 1_042.0), 120);
}

#[test]
fn synthetic_coverage_and_reads_are_owned_by_their_sections() {
    let geometry = track_section_geometry(100.0, 900.0, 360.0, true, true, Some(84.0));
    let read_content_top = geometry.read_clip_top + READ_PANE_HEADER_HEIGHT;
    let both = synthetic_section_display(800.0, 10_000, 900.0, geometry, true, true);
    let annotations_only = synthetic_section_display(800.0, 10_000, 900.0, geometry, true, false);
    let reads_only = synthetic_section_display(800.0, 10_000, 900.0, geometry, false, true);

    assert!(
        annotations_only
            .rectangles
            .iter()
            .all(|primitive| primitive.y + primitive.height <= geometry.annotation_clip_bottom)
    );
    assert!(
        reads_only
            .rectangles
            .iter()
            .all(|primitive| primitive.y >= read_content_top)
    );
    assert_eq!(
        both.rectangles.len(),
        annotations_only.rectangles.len() + reads_only.rectangles.len()
    );
    assert!(both.rectangles.iter().any(|primitive| {
        (primitive.y + primitive.height - 900.0).abs() < SYNTHETIC_READ_ROW_STRIDE
    }));
}

#[test]
fn content_sized_annotation_section_does_not_inherit_blank_pane_space() {
    let geometry = track_section_geometry(100.0, 900.0, 360.0, true, true, Some(84.0));

    assert_eq!(geometry.annotation_clip_top, 126.0);
    assert_eq!(geometry.annotation_clip_bottom, 210.0);
    assert_eq!(geometry.read_clip_top, 236.0);
}

#[test]
fn annotation_section_adapts_to_measured_frames_up_to_its_configured_limit() {
    let frames = vec![
        TrackFrame {
            key: TrackFrameKey::Reference,
            channel: crate::ui::TrackPane::Annotation,
            name: "Reference".to_owned(),
            kind: TrackKind::Reference,
            source_label: None,
            top: 126.0,
            bottom: 166.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
        TrackFrame {
            key: TrackFrameKey::Auxiliary(TrackId::new(9)),
            channel: crate::ui::TrackPane::Annotation,
            name: "Features".to_owned(),
            kind: TrackKind::Feature,
            source_label: None,
            top: 166.0,
            bottom: 306.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
    ];
    let measured = annotation_frame_content_height(&frames).unwrap();
    let fitted = track_section_geometry(100.0, 900.0, 360.0, true, true, Some(measured));
    let overflowing = track_section_geometry(100.0, 900.0, 360.0, true, true, Some(900.0));

    assert_eq!(measured, 180.0);
    assert_eq!(fitted.annotation_clip_bottom, 306.0);
    assert_eq!(fitted.read_clip_top, 332.0);
    assert_eq!(overflowing.annotation_clip_bottom, 540.0);
}

#[test]
fn explicit_read_panel_heights_remain_literal_until_the_pane_is_constrained() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let equal = read_panel_bounds(
        &[
            ReadPanelSpec::expanded(ReadPanelKey::Track(first), Some(100.0)),
            ReadPanelSpec::expanded(ReadPanelKey::Track(second), Some(100.0)),
        ],
        200.0,
        600.0,
    );
    let resized = read_panel_bounds(
        &[
            ReadPanelSpec::expanded(ReadPanelKey::Track(first), Some(300.0)),
            ReadPanelSpec::expanded(ReadPanelKey::Track(second), Some(100.0)),
        ],
        200.0,
        600.0,
    );

    assert_eq!(equal[0].bottom - equal[0].top, 100.0);
    assert_eq!(equal[1].bottom - equal[1].top, 100.0);
    assert_eq!(equal.last().map(|panel| panel.bottom), Some(400.0));
    assert_eq!(resized[0].bottom - resized[0].top, 300.0);
    assert_eq!(resized[1].bottom - resized[1].top, 100.0);
    assert_eq!(resized.last().map(|panel| panel.bottom), Some(600.0));
}

#[test]
fn a_group_after_an_expanded_sibling_does_not_get_pushed_to_the_pane_bottom() {
    let sibling = TrackId::new(101);
    let expanded = read_panel_bounds(
        &[
            ReadPanelSpec::expanded(ReadPanelKey::Track(sibling), Some(100.0)),
            ReadPanelSpec::header(ReadPanelKey::Group(7)),
        ],
        200.0,
        600.0,
    );
    let sibling_hidden = read_panel_bounds(
        &[
            ReadPanelSpec::header(ReadPanelKey::Track(sibling)),
            ReadPanelSpec::header(ReadPanelKey::Group(7)),
        ],
        200.0,
        600.0,
    );

    assert_eq!((expanded[0].top, expanded[0].bottom), (200.0, 300.0));
    assert_eq!(
        (expanded[1].top, expanded[1].bottom),
        (300.0, 300.0 + READ_PANE_HEADER_HEIGHT)
    );
    assert!(expanded[1].bottom < 600.0);
    assert_eq!(
        (sibling_hidden[1].top, sibling_hidden[1].bottom),
        (
            200.0 + READ_PANE_HEADER_HEIGHT,
            200.0 + READ_PANE_HEADER_HEIGHT * 2.0,
        )
    );
}

#[test]
fn read_group_header_reserves_only_read_pane_space() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let bounds = read_panel_bounds(
        &[
            ReadPanelSpec::header(ReadPanelKey::Group(7)),
            ReadPanelSpec::expanded(ReadPanelKey::Track(first), Some(100.0)),
            ReadPanelSpec::expanded(ReadPanelKey::Track(second), Some(100.0)),
        ],
        200.0,
        600.0,
    );

    assert_eq!(bounds[0].key, ReadPanelKey::Group(7));
    assert_eq!(bounds[0].bottom - bounds[0].top, READ_PANE_HEADER_HEIGHT);
    assert_eq!(bounds[1].key, ReadPanelKey::Track(first));
    assert_eq!(bounds[2].key, ReadPanelKey::Track(second));
    assert_eq!(bounds[0].bottom, bounds[1].top);
    assert_eq!(bounds[1].bottom, bounds[2].top);
    assert_eq!(bounds[2].bottom, 422.0);
    assert!(bounds[2].bottom < 600.0);
}

#[test]
fn read_stack_overflow_preserves_every_header_and_reports_parent_extent() {
    let specs = (1..=8)
        .flat_map(|id| {
            [
                ReadPanelSpec::header(ReadPanelKey::Group(id)),
                ReadPanelSpec::expanded(
                    ReadPanelKey::Track(TrackId::new(u64::from(id))),
                    Some(80.0),
                ),
            ]
        })
        .collect::<Vec<_>>();

    let layout = read_pane_layout(&specs, 200.0, 500.0);

    assert!(layout.content_bottom > 500.0);
    assert_eq!(layout.bounds.len(), specs.len());
    for (spec, bounds) in specs.iter().zip(&layout.bounds) {
        assert!(bounds.bottom > bounds.top);
        if matches!(
            spec.content,
            crate::display::alignment::ReadPanelContent::HeaderOnly
        ) {
            assert_eq!(bounds.bottom - bounds.top, READ_PANE_HEADER_HEIGHT);
        } else {
            assert!(bounds.bottom - bounds.top >= 40.0);
        }
    }
    assert!(
        layout
            .bounds
            .windows(2)
            .all(|rows| (rows[0].bottom - rows[1].top).abs() < f32::EPSILON)
    );
}

#[test]
fn annotation_pane_background_is_bounded_and_precedes_track_geometry() {
    let foreground = RectPrimitive {
        x: 20.0,
        y: 30.0,
        width: 40.0,
        height: 5.0,
        color: Rgba::new(0.2, 0.4, 0.8, 1.0),
        object_id: 7,
        shape: Default::default(),
    };
    let mut display = DisplayList::default();
    append_annotation_pane_background(&mut display, 300.0, 10.0, 90.0);
    display.rectangles.push(foreground);

    assert_eq!(display.rectangles.len(), 2);
    assert_eq!(display.rectangles[0].x, 0.0);
    assert_eq!(display.rectangles[0].y, 10.0);
    assert_eq!(display.rectangles[0].width, 300.0);
    assert_eq!(display.rectangles[0].height, 80.0);
    assert_eq!(display.rectangles[0].color, ANNOTATION_PANE_BACKGROUND);
    assert_eq!(display.rectangles[0].object_id, 0);
    assert_eq!(display.rectangles[1], foreground);

    append_annotation_pane_background(&mut display, 300.0, 90.0, 90.0);
    assert_eq!(display.rectangles.len(), 2);
}

#[test]
fn annotation_composition_follows_presentation_order_without_reordering_sources() {
    let first = TrackId::new(1);
    let second = TrackId::new(2);
    let mut presentation = WorkspacePresentation::default();
    presentation.pane_tracks = vec![
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: second,
            content_visible: true,
            preferred_height: Some(50.0),
        },
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: first,
            content_visible: true,
            preferred_height: Some(50.0),
        },
    ];
    let frame = |id, top| TrackFrame {
        key: TrackFrameKey::Auxiliary(id),
        channel: ui::TrackPane::Annotation,
        name: format!("track-{}", id.get()),
        kind: TrackKind::Feature,
        source_label: None,
        top,
        bottom: top + 50.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    };
    let mut frames = vec![frame(first, 0.0), frame(second, 50.0)];
    let mut display = DisplayList {
        rectangles: vec![
            RectPrimitive {
                x: 0.0,
                y: 10.0,
                width: 10.0,
                height: 2.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                object_id: 1,
                shape: Default::default(),
            },
            RectPrimitive {
                x: 0.0,
                y: 60.0,
                width: 10.0,
                height: 2.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                object_id: 2,
                shape: Default::default(),
            },
        ],
        texts: Vec::new(),
    };

    let bottom = reflow_annotation_presentation(&presentation, &mut frames, &mut display, 0.0);

    assert_eq!(bottom, 100.0);
    assert_eq!(frames[0].key, TrackFrameKey::Auxiliary(second));
    assert_eq!((frames[0].top, frames[0].bottom), (0.0, 50.0));
    assert_eq!(frames[1].key, TrackFrameKey::Auxiliary(first));
    assert_eq!((frames[1].top, frames[1].bottom), (50.0, 100.0));
    assert_eq!(display.rectangles[0].y, 60.0);
    assert_eq!(display.rectangles[1].y, 10.0);
}

#[test]
fn annotation_composition_applies_workspace_row_heights() {
    let first = TrackId::new(11);
    let second = TrackId::new(22);
    let mut presentation = WorkspacePresentation::default();
    presentation.pane_tracks = vec![
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: second,
            content_visible: true,
            preferred_height: None,
        },
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: first,
            content_visible: true,
            preferred_height: None,
        },
    ];
    let frame = |id, top| TrackFrame {
        key: TrackFrameKey::Auxiliary(id),
        channel: ui::TrackPane::Annotation,
        name: format!("track-{}", id.get()),
        kind: TrackKind::Feature,
        source_label: None,
        top,
        bottom: top + 50.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    };
    let mut frames = vec![frame(first, 0.0), frame(second, 50.0)];
    let mut display = DisplayList {
        rectangles: vec![
            RectPrimitive {
                x: 0.0,
                y: 10.0,
                width: 10.0,
                height: 2.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                object_id: 1,
                shape: Default::default(),
            },
            RectPrimitive {
                x: 0.0,
                y: 60.0,
                width: 10.0,
                height: 2.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                object_id: 2,
                shape: Default::default(),
            },
        ],
        texts: Vec::new(),
    };
    let shared = BTreeMap::from([
        (
            PresentationNodeId::Track {
                channel: ui::TrackPane::Annotation,
                track_id: first,
            },
            70.0,
        ),
        (
            PresentationNodeId::Track {
                channel: ui::TrackPane::Annotation,
                track_id: second,
            },
            80.0,
        ),
    ]);

    let bottom = reflow_annotation_presentation_with_heights(
        &presentation,
        &mut frames,
        &mut display,
        0.0,
        &shared,
    );

    assert_eq!(bottom, 150.0);
    assert_eq!(frames[0].key, TrackFrameKey::Auxiliary(second));
    assert_eq!((frames[0].top, frames[0].bottom), (0.0, 80.0));
    assert_eq!(frames[1].key, TrackFrameKey::Auxiliary(first));
    assert_eq!((frames[1].top, frames[1].bottom), (80.0, 150.0));
    assert_eq!(display.rectangles[0].y, 90.0);
    assert_eq!(display.rectangles[1].y, 10.0);
}

#[test]
fn annotation_tree_retains_a_restore_header_when_a_renderer_omits_its_frame() {
    let rendered = TrackId::new(1);
    let omitted = TrackId::new(2);
    let mut presentation = WorkspacePresentation::default();
    presentation.pane_tracks = vec![
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: rendered,
            content_visible: true,
            preferred_height: None,
        },
        SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id: omitted,
            content_visible: false,
            preferred_height: None,
        },
    ];
    let frame = |id, collapsed| TrackFrame {
        key: TrackFrameKey::Auxiliary(id),
        channel: ui::TrackPane::Annotation,
        name: format!("track-{}", id.get()),
        kind: TrackKind::Feature,
        source_label: None,
        top: 0.0,
        bottom: 50.0,
        visible: true,
        collapsed,
        frozen_bottom: None,
        subtracks: Vec::new(),
    };
    let mut frames = vec![frame(rendered, false)];
    let fallbacks = [frame(rendered, false), frame(omitted, true)];

    let bottom =
        ensure_annotation_presentation_frames(&presentation, &mut frames, &fallbacks, 50.0);

    assert_eq!(bottom, 76.0);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].key, TrackFrameKey::Auxiliary(omitted));
    assert_eq!((frames[1].top, frames[1].bottom), (50.0, 76.0));
    assert!(frames[1].collapsed);
}

#[test]
fn overflowing_annotation_rows_remain_reachable_after_an_unrelated_track_is_hidden() {
    let first = TrackId::new(1);
    let hidden_unrelated = TrackId::new(2);
    let below_viewport = TrackId::new(3);
    let mut presentation = WorkspacePresentation::default();
    presentation.pane_tracks = [
        (first, true, 80.0),
        (hidden_unrelated, false, 26.0),
        (below_viewport, true, 80.0),
    ]
    .into_iter()
    .map(
        |(track_id, content_visible, preferred_height)| SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id,
            content_visible,
            preferred_height: Some(preferred_height),
        },
    )
    .collect();
    let frame = |id, top: f32, height: f32, collapsed| TrackFrame {
        key: TrackFrameKey::Auxiliary(id),
        channel: ui::TrackPane::Annotation,
        name: format!("track-{}", id.get()),
        kind: TrackKind::Feature,
        source_label: None,
        top,
        bottom: top + height,
        visible: true,
        collapsed,
        frozen_bottom: None,
        subtracks: Vec::new(),
    };
    let frames = vec![
        frame(first, 0.0, 80.0, false),
        frame(hidden_unrelated, 80.0, 26.0, true),
        frame(below_viewport, 106.0, 80.0, false),
    ];

    let mut layout = pane_layout_snapshot(
        &presentation,
        &frames,
        ui::TrackPane::Annotation,
        0.0,
        100.0,
        186.0,
        0.0,
        0.0,
        300.0,
    );

    assert_eq!(layout.items.len(), 3);
    let target = layout
        .items
        .iter()
        .find(|item| {
            item.id
                == PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id: below_viewport,
                }
        })
        .unwrap();
    assert_eq!(target.row_rect.height(), 0.0);
    assert!(target.desired_content_visible);
    assert!(target.effective_content_visible);

    // This is the fast-scroll path: no renderer or presentation rebuild is
    // needed for the previously clipped row to become sidebar-visible.
    layout.reproject_scroll(86.0);
    let target = layout
        .items
        .iter()
        .find(|item| {
            item.id
                == PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id: below_viewport,
                }
        })
        .unwrap();
    assert_eq!(
        (target.row_rect.top(), target.row_rect.bottom()),
        (20.0, 100.0)
    );
    assert_eq!(
        (target.header_rect.top(), target.header_rect.bottom()),
        (20.0, 42.0)
    );
    assert!(target.content_rect.is_some());
}

#[test]
fn annotation_tree_does_not_duplicate_a_composite_member_row() {
    let anchor = TrackId::new(1);
    let member = TrackId::new(2);
    let mut presentation = WorkspacePresentation::default();
    presentation.pane_tracks = [anchor, member]
        .into_iter()
        .map(|track_id| SessionPaneTrackState {
            channel: ui::TrackPane::Annotation,
            track_id,
            content_visible: true,
            preferred_height: None,
        })
        .collect();
    let mut overlay = TrackFrame {
        key: TrackFrameKey::Auxiliary(anchor),
        channel: ui::TrackPane::Annotation,
        name: "Overlay 1 · 2 tracks".to_owned(),
        kind: TrackKind::Quantitative,
        source_label: None,
        top: 0.0,
        bottom: 80.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    };
    overlay.subtracks.push(TrackSubtrack {
        alignment_subtrack: None,
        label: "member".to_owned(),
        detail: None,
        top: 22.0,
        bottom: 42.0,
        collapsed: false,
        composite_member: Some(member),
    });
    let mut frames = vec![overlay.clone()];
    let mut member_fallback = overlay;
    member_fallback.key = TrackFrameKey::Auxiliary(member);
    member_fallback.name = "member".to_owned();

    let bottom =
        ensure_annotation_presentation_frames(&presentation, &mut frames, &[member_fallback], 80.0);

    assert_eq!(bottom, 80.0);
    assert_eq!(frames.len(), 1);
}

#[test]
fn reference_gutter_labels_include_a_colour_independent_orientation_indicator() {
    assert_eq!(reference_bases_label(false), "> Bases · forward");
    assert_eq!(reference_bases_label(true), "< Bases · reverse strand");
}

#[test]
fn only_the_primary_visible_gene_model_is_pinned() {
    let source = |name: &str| TrackSource::Local {
        path: format!("{name}.gtf"),
    };
    let feature = TrackConfig::new(
        TrackId::new(1),
        "Features".to_owned(),
        TrackKind::Feature,
        source("features"),
    );
    let mut hidden_gene = TrackConfig::new(
        TrackId::new(2),
        "Hidden genes".to_owned(),
        TrackKind::GeneModel,
        source("hidden"),
    );
    hidden_gene.visible = false;
    let primary_gene = TrackConfig::new(
        TrackId::new(3),
        "Primary genes".to_owned(),
        TrackKind::GeneModel,
        source("primary"),
    );
    let comparative_gene = TrackConfig::new(
        TrackId::new(4),
        "Comparative genes".to_owned(),
        TrackKind::GeneModel,
        source("comparative"),
    );

    assert_eq!(
        primary_pinned_gene_id(&[feature, hidden_gene, primary_gene, comparative_gene]),
        Some(TrackId::new(3))
    );
}

#[test]
fn pinned_context_heights_are_fixed_layout_dimensions() {
    let mut gene = TrackConfig::new(
        TrackId::new(1),
        "Primary genes".to_owned(),
        TrackKind::GeneModel,
        TrackSource::Local {
            path: "genes.gtf".to_owned(),
        },
    );
    gene.height = 84.0;
    assert!((pinned_gene_track_height(&gene) - 84.0).abs() < f32::EPSILON);
    gene.collapsed = true;
    assert!((pinned_gene_track_height(&gene) - 26.0).abs() < f32::EPSILON);

    assert!((PINNED_ALIGNMENT_COVERAGE_HEIGHT - 48.0).abs() < f32::EPSILON);
    assert!((PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT - 52.0).abs() < f32::EPSILON);
    assert!((PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT - 120.0).abs() < f32::EPSILON);
    assert!((PINNED_ALIGNMENT_MODIFICATION_HEIGHT - 26.0).abs() < f32::EPSILON);
}

#[test]
fn read_density_does_not_change_alignment_annotation_lane_heights() {
    let mut filters = SessionFilters {
        alignment_density: MIN_VERTICAL_DENSITY,
        ..SessionFilters::default()
    };
    let compact_reads = alignment_annotation_lane_heights(&filters);
    filters.alignment_density = MAX_VERTICAL_DENSITY;
    let expanded_reads = alignment_annotation_lane_heights(&filters);

    assert_eq!(compact_reads, expanded_reads);
    assert_eq!(compact_reads.coverage, PINNED_ALIGNMENT_COVERAGE_HEIGHT);
    assert_eq!(
        compact_reads.junction,
        PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT
    );
    assert_eq!(
        compact_reads.modification,
        PINNED_ALIGNMENT_MODIFICATION_HEIGHT
    );

    filters.junction_display_mode = JunctionDisplayMode::Expanded;
    assert_eq!(
        alignment_annotation_lane_heights(&filters).junction,
        PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT
    );
}

#[test]
fn unified_annotation_lane_layout_exists_without_a_legacy_query() {
    let mut config = SessionAlignmentTrack::new(
        TrackId::new(77),
        "RNA scene".to_owned(),
        TrackSource::Local {
            path: "rna.bam".to_owned(),
        },
    );
    config.filters.show_coverage = true;
    config.filters.show_junctions = true;
    config.filters.show_modification_frequency = true;
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 10_100).unwrap(),
        800.0,
    )
    .unwrap();

    let frame = alignment_scene_annotation_frame(&config, viewport, 30.0, None);
    assert_eq!(frame.key, TrackFrameKey::Alignment(config.id));
    assert!(frame.bottom > frame.top + 22.0);
    assert_eq!(
        frame
            .subtracks
            .iter()
            .map(|subtrack| (subtrack.alignment_subtrack, subtrack.collapsed))
            .collect::<Vec<_>>(),
        vec![
            (Some(AlignmentSubtrack::Coverage), false),
            (Some(AlignmentSubtrack::Sashimi), false),
            (Some(AlignmentSubtrack::Modifications), false),
        ]
    );
}

#[test]
fn shifted_macos_wheel_uses_horizontal_fallback_for_vertical_scroll() {
    let horizontal_line = wheel_primary_axis(MouseScrollDelta::LineDelta(2.0, 0.0));
    let horizontal_pixel = wheel_primary_axis(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
        -24.0, 0.0,
    )));
    let vertical_wins = wheel_primary_axis(MouseScrollDelta::LineDelta(8.0, -1.0));
    assert!((horizontal_line - 80.0).abs() < f32::EPSILON);
    assert!((horizontal_pixel + 24.0).abs() < f32::EPSILON);
    assert!((vertical_wins + 40.0).abs() < f32::EPSILON);
}

#[test]
fn overview_drag_combines_pointer_anchored_zoom_and_pan() {
    let origin = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 1_000, 2_000).unwrap(),
        1_000.0,
    )
    .unwrap();
    let zoomed = overview_drag_step(origin, 0.25, 0.0, 80.0, 10_000).unwrap();
    assert!(zoomed.interval().len() < origin.interval().len());
    let anchored = zoomed.interval().start() as f64 + zoomed.interval().len() as f64 * 0.25;
    assert!((anchored - 1_250.0).abs() <= 1.0);

    let panned = overview_drag_step(origin, 0.25, 100.0, 0.0, 10_000).unwrap();
    assert!((panned.span() - 1_000.0).abs() < f64::EPSILON);
    assert!(panned.exact_start() > 1_100.0);

    let fine_pan = overview_drag_step(zoomed, 0.25, 100.0, 0.0, 10_000).unwrap();
    let coarse_shift = panned.exact_start() - origin.exact_start();
    let fine_shift = fine_pan.exact_start() - zoomed.exact_start();
    assert!(fine_shift < coarse_shift);
    assert!(overview_pan_gain(10_000.0, 10_000) > overview_pan_gain(100.0, 10_000));
}

#[test]
fn overview_pan_remains_continuous_at_single_base_zoom() {
    let mut viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 1_000, 1_001).unwrap(),
        1_000.0,
    )
    .unwrap();
    for _ in 0..100 {
        let previous = viewport.exact_start();
        viewport = overview_drag_step(viewport, 0.5, 1.0, 0.0, 10_000).unwrap();
        assert!(viewport.exact_start() > previous);
    }
    let expected = 1_000.0 + overview_pan_gain(1.0, 10_000) * 0.1;
    assert!((viewport.exact_start() - expected).abs() < 1.0e-8);
    assert!((viewport.span() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn fine_scale_handle_press_never_becomes_a_chromosome_jump() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 906_340, 906_341).unwrap(),
        1_000.0,
    )
    .unwrap();
    // At chromosome scale a pointer pixel inside the minimum-width visual
    // handle can map thousands of bases away from the exact viewport.
    let mapped_pointer_position = 908_000;
    let (handle_viewport, anchor) =
        overview_drag_origin(viewport, mapped_pointer_position, Some(0.5), 3_000_000);
    assert_eq!(handle_viewport, viewport);
    assert!((anchor - 0.5).abs() < f64::EPSILON);

    let (rail_viewport, _) =
        overview_drag_origin(viewport, mapped_pointer_position, None, 3_000_000);
    assert_ne!(rail_viewport.interval(), viewport.interval());
    assert!(rail_viewport.interval().contains(mapped_pointer_position));
}

#[test]
fn zoom_out_retains_data_from_the_overlapping_loaded_interval() {
    let contig = ContigId::new(0);
    let loaded = GenomicInterval::new(contig, 904_160, 904_392).unwrap();
    let expanded_view = GenomicInterval::new(contig, 897_845, 910_760).unwrap();
    assert!(!loaded.contains_interval(expanded_view));
    assert!(retained_data_overlaps(loaded, expanded_view));

    let disjoint_view = GenomicInterval::new(contig, 920_000, 930_000).unwrap();
    assert!(!retained_data_overlaps(loaded, disjoint_view));
}

#[test]
fn index_only_overview_builds_a_visible_read_canvas_notice_without_query_data() {
    let display = index_only_read_display(800.0, 240.0, 500.0, None);

    assert_eq!(display.rectangles.len(), 2);
    assert_eq!(display.texts.len(), 1);
    assert_eq!(display.texts[0].text, INDEX_ONLY_READ_NOTICE);
    assert_eq!((display.texts[0].x, display.texts[0].y), (400.0, 261.0));
    assert!(display.texts[0].y >= 240.0 && display.texts[0].y < 500.0);

    let clipped = display.scrolled_and_clipped_vertical(0.0, 240.0, 500.0);
    assert_eq!(clipped.texts, display.texts);
}

#[test]
fn index_only_overview_prefers_the_indexed_contig_count_when_available() {
    assert_eq!(
        index_only_read_notice(Some(31)),
        "31 indexed alignments on this contig — zoom in to view individual reads"
    );
    assert_eq!(
        index_only_read_notice(Some(1)),
        "1 indexed alignment on this contig — zoom in to view individual reads"
    );
    assert_eq!(index_only_read_notice(None), INDEX_ONLY_READ_NOTICE);
}

#[test]
fn checking_read_state_is_visible_but_never_interactive() {
    let display = read_state_display(
        800.0,
        240.0,
        500.0,
        "Checking current viewport for complete reads…",
    );
    assert!(
        display
            .rectangles
            .iter()
            .all(|primitive| primitive.object_id == 0)
    );
    assert_eq!(display.texts.len(), 1);
    assert!(display.texts[0].text.contains("Checking current viewport"));
}

#[test]
fn horizontal_track_overview_drag_scales_density_logarithmically() {
    assert!((density_from_horizontal_drag(1.0, 0.0) - 1.0).abs() < f32::EPSILON);
    assert!(density_from_horizontal_drag(1.0, 50.0) > 1.0);
    assert!(density_from_horizontal_drag(1.0, -50.0) < 1.0);
    assert!(
        (density_from_horizontal_drag(1.0, 1_000.0) - MAX_VERTICAL_DENSITY).abs() < f32::EPSILON
    );
    assert!(
        (density_from_horizontal_drag(1.0, -1_000.0) - MIN_VERTICAL_DENSITY).abs() < f32::EPSILON
    );
    assert!(density_from_horizontal_drag(1.0, 150.0) > 3.0);
}

#[test]
fn density_navigation_creates_a_real_camera_for_sparse_content() {
    let viewport_height = 600.0;
    let actual_bottom = 700.0;
    let viewport_bottom = 700.0;
    assert!(
        vertical_camera_maximum(
            actual_bottom,
            viewport_bottom,
            viewport_height,
            MIN_VERTICAL_DENSITY,
        )
        .abs()
            < f32::EPSILON
    );

    let density = 1.0;
    let maximum = vertical_camera_maximum(actual_bottom, viewport_bottom, viewport_height, density);
    let expected = viewport_height * (density / MIN_VERTICAL_DENSITY - 1.0);
    assert!((maximum - expected).abs() < 1.0e-3);

    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(22.0, 700.0));
    let geometry = ui::scrollbar_geometry_for_density_in_viewport(
        rect,
        maximum,
        0.0,
        density,
        viewport_height,
    );
    assert!(geometry.thumb.height() < geometry.track.height());
}

#[test]
fn vertical_overview_couples_navigation_to_live_density() {
    let (expanded_density, anchored_scroll) =
        vertical_overview_step(1.0, 1_000.0, 200.0, 50.0, 0.0);
    let before_anchor = (1_000.0 + 200.0) / 1.0;
    let after_anchor = (anchored_scroll + 200.0) / expanded_density;
    assert!((before_anchor - after_anchor).abs() < 1.0e-3);

    let (_, compact_pan) = vertical_overview_step(MIN_VERTICAL_DENSITY, 1_000.0, 200.0, 0.0, 50.0);
    let (_, expanded_pan) = vertical_overview_step(MAX_VERTICAL_DENSITY, 1_000.0, 200.0, 0.0, 50.0);
    assert!(compact_pan - 1_000.0 > expanded_pan - 1_000.0);
}

#[test]
fn vertical_overview_retains_its_camera_while_content_fits() {
    let (fitting_scroll, latent) = reconcile_vertical_scroll(0.0, 0.0, 0.0, None, Some(0.68));
    assert!(fitting_scroll.abs() < f32::EPSILON);
    assert_eq!(latent, Some(0.68));

    let (overflow_scroll, retained) =
        reconcile_vertical_scroll(fitting_scroll, 0.0, 400.0, latent, None);
    assert!((overflow_scroll - 272.0).abs() < 1.0e-4);
    assert_eq!(retained, Some(0.68));

    let (ordinary_scroll, ordinary_fraction) =
        reconcile_vertical_scroll(300.0, 400.0, 600.0, retained, None);
    assert!((ordinary_scroll - 300.0).abs() < f32::EPSILON);
    assert_eq!(ordinary_fraction, Some(0.5));
}

#[test]
fn mate_reveal_centers_the_stable_row_without_repacking_it() {
    assert_eq!(
        center_read_row_scroll(222.0, 7.0, 0, 200.0, 600.0, 800.0),
        0.0
    );
    assert_eq!(
        center_read_row_scroll(222.0, 7.0, 80, 200.0, 600.0, 800.0),
        385.5
    );
    assert_eq!(
        center_read_row_scroll(222.0, 7.0, 800, 200.0, 600.0, 800.0),
        800.0
    );
}

#[test]
fn unified_row_camera_survives_stable_lineage_but_resets_after_a_repack() {
    let stable =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(1, 2));
    let repacked =
        RowCoordinateSpaceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(1, 3));
    let anchor = capture_stable_row_camera(10.5 * 7.0, 7.0, stable).unwrap();

    assert_eq!(reproject_stable_row_camera(anchor, 7.0, stable), None);
    assert_eq!(
        reproject_stable_row_camera(anchor, 14.0, stable),
        Some(10.5 * 14.0)
    );
    assert_eq!(
        reproject_stable_row_camera(anchor, 7.0, repacked),
        Some(0.0)
    );
}

#[test]
fn alignment_lane_routing_uses_stable_track_frames() {
    let frames = vec![
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(10)),
            channel: ui::TrackPane::Annotation,
            name: "Tumour".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 100.0,
            bottom: 200.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(11)),
            channel: ui::TrackPane::Annotation,
            name: "Normal".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 200.0,
            bottom: 300.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
    ];
    assert_eq!(
        alignment_track_at_content_y(&frames, 225.0),
        Some(TrackId::new(11))
    );
}

#[test]
fn alignment_lane_picking_distinguishes_scrolled_read_groups() {
    let frames = vec![
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(10)),
            channel: ui::TrackPane::Reads,
            name: "Tumour".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 300.0,
            bottom: 500.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(322.0),
            subtracks: vec![TrackSubtrack {
                alignment_subtrack: Some(AlignmentSubtrack::Reads),
                label: "Reads".to_owned(),
                detail: None,
                top: 325.0,
                bottom: 375.0,
                collapsed: false,
                composite_member: None,
            }],
        },
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(11)),
            channel: ui::TrackPane::Reads,
            name: "Normal".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 500.0,
            bottom: 800.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(522.0),
            subtracks: vec![TrackSubtrack {
                alignment_subtrack: Some(AlignmentSubtrack::Reads),
                label: "Reads".to_owned(),
                detail: None,
                top: 525.0,
                bottom: 575.0,
                collapsed: false,
                composite_member: None,
            }],
        },
    ];

    assert_eq!(
        alignment_lane_at_screen_y(
            &frames,
            AlignmentSubtrack::Reads,
            350.0,
            0.0,
            0.0,
            300.0,
            100.0,
            100.0,
            800.0,
        ),
        Some(TrackId::new(10))
    );
    assert_eq!(
        alignment_lane_at_screen_y(
            &frames,
            AlignmentSubtrack::Reads,
            350.0,
            200.0,
            0.0,
            300.0,
            100.0,
            100.0,
            800.0,
        ),
        Some(TrackId::new(11))
    );
    assert_eq!(
        alignment_lane_at_screen_y(
            &frames,
            AlignmentSubtrack::Reads,
            250.0,
            200.0,
            0.0,
            300.0,
            100.0,
            100.0,
            800.0,
        ),
        None,
        "a read lane must not be pickable through the annotation pane"
    );
}

#[test]
fn collapsed_alignment_lanes_are_not_pickable() {
    let frames = vec![TrackFrame {
        key: TrackFrameKey::Alignment(TrackId::new(10)),
        channel: ui::TrackPane::Annotation,
        name: "Tumour".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 180.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(180.0),
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Coverage),
            label: "Coverage".to_owned(),
            detail: None,
            top: 122.0,
            bottom: 142.0,
            collapsed: true,
            composite_member: None,
        }],
    }];

    assert_eq!(
        alignment_lane_at_screen_y(
            &frames,
            AlignmentSubtrack::Coverage,
            130.0,
            0.0,
            0.0,
            180.0,
            180.0,
            100.0,
            500.0,
        ),
        None
    );
}

#[test]
fn quantitative_selection_prefers_the_narrowest_half_open_source_interval() {
    let records = [
        rgv_hts::QuantitativeRecord {
            start: 100,
            end: 200,
            value: 1.0,
        },
        rgv_hts::QuantitativeRecord {
            start: 120,
            end: 130,
            value: 2.0,
        },
    ];
    assert_eq!(
        quantitative_record_index_at_position(&records, 125),
        Some(1)
    );
    assert_eq!(
        quantitative_record_index_at_position(&records, 130),
        Some(0)
    );
    assert_eq!(quantitative_record_index_at_position(&records, 200), None);
}

#[test]
fn mutation_selection_requires_the_exact_sample_and_half_open_interval() {
    let mutation = |sample_index, start, end| rgv_hts::MutationRecord {
        sample_index,
        start,
        end,
        source_start: start.saturating_add(1),
        source_end: end,
        insertion_boundary: false,
        gene: "TP53".to_owned(),
        classification: "Missense_Mutation".to_owned(),
        variant_type: "SNP".to_owned(),
        reference: "C".to_owned(),
        tumor_allele_one: "C".to_owned(),
        tumor_allele_two: "T".to_owned(),
        annotations: Vec::new(),
        annotation_values_omitted: 0,
    };
    let records = [
        mutation(0, 100, 200),
        mutation(0, 120, 130),
        mutation(1, 120, 130),
    ];

    assert_eq!(mutation_record_index_at_position(&records, 0, 125), Some(1));
    assert_eq!(mutation_record_index_at_position(&records, 1, 125), Some(2));
    assert_eq!(mutation_record_index_at_position(&records, 1, 130), None);
}

#[test]
fn interaction_selection_prefers_endpoints_then_the_narrowest_arc() {
    let endpoint = |start, end| rgv_hts::InteractionEndpoint {
        contig: "chr1".to_owned(),
        start,
        end,
        name: None,
        strand: rgv_hts::Strand::Unstranded,
    };
    let interaction = |source, target, score| rgv_hts::InteractionRecord {
        source,
        target,
        name: None,
        score: Some(score),
        value: None,
        experiment: None,
        color: None,
        directional: false,
    };
    let records = [
        interaction(endpoint(100, 110), endpoint(190, 200), 100.0),
        interaction(endpoint(120, 130), endpoint(170, 180), 20.0),
    ];
    assert_eq!(
        interaction_record_index_at_position(&records, "chr1", 125, 1),
        Some(1)
    );
    assert_eq!(
        interaction_record_index_at_position(&records, "chr1", 150, 1),
        Some(1)
    );
    assert_eq!(
        interaction_record_index_at_position(&records, "chr2", 150, 1),
        None
    );
}

#[test]
fn shared_quantitative_range_uses_visible_compatible_participants_only() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    let config = |id: u64, value_transform, visible| {
        let mut track = TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bw"),
            },
        );
        track.visible = visible;
        track.quantitative.scale_mode = QuantitativeScaleMode::Shared;
        track.quantitative.scale_transform = value_transform;
        track
    };
    let tracks = vec![
        config(1, QuantitativeScaleTransform::Linear, true),
        config(2, QuantitativeScaleTransform::Linear, true),
        config(3, QuantitativeScaleTransform::SignedLog10, true),
        config(4, QuantitativeScaleTransform::Linear, false),
    ];
    let snapshot = |id: u64, value: f32| TrackSnapshot {
        id: TrackId::new(id),
        name: format!("signal-{id}"),
        data: TrackSnapshotData::Quantitative(Arc::new(rgv_hts::QuantitativeQuery {
            records: vec![rgv_hts::QuantitativeRecord {
                start: 100,
                end: 200,
                value,
            }],
            provenance: rgv_hts::QuantitativeQueryProvenance::default(),
            source_metadata: None,
        })),
    };
    let snapshots = vec![
        snapshot(1, -2.0),
        snapshot(2, 8.0),
        snapshot(3, 100.0),
        snapshot(4, 1_000.0),
    ];
    assert_eq!(
        shared_quantitative_range(
            &tracks,
            &snapshots,
            viewport,
            QuantitativeScaleTransform::Linear,
            None,
        ),
        Some(QuantitativeRange {
            minimum: -2.0,
            maximum: 8.0,
        })
    );
}

#[test]
fn quantitative_overlay_groups_preserve_track_order_and_explicit_separation() {
    let track = |id: u64, overlay_group| {
        let mut track = TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bw"),
            },
        );
        track.quantitative.overlay_group = overlay_group;
        track
    };
    let tracks = [
        track(1, Some(7)),
        track(2, None),
        track(3, Some(7)),
        track(4, Some(8)),
    ];
    let visible = tracks.iter().collect::<Vec<_>>();
    assert_eq!(
        quantitative_overlay_members(&tracks[0], &visible)
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        vec![TrackId::new(1), TrackId::new(3)]
    );
    assert_eq!(
        quantitative_overlay_members(&tracks[1], &visible)
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        vec![TrackId::new(2)]
    );
}

#[test]
fn quantitative_overlay_settings_share_the_edited_transform() {
    let settings = |group, transform| QuantitativeTrackSettings {
        overlay_group: group,
        scale_transform: transform,
        ..QuantitativeTrackSettings::default()
    };
    let mut controls = vec![
        (
            TrackId::new(1),
            settings(Some(7), QuantitativeScaleTransform::Linear),
        ),
        (
            TrackId::new(2),
            settings(Some(7), QuantitativeScaleTransform::SignedLog10),
        ),
        (
            TrackId::new(3),
            settings(Some(8), QuantitativeScaleTransform::Linear),
        ),
        (
            TrackId::new(4),
            settings(None, QuantitativeScaleTransform::SignedLog10),
        ),
    ];

    normalize_quantitative_overlay_settings(&mut controls, Some(TrackId::new(2)));

    for (_, settings) in &controls[..2] {
        assert_eq!(settings.scale_mode, QuantitativeScaleMode::Shared);
        assert_eq!(settings.scale_group, Some(7));
        assert_eq!(
            settings.scale_transform,
            QuantitativeScaleTransform::SignedLog10
        );
    }
    assert_eq!(controls[2].1.scale_group, Some(8));
    assert_eq!(
        controls[2].1.scale_transform,
        QuantitativeScaleTransform::Linear
    );
    assert_eq!(controls[3].1.scale_group, None);
}

#[test]
fn quantitative_overlay_gutter_labels_are_bounded() {
    let tracks = (0..MAX_OVERLAY_GUTTER_MEMBERS + 10)
        .map(|index| {
            let mut track = TrackConfig::new(
                TrackId::new(u64::try_from(index + 1).unwrap()),
                format!("signal-{index}"),
                TrackKind::Quantitative,
                TrackSource::Local {
                    path: format!("signal-{index}.bw"),
                },
            );
            track.quantitative.overlay_group = Some(1);
            track
        })
        .collect::<Vec<_>>();
    let members = tracks.iter().collect::<Vec<_>>();

    let subtracks = quantitative_overlay_subtracks(&members, 100.0, 2_000.0);

    assert_eq!(subtracks.len(), MAX_OVERLAY_GUTTER_MEMBERS);
    assert_eq!(subtracks.last().unwrap().label, "+11 more tracks");
    assert!(subtracks.iter().all(|subtrack| subtrack.bottom <= 2_100.0));
}

#[test]
fn quantitative_overlay_rows_keep_member_identity_and_visibility_state() {
    let mut first = TrackConfig::new(
        TrackId::new(1),
        "tumour".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Local {
            path: "tumour.bw".to_owned(),
        },
    );
    first.quantitative.overlay_group = Some(3);
    let mut second = TrackConfig::new(
        TrackId::new(2),
        "normal".to_owned(),
        TrackKind::Quantitative,
        TrackSource::Local {
            path: "normal.bw".to_owned(),
        },
    );
    second.quantitative.overlay_group = Some(3);
    second.collapsed = true;

    let rows = quantitative_overlay_subtracks(&[&first, &second], 100.0, 120.0);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].composite_member, Some(TrackId::new(1)));
    assert!(!rows[0].collapsed);
    assert_eq!(rows[1].composite_member, Some(TrackId::new(2)));
    assert!(rows[1].collapsed);

    let collapsed_rows = quantitative_overlay_subtracks(&[&first, &second], 100.0, 26.0);
    assert_eq!(collapsed_rows.len(), 1);
    assert_eq!(collapsed_rows[0].composite_member, Some(TrackId::new(1)));
}

#[test]
fn shared_quantitative_scale_groups_are_independent() {
    let config = |id: u64, group| {
        let mut track = TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bw"),
            },
        );
        track.quantitative.scale_mode = QuantitativeScaleMode::Shared;
        track.quantitative.scale_group = Some(group);
        track
    };
    let tracks = vec![config(1, 1), config(2, 1), config(3, 2)];
    let snapshot = |id: u64, value: f32| TrackSnapshot {
        id: TrackId::new(id),
        name: format!("signal-{id}"),
        data: TrackSnapshotData::Quantitative(Arc::new(rgv_hts::QuantitativeQuery {
            records: vec![rgv_hts::QuantitativeRecord {
                start: 100,
                end: 200,
                value,
            }],
            provenance: rgv_hts::QuantitativeQueryProvenance::default(),
            source_metadata: None,
        })),
    };
    let snapshots = vec![snapshot(1, -2.0), snapshot(2, 8.0), snapshot(3, 100.0)];
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    assert_eq!(
        shared_quantitative_range(
            &tracks,
            &snapshots,
            viewport,
            QuantitativeScaleTransform::Linear,
            Some(1),
        ),
        Some(QuantitativeRange {
            minimum: -2.0,
            maximum: 8.0,
        })
    );
}

#[test]
fn shared_copy_number_range_uses_only_visible_semantically_compatible_matrices() {
    let config = |id: u64, mode, visible| {
        let mut track = TrackConfig::new(
            TrackId::new(id),
            format!("cohort-{id}"),
            TrackKind::CopyNumber,
            TrackSource::Local {
                path: format!("cohort-{id}.seg"),
            },
        );
        track.visible = visible;
        track.quantitative.scale_mode = QuantitativeScaleMode::Shared;
        track.copy_number.value_mode = mode;
        track
    };
    let tracks = vec![
        config(1, CopyNumberValueMode::Absolute, true),
        config(2, CopyNumberValueMode::Absolute, true),
        config(3, CopyNumberValueMode::LogRatio, true),
        config(4, CopyNumberValueMode::Absolute, false),
    ];
    let snapshot = |id: u64, value: f32| TrackSnapshot {
        id: TrackId::new(id),
        name: format!("cohort-{id}"),
        data: TrackSnapshotData::CopyNumber(Arc::new(rgv_hts::CopyNumberQuery {
            samples: vec!["sample".to_owned()],
            segments: vec![rgv_hts::CopyNumberSegment {
                sample_index: 0,
                start: 100,
                end: 200,
                marker_count: None,
                value,
            }],
            sample_sort_values: vec![None],
            inferred_value_semantics: rgv_hts::CopyNumberValueSemantics::Absolute,
            provenance: rgv_hts::CopyNumberQueryProvenance::default(),
        })),
    };
    let snapshots = vec![
        snapshot(1, 1.0),
        snapshot(2, 5.0),
        snapshot(3, -10.0),
        snapshot(4, 100.0),
    ];
    assert_eq!(
        shared_copy_number_range(&tracks[0], &tracks, &snapshots),
        Some(QuantitativeRange {
            minimum: 1.0,
            maximum: 5.0,
        })
    );
}

#[test]
fn copy_number_inspector_reports_exact_segment_and_sort_provenance() {
    let inspector = CopyNumberInspectorSnapshot {
        track_name: "cohort".to_owned(),
        query: Arc::new(rgv_hts::CopyNumberQuery {
            samples: vec!["tumour".to_owned()],
            segments: vec![rgv_hts::CopyNumberSegment {
                sample_index: 0,
                start: 99,
                end: 200,
                marker_count: Some(42),
                value: -0.75,
            }],
            sample_sort_values: vec![Some(-0.75)],
            inferred_value_semantics: rgv_hts::CopyNumberValueSemantics::LogRatio,
            provenance: rgv_hts::CopyNumberQueryProvenance {
                records_examined: 1,
                records_retained: 1,
                sort_records_examined: 1,
                ..rgv_hts::CopyNumberQueryProvenance::default()
            },
        }),
        segment_index: 0,
        settings: CopyNumberTrackSettings {
            sort_mode: CopyNumberSortMode::Deletion,
            sort_locus: Some(CopyNumberSortLocus {
                contig: "chr1".to_owned(),
                start: 99,
                end: 200,
            }),
            ..CopyNumberTrackSettings::default()
        },
    };
    let fields = copy_number_inspector_fields(&inspector);
    assert!(fields.contains(&("Sample".to_owned(), "tumour".to_owned())));
    assert!(fields.contains(&("Markers".to_owned(), "42".to_owned())));
    assert!(fields.iter().any(|(label, value)| {
        label == "Segment" && value.contains("100-200") && value.contains("101 bp")
    }));
    assert!(
        fields.iter().any(|(label, value)| {
            label == "Sort locus" && value.contains("base-weighted mean")
        })
    );
    let document = copy_number_inspector_document(&inspector);
    assert!(document.all_fields().any(|field| {
        field.label == "Value interpretation"
            && matches!(field.provenance, InspectorProvenance::Inferred(_))
    }));
    assert_eq!(document.flatten_automation_fields(), fields);
}

#[test]
fn mutation_inspector_preserves_source_order_and_cap_provenance() {
    let inspector = MutationInspectorSnapshot {
        track_name: "somatic".to_owned(),
        contig: "chr17".to_owned(),
        query: Arc::new(rgv_hts::MutationQuery {
            headers: vec!["Hugo_Symbol".to_owned(), "HGVSp_Short".to_owned()],
            samples: vec!["TUMOUR-A".to_owned()],
            records: vec![rgv_hts::MutationRecord {
                sample_index: 0,
                start: 99,
                end: 100,
                source_start: 100,
                source_end: 100,
                insertion_boundary: false,
                gene: "TP53".to_owned(),
                classification: "Missense_Mutation".to_owned(),
                variant_type: "SNP".to_owned(),
                reference: "C".to_owned(),
                tumor_allele_one: "C".to_owned(),
                tumor_allele_two: "T".to_owned(),
                annotations: vec![
                    rgv_hts::MutationAnnotation {
                        column_index: 0,
                        value: "TP53".to_owned(),
                        truncated: false,
                    },
                    rgv_hts::MutationAnnotation {
                        column_index: 1,
                        value: "p.R175H".to_owned(),
                        truncated: true,
                    },
                ],
                annotation_values_omitted: 0,
            }],
            provenance: rgv_hts::MutationQueryProvenance {
                records_examined: 1,
                records_retained: 1,
                header_columns_omitted: 2,
                annotation_values_truncated: 1,
                ..rgv_hts::MutationQueryProvenance::default()
            },
        }),
        record_index: 0,
    };

    let fields = mutation_inspector_fields(&inspector);

    assert!(fields.contains(&("Sample".to_owned(), "TUMOUR-A".to_owned())));
    assert!(fields.contains(&("MAF · Hugo_Symbol".to_owned(), "TP53".to_owned())));
    assert!(fields.iter().any(|(label, value)| {
        label == "MAF · HGVSp_Short" && value.contains("value truncated")
    }));
    assert!(fields.iter().any(|(label, value)| {
        label == "Annotation provenance"
            && value.contains("2 omitted header columns")
            && value.contains("1 truncated/omitted values")
    }));
    let document = mutation_inspector_document(&inspector);
    assert_eq!(
        document
            .raw_fields
            .iter()
            .map(|field| field.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Hugo_Symbol", "HGVSp_Short"]
    );
    assert!(matches!(
        document.raw_fields[1].provenance,
        InspectorProvenance::Truncated { total: None, .. }
    ));
    assert!(
        document
            .notices
            .iter()
            .any(|notice| notice.id == "mutation-resource-bounds")
    );
    assert_eq!(document.flatten_automation_fields(), fields);
}

#[test]
fn feature_picking_uses_the_same_packed_row_as_rendering() {
    let record = |start, end, name: &str| rgv_hts::FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(name.to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: rgv_hts::Strand::Forward,
        blocks: vec![(start, end)],
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let records = vec![record(100, 300, "row-0"), record(150, 250, "row-1")];
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 300).unwrap(),
        1_000.0,
    )
    .unwrap();

    assert_eq!(
        feature_record_index_at_position_and_row(
            &records,
            viewport,
            200,
            0,
            12,
            FeatureDisplayMode::Expanded
        ),
        Some(0)
    );
    assert_eq!(
        feature_record_index_at_position_and_row(
            &records,
            viewport,
            200,
            1,
            12,
            FeatureDisplayMode::Expanded
        ),
        Some(1)
    );
    assert_eq!(
        feature_record_index_at_position_and_row(
            &records,
            viewport,
            300,
            0,
            12,
            FeatureDisplayMode::Expanded
        ),
        None
    );
    assert_eq!(
        feature_record_index_at_position_and_row(
            &records,
            viewport,
            200,
            0,
            1,
            FeatureDisplayMode::Collapsed
        ),
        Some(1)
    );
}

#[test]
fn pinned_gene_picking_uses_the_rendered_row_count() {
    let height = 154.386_72;
    let layout_maximum_rows = pinned_gene_layout_maximum_rows(height);
    let rendered_rows = feature_layout_row_count(layout_maximum_rows, FeatureDisplayMode::Expanded);
    let third_row_pointer = 2.5 * height / rendered_rows as f32;
    let picked_row = (third_row_pointer / (height / rendered_rows as f32)).floor() as usize;
    let records = (0..7)
        .map(|index| rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 100,
            end: 300,
            name: Some(format!("row-{index}")),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Forward,
            blocks: vec![(100, 300)],
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        })
        .collect::<Vec<_>>();
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 300).unwrap(),
        1_000.0,
    )
    .unwrap();

    assert_eq!(layout_maximum_rows, 7);
    assert_eq!(picked_row, 2);
    assert_eq!(
        feature_record_index_at_position_and_row(
            &records,
            viewport,
            200,
            picked_row,
            rendered_rows,
            FeatureDisplayMode::Expanded,
        ),
        Some(2)
    );
}

#[test]
fn gene_picking_uses_the_exact_rendered_primitive_geometry() {
    let records = (0..6)
        .map(|index| rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 100,
            end: 300,
            name: Some(format!("row-{index}")),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Forward,
            blocks: vec![(120, 180), (240, 280)],
            coding_interval: Some((130, 270)),
            peak: None,
            attributes: Vec::new(),
        })
        .collect::<Vec<_>>();
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 300).unwrap(),
        1_000.0,
    )
    .unwrap();
    let object_id_base = 50;
    let display = layout_features(
        &records,
        viewport,
        TrackLayoutOptions {
            top: 0.0,
            height: 48.0,
            maximum_rows: 6,
            object_id_base,
            feature_display_mode: FeatureDisplayMode::Expanded,
            ..TrackLayoutOptions::default()
        },
    )
    .0;
    let third_row_rectangle = display
        .rectangles
        .iter()
        .find(|rectangle| rectangle.object_id == object_id_base + 2 && rectangle.height > 1.0)
        .unwrap();
    let picked = feature_record_index_at_display_point(
        &display,
        &records,
        third_row_rectangle.x + third_row_rectangle.width * 0.5,
        third_row_rectangle.y + third_row_rectangle.height * 0.5,
        object_id_base,
    );
    assert_eq!(picked, Some(2));

    assert_eq!(
        feature_record_index_at_display_point(
            &display,
            &records,
            third_row_rectangle.x + third_row_rectangle.width * 0.5,
            47.5,
            object_id_base,
        ),
        None,
    );
}

#[test]
fn feature_inspector_reports_gene_models_and_peak_statistics() {
    let inspector = FeatureInspectorSnapshot {
        track_name: "peaks".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 99,
            end: 200,
            name: Some("peak-1".to_owned()),
            feature_type: Some("narrowPeak".to_owned()),
            score: Some(750.0),
            strand: rgv_hts::Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: Some(rgv_hts::PeakMetrics {
                signal_value: 18.25,
                p_value: Some(5.0),
                q_value: None,
                summit: Some(149),
            }),
            attributes: Vec::new(),
        },
    };
    let fields = feature_inspector_fields(&inspector);

    assert!(fields.contains(&("Signal value".to_owned(), "18.25".to_owned())));
    assert!(fields.iter().any(|(label, value)| {
        label == "Called summit" && value.contains("chr1:150") && value.contains("offset 50")
    }));
    assert!(
        fields
            .iter()
            .any(|(label, value)| { label == "Location" && value.contains("chr1:100-200") })
    );
}

#[test]
fn builtin_gene_properties_include_model_and_bundle_provenance() {
    let inspector = FeatureInspectorSnapshot {
        track_name: "Genes".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 100,
            end: 400,
            name: Some("TP53".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Reverse,
            blocks: vec![(100, 150), (220, 260), (350, 400)],
            coding_interval: Some((120, 380)),
            peak: None,
            attributes: vec![
                ("gene_name".to_owned(), "TP53".to_owned()),
                ("gene_id".to_owned(), "7157".to_owned()),
                ("transcript_id".to_owned(), "NM_000546.6".to_owned()),
                ("gene_biotype".to_owned(), "protein_coding".to_owned()),
                ("landmark_bundle_id".to_owned(), "hg38".to_owned()),
                ("landmark_bundle_version".to_owned(), "1.0-rgv1".to_owned()),
                (
                    "landmark_asset_role".to_owned(),
                    "representative_genes".to_owned(),
                ),
                ("landmark_asset_sha256".to_owned(), "a".repeat(64)),
                (
                    "landmark_selection_policy".to_owned(),
                    "MANE Select".to_owned(),
                ),
            ],
        },
    };
    let document = feature_inspector_document(&inspector);
    assert_eq!(document.header.title, "TP53");
    assert_eq!(
        document.header.subtitle.as_deref(),
        Some("transcript · NM_000546.6")
    );
    let gene = document
        .sections
        .iter()
        .find(|section| section.id == "gene")
        .unwrap();
    assert!(gene.fields.iter().any(|field| {
        field.label == "Gene symbol" && field.value == InspectorValue::Identifier("TP53".to_owned())
    }));
    assert!(gene.fields.iter().any(|field| {
        field.label == "Gene ID" && field.value == InspectorValue::Identifier("7157".to_owned())
    }));
    let transcript = document
        .sections
        .iter()
        .find(|section| section.id == "transcript")
        .unwrap();
    assert!(transcript.fields.iter().any(|field| {
        field.label == "Transcript ID"
            && field.value == InspectorValue::Identifier("NM_000546.6".to_owned())
    }));
    let overview = document
        .sections
        .iter()
        .find(|section| section.id == "overview")
        .unwrap();
    assert!(overview.fields.iter().any(|field| field.id == "exon-count"));
    assert!(
        overview
            .fields
            .iter()
            .any(|field| field.id == "coding-status")
    );
    assert!(
        document
            .sections
            .iter()
            .any(|section| section.id == "landmark-source")
    );
    assert!(document.actions.iter().any(|action| {
        action.id == InspectorActionId::ShowAllTranscriptsForGene("hg38".to_owned())
    }));
}

#[test]
fn feature_inspector_labels_psl_blocks_and_preserves_alignment_counts() {
    let inspector = FeatureInspectorSnapshot {
        track_name: "PSL".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 100,
            end: 190,
            name: Some("query-1".to_owned()),
            feature_type: Some("PSL alignment".to_owned()),
            score: Some(937.5),
            strand: rgv_hts::Strand::Forward,
            blocks: vec![(100, 130), (140, 190)],
            coding_interval: None,
            peak: None,
            attributes: vec![("matches".to_owned(), "70".to_owned())],
        },
    };
    let fields = feature_inspector_fields(&inspector);
    assert!(
        fields
            .iter()
            .any(|(label, value)| label == "Alignment blocks (2)" && value.contains("101-130"))
    );
    assert!(fields.contains(&("Attribute · matches".to_owned(), "70".to_owned())));
}

#[test]
fn feature_document_promotes_gencode_semantics_and_preserves_duplicate_tags() {
    let inspector = FeatureInspectorSnapshot {
        track_name: "GENCODE v29".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 36_339_623,
            end: 36_341_973,
            name: Some("STK40".to_owned()),
            feature_type: Some("UTR".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Reverse,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: vec![
                ("gene_id".to_owned(), "ENSG00000196182.10".to_owned()),
                ("gene_name".to_owned(), "STK40".to_owned()),
                ("transcript_id".to_owned(), "ENST00000373130.7".to_owned()),
                ("transcript_name".to_owned(), "STK40-203".to_owned()),
                ("transcript_type".to_owned(), "protein_coding".to_owned()),
                ("transcript_support_level".to_owned(), "1".to_owned()),
                ("exon_number".to_owned(), "11".to_owned()),
                ("tag".to_owned(), "basic".to_owned()),
                ("tag".to_owned(), "CCDS".to_owned()),
                ("tag".to_owned(), "appris_principal_1".to_owned()),
            ],
        },
    };
    let legacy = feature_inspector_fields(&inspector);
    let document = feature_inspector_document(&inspector);

    assert_eq!(document.header.title, "STK40");
    assert_eq!(
        document.header.subtitle.as_deref(),
        Some("UTR · STK40-203 · exon 11")
    );
    assert!(document.sections.iter().any(|section| section.id == "gene"));
    assert!(
        document
            .sections
            .iter()
            .any(|section| section.id == "transcript")
    );
    let tags = document
        .sections
        .iter()
        .find(|section| section.id == "tags")
        .and_then(|section| section.fields.first())
        .map(|field| &field.value);
    assert_eq!(
        tags,
        Some(&InspectorValue::Tags(vec![
            "Basic".to_owned(),
            "CCDS".to_owned(),
            "APPRIS principal 1".to_owned(),
        ]))
    );
    assert_eq!(
        document
            .raw_fields
            .iter()
            .filter(|field| field.label == "tag")
            .count(),
        3
    );
    assert_eq!(document.flatten_automation_fields(), legacy);
}

#[test]
fn feature_document_fallback_keeps_blocks_unknown_attributes_and_complete_copy_values() {
    let long_value = "ACGT".repeat(1_200);
    let inspector = FeatureInspectorSnapshot {
        track_name: "custom BED12".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr7".to_owned(),
            start: 100,
            end: 260,
            name: None,
            feature_type: None,
            score: None,
            strand: rgv_hts::Strand::Forward,
            blocks: vec![(100, 140), (200, 260)],
            coding_interval: Some((110, 240)),
            peak: None,
            attributes: vec![
                ("custom".to_owned(), "first".to_owned()),
                ("custom".to_owned(), long_value.clone()),
            ],
        },
    };
    let document = feature_inspector_document(&inspector);

    assert_eq!(document.header.title, "Feature");
    assert!(document.header.location.is_some());
    assert!(document.sections.iter().any(|section| {
        section.fields.iter().any(|field| {
            field.label == "Blocks / exons"
                && matches!(&field.value, InspectorValue::List(blocks) if blocks.len() == 2)
        })
    }));
    assert!(
        !document
            .sections
            .iter()
            .any(|section| { section.fields.iter().any(|field| field.label == "Score") })
    );
    assert_eq!(document.raw_fields.len(), 2);
    assert_eq!(document.raw_fields[1].complete_copy_value(), long_value);
    assert!(document.copy_details_text().contains(&long_value));
    assert!(matches!(
        document.raw_fields[1].provenance,
        InspectorProvenance::Truncated { .. }
    ));
}

#[test]
fn feature_documents_cover_gene_transcript_exon_cds_and_utr_contexts() {
    for feature_type in ["gene", "transcript", "exon", "CDS", "UTR"] {
        let inspector = FeatureInspectorSnapshot {
            track_name: "GENCODE".to_owned(),
            record: rgv_hts::FeatureRecord {
                contig: "chr3".to_owned(),
                start: 10,
                end: 40,
                name: None,
                feature_type: Some(feature_type.to_owned()),
                score: None,
                strand: rgv_hts::Strand::Forward,
                blocks: Vec::new(),
                coding_interval: None,
                peak: None,
                attributes: vec![
                    ("gene_id".to_owned(), "ENSG00000000001.2".to_owned()),
                    ("gene_name".to_owned(), "GENE1".to_owned()),
                    ("transcript_id".to_owned(), "ENST00000000001.3".to_owned()),
                    ("transcript_name".to_owned(), "GENE1-201".to_owned()),
                    ("exon_id".to_owned(), "ENSE00000000001.1".to_owned()),
                    ("exon_number".to_owned(), "2".to_owned()),
                ],
            },
        };
        let document = feature_inspector_document(&inspector);

        assert!(document.sections.iter().any(|section| section.id == "gene"));
        assert!(
            document
                .sections
                .iter()
                .any(|section| section.id == "transcript")
        );
        if matches!(feature_type, "exon" | "CDS" | "UTR") {
            let context = document
                .sections
                .iter()
                .find(|section| section.id == "feature-context")
                .unwrap();
            assert!(context.fields.iter().any(|field| {
                field.id == "feature-context-type" && field.value.display_text() == feature_type
            }));
        }
    }
}

#[test]
fn feature_documents_keep_genepred_and_refflat_model_geometry() {
    for track_name in ["genes.genePred", "genes.refFlat"] {
        let inspector = FeatureInspectorSnapshot {
            track_name: track_name.to_owned(),
            record: rgv_hts::FeatureRecord {
                contig: "chr12".to_owned(),
                start: 100,
                end: 500,
                name: Some("NM_000001".to_owned()),
                feature_type: Some("transcript".to_owned()),
                score: None,
                strand: rgv_hts::Strand::Reverse,
                blocks: vec![(100, 180), (300, 500)],
                coding_interval: Some((130, 450)),
                peak: None,
                attributes: vec![("gene_name".to_owned(), "GENE2".to_owned())],
            },
        };
        let document = feature_inspector_document(&inspector);

        let overview = document
            .sections
            .iter()
            .find(|section| section.id == "overview")
            .unwrap();
        assert!(overview.fields.iter().any(|field| {
            field.label == "Blocks / exons"
                && matches!(&field.value, InspectorValue::List(blocks) if blocks == &vec!["101–180".to_owned(), "301–500".to_owned()])
        }));
        assert!(overview.fields.iter().any(|field| {
            field.label == "Coding sequence"
                && matches!(&field.value, InspectorValue::GenomicLocation(_))
        }));
        assert_eq!(document.header.source.as_deref(), Some(track_name));
    }
}

#[test]
fn interaction_document_uses_semantic_endpoints_and_preserves_actions_and_compatibility() {
    let inspector = InteractionInspectorSnapshot {
        track_name: "loops".to_owned(),
        query: Arc::new(rgv_hts::InteractionQuery {
            records: vec![rgv_hts::InteractionRecord {
                source: rgv_hts::InteractionEndpoint {
                    contig: "chr1".to_owned(),
                    start: 100,
                    end: 120,
                    name: Some("promoter".to_owned()),
                    strand: rgv_hts::Strand::Forward,
                },
                target: rgv_hts::InteractionEndpoint {
                    contig: "chr8".to_owned(),
                    start: 800,
                    end: 850,
                    name: Some("enhancer".to_owned()),
                    strand: rgv_hts::Strand::Reverse,
                },
                name: Some("loop-1".to_owned()),
                score: Some(42.0),
                value: Some(0.75),
                experiment: Some("Hi-C".to_owned()),
                color: None,
                directional: true,
            }],
            provenance: rgv_hts::InteractionQueryProvenance {
                endpoint_entries_examined: 100_001,
                records_truncated: true,
                ..rgv_hts::InteractionQueryProvenance::default()
            },
        }),
        record_index: 0,
        settings: InteractionTrackSettings::default(),
    };
    let compatibility = interaction_inspector_fields(&inspector);
    let document = interaction_inspector_presentation_document(&inspector, 0);

    let endpoints = document
        .sections
        .iter()
        .find(|section| section.id == "interaction-endpoints")
        .unwrap();
    assert_eq!(endpoints.fields.len(), 2);
    assert!(
        endpoints
            .fields
            .iter()
            .all(|field| matches!(&field.value, InspectorValue::GenomicLocation(_)))
    );
    assert!(
        document
            .actions
            .iter()
            .filter(|action| {
                matches!(action.id, InspectorActionId::OpenInteractionEndpoint(_))
                    && !action.enabled
            })
            .count()
            == 2
    );
    assert!(
        document
            .notices
            .iter()
            .any(|notice| notice.id == "interaction-query-bound")
    );
    assert_eq!(document.flatten_automation_fields(), compatibility);
}
