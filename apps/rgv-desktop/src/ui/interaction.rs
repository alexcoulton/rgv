use crate::diagnostics::{InteractionTraceStage, LatencyEvent};
use crate::display::hit_test::JunctionArcHitTestResult;
use crate::ui::gestures::{
    density_from_horizontal_drag, overview_drag_origin, overview_drag_step,
    vertical_camera_maximum, vertical_density_camera_padding, vertical_overview_step,
};
use crate::ui::{TrackFrameKey, UiCommand};
use crate::workspace::panels::{
    PanelWorkspace, ReadRowCoordinateAuthority, current_read_row_authority,
};
use crate::{
    COMMAND_SEARCH_ID, HorizontalOverviewDrag, HoverInspectorTarget, OverviewCapture,
    SashimiHitTestGate, UiPanel, VerticalOverviewDrag, Viewer, ui,
};
use anyhow::Result;
use rgv_alignment_scene::AlignmentHit;
use rgv_core::{GenomicInterval, InspectorActivation, PanelId, TrackId, Viewport};
use std::time::Instant;
use winit::dpi::PhysicalPosition;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorGrabMode, CursorIcon};

fn pointer_targets_unobstructed_canvas(
    canvas_contains_pointer: bool,
    canvas_layer: Option<egui::LayerId>,
    layer_under_pointer: Option<egui::LayerId>,
) -> bool {
    canvas_contains_pointer && canvas_layer.is_some() && canvas_layer == layer_under_pointer
}

/// Hover feedback is hit-tested only where a click would land on the canvas: not while egui
/// owns the pointer, not during a drag, and not underneath a floating window or popup.
const fn hover_hit_testing_allowed(
    egui_consumed: bool,
    dragging: bool,
    over_unobstructed_canvas: bool,
) -> bool {
    !egui_consumed && !dragging && over_unobstructed_canvas
}

/// The read stack scrolled by the keyboard. In the focused panel: the selected alignment track,
/// else the one under the pointer, else the primary alignment. With the pointer over another
/// panel the keys scroll that panel, and there the track under the pointer comes first.
fn keyboard_scroll_track(
    pointer_panel_is_focused: bool,
    selected: Option<TrackId>,
    under_pointer: Option<TrackId>,
    primary: TrackId,
) -> TrackId {
    if pointer_panel_is_focused {
        selected.or(under_pointer).unwrap_or(primary)
    } else {
        under_pointer.or(selected).unwrap_or(primary)
    }
}

/// Cmd/Ctrl+Z undoes; with Shift it redoes.
const fn undo_shortcut_command(shift: bool) -> UiCommand {
    if shift {
        UiCommand::Redo
    } else {
        UiCommand::Undo
    }
}

/// Moves the single read hover to `hover_panel`, clearing it everywhere else, and returns the
/// panels whose hover changed. The hover panel need not be the focused one.
pub(crate) fn retarget_alignment_hover(
    panels: &mut PanelWorkspace,
    hover_panel: PanelId,
    hit: Option<AlignmentHit>,
) -> Vec<PanelId> {
    let mut changed = Vec::new();
    for panel in panels.iter_mut() {
        let next = if panel.id == hover_panel { hit } else { None };
        if panel.hovered_alignment_hit != next {
            panel.hovered_alignment_hit = next;
            changed.push(panel.id);
        }
    }
    changed
}

impl Viewer {
    /// One-way compatibility projection for display/layout code that still
    /// consumes the compact canvas view. The immutable workspace snapshot is
    /// the only stored geometry authority.
    pub(crate) fn canvas_geometry(&self) -> ui::CanvasGeometry {
        self.workspace_layout.canvas_geometry()
    }

