use crate::ui::CanvasGeometry;
use crate::ui::tracks::chrome::{
    paint_track_viewport_chrome, paint_track_viewport_chrome_with_groups,
};
use crate::ui::tracks::geometry::{
    VerticalScreenBounds, boundary_inside_clip, clipped_frame_screen_bounds, frame_screen_bounds,
    reference_orientation_accessibility, subtrack_screen_bounds,
};
use crate::ui::tracks::gutter::{
    GUTTER_TEXT_GAP, TrackRenameState, alignment_subtrack_has_independent_visibility,
    begin_track_rename, editable_track_name, group_chevron_segments, gutter_row_widths,
    layout_elided_text, track_header_label_geometry, track_rename_state_id,
};
use crate::ui::tracks::resize::{keyboard_reorder_index, resized_track_height, track_drop_index};
use crate::ui::tracks::scrollbar::{
    paint_annotation_scrollbar, paint_vertical_scrollbar, scrollbar_geometry,
};
use crate::ui::tracks::scrollbar::{
    scrollbar_geometry_for_density, scrollbar_geometry_for_density_at_fraction,
    scrollbar_offset_at_pointer, scrollbar_overview_bin_rect,
};
use crate::ui::{
    AlignmentSubtrack, ScrollbarInteraction, TrackFrame, TrackFrameKey, TrackGroupChrome,
    TrackPane, TrackSectionChrome, TrackSubtrack, TrackUiAction, frame_content_y_at_screen_y,
    paint_read_panel_scrollbar, paint_track_viewport_chrome_with_sections,
    resized_read_pane_height, resized_read_track_height, scrollbar_fraction_at_grab,
    scrollbar_geometry_for_density_in_viewport, scrollbar_offset_at_grab, scrollbar_overview,
};
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY};
use egui::{Color32, FontId, Rect, Ui, pos2};
use rgv_core::{PanelId, PresentationNodeId, TrackChannel, TrackId, TrackKind};
use rgv_layout::DisplayList;

#[test]
fn annotation_boundaries_cannot_enter_the_read_pane() {
    assert!(boundary_inside_clip(240.0, 100.0, 300.0));
    assert!(boundary_inside_clip(300.0, 100.0, 300.0));
    assert!(!boundary_inside_clip(301.0, 100.0, 300.0));
    assert!(!boundary_inside_clip(480.0, 100.0, 300.0));
}

#[test]
fn reference_orientation_indicator_has_accessible_names_and_tooltips() {
    assert_eq!(
        reference_orientation_accessibility("> Bases · forward"),
        Some((
            "Reference sequence, forward genomic direction",
            "Reference sequence is displayed in the forward genomic direction"
        ))
    );
    assert_eq!(
        reference_orientation_accessibility("< Bases · reverse strand"),
        Some((
            "Reference sequence, reverse-complement direction",
            "Reference sequence is displayed as the reverse complement"
        ))
    );
    assert_eq!(reference_orientation_accessibility("Bases"), None);
}

#[test]
fn alignment_subtrack_identity_does_not_depend_on_mutable_label_text() {
    let subtrack = TrackSubtrack {
        alignment_subtrack: Some(AlignmentSubtrack::Coverage),
        label: "Renamed signal lane".to_owned(),
        detail: None,
        top: 10.0,
        bottom: 30.0,
        collapsed: false,
        composite_member: None,
    };
    assert_eq!(
        subtrack.alignment_subtrack,
        Some(AlignmentSubtrack::Coverage)
    );
}

fn read_frame_with_status(track_id: TrackId) -> TrackFrame {
    TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Reads,
        name: "Reads · Tumour paired".to_owned(),
        kind: TrackKind::Alignment,
        source_label: Some("tumour-paired.bam".to_owned()),
        top: 20.0,
        bottom: 180.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(42.0),
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Reads),
            label: "Reads".to_owned(),
            detail: Some("159 visible · 211 loaded".to_owned()),
            top: 42.0,
            bottom: 180.0,
            collapsed: false,
            composite_member: None,
        }],
    }
}

#[test]
fn read_status_row_does_not_own_a_second_visibility_control() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let frame = read_frame_with_status(TrackId::new(9));
    assert!(!alignment_subtrack_has_independent_visibility(
        &frame,
        AlignmentSubtrack::Reads,
    ));

    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 240.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let _ = paint_track_viewport_chrome(
                ui,
                geometry,
                std::slice::from_ref(&frame),
                0.0,
                0.0,
                0.0,
                geometry.viewport.top(),
                None,
                &[],
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    let visibility_labels = update
        .nodes
        .iter()
        .filter_map(|(_, node)| {
            (node.role() == egui::accesskit::Role::CheckBox)
                .then(|| node.label())
                .flatten()
        })
        .filter(|label| label.contains("read content") || label.contains("Reads subtrack"))
        .collect::<Vec<_>>();

    assert_eq!(
        visibility_labels,
        vec!["Reads · Tumour paired read content visible"]
    );
}

