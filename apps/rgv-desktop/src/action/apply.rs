//! The one path every state-changing intent takes: `Viewer::apply_action`.
//!
//! Navigation and panel management run the headless reducer and then carry out its effects.
//! Track actions call the workspace's own implementation. Either way the window and the
//! automation protocol reach the same code, and what they did is recorded for undo.

use super::history::UndoEntry;
use super::{Action, ActionOutcome, Effect, ViewportCommit};
use crate::Viewer;
use rgv_session::session::{ActionHost, HistoryStep, PanelCanvas, SessionReduction};

type Session = rgv_session::Session<crate::workspace::panels::ViewPanelController>;
use crate::sources::{source_path, track_spec_kind};
use anyhow::Result;
use rgv_core::{PanelId, TrackConfig, TrackId};
use std::collections::BTreeSet;
use winit::window::CursorIcon;

impl ActionHost for Viewer {
    fn undo_history(&mut self) -> &mut rgv_session::action::history::UndoHistory {
        &mut self.session.undo
    }

    fn perform_action(&mut self, action: Action) -> Result<(ActionOutcome, Option<UndoEntry>)> {
        self.perform_action_now(action)
    }
}

impl Viewer {
    /// Applies one user intent and records how to undo it.
    pub(crate) fn apply_action(&mut self, action: Action) -> Result<ActionOutcome> {
        rgv_session::session::apply_action(self, action)
    }