    pub(crate) fn cursor_moved(&mut self, position: PhysicalPosition<f64>, consumed: bool) {
        self.cursor = position;
        if self.dragging {
            if !consumed && let Some(previous) = self.last_cursor {
                let scale = self.window.scale_factor();
                let delta_points = (position.x - previous.x) / scale;
                if let Ok(viewport) = self.session.panels.active().viewport.pan_pixels(
                    -delta_points,
                    self.session.panels.active_mut().contig_length,
                ) {
                    self.set_panel_viewport(
                        self.session.panels.active().id,
                        viewport,
                        crate::action::ViewportCommit::Gesture,
                    );
                }
            }
            // A consumed movement must not pan, but it still establishes the next delta's
            // baseline. Otherwise the first unconsumed movement after egui capture is lost.
            self.last_cursor = Some(position);
        } else {
            self.last_cursor = None;
        }
        // The same unobstructed test clicks use: a window floating over the canvas must not let
        // hover highlight, or hover-select, the reads underneath it.
        let hover_allowed = hover_hit_testing_allowed(
            consumed,
            self.dragging,
            self.cursor_over_unobstructed_genomic_canvas(),
        );
        // Read hover follows the pointer into whichever panel it is over; focus is not needed
        // to see what a read is. Hover-to-inspect below stays with the focused panel, because
        // the inspector shows that panel's selection.
        let hover_panel = self
            .panel_at_cursor()
            .unwrap_or_else(|| self.session.panels.active().id);
        let hovered_scene = hover_allowed
            .then(|| self.alignment_scene_hit_at_cursor(hover_panel))
            .flatten();
        let hover_changed =
            retarget_alignment_hover(&mut self.session.panels, hover_panel, hovered_scene);
        if !hover_changed.is_empty() {
            for panel_id in hover_changed {
                self.synchronize_alignment_scene_interactions(panel_id);
            }
            self.prepare_alignment_scene_frame();
            self.window.request_redraw();
        }
        self.update_track_tile_hover(hover_allowed);
        let cursor_in_genomic_canvas = self.cursor_in_genomic_canvas();
        let should_hit_test_junction_arc = hover_allowed;
        let junction_hit_started = self.diagnostics_open.then(Instant::now);
        let junction_hit = if should_hit_test_junction_arc {
            self.junction_arc_hit_test_at_cursor()
        } else {
            JunctionArcHitTestResult {
                gate: if self.dragging {
                    SashimiHitTestGate::Dragging
                } else if !cursor_in_genomic_canvas {
                    SashimiHitTestGate::OutsideCanvas
                } else {
                    SashimiHitTestGate::PointerConsumed
                },
                ..JunctionArcHitTestResult::default()
            }
        };
        let junction_hit_duration = junction_hit_started.map(|started| started.elapsed());
        let hovered_junction_arc = junction_hit.hover;
        let previous_junction_arc = self.session.panels.active().hovered_junction_arc;
        let junction_arc_changed = hovered_junction_arc
            .map(|hover| (hover.key, hover.interaction_token, hover.content_revision))
            != previous_junction_arc
                .map(|hover| (hover.key, hover.interaction_token, hover.content_revision));
        let scale = self.window.scale_factor();
        self.sashimi_interaction_diagnostics.record_latest_probe(
            egui::pos2((position.x / scale) as f32, (position.y / scale) as f32),
            self.session.panels.active().navigation_presentation.phase,
            junction_hit,
        );
        if should_hit_test_junction_arc {
            self.sashimi_interaction_diagnostics.record_hit_test(
                junction_hit,
                previous_junction_arc,
                hovered_junction_arc,
                junction_hit_duration,
                junction_arc_changed,
            );
        } else if junction_arc_changed {
            self.sashimi_interaction_diagnostics.clears = self
                .sashimi_interaction_diagnostics
                .clears
                .saturating_add(1);
            self.sashimi_interaction_diagnostics.hover_redraws = self
                .sashimi_interaction_diagnostics
                .hover_redraws
                .saturating_add(1);
        }
        if junction_arc_changed {
            self.session.panels.active_mut().hovered_junction_arc = hovered_junction_arc;
            self.refresh_sashimi_hover_annotation();
            if junction_hit.native_authority {
                // Hover is frame state for persistent junction pages. Recapture uniforms from the
                // already-published scene; no source query or geometry upload is involved.
                self.prepare_alignment_scene_frame();
            }
            self.window.set_cursor(if hovered_junction_arc.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
            if self.session.synthetic
                || !self.refresh_panel_scroll_display(self.session.panels.active().id)
            {
                self.mark_display_dirty("wheel read-panel fast-scroll refresh was unavailable");
            }
            self.window.request_redraw();
        }
        if hover_allowed && self.inspector_activation_at_cursor() == InspectorActivation::Hover {
            let target = self.hover_inspector_target_at_cursor();
            if target != self.session.panels.active().hover_inspector_target {
                self.session.panels.active_mut().hover_inspector_target = target;
                if target.is_some_and(|target| {
                    !matches!(target, HoverInspectorTarget::Junction(_, _, _, _))
                }) {
                    self.select_at_cursor();
                }
                self.window.request_redraw();
            }
        } else if self
            .session
            .panels
            .active()
            .hover_inspector_target
            .is_some()
        {
            self.session.panels.active_mut().hover_inspector_target = None;
            self.window.request_redraw();
        }
    }

    pub(crate) fn hover_inspector_target_at_cursor(&self) -> Option<HoverInspectorTarget> {
        if let Some((track_id, position)) = self.modification_position_at_cursor() {
            return Some(HoverInspectorTarget::Modification(track_id, position));
        }
        if let Some((track_id, position)) = self.pileup_position_at_cursor() {
            return Some(HoverInspectorTarget::Pileup(track_id, position));
        }
        if let Some((track_id, start, end, component)) = self.junction_at_cursor() {
            return Some(HoverInspectorTarget::Junction(
                track_id, start, end, component,
            ));
        }
        self.alignment_scene_hit_at_cursor(self.session.panels.active().id)
            .map(HoverInspectorTarget::Read)
    }

    /// Zooms the active panel around the cursor. One step of a continuous gesture: the history
    /// entry is recorded once, when the gesture settles.
    pub(crate) fn zoom_at_cursor(&mut self, factor: f64) {
        let panel_rect = self.active_panel_rect();
        let anchor_points =
            self.cursor.x / self.window.scale_factor() - f64::from(panel_rect.left());
        if let Ok(viewport) = self.session.panels.active_mut().viewport.zoom_at(
            anchor_points,
            factor,
            self.session.panels.active_mut().contig_length,
        ) {
            self.set_panel_viewport(
                self.session.panels.active().id,
                viewport,
                crate::action::ViewportCommit::Gesture,
            );
        }
    }

    /// Scrolls the track stack under the cursor by logical points.
    pub(crate) fn scroll_tracks_points(&mut self, points: f32) {
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let annotation_clip_bottom = self
            .session
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .map_or(self.canvas_geometry().viewport.top(), |cache| {
                cache.annotation_clip_bottom
            });
        if cursor_y < annotation_clip_bottom {
            self.scroll_annotations_by(-points);
        } else if let Some(track_id) =
            self.read_panel_at_screen_y(self.session.panels.active().id, cursor_y)
        {
            self.scroll_read_panel_by(self.session.panels.active().id, track_id, -points);
        }
    }

    /// Up/Down/PageUp/PageDown scroll the read stack the user is working in, not always the
    /// primary alignment.
    fn keyboard_scroll_reads(&mut self, points: f32) {
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let active = self.session.panels.active().id;
        let panel_id = self.panel_at_cursor().unwrap_or(active);
        let track_id = keyboard_scroll_track(
            panel_id == active,
            self.session.panels.panel(panel_id).selected_alignment_track,
            self.read_panel_at_screen_y(panel_id, cursor_y),
            self.primary_alignment_track_id(),
        );
        self.scroll_read_panel_by(panel_id, track_id, points);
    }

    pub(crate) fn scroll_read_panel_by(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        points: f32,
    ) {
        let current = self
            .session
            .panels
            .panel(panel_id)
            .read_panel_scroll_state(track_id)
            .scroll_points;
        self.set_read_panel_scroll(panel_id, track_id, current + points);
    }

    fn set_read_panel_scroll(&mut self, panel_id: PanelId, track_id: TrackId, requested: f32) {
        let Some((_, clip_bottom, _)) = self.read_panel_metrics(panel_id, track_id) else {
            if self.diagnostics.recording() {
                self.diagnostics.event(
                    "vertical_navigation",
                    "scroll_rejected",
                    serde_json::json!({
                        "reason": "read panel metrics are unavailable",
                        "panel_id": panel_id.get(),
                        "track_id": track_id.get(),
                        "requested_points": requested,
                        "scroll_cache_authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                            panel_id,
                            track_id,
                        ),
                    }),
                );
            }
            return;
        };
        let row_authority = self.read_panel_row_authority(panel_id, track_id);
        if !row_authority.is_exact() {
            // Preserve user intent while exact row authority is transitioning. The fraction is
            // rebound to the successor's exact extent when it arrives; silently discarding this
            // movement is what made a captured thumb appear permanently locked.
            let state = self
                .session
                .panels
                .panel(panel_id)
                .read_panel_scroll_state(track_id);
            let maximum = self.read_panel_presented_scroll_maximum(panel_id, track_id, clip_bottom);
            let desired_fraction = if maximum > f32::EPSILON {
                Some((requested / maximum).clamp(0.0, 1.0))
            } else {
                state.latent_scroll_fraction
            };
            if let Some(fraction) = desired_fraction {
                self.session
                    .panels
                    .panel_mut(panel_id)
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .latent_scroll_fraction = Some(fraction);
                if track_id == self.primary_alignment_track_id() {
                    self.session
                        .panels
                        .panel_mut(panel_id)
                        .latent_vertical_scroll_fraction = Some(fraction);
                }
            }
            if self.diagnostics.recording() {
                self.diagnostics.event(
                    "vertical_navigation",
                    "scroll_rejected",
                    serde_json::json!({
                        "reason": "read row authority is not exact",
                        "panel_id": panel_id.get(),
                        "track_id": track_id.get(),
                        "requested_points": requested,
                        "row_authority": format!("{row_authority:?}"),
                        "scroll_cache_authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                            panel_id,
                            track_id,
                        ),
                    }),
                );
            }
            self.window.request_redraw();
            return;
        }
        let state = self
            .session
            .panels
            .panel(panel_id)
            .read_panel_scroll_state(track_id);
        let maximum = self.read_panel_presented_scroll_maximum(panel_id, track_id, clip_bottom);
        let next = requested.clamp(0.0, maximum);
        self.record_manual_vertical_scroll_intent(
            panel_id,
            track_id,
            requested,
            state.scroll_points,
            maximum,
            next,
            "scroll command",
        );
        if (next - state.scroll_points).abs() <= f32::EPSILON {
            return;
        }
        let primary_track_id = self.primary_alignment_track_id();
        let latent_fraction = {
            let state = self
                .session
                .panels
                .panel_mut(panel_id)
                .read_panel_scrolls
                .entry(track_id)
                .or_default();
            state.scroll_points = next;
            if maximum > f32::EPSILON {
                state.latent_scroll_fraction = Some((next / maximum).clamp(0.0, 1.0));
            }
            state.latent_scroll_fraction
        };
        if track_id == primary_track_id {
            self.session
                .panels
                .panel_mut(panel_id)
                .vertical_scroll_points = next;
            self.session
                .panels
                .panel_mut(panel_id)
                .latent_vertical_scroll_fraction = latent_fraction;
        }
        let materialization = self.read_panel_materialization_coverage_for(panel_id, track_id);
        if self.session.synthetic {
            self.mark_display_dirty("read-panel synthetic scroll requires display rebuild");
        } else if materialization.is_covered() {
            if !self.refresh_panel_scroll_display(panel_id) {
                self.mark_display_dirty("covered read-panel fast-scroll refresh was unavailable");
            }
        } else if self.diagnostics.recording() {
            self.diagnostics.event(
                "vertical_navigation",
                "scroll_waiting_for_materialization",
                serde_json::json!({
                    "panel_id": panel_id.get(),
                    "track_id": track_id.get(),
                    "coverage": format!("{materialization:?}"),
                    "preserved_committed_display": true,
                }),
            );
        }
        self.request_alignment_scene_camera_update(panel_id, track_id);
        self.window.request_redraw();
    }

    pub(crate) fn scroll_annotations_by(&mut self, points: f32) {
        let maximum =
            crate::workspace::panels::maximum_annotation_scroll_points(&self.session.panels);
        let next = (self.session.panels.annotation_scroll_points + points).clamp(0.0, maximum);
        if (next - self.session.panels.annotation_scroll_points).abs() <= f32::EPSILON {
            return;
        }
        self.invalidate_navigation_contract("annotation vertical scroll changed");
        if !self.refresh_shared_annotation_scroll_display(next) {
            self.mark_display_dirty("annotation fast-scroll refresh was unavailable");
        }
        self.window.request_redraw();
    }

    pub(crate) fn keyboard_input(&mut self, key: &Key) {
        let primary = self.modifiers.control_key() || self.modifiers.super_key();
        if self.modifiers.alt_key() {
            match key {
                Key::Named(NamedKey::ArrowLeft) => self.navigate_history_back(),
                Key::Named(NamedKey::ArrowRight) => self.navigate_history_forward(),
                _ => {}
            }
            self.window.request_redraw();
            return;
        }
        match key {
            Key::Character(character) => match (primary, character.to_ascii_lowercase().as_str()) {
                (true, "o") => self.pending_command = Some(UiCommand::OpenAlignment),
                (true, "s") => self.pending_command = Some(UiCommand::SaveSession),
                (true, "e") => self.pending_command = Some(UiCommand::ExportPng),
                (true, "k") => {
                    self.open_panel = Some(UiPanel::CommandPalette);
                    self.egui_context.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(COMMAND_SEARCH_ID));
                    });
                }
                (true, "l") => self.pending_command = Some(UiCommand::FocusLocus),
                (true, "z") => {
                    self.pending_command = Some(undo_shortcut_command(self.modifiers.shift_key()));
                }
                // IGV's feature-jump shortcuts.
                (true, "f") => self.request_adjacent_feature(true),
                (true, "b") => self.request_adjacent_feature(false),
                (true, "d") => {
                    self.diagnostics_open = ui::toggled_diagnostics(self.diagnostics_open);
                }
                (true, ",") => self.open_panel = Some(UiPanel::Settings),
                (false, "+" | "=") => self.keyboard_zoom(0.8),
                (false, "-" | "_") => self.keyboard_zoom(1.25),
                (false, "n") => self.select_visible_read(true),
                (false, "p") => self.select_visible_read(false),
                (false, "]") => self.select_loaded_track_evidence(true),
                (false, "[") => self.select_loaded_track_evidence(false),
                _ => {}
            },
            Key::Named(NamedKey::ArrowLeft) => self.keyboard_pan(-0.1),
            Key::Named(NamedKey::ArrowRight) => self.keyboard_pan(0.1),
            Key::Named(NamedKey::ArrowUp) => self.keyboard_scroll_reads(-80.0),
            Key::Named(NamedKey::ArrowDown) => self.keyboard_scroll_reads(80.0),
            Key::Named(NamedKey::PageUp) => self.keyboard_scroll_reads(-self.canvas_page_height()),
            Key::Named(NamedKey::PageDown) => self.keyboard_scroll_reads(self.canvas_page_height()),
            Key::Named(NamedKey::Home) => self.navigate_home(),
            Key::Named(NamedKey::Enter) => {
                if let Some(track) = self.session.panels.active_mut().selected_track {
                    self.open_panel = Some(UiPanel::TrackSettings(track));
                }
            }
            _ => {}
        }
        self.window.request_redraw();
    }

    /// One keyboard or toolbar zoom step about the centre of the focused panel. `factor` is
    /// the span multiplier (0.8 zooms in). A step the contig cannot accommodate is ignored.
    pub(crate) fn keyboard_zoom(&mut self, factor: f64) {
        let _ = self.apply_action(crate::action::translate::keyboard_zoom(
            self.session.panels.active().id,
            factor,
        ));
    }

    /// One keyboard pan step of the focused panel, as a fraction of its width.
    pub(crate) fn keyboard_pan(&mut self, fraction: f64) {
        let _ = self.apply_action(crate::action::translate::keyboard_pan(
            self.session.panels.active().id,
            fraction,
        ));
    }

    /// A discrete navigation: the caller has already recorded its history entry.
    pub(crate) fn viewport_changed(&mut self, panel_id: PanelId) {
        self.remember_current_locus(panel_id);
        self.discrete_viewport_changed(panel_id);
    }

    /// One repeatable navigation step (key repeat, toolbar zoom, span slider). The viewport
    /// updates immediately; history and recent loci get one entry when the burst settles.
    pub(crate) fn viewport_step_changed(&mut self, panel_id: PanelId) {
        if self.session.panels.is_active(panel_id) {
            self.sync_locus_editor();
        }
        self.note_navigation_gesture(panel_id);
        self.discrete_viewport_changed(panel_id);
    }

    fn discrete_viewport_changed(&mut self, panel_id: PanelId) {
        self.session
            .panels
            .panel_mut(panel_id)
            .hovered_alignment_hit = None;
        self.session
            .panels
            .panel_mut(panel_id)
            .hover_inspector_target = None;
        self.session.panels.panel_mut(panel_id).hovered_junction_arc = None;
        self.window.set_cursor(CursorIcon::Default);
        self.synchronize_alignment_scene_interactions(panel_id);
        self.record_manual_navigation_intent(panel_id, "discrete_viewport_change");
        self.begin_viewport_navigation(panel_id, false, false);
    }

    pub(crate) fn viewport_gesture_changed(&mut self, panel_id: PanelId) {
        if self.session.panels.is_active(panel_id) {
            self.sync_locus_editor();
        }
        self.note_navigation_gesture(panel_id);
        let panel = self.session.panels.panel_mut(panel_id);
        panel.hovered_alignment_hit = None;
        panel.hover_inspector_target = None;
        panel.hovered_junction_arc = None;
        self.window.set_cursor(CursorIcon::Default);
        self.synchronize_alignment_scene_interactions(panel_id);
        self.record_manual_navigation_intent(panel_id, "gesture_viewport_change");
        let hold_commit_until_end = self.dragging
            || self.horizontal_overview_drag.is_some()
            || self.overview_capture == Some(OverviewCapture::Horizontal);
        self.begin_viewport_navigation(panel_id, true, hold_commit_until_end);
    }

    /// Moves a panel to a viewport computed by the window (pointer gestures, overview, span
    /// slider) through the action path.
    pub(crate) fn set_panel_viewport(
        &mut self,
        panel_id: PanelId,
        viewport: Viewport,
        commit: crate::action::ViewportCommit,
    ) {
        if let Err(error) = self.apply_action(crate::action::Action::SetViewport {
            panel_id,
            viewport,
            commit,
        }) {
            self.report_error(error.to_string());
        }
    }

    /// Preserve a read selection across an alignment query refresh.
    ///
    /// Alignment object IDs are query-local record indexes. Keeping the index
    /// alone would either drop the highlight or select an unrelated record
    /// when a zoom changes the loaded query. The read name is stable across
    /// detail levels. Contig/start and template-end identity disambiguate
    /// paired or supplementary records which legitimately share that name.
    pub(crate) fn retain_active_read_selection_for_query_refresh(&mut self) {
        // Stable scene handles, not query-local record indexes, retain selection across refreshes.
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_overview_interaction(&mut self, interaction: ui::OverviewInteraction) {
        if interaction.pointer_down && self.overview_capture.is_none() {
            self.suppress_horizontal_overview_click = false;
        }
        if interaction.show_all {
            if let Ok(interval) = GenomicInterval::new(
                self.session
                    .panels
                    .active_mut()
                    .viewport
                    .interval()
                    .contig(),
                0,
                self.session.panels.active_mut().contig_length.max(1),
            ) && let Ok(viewport) = Viewport::new(
                interval,
                self.session.panels.active_mut().viewport.pixel_width(),
            ) {
                self.horizontal_overview_drag = None;
                self.set_panel_viewport(
                    self.session.panels.active().id,
                    viewport,
                    crate::action::ViewportCommit::Discrete,
                );
            }
            return;
        }
        if let Some(fraction) = interaction.keyboard_pan_fraction {
            self.keyboard_pan(fraction);
            return;
        }
        if let Some(factor) = interaction.keyboard_zoom_factor {
            self.keyboard_zoom(factor);
            return;
        }
        if let Some(position) = interaction.clicked_position {
            if std::mem::take(&mut self.suppress_horizontal_overview_click) {
                return;
            }
            if let Ok(viewport) = self
                .session
                .panels
                .active()
                .viewport
                .centered_on(position, self.session.panels.active_mut().contig_length)
            {
                self.set_panel_viewport(
                    self.session.panels.active().id,
                    viewport,
                    crate::action::ViewportCommit::Discrete,
                );
            }
            return;
        }
        if (interaction.pointer_down || interaction.drag_started)
            && self.horizontal_overview_drag.is_none()
            && let (Some(anchor_position), Some(press_x), Some(press_y)) = (
                interaction.press_position,
                interaction.press_x,
                interaction.press_y,
            )
        {
            let (viewport, anchor_fraction) = overview_drag_origin(
                self.session.panels.active().viewport,
                anchor_position,
                interaction.press_handle_fraction,
                self.session.panels.active().contig_length,
            );
            self.horizontal_overview_drag = Some(HorizontalOverviewDrag {
                anchor_fraction,
                last_pointer_x: press_x,
                last_pointer_y: press_y,
            });
            if viewport != self.session.panels.active_mut().viewport {
                self.set_panel_viewport(
                    self.session.panels.active().id,
                    viewport,
                    crate::action::ViewportCommit::Gesture,
                );
            }
        }
        let was_captured = self.overview_capture == Some(OverviewCapture::Horizontal);
        if interaction.dragging
            && !was_captured
            && let Some(mut drag) = self.horizontal_overview_drag
            && let (Some(pointer_x), Some(pointer_y)) =
                (interaction.pointer_x, interaction.pointer_y)
        {
            let delta_x = pointer_x - drag.last_pointer_x;
            let delta_y = pointer_y - drag.last_pointer_y;
            if let Some(viewport) = overview_drag_step(
                self.session.panels.active_mut().viewport,
                drag.anchor_fraction,
                delta_x,
                delta_y,
                self.session.panels.active_mut().contig_length,
            ) {
                self.set_panel_viewport(
                    self.session.panels.active().id,
                    viewport,
                    crate::action::ViewportCommit::Gesture,
                );
            }
            drag.last_pointer_x = pointer_x;
            drag.last_pointer_y = pointer_y;
            self.horizontal_overview_drag = Some(drag);
        }
        if (interaction.pointer_down || interaction.drag_started)
            && self.horizontal_overview_drag.is_some()
            && self.overview_capture.is_none()
        {
            self.begin_overview_capture(OverviewCapture::Horizontal);
            if interaction.drag_started {
                self.captured_motion_distance = self.captured_motion_distance.max(1.0);
            }
        }
        if interaction.drag_stopped && self.horizontal_overview_drag.take().is_some() {
            self.commit_navigation_gesture();
            self.finish_navigation_gesture(self.session.panels.active().id);
        }
    }

    pub(crate) fn apply_read_panel_overview_interaction(
        &mut self,
        track_id: TrackId,
        interaction: ui::ScrollbarInteraction,
        requested_scroll: &mut f32,
    ) {
        let row_authority =
            self.read_panel_row_authority(self.session.panels.active().id, track_id);
        if !row_authority.is_exact() {
            if self.diagnostics.recording()
                && (interaction.pointer_down
                    || interaction.keyboard_density_pixels.abs() > f32::EPSILON)
            {
                self.diagnostics.event(
                    "vertical_navigation",
                    "overview_interaction_rejected",
                    serde_json::json!({
                        "reason": "read row authority is not exact",
                        "panel_id": self.session.panels.active().id.get(),
                        "track_id": track_id.get(),
                        "requested_points": *requested_scroll,
                        "row_authority": format!("{row_authority:?}"),
                        "scroll_cache_authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                            self.session.panels.active().id,
                            track_id,
                        ),
                    }),
                );
            }
            return;
        }
        if interaction.pointer_down {
            self.diagnostics.latency(LatencyEvent::StageOnce(
                InteractionTraceStage::EguiPointerDown,
            ));
        }
        if interaction.keyboard_density_pixels.abs() > f32::EPSILON {
            let viewport_height = self
                .read_panel_metrics(self.session.panels.active().id, track_id)
                .map_or(
                    self.canvas_geometry().viewport.height(),
                    |(top, bottom, _)| (bottom - top).max(1.0),
                );
            let panel_id = self.session.panels.active().id;
            let previous_density = self.alignment_density_for_panel_track(panel_id, track_id);
            let anchor_y = viewport_height * 0.5;
            let (density, scroll) = vertical_overview_step(
                previous_density,
                *requested_scroll,
                anchor_y,
                interaction.keyboard_density_pixels,
                0.0,
            );
            if (density - previous_density).abs() > 1.0e-4 {
                self.set_read_panel_density(panel_id, track_id, density);
                self.mark_display_dirty("keyboard changed read-panel density");
            }
            let maximum = self
                .read_panel_metrics(self.session.panels.active().id, track_id)
                .map_or(0.0, |(_, bottom, _)| {
                    self.read_panel_presented_scroll_maximum(
                        self.session.panels.active().id,
                        track_id,
                        bottom,
                    )
                });
            *requested_scroll = scroll.clamp(0.0, maximum);
            self.mark_display_dirty("keyboard changed read-panel scroll camera");
        }
        self.set_read_panel_scroll(self.session.panels.active().id, track_id, *requested_scroll);
    }

    pub(crate) fn begin_vertical_scrollbar_drag(&mut self) -> bool {
        if self.vertical_overview_drag.is_some() || !self.cursor_in_vertical_scrollbar() {
            return false;
        }
        // A fitting track stack has no vertical pan range, but the same control must
        // remain draggable horizontally so the user can expand it into an overflow.
        let scale = self.window.scale_factor().max(f64::EPSILON);
        let pointer = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        let target_panel = self
            .session
            .panels
            .iter()
            .enumerate()
            .find_map(|(index, panel)| {
                (self.panel_scrollbar_rect_at(index).contains(pointer)
                    && panel.live_read_panel_at_y(pointer.y).is_some())
                .then_some(panel.id)
            });
        let Some(target_panel) = target_panel else {
            return false;
        };
        self.activate_view_panel(target_panel);
        let Some(track_id) =
            self.read_panel_at_screen_y(self.session.panels.active().id, pointer.y)
        else {
            return false;
        };
        if !self
            .read_panel_row_authority(self.session.panels.active().id, track_id)
            .is_exact()
        {
            return false;
        }
        let Some((clip_top, clip_bottom, _)) =
            self.read_panel_metrics(self.session.panels.active().id, track_id)
        else {
            return false;
        };
        let state = self
            .session
            .panels
            .active()
            .read_panel_scroll_state(track_id);
        let maximum = self.read_panel_presented_scroll_maximum(
            self.session.panels.active().id,
            track_id,
            clip_bottom,
        );
        let camera_viewport_height = (clip_bottom - clip_top).max(1.0);
        let scrollbar_rect = self
            .read_panel_scrollbar_rect(track_id)
            .unwrap_or(self.canvas_geometry().scrollbar);
        let density =
            self.alignment_density_for_panel_track(self.session.panels.active().id, track_id);
        let latent_fraction = state.latent_scroll_fraction;
        let scroll_fraction =
            (maximum > f32::EPSILON).then(|| (state.scroll_points / maximum).clamp(0.0, 1.0));
        let geometry = scroll_fraction.or(latent_fraction).map_or_else(
            || {
                ui::scrollbar_geometry_for_density_in_viewport(
                    scrollbar_rect,
                    maximum,
                    state.scroll_points,
                    density,
                    camera_viewport_height,
                )
            },
            |fraction| {
                ui::scrollbar_geometry_for_density_at_fraction_in_viewport(
                    scrollbar_rect,
                    maximum,
                    density,
                    fraction,
                    camera_viewport_height,
                )
            },
        );
        let thumb_hit = geometry.thumb.contains(pointer);
        let thumb_grab_fraction = if thumb_hit && geometry.thumb.height() > f32::EPSILON {
            ((pointer.y - geometry.thumb.top()) / geometry.thumb.height()).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let virtual_scroll_fraction = scroll_fraction
            .or(latent_fraction)
            .unwrap_or(thumb_grab_fraction);
        self.vertical_overview_drag = Some(VerticalOverviewDrag {
            panel_id: self.session.panels.active().id,
            track_id,
            grab_offset: if thumb_hit {
                pointer.y - geometry.thumb.top()
            } else {
                geometry.thumb.height() * 0.5
            },
            press_x: pointer.x,
            press_y: pointer.y,
            last_pointer_x: pointer.x,
            thumb_grab_fraction,
            virtual_pointer_y: pointer
                .y
                .clamp(geometry.track.top(), geometry.track.bottom()),
            virtual_scroll_fraction,
            density_dragging: false,
            pending_motion_x: 0.0,
            pending_motion_y: 0.0,
        });
        self.diagnostics.latency(LatencyEvent::Begin);
        if !thumb_hit {
            self.update_vertical_scrollbar_drag(self.cursor);
        }
        true
    }

    pub(crate) fn update_vertical_scrollbar_drag(
        &mut self,
        position: PhysicalPosition<f64>,
    ) -> bool {
        let Some(mut drag) = self.vertical_overview_drag else {
            return false;
        };
        let scale = self.window.scale_factor().max(f64::EPSILON);
        let pointer = egui::pos2((position.x / scale) as f32, (position.y / scale) as f32);
        let total_x = pointer.x - drag.press_x;
        let total_y = pointer.y - drag.press_y;
        if !drag.density_dragging && total_x.abs() >= 1.5 && total_x.abs() > total_y.abs() * 1.25 {
            drag.density_dragging = true;
            drag.last_pointer_x = pointer.x;
            self.vertical_overview_drag = Some(drag);
            self.diagnostics.latency(LatencyEvent::StageOnce(
                InteractionTraceStage::DensityIntentDetected,
            ));
            self.apply_vertical_density_delta(total_x, total_y);
            self.begin_overview_capture(OverviewCapture::VerticalDensity);
            drag = self
                .vertical_overview_drag
                .expect("vertical density drag was initialized above");
        }
        if self.overview_capture == Some(OverviewCapture::VerticalDensity) {
            self.window.request_redraw();
            return true;
        }
        if drag.density_dragging {
            let delta_x = pointer.x - drag.last_pointer_x;
            let delta_y = pointer.y - drag.virtual_pointer_y;
            drag.last_pointer_x = pointer.x;
            self.vertical_overview_drag = Some(drag);
            self.apply_vertical_density_delta(delta_x, delta_y);
            self.window.request_redraw();
            return true;
        }
        let Some((clip_top, clip_bottom, _)) =
            self.read_panel_metrics(self.session.panels.active().id, drag.track_id)
        else {
            return false;
        };
        if !self
            .read_panel_row_authority(self.session.panels.active().id, drag.track_id)
            .is_exact()
        {
            let state = self
                .session
                .panels
                .active()
                .read_panel_scroll_state(drag.track_id);
            let maximum = self.read_panel_presented_scroll_maximum(
                self.session.panels.active().id,
                drag.track_id,
                clip_bottom,
            );
            let camera_viewport_height = (clip_bottom - clip_top).max(1.0);
            let density = self.alignment_density_for_panel_track(drag.panel_id, drag.track_id);
            let scrollbar_rect = self
                .read_panel_scrollbar_rect(drag.track_id)
                .unwrap_or(self.canvas_geometry().scrollbar);
            let geometry = state.latent_scroll_fraction.map_or_else(
                || {
                    ui::scrollbar_geometry_for_density_in_viewport(
                        scrollbar_rect,
                        maximum,
                        state.scroll_points,
                        density,
                        camera_viewport_height,
                    )
                },
                |fraction| {
                    ui::scrollbar_geometry_for_density_at_fraction_in_viewport(
                        scrollbar_rect,
                        maximum,
                        density,
                        fraction,
                        camera_viewport_height,
                    )
                },
            );
            drag.virtual_pointer_y = pointer
                .y
                .clamp(geometry.track.top(), geometry.track.bottom());
            drag.virtual_scroll_fraction = ui::scrollbar_fraction_at_grab(
                geometry,
                drag.virtual_pointer_y,
                drag.thumb_grab_fraction,
            );
            drag.last_pointer_x = pointer.x;
            self.session
                .panels
                .active_mut()
                .read_panel_scrolls
                .entry(drag.track_id)
                .or_default()
                .latent_scroll_fraction = Some(drag.virtual_scroll_fraction);
            if drag.track_id == self.primary_alignment_track_id() {
                self.session
                    .panels
                    .active_mut()
                    .latent_vertical_scroll_fraction = Some(drag.virtual_scroll_fraction);
            }
            self.vertical_overview_drag = Some(drag);
            self.window.request_redraw();
            return true;
        }
        let state = self
            .session
            .panels
            .active()
            .read_panel_scroll_state(drag.track_id);
        let maximum = self.read_panel_presented_scroll_maximum(
            self.session.panels.active().id,
            drag.track_id,
            clip_bottom,
        );
        let camera_viewport_height = (clip_bottom - clip_top).max(1.0);
        let density = self.alignment_density_for_panel_track(drag.panel_id, drag.track_id);
        let scrollbar_rect = self
            .read_panel_scrollbar_rect(drag.track_id)
            .unwrap_or(self.canvas_geometry().scrollbar);
        let geometry = state.latent_scroll_fraction.map_or_else(
            || {
                ui::scrollbar_geometry_for_density_in_viewport(
                    scrollbar_rect,
                    maximum,
                    state.scroll_points,
                    density,
                    camera_viewport_height,
                )
            },
            |fraction| {
                ui::scrollbar_geometry_for_density_at_fraction_in_viewport(
                    scrollbar_rect,
                    maximum,
                    density,
                    fraction,
                    camera_viewport_height,
                )
            },
        );
        let next = ui::scrollbar_offset_at_grab(geometry, maximum, pointer.y, drag.grab_offset);
        self.set_read_panel_scroll(self.session.panels.active().id, drag.track_id, next);
        drag.last_pointer_x = pointer.x;
        self.vertical_overview_drag = Some(drag);
        self.window.request_redraw();
        true
    }

    pub(crate) fn finish_vertical_scrollbar_drag(&mut self) {
        if self.vertical_overview_drag.take().is_some() {
            if self.diagnostics.latency_active() {
                self.diagnostics.latency(LatencyEvent::Release);
            }
            self.window.request_redraw();
        }
    }

    pub(crate) fn begin_overview_capture(&mut self, capture: OverviewCapture) -> bool {
        if self.overview_capture.is_some() {
            return false;
        }
        let grabbed = self
            .window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| self.window.set_cursor_grab(CursorGrabMode::Confined));
        match grabbed {
            Ok(()) => {
                self.window.set_cursor_visible(false);
                self.overview_capture = Some(capture);
                self.captured_motion = (0.0, 0.0);
                self.captured_motion_distance = 0.0;
                if capture == OverviewCapture::VerticalDensity {
                    self.diagnostics.latency(LatencyEvent::StageOnce(
                        InteractionTraceStage::PointerCaptureAcquired,
                    ));
                }
                true
            }
            Err(error) => {
                tracing::warn!(%error, "pointer capture is unavailable; using bounded overview drag");
                false
            }
        }
    }

    pub(crate) fn capture_mouse_motion(&mut self, delta: (f64, f64)) {
        if self.overview_capture.is_none()
            && let Some(mut drag) = self.vertical_overview_drag
        {
            let scale = self.window.scale_factor().max(f64::EPSILON);
            if drag.density_dragging {
                drag.last_pointer_x += (delta.0 / scale) as f32;
                self.vertical_overview_drag = Some(drag);
                self.apply_vertical_density_delta(
                    (delta.0 / scale) as f32,
                    (delta.1 / scale) as f32,
                );
                self.window.request_redraw();
                return;
            }
            drag.pending_motion_x += delta.0;
            drag.pending_motion_y += delta.1;
            let logical_x = drag.pending_motion_x / scale;
            let logical_y = drag.pending_motion_y / scale;
            if logical_x.abs() >= 0.5 && logical_x.abs() > logical_y.abs() * 1.25 {
                drag.density_dragging = true;
                self.vertical_overview_drag = Some(drag);
                self.diagnostics.latency(LatencyEvent::StageOnce(
                    InteractionTraceStage::DensityIntentDetected,
                ));
                if self.begin_overview_capture(OverviewCapture::VerticalDensity) {
                    self.captured_motion = (drag.pending_motion_x, drag.pending_motion_y);
                    self.captured_motion_distance =
                        drag.pending_motion_x.hypot(drag.pending_motion_y);
                } else {
                    drag.last_pointer_x += logical_x as f32;
                    self.vertical_overview_drag = Some(drag);
                    self.apply_vertical_density_delta(logical_x as f32, logical_y as f32);
                }
                self.window.request_redraw();
            } else {
                self.vertical_overview_drag = Some(drag);
            }
            return;
        }
        if self.overview_capture.is_none() {
            return;
        }
        self.captured_motion.0 += delta.0;
        self.captured_motion.1 += delta.1;
        self.captured_motion_distance += delta.0.hypot(delta.1);
        self.window.request_redraw();
    }

    pub(crate) fn apply_captured_motion(&mut self) {
        let Some(capture) = self.overview_capture else {
            return;
        };
        let (physical_x, physical_y) = std::mem::take(&mut self.captured_motion);
        if physical_x.abs() <= f64::EPSILON && physical_y.abs() <= f64::EPSILON {
            return;
        }
        let scale = self.window.scale_factor().max(f64::EPSILON);
        let delta_x = physical_x / scale;
        let delta_y = physical_y / scale;
        match capture {
            OverviewCapture::Horizontal => {
                let Some(drag) = self.horizontal_overview_drag else {
                    return;
                };
                if let Some(viewport) = overview_drag_step(
                    self.session.panels.active_mut().viewport,
                    drag.anchor_fraction,
                    delta_x,
                    delta_y,
                    self.session.panels.active_mut().contig_length,
                ) {
                    self.set_panel_viewport(
                        self.session.panels.active().id,
                        viewport,
                        crate::action::ViewportCommit::Gesture,
                    );
                }
            }
            OverviewCapture::VerticalDensity => {
                self.apply_vertical_density_delta(delta_x as f32, delta_y as f32);
            }
        }
    }

    fn apply_vertical_density_delta(&mut self, delta_x: f32, delta_y: f32) {
        let Some(mut drag) = self.vertical_overview_drag else {
            return;
        };
        if drag.panel_id != self.session.panels.active().id {
            return;
        }
        if !self
            .read_panel_row_authority(self.session.panels.active().id, drag.track_id)
            .is_exact()
        {
            return;
        }
        let previous_density = self.alignment_density_for_panel_track(drag.panel_id, drag.track_id);
        let density = density_from_horizontal_drag(previous_density, delta_x);
        let Some((read_viewport_top, read_viewport_bottom, actual_content_bottom)) =
            self.read_panel_metrics(self.session.panels.active().id, drag.track_id)
        else {
            return;
        };
        let viewport_height = (read_viewport_bottom - read_viewport_top).max(1.0);
        let maximum = vertical_camera_maximum(
            actual_content_bottom,
            read_viewport_bottom,
            viewport_height,
            density,
        );
        let scrollbar_rect = self
            .read_panel_scrollbar_rect(drag.track_id)
            .unwrap_or(self.canvas_geometry().scrollbar);
        let geometry = ui::scrollbar_geometry_for_density_at_fraction_in_viewport(
            scrollbar_rect,
            maximum,
            density,
            drag.virtual_scroll_fraction,
            viewport_height,
        );
        let grab_fraction = drag.thumb_grab_fraction.clamp(0.0, 1.0);
        let zoom_fraction =
            ui::scrollbar_fraction_at_grab(geometry, drag.virtual_pointer_y, grab_fraction);
        drag.virtual_pointer_y += delta_y;
        let minimum_pointer = geometry.track.top() + grab_fraction * geometry.thumb.height();
        let maximum_pointer =
            geometry.track.bottom() - (1.0 - grab_fraction) * geometry.thumb.height();
        drag.virtual_pointer_y = drag
            .virtual_pointer_y
            .clamp(minimum_pointer, maximum_pointer.max(minimum_pointer));
        let previous_fraction = drag.virtual_scroll_fraction;
        drag.virtual_scroll_fraction =
            ui::scrollbar_fraction_at_grab(geometry, drag.virtual_pointer_y, grab_fraction);
        let next_scroll = drag.virtual_scroll_fraction * maximum;
        let previous_state = self
            .session
            .panels
            .active()
            .read_panel_scroll_state(drag.track_id);
        let previous_effective_scroll = previous_state.effective_scroll_points();
        self.record_manual_vertical_scroll_intent(
            self.session.panels.active().id,
            drag.track_id,
            next_scroll,
            previous_state.scroll_points,
            maximum,
            next_scroll.clamp(0.0, maximum),
            "density scrollbar drag",
        );
        let camera_padding = vertical_density_camera_padding(viewport_height, density);
        let zoom_scroll = zoom_fraction * maximum;
        let content_origin = (zoom_scroll - previous_effective_scroll).clamp(0.0, camera_padding);
        let density_changed = (density - previous_density).abs() > 1.0e-4;
        let scroll_changed = (next_scroll - previous_state.scroll_points).abs() > f32::EPSILON;
        self.vertical_overview_drag = Some(drag);
        let state = self
            .session
            .panels
            .active_mut()
            .read_panel_scrolls
            .entry(drag.track_id)
            .or_default();
        state.scroll_points = next_scroll;
        state.content_origin_points = content_origin;
        state.latent_scroll_fraction = Some(drag.virtual_scroll_fraction);
        state.content_bottom_points = read_viewport_bottom + maximum;
        if drag.track_id == self.primary_alignment_track_id() {
            self.session.panels.active_mut().vertical_scroll_points = next_scroll;
            self.session
                .panels
                .active_mut()
                .vertical_content_origin_points = content_origin;
            self.session
                .panels
                .active_mut()
                .latent_vertical_scroll_fraction = Some(drag.virtual_scroll_fraction);
            self.session.panels.active_mut().content_bottom_points = read_viewport_bottom + maximum;
        }
        if density_changed {
            self.set_read_panel_density(drag.panel_id, drag.track_id, density);
            self.mark_display_dirty("vertical overview changed read-panel density");
            self.diagnostics.latency(LatencyEvent::StageOnce(
                InteractionTraceStage::DensityChanged,
            ));
        } else if scroll_changed {
            let materialization = self.read_panel_materialization_coverage_for(
                self.session.panels.active().id,
                drag.track_id,
            );
            if self.session.synthetic {
                self.mark_display_dirty("synthetic vertical overview requires display rebuild");
            } else if materialization.is_covered()
                && !self.refresh_panel_scroll_display(self.session.panels.active().id)
            {
                self.mark_display_dirty(
                    "covered vertical overview fast-scroll refresh was unavailable",
                );
            } else if !materialization.is_covered() && self.diagnostics.recording() {
                self.diagnostics.event(
                    "vertical_navigation",
                    "scroll_waiting_for_materialization",
                    serde_json::json!({
                        "panel_id": drag.panel_id.get(),
                        "track_id": drag.track_id.get(),
                        "coverage": format!("{materialization:?}"),
                        "preserved_committed_display": true,
                    }),
                );
            }
        } else if (drag.virtual_scroll_fraction - previous_fraction).abs() > f32::EPSILON {
            self.window.request_redraw();
        }
        if density_changed || scroll_changed {
            self.request_alignment_scene_camera_update(
                self.session.panels.active().id,
                drag.track_id,
            );
        }
    }

    pub(crate) fn finish_overview_capture(&mut self) {
        let Some(capture) = self.overview_capture else {
            return;
        };
        self.apply_captured_motion();
        let moved = self.captured_motion_distance >= 0.5;
        self.overview_capture = None;
        self.captured_motion = (0.0, 0.0);
        self.captured_motion_distance = 0.0;
        if let Err(error) = self.window.set_cursor_grab(CursorGrabMode::None) {
            tracing::warn!(%error, "could not release captured pointer");
        }
        self.window.set_cursor_visible(true);
        match capture {
            OverviewCapture::Horizontal => {
                self.suppress_horizontal_overview_click = moved;
                if self.horizontal_overview_drag.take().is_some() {
                    self.commit_navigation_gesture();
                    self.finish_navigation_gesture(self.session.panels.active().id);
                }
            }
            OverviewCapture::VerticalDensity => {
                self.mark_display_dirty("vertical density capture finished");
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn query_options_changed(&mut self) {
        self.remember_current_locus(self.session.panels.active().id);
        self.mark_display_dirty("explicit display invalidation");
        self.invalidate_navigation_contract("query or presentation options changed");
        if !self.session.synthetic
            && (self.has_alignment_engine() || self.session.track_engine.is_some())
        {
            for panel in &mut self.session.panels.panels {
                panel.generation_clock.advance();
            }
            for track in &mut self.session.alignment_tracks {
                for projection in track.projections.values_mut() {
                    projection.advance_scene_generation();
                }
            }
            self.begin_all_panel_refresh_transactions();
            self.request_all_panel_queries();
        }
    }

    pub(crate) fn canvas_page_height(&self) -> f32 {
        self.canvas_geometry().viewport.height().max(80.0) * 0.8
    }

    pub(crate) fn cursor_in_genomic_canvas(&self) -> bool {
        let scale = self.window.scale_factor();
        let point = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        self.workspace_layout
            .panels
            .iter()
            .any(|panel| panel.canvas.contains(point))
    }

    pub(crate) fn cursor_over_unobstructed_genomic_canvas(&self) -> bool {
        if !self.cursor_in_genomic_canvas() {
            return false;
        }
        let scale = self.window.scale_factor();
        let pointer = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        pointer_targets_unobstructed_canvas(
            true,
            self.genomic_canvas_layer_id,
            self.egui_context.layer_id_at(pointer),
        )
    }

    pub(crate) fn panel_column_rect_at(&self, index: usize) -> egui::Rect {
        self.workspace_layout
            .panels
            .get(index)
            .expect("committed workspace layout contains every locus panel")
            .canvas
    }

    pub(crate) fn panel_scrollbar_rect_at(&self, index: usize) -> egui::Rect {
        self.workspace_layout
            .panels
            .get(index)
            .expect("committed workspace layout contains every locus panel")
            .scrollbar
    }

    pub(crate) fn panel_column_rect(&self, id: PanelId) -> Option<egui::Rect> {
        self.session
            .panels
            .iter()
            .position(|panel| panel.id == id)
            .map(|index| self.panel_column_rect_at(index))
    }

    /// The committed column of `panel_id`; a panel that has left the layout reports the focused
    /// panel's column, as [`PanelWorkspace::panel`] does.
    pub(crate) fn panel_rect(&self, panel_id: PanelId) -> egui::Rect {
        self.panel_column_rect(panel_id)
            .unwrap_or_else(|| self.active_panel_rect())
    }

    pub(crate) fn active_panel_rect(&self) -> egui::Rect {
        self.panel_column_rect(self.session.panels.active().id)
            .expect("committed workspace layout contains the active locus panel")
    }

    pub(crate) fn panel_at_cursor(&self) -> Option<PanelId> {
        let scale = self.window.scale_factor();
        let point = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        self.workspace_layout
            .panel_at(point)
            .map(|panel| panel.panel_id)
    }

    pub(crate) fn synchronize_panel_widths(&mut self) {
        let current_ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let layout_ids = self
            .workspace_layout
            .panels
            .iter()
            .map(|panel| panel.panel_id)
            .collect::<Vec<_>>();
        if current_ids != layout_ids {
            let panel_inputs = self
                .session
                .panels
                .iter()
                .map(|panel| {
                    let cytobands = if self
                        .session
                        .assembly
                        .cytobands_for(panel.viewport.interval().contig())
                        .next()
                        .is_some()
                    {
                        ui::CytobandAvailability::Present
                    } else {
                        ui::CytobandAvailability::Absent
                    };
                    ui::WorkspacePanelLayoutInput {
                        panel_id: panel.id,
                        contig_name: panel.contig_name.clone(),
                        contig_length: panel.contig_length,
                        cytobands,
                    }
                })
                .collect::<Vec<_>>();
            self.workspace_layout = ui::WorkspaceLayoutSnapshot::compute(
                ui::WorkspaceLayoutInput {
                    workspace: self.workspace_layout.workspace,
                    properties: self.workspace_layout.properties,
                    properties_expanded: self.workspace_layout.properties_expanded,
                    panels: panel_inputs,
                    metrics: self.workspace_layout.metrics,
                    requested_track_gutter_width: self
                        .workspace_layout
                        .requested_track_gutter_width,
                    minimum_track_gutter_width: self.workspace_layout.minimum_track_gutter_width,
                    requested_scrollbar_width: self.workspace_layout.requested_scrollbar_width,
                    minimum_scrollbar_width: self.workspace_layout.minimum_scrollbar_width,
                    display_scale: self.workspace_layout.display_scale,
                    accessibility_scale: self.workspace_layout.accessibility_scale,
                },
                self.workspace_layout.revision.saturating_add(1),
            );
            self.workspace_layout_recomputations =
                self.workspace_layout_recomputations.saturating_add(1);
        }
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in &ids {
            let width = self
                .workspace_layout
                .panel(*id)
                .expect("committed workspace layout contains every locus panel")
                .canvas
                .width()
                .max(1.0);
            if let Some(panel) = self.session.panels.get_mut(*id)
                && let Ok(viewport) = panel.viewport.resized(f64::from(width))
            {
                panel.viewport = viewport;
            }
        }
        self.mark_display_dirty("explicit display invalidation");
    }

    /// Gives a panel keyboard and locus-box focus. False when the panel does not exist.
    pub(crate) fn activate_view_panel(&mut self, id: PanelId) -> bool {
        self.apply_action(crate::action::Action::ActivatePanel { panel_id: id })
            .is_ok_and(|outcome| outcome.panel_id.is_some())
    }

    pub(crate) fn refresh_active_panel_context(&mut self) {
        let interval = self.session.panels.active().viewport.interval();
        self.locus_editor = format!(
            "{}:{}-{}",
            self.session.panels.active().contig_name,
            interval.start().saturating_add(1),
            interval.end()
        );
    }

    pub(crate) fn add_view_panel(&mut self, locus: &str) -> Result<PanelId> {
        let outcome = self.apply_action(crate::action::Action::AddPanel {
            locus: locus.to_owned(),
        })?;
        outcome
            .panel_id
            .ok_or_else(|| anyhow::anyhow!("the panel was not created"))
    }

    pub(crate) fn cursor_in_track_viewport(&self) -> bool {
        let scale = self.window.scale_factor();
        self.canvas_geometry().viewport.contains(egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        ))
    }

    pub(crate) fn cursor_over_unobstructed_track_viewport(&self) -> bool {
        if !self.cursor_in_track_viewport() {
            return false;
        }
        let scale = self.window.scale_factor();
        let pointer = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        pointer_targets_unobstructed_canvas(
            true,
            self.genomic_canvas_layer_id,
            self.egui_context.layer_id_at(pointer),
        )
    }

    pub(crate) fn cursor_in_vertical_scrollbar(&self) -> bool {
        let scale = self.window.scale_factor();
        let point = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        self.session
            .panels
            .iter()
            .enumerate()
            .any(|(index, panel)| {
                self.panel_scrollbar_rect_at(index).contains(point)
                    && panel.live_read_panel_at_y(point.y).is_some()
            })
    }

    fn read_panel_at_screen_y(&self, panel_id: PanelId, screen_y: f32) -> Option<TrackId> {
        self.session
            .panels
            .panel(panel_id)
            .live_read_panel_at_y(screen_y)
    }

    pub(crate) fn read_panel_metrics(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<(f32, f32, f32)> {
        let panel = self.session.panels.panel(panel_id);
        let allocation = panel.live_read_panel_rect(track_id)?;
        let actual_content_bottom = panel
            .scroll_display_cache
            .as_ref()?
            .read_panels
            .iter()
            .find(|panel| panel.track_id == track_id)
            .map(|panel| panel.actual_content_bottom)?;
        Some((allocation.top(), allocation.bottom(), actual_content_bottom))
    }

    pub(crate) fn read_panel_row_authority(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> ReadRowCoordinateAuthority {
        let current_target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        let row_target_compatible = self
            .alignment_scene_publication_authority
            .committed_row_target_matches_desired(panel_id, track_id);
        let Some(panel) = self
            .session
            .panels
            .panel(panel_id)
            .scroll_display_cache
            .as_ref()
            .and_then(|cache| {
                cache
                    .read_panels
                    .iter()
                    .find(|panel| panel.track_id == track_id)
            })
        else {
            return ReadRowCoordinateAuthority::Absent;
        };
        current_read_row_authority(
            panel.presentation_target_revision,
            current_target_revision,
            row_target_compatible,
            panel.row_coordinate_authority,
        )
    }

    pub(crate) fn read_panel_presented_scroll_maximum(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        clip_bottom: f32,
    ) -> f32 {
        crate::display::read_panel_scrollbar_maximum(
            self.session
                .panels
                .panel(panel_id)
                .read_panel_scroll_state(track_id),
            clip_bottom,
            self.read_panel_row_authority(panel_id, track_id),
        )
    }

    fn read_panel_scrollbar_rect(&self, track_id: TrackId) -> Option<egui::Rect> {
        let (top, bottom, _) =
            self.read_panel_metrics(self.session.panels.active().id, track_id)?;
        let active_index = self.session.panels.active;
        let full = self.panel_scrollbar_rect_at(active_index);
        Some(egui::Rect::from_min_max(
            egui::pos2(full.left(), top.clamp(full.top(), full.bottom())),
            egui::pos2(full.right(), bottom.clamp(full.top(), full.bottom())),
        ))
    }

    pub(crate) fn inspector_activation_at_cursor(&self) -> InspectorActivation {
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let panel = self.session.panels.active();
        let annotation_clip_top = panel
            .scroll_display_cache
            .as_ref()
            .map_or(self.canvas_geometry().viewport.top(), |cache| {
                cache.annotation_clip_top
            });
        let annotation_clip_bottom = panel
            .scroll_display_cache
            .as_ref()
            .map_or(self.canvas_geometry().viewport.top(), |cache| {
                cache.annotation_clip_bottom
            });
        let alignment_track = panel
            .pane_layout
            .annotation
            .items
            .iter()
            .chain(panel.pane_layout.reads.items.iter())
            .find_map(|item| {
                let frame = &item.frame;
                let TrackFrameKey::Alignment(id) = frame.key else {
                    return None;
                };
                if cursor_y < item.row_rect.top() || cursor_y >= item.row_rect.bottom() {
                    return None;
                }
                let read_panel = frame.channel == ui::TrackPane::Reads;
                ui::frame_content_y_at_screen_y(
                    frame,
                    cursor_y,
                    if read_panel {
                        0.0
                    } else {
                        self.session.panels.annotation_scroll_points
                    },
                    if read_panel {
                        frame.top
                    } else {
                        annotation_clip_top
                    },
                    self.canvas_geometry().viewport.top(),
                    if read_panel {
                        frame.bottom
                    } else {
                        annotation_clip_bottom
                    },
                )
                .map(|_| id)
            });
        alignment_track.map_or(
            self.primary_alignment_filters().inspector_activation,
            |id| {
                if id == self.primary_alignment_track_id() {
                    self.primary_alignment_filters().inspector_activation
                } else {
                    self.session
                        .alignment_tracks
                        .iter()
                        .find(|track| track.config.id == id)
                        .map_or(
                            self.primary_alignment_filters().inspector_activation,
                            |track| track.config.filters.inspector_activation,
                        )
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::panels::ViewPanelController;

    #[test]
    fn keyboard_scrolling_targets_the_track_in_use_before_the_primary_alignment() {
        let (selected, hovered, primary) = (TrackId::new(7), TrackId::new(5), TrackId::new(1));
        assert_eq!(
            keyboard_scroll_track(true, Some(selected), Some(hovered), primary),
            selected
        );
        assert_eq!(
            keyboard_scroll_track(true, None, Some(hovered), primary),
            hovered
        );
        assert_eq!(keyboard_scroll_track(true, None, None, primary), primary);
    }

    #[test]
    fn keyboard_scrolling_over_another_panel_targets_the_track_under_the_pointer() {
        let (selected, hovered, primary) = (TrackId::new(7), TrackId::new(5), TrackId::new(1));
        assert_eq!(
            keyboard_scroll_track(false, Some(selected), Some(hovered), primary),
            hovered
        );
        assert_eq!(
            keyboard_scroll_track(false, Some(selected), None, primary),
            selected
        );
        assert_eq!(keyboard_scroll_track(false, None, None, primary), primary);
    }

    fn hover_test_panel(id: u64) -> ViewPanelController {
        let interval = GenomicInterval::new(rgv_core::ContigId::new(0), 100, 200).unwrap();
        ViewPanelController::new(
            PanelId::new(id),
            Viewport::new(interval, 1_000.0).unwrap(),
            None,
            "chr1".to_owned(),
            1_000_000,
        )
    }

    fn hover_test_hit(offset: u64) -> AlignmentHit {
        use rgv_alignment_scene::{
            AlignmentComponent, AlignmentHandle, AlignmentSourceRevision, RevisionFingerprint,
            SceneRevision, SourceFingerprint,
        };
        AlignmentHit {
            track: TrackId::new(1),
            scene_revision: SceneRevision::new(RevisionFingerprint::new(22, 23)),
            handle: AlignmentHandle::new(
                AlignmentSourceRevision::new(
                    SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                    RevisionFingerprint::new(3, 4),
                ),
                rgv_hts::AlignmentSourcePosition::bam_virtual_offset(offset),
            ),
            component: AlignmentComponent::ReadSpan,
        }
    }

    #[test]
    fn read_hover_lands_in_the_panel_under_the_pointer_without_moving_focus() {
        let mut panels = PanelWorkspace::new(hover_test_panel(1));
        assert!(panels.push(hover_test_panel(2)));
        assert_eq!(panels.active().id, PanelId::new(1));

        let hit = hover_test_hit(1);
        let changed = retarget_alignment_hover(&mut panels, PanelId::new(2), Some(hit));
        assert_eq!(changed, vec![PanelId::new(2)]);
        assert_eq!(
            panels.panel(PanelId::new(2)).hovered_alignment_hit,
            Some(hit)
        );
        assert_eq!(panels.panel(PanelId::new(1)).hovered_alignment_hit, None);
        assert_eq!(
            panels.active().id,
            PanelId::new(1),
            "hover never moves focus"
        );

        // Moving back over the focused panel clears the other panel's hover in the same step.
        let next = hover_test_hit(2);
        let changed = retarget_alignment_hover(&mut panels, PanelId::new(1), Some(next));
        assert_eq!(changed, vec![PanelId::new(1), PanelId::new(2)]);
        assert_eq!(panels.panel(PanelId::new(2)).hovered_alignment_hit, None);
        assert_eq!(
            panels.panel(PanelId::new(1)).hovered_alignment_hit,
            Some(next)
        );

        assert!(retarget_alignment_hover(&mut panels, PanelId::new(1), Some(next)).is_empty());
    }

    #[test]
    fn hover_is_hit_tested_only_where_a_click_would_reach_the_canvas() {
        assert!(hover_hit_testing_allowed(false, false, true));
        assert!(
            !hover_hit_testing_allowed(false, false, false),
            "a floating window over the canvas blocks hover"
        );
        assert!(!hover_hit_testing_allowed(true, false, true));
        assert!(!hover_hit_testing_allowed(false, true, true));
        // The obstruction test itself: the pointer is inside the canvas rectangle, but another
        // layer (a floating window) is on top of it.
        let canvas = egui::LayerId::background();
        let window = egui::LayerId::new(egui::Order::Middle, egui::Id::new("floating"));
        assert!(hover_hit_testing_allowed(
            false,
            false,
            pointer_targets_unobstructed_canvas(true, Some(canvas), Some(canvas)),
        ));
        assert!(!hover_hit_testing_allowed(
            false,
            false,
            pointer_targets_unobstructed_canvas(true, Some(canvas), Some(window)),
        ));
    }

    #[test]
    fn native_canvas_pointer_survives_coarse_egui_area_consumption() {
        let context = egui::Context::default();
        let pointer = egui::pos2(50.0, 50.0);
        let mut canvas_layer = None;
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 200.0),
                )),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..egui::RawInput::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let response = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("test-genomic-canvas"),
                        egui::Sense::focusable_noninteractive(),
                    );
                    canvas_layer = Some(response.layer_id);
                });
            },
        );

        assert!(context.egui_wants_pointer_input());
        assert!(pointer_targets_unobstructed_canvas(
            true,
            canvas_layer,
            context.layer_id_at(pointer),
        ));
    }

    #[test]
    fn native_canvas_pointer_does_not_click_through_an_overlay_layer() {
        let canvas = egui::LayerId::new(egui::Order::Background, egui::Id::new("canvas"));
        let overlay = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("dialog"));

        assert!(!pointer_targets_unobstructed_canvas(
            true,
            Some(canvas),
            Some(overlay),
        ));
        assert!(!pointer_targets_unobstructed_canvas(
            false,
            Some(canvas),
            Some(canvas),
        ));
    }

    #[test]
    fn track_settings_window_blocks_native_viewer_wheel_navigation() {
        let context = egui::Context::default();
        let pointer = egui::pos2(80.0, 80.0);
        let mut viewer_layer = None;
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 240.0),
                )),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..egui::RawInput::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let response = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("test-track-viewport"),
                        egui::Sense::focusable_noninteractive(),
                    );
                    viewer_layer = Some(response.layer_id);
                });
                egui::Window::new("Track settings")
                    .fixed_pos(egui::pos2(20.0, 20.0))
                    .fixed_size(egui::vec2(180.0, 160.0))
                    .vscroll(true)
                    .show(root.ctx(), |ui| {
                        ui.label("Alignment track");
                        for index in 0..20 {
                            ui.label(format!("Setting {index}"));
                        }
                    });
            },
        );

        let layer_under_pointer = context.layer_id_at(pointer);
        assert_ne!(layer_under_pointer, viewer_layer);
        assert!(!pointer_targets_unobstructed_canvas(
            true,
            viewer_layer,
            layer_under_pointer,
        ));
    }
}