#[test]
fn read_status_eye_column_is_non_interactive() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
    let frame = read_frame_with_status(TrackId::new(9));
    let pointer = pos2(11.0, 51.0);
    let render = |ui: &mut Ui| {
        let geometry = CanvasGeometry::from_viewport(ui.max_rect());
        paint_track_viewport_chrome(
            ui,
            geometry,
            std::slice::from_ref(&frame),
            0.0,
            0.0,
            0.0,
            geometry.viewport.top(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert!(actions.is_empty());
}

#[test]
fn read_track_inline_rename_edits_the_shared_source_name() {
    let frame = read_frame_with_status(TrackId::new(9));
    let state = begin_track_rename(&frame).unwrap();

    assert_eq!(state.key, TrackFrameKey::Alignment(TrackId::new(9)));
    assert_eq!(state.pane, TrackPane::Reads);
    assert_eq!(state.draft, "Tumour paired");
    assert!(
        editable_track_name(&TrackFrame {
            key: TrackFrameKey::Reference,
            channel: TrackPane::Annotation,
            name: "Reference sequence".to_owned(),
            ..frame
        })
        .is_none()
    );
}

#[test]
fn double_clicking_read_track_name_begins_inline_rename() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
    let frame = read_frame_with_status(TrackId::new(9));
    let pointer = pos2(80.0, 31.0);
    let render = |input: egui::RawInput| {
        context.run_ui(input, |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let _ = paint_track_viewport_chrome(
                ui,
                geometry,
                std::slice::from_ref(&frame),
                0.0,
                0.0,
                0.0,
                geometry.viewport.top(),
                None,
                &[],
            );
        })
    };
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen_rect),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };

    let _ = render(input(0.0, vec![egui::Event::PointerMoved(pointer)]));
    for (time, pressed) in [(0.10, true), (0.11, false), (0.20, true), (0.21, false)] {
        let _ = render(input(
            time,
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }],
        ));
    }

    let state = context
        .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
        .unwrap();
    assert_eq!(state.key, TrackFrameKey::Alignment(TrackId::new(9)));
    assert_eq!(state.pane, TrackPane::Reads);
    assert_eq!(state.draft, "Tumour paired");
}

#[test]
fn enter_commits_inline_rename_as_one_canonical_action() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
    let frame = read_frame_with_status(TrackId::new(9));
    context.data_mut(|data| {
        data.insert_temp(
            track_rename_state_id(),
            TrackRenameState {
                key: frame.key,
                pane: frame.channel,
                draft: "  Relapse tumour  ".to_owned(),
                request_focus: true,
            },
        );
    });
    let render = |input: egui::RawInput, actions: &mut Vec<TrackUiAction>| {
        context.run_ui(input, |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            *actions = paint_track_viewport_chrome(
                ui,
                geometry,
                std::slice::from_ref(&frame),
                0.0,
                0.0,
                0.0,
                geometry.viewport.top(),
                None,
                &[],
            )
            .actions;
        })
    };

    let mut actions = Vec::new();
    let _ = render(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        &mut actions,
    );
    let _ = render(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        &mut actions,
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::RenameTrack(
            TrackFrameKey::Alignment(TrackId::new(9)),
            "Relapse tumour".to_owned(),
        )]
    );
    assert!(
        context
            .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
            .is_none()
    );
}

#[test]
fn escape_cancels_inline_rename_without_an_action() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
    let frame = read_frame_with_status(TrackId::new(9));
    context.data_mut(|data| {
        data.insert_temp(
            track_rename_state_id(),
            TrackRenameState {
                key: frame.key,
                pane: frame.channel,
                draft: "Uncommitted name".to_owned(),
                request_focus: true,
            },
        );
    });
    let render = |input: egui::RawInput, actions: &mut Vec<TrackUiAction>| {
        context.run_ui(input, |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            *actions = paint_track_viewport_chrome(
                ui,
                geometry,
                std::slice::from_ref(&frame),
                0.0,
                0.0,
                0.0,
                geometry.viewport.top(),
                None,
                &[],
            )
            .actions;
        })
    };

    let mut actions = Vec::new();
    let _ = render(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        &mut actions,
    );
    let _ = render(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        &mut actions,
    );

    assert!(actions.is_empty());
    assert!(
        context
            .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
            .is_none()
    );
}

