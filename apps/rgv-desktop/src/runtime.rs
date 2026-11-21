use super::*;

fn native_canvas_press_starts_drag(
    unobstructed_canvas: bool,
    pointer: egui::Pos2,
    pane_divider: Option<egui::Rect>,
) -> bool {
    unobstructed_canvas && pane_divider.is_none_or(|divider| !divider.contains(pointer))
}

pub(super) struct Application {
    pub(super) viewer: Option<Viewer>,
    pub(super) options: LaunchOptions,
    pub(super) process_started: Instant,
    /// Set by a worker that has published a result and asked the event loop to wake; cleared
    /// just before the results are collected.
    pub(super) worker_wake_pending: Arc<std::sync::atomic::AtomicBool>,
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.viewer.is_some() {
            return;
        }
        let attributes = WindowAttributes::default()
            .with_title("RGV — Rust Genome Viewer")
            .with_theme(Some(Theme::Light))
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(640.0, 400.0));
        let result = event_loop
            .create_window(attributes)
            .context("failed to create RGV window")
            .and_then(|window| {
                pollster::block_on(Viewer::new(
                    Arc::new(window),
                    self.options.clone(),
                    self.process_started,
                ))
            });
        match result {
            Ok(viewer) => self.viewer = Some(viewer),
            Err(error) => {
                tracing::error!(?error, "failed to initialize RGV");
                event_loop.exit();
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        if window_id != viewer.window.id() {
            return;
        }

        viewer.stress_health.note_ui_heartbeat();
        observe_stress_input_health(viewer, &event);

        let egui_response = viewer.egui_state.on_window_event(&viewer.window, &event);
        viewer.record_manual_window_event(&event, egui_response.consumed);
        let mut recording_observation =
            viewer.prepare_native_input_observation(&event, egui_response.consumed);
        // egui-winit reports `repaint: true` for `RedrawRequested`. Honouring
        // that response after rendering this same event creates an endless
        // RedrawRequested -> request_redraw feedback loop.
        let request_egui_redraw = should_request_egui_redraw(&event, egui_response.repaint);
        match event {
            WindowEvent::CloseRequested => {
                // Unsaved changes open the Save / Discard / Cancel prompt instead of exiting.
                if viewer.request_exit() {
                    viewer.finish_native_input_observation(recording_observation.take());
                    viewer.abort_interaction_recording_before_exit();
                    viewer.save_recording_manual_diagnostic_trace_before_exit();
                    event_loop.exit();
                    return;
                }
                viewer.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && is_exit_shortcut(&event.logical_key, viewer.modifiers) =>
            {
                Viewer::mark_native_input_handled(&mut recording_observation);
                if viewer.request_exit_from_shortcut() {
                    viewer.finish_native_input_observation(recording_observation.take());
                    viewer.abort_interaction_recording_before_exit();
                    viewer.save_recording_manual_diagnostic_trace_before_exit();
                    event_loop.exit();
                    return;
                }
                viewer.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                // Escape only ever dismisses the topmost transient surface; it never exits.
                Viewer::mark_native_input_handled(&mut recording_observation);
                if viewer.escape_pressed(event.repeat, egui_response.consumed) {
                    viewer.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && (event.logical_key == Key::Named(NamedKey::ContextMenu)
                        || (event.logical_key == Key::Named(NamedKey::F10)
                            && viewer.modifiers.shift_key())) =>
            {
                Viewer::mark_native_input_handled(&mut recording_observation);
                viewer.open_alignment_context_menu_at_cursor("keyboard");
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !egui_response.consumed =>
            {
                Viewer::mark_native_input_handled(&mut recording_observation);
                viewer.keyboard_input(&event.logical_key);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Released => {
                Viewer::mark_native_input_handled(&mut recording_observation);
                // A held navigation key is one gesture: record it when the key comes up.
                viewer.end_navigation_gesture();
            }
            WindowEvent::ModifiersChanged(modifiers) => viewer.modifiers = modifiers.state(),
            WindowEvent::Resized(size) => {
                Viewer::mark_native_input_handled(&mut recording_observation);
                viewer.resize(size);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                viewer.resize(viewer.window.inner_size());
            }
            WindowEvent::CursorMoved { position, .. } => {
                Viewer::mark_native_input_handled(&mut recording_observation);
                viewer.update_vertical_scrollbar_drag(position);
                viewer.cursor_moved(position, egui_response.consumed);
            }
            WindowEvent::CursorLeft { .. } => {
                if viewer
                    .panels
                    .active_mut()
                    .hovered_alignment_hit
                    .take()
                    .is_some()
                {
                    viewer.mark_display_dirty("runtime window invalidation");
                    viewer.window.request_redraw();
                }
                viewer.panels.active_mut().hover_inspector_target = None;
                if viewer
                    .panels
                    .active_mut()
                    .hovered_junction_arc
                    .take()
                    .is_some()
                {
                    viewer.window.set_cursor(CursorIcon::Default);
                    if viewer.synthetic || !viewer.refresh_active_panel_scroll_display() {
                        viewer.mark_display_dirty("runtime window invalidation");
                    }
                    viewer.window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                Viewer::mark_native_input_handled(&mut recording_observation);
                viewer.finish_overview_capture();
                viewer.finish_vertical_scrollbar_drag();
                let was_canvas_dragging = viewer.dragging;
                viewer.dragging = false;
                viewer.last_cursor = None;
                viewer.press_cursor = None;
                let cleared_junction_hover = viewer
                    .panels
                    .active_mut()
                    .hovered_junction_arc
                    .take()
                    .is_some();
                viewer.panels.active_mut().hover_inspector_target = None;
                viewer.window.set_cursor(CursorIcon::Default);
                if cleared_junction_hover {
                    viewer.sashimi_interaction_diagnostics.clears = viewer
                        .sashimi_interaction_diagnostics
                        .clears
                        .saturating_add(1);
                    viewer.sashimi_interaction_diagnostics.hover_redraws = viewer
                        .sashimi_interaction_diagnostics
                        .hover_redraws
                        .saturating_add(1);
                    if viewer.synthetic || !viewer.refresh_active_panel_scroll_display() {
                        viewer.mark_display_dirty("runtime window invalidation");
                    }
                    viewer.window.request_redraw();
                }
                viewer.drag_origin_interval = None;
                viewer.end_navigation_gesture();
                if was_canvas_dragging {
                    viewer.finish_navigation_gesture();
                }
            }
            WindowEvent::Focused(true) => {
                Viewer::mark_native_input_handled(&mut recording_observation);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                Viewer::mark_native_input_handled(&mut recording_observation);
                if state == ElementState::Pressed
                    && viewer.modifiers.control_key()
                    && viewer.open_alignment_context_menu_at_cursor("control_click")
                {
                    viewer.dragging = false;
                    viewer.press_cursor = None;
                    viewer.last_cursor = None;
                    viewer.finish_native_input_observation(recording_observation.take());
                    return;
                }
                if state == ElementState::Pressed {
                    viewer.begin_vertical_scrollbar_drag();
                }
                let scale = viewer.window.scale_factor();
                let pointer = egui::pos2(
                    (viewer.cursor.x / scale) as f32,
                    (viewer.cursor.y / scale) as f32,
                );
                if state == ElementState::Pressed
                    && native_canvas_press_starts_drag(
                        viewer.cursor_over_unobstructed_genomic_canvas(),
                        pointer,
                        viewer.pane_divider_rect,
                    )
                {
                    if let Some(panel_id) = viewer.panel_at_cursor() {
                        viewer.activate_view_panel(panel_id);
                    }
                    viewer.dragging = true;
                    viewer.drag_origin_interval = Some(viewer.panels.active().viewport.interval());
                    viewer.press_cursor = Some(viewer.cursor);
                } else if state == ElementState::Released {
                    viewer.finish_overview_capture();
                    viewer.finish_vertical_scrollbar_drag();
                    let clicked = viewer.press_cursor.is_some_and(|pressed| {
                        let dx = pressed.x - viewer.cursor.x;
                        let dy = pressed.y - viewer.cursor.y;
                        dx.mul_add(dx, dy * dy) <= 16.0
                    });
                    viewer.dragging = false;
                    viewer.last_cursor = None;
                    viewer.press_cursor = None;
                    if viewer.drag_origin_interval.take().is_some() {
                        // One history entry and one recent locus for the whole drag.
                        viewer.end_navigation_gesture();
                        viewer.finish_navigation_gesture();
                    }
                    // `press_cursor` is only recorded for the unobstructed GPU canvas.
                    // The surrounding egui panel reports ordinary canvas presses as
                    // consumed, so the native click must finish independently of that
                    // coarse area-level response.
                    if clicked {
                        viewer.select_at_cursor();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if viewer.open_alignment_context_menu_at_cursor("secondary_click") {
                    Viewer::mark_native_input_handled(&mut recording_observation);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } => {
                Viewer::mark_native_input_handled(&mut recording_observation);
            }
            WindowEvent::MouseWheel { delta, phase, .. }
                if viewer.cursor_over_unobstructed_track_viewport() =>
            {
                Viewer::mark_native_input_handled(&mut recording_observation);
                // The central panel is painted by egui, which may mark wheel input as
                // consumed even though the genomic canvas itself is GPU rendered. Use
                // the painted layer under the pointer instead: floating windows and
                // popups must own their wheel input and block the viewer underneath.
                if let Some(panel_id) = viewer.panel_at_cursor() {
                    viewer.activate_view_panel(panel_id);
                }
                let navigated = viewer.apply_scroll(delta);
                // The event that ends a trackpad scroll usually carries a zero delta, so the
                // gesture is settled for any scroll that ends over the canvas.
                let over_canvas =
                    viewer.cursor_in_genomic_canvas() && !viewer.modifiers.shift_key();
                if (navigated || over_canvas)
                    && matches!(
                        phase,
                        winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                    )
                {
                    viewer.finish_navigation_gesture();
                }
            }
            WindowEvent::PinchGesture { delta, phase, .. }
                if viewer.cursor_over_unobstructed_genomic_canvas() =>
            {
                Viewer::mark_native_input_handled(&mut recording_observation);
                if let Some(panel_id) = viewer.panel_at_cursor() {
                    viewer.activate_view_panel(panel_id);
                }
                viewer.apply_pinch(delta);
                if matches!(
                    phase,
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                ) {
                    viewer.finish_navigation_gesture();
                }
            }
            WindowEvent::DroppedFile(path) => viewer.handle_dropped_file(&path),
            WindowEvent::RedrawRequested => {
                viewer.render();
                viewer.stress_health.note_frame_presented();
            }
            _ => {}
        }

        viewer.finish_native_input_observation(recording_observation);

        if request_egui_redraw {
            viewer.window.request_redraw();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        viewer.stress_health.note_ui_heartbeat();
        viewer.record_manual_device_event(&event);
        if let DeviceEvent::MouseMotion { delta } = event {
            viewer.stress_health.note_native_input();
            viewer.capture_mouse_motion(delta);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Clear before collecting: a result published after this point asks for another wake.
        self.worker_wake_pending
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(viewer) = &mut self.viewer {
            viewer.stress_health.note_ui_heartbeat();
            viewer.poll_background_work();
            let now = Instant::now();
            viewer.maybe_start_autosave(now);
            viewer.advance_automation(now);
            if viewer.automation_exit_requested || viewer.user_exit_requested {
                viewer.abort_interaction_recording_before_exit();
                viewer.save_recording_manual_diagnostic_trace_before_exit();
                event_loop.exit();
                return;
            }
            if viewer
                .next_egui_repaint
                .is_some_and(|deadline| deadline <= now)
            {
                viewer.next_egui_repaint = None;
                viewer.window.request_redraw();
            }
            // A lost GPU device is retried on a timer, not only when input causes a redraw.
            let gpu_recovery_deadline = viewer.gpu_recovery_deadline(now);
            if gpu_recovery_deadline.is_some_and(|deadline| deadline <= now) {
                viewer.window.request_redraw();
            }
            viewer.commit_due_navigation_gesture(now);
            viewer.capture_status_notification();
            let engines_open = viewer.has_alignment_engine()
                || viewer.reference_engine.is_some()
                || viewer.track_engine.is_some();
            let polled_work_in_flight = !viewer.export_tasks.is_empty()
                || viewer.pending_native_window_capture.is_some()
                || !viewer.session_save_tasks.is_empty()
                || !viewer.session_load_tasks.is_empty()
                || !viewer.workspace_list_import_tasks.is_empty()
                || !viewer.genome_tasks.is_empty()
                || !viewer.alignment_retirement_tasks.is_empty()
                || !viewer.engine_retirement_tasks.is_empty()
                || viewer
                    .automation
                    .as_ref()
                    .is_some_and(|runner| !runner.finished);
            let next_wake = crate::app::shell_state::next_wake(
                now,
                &[
                    viewer.next_egui_repaint,
                    viewer.pending_resolution_query_refresh,
                    viewer
                        .settings
                        .autosave_enabled
                        .then_some(viewer.next_autosave),
                    viewer.navigation_gesture_deadline(),
                    // After a due attempt the redraw above moves this into the future.
                    gpu_recovery_deadline.filter(|deadline| *deadline > now),
                ],
                crate::app::shell_state::WakeState {
                    polled_work_in_flight,
                    engines_open,
                    // Only consulted while engines are open and nothing is being polled.
                    workspace_settled: engines_open
                        && !polled_work_in_flight
                        && viewer.automation_is_settled(),
                    control_service_open: viewer.control_service.is_some(),
                },
            );
            event_loop
                .set_control_flow(next_wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(viewer) = &mut self.viewer {
            viewer.finish_overview_capture();
            viewer.finish_session_load_tasks();
            viewer.finish_session_save_tasks();
            viewer.finish_export_tasks();
            viewer.finish_workspace_list_import_tasks();
            viewer.finish_alignment_retirement_tasks();
            viewer.finish_engine_retirement_tasks();
            viewer.finish_automation_tasks();
            if let Some(service) = viewer.control_service.as_mut() {
                service.shutdown();
            }
            viewer.persist_settings();
            viewer.finish_settings_writer();
        }
    }
}

fn observe_stress_input_health(viewer: &Viewer, event: &WindowEvent) {
    let is_native_input = matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::Focused(_)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
    );
    if is_native_input {
        viewer.stress_health.note_native_input();
    }
    match event {
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } => {
            viewer.stress_health.begin_pointer_interaction();
        }
        WindowEvent::MouseInput {
            state: ElementState::Released,
            ..
        } => viewer.stress_health.finish_interaction(),
        WindowEvent::MouseWheel { phase, .. } => match phase {
            winit::event::TouchPhase::Started | winit::event::TouchPhase::Moved => {
                viewer.stress_health.begin_scroll_interaction();
            }
            winit::event::TouchPhase::Ended => viewer.stress_health.finish_interaction(),
            winit::event::TouchPhase::Cancelled => viewer.stress_health.cancel_interaction(),
        },
        WindowEvent::Focused(false) => viewer.stress_health.cancel_interaction(),
        WindowEvent::Focused(true) => viewer.stress_health.clear_cancelled_interaction(),
        _ => {}
    }
}

const fn should_request_egui_redraw(event: &WindowEvent, repaint: bool) -> bool {
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

fn is_exit_shortcut(key: &Key, modifiers: ModifiersState) -> bool {
    (modifiers.control_key() || modifiers.super_key())
        && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("w"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_event_does_not_schedule_itself_again() {
        assert!(!should_request_egui_redraw(
            &WindowEvent::RedrawRequested,
            true,
        ));
    }

    #[test]
    fn egui_repaint_requests_from_input_events_are_preserved() {
        assert!(should_request_egui_redraw(
            &WindowEvent::Focused(true),
            true,
        ));
        assert!(!should_request_egui_redraw(
            &WindowEvent::Focused(true),
            false,
        ));
    }

    #[test]
    fn pane_divider_press_is_owned_by_resize_instead_of_native_canvas_pan() {
        let divider = egui::Rect::from_min_max(egui::pos2(0.0, 244.0), egui::pos2(786.0, 256.0));

        assert!(!native_canvas_press_starts_drag(
            true,
            egui::pos2(400.0, 250.0),
            Some(divider),
        ));
        assert!(native_canvas_press_starts_drag(
            true,
            egui::pos2(400.0, 300.0),
            Some(divider),
        ));
        assert!(native_canvas_press_starts_drag(
            true,
            egui::pos2(400.0, 250.0),
            None,
        ));
        assert!(!native_canvas_press_starts_drag(
            false,
            egui::pos2(400.0, 300.0),
            Some(divider),
        ));
    }

    #[test]
    fn exit_shortcut_accepts_primary_modifiers_only() {
        let w = Key::Character("w".into());

        assert!(is_exit_shortcut(&w, ModifiersState::SUPER));
        assert!(is_exit_shortcut(&w, ModifiersState::CONTROL));
        assert!(!is_exit_shortcut(&w, ModifiersState::empty()));
        assert!(!is_exit_shortcut(
            &Key::Character("x".into()),
            ModifiersState::SUPER,
        ));
    }
}
