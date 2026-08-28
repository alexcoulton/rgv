use super::super::*;

#[cfg(test)]
fn alignment_query_supports_gesture_detail(
    query: &AlignmentQuery,
    interval: GenomicInterval,
    detail: DetailLevel,
    bases_per_pixel: f64,
    reference_available: bool,
) -> bool {
    if !query.interval.contains_interval(interval) {
        return false;
    }
    if matches!(detail, DetailLevel::Reads | DetailLevel::Bases)
        && (0..query.batch.len()).any(|index| {
            matches!(
                query.batch.sequence(index),
                None | Some(rgv_hts::SequenceView::NotLoaded)
            )
        })
    {
        return false;
    }
    bases_per_pixel > MAXIMUM_BASE_MARK_BASES_PER_PIXEL
        || !reference_available
        || query
            .reference
            .as_ref()
            .is_some_and(|reference| reference.interval.contains_interval(interval))
}

fn read_selection_for_query_refresh(
    selected_track: Option<TrackId>,
    selected_record: Option<usize>,
    query_track: TrackId,
    query: &AlignmentQuery,
) -> Option<PendingReadSelection> {
    let track_id = selected_track.filter(|selected| *selected == query_track)?;
    let record = query.materialize_record(selected_record?).ok()?;
    PendingReadSelection::for_record(track_id, &record)
}