fn frame(id: u64, top: f32) -> TrackFrame {
    TrackFrame {
        key: TrackFrameKey::Auxiliary(TrackId::new(id)),
        channel: TrackPane::Annotation,
        name: format!("track-{id}"),
        kind: TrackKind::Feature,
        source_label: Some(format!("track-{id}.bed")),
        top,
        bottom: top + 100.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    }
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1.0e-3,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn gutter_row_widths_keep_label_and_detail_apart() {
    let available_width = 143.0;
    let (label_width, detail_width) = gutter_row_widths(62.0, 240.0, available_width);

    assert_close(label_width, 62.0);
    assert_close(
        label_width + GUTTER_TEXT_GAP + detail_width,
        available_width,
    );
    assert!(detail_width > 0.0);
}

#[test]
fn gutter_text_is_elided_to_one_bounded_row() {
    let context = egui::Context::default();
    let _ = context.run_ui(egui::RawInput::default(), |ui| {
        let galley = layout_elided_text(
            ui.painter(),
            "2 types · 5 tagged reads · 0 malformed · 0 unavailable",
            FontId::proportional(9.0),
            Color32::from_gray(105),
            70.0,
        );

        assert!(galley.elided);
        assert_eq!(galley.rows.len(), 1);
        assert!(galley.size().x <= 70.0);
    });
}

#[test]
fn drop_target_accounts_for_vertical_scroll() {
    let frames = [frame(1, 200.0), frame(2, 300.0), frame(3, 400.0)];
    let order = [TrackId::new(1), TrackId::new(2), TrackId::new(3)];
    assert_eq!(track_drop_index(&frames, &order, 220.0, 100.0, 0.0), 1);
    assert_eq!(track_drop_index(&frames, &order, 500.0, 100.0, 0.0), 2);
}

#[test]
fn keyboard_reorder_stops_at_both_track_boundaries() {
    assert_eq!(keyboard_reorder_index(0, 4, -1), 0);
    assert_eq!(keyboard_reorder_index(1, 4, -1), 0);
    assert_eq!(keyboard_reorder_index(1, 4, 1), 2);
    assert_eq!(keyboard_reorder_index(3, 4, 1), 3);
}

#[test]
fn scrollbar_overview_preserves_empty_and_dense_content_regions() {
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(TrackId::new(1)),
        channel: TrackPane::Reads,
        name: "reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 500.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    }];
    let display = DisplayList {
        rectangles: (0..24)
            .map(|index| rgv_layout::RectPrimitive {
                x: 0.0,
                y: 300.0 + index as f32,
                width: 80.0,
                height: 3.0,
                color: rgv_layout::Rgba::new(0.2, 0.4, 0.8, 1.0),
                object_id: index,
                shape: Default::default(),
            })
            .collect(),
        texts: Vec::new(),
    };

    let overview = scrollbar_overview(&frames, &display, 100.0, 500.0, 100.0, 0.0, 100.0, 40);
    assert_eq!(overview.bins.len(), 40);
    assert!(overview.bins[..15].iter().all(|bin| bin.density == 0.0));
    assert!(overview.bins[20..25].iter().any(|bin| bin.density > 0.5));
    assert!(
        overview
            .bins
            .iter()
            .all(|bin| bin.kind == Some(TrackKind::Alignment))
    );
    assert_eq!(overview.boundaries, vec![0.0]);
}

#[test]
fn scrollbar_overview_places_activity_in_the_density_camera_world() {
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(TrackId::new(1)),
        channel: TrackPane::Reads,
        name: "reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 500.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(200.0),
        subtracks: Vec::new(),
    }];
    let display = DisplayList {
        rectangles: vec![rgv_layout::RectPrimitive {
            x: 0.0,
            y: 220.0,
            width: 80.0,
            height: 10.0,
            color: rgv_layout::Rgba::new(0.2, 0.4, 0.8, 1.0),
            object_id: 1,
            shape: Default::default(),
        }],
        texts: Vec::new(),
    };

    let overview = scrollbar_overview(&frames, &display, 100.0, 900.0, 200.0, 300.0, 100.0, 80);
    assert!(overview.bins[40..45].iter().any(|bin| bin.density > 0.0));
    assert!(overview.bins[..35].iter().all(|bin| bin.density == 0.0));
    assert_eq!(overview.bins[42].kind, Some(TrackKind::Alignment));
}

#[test]
fn custom_vertical_overview_exposes_a_named_accessibility_slider() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(100.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let mut offset = 250.0;
            let _ = paint_vertical_scrollbar(
                ui,
                Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                1_000.0,
                &mut offset,
                1.0,
                400.0,
                None,
                None,
            );
            let mut annotation_offset = 500.0;
            let _ = paint_annotation_scrollbar(
                ui,
                Rect::from_min_size(egui::pos2(40.0, 10.0), egui::vec2(22.0, 400.0)),
                1_000.0,
                &mut annotation_offset,
                None,
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Slider
            && node
                .label()
                .is_some_and(|label| label.starts_with("Read stack overview"))
            && node.numeric_value() == Some(25.0)
    }));
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Slider
            && node
                .label()
                .is_some_and(|label| label.starts_with("Annotation track scrollbar"))
            && node.numeric_value() == Some(50.0)
    }));
}

#[test]
fn split_workspaces_have_distinct_accessible_read_scrollbars() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(100.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let mut first_offset = 100.0;
            let _ = paint_read_panel_scrollbar(
                ui,
                Rect::from_min_max(pos2(10.0, 100.0), pos2(32.0, 300.0)),
                PanelId::new(1),
                TrackId::new(11),
                1_000.0,
                &mut first_offset,
                1.0,
                200.0,
                None,
                None,
                false,
            );
            let mut second_offset = 700.0;
            let _ = paint_read_panel_scrollbar(
                ui,
                Rect::from_min_max(pos2(68.0, 100.0), pos2(90.0, 300.0)),
                PanelId::new(2),
                TrackId::new(11),
                1_000.0,
                &mut second_offset,
                0.5,
                200.0,
                None,
                None,
                false,
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    let read_sliders = update
        .nodes
        .iter()
        .filter(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Read stack overview"))
        })
        .collect::<Vec<_>>();
    assert_eq!(read_sliders.len(), 2);
    assert!(
        read_sliders
            .iter()
            .any(|(_, node)| node.numeric_value() == Some(10.0))
    );
    assert!(
        read_sliders
            .iter()
            .any(|(_, node)| node.numeric_value() == Some(70.0))
    );
}

#[test]
fn vertical_overview_reports_press_before_egui_drag_threshold() {
    let context = egui::Context::default();
    let pointer = egui::pos2(20.0, 100.0);
    let mut interaction = ScrollbarInteraction::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 500.0));
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let mut offset = 250.0;
            let _ = paint_vertical_scrollbar(
                ui,
                Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                1_000.0,
                &mut offset,
                1.0,
                400.0,
                None,
                None,
            );
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let mut offset = 250.0;
            interaction = paint_vertical_scrollbar(
                ui,
                Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                1_000.0,
                &mut offset,
                1.0,
                400.0,
                None,
                None,
            );
        },
    );

    assert!(interaction.pointer_down);
    assert!(!interaction.drag_started);
    assert!(!interaction.dragging);
}

