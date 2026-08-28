use super::*;

pub(super) struct Application {
    pub(super) viewer: Option<Viewer>,
    pub(super) options: LaunchOptions,
    pub(super) process_started: Instant,
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

        let egui_response = viewer.egui_state.on_window_event(&viewer.window, &event);
        // egui-winit reports `repaint: true` for `RedrawRequested`. Honouring
        // that response after rendering this same event creates an endless
        // RedrawRequested -> request_redraw feedback loop.
        let request_egui_redraw = should_request_egui_redraw(&event, egui_response.repaint);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && is_exit_shortcut(&event.logical_key, viewer.modifiers) =>
            {
                if confirm_or_exit(&mut viewer.exit_confirmation_open) {
                    event_loop.exit();
                } else {
                    viewer.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                if viewer.exit_confirmation_open {
                    viewer.exit_confirmation_open = false;
                    viewer.window.request_redraw();
                } else if viewer.open_panel.take().is_some() {
                    viewer.command_search.clear();
                    viewer.window.request_redraw();
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !egui_response.consumed =>
            {
                viewer.keyboard_input(&event.logical_key);
            }
            WindowEvent::ModifiersChanged(modifiers) => viewer.modifiers = modifiers.state(),
            WindowEvent::Resized(size) => viewer.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                viewer.resize(viewer.window.inner_size());
            }
            WindowEvent::CursorMoved { position, .. } => {
                viewer.update_vertical_scrollbar_drag(position);
                viewer.cursor_moved(position, egui_response.consumed);
            }
            WindowEvent::CursorLeft { .. } => {
                if viewer.panels.active_mut().hovered_record.take().is_some() {
                    viewer.display_dirty = true;
                    viewer.window.request_redraw();
                }
                viewer.panels.active_mut().hover_inspector_target = None;
            }
            WindowEvent::Focused(false) => {
                viewer.finish_overview_capture();
                viewer.finish_vertical_scrollbar_drag();
                let was_canvas_dragging = viewer.dragging;
                viewer.dragging = false;
                viewer.last_cursor = None;
                viewer.press_cursor = None;
                if let Some(origin) = viewer.drag_origin_interval.take() {
                    let interval = viewer.panels.active().viewport.interval();
                    if origin != interval {
                        viewer
                            .panels
                            .active_mut()
                            .navigation_history
                            .visit(interval);
                    }
                }
                if was_canvas_dragging {
                    viewer.finish_navigation_gesture();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    viewer.begin_vertical_scrollbar_drag();
                }
                if state == ElementState::Pressed
                    && viewer.cursor_over_unobstructed_genomic_canvas()
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
                    if let Some(origin) = viewer.drag_origin_interval.take() {
                        let interval = viewer.panels.active().viewport.interval();
                        if origin != interval {
                            viewer
                                .panels
                                .active_mut()
                                .navigation_history
                                .visit(interval);
                        }
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
            WindowEvent::MouseWheel { delta, phase, .. }
                if viewer.cursor_over_unobstructed_track_viewport() =>
            {
                // The central panel is painted by egui, which may mark wheel input as
                // consumed even though the genomic canvas itself is GPU rendered. Use
                // the painted layer under the pointer instead: floating windows and
                // popups must own their wheel input and block the viewer underneath.
                if let Some(panel_id) = viewer.panel_at_cursor() {
                    viewer.activate_view_panel(panel_id);
                }
                if viewer.modifiers.shift_key() || !viewer.cursor_in_genomic_canvas() {
                    viewer.scroll_tracks(delta);
                } else {
                    viewer.zoom(delta);
                    if matches!(
                        phase,
                        winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                    ) {
                        viewer.finish_navigation_gesture();
                    }
                }
            }
            WindowEvent::DroppedFile(path) => viewer.handle_dropped_file(&path),
            WindowEvent::RedrawRequested => viewer.render(),
            _ => {}
        }

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
        if let DeviceEvent::MouseMotion { delta } = event {
            viewer.capture_mouse_motion(delta);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(viewer) = &mut self.viewer {
            viewer.poll_background_work();
            let now = Instant::now();
            viewer.maybe_start_autosave(now);
            viewer.advance_automation(now);
            if viewer.automation_exit_requested || viewer.user_exit_requested {
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
            let mut next_wake = viewer.next_egui_repaint;
            if let Some(deadline) = viewer.pending_resolution_query_refresh {
                next_wake = Some(next_wake.map_or(deadline, |wake| wake.min(deadline)));
            }
            if viewer.settings.autosave_enabled {
                next_wake = Some(
                    next_wake.map_or(viewer.next_autosave, |wake| wake.min(viewer.next_autosave)),
                );
            }
            if viewer.has_alignment_engine()
                || viewer.reference_engine.is_some()
                || viewer.track_engine.is_some()
                || !viewer.export_tasks.is_empty()
                || viewer.pending_native_window_capture.is_some()
                || !viewer.session_save_tasks.is_empty()
                || !viewer.session_load_tasks.is_empty()
                || !viewer.workspace_list_import_tasks.is_empty()
                || !viewer.alignment_retirement_tasks.is_empty()
                || viewer
                    .automation
                    .as_ref()
                    .is_some_and(|runner| !runner.finished)
                || viewer.control_service.is_some()
            {
                let worker_poll = now + Duration::from_millis(16);
                next_wake = Some(next_wake.map_or(worker_poll, |wake| wake.min(worker_poll)));
            }
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
            viewer.finish_automation_tasks();
            if let Some(service) = viewer.control_service.as_mut() {
                service.shutdown();
            }
            viewer.persist_settings();
        }
    }
}

const fn should_request_egui_redraw(event: &WindowEvent, repaint: bool) -> bool {
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

fn is_exit_shortcut(key: &Key, modifiers: ModifiersState) -> bool {
    (modifiers.control_key() || modifiers.super_key())
        && matches!(key, Key::Character(character) if character.eq_ignore_ascii_case("w"))
}

fn confirm_or_exit(confirmation_open: &mut bool) -> bool {
    if *confirmation_open {
        true
    } else {
        *confirmation_open = true;
        false
    }
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
    fn exit_shortcut_requires_confirmation_before_exit() {
        let mut confirmation_open = false;

        assert!(!confirm_or_exit(&mut confirmation_open));
        assert!(confirmation_open);
        assert!(confirm_or_exit(&mut confirmation_open));
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
