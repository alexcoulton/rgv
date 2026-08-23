use super::super::*;

impl Viewer {
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
                    self.viewport_changed();
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
        if self.inspector_activation_at_cursor() == InspectorActivation::Hover
            && !consumed
            && !self.dragging
        {
            let target = self.hover_inspector_target_at_cursor();
            if target.is_some() && target != self.panels.active_mut().hover_inspector_target {
                self.panels.active_mut().hover_inspector_target = target;
                self.select_at_cursor();
                self.window.request_redraw();
            }
        } else if consumed || self.dragging {
            self.panels.active_mut().hover_inspector_target = None;
        }
    }

    pub(crate) fn hover_inspector_target_at_cursor(&self) -> Option<HoverInspectorTarget> {
        if let Some((track_id, position)) = self.modification_position_at_cursor() {
            return Some(HoverInspectorTarget::Modification(track_id, position));
        }
        if let Some((track_id, position)) = self.pileup_position_at_cursor() {
            return Some(HoverInspectorTarget::Pileup(track_id, position));
        }
        if let Some((track_id, start, end)) = self.junction_at_cursor() {
            return Some(HoverInspectorTarget::Junction(track_id, start, end));
        }
        let (track_id, index) = self.record_at_cursor()?;
        Some(HoverInspectorTarget::Record {
            track_id,
            index,
            operation_index: self
                .read_event_at_cursor(track_id, index)
                .map(|event| event.operation_index),
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
            self.viewport_changed();
        }
    }

    pub(crate) fn scroll_tracks(&mut self, delta: MouseScrollDelta) {
        let points = wheel_primary_axis(delta);
        self.scroll_tracks_by(-points);
    }

    pub(crate) fn scroll_tracks_by(&mut self, points: f32) {
        let maximum = (self.panels.active().content_bottom_points
            - self.canvas_geometry.viewport.bottom())
        .max(0.0);
        let next = (self.panels.active().vertical_scroll_points + points).clamp(0.0, maximum);
        if (next - self.panels.active().vertical_scroll_points).abs() <= f32::EPSILON {
            return;
        }
        self.panels.active_mut().vertical_scroll_points = next;
        if self.synthetic {
            self.display_dirty = true;
        } else {
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
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
        self.panels.active_mut().last_gesture_query_at = None;
        self.remember_current_locus();
        self.panels.active_mut().hovered_record = None;
        self.display_dirty = true;
        if !self.synthetic
            && (self.primary_alignment_dictionary().is_some()
                || self.reference_dictionary.is_some()
                || self.track_engine.is_some())
        {
            self.panels.active().generation_clock.advance();
            self.advance_alignment_generations(self.panels.active().id);
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().pending_read_selection = None;
            let interval = self.panels.active().viewport.interval();
            let alignment_covers_viewport = !self.has_alignment_engine()
                || self
                    .primary_alignment_query()
                    .is_some_and(|query| query.interval.contains_interval(interval));
            let tracks_cover_viewport = self.track_engine.is_none()
                || self
                    .panels
                    .active()
                    .track_snapshot_interval
                    .is_some_and(|loaded| loaded.contains_interval(interval));
            let reference_covers_viewport = self.reference_engine.is_none()
                || self.panels.active().viewport.detail_level() != DetailLevel::Bases
                || self
                    .panels
                    .active()
                    .reference_slice
                    .as_ref()
                    .is_some_and(|slice| slice.interval.contains_interval(interval));
            if alignment_covers_viewport && tracks_cover_viewport && reference_covers_viewport {
                self.rebuild_genomic_display();
            }
            self.request_alignment_query();
            self.request_reference_query();
            self.request_track_query();
        }
    }

    pub(crate) fn viewport_gesture_changed(&mut self) {
        self.remember_current_locus();
        self.panels.active_mut().hovered_record = None;
        self.display_dirty = true;
        self.rebuild_genomic_display();
        self.request_gesture_refinement_if_needed();
    }

    pub(crate) fn request_gesture_refinement_if_needed(&mut self) {
        if self.synthetic {
            return;
        }
        let interval = self.panels.active().viewport.interval();
        let alignment_needs_refinement = self.has_alignment_engine()
            && !self
                .primary_alignment_query()
                .is_some_and(|query| query.interval.contains_interval(interval));
        let tracks_need_refinement = self.track_engine.is_some()
            && !self
                .panels
                .active()
                .track_snapshot_interval
                .is_some_and(|loaded| loaded.contains_interval(interval));
        let reference_needs_refinement = self.reference_engine.is_some()
            && self.panels.active_mut().viewport.detail_level() == DetailLevel::Bases
            && !self
                .panels
                .active()
                .reference_slice
                .as_ref()
                .is_some_and(|slice| slice.interval.contains_interval(interval));
        if !alignment_needs_refinement && !tracks_need_refinement && !reference_needs_refinement {
            return;
        }
        let now = Instant::now();
        if self
            .panels
            .active()
            .last_gesture_query_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.panels.active_mut().last_gesture_query_at = Some(now);
        self.panels.active().generation_clock.advance();
        self.advance_alignment_generations(self.panels.active().id);
        if alignment_needs_refinement {
            self.request_alignment_query();
        }
        if reference_needs_refinement {
            self.request_reference_query();
        }
        if tracks_need_refinement {
            self.request_track_query();
        }
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
            if viewport != self.panels.active_mut().viewport {
                self.panels.active_mut().viewport = viewport;
                self.viewport_gesture_changed();
            }
            self.horizontal_overview_drag = Some(HorizontalOverviewDrag {
                anchor_fraction,
                last_pointer_x: press_x,
                last_pointer_y: press_y,
            });
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
            self.viewport_changed();
        }
    }

    pub(crate) fn apply_vertical_overview_interaction(
        &mut self,
        interaction: ui::ScrollbarInteraction,
        requested_scroll: &mut f32,
    ) {
        if interaction.keyboard_density_pixels.abs() > f32::EPSILON {
            let anchor_y = self.canvas_geometry.viewport.height() * 0.5;
            let (density, scroll) = vertical_overview_step(
                self.primary_alignment_filters().alignment_density,
                *requested_scroll,
                anchor_y,
                interaction.keyboard_density_pixels,
                0.0,
            );
            if (density - self.primary_alignment_filters().alignment_density).abs() > 1.0e-4 {
                self.primary_alignment_filters_mut().alignment_density = density;
                self.rebuild_genomic_display();
            }
            let maximum = (self.panels.active().content_bottom_points
                - self.canvas_geometry.viewport.bottom())
            .max(0.0);
            *requested_scroll = scroll.clamp(0.0, maximum);
            self.display_dirty = true;
        }
        if interaction.drag_started
            && self.vertical_overview_drag.is_none()
            && let Some(pointer) = interaction.pointer
        {
            let press = pointer - interaction.drag_delta;
            self.vertical_overview_drag = Some(VerticalOverviewDrag {
                last_pointer_x: press.x,
                last_pointer_y: press.y,
                anchor_y: (press.y - self.canvas_geometry.viewport.top())
                    .clamp(0.0, self.canvas_geometry.viewport.height()),
            });
        }
        let was_captured = self.overview_capture == Some(OverviewCapture::Vertical);
        if interaction.dragging
            && !was_captured
            && let Some(mut drag) = self.vertical_overview_drag
            && let Some(pointer) = interaction.pointer
        {
            let delta_x = pointer.x - drag.last_pointer_x;
            let delta_y = pointer.y - drag.last_pointer_y;
            let (density, scroll) = vertical_overview_step(
                self.primary_alignment_filters().alignment_density,
                *requested_scroll,
                drag.anchor_y,
                delta_x,
                delta_y,
            );
            if (density - self.primary_alignment_filters().alignment_density).abs() > 1.0e-4 {
                self.primary_alignment_filters_mut().alignment_density = density;
                self.rebuild_genomic_display();
            }
            let maximum = (self.panels.active().content_bottom_points
                - self.canvas_geometry.viewport.bottom())
            .max(0.0);
            *requested_scroll = scroll.clamp(0.0, maximum);
            self.display_dirty = true;
            drag.last_pointer_x = pointer.x;
            drag.last_pointer_y = pointer.y;
            self.vertical_overview_drag = Some(drag);
        }
        if interaction.drag_started
            && self.vertical_overview_drag.is_some()
            && self.overview_capture.is_none()
        {
            self.begin_overview_capture(OverviewCapture::Vertical);
        }
        if interaction.drag_stopped {
            self.vertical_overview_drag = None;
        }
    }

    pub(crate) fn begin_overview_capture(&mut self, capture: OverviewCapture) {
        if self.overview_capture.is_some() {
            return;
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
            }
            Err(error) => {
                tracing::warn!(%error, "pointer capture is unavailable; using bounded overview drag");
            }
        }
    }

    pub(crate) fn capture_mouse_motion(&mut self, delta: (f64, f64)) {
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
            OverviewCapture::Vertical => {
                let Some(drag) = self.vertical_overview_drag else {
                    return;
                };
                let (density, scroll) = vertical_overview_step(
                    self.primary_alignment_filters().alignment_density,
                    self.panels.active().vertical_scroll_points,
                    drag.anchor_y,
                    delta_x as f32,
                    delta_y as f32,
                );
                let density_changed =
                    (density - self.primary_alignment_filters().alignment_density).abs() > 1.0e-4;
                if density_changed {
                    self.primary_alignment_filters_mut().alignment_density = density;
                    self.rebuild_genomic_display();
                }
                let maximum = (self.panels.active().content_bottom_points
                    - self.canvas_geometry.viewport.bottom())
                .max(0.0);
                let scroll = scroll.clamp(0.0, maximum);
                let scroll_changed =
                    (scroll - self.panels.active().vertical_scroll_points).abs() > f32::EPSILON;
                if scroll_changed {
                    self.panels.active_mut().vertical_scroll_points = scroll;
                }
                if density_changed || scroll_changed {
                    if !self.synthetic {
                        self.rebuild_genomic_display();
                    }
                    self.display_dirty = true;
                }
            }
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
                    self.viewport_changed();
                }
            }
            OverviewCapture::Vertical => self.vertical_overview_drag = None,
        }
        self.window.request_redraw();
    }

    pub(crate) fn query_options_changed(&mut self) {
        self.remember_current_locus();
        self.display_dirty = true;
        self.layout_cache.clear();
        if !self.synthetic && (self.has_alignment_engine() || self.track_engine.is_some()) {
            for panel in &mut self.panels.panels {
                panel.generation_clock.advance();
                panel.selected_record = None;
                panel.selected_alignment_track = None;
                panel.selected_read_event = None;
                panel.selected_junction = None;
            }
            for track in &mut self.alignment_tracks {
                for projection in track.projections.values_mut() {
                    projection.clock.advance();
                    projection.current_query = None;
                    projection.alignment_rows = None;
                    projection.cached_layout = None;
                }
            }
            self.request_all_panel_queries();
        }
    }

    pub(crate) fn canvas_page_height(&self) -> f32 {
        self.canvas_geometry.viewport.height().max(80.0) * 0.8
    }

    pub(crate) fn cursor_in_genomic_canvas(&self) -> bool {
        let scale = self.window.scale_factor();
        self.canvas_geometry.genomic.contains(egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        ))
    }

    pub(crate) fn panel_column_rect_at(&self, index: usize) -> egui::Rect {
        let canvas = self.canvas_geometry.genomic;
        let count = self.panels.len().max(1) as f32;
        let width = canvas.width() / count;
        let left = canvas.left() + index as f32 * width;
        let right = if index + 1 == self.panels.len() {
            canvas.right()
        } else {
            left + width
        };
        egui::Rect::from_min_max(
            egui::pos2(left, canvas.top()),
            egui::pos2(right, canvas.bottom()),
        )
    }

    pub(crate) fn panel_column_rect(&self, id: PanelId) -> Option<egui::Rect> {
        self.panels
            .iter()
            .position(|panel| panel.id == id)
            .map(|index| self.panel_column_rect_at(index))
    }

    pub(crate) fn active_panel_rect(&self) -> egui::Rect {
        self.panel_column_rect(self.panels.active().id)
            .unwrap_or(self.canvas_geometry.genomic)
    }

    pub(crate) fn panel_at_cursor(&self) -> Option<PanelId> {
        let scale = self.window.scale_factor();
        let point = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        self.panels.iter().enumerate().find_map(|(index, panel)| {
            self.panel_column_rect_at(index)
                .contains(point)
                .then_some(panel.id)
        })
    }

    pub(crate) fn synchronize_panel_widths(&mut self) {
        let width = f64::from(self.panel_column_rect_at(0).width().max(1.0));
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in &ids {
            if let Some(panel) = self.panels.get_mut(*id)
                && let Ok(viewport) = panel.viewport.resized(width)
            {
                panel.viewport = viewport;
                panel.generation_clock.advance();
                panel.pending_queries.tracks = self.track_engine.is_some();
            }
        }
        for track in &mut self.alignment_tracks {
            for id in &ids {
                let visible = track.config.visible;
                let projection = track.projection_mut(*id);
                projection.clock.advance();
                projection.pending_query = visible;
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
        }
        self.layout_cache.clear();
        self.display_dirty = true;
    }

    pub(crate) fn activate_view_panel(&mut self, id: PanelId) -> bool {
        let changed = id != self.panels.active().id;
        if !self.panels.activate(id) {
            return false;
        }
        self.refresh_active_panel_context();
        self.display_dirty = true;
        if changed && !self.synthetic {
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
            self.canvas_geometry.genomic.width() / (self.panels.len() + 1) as f32 >= 140.0,
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
            (self.canvas_geometry.genomic.width() / (self.panels.len() + 1) as f32).max(1.0),
        );
        let viewport = Viewport::new(interval, width)?;
        let mut panel = ViewPanelController::new(
            id,
            viewport,
            Some(locus.to_owned()),
            contig_name,
            contig_length,
        );
        panel.content_bottom_points = self.canvas_geometry.viewport.top();
        anyhow::ensure!(self.panels.push(panel), "view panel identity collision");
        for track in &mut self.alignment_tracks {
            track.projection_mut(id).pending_query = track.config.visible;
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
            self.display_dirty = true;
            self.rebuild_all_panel_displays();
        }
        moved
    }

    pub(crate) fn cursor_in_track_viewport(&self) -> bool {
        let scale = self.window.scale_factor();
        self.canvas_geometry.viewport.contains(egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        ))
    }

    pub(crate) fn inspector_activation_at_cursor(&self) -> InspectorActivation {
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let content_y = cursor_y + self.panels.active().vertical_scroll_points;
        alignment_track_at_content_y(&self.panels.active().track_frames, content_y).map_or(
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