#[test]
fn custom_track_gutter_exposes_named_selection_and_resize_controls() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let frames = [frame(7, 20.0)];
            let _ = paint_track_viewport_chrome(
                ui,
                geometry,
                &frames,
                0.0,
                0.0,
                geometry.viewport.top(),
                geometry.viewport.bottom(),
                Some(TrackFrameKey::Auxiliary(TrackId::new(7))),
                &[TrackId::new(7)],
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Button
            && node
                .label()
                .is_some_and(|label| label.starts_with("track-7 Feature track"))
            && node.toggled() == Some(egui::accesskit::Toggled::True)
    }));
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Splitter
            && node
                .label()
                .is_some_and(|label| label.starts_with("Resize track-7 track"))
    }));
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::CheckBox
            && node
                .label()
                .is_some_and(|label| label == "track-7 track content visible")
            && node.toggled() == Some(egui::accesskit::Toggled::True)
    }));
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Button
            && node
                .label()
                .is_some_and(|label| label == "Open settings for track-7")
    }));
}

#[test]
fn duplicated_primary_alignment_frames_have_distinct_header_controls() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let track_id = TrackId::new(9);
            let frames = [
                TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    channel: TrackPane::Annotation,
                    name: "exact-bases.bam".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: None,
                    top: 20.0,
                    bottom: 120.0,
                    visible: true,
                    collapsed: false,
                    frozen_bottom: None,
                    subtracks: Vec::new(),
                },
                TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    channel: TrackPane::Reads,
                    name: "Reads · exact-bases.bam".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: None,
                    top: 180.0,
                    bottom: 420.0,
                    visible: true,
                    collapsed: false,
                    frozen_bottom: Some(202.0),
                    subtracks: Vec::new(),
                },
            ];
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let _ =
                paint_track_viewport_chrome(ui, geometry, &frames, 0.0, 0.0, 0.0, 180.0, None, &[]);
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    let settings_buttons = update
        .nodes
        .iter()
        .filter(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Open settings for "))
        })
        .count();
    let visibility_eyes = update
        .nodes
        .iter()
        .filter(|(_, node)| {
            node.role() == egui::accesskit::Role::CheckBox
                && node
                    .label()
                    .is_some_and(|label| label.contains("content visible"))
        })
        .count();

    assert_eq!(settings_buttons, 2);
    assert_eq!(visibility_eyes, 2);
}

#[test]
fn clicking_read_frame_selects_the_read_pane_not_its_annotation_frame() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Annotation,
            name: "exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 20.0,
            bottom: 120.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 180.0,
            bottom: 420.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(202.0),
            subtracks: Vec::new(),
        },
    ];
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            180.0,
            None,
            &[],
        )
    };
    let pointer = pos2(50.0, 250.0);

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(
        actions,
        [TrackUiAction::Select(
            TrackFrameKey::Alignment(track_id),
            TrackPane::Reads,
        )]
    );
}

#[test]
fn read_frame_selection_does_not_highlight_the_matching_annotation_frame() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let track_id = TrackId::new(9);
    let frames = [
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Annotation,
            name: "Annotation · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 20.0,
            bottom: 120.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 180.0,
            bottom: 420.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(202.0),
            subtracks: Vec::new(),
        },
    ];
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let _ = paint_track_viewport_chrome_with_groups(
                ui,
                geometry,
                &frames,
                0.0,
                0.0,
                0.0,
                180.0,
                Some(TrackFrameKey::Alignment(track_id)),
                &[],
                &[track_id],
                &[],
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    let annotation = &update
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Annotation · exact-bases.bam "))
                && node
                    .label()
                    .is_some_and(|label| label.contains("Alignment track;"))
        })
        .unwrap()
        .1;
    let reads = &update
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Reads · exact-bases.bam "))
                && node
                    .label()
                    .is_some_and(|label| label.contains("Alignment track;"))
        })
        .unwrap()
        .1;

    assert_eq!(annotation.toggled(), Some(egui::accesskit::Toggled::False));
    assert_eq!(reads.toggled(), Some(egui::accesskit::Toggled::True));
}

#[test]
fn annotation_track_body_context_menu_stays_open_and_offers_grouping() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Annotation,
            name: "exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 20.0,
            bottom: 120.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        },
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 180.0,
            bottom: 420.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(202.0),
            subtracks: Vec::new(),
        },
    ];
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            180.0,
            None,
            &[track_id, TrackId::new(10)],
        )
    };
    // This is deliberately below the 22-pixel header, in the annotation
    // track body where users naturally right-click coverage or summaries.
    let pointer = pos2(50.0, 80.0);

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let popup_id =
        egui::Id::new(("track-label", TrackFrameKey::Alignment(track_id), false)).with("popup");
    assert!(egui::Popup::is_id_open(&context, popup_id));

    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    assert!(egui::Popup::is_id_open(&context, popup_id));
    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Button
            && node
                .label()
                .is_some_and(|label| label == "Group 2 selected tracks")
    }));
}

#[test]
fn read_track_context_menu_offers_grouping_for_a_read_range_selection() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Reads,
        name: "Reads · exact-bases.bam".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 180.0,
        bottom: 420.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(202.0),
        subtracks: Vec::new(),
    }];
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome_with_groups(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            180.0,
            Some(TrackFrameKey::Alignment(track_id)),
            &[],
            &[track_id, TrackId::new(10)],
            &[],
        )
    };
    let pointer = pos2(50.0, 250.0);

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );

    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Button
            && node
                .label()
                .is_some_and(|label| label == "Group 2 selected tracks")
    }));
}

