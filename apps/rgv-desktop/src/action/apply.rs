//! The one path every state-changing intent takes: `Viewer::apply_action`.
//!
//! Navigation and panel management run the headless reducer and then carry out its effects.
//! Track actions call the workspace's own implementation. Either way the window and the
//! automation protocol reach the same code, and what they did is recorded for undo.

use super::history::UndoEntry;
use super::reduce::{self, PanelContext, Reduced};
use super::{Action, ActionOutcome, Effect, ViewportCommit};
use crate::Viewer;
use crate::sources::{source_path, track_spec_kind};
use crate::workspace::panels::PanelWorkspace;
use anyhow::Result;
use rgv_core::{PanelId, TrackConfig, TrackId, TrackIntent};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::Instant;
use winit::window::CursorIcon;

/// The gesture a track-arrangement edit may be one sample of. Divider drags report a new
/// height every frame; edits with the same key that arrive close together are one undo step.
/// Toggles, moves and grouping are complete edits and never coalesce.
pub(crate) fn track_intents_coalesce_key(intents: &[TrackIntent]) -> Option<String> {
    let mut key = String::new();
    for intent in intents {
        match intent {
            TrackIntent::ResizeNode { target, .. } => {
                let _ = write!(key, "resize:{target:?};");
            }
            TrackIntent::ResizeReadPane { .. } => key.push_str("resize-read-pane;"),
            TrackIntent::SetSectionExpanded { .. }
            | TrackIntent::SetPaneRowsExpanded { .. }
            | TrackIntent::ResetPaneRowHeights { .. }
            | TrackIntent::SetNodeVisibility { .. }
            | TrackIntent::SetGroupExpanded { .. }
            | TrackIntent::SetGroupVisibility { .. }
            | TrackIntent::SetCompositeVisibility { .. }
            | TrackIntent::GroupNodes { .. }
            | TrackIntent::Ungroup { .. }
            | TrackIntent::MoveNode { .. } => return None,
        }
    }
    (!key.is_empty()).then_some(key)
}

impl Viewer {
    /// Applies one user intent and records how to undo it.
    pub(crate) fn apply_action(&mut self, action: Action) -> Result<ActionOutcome> {
        let (outcome, entry) = self.perform_action(action)?;
        if let Some(entry) = entry {
            self.shell.undo.record(entry, Instant::now());
        }
        Ok(outcome)
    }

