//! Window-shell policies that must stay testable without a window: Escape handling, the
//! unsaved-changes gate, wheel/trackpad interpretation, the newer-writer guard and dropped
//! rectangle reporting. The notification log, gesture commits, track-id allocation, autosave
//! bookkeeping and undo history are session state (`rgv_session`), re-exported here.
//!
//! `ShellState` is a field of `Viewer` (`Viewer::shell`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) use rgv_session::document_state::autosave_needed;
pub(crate) use rgv_session::gesture::*;
pub(crate) use rgv_session::notifications::*;
pub(crate) use rgv_session::track_ids::*;
use winit::event::MouseScrollDelta;

use super::tasks::SessionLoadKind;

// ---------------------------------------------------------------------------------------------
// Escape (UI-1)
// ---------------------------------------------------------------------------------------------

/// Everything Escape may dismiss, in the order it is checked.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct EscapeContext {
    pub(crate) repeat: bool,
    /// egui wants the key, typically because a text field has keyboard focus.
    pub(crate) egui_consumed: bool,
    pub(crate) context_menu_open: bool,
    pub(crate) unsaved_prompt_open: bool,
    pub(crate) exit_prompt_open: bool,
    pub(crate) search_popup_open: bool,
    pub(crate) command_palette_open: bool,
    pub(crate) notifications_open: bool,
    pub(crate) other_panel_open: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EscapeAction {
    /// Nothing transient is open, or egui owns the key. Escape never exits the application.
    Nothing,
    CloseContextMenu,
    CancelUnsavedPrompt,
    CloseExitPrompt,
    CloseSearchPopup,
    CloseNotifications,
    ClosePanel,
}

/// Escape closes the topmost transient surface and otherwise does nothing.
///
/// Menus, prompts, the search popup and the command palette are dismissed even while a text field
/// has focus, because the focused field belongs to them (or they are modal). An ordinary dialog
/// yields to egui first so Escape can leave its text field without closing the dialog.
pub(crate) const fn escape_action(context: EscapeContext) -> EscapeAction {
    if context.repeat {
        EscapeAction::Nothing
    } else if context.context_menu_open {
        EscapeAction::CloseContextMenu
    } else if context.unsaved_prompt_open {
        EscapeAction::CancelUnsavedPrompt
    } else if context.exit_prompt_open {
        EscapeAction::CloseExitPrompt
    } else if context.search_popup_open {
        EscapeAction::CloseSearchPopup
    } else if context.command_palette_open {
        EscapeAction::ClosePanel
    } else if context.egui_consumed {
        EscapeAction::Nothing
    } else if context.notifications_open {
        EscapeAction::CloseNotifications
    } else if context.other_panel_open {
        EscapeAction::ClosePanel
    } else {
        EscapeAction::Nothing
    }
}

// ---------------------------------------------------------------------------------------------
// Unsaved-changes gate (APP-1)
// ---------------------------------------------------------------------------------------------

/// An action that discards the current document and therefore passes through the gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GuardedAction {
    Exit,
    LoadSession {
        path: PathBuf,
        kind: SessionLoadKind,
    },
}