#[test]
fn clicking_visible_track_settings_button_opens_settings() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let frames = [frame(7, 20.0)];
    let pointer = pos2(157.0, 31.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![
            TrackUiAction::Select(
                TrackFrameKey::Auxiliary(TrackId::new(7)),
                TrackPane::Annotation,
            ),
            TrackUiAction::OpenSettings(TrackFrameKey::Auxiliary(TrackId::new(7))),
        ]
    );
}

#[test]
fn clicking_a_track_eye_requests_collapse() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let frames = [frame(7, 20.0)];
    let pointer = pos2(11.0, 31.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::SetNodeVisibility(
            PresentationNodeId::Track {
                channel: TrackChannel::Annotation,
                track_id: TrackId::new(7),
            },
            false,
        )]
    );
}

#[test]
fn group_header_eye_toggles_group_content_without_changing_expansion() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let frames = [TrackFrame {
        key: TrackFrameKey::Group {
            channel: TrackPane::Annotation,
            id: 4,
        },
        channel: TrackPane::Annotation,
        name: "Group 4 · 2 tracks".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 20.0,
        bottom: 46.0,
        visible: true,
        collapsed: true,
        frozen_bottom: None,
        subtracks: Vec::new(),
    }];
    let groups = [TrackGroupChrome {
        id: 4,
        pane: TrackPane::Annotation,
        anchor: TrackId::new(7),
        members: vec![TrackId::new(7), TrackId::new(8)],
        collapsed: false,
        content_visible: true,
    }];
    let pointer = pos2(11.0, 31.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome_with_groups(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
            &[],
            &groups,
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::ToggleTrackGroupVisibility(
            TrackPane::Annotation,
            4
        )]
    );
}

#[test]
fn group_chevrons_are_vector_geometry_and_labels_start_after_the_toggle() {
    let label_rect = Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(168.0, 22.0));
    let toggle_rect = Rect::from_center_size(pos2(40.0, 31.0), egui::vec2(16.0, 16.0));
    let collapsed = group_chevron_segments(toggle_rect, true);
    let expanded = group_chevron_segments(toggle_rect, false);

    assert!(collapsed[0][1].x > collapsed[0][0].x);
    assert_eq!(collapsed[0][1], collapsed[1][0]);
    assert!(expanded[0][1].y > expanded[0][0].y);
    assert_eq!(expanded[0][1], expanded[1][0]);
    assert!(
        collapsed
            .into_iter()
            .flatten()
            .all(|point| toggle_rect.contains(point))
    );
    assert!(
        expanded
            .into_iter()
            .flatten()
            .all(|point| toggle_rect.contains(point))
    );

    let (group_position, group_width) =
        track_header_label_geometry(label_rect, label_rect.top(), true);
    let (track_position, track_width) =
        track_header_label_geometry(label_rect, label_rect.top(), false);
    assert!(group_position.x >= toggle_rect.right() + 2.0);
    assert_eq!(track_position.x, label_rect.left() + 22.0);
    assert_eq!(group_width, 124.0);
    assert_eq!(track_width, 115.0);
}

#[test]
fn clicking_a_hidden_track_eye_requests_visibility_restore() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let mut hidden = frame(7, 20.0);
    hidden.bottom = hidden.top + 26.0;
    hidden.visible = false;
    let frames = [hidden];
    let pointer = pos2(11.0, 31.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::SetNodeVisibility(
            PresentationNodeId::Track {
                channel: TrackChannel::Annotation,
                track_id: TrackId::new(7),
            },
            true,
        )]
    );
}

#[test]
fn clicking_read_pane_eye_toggles_reads_instead_of_the_whole_track() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Reads,
        name: "Reads · exact-bases.bam".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 180.0,
        bottom: 420.0,
        visible: true,
        collapsed: true,
        frozen_bottom: Some(202.0),
        subtracks: Vec::new(),
    }];
    let pointer = pos2(11.0, 191.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            180.0,
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::SetNodeVisibility(
            PresentationNodeId::Track {
                channel: TrackChannel::Reads,
                track_id,
            },
            true,
        )]
    );
}

#[test]
fn hidden_alignment_subtrack_keeps_a_collapsed_closed_eye_row() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let track_id = TrackId::new(9);
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let frames = [TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: TrackPane::Annotation,
                name: "reads.bam".to_owned(),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 20.0,
                bottom: 300.0,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: vec![TrackSubtrack {
                    alignment_subtrack: Some(AlignmentSubtrack::Coverage),
                    label: "Coverage".to_owned(),
                    detail: None,
                    top: 42.0,
                    bottom: 62.0,
                    collapsed: true,
                    composite_member: None,
                }],
            }];
            let _ = paint_track_viewport_chrome(
                ui,
                geometry,
                &frames,
                0.0,
                0.0,
                0.0,
                geometry.viewport.bottom(),
                None,
                &[],
            );
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::CheckBox
            && node
                .label()
                .is_some_and(|label| label == "Coverage subtrack hidden")
            && node.toggled() == Some(egui::accesskit::Toggled::False)
    }));
}

#[test]
fn renamed_alignment_subtrack_uses_explicit_identity_for_gutter_action() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Annotation,
        name: "reads.bam".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 20.0,
        bottom: 300.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Coverage),
            label: "Renamed signal lane".to_owned(),
            detail: None,
            top: 42.0,
            bottom: 62.0,
            collapsed: true,
            composite_member: None,
        }],
    }];
    let pointer = pos2(11.0, 51.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| {
            actions = render(ui).actions;
        },
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::ToggleAlignmentSubtrack(
            track_id,
            AlignmentSubtrack::Coverage,
        )]
    );
}