fn pointer_targets_unobstructed_canvas(
    canvas_contains_pointer: bool,
    canvas_layer: Option<egui::LayerId>,
    layer_under_pointer: Option<egui::LayerId>,
) -> bool {
    canvas_contains_pointer && canvas_layer.is_some() && canvas_layer == layer_under_pointer
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
        if self.dragging && !consumed {
            if let Some(previous) = self.last_cursor {
                let scale = self.window.scale_factor();
                let delta_points = (position.x - previous.x) / scale;
                if let Ok(viewport) = self
                    .panels
                    .active()
                    .viewport
                    .pan_pixels(-delta_points, self.panels.active_mut().contig_length)
                {
                    self.panels.active_mut().viewport = viewport;
                    self.viewport_gesture_changed();
                }
            }
            self.last_cursor = Some(position);
        } else if !self.dragging {
            self.last_cursor = None;
        }
        let hovered = (!consumed && !self.dragging && self.cursor_in_genomic_canvas())
            .then(|| self.record_at_cursor())
            .flatten();
        if hovered != self.panels.active_mut().hovered_record {
            self.panels.active_mut().hovered_record = hovered;
            self.display_dirty = true;
            self.window.request_redraw();
        }
        let cursor_in_genomic_canvas = self.cursor_in_genomic_canvas();
        let should_hit_test_junction_arc = !consumed && !self.dragging && cursor_in_genomic_canvas;
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
                total_targets: self.panels.active().junction_arc_targets.len(),
                ..JunctionArcHitTestResult::default()
            }
        };
        let junction_hit_duration = junction_hit_started.map(|started| started.elapsed());
        let hovered_junction_arc = junction_hit.hover;
        let previous_junction_arc = self.panels.active().hovered_junction_arc;
        let junction_arc_changed = hovered_junction_arc
            .map(|hover| (hover.key, hover.interaction_token, hover.content_revision))
            != previous_junction_arc
                .map(|hover| (hover.key, hover.interaction_token, hover.content_revision));
        let scale = self.window.scale_factor();
        self.sashimi_interaction_diagnostics.record_latest_probe(
            egui::pos2((position.x / scale) as f32, (position.y / scale) as f32),
            self.panels.active().navigation_presentation.phase,
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
            self.panels.active_mut().hovered_junction_arc = hovered_junction_arc;
            self.window.set_cursor(if hovered_junction_arc.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
            if self.synthetic || !self.refresh_active_panel_scroll_display() {
                self.display_dirty = true;
            }
            self.window.request_redraw();
        }
        if self.inspector_activation_at_cursor() == InspectorActivation::Hover
            && !consumed
            && !self.dragging
        {
            let target = self.hover_inspector_target_at_cursor();
            if target != self.panels.active().hover_inspector_target {
                self.panels.active_mut().hover_inspector_target = target;
                if target.is_some_and(|target| {
                    !matches!(target, HoverInspectorTarget::Junction(_, _, _, _))
                }) {
                    self.select_at_cursor();
                }
                self.window.request_redraw();
            }
        } else if self.panels.active().hover_inspector_target.is_some() {
            self.panels.active_mut().hover_inspector_target = None;
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
        let (track_id, index) = self.record_at_cursor()?;
        let event = self.read_event_at_cursor(track_id, index);
        Some(HoverInspectorTarget::Record {
            track_id,
            index,
            operation_index: event.map(|event| event.operation_index),
            operation_offset: event.and_then(|event| event.operation_offset),
        })
    }

    pub(crate) fn zoom(&mut self, delta: MouseScrollDelta) {
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y),
            MouseScrollDelta::PixelDelta(position) => position.y * 0.02,
        };
        let factor = 0.85_f64.powf(amount);
        let panel_rect = self.active_panel_rect();
        let anchor_points =
            self.cursor.x / self.window.scale_factor() - f64::from(panel_rect.left());
        if let Ok(viewport) = self.panels.active_mut().viewport.zoom_at(
            anchor_points,
            factor,
            self.panels.active_mut().contig_length,
        ) {
            self.panels.active_mut().viewport = viewport;
            self.panels
                .active_mut()
                .navigation_history
                .visit(viewport.interval());
            self.viewport_gesture_changed();
        }
    }

    pub(crate) fn scroll_tracks(&mut self, delta: MouseScrollDelta) {
        let points = wheel_primary_axis(delta);
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let annotation_clip_bottom = self
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .map_or(self.canvas_geometry().viewport.top(), |cache| {
                cache.annotation_clip_bottom
            });
        if cursor_y < annotation_clip_bottom {
            self.scroll_annotations_by(-points);
        } else if let Some(track_id) = self.read_panel_at_screen_y(cursor_y) {
            self.scroll_read_panel_by(track_id, -points);
        }
    }

    /// Backward-compatible primary-track entry point used by automation and
    /// older tests. Pointer interaction calls `scroll_read_panel_by` directly.
    pub(crate) fn scroll_tracks_by(&mut self, points: f32) {
        self.scroll_read_panel_by(self.primary_alignment_track_id(), points);
    }

    pub(crate) fn scroll_read_panel_by(&mut self, track_id: TrackId, points: f32) {
        let current = self
            .panels
            .active()
            .read_panel_scroll_state(track_id)
            .scroll_points;
        self.set_read_panel_scroll(track_id, current + points);
    }

    fn set_read_panel_scroll(&mut self, track_id: TrackId, requested: f32) {
        let Some((_, clip_bottom, _)) = self.read_panel_metrics(track_id) else {
            return;
        };
        let state = self.panels.active().read_panel_scroll_state(track_id);
        let maximum = (state.content_bottom_points - clip_bottom).max(0.0);
        let next = requested.clamp(0.0, maximum);
        if (next - state.scroll_points).abs() <= f32::EPSILON {
            return;
        }
        self.invalidate_navigation_preview_contract("read panel vertical scroll changed");
        let primary_track_id = self.primary_alignment_track_id();
        let latent_fraction = {
            let state = self
                .panels
                .active_mut()
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
            self.panels.active_mut().vertical_scroll_points = next;
            self.panels.active_mut().latent_vertical_scroll_fraction = latent_fraction;
        }
        if self.synthetic || !self.refresh_active_panel_scroll_display() {
            self.display_dirty = true;
        }
        self.window.request_redraw();
    }

    pub(crate) fn scroll_annotations_by(&mut self, points: f32) {
        let maximum = self.panels.maximum_annotation_scroll_points();
        let next = (self.panels.annotation_scroll_points + points).clamp(0.0, maximum);
        if (next - self.panels.annotation_scroll_points).abs() <= f32::EPSILON {
            return;
        }
        self.invalidate_navigation_preview_contract("annotation vertical scroll changed");
        if !self.refresh_shared_annotation_scroll_display(next) {
            self.display_dirty = true;
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
            Key::Named(NamedKey::ArrowUp) => self.scroll_tracks_by(-80.0),
            Key::Named(NamedKey::ArrowDown) => self.scroll_tracks_by(80.0),
            Key::Named(NamedKey::PageUp) => self.scroll_tracks_by(-self.canvas_page_height()),
            Key::Named(NamedKey::PageDown) => self.scroll_tracks_by(self.canvas_page_height()),
            Key::Named(NamedKey::Home) => self.navigate_home(),
            Key::Named(NamedKey::Enter) => {
                if let Some(track) = self.panels.active_mut().selected_track {
                    self.open_panel = Some(UiPanel::TrackSettings(track));
                }
            }
            _ => {}
        }
        self.window.request_redraw();
    }

    pub(crate) fn keyboard_zoom(&mut self, factor: f64) {
        if let Ok(viewport) = self.panels.active_mut().viewport.zoom_at(
            self.panels.active_mut().viewport.pixel_width() / 2.0,
            factor,
            self.panels.active_mut().contig_length,
        ) {
            self.panels.active_mut().viewport = viewport;
            self.panels
                .active_mut()
                .navigation_history
                .visit(viewport.interval());
            self.viewport_changed();
        }
    }

    pub(crate) fn keyboard_pan(&mut self, fraction: f64) {
        if let Ok(viewport) = self.panels.active_mut().viewport.pan_pixels(
            self.panels.active_mut().viewport.pixel_width() * fraction,
            self.panels.active_mut().contig_length,
        ) {
            self.panels.active_mut().viewport = viewport;
            self.panels
                .active_mut()
                .navigation_history
                .visit(viewport.interval());
            self.viewport_changed();
        }
    }

    pub(crate) fn viewport_changed(&mut self) {
        self.remember_current_locus();
        self.panels.active_mut().hovered_record = None;
        self.panels.active_mut().hover_inspector_target = None;
        self.panels.active_mut().hovered_junction_arc = None;
        self.panels.active_mut().junction_arc_targets.clear();
        self.window.set_cursor(CursorIcon::Default);
        self.begin_viewport_navigation(false, false);
    }

    pub(crate) fn viewport_gesture_changed(&mut self) {
        self.remember_current_locus();
        self.panels.active_mut().hovered_record = None;
        self.panels.active_mut().hover_inspector_target = None;
        self.panels.active_mut().hovered_junction_arc = None;
        self.panels.active_mut().junction_arc_targets.clear();
        self.window.set_cursor(CursorIcon::Default);
        let hold_commit_until_end = self.dragging
            || self.horizontal_overview_drag.is_some()
            || self.overview_capture == Some(OverviewCapture::Horizontal);
        self.begin_viewport_navigation(true, hold_commit_until_end);
    }

    /// Preserve a read selection across an alignment query refresh.
    ///
    /// Alignment object IDs are query-local record indexes. Keeping the index
    /// alone would either drop the highlight or select an unrelated record
    /// when a zoom changes the loaded query. The read name is stable across
    /// detail levels. Contig/start and template-end identity disambiguate
    /// paired or supplementary records which legitimately share that name.
    pub(crate) fn retain_active_read_selection_for_query_refresh(&mut self) {
        let Some(track_id) = self.panels.active().selected_alignment_track else {
            return;
        };
        if self.panels.active().pending_read_selection.is_some() {
            // The semantic identity is the authority once it has been
            // captured. A record index may already belong to a replacement
            // query, so never use it to overwrite that identity during a
            // later refresh.
            return;
        }
        if self.panels.active().selected_record.is_none() {
            return;
        }
        let Some(pending) = self
            .complete_read_query_for_track(track_id)
            .and_then(|query| {
                read_selection_for_query_refresh(
                    self.panels.active().selected_alignment_track,
                    self.panels.active().selected_record,
                    track_id,
                    query,
                )
            })
        else {
            return;
        };
        self.panels.active_mut().pending_read_selection = Some(pending);
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_overview_interaction(&mut self, interaction: ui::OverviewInteraction) {
        if interaction.pointer_down && self.overview_capture.is_none() {
            self.suppress_horizontal_overview_click = false;
        }
        if interaction.show_all {
            if let Ok(interval) = GenomicInterval::new(
                self.panels.active_mut().viewport.interval().contig(),
                0,
                self.panels.active_mut().contig_length.max(1),
            ) && let Ok(viewport) =
                Viewport::new(interval, self.panels.active_mut().viewport.pixel_width())
            {
                self.panels.active_mut().viewport = viewport;
                self.panels.active_mut().navigation_history.visit(interval);
                self.horizontal_overview_drag = None;
                self.viewport_changed();
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
                .panels
                .active()
                .viewport
                .centered_on(position, self.panels.active_mut().contig_length)
            {
                self.panels.active_mut().viewport = viewport;
                self.panels
                    .active_mut()
                    .navigation_history
                    .visit(viewport.interval());
                self.viewport_changed();
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
                self.panels.active().viewport,
                anchor_position,
                interaction.press_handle_fraction,
                self.panels.active().contig_length,
            );
            self.horizontal_overview_drag = Some(HorizontalOverviewDrag {
                anchor_fraction,
                last_pointer_x: press_x,
                last_pointer_y: press_y,
            });
            if viewport != self.panels.active_mut().viewport {
                self.panels.active_mut().viewport = viewport;
                self.viewport_gesture_changed();
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
                self.panels.active_mut().viewport,
                drag.anchor_fraction,
                delta_x,
                delta_y,
                self.panels.active_mut().contig_length,
            ) {
                self.panels.active_mut().viewport = viewport;
                self.viewport_gesture_changed();
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
            let interval = self.panels.active().viewport.interval();
            self.panels.active_mut().navigation_history.visit(interval);
            self.finish_navigation_gesture();
        }
    }

    pub(crate) fn apply_read_panel_overview_interaction(
        &mut self,
        track_id: TrackId,
        interaction: ui::ScrollbarInteraction,
        requested_scroll: &mut f32,
    ) {
        if interaction.pointer_down {
            self.interaction_trace
                .record_once(InteractionTraceStage::EguiPointerDown);
        }
        if interaction.keyboard_density_pixels.abs() > f32::EPSILON {
            let viewport_height = self.read_panel_metrics(track_id).map_or(
                self.canvas_geometry().viewport.height(),
                |(top, bottom, _)| (bottom - top).max(1.0),
            );
            let panel_id = self.panels.active().id;
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
                self.display_dirty = true;
            }
            let maximum = self
                .read_panel_metrics(track_id)
                .map_or(0.0, |(_, clip_bottom, _)| {
                    (self
                        .panels
                        .active()
                        .read_panel_scroll_state(track_id)
                        .content_bottom_points
                        - clip_bottom)
                        .max(0.0)
                });
            *requested_scroll = scroll.clamp(0.0, maximum);
            self.display_dirty = true;
        }
        self.set_read_panel_scroll(track_id, *requested_scroll);
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
        let target_panel = self.panels.iter().enumerate().find_map(|(index, panel)| {
            (self.panel_scrollbar_rect_at(index).contains(pointer)
                && panel.scroll_display_cache.as_ref().is_some_and(|cache| {
                    cache.read_panels.iter().any(|read_panel| {
                        pointer.y >= read_panel.clip_top && pointer.y < read_panel.clip_bottom
                    })
                }))
            .then_some(panel.id)
        });
        let Some(target_panel) = target_panel else {
            return false;
        };
        self.activate_view_panel(target_panel);
        let Some(track_id) = self.read_panel_at_screen_y(pointer.y) else {
            return false;
        };
        let Some((clip_top, clip_bottom, _)) = self.read_panel_metrics(track_id) else {
            return false;
        };
        let state = self.panels.active().read_panel_scroll_state(track_id);
        let maximum = (state.content_bottom_points - clip_bottom).max(0.0);
        let camera_viewport_height = (clip_bottom - clip_top).max(1.0);
        let scrollbar_rect = self
            .read_panel_scrollbar_rect(track_id)
            .unwrap_or(self.canvas_geometry().scrollbar);
        let density = self.alignment_density_for_panel_track(self.panels.active().id, track_id);
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
            panel_id: self.panels.active().id,
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
        self.interaction_trace.begin();
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
            self.interaction_trace
                .record_once(InteractionTraceStage::DensityIntentDetected);
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
        let Some((clip_top, clip_bottom, _)) = self.read_panel_metrics(drag.track_id) else {
            return false;
        };
        let state = self.panels.active().read_panel_scroll_state(drag.track_id);
        let maximum = (state.content_bottom_points - clip_bottom).max(0.0);
        let camera_viewport_height = (clip_bottom - clip_top).max(1.0);
        let density = self.alignment_density_for_panel_track(drag.panel_id, drag.track_id);
        let scrollbar_rect = self
            .read_panel_scrollbar_rect(drag.track_id)
            .unwrap_or(self.canvas_geometry().scrollbar);
        let geometry = ui::scrollbar_geometry_for_density_in_viewport(
            scrollbar_rect,
            maximum,
            state.scroll_points,
            density,
            camera_viewport_height,
        );
        let next = ui::scrollbar_offset_at_grab(geometry, maximum, pointer.y, drag.grab_offset);
        self.set_read_panel_scroll(drag.track_id, next);
        drag.last_pointer_x = pointer.x;
        self.vertical_overview_drag = Some(drag);
        self.window.request_redraw();
        true
    }

    pub(crate) fn finish_vertical_scrollbar_drag(&mut self) {
        if self.vertical_overview_drag.take().is_some() {
            if self.interaction_trace.is_active() {
                self.interaction_trace.release();
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
                    self.interaction_trace
                        .record_once(InteractionTraceStage::PointerCaptureAcquired);
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
                self.interaction_trace
                    .record_once(InteractionTraceStage::DensityIntentDetected);
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
                    self.panels.active_mut().viewport,
                    drag.anchor_fraction,
                    delta_x,
                    delta_y,
                    self.panels.active_mut().contig_length,
                ) {
                    self.panels.active_mut().viewport = viewport;
                    self.viewport_gesture_changed();
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
        if drag.panel_id != self.panels.active().id {
            return;
        }
        let previous_density = self.alignment_density_for_panel_track(drag.panel_id, drag.track_id);
        let density = density_from_horizontal_drag(previous_density, delta_x);
        let Some((read_viewport_top, read_viewport_bottom, actual_content_bottom)) =
            self.read_panel_metrics(drag.track_id)
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
        let previous_state = self.panels.active().read_panel_scroll_state(drag.track_id);
        let previous_effective_scroll = previous_state.effective_scroll_points();
        let camera_padding = vertical_density_camera_padding(viewport_height, density);
        let zoom_scroll = zoom_fraction * maximum;
        let content_origin = (zoom_scroll - previous_effective_scroll).clamp(0.0, camera_padding);
        let density_changed = (density - previous_density).abs() > 1.0e-4;
        let scroll_changed = (next_scroll - previous_state.scroll_points).abs() > f32::EPSILON;
        self.vertical_overview_drag = Some(drag);
        let state = self
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
            self.panels.active_mut().vertical_scroll_points = next_scroll;
            self.panels.active_mut().vertical_content_origin_points = content_origin;
            self.panels.active_mut().latent_vertical_scroll_fraction =
                Some(drag.virtual_scroll_fraction);
            self.panels.active_mut().content_bottom_points = read_viewport_bottom + maximum;
        }
        if density_changed {
            self.set_read_panel_density(drag.panel_id, drag.track_id, density);
            self.display_dirty = true;
            self.interaction_trace
                .record_once(InteractionTraceStage::DensityChanged);
        } else if scroll_changed {
            self.invalidate_navigation_preview_contract("read panel vertical scroll changed");
            if self.synthetic || !self.refresh_active_panel_scroll_display() {
                self.display_dirty = true;
            }
        } else if (drag.virtual_scroll_fraction - previous_fraction).abs() > f32::EPSILON {
            self.window.request_redraw();
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
                    let interval = self.panels.active().viewport.interval();
                    self.panels.active_mut().navigation_history.visit(interval);
                    self.finish_navigation_gesture();
                }
            }
            OverviewCapture::VerticalDensity => {
                self.display_dirty = true;
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn query_options_changed(&mut self) {
        self.remember_current_locus();
        self.display_dirty = true;
        self.layout_cache.clear();
        self.invalidate_navigation_preview_contract("query or presentation options changed");
        if !self.synthetic && (self.has_alignment_engine() || self.track_engine.is_some()) {
            for panel in &mut self.panels.panels {
                panel.generation_clock.advance();
            }
            for track in &mut self.alignment_tracks {
                for projection in track.projections.values_mut() {
                    projection.clock.advance();
                    // Retain the last committed query only as non-visible
                    // history. Generation gates replace its read geometry and
                    // interactions with the neutral Checking state until the
                    // replacement query and layout horizon commit together.
                    projection.cached_layout = None;
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
        self.panels
            .iter()
            .position(|panel| panel.id == id)
            .map(|index| self.panel_column_rect_at(index))
    }

    pub(crate) fn active_panel_rect(&self) -> egui::Rect {
        self.panel_column_rect(self.panels.active().id)
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
        let current_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let layout_ids = self
            .workspace_layout
            .panels
            .iter()
            .map(|panel| panel.panel_id)
            .collect::<Vec<_>>();
        if current_ids != layout_ids {
            let panel_inputs = self
                .panels
                .iter()
                .map(|panel| {
                    let cytobands = if self
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
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in &ids {
            let width = self
                .workspace_layout
                .panel(*id)
                .expect("committed workspace layout contains every locus panel")
                .canvas
                .width()
                .max(1.0);
            if let Some(panel) = self.panels.get_mut(*id)
                && let Ok(viewport) = panel.viewport.resized(f64::from(width))
            {
                panel.viewport = viewport;
            }
        }
        for track in &mut self.alignment_tracks {
            for id in &ids {
                let projection = track.projection_mut(*id);
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
        }
        self.layout_cache.clear();
        self.display_dirty = true;
    }

    pub(crate) fn activate_view_panel(&mut self, id: PanelId) -> bool {
        let changed = id != self.panels.active().id;
        if changed {
            self.panels.active_mut().hovered_junction_arc = None;
            self.panels.active_mut().hover_inspector_target = None;
        }
        if !self.panels.activate(id) {
            return false;
        }
        if changed {
            self.panels.active_mut().hovered_junction_arc = None;
            self.panels.active_mut().hover_inspector_target = None;
            self.window.set_cursor(CursorIcon::Default);
        }
        self.refresh_active_panel_context();
        self.display_dirty = true;
        if changed && !self.synthetic {
            // A panel switch must not turn its semantic selection back into an
            // unsafe query-local record index when activation refreshes data.
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query();
            self.request_reference_query();
            self.request_track_query();
        }
        true
    }

    pub(crate) fn refresh_active_panel_context(&mut self) {
        let interval = self.panels.active().viewport.interval();
        self.locus_editor = format!(
            "{}:{}-{}",
            self.panels.active().contig_name,
            interval.start().saturating_add(1),
            interval.end()
        );
    }

    pub(crate) fn add_view_panel(&mut self, locus: &str) -> Result<PanelId> {
        anyhow::ensure!(
            self.panels.len() < MAXIMUM_VIEW_PANELS,
            "a workspace supports at most {MAXIMUM_VIEW_PANELS} view panels"
        );
        anyhow::ensure!(
            self.workspace_layout
                .projected_canvas_width(self.panels.len() + 1)
                >= 140.0,
            "the window is too narrow to add another readable panel"
        );
        let dictionary = self
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref())
            .unwrap_or(&self.assembly.dictionary);
        let (interval, contig_name, contig_length) =
            resolve_initial_locus(dictionary, &self.contig_aliases, Some(locus))?;
        let id = self.panels.next_id();
        let width = f64::from(
            self.workspace_layout
                .projected_canvas_width(self.panels.len() + 1)
                .max(1.0),
        );
        let viewport = Viewport::new(interval, width)?;
        let mut panel = ViewPanelController::new(
            id,
            viewport,
            Some(locus.to_owned()),
            contig_name,
            contig_length,
        );
        panel.content_bottom_points = self.canvas_geometry().viewport.top();
        anyhow::ensure!(self.panels.push(panel), "view panel identity collision");
        self.invalidate_navigation_preview_contract("view panel added");
        for track in &mut self.alignment_tracks {
            track.projection_mut(id).pending_query =
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
        }
        self.synchronize_panel_widths();
        self.activate_view_panel(id);
        self.request_all_panel_queries();
        Ok(id)
    }

    pub(crate) fn close_view_panel(&mut self, id: PanelId) -> bool {
        if self.panels.len() == 1 || self.panels.get(id).is_none() {
            return false;
        }
        for track in &mut self.alignment_tracks {
            if let Some(projection) = track.projections.remove(&id) {
                projection.clock.advance();
            }
        }
        let removed = self.panels.remove(id).is_some();
        if removed {
            self.invalidate_navigation_preview_contract("view panel removed");
            self.synchronize_panel_widths();
            self.refresh_active_panel_context();
            self.request_all_panel_queries();
            self.rebuild_all_panel_displays();
        }
        removed
    }

    pub(crate) fn move_view_panel(&mut self, id: PanelId, new_index: usize) -> bool {
        let moved = self.panels.move_panel(id, new_index);
        if moved {
            self.invalidate_navigation_preview_contract("view panel order changed");
            self.display_dirty = true;
            self.rebuild_all_panel_displays();
        }
        moved
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
        self.panels.iter().enumerate().any(|(index, panel)| {
            self.panel_scrollbar_rect_at(index).contains(point)
                && panel.scroll_display_cache.as_ref().is_some_and(|cache| {
                    cache.read_panels.iter().any(|read_panel| {
                        point.y >= read_panel.clip_top && point.y < read_panel.clip_bottom
                    })
                })
        })
    }

    fn read_panel_at_screen_y(&self, screen_y: f32) -> Option<TrackId> {
        self.panels
            .active()
            .scroll_display_cache
            .as_ref()?
            .read_panels
            .iter()
            .find(|panel| screen_y >= panel.clip_top && screen_y < panel.clip_bottom)
            .map(|panel| panel.track_id)
    }

    fn read_panel_metrics(&self, track_id: TrackId) -> Option<(f32, f32, f32)> {
        self.panels
            .active()
            .scroll_display_cache
            .as_ref()?
            .read_panels
            .iter()
            .find(|panel| panel.track_id == track_id)
            .map(|panel| {
                (
                    panel.clip_top,
                    panel.clip_bottom,
                    panel.actual_content_bottom,
                )
            })
    }

    fn read_panel_scrollbar_rect(&self, track_id: TrackId) -> Option<egui::Rect> {
        let (top, bottom, _) = self.read_panel_metrics(track_id)?;
        let active_index = self.panels.active;
        let full = self.panel_scrollbar_rect_at(active_index);
        Some(egui::Rect::from_min_max(
            egui::pos2(full.left(), top.clamp(full.top(), full.bottom())),
            egui::pos2(full.right(), bottom.clamp(full.top(), full.bottom())),
        ))
    }

    pub(crate) fn inspector_activation_at_cursor(&self) -> InspectorActivation {
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let panel = self.panels.active();
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
                        self.panels.annotation_scroll_points
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
                    self.alignment_tracks
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

    #[test]
    fn hover_targets_distinguish_substitutions_inside_one_cigar_operation() {
        let target = |operation_offset| HoverInspectorTarget::Record {
            track_id: TrackId::new(1),
            index: 7,
            operation_index: Some(2),
            operation_offset: Some(operation_offset),
        };
        assert_ne!(target(3), target(4));
    }

    fn gesture_query(interval: GenomicInterval) -> AlignmentQuery {
        AlignmentQuery {
            interval,
            header: Arc::default(),
            batch: rgv_hts::AlignmentBatch::default(),
            pileup: rgv_hts::PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats::default(),
        }
    }

    #[test]
    fn selected_read_identity_survives_query_refresh_boundaries() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let mut query = gesture_query(interval);
        query
            .batch
            .push(&AlignmentRecord {
                contig: ContigId::new(0),
                start: 101,
                end: 106,
                flags: rgv_hts::AlignmentFlags::from_bits(0),
                mapping_quality: Some(42),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: b"selected-read".to_vec(),
                cigar: vec![rgv_hts::CigarOp {
                    kind: rgv_hts::CigarOpKind::Match,
                    len: 5,
                }],
                sequence: rgv_hts::SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .unwrap();
        let track_id = TrackId::new(7);

        assert_eq!(
            read_selection_for_query_refresh(Some(track_id), Some(0), track_id, &query),
            PendingReadSelection::for_record(track_id, &query.materialize_record(0).unwrap())
        );
        assert_eq!(
            read_selection_for_query_refresh(Some(track_id), Some(1), track_id, &query),
            None
        );
        assert_eq!(
            read_selection_for_query_refresh(Some(TrackId::new(8)), Some(0), track_id, &query),
            None
        );
    }

    #[test]
    fn mate_selection_targets_the_exact_opposite_template_end() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 50, 260).unwrap();
        let mut query = gesture_query(interval);
        let record = |start, flags, mate_start| AlignmentRecord {
            contig,
            start,
            end: start + 40,
            flags: rgv_hts::AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: Some(contig),
            mate_start: Some(mate_start),
            template_length: 140,
            read_name: b"paired-read".to_vec(),
            cigar: Vec::new(),
            sequence: rgv_hts::SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let first = record(100, 0x41, 200);
        let last = record(200, 0x81, 100);
        query.batch.push(&first).unwrap();
        query.batch.push(&last).unwrap();
        let track_id = TrackId::new(7);

        assert_eq!(
            PendingReadSelection::for_record(track_id, &first)
                .unwrap()
                .matching_index(&query),
            Some(0)
        );
        assert_eq!(
            PendingReadSelection::for_mate(track_id, &first)
                .unwrap()
                .matching_index(&query),
            Some(1)
        );
    }

    #[test]
    fn resolved_read_identity_rebinds_when_zoom_out_changes_its_index() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 50, 300).unwrap();
        let record = |name: &[u8], start| AlignmentRecord {
            contig,
            start,
            end: start + 25,
            flags: rgv_hts::AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: Vec::new(),
            sequence: rgv_hts::SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let selected = record(b"selected-read", 150);
        let track_id = TrackId::new(7);
        let identity = PendingReadSelection::for_record(track_id, &selected).unwrap();

        let mut wider_query = gesture_query(interval);
        wider_query
            .batch
            .push(&record(b"newly-fetched-read", 75))
            .unwrap();
        wider_query.batch.push(&selected).unwrap();
        assert_eq!(identity.matching_index(&wider_query), Some(1));

        let rebound_record = wider_query.materialize_record(1).unwrap();
        let (retained, reveal) = identity.retained_after_resolution(Some(&rebound_record));
        assert!(!reveal);
        assert_eq!(retained.matching_index(&wider_query), Some(1));

        let mut widest_query = gesture_query(interval);
        widest_query
            .batch
            .push(&record(b"another-new-read", 55))
            .unwrap();
        widest_query
            .batch
            .push(&record(b"newly-fetched-read", 75))
            .unwrap();
        widest_query.batch.push(&selected).unwrap();
        assert_eq!(retained.matching_index(&widest_query), Some(2));
    }

    #[test]
    fn mate_panel_identities_rebind_independently_after_zoom_out() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 50, 350).unwrap();
        let paired_record = |start, flags, mate_start| AlignmentRecord {
            contig,
            start,
            end: start + 40,
            flags: rgv_hts::AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: Some(contig),
            mate_start: Some(mate_start),
            template_length: 140,
            read_name: b"paired-read".to_vec(),
            cigar: Vec::new(),
            sequence: rgv_hts::SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let unrelated = |name: &[u8], start| AlignmentRecord {
            read_name: name.to_vec(),
            ..paired_record(start, 0, start)
        };
        let first = paired_record(100, 0x41, 200);
        let last = paired_record(200, 0x81, 100);
        let track_id = TrackId::new(7);
        let source_identity = PendingReadSelection::for_record(track_id, &first).unwrap();
        let mate_identity = PendingReadSelection::for_mate(track_id, &first).unwrap();

        let mut source_query = gesture_query(interval);
        source_query
            .batch
            .push(&unrelated(b"source-prefix", 60))
            .unwrap();
        source_query.batch.push(&first).unwrap();
        source_query.batch.push(&last).unwrap();

        let mut mate_query = gesture_query(interval);
        mate_query
            .batch
            .push(&unrelated(b"mate-prefix-a", 150))
            .unwrap();
        mate_query
            .batch
            .push(&unrelated(b"mate-prefix-b", 175))
            .unwrap();
        mate_query.batch.push(&last).unwrap();

        assert_eq!(source_identity.matching_index(&source_query), Some(1));
        assert_eq!(mate_identity.matching_index(&mate_query), Some(2));

        let mate_record = mate_query.materialize_record(2).unwrap();
        let (retained_mate, reveal) = mate_identity.retained_after_resolution(Some(&mate_record));
        assert!(reveal);
        assert_eq!(retained_mate.matching_index(&mate_query), Some(2));
        let (_, reveal_again) = retained_mate.retained_after_resolution(Some(&mate_record));
        assert!(!reveal_again);
    }

    #[test]
    fn gesture_query_refines_immediately_when_crossing_base_mark_threshold() {
        let contig = ContigId::new(0);
        let loaded = GenomicInterval::new(contig, 100, 300).unwrap();
        let visible = GenomicInterval::new(contig, 150, 200).unwrap();
        let mut query = gesture_query(loaded);

        assert!(alignment_query_supports_gesture_detail(
            &query,
            visible,
            DetailLevel::Reads,
            MAXIMUM_BASE_MARK_BASES_PER_PIXEL + 0.01,
            true,
        ));
        assert!(!alignment_query_supports_gesture_detail(
            &query,
            visible,
            DetailLevel::Reads,
            MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
            true,
        ));

        query.reference = Some(ReferenceSlice {
            interval: loaded,
            bases: vec![b'A'; loaded.len() as usize],
        });
        assert!(alignment_query_supports_gesture_detail(
            &query,
            visible,
            DetailLevel::Reads,
            MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
            true,
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