    /// Whether Undo / Redo have something to act on, and what it is called.
    pub(crate) fn undo_label(&self) -> Option<&'static str> {
        self.session.undo.undo_label()
    }

    pub(crate) fn redo_label(&self) -> Option<&'static str> {
        self.session.undo.redo_label()
    }

    /// Takes back the most recent recorded edit. Returns whether there was one.
    pub(crate) fn undo_action(&mut self) -> bool {
        match rgv_session::session::undo_action(self) {
            HistoryStep::Nothing => false,
            HistoryStep::Failed { label, error } => {
                // The document moved under the entry (a source vanished, the window is too
                // narrow for the panel). Report it and drop the step; never leave it half
                // applied on the stack.
                self.report_error(format!("Could not undo {label}: {error}"));
                true
            }
            HistoryStep::Done { label } => {
                self.session.data_status = format!("Undid {label}");
                self.after_history_step();
                true
            }
        }
    }

    /// Re-applies the most recently undone edit. Returns whether there was one.
    pub(crate) fn redo_action(&mut self) -> bool {
        match rgv_session::session::redo_action(self) {
            HistoryStep::Nothing => false,
            HistoryStep::Failed { label, error } => {
                self.report_error(format!("Could not redo {label}: {error}"));
                true
            }
            HistoryStep::Done { label } => {
                self.session.data_status = format!("Redid {label}");
                self.after_history_step();
                true
            }
        }
    }

    fn after_history_step(&mut self) {
        self.bump_workspace_revision();
        self.automation_alignment_handles.clear();
        self.mark_retained_evidence_stale("an edit was undone or redone");
        self.mark_display_dirty("undo history step");
        self.window.request_redraw();
    }

    /// Applies an action and returns the undo entry it earned, without recording it.
    fn perform_action_now(&mut self, action: Action) -> Result<(ActionOutcome, Option<UndoEntry>)> {
        let mut coalesce = Session::track_intents_coalesce_key(&action);
        let (outcome, inverse) = if Session::reduces(&action) {
            let (outcome, inverse, gesture) = self.perform_session_action(&action)?;
            coalesce = gesture;
            (outcome, inverse)
        } else {
            match &action {
                Action::ApplyTrackIntents { intents } => {
                    let before = self.session.track_presentation.clone();
                    let change_set = self.apply_track_intents_now(intents)?;
                    let changed = self.session.track_presentation != before;
                    (
                        ActionOutcome {
                            changed,
                            track_changes: Some(change_set),
                            ..ActionOutcome::default()
                        },
                        if changed {
                            vec![Action::RestorePresentation {
                                presentation: Box::new(before),
                            }]
                        } else {
                            Vec::new()
                        },
                    )
                }
                Action::RestorePresentation { presentation } => {
                    let before = self.session.track_presentation.clone();
                    let changed = before != **presentation;
                    if changed {
                        self.session.track_presentation = (**presentation).clone();
                        self.track_arrangement_replaced();
                    }
                    (
                        ActionOutcome {
                            changed,
                            ..ActionOutcome::default()
                        },
                        if changed {
                            vec![Action::RestorePresentation {
                                presentation: Box::new(before),
                            }]
                        } else {
                            Vec::new()
                        },
                    )
                }
                Action::RemoveTracks { track_ids } => self.perform_track_removal(track_ids)?,
                Action::RestoreTracks(restore) => self.perform_track_restore(restore)?,
                _ => unreachable!("the session reduces every other action"),
            }
        };
        let entry = Session::undo_entry(action, inverse, coalesce);
        Ok((outcome, entry))
    }

    /// Navigation, panel management and list edits: the session carries them out and this
    /// shell runs the effects it reports.
    fn perform_session_action(
        &mut self,
        action: &Action,
    ) -> Result<(ActionOutcome, Vec<Action>, Option<String>)> {
        let dictionary_panel = self.session.dictionary_panel(action);
        let replaces_panels = matches!(action, Action::GoToLoci { loci, .. } if loci.len() > 1);
        let content_top = self.canvas_geometry().viewport.top();
        let reduction = {
            let selected = self
                .session
                .panels
                .panel(dictionary_panel)
                .selected_alignment_track;
            let layout = &self.workspace_layout;
            let canvas_width_for = |count: usize| layout.projected_canvas_width(count);
            let canvas = PanelCanvas {
                width_for: &canvas_width_for,
                content_top,
            };
            self.session.reduce(action, &canvas, selected)?
        };
        let SessionReduction {
            outcome,
            effects,
            inverse,
            coalesce,
        } = reduction;
        for effect in effects {
            self.run_panel_effect(effect);
        }
        if replaces_panels {
            self.synchronize_panel_widths();
            self.request_all_panel_queries();
        }
        Ok((outcome, inverse, coalesce))
    }

    fn run_panel_effect(&mut self, effect: Effect) {
        match effect {
            Effect::ViewportChanged { panel_id, commit } => match commit {
                ViewportCommit::Discrete => self.viewport_changed(panel_id),
                ViewportCommit::Step => self.viewport_step_changed(panel_id),
                ViewportCommit::Gesture => self.viewport_gesture_changed(panel_id),
            },
            Effect::PanelAdded {
                panel_id: _,
                previous_focus,
            } => {
                self.invalidate_navigation_contract("view panel added");
                self.synchronize_panel_widths();
                let focused = self.session.panels.active().id;
                if focused != previous_focus {
                    self.panel_focus_changed(previous_focus, focused);
                }
                self.request_all_panel_queries();
            }
            Effect::PanelClosed { panel_id } => {
                for track in &mut self.session.alignment_tracks {
                    if let Some(projection) = track.projections.remove(&panel_id) {
                        projection.advance_scene_generation();
                    }
                    // The closed panel's coverage request, publication and retained lane.
                    if let Some(engine) = &track.coverage_engine {
                        engine.release(
                            crate::workspace::alignment_coverage::alignment_coverage_owner(
                                panel_id,
                            ),
                        );
                    }
                }
                self.invalidate_navigation_contract("view panel removed");
                self.synchronize_panel_widths();
                self.refresh_active_panel_context();
                self.request_all_panel_queries();
                self.rebuild_all_panel_displays();
            }
            Effect::PanelsReordered => {
                self.invalidate_navigation_contract("view panel order changed");
                self.mark_display_dirty("explicit display invalidation");
                self.rebuild_all_panel_displays();
            }
            Effect::PanelActivated { previous, current } => {
                self.panel_focus_changed(previous, current);
            }
            Effect::WorkspaceListsChanged {
                regions_changed,
                review_changed,
            } => {
                if regions_changed {
                    self.invalidate_navigation_contract("region overlays changed");
                    self.rebuild_all_panel_displays();
                    self.mark_display_dirty("explicit display invalidation");
                } else if review_changed {
                    self.mark_display_dirty("explicit display invalidation");
                }
            }
        }
    }

    /// Focus moved from `previous` to `current` (possibly the same panel): the transient state
    /// tied to focus follows it.
    fn panel_focus_changed(&mut self, previous: PanelId, current: PanelId) {
        let changed = previous != current;
        if changed {
            for panel_id in [previous, current] {
                if let Some(panel) = self.session.panels.get_mut(panel_id) {
                    panel.hovered_junction_arc = None;
                    panel.hover_inspector_target = None;
                }
            }
            self.window.set_cursor(CursorIcon::Default);
        }
        self.refresh_active_panel_context();
        if changed {
            self.mark_display_dirty("active panel changed");
        }
        if changed && !self.session.synthetic {
            self.request_alignment_query(current);
            self.request_reference_query(current);
            self.request_track_query(current);
        }
    }

    /// The whole track arrangement was replaced by an undo step.
    fn track_arrangement_replaced(&mut self) {
        // Tracks opened since the snapshot was taken keep a row: sources are authoritative.
        self.reconcile_track_presentation();
        self.reconcile_presentation_interaction_state();
        self.invalidate_navigation_contract("track arrangement restored");
        self.request_all_panel_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
    }

    fn perform_track_removal(
        &mut self,
        track_ids: &[TrackId],
    ) -> Result<(ActionOutcome, Vec<Action>)> {
        let selected = track_ids.iter().copied().collect::<BTreeSet<_>>();
        let restore = super::TrackRestore {
            alignment: self
                .session
                .alignment_tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.config.id))
                .map(|(index, track)| (index, track.config.clone()))
                .collect(),
            auxiliary: self
                .session
                .tracks
                .tracks()
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.id))
                .map(|(index, track)| (index, track.clone()))
                .collect(),
            presentation: Box::new(self.session.track_presentation.clone()),
            suppressed_builtin_gene_bundles: self.session.suppressed_builtin_gene_bundles.clone(),
        };
        let metadata_selection = self.metadata_selected_tracks.clone();
        let removed = self.remove_tracks_now(&selected)?;
        if removed > 0 {
            // Removing a track drops it from the metadata selection; it does not discard the
            // rest of that selection.
            self.metadata_selected_tracks = metadata_selection
                .into_iter()
                .filter(|id| !selected.contains(id))
                .collect();
        }
        Ok((
            ActionOutcome {
                changed: removed > 0,
                removed_tracks: removed,
                ..ActionOutcome::default()
            },
            if removed > 0 {
                vec![Action::RestoreTracks(restore)]
            } else {
                Vec::new()
            },
        ))
    }

    /// Reopens removed tracks from their saved configuration. The engines were released when
    /// the tracks were removed; this starts new ones, exactly as loading the sources would.
    fn perform_track_restore(
        &mut self,
        restore: &super::TrackRestore,
    ) -> Result<(ActionOutcome, Vec<Action>)> {
        let mut restored = Vec::new();
        if !restore.alignment.is_empty() {
            let mut configs = self
                .session
                .alignment_tracks
                .iter()
                .map(|track| track.config.clone())
                .collect::<Vec<_>>();
            for (index, config) in &restore.alignment {
                if configs.iter().any(|existing| existing.id == config.id) {
                    continue;
                }
                configs.insert((*index).min(configs.len()), config.clone());
                restored.push(config.id);
            }
            let primary_before = self
                .session
                .alignment_tracks
                .first()
                .map(|track| track.config.id);
            self.replace_alignment_track_controllers(configs)?;
            if self
                .session
                .alignment_tracks
                .first()
                .map(|track| track.config.id)
                != primary_before
            {
                // The restored track is the first one again: it owns the primary settings.
                self.activate_first_alignment_track()?;
            }
            self.invalidate_navigation_contract("alignment source set changed");
            self.begin_all_panel_refresh_transactions();
            self.session.synthetic = false;
        }
        for (index, config) in &restore.auxiliary {
            if self
                .session
                .tracks
                .tracks()
                .iter()
                .any(|track| track.id == config.id)
            {
                continue;
            }
            let Some(kind) = track_spec_kind(config.kind, &config.source, config.format) else {
                anyhow::bail!("track {} cannot be reopened", config.name);
            };
            self.restore_auxiliary_track(*index, kind, config.clone())?;
            restored.push(config.id);
        }
        self.session
            .suppressed_builtin_gene_bundles
            .clone_from(&restore.suppressed_builtin_gene_bundles);
        self.session.track_presentation = (*restore.presentation).clone();
        self.track_arrangement_replaced();
        Ok((
            ActionOutcome {
                changed: !restored.is_empty(),
                ..ActionOutcome::default()
            },
            if restored.is_empty() {
                Vec::new()
            } else {
                vec![Action::RemoveTracks {
                    track_ids: restored,
                }]
            },
        ))
    }

    fn restore_auxiliary_track(
        &mut self,
        index: usize,
        kind: rgv_engine::TrackSpecKind,
        config: TrackConfig,
    ) -> Result<()> {
        let id = config.id;
        let path = source_path(&config.source);
        self.add_preconfigured_track_source(&path, kind, config)?;
        // A reopened track takes its place in the saved order, not the end of it.
        self.session.tracks.move_track(id, index);
        self.automation_auxiliary_open_state.remove(&id);
        Ok(())
    }
}