#[test]
fn clicking_composite_member_eye_targets_only_that_member() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let member_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Auxiliary(TrackId::new(7)),
        channel: TrackPane::Annotation,
        name: "Overlay 1 · 3 tracks".to_owned(),
        kind: TrackKind::Quantitative,
        source_label: None,
        top: 20.0,
        bottom: 140.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: None,
            label: "normal".to_owned(),
            detail: Some("overlay 1".to_owned()),
            top: 42.0,
            bottom: 62.0,
            collapsed: false,
            composite_member: Some(member_id),
        }],
    }];
    let pointer = pos2(11.0, 51.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            ui.max_rect().bottom(),
            None,
            &[],
        )
    };

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::ToggleCompositeMember(member_id)]
    );
}

#[test]
fn resize_clamps_to_track_height_constraints() {
    assert_close(resized_track_height(120.0, 30.0), 150.0);
    assert_close(resized_track_height(120.0, -500.0), 40.0);
    assert_close(resized_track_height(120.0, 900.0), 600.0);
    assert_close(resized_read_track_height(120.0, 30.0), 150.0);
    assert_close(resized_read_track_height(120.0, -500.0), 40.0);
    assert_close(resized_read_track_height(120.0, 2_000.0), 1_200.0);
}

#[test]
fn read_group_boundary_is_resizable_from_the_left_gutter() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Reads,
        name: "Reads · sample A".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 200.0,
        bottom: 300.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(222.0),
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Reads),
            label: "Reads".to_owned(),
            detail: None,
            top: 222.0,
            bottom: 300.0,
            collapsed: false,
            composite_member: None,
        }],
    }];
    let pointer = pos2(50.0, 300.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            200.0,
            None,
            &[],
        )
    };
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(
        actions,
        vec![TrackUiAction::ResizeReadTrackStarted(track_id, 100.0)]
    );
}

#[test]
fn a_single_read_panel_has_no_lower_resize_boundary() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let track_id = TrackId::new(9);
    let frames = [TrackFrame {
        key: TrackFrameKey::Alignment(track_id),
        channel: TrackPane::Reads,
        name: "Reads · sample A".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 200.0,
        bottom: 500.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(222.0),
        subtracks: vec![TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Reads),
            label: "Reads".to_owned(),
            detail: None,
            top: 222.0,
            bottom: 500.0,
            collapsed: false,
            composite_member: None,
        }],
    }];
    let pointer = pos2(50.0, 499.0);
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome_with_sections(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &frames,
            0.0,
            0.0,
            0.0,
            200.0,
            None,
            &[],
            &[],
            &[],
            TrackSectionChrome {
                annotation_expanded: true,
                reads_expanded: true,
                annotation_track_count: 0,
                read_track_count: 1,
                annotation_content_top: 0.0,
                read_content_top: 200.0,
            },
        )
    };
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert!(!actions.iter().any(|action| matches!(
        action,
        TrackUiAction::ResizeReadTrackStarted(id, _) if *id == track_id
    )));
}

#[test]
fn incremental_track_resize_deltas_accumulate() {
    let after_first_frame = resized_track_height(120.0, 10.0);
    let after_second_frame = resized_track_height(after_first_frame, 10.0);
    assert_close(after_second_frame, 140.0);
}

#[test]
fn pane_resize_moves_the_divider_and_preserves_both_minimums() {
    assert_close(resized_read_pane_height(360.0, -30.0, 800.0), 390.0);
    assert_close(resized_read_pane_height(360.0, 30.0, 800.0), 330.0);
    assert_close(resized_read_pane_height(120.0, 200.0, 800.0), 100.0);
    assert_close(resized_read_pane_height(700.0, -200.0, 800.0), 720.0);
}

#[test]
fn pane_divider_reports_incremental_drag_actions() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &[],
            0.0,
            0.0,
            0.0,
            250.0,
            None,
            &[],
        )
    };
    // The pane divider must be draggable from the left gutter as well as
    // from the genomic canvas.
    let pointer = pos2(50.0, 250.0);
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );
    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneStarted]);
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerMoved(pos2(50.0, 260.0))],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );
    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerMoved(pos2(50.0, 270.0))],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );
    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);
}

#[test]
fn pane_divider_owns_drag_above_the_read_section_header() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome_with_sections(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &[],
            0.0,
            0.0,
            26.0,
            250.0,
            None,
            &[],
            &[],
            &[],
            TrackSectionChrome {
                annotation_expanded: true,
                reads_expanded: true,
                annotation_track_count: 1,
                read_track_count: 1,
                annotation_content_top: 26.0,
                read_content_top: 276.0,
            },
        )
    };
    // The lower half of the splitter overlaps the painted Read data
    // section header. That overlap must belong to the splitter on press.
    let pointer = pos2(400.0, 252.0);
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneStarted]);
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerMoved(pos2(400.0, 262.0))],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );
    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);
}

#[test]
fn pane_divider_remains_draggable_above_the_canvas_accessibility_surface() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let render = |ui: &mut Ui| {
        let geometry = CanvasGeometry::from_viewport(ui.max_rect());
        let result =
            paint_track_viewport_chrome(ui, geometry, &[], 0.0, 0.0, 0.0, 250.0, None, &[]);
        ui.interact(
            geometry.genomic,
            egui::Id::new("test-genomic-canvas-accessibility"),
            egui::Sense::focusable_noninteractive(),
        );
        result
    };
    let pointer = pos2(400.0, 250.0);
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneStarted]);
}