impl GuardedAction {
    pub(crate) fn prompt(&self) -> String {
        match self {
            Self::Exit => "Save changes to this project before exiting RGV?".to_owned(),
            Self::LoadSession { path, kind } => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let verb = match kind {
                    SessionLoadKind::Native => "opening",
                    SessionLoadKind::IgvImport => "importing",
                    SessionLoadKind::Recovery => "recovering",
                };
                format!("Save changes to this project before {verb} {name}?")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GateOutcome {
    /// Perform the action now.
    Proceed(GuardedAction),
    /// The Save / Discard / Cancel prompt is open; nothing else happens yet.
    Prompt,
    /// Save the document first; the action resumes from [`UnsavedChangesGate::save_finished`].
    SaveFirst,
    /// The user cancelled, or there was nothing to resolve.
    Stay,
}

/// The single policy for every document-replacing or exiting GUI action.
#[derive(Debug, Default)]
pub(crate) struct UnsavedChangesGate {
    prompt: Option<GuardedAction>,
    after_save: Option<GuardedAction>,
}

impl UnsavedChangesGate {
    pub(crate) fn request(&mut self, action: GuardedAction, dirty: bool) -> GateOutcome {
        if dirty {
            // A newer request replaces the one being asked about; a save already running for an
            // older request must not resume it afterwards.
            self.after_save = None;
            self.prompt = Some(action);
            GateOutcome::Prompt
        } else {
            self.prompt = None;
            self.after_save = None;
            GateOutcome::Proceed(action)
        }
    }

    pub(crate) fn prompt(&self) -> Option<&GuardedAction> {
        self.prompt.as_ref()
    }

    pub(crate) fn resolve(&mut self, choice: UnsavedChoice) -> GateOutcome {
        let Some(action) = self.prompt.take() else {
            return GateOutcome::Stay;
        };
        match choice {
            UnsavedChoice::Cancel => GateOutcome::Stay,
            UnsavedChoice::Discard => GateOutcome::Proceed(action),
            UnsavedChoice::Save => {
                self.after_save = Some(action);
                GateOutcome::SaveFirst
            }
        }
    }

    /// The save requested by `SaveFirst` could not start (no destination chosen, or an error).
    pub(crate) fn save_abandoned(&mut self) {
        self.after_save = None;
    }

    pub(crate) fn awaiting_save(&self) -> bool {
        self.after_save.is_some()
    }

    /// A manual save finished. The pending action resumes only when the save succeeded and the
    /// document is clean; edits made while saving ask again rather than being discarded.
    pub(crate) fn save_finished(&mut self, succeeded: bool, still_dirty: bool) -> GateOutcome {
        let Some(action) = self.after_save.take() else {
            return GateOutcome::Stay;
        };
        if !succeeded {
            GateOutcome::Stay
        } else if still_dirty {
            self.prompt = Some(action);
            GateOutcome::Prompt
        } else {
            GateOutcome::Proceed(action)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Wheel and trackpad interpretation (UI-2)
// ---------------------------------------------------------------------------------------------

/// Logical points one wheel "line" scrolls.
pub(crate) const WHEEL_LINE_POINTS: f64 = 40.0;
/// Zoom steps per logical point of precise (trackpad) scrolling. One step is one wheel line.
pub(crate) const PIXEL_ZOOM_STEPS_PER_POINT: f64 = 0.04;
const ZOOM_STEP_FACTOR: f64 = 0.85;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollIntent {
    /// Multiply the visible span by this factor around the cursor.
    Zoom {
        factor: f64,
    },
    /// Pan the viewport by this many logical points (positive moves towards higher coordinates).
    Pan {
        points: f64,
    },
    /// Scroll the track stack under the cursor by this many logical points.
    ScrollTracks {
        points: f32,
    },
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScrollModifiers {
    pub(crate) shift: bool,
    /// Ctrl or Cmd.
    pub(crate) primary: bool,
}

/// Scroll delta in logical points. `PixelDelta` is physical and is divided by the scale factor.
pub(crate) fn scroll_delta_points(delta: MouseScrollDelta, scale_factor: f64) -> (f64, f64) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (
            f64::from(x) * WHEEL_LINE_POINTS,
            f64::from(y) * WHEEL_LINE_POINTS,
        ),
        MouseScrollDelta::PixelDelta(position) => {
            let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
                scale_factor
            } else {
                1.0
            };
            (position.x / scale, position.y / scale)
        }
    }
}

fn zoom_steps(delta: MouseScrollDelta, points: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(..) => points / WHEEL_LINE_POINTS,
        MouseScrollDelta::PixelDelta(_) => points * PIXEL_ZOOM_STEPS_PER_POINT,
    }
}

/// Decides what one wheel or two-finger scroll event does.
///
/// * Ctrl/Cmd + scroll always zooms at the cursor.
/// * Shift + scroll, or any scroll outside the genomic canvas, scrolls the track stack.
/// * Over the canvas a predominantly horizontal delta pans; otherwise vertical scroll zooms.
pub(crate) fn interpret_scroll(
    delta: MouseScrollDelta,
    scale_factor: f64,
    modifiers: ScrollModifiers,
    over_genomic_canvas: bool,
) -> ScrollIntent {
    let (x, y) = scroll_delta_points(delta, scale_factor);
    if !x.is_finite() || !y.is_finite() || (x == 0.0 && y == 0.0) {
        return ScrollIntent::Ignore;
    }
    let primary_axis = if y.abs() > f64::EPSILON { y } else { x };
    if modifiers.primary {
        return ScrollIntent::Zoom {
            factor: ZOOM_STEP_FACTOR.powf(zoom_steps(delta, primary_axis)),
        };
    }
    if modifiers.shift || !over_genomic_canvas {
        return ScrollIntent::ScrollTracks {
            points: primary_axis as f32,
        };
    }
    if x.abs() > y.abs() {
        // Content follows the fingers: a rightward swipe reveals lower coordinates.
        ScrollIntent::Pan { points: -x }
    } else {
        ScrollIntent::Zoom {
            factor: ZOOM_STEP_FACTOR.powf(zoom_steps(delta, y)),
        }
    }
}

/// Span factor for a magnification gesture step; a positive delta zooms in.
pub(crate) fn pinch_zoom_factor(delta: f64) -> Option<f64> {
    (delta.is_finite() && delta != 0.0).then(|| (-delta).exp().clamp(0.2, 5.0))
}

// ---------------------------------------------------------------------------------------------
// Event-loop wake policy (APP-12)
// ---------------------------------------------------------------------------------------------

/// Poll for work that is observed only by asking whether it has finished (file workers, an
/// automation batch in progress).
pub(crate) const POLLED_WORK_INTERVAL: Duration = Duration::from_millis(16);
/// Poll while navigation or data is still settling: debounced queries and retries are
/// time-driven and have no wake-up of their own.
pub(crate) const UNSETTLED_ENGINE_INTERVAL: Duration = Duration::from_millis(50);
/// Coarse backstop while everything is settled. Engines and the control service wake the loop
/// themselves when they have something to deliver; this only bounds the cost of a missed wake.
pub(crate) const IDLE_SAFETY_TICK: Duration = Duration::from_secs(1);

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WakeState {
    /// Workers that are only observed by polling are in flight.
    pub(crate) polled_work_in_flight: bool,
    /// At least one data engine is open.
    pub(crate) engines_open: bool,
    /// Every panel's navigation and data have settled.
    pub(crate) workspace_settled: bool,
    /// The local control service is listening.
    pub(crate) control_service_open: bool,
}

/// When the event loop should next wake without an external event, or `None` to sleep until
/// one arrives. `deadlines` are exact scheduled wake-ups (repaints, autosave, debounces).
pub(crate) fn next_wake(
    now: Instant,
    deadlines: &[Option<Instant>],
    state: WakeState,
) -> Option<Instant> {
    let interval = if state.polled_work_in_flight {
        Some(POLLED_WORK_INTERVAL)
    } else if state.engines_open && !state.workspace_settled {
        Some(UNSETTLED_ENGINE_INTERVAL)
    } else if state.engines_open || state.control_service_open {
        Some(IDLE_SAFETY_TICK)
    } else {
        None
    };
    deadlines
        .iter()
        .copied()
        .flatten()
        .chain(interval.map(|interval| now + interval))
        .min()
}

/// [`crate::Viewer::report_error`] for call sites that hold a borrow of another `Viewer` field
/// and so cannot call a `&mut self` method: logs the failure and shows it on the status line.
pub(crate) fn report_error_to(
    notifications: &mut NotificationLog,
    data_status: &mut String,
    message: impl Into<String>,
) {
    let message = message.into();
    notifications.push_reported(NotificationSeverity::Error, None, &message);
    *data_status = message;
}

// ---------------------------------------------------------------------------------------------
// Incomplete-frame reporting (REND-14)
// ---------------------------------------------------------------------------------------------

/// Turns the renderer's per-frame dropped-rectangle count into one notification per episode:
/// reported when frames start being incomplete, silent while they stay so, re-armed once a
/// complete frame is drawn.
#[derive(Debug, Default)]
pub(crate) struct DroppedRectangleReporter {
    dropping: bool,
}

impl DroppedRectangleReporter {
    /// `dropped` rectangles of the frame just prepared could not be drawn; the renderer draws at
    /// most `maximum` per frame. Returns the message to log on the transition into dropping.
    pub(crate) fn observe(&mut self, dropped: usize, maximum: usize) -> Option<String> {
        let was_dropping = std::mem::replace(&mut self.dropping, dropped > 0);
        (dropped > 0 && !was_dropping).then(|| {
            format!(
                "The view is incomplete: {dropped} annotation and overlay marks exceed the \
                 {maximum} the renderer draws per frame and were left out. Zoom in, or hide or \
                 collapse dense tracks, to see everything."
            )
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Newer-writer overwrite guard (CORE-9)
// ---------------------------------------------------------------------------------------------

/// What a save must do about a project that a newer RGV wrote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NewerWriterSave {
    /// Nothing to warn about: save.
    Proceed,
    /// The save would discard the newer RGV's settings and the user has not yet been told.
    /// Show `warning` first; asking again proceeds.
    ConfirmFirst { warning: String },
}

/// Remembers that the open project was read from a file written by a newer RGV, so the first
/// save back over that file is held until the data-loss warning has been shown.
#[derive(Debug, Default)]
pub(crate) struct NewerWriterGuard {
    armed: Option<(PathBuf, String)>,
}

impl NewerWriterGuard {
    /// Call when a project replaces the workspace. `origin` is the file it was read from when a
    /// plain save would go back to it; `notice` is the document's newer-writer notice.
    pub(crate) fn project_opened(&mut self, origin: Option<&Path>, notice: Option<&str>) {
        self.armed = origin
            .zip(notice)
            .map(|(origin, notice)| (origin.to_owned(), notice.to_owned()));
    }

    /// Decides a save to `destination`. `same_file` compares two destinations. A warning is
    /// issued once: the repeated request is the user's confirmation.
    pub(crate) fn before_save(
        &mut self,
        destination: &Path,
        same_file: impl Fn(&Path, &Path) -> bool,
    ) -> NewerWriterSave {
        match self.armed.take() {
            Some((origin, notice)) if same_file(&origin, destination) => {
                NewerWriterSave::ConfirmFirst {
                    warning: format!(
                        "{notice} Save again to overwrite {}, or use Save project as… to keep \
                         the original.",
                        destination.display()
                    ),
                }
            }
            // Saving elsewhere leaves the original intact and the guard armed.
            armed => {
                self.armed = armed;
                NewerWriterSave::Proceed
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Aggregate
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct ShellState {
    pub(crate) unsaved_gate: UnsavedChangesGate,
    pub(crate) notifications_open: bool,
    pub(crate) newer_writer_guard: NewerWriterGuard,
    pub(crate) dropped_rectangles: DroppedRectangleReporter,
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn dropped_rectangles_are_reported_once_per_episode() {
        let mut reporter = DroppedRectangleReporter::default();
        assert_eq!(reporter.observe(0, 1_000), None);
        let message = reporter
            .observe(250, 1_000)
            .expect("entering the dropping state reports");
        assert!(
            message.contains("250") && message.contains("1000"),
            "{message}"
        );
        // Every following incomplete frame is silent, whatever its count.
        assert_eq!(reporter.observe(250, 1_000), None);
        assert_eq!(reporter.observe(9_000, 1_000), None);
        // A complete frame re-arms the report.
        assert_eq!(reporter.observe(0, 1_000), None);
        assert!(reporter.observe(1, 1_000).is_some());
    }

    #[test]
    fn the_newer_writer_warning_is_shown_before_the_original_is_overwritten() {
        let same = |left: &Path, right: &Path| left == right;
        let origin = Path::new("/projects/study.rgv.json");
        let notice = "This project was written by a newer RGV (rgv 9.9.9). Settings … lost.";
        let mut guard = NewerWriterGuard::default();
        assert_eq!(guard.before_save(origin, same), NewerWriterSave::Proceed);

        guard.project_opened(Some(origin), Some(notice));
        // Saving a copy elsewhere is harmless and does not use up the warning.
        assert_eq!(
            guard.before_save(Path::new("/projects/copy.rgv.json"), same),
            NewerWriterSave::Proceed
        );
        let NewerWriterSave::ConfirmFirst { warning } = guard.before_save(origin, same) else {
            panic!("the first overwrite must be held");
        };
        assert!(warning.starts_with(notice), "{warning}");
        assert!(warning.contains("Save again to overwrite /projects/study.rgv.json"));
        // The repeated request is the confirmation.
        assert_eq!(guard.before_save(origin, same), NewerWriterSave::Proceed);

        // A project from this version, or one with no file to overwrite, never warns.
        guard.project_opened(Some(origin), None);
        assert_eq!(guard.before_save(origin, same), NewerWriterSave::Proceed);
        guard.project_opened(None, Some(notice));
        assert_eq!(guard.before_save(origin, same), NewerWriterSave::Proceed);
        // Opening another project replaces an unconfirmed warning.
        guard.project_opened(Some(origin), Some(notice));
        guard.project_opened(Some(Path::new("/other.rgv.json")), None);
        assert_eq!(guard.before_save(origin, same), NewerWriterSave::Proceed);
    }

    #[test]
    fn escape_never_exits_and_closes_only_the_topmost_transient_surface() {
        assert_eq!(
            escape_action(EscapeContext::default()),
            EscapeAction::Nothing,
            "with nothing open Escape does nothing"
        );
        let everything = EscapeContext {
            context_menu_open: true,
            unsaved_prompt_open: true,
            exit_prompt_open: true,
            search_popup_open: true,
            command_palette_open: true,
            notifications_open: true,
            other_panel_open: true,
            ..EscapeContext::default()
        };
        assert_eq!(escape_action(everything), EscapeAction::CloseContextMenu);
        let mut context = EscapeContext {
            context_menu_open: false,
            ..everything
        };
        assert_eq!(escape_action(context), EscapeAction::CancelUnsavedPrompt);
        context.unsaved_prompt_open = false;
        assert_eq!(escape_action(context), EscapeAction::CloseExitPrompt);
        context.exit_prompt_open = false;
        assert_eq!(escape_action(context), EscapeAction::CloseSearchPopup);
        context.search_popup_open = false;
        assert_eq!(escape_action(context), EscapeAction::ClosePanel);
        context.command_palette_open = false;
        assert_eq!(escape_action(context), EscapeAction::CloseNotifications);
        context.notifications_open = false;
        assert_eq!(escape_action(context), EscapeAction::ClosePanel);
        context.other_panel_open = false;
        assert_eq!(escape_action(context), EscapeAction::Nothing);
    }

    #[test]
    fn escape_respects_egui_focus_and_ignores_key_repeat() {
        let dialog_with_focused_field = EscapeContext {
            egui_consumed: true,
            other_panel_open: true,
            ..EscapeContext::default()
        };
        assert_eq!(
            escape_action(dialog_with_focused_field),
            EscapeAction::Nothing
        );
        let palette = EscapeContext {
            egui_consumed: true,
            command_palette_open: true,
            ..EscapeContext::default()
        };
        assert_eq!(escape_action(palette), EscapeAction::ClosePanel);
        let held = EscapeContext {
            repeat: true,
            other_panel_open: true,
            exit_prompt_open: true,
            ..EscapeContext::default()
        };
        assert_eq!(escape_action(held), EscapeAction::Nothing);
    }

    fn load() -> GuardedAction {
        GuardedAction::LoadSession {
            path: PathBuf::from("/tmp/other.json"),
            kind: SessionLoadKind::Native,
        }
    }

    #[test]
    fn clean_documents_pass_the_gate_without_a_prompt() {
        let mut gate = UnsavedChangesGate::default();
        assert_eq!(
            gate.request(GuardedAction::Exit, false),
            GateOutcome::Proceed(GuardedAction::Exit)
        );
        assert!(gate.prompt().is_none());
    }

    #[test]
    fn dirty_documents_prompt_and_each_choice_has_one_outcome() {
        let mut gate = UnsavedChangesGate::default();
        assert_eq!(gate.request(load(), true), GateOutcome::Prompt);
        assert_eq!(gate.prompt(), Some(&load()));
        assert_eq!(gate.resolve(UnsavedChoice::Cancel), GateOutcome::Stay);
        assert!(gate.prompt().is_none());
        assert_eq!(
            gate.resolve(UnsavedChoice::Discard),
            GateOutcome::Stay,
            "a choice without a prompt does nothing"
        );

        assert_eq!(gate.request(load(), true), GateOutcome::Prompt);
        assert_eq!(
            gate.resolve(UnsavedChoice::Discard),
            GateOutcome::Proceed(load())
        );

        assert_eq!(gate.request(GuardedAction::Exit, true), GateOutcome::Prompt);
        assert_eq!(gate.resolve(UnsavedChoice::Save), GateOutcome::SaveFirst);
        assert!(gate.awaiting_save());
        assert_eq!(
            gate.save_finished(true, false),
            GateOutcome::Proceed(GuardedAction::Exit)
        );
        assert!(!gate.awaiting_save());
    }

    #[test]
    fn failed_or_abandoned_saves_never_run_the_guarded_action() {
        let mut gate = UnsavedChangesGate::default();
        gate.request(GuardedAction::Exit, true);
        gate.resolve(UnsavedChoice::Save);
        assert_eq!(gate.save_finished(false, true), GateOutcome::Stay);
        assert_eq!(gate.save_finished(true, false), GateOutcome::Stay);

        gate.request(GuardedAction::Exit, true);
        gate.resolve(UnsavedChoice::Save);
        gate.save_abandoned();
        assert_eq!(gate.save_finished(true, false), GateOutcome::Stay);

        // Edits made while the save was running are asked about again.
        gate.request(load(), true);
        gate.resolve(UnsavedChoice::Save);
        assert_eq!(gate.save_finished(true, true), GateOutcome::Prompt);
        assert_eq!(gate.prompt(), Some(&load()));

        // An unrelated manual save does not trigger anything.
        let mut idle = UnsavedChangesGate::default();
        assert_eq!(idle.save_finished(true, false), GateOutcome::Stay);
    }

    #[test]
    fn a_newer_request_supersedes_an_action_waiting_for_its_save() {
        let mut gate = UnsavedChangesGate::default();
        gate.request(GuardedAction::Exit, true);
        gate.resolve(UnsavedChoice::Save);
        assert_eq!(gate.request(load(), true), GateOutcome::Prompt);
        assert_eq!(
            gate.save_finished(true, false),
            GateOutcome::Stay,
            "the superseded exit must not run when its save lands"
        );
        assert_eq!(gate.prompt(), Some(&load()));
    }

    const NONE: ScrollModifiers = ScrollModifiers {
        shift: false,
        primary: false,
    };

    fn pixels(x: f64, y: f64) -> MouseScrollDelta {
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(x, y))
    }

    #[test]
    fn pixel_deltas_are_converted_from_physical_to_logical_points() {
        assert_eq!(scroll_delta_points(pixels(30.0, -60.0), 2.0), (15.0, -30.0));
        assert_eq!(scroll_delta_points(pixels(30.0, -60.0), 1.0), (30.0, -60.0));
        assert_eq!(
            scroll_delta_points(pixels(30.0, -60.0), 0.0),
            (30.0, -60.0),
            "a degenerate scale factor is treated as 1"
        );
        assert_eq!(
            scroll_delta_points(MouseScrollDelta::LineDelta(1.0, -2.0), 2.0),
            (40.0, -80.0),
            "line deltas are already device independent"
        );
        // The same physical swipe pans the same logical distance on any display.
        let ScrollIntent::Pan { points: retina } =
            interpret_scroll(pixels(-80.0, 4.0), 2.0, NONE, true)
        else {
            panic!("horizontal swipe pans")
        };
        assert!((retina - 40.0).abs() < 1e-9);
    }

    #[test]
    fn vertical_scroll_zooms_and_a_dominant_horizontal_delta_pans() {
        let ScrollIntent::Zoom { factor } =
            interpret_scroll(MouseScrollDelta::LineDelta(0.0, 1.0), 1.0, NONE, true)
        else {
            panic!("a wheel tick over the canvas zooms")
        };
        assert!((factor - 0.85).abs() < 1e-9, "one line is one zoom step");
        let ScrollIntent::Zoom { factor } =
            interpret_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 1.0, NONE, true)
        else {
            panic!("a wheel tick over the canvas zooms")
        };
        assert!(factor > 1.0);
        assert!(matches!(
            interpret_scroll(pixels(3.0, 20.0), 1.0, NONE, true),
            ScrollIntent::Zoom { .. }
        ));
        assert_eq!(
            interpret_scroll(pixels(20.0, 3.0), 1.0, NONE, true),
            ScrollIntent::Pan { points: -20.0 }
        );
        assert_eq!(
            interpret_scroll(pixels(0.0, 0.0), 1.0, NONE, true),
            ScrollIntent::Ignore
        );
    }

    #[test]
    fn modifiers_and_pointer_location_select_zoom_or_track_scrolling() {
        let primary = ScrollModifiers {
            shift: false,
            primary: true,
        };
        let shift = ScrollModifiers {
            shift: true,
            primary: false,
        };
        // Ctrl/Cmd zooms even for a horizontal delta and outside the canvas.
        assert!(matches!(
            interpret_scroll(pixels(20.0, 0.0), 1.0, primary, false),
            ScrollIntent::Zoom { .. }
        ));
        assert!(matches!(
            interpret_scroll(pixels(20.0, 3.0), 1.0, primary, true),
            ScrollIntent::Zoom { .. }
        ));
        assert_eq!(
            interpret_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 1.0, shift, true),
            ScrollIntent::ScrollTracks { points: -40.0 }
        );
        assert_eq!(
            interpret_scroll(pixels(0.0, 24.0), 2.0, NONE, false),
            ScrollIntent::ScrollTracks { points: 12.0 }
        );
        // Shift commonly turns a vertical wheel into a horizontal delta.
        assert_eq!(
            interpret_scroll(MouseScrollDelta::LineDelta(1.0, 0.0), 1.0, shift, true),
            ScrollIntent::ScrollTracks { points: 40.0 }
        );
    }

    #[test]
    fn pinch_magnification_zooms_in_and_is_bounded() {
        assert_eq!(pinch_zoom_factor(0.0), None);
        assert_eq!(pinch_zoom_factor(f64::NAN), None);
        assert!(pinch_zoom_factor(0.1).unwrap() < 1.0);
        assert!(pinch_zoom_factor(-0.1).unwrap() > 1.0);
        let round_trip = pinch_zoom_factor(0.25).unwrap() * pinch_zoom_factor(-0.25).unwrap();
        assert!((round_trip - 1.0).abs() < 1e-12);
        assert!((pinch_zoom_factor(100.0).unwrap() - 0.2).abs() < 1e-12);
        assert!((pinch_zoom_factor(-100.0).unwrap() - 5.0).abs() < 1e-12);
    }

    #[test]
    fn an_idle_window_does_not_poll_at_frame_rate() {
        let now = Instant::now();
        assert_eq!(next_wake(now, &[None, None], WakeState::default()), None);
        // Settled engines and an idle control service only get the coarse safety tick.
        let idle = WakeState {
            engines_open: true,
            workspace_settled: true,
            control_service_open: true,
            ..WakeState::default()
        };
        assert_eq!(next_wake(now, &[], idle), Some(now + IDLE_SAFETY_TICK));
        let service_only = WakeState {
            control_service_open: true,
            ..WakeState::default()
        };
        assert_eq!(
            next_wake(now, &[], service_only),
            Some(now + IDLE_SAFETY_TICK)
        );
    }

    #[test]
    fn unsettled_and_polled_work_keep_their_short_intervals() {
        let now = Instant::now();
        let settling = WakeState {
            engines_open: true,
            workspace_settled: false,
            ..WakeState::default()
        };
        assert_eq!(
            next_wake(now, &[], settling),
            Some(now + UNSETTLED_ENGINE_INTERVAL)
        );
        let exporting = WakeState {
            polled_work_in_flight: true,
            engines_open: true,
            workspace_settled: true,
            ..WakeState::default()
        };
        assert_eq!(
            next_wake(now, &[], exporting),
            Some(now + POLLED_WORK_INTERVAL)
        );
    }

    #[test]
    fn exact_deadlines_win_when_they_are_sooner() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(5);
        let later = now + Duration::from_secs(30);
        let idle = WakeState {
            engines_open: true,
            workspace_settled: true,
            ..WakeState::default()
        };
        assert_eq!(next_wake(now, &[Some(later), Some(soon)], idle), Some(soon));
        assert_eq!(
            next_wake(now, &[Some(later)], idle),
            Some(now + IDLE_SAFETY_TICK)
        );
        assert_eq!(
            next_wake(now, &[Some(later), None], WakeState::default()),
            Some(later)
        );
    }
}