    /// Whether Undo / Redo have something to act on, and what it is called.
    pub(crate) fn undo_label(&self) -> Option<&'static str> {
        self.shell.undo.undo_label()
    }

    pub(crate) fn redo_label(&self) -> Option<&'static str> {
        self.shell.undo.redo_label()
    }

    /// Takes back the most recent recorded edit. Returns whether there was one.
    pub(crate) fn undo_action(&mut self) -> bool {
        let Some(entry) = self.shell.undo.take_undo() else {
            return false;
        };
        for action in entry.undo.clone() {
            if let Err(error) = self.perform_action(action) {
                // The document moved under the entry (a source vanished, the window is too
                // narrow for the panel). Report it and drop the step; never leave it half
                // applied on the stack.
                self.report_error(format!("Could not undo {}: {error}", entry.label));
                return true;
            }
        }
        self.data_status = format!("Undid {}", entry.label);
        self.shell.undo.push_redo(entry);
        self.after_history_step();
        true
    }

    /// Re-applies the most recently undone edit. Returns whether there was one.
    pub(crate) fn redo_action(&mut self) -> bool {
        let Some(entry) = self.shell.undo.take_redo() else {
            return false;
        };
        let mut undo = Vec::new();
        for action in entry.redo.clone() {
            match self.perform_action(action) {
                Ok((_, Some(mut redone))) => {
                    // The fresh inverse describes the document as it is now.
                    redone.undo.append(&mut undo);
                    undo = redone.undo;
                }
                Ok((_, None)) => {}
                Err(error) => {
                    self.report_error(format!("Could not redo {}: {error}", entry.label));
                    return true;
                }
            }
        }
        self.data_status = format!("Redid {}", entry.label);
        if !undo.is_empty() {
            self.shell.undo.record_redone(UndoEntry {
                label: entry.label,
                redo: entry.redo,
                undo,
                coalesce: None,
            });
        }
        self.after_history_step();
        true
    }

    fn after_history_step(&mut self) {
        self.bump_workspace_revision();
        self.automation_alignment_handles.clear();
        self.mark_retained_evidence_stale("an edit was undone or redone");
        self.mark_display_dirty("undo history step");
        self.window.request_redraw();
    }

    /// Applies an action and returns the undo entry it earned, without recording it.
    fn perform_action(&mut self, action: Action) -> Result<(ActionOutcome, Option<UndoEntry>)> {
        let label = action.label();
        let mut coalesce = None;
        let (outcome, inverse) = if reduce::is_panel_action(&action) {
            self.perform_panel_action(&action)?
        } else {
            match &action {
                Action::ApplyTrackIntents { intents } => {
                    coalesce = track_intents_coalesce_key(intents);
                    let before = self.track_presentation.clone();
                    let change_set = self.apply_track_intents_now(intents)?;
                    let changed = self.track_presentation != before;
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
                    let before = self.track_presentation.clone();
                    let changed = before != **presentation;
                    if changed {
                        self.track_presentation = (**presentation).clone();
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
                Action::SetWorkspaceLists {
                    regions,
                    review_items,
                } => {
                    let (change, inverse) = reduce::reduce_workspace_lists(
                        &mut self.regions_of_interest,
                        &mut self.review_items,
                        regions.clone(),
                        review_items.clone(),
                    );
                    if change.in_place {
                        coalesce = Some("workspace-lists:in-place".to_owned());
                    }
                    if change.regions_changed {
                        self.invalidate_navigation_contract("region overlays changed");
                        self.rebuild_all_panel_displays();
                        self.mark_display_dirty("explicit display invalidation");
                    } else if change.review_changed {
                        self.mark_display_dirty("explicit display invalidation");
                    }
                    (
                        ActionOutcome {
                            changed: inverse.is_some(),
                            ..ActionOutcome::default()
                        },
                        inverse.into_iter().collect(),
                    )
                }
                _ => unreachable!("panel actions are reduced above"),
            }
        };
        let entry = (!inverse.is_empty()).then(|| UndoEntry {
            label,
            redo: vec![action],
            undo: inverse,
            coalesce,
        });
        Ok((outcome, entry))
    }

    fn perform_panel_action(&mut self, action: &Action) -> Result<(ActionOutcome, Vec<Action>)> {
        let focused = self.panels.active().id;
        // A new panel, and a workspace laid out from a locus list, are expressed in the focused
        // panel's dictionary; everything else in the addressed panel's.
        let dictionary_panel = match action {
            Action::AddPanel { .. } | Action::RestorePanel(_) => focused,
            _ => action.panel_id().unwrap_or(focused),
        };
        let replaces_panels = matches!(action, Action::GoToLoci { loci, .. } if loci.len() > 1);
        let content_top = self.canvas_geometry().viewport.top();
        let reduced = {
            let selected = self.panels.panel(dictionary_panel).selected_alignment_track;
            let alignment_tracks = &self.alignment_tracks;
            let dictionary = selected
                .and_then(|id| {
                    alignment_tracks
                        .iter()
                        .find(|track| track.config.id == id)
                        .and_then(|track| track.dictionary.as_ref())
                })
                .or_else(|| {
                    alignment_tracks
                        .first()
                        .and_then(|track| track.dictionary.as_ref())
                })
                .or(self.reference_dictionary.as_ref());
            let layout = &self.workspace_layout;
            let canvas_width_for = |count: usize| layout.projected_canvas_width(count);
            let ctx = PanelContext {
                dictionary,
                fallback_dictionary: &self.assembly.dictionary,
                aliases: &self.contig_aliases,
                canvas_width_for: &canvas_width_for,
                content_top,
            };
            let gesture = &mut self.shell.navigation_gesture;
            let settings = &mut self.settings;
            // A still-pending gesture becomes its own history entry and recent locus before a
            // discrete navigation moves the panel, so the gesture's end point is not lost.
            let mut commit_pending_gesture = |panels: &mut PanelWorkspace| {
                if let Some(panel_id) = gesture.take()
                    && let Some(locus) = reduce::record_panel_visit(panels, panel_id)
                {
                    settings.remember_locus(locus);
                }
            };
            reduce::reduce_panel_action(
                &mut self.panels,
                &ctx,
                action,
                &mut commit_pending_gesture,
            )?
        };
        let Reduced {
            outcome,
            effects,
            inverse,
        } = reduced;
        for effect in effects {
            self.run_panel_effect(effect);
        }
        if replaces_panels {
            self.synchronize_panel_widths();
            self.request_all_panel_queries();
        }
        Ok((outcome, inverse))
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
                let focused = self.panels.active().id;
                if focused != previous_focus {
                    self.panel_focus_changed(previous_focus, focused);
                }
                self.request_all_panel_queries();
            }
            Effect::PanelClosed { panel_id } => {
                for track in &mut self.alignment_tracks {
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
        }
    }

    /// Focus moved from `previous` to `current` (possibly the same panel): the transient state
    /// tied to focus follows it.
    fn panel_focus_changed(&mut self, previous: PanelId, current: PanelId) {
        let changed = previous != current;
        if changed {
            for panel_id in [previous, current] {
                if let Some(panel) = self.panels.get_mut(panel_id) {
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
        if changed && !self.synthetic {
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
                .alignment_tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.config.id))
                .map(|(index, track)| (index, track.config.clone()))
                .collect(),
            auxiliary: self
                .tracks
                .tracks()
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.id))
                .map(|(index, track)| (index, track.clone()))
                .collect(),
            presentation: Box::new(self.track_presentation.clone()),
            suppressed_builtin_gene_bundles: self.suppressed_builtin_gene_bundles.clone(),
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
            let primary_before = self.alignment_tracks.first().map(|track| track.config.id);
            self.replace_alignment_track_controllers(configs)?;
            if self.alignment_tracks.first().map(|track| track.config.id) != primary_before {
                // The restored track is the first one again: it owns the primary settings.
                self.activate_first_alignment_track()?;
            }
            self.invalidate_navigation_contract("alignment source set changed");
            self.begin_all_panel_refresh_transactions();
            self.synthetic = false;
        }
        for (index, config) in &restore.auxiliary {
            if self
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
        self.suppressed_builtin_gene_bundles
            .clone_from(&restore.suppressed_builtin_gene_bundles);
        self.track_presentation = (*restore.presentation).clone();
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
        self.tracks.move_track(id, index);
        self.automation_auxiliary_open_state.remove(&id);
        Ok(())
    }
}