#[test]
fn alignment_read_content_supports_a_panel_local_scroll_offset() {
    let frame = TrackFrame {
        key: TrackFrameKey::Alignment(TrackId::new(1)),
        channel: TrackPane::Reads,
        name: "reads.bam".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 800.0,
        visible: true,
        collapsed: false,
        frozen_bottom: Some(260.0),
        subtracks: vec![
            TrackSubtrack {
                alignment_subtrack: Some(AlignmentSubtrack::Reference),
                label: "Reference".to_owned(),
                detail: None,
                top: 125.0,
                bottom: 165.0,
                collapsed: false,
                composite_member: None,
            },
            TrackSubtrack {
                alignment_subtrack: Some(AlignmentSubtrack::Reads),
                label: "Reads".to_owned(),
                detail: None,
                top: 260.0,
                bottom: 800.0,
                collapsed: false,
                composite_member: None,
            },
        ],
    };

    assert_eq!(frame_screen_bounds(&frame, 300.0), (-200.0, 500.0));
    assert_eq!(
        subtrack_screen_bounds(&frame, &frame.subtracks[0], 300.0),
        (-175.0, -135.0)
    );
    assert_eq!(
        subtrack_screen_bounds(&frame, &frame.subtracks[1], 300.0),
        (-40.0, 500.0)
    );
}

#[test]
fn scrollable_frame_bounds_are_clipped_below_the_pinned_gutter() {
    let crossing = frame(1, 200.0);
    let below = frame(2, 300.0);

    assert_eq!(
        clipped_frame_screen_bounds(&crossing, 0.0, 260.0, 100.0, 700.0),
        Some(VerticalScreenBounds {
            top: 260.0,
            bottom: 300.0,
        })
    );
    assert_eq!(
        clipped_frame_screen_bounds(&crossing, 40.0, 260.0, 100.0, 700.0),
        None
    );
    assert_eq!(
        clipped_frame_screen_bounds(&below, 20.5, 260.0, 100.0, 700.0),
        Some(VerticalScreenBounds {
            top: 279.5,
            bottom: 379.5,
        })
    );
}

#[test]
fn adjacent_scrolled_frames_never_overlap_after_boundary_clipping() {
    let frames = [frame(1, 200.0), frame(2, 300.0), frame(3, 400.0)];
    for scroll in [0.0, 25.5, 100.0, 240.0] {
        let visible = frames
            .iter()
            .filter_map(|frame| clipped_frame_screen_bounds(frame, scroll, 260.0, 100.0, 700.0))
            .collect::<Vec<_>>();
        assert!(visible.windows(2).all(|pair| pair[0].bottom <= pair[1].top));
    }
}

#[test]
fn screen_to_content_mapping_accepts_a_panel_local_scroll_offset() {
    let scrollable = frame(1, 300.0);
    assert_eq!(
        frame_content_y_at_screen_y(&scrollable, 220.0, 100.0, 260.0, 100.0, 700.0),
        None
    );
    assert_eq!(
        frame_content_y_at_screen_y(&scrollable, 260.0, 100.0, 260.0, 100.0, 700.0),
        Some(360.0)
    );

    let mut read_group = frame(2, 120.0);
    read_group.bottom = 720.0;
    read_group.frozen_bottom = Some(142.0);
    assert_eq!(
        frame_content_y_at_screen_y(&read_group, 260.0, 300.0, 260.0, 100.0, 700.0),
        Some(560.0)
    );

    let mut split = frame(3, 100.0);
    split.bottom = 800.0;
    split.frozen_bottom = Some(260.0);
    assert_eq!(
        frame_content_y_at_screen_y(&split, 150.0, 300.0, 260.0, 100.0, 700.0),
        None
    );
    assert_eq!(
        frame_content_y_at_screen_y(&split, 260.0, 300.0, 260.0, 100.0, 700.0),
        Some(560.0)
    );

    split.bottom = 800.0;
    split.subtracks = vec![TrackSubtrack {
        alignment_subtrack: Some(AlignmentSubtrack::Reads),
        label: "Reads".to_owned(),
        detail: None,
        top: 260.0,
        bottom: 900.0,
        collapsed: false,
        composite_member: None,
    }];
    assert_eq!(
        frame_content_y_at_screen_y(&split, 420.0, 300.0, 260.0, 100.0, 700.0),
        Some(720.0),
        "a read group maps through its panel-local scroll"
    );
    assert_eq!(
        frame_content_y_at_screen_y(&split, 520.0, 300.0, 260.0, 100.0, 700.0),
        None,
        "the read scrollbar cannot pick through the annotation pane"
    );
}

#[test]
fn deep_scroll_keeps_frame_and_scrollbar_offsets_in_lockstep() {
    let frame = frame(1, 2_400.0);
    let offset = 2_250.0;
    assert_close(frame.top - offset, 150.0);
    let rect = Rect::from_min_max(pos2(0.0, 100.0), pos2(22.0, 700.0));
    let top = scrollbar_geometry(rect, 3_000.0, 0.0);
    let middle = scrollbar_geometry(rect, 3_000.0, 1_500.0);
    let bottom = scrollbar_geometry(rect, 3_000.0, 3_000.0);
    assert_close(top.thumb.top(), top.track.top());
    assert_close(middle.thumb.center().y, middle.track.center().y);
    assert_close(bottom.thumb.bottom(), bottom.track.bottom());
    assert_close(top.track.top(), rect.top() + 5.0);
    assert_close(top.track.bottom(), rect.bottom() - 5.0);
    assert_close(
        scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.top()),
        0.0,
    );
    assert_close(
        scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.center().y),
        1_500.0,
    );
    assert_close(
        scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.bottom()),
        3_000.0,
    );
}

