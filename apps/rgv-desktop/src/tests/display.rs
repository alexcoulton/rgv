use super::super::*;
use crate::display::alignment::{ReadPanelKey, read_panel_bounds, read_panel_bounds_for_track};
use crate::display::{
    ANNOTATION_PANE_BACKGROUND, COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT,
    PINNED_ALIGNMENT_COVERAGE_HEIGHT, PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT,
    PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT, PINNED_ALIGNMENT_MODIFICATION_HEIGHT,
    PINNED_ALIGNMENT_REFERENCE_HEIGHT, alignment_annotation_lane_heights,
    append_annotation_pane_background, pinned_gene_track_height, primary_pinned_gene_id,
};

#[test]
fn read_panels_tile_the_read_pane_with_independent_viewport_heights() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let bounds = read_panel_bounds(
        &[
            (ReadPanelKey::Track(first), true, Some(300.0)),
            (ReadPanelKey::Track(second), true, Some(100.0)),
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
fn resizing_one_read_panel_reallocates_its_own_scrollbar_segment() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let equal = read_panel_bounds(
        &[
            (ReadPanelKey::Track(first), true, Some(100.0)),
            (ReadPanelKey::Track(second), true, Some(100.0)),
        ],
        200.0,
        600.0,
    );
    let resized = read_panel_bounds(
        &[
            (ReadPanelKey::Track(first), true, Some(300.0)),
            (ReadPanelKey::Track(second), true, Some(100.0)),
        ],
        200.0,
        600.0,
    );

    assert_eq!(equal[0].bottom - equal[0].top, 200.0);
    assert_eq!(equal[1].bottom - equal[1].top, 200.0);
    assert_eq!(resized[0].bottom - resized[0].top, 300.0);
    assert_eq!(resized[1].bottom - resized[1].top, 100.0);
    assert_eq!(resized.last().map(|panel| panel.bottom), Some(600.0));
}

#[test]
fn read_group_header_reserves_only_read_pane_space() {
    let first = TrackId::new(101);
    let second = TrackId::new(202);
    let bounds = read_panel_bounds(
        &[
            (ReadPanelKey::Group(7), false, None),
            (ReadPanelKey::Track(first), true, Some(100.0)),
            (ReadPanelKey::Track(second), true, Some(100.0)),
        ],
        200.0,
        600.0,
    );

    assert_eq!(bounds[0].key, ReadPanelKey::Group(7));
    assert_eq!(
        bounds[0].bottom - bounds[0].top,
        COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT
    );
    assert_eq!(bounds[1].key, ReadPanelKey::Track(first));
    assert_eq!(bounds[2].key, ReadPanelKey::Track(second));
    assert_eq!(bounds[0].bottom, bounds[1].top);
    assert_eq!(bounds[1].bottom, bounds[2].top);
    assert_eq!(bounds[2].bottom, 600.0);
}

#[test]
fn collapsed_alignment_without_a_read_panel_keeps_annotation_header_bounds() {
    let track_id = TrackId::new(303);
    let fallback = read_panel_bounds_for_track(&[], track_id, 600.0, true).unwrap();

    assert_eq!(fallback.key, ReadPanelKey::Track(track_id));
    assert_eq!((fallback.top, fallback.bottom), (600.0, 600.0));
    assert!(read_panel_bounds_for_track(&[], track_id, 600.0, false).is_none());
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

    assert!((PINNED_ALIGNMENT_REFERENCE_HEIGHT - 28.0).abs() < f32::EPSILON);
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
fn overview_read_detail_distinguishes_present_empty_and_incomplete_data() {
    assert_eq!(
        overview_read_detail(31, false),
        "31 present · hidden at this zoom"
    );
    assert_eq!(overview_read_detail(0, false), "no alignments in region");
    assert_eq!(
        overview_read_detail(31, true),
        "31+ found · overview incomplete"
    );
}

#[test]
fn filtered_read_details_report_ignored_unmapped_records() {
    let details = filtered_read_details(rgv_hts::QueryStats {
        records_filtered_unmapped: 3,
        ..rgv_hts::QueryStats::default()
    });
    assert_eq!(details, ["3 unmapped ignored"]);
}

#[test]
fn alignment_layer_partition_keeps_the_header_out_of_the_read_scroll() {
    let primitive = |y, object_id| rgv_layout::RectPrimitive {
        x: 0.0,
        y,
        width: 10.0,
        height: 5.0,
        color: rgv_layout::Rgba::new(0.5, 0.5, 0.5, 1.0),
        object_id,
        shape: Default::default(),
    };
    let text = |y, value: &str| rgv_layout::TextPrimitive {
        x: 5.0,
        y,
        text: value.to_owned(),
        font_size: 10.0,
        color: rgv_layout::Rgba::new(0.0, 0.0, 0.0, 1.0),
    };
    let layout = AlignmentLayout {
        display_list: DisplayList {
            rectangles: vec![primitive(120.0, 0), primitive(280.0, 1)],
            texts: vec![text(120.0, "A"), text(280.0, "G")],
        },
        pinned_rectangle_count: 1,
        pinned_text_count: 1,
        stats: AlignmentLayoutStats::default(),
    };
    let mut pinned = DisplayList::default();
    let mut scrollable = DisplayList::default();

    extend_alignment_layers(&mut pinned, &mut scrollable, &layout, None);

    assert_eq!(pinned.rectangles, vec![primitive(120.0, 0)]);
    assert_eq!(pinned.texts, vec![text(120.0, "A")]);
    assert_eq!(scrollable.rectangles, vec![primitive(280.0, 1)]);
    assert_eq!(scrollable.texts, vec![text(280.0, "G")]);
}

#[test]
fn alignment_layer_partition_clips_reads_to_the_resized_group() {
    let layout = AlignmentLayout {
        display_list: DisplayList {
            rectangles: vec![rgv_layout::RectPrimitive {
                x: 0.0,
                y: 280.0,
                width: 10.0,
                height: 10.0,
                color: rgv_layout::Rgba::new(0.5, 0.5, 0.5, 1.0),
                object_id: 1,
                shape: Default::default(),
            }],
            texts: Vec::new(),
        },
        pinned_rectangle_count: 0,
        pinned_text_count: 0,
        stats: AlignmentLayoutStats::default(),
    };
    let mut pinned = DisplayList::default();
    let mut scrollable = DisplayList::default();

    extend_alignment_layers(&mut pinned, &mut scrollable, &layout, Some((250.0, 284.0)));

    assert_eq!(scrollable.rectangles.len(), 1);
    assert!((scrollable.rectangles[0].height - 4.0).abs() < f32::EPSILON);
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
fn read_detail_primitives_remain_pickable_as_their_parent_record() {
    assert_eq!(alignment_record_index(1), Some(0));
    assert_eq!(
        alignment_record_index(rgv_layout::read_detail_object_id(7)),
        Some(6)
    );
    assert_eq!(alignment_record_index(0), None);
    assert_eq!(alignment_record_index(1_000_000), None);
}

#[test]
fn alignment_picking_routes_by_stable_track_frame_before_record_id() {
    let frames = vec![
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(10)),
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
    let display = DisplayList {
        rectangles: vec![rgv_layout::RectPrimitive {
            x: 50.0,
            y: 225.0,
            width: 100.0,
            height: 10.0,
            color: rgv_layout::Rgba::new(0.5, 0.5, 0.5, 1.0),
            object_id: 8,
            shape: Default::default(),
        }],
        texts: Vec::new(),
    };

    assert_eq!(
        alignment_track_at_content_y(&frames, 225.0),
        Some(TrackId::new(11))
    );
    assert_eq!(alignment_record_at_point(&display, 75.0, 230.0), Some(7));
    assert_eq!(alignment_record_at_point(&display, 25.0, 230.0), None);
}

#[test]
fn alignment_lane_picking_distinguishes_scrolled_read_groups() {
    let frames = vec![
        TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(10)),
            name: "Tumour".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 300.0,
            bottom: 500.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(322.0),
            subtracks: vec![TrackSubtrack {
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
            name: "Normal".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 500.0,
            bottom: 800.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(522.0),
            subtracks: vec![TrackSubtrack {
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
            &frames, "Reads", 350.0, 0.0, 0.0, 300.0, 100.0, 100.0, 800.0,
        ),
        Some(TrackId::new(10))
    );
    assert_eq!(
        alignment_lane_at_screen_y(
            &frames, "Reads", 350.0, 200.0, 0.0, 300.0, 100.0, 100.0, 800.0,
        ),
        Some(TrackId::new(11))
    );
    assert_eq!(
        alignment_lane_at_screen_y(
            &frames, "Reads", 250.0, 200.0, 0.0, 300.0, 100.0, 100.0, 800.0,
        ),
        None,
        "a read lane must not be pickable through the annotation pane"
    );
}

#[test]
fn collapsed_alignment_lanes_are_not_pickable() {
    let frames = vec![TrackFrame {
        key: TrackFrameKey::Alignment(TrackId::new(10)),
        name: "Tumour".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 180.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(180.0),
        subtracks: vec![TrackSubtrack {
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
            &frames, "Coverage", 130.0, 0.0, 0.0, 180.0, 180.0, 100.0, 500.0,
        ),
        None
    );
}

#[test]
fn layout_cache_distinguishes_prefetch_query_intervals() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let narrow = GenomicInterval::new(ContigId::new(0), 50, 250).unwrap();
    let broad = GenomicInterval::new(ContigId::new(0), 0, 500).unwrap();
    let filters = ActiveFilters {
        alignment: AlignmentFilter::default(),
        include_secondary: false,
        include_supplementary: false,
    };
    let options = AlignmentLayoutOptions::default();
    let narrow_key = alignment_layout_key(
        PanelTrackId::new(PanelId::new(1), TrackId::new(1)),
        None,
        viewport,
        narrow,
        &filters,
        &options,
        126.0,
    );
    let broad_key = alignment_layout_key(
        PanelTrackId::new(PanelId::new(1), TrackId::new(1)),
        None,
        viewport,
        broad,
        &filters,
        &options,
        126.0,
    );
    assert_ne!(narrow_key, broad_key);
}

#[test]
fn layout_cache_identity_includes_panel_and_track() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let filters = ActiveFilters {
        alignment: AlignmentFilter::default(),
        include_secondary: false,
        include_supplementary: false,
    };
    let key = |panel, track| {
        alignment_layout_key(
            PanelTrackId::new(PanelId::new(panel), TrackId::new(track)),
            None,
            viewport,
            viewport.interval(),
            &filters,
            &AlignmentLayoutOptions::default(),
            126.0,
        )
    };

    assert_ne!(key(1, 1), key(2, 1));
    assert_ne!(key(1, 1), key(1, 2));
}

#[test]
fn layout_cache_distinguishes_subtrack_visibility_and_coverage_scale() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let interval = viewport.interval();
    let filters = ActiveFilters {
        alignment: AlignmentFilter::default(),
        include_secondary: false,
        include_supplementary: false,
    };
    let baseline = AlignmentLayoutOptions::default();
    let baseline_key = alignment_layout_key(
        PanelTrackId::new(PanelId::new(1), TrackId::new(1)),
        None,
        viewport,
        interval,
        &filters,
        &baseline,
        126.0,
    );
    for changed in [
        AlignmentLayoutOptions {
            show_reference: false,
            ..baseline
        },
        AlignmentLayoutOptions {
            show_coverage: false,
            ..baseline
        },
        AlignmentLayoutOptions {
            show_reads: false,
            ..baseline
        },
        AlignmentLayoutOptions {
            coverage_scale_maximum: Some(100),
            ..baseline
        },
        AlignmentLayoutOptions {
            promoted_read_name_hash: Some(alignment_read_name_hash(b"selected")),
            ..baseline
        },
    ] {
        assert_ne!(
            baseline_key,
            alignment_layout_key(
                PanelTrackId::new(PanelId::new(1), TrackId::new(1)),
                None,
                viewport,
                interval,
                &filters,
                &changed,
                126.0,
            )
        );
    }
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