#[test]
fn exact_scrollbar_drag_preserves_the_pointer_position_inside_the_thumb() {
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
    let geometry = scrollbar_geometry(rect, 1_759.0, 500.0);
    let grab_offset = 9.0;
    let pointer_y = geometry.track.top()
        + (geometry.track.height() - geometry.thumb.height()) * 0.75
        + grab_offset;

    assert_close(
        scrollbar_offset_at_grab(geometry, 1_759.0, pointer_y, grab_offset),
        1_319.25,
    );
}

#[test]
fn fitting_content_always_uses_a_full_thumb() {
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
    let compact = scrollbar_geometry_for_density(rect, 0.0, 0.0, MIN_VERTICAL_DENSITY);
    let first_expansion = scrollbar_geometry_for_density(
        rect,
        0.0,
        0.0,
        MIN_VERTICAL_DENSITY * (2.0_f32 * 0.012).exp(),
    );

    assert_close(compact.thumb.height(), compact.track.height());
    assert_close(first_expansion.thumb.height(), compact.thumb.height());

    let anchored =
        scrollbar_geometry_for_density_at_fraction(rect, 0.0, MIN_VERTICAL_DENSITY * 2.0, 0.5);
    assert_close(anchored.thumb.height(), compact.track.height());
    assert_close(anchored.thumb.top(), compact.track.top());
}

#[test]
fn density_thumb_does_not_imply_overflow_during_a_fitting_density_gesture() {
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
    let compact = scrollbar_geometry_for_density(rect, 0.0, 0.0, MIN_VERTICAL_DENSITY);
    let grabbed_y = compact.track.center().y;
    let expanded =
        scrollbar_geometry_for_density_at_fraction(rect, 0.0, MIN_VERTICAL_DENSITY * 2.0, 0.5);

    assert_close(expanded.thumb.height(), expanded.track.height());
    assert_close(expanded.thumb.center().y, grabbed_y);
    assert_close(scrollbar_fraction_at_grab(expanded, grabbed_y, 0.5), 0.5);
}

#[test]
fn density_thumb_uses_the_scrollable_height_below_pinned_tracks() {
    let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
    let geometry =
        scrollbar_geometry_for_density_in_viewport(rect, 600.0, 0.0, MIN_VERTICAL_DENSITY, 300.0);
    let expected_fraction = 300.0 / (300.0 + 600.0);
    assert_close(
        geometry.thumb.height(),
        geometry.track.height() * expected_fraction,
    );
}

#[test]
fn read_scrollbar_rail_is_invariant_while_only_the_thumb_adapts() {
    let rect = Rect::from_min_max(pos2(0.0, 120.0), pos2(22.0, 620.0));
    let pending = scrollbar_geometry_for_density_in_viewport(
        rect,
        0.0,
        0.0,
        MIN_VERTICAL_DENSITY,
        rect.height(),
    );
    let exact = scrollbar_geometry_for_density_in_viewport(
        rect,
        1_500.0,
        400.0,
        MIN_VERTICAL_DENSITY,
        rect.height(),
    );
    let dense = scrollbar_geometry_for_density_in_viewport(
        rect,
        1_500.0,
        400.0,
        MAX_VERTICAL_DENSITY,
        rect.height(),
    );

    assert_eq!(pending.track, exact.track);
    assert_eq!(exact.track, dense.track);
    assert_eq!(pending.thumb, pending.track);
    assert!(exact.thumb.height() < pending.thumb.height());
    assert!(dense.thumb.height() <= exact.thumb.height());
}

#[test]
fn read_density_preview_spans_the_complete_fixed_rail() {
    let rect = Rect::from_min_max(pos2(0.0, 80.0), pos2(22.0, 580.0));
    let geometry = scrollbar_geometry_for_density_in_viewport(
        rect,
        2_000.0,
        750.0,
        MAX_VERTICAL_DENSITY,
        rect.height(),
    );
    let bin_count = 128;
    let first = scrollbar_overview_bin_rect(geometry.track, 0, bin_count);
    let last = scrollbar_overview_bin_rect(geometry.track, bin_count - 1, bin_count);

    assert_eq!(first.top(), geometry.track.top());
    assert_eq!(last.bottom(), geometry.track.bottom());
    assert_eq!(first.x_range(), geometry.track.x_range());
    assert_eq!(last.x_range(), geometry.track.x_range());
    assert!(geometry.thumb.height() < geometry.track.height());
}

#[test]
fn fixed_section_header_toggles_without_targeting_a_child_row() {
    let context = egui::Context::default();
    let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
    let render = |ui: &mut Ui| {
        paint_track_viewport_chrome_with_sections(
            ui,
            CanvasGeometry::from_viewport(ui.max_rect()),
            &[],
            0.0,
            0.0,
            26.0,
            180.0,
            None,
            &[],
            &[],
            &[],
            TrackSectionChrome {
                annotation_expanded: true,
                reads_expanded: true,
                annotation_track_count: 4,
                read_track_count: 2,
                annotation_content_top: 26.0,
                read_content_top: 206.0,
            },
        )
    };
    let pointer = pos2(300.0, 13.0);

    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = render(ui);
        },
    );
    let mut actions = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ui| actions = render(ui).actions,
    );

    assert_eq!(
        actions,
        [TrackUiAction::ToggleSection(TrackPane::Annotation)]
    );
}
