use crate::action::translate::{AlignmentOption, TrackLabelEdit};
use crate::automation::commands::evidence::{automation_locus, bounded_automation_text};
use crate::automation::{
    automation_bool, automation_f32, automation_inspector_export, automation_name,
    automation_optional_name, automation_optional_u64, automation_u8, automation_u32,
    automation_u64,
};
use crate::inspection::model::{InspectorSnapshot, RetainedEvidenceSnapshot};
use crate::{Viewer, ui};
use anyhow::Result;
use rgv_automation::{
    AutomationEvidenceField, AutomationOptionScope, AutomationSelectionGranularity,
    AutomationSelectionKind, AutomationSetting, AutomationViewportOperation,
};
use rgv_core::{
    CoverageScaleTransform, PanelId, PresentationNodeId, ReadStackingMode, TrackId, TrackIntent,
};
use std::time::Instant;

/// Resolves the alignment track a per-track automation setting addresses. A named track must
/// be an open alignment track; an unnamed one is the primary alignment, as before the settings
/// could name a track.
pub(crate) fn automation_alignment_target(
    named: Option<TrackId>,
    primary: TrackId,
    is_alignment_track: impl Fn(TrackId) -> bool,
) -> Result<TrackId> {
    match named {
        None => Ok(primary),
        Some(track_id) if is_alignment_track(track_id) => Ok(track_id),
        Some(track_id) => anyhow::bail!("alignment track {} was not found", track_id.get()),
    }
}

/// Reads the JSON value of a `set_option` workspace key other than `show_reads`.
pub(crate) fn workspace_alignment_option(
    key: &str,
    value: &serde_json::Value,
) -> Result<AlignmentOption> {
    Ok(match key {
        "show_reference" => AlignmentOption::ShowReference(automation_bool(value, key)?),
        "show_coverage" => AlignmentOption::ShowCoverage(automation_bool(value, key)?),
        "show_junctions" => AlignmentOption::ShowJunctions(automation_bool(value, key)?),
        "show_junction_labels" => AlignmentOption::ShowJunctionLabels(automation_bool(value, key)?),
        "show_coverage_center_line" => {
            AlignmentOption::ShowCoverageCenterLine(automation_bool(value, key)?)
        }
        "view_as_pairs" => AlignmentOption::ViewAsPairs(automation_bool(value, key)?),
        "quality_weighted_alleles" => {
            AlignmentOption::QualityWeightedAlleles(automation_bool(value, key)?)
        }
        "shade_read_bases_by_quality" => {
            AlignmentOption::ShadeReadBasesByQuality(automation_bool(value, key)?)
        }
        "show_modified_bases" => AlignmentOption::ShowModifiedBases(automation_bool(value, key)?),
        "show_modification_frequency" => {
            AlignmentOption::ShowModificationFrequency(automation_bool(value, key)?)
        }
        "minimum_mapping_quality" => AlignmentOption::MinimumMappingQuality(
            automation_optional_u64(value, key)?
                .map(|value| automation_u8(value, key))
                .transpose()?,
        ),
        "include_secondary" => AlignmentOption::IncludeSecondary(automation_bool(value, key)?),
        "include_supplementary" => {
            AlignmentOption::IncludeSupplementary(automation_bool(value, key)?)
        }
        "minimum_base_quality" => {
            AlignmentOption::MinimumBaseQuality(automation_u8(automation_u64(value, key)?, key)?)
        }
        "minimum_allele_fraction" => {
            AlignmentOption::MinimumAlleleFraction(automation_f32(value, key)?)
        }
        "vertical_density" => AlignmentOption::VerticalDensity(automation_f32(value, key)?),
        "coverage_scale_maximum" => AlignmentOption::CoverageScaleMaximum(
            automation_optional_u64(value, key)?
                .map(|value| automation_u32(value, key))
                .transpose()?,
        ),
        "coverage_scale_transform" => {
            AlignmentOption::CoverageScaleTransform(match automation_name(value, key)?.as_str() {
                "linear" => CoverageScaleTransform::Linear,
                "square_root" => CoverageScaleTransform::SquareRoot,
                _ => anyhow::bail!("{key} must be linear or square_root"),
            })
        }
        "minimum_indel_size" => {
            AlignmentOption::MinimumIndelSize(automation_u32(automation_u64(value, key)?, key)?)
        }
        "minimum_event_label_size" => AlignmentOption::MinimumEventLabelSize(automation_u32(
            automation_u64(value, key)?,
            key,
        )?),
        _ => anyhow::bail!("unknown workspace option {key}"),
    })
}

impl Viewer {
    pub(crate) fn bump_workspace_revision(&mut self) -> u64 {
        self.session.bump_workspace_revision()
    }

    pub(crate) fn mark_retained_evidence_stale(&mut self, reason: &str) {
        for panel in self.session.panels.iter_mut() {
            if let Some(retained) = &mut panel.retained_evidence {
                retained.selection.stale = true;
                retained.selection.stale_reason = Some(bounded_automation_text(reason, 512));
            }
        }
    }

    pub(crate) fn retain_active_selection(
        &mut self,
        granularity: Option<AutomationSelectionGranularity>,
    ) {
        let Some(mut selection) = self.live_automation_selection_snapshot() else {
            self.session.panels.active_mut().retained_evidence = None;
            return;
        };
        selection.selection_id = uuid::Uuid::new_v4().to_string();
        selection.evidence_revision = self.next_evidence_revision;
        selection.workspace_revision_at_selection = self.session.workspace_revision;
        selection.granularity = granularity;
        if let Some(track_id) = selection.track_id
            && let Some((authority, generation)) = self
                .session
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
                .and_then(|track| {
                    if track.scene_engine.is_some() {
                        self.presented_alignment_lane(selection.panel_id, track_id)
                            .map(|presented| ("scene_target_revision", presented.target_revision))
                    } else {
                        track.projection(selection.panel_id).map(|projection| {
                            ("query_generation", projection.clock.current().get())
                        })
                    }
                })
        {
            selection.provenance.push(AutomationEvidenceField {
                key: authority.to_owned(),
                value: generation.to_string(),
            });
        }
        self.next_evidence_revision = self.next_evidence_revision.saturating_add(1);
        let inspector = match selection.kind {
            AutomationSelectionKind::Read | AutomationSelectionKind::ReadEvent => {
                self.inspector_snapshot()
            }
            AutomationSelectionKind::Pileup => self.pileup_inspector_snapshot(),
            AutomationSelectionKind::Modification => self.modification_inspector_snapshot(),
            AutomationSelectionKind::Junction => self.junction_inspector_snapshot(),
            _ => None,
        };
        selection.presentation_truncated = inspector
            .as_ref()
            .and_then(|inspector| inspector.cigar.as_ref())
            .is_some_and(|cigar| cigar.len() > 256);
        let export = inspector.as_ref().map_or_else(
            || {
                if selection.fields.is_empty() {
                    String::new()
                } else {
                    automation_inspector_export(&InspectorSnapshot {
                        fields: selection
                            .fields
                            .iter()
                            .map(|field| (field.key.clone(), field.value.clone()))
                            .collect(),
                        auxiliary: Vec::new(),
                        event_sequence: None,
                        cigar: None,
                        read_sequence: None,
                    })
                }
            },
            automation_inspector_export,
        );
        self.session.panels.active_mut().retained_evidence = Some(RetainedEvidenceSnapshot {
            selection,
            inspector,
            export,
        });
    }

    /// Applies an automation command that is a pure translation to actions (see
    /// [`crate::action::translate::automation_command_actions`]) and returns the outcome of the
    /// last one.
    pub(crate) fn apply_command_translation(
        &mut self,
        command: &rgv_automation::AutomationCommand,
    ) -> Result<crate::action::ActionOutcome> {
        let focused = self.session.panels.active().id;
        let actions = crate::action::translate::automation_command_actions(command, focused)
            .or_else(|| {
                crate::action::translate::automation_session_actions(&self.session, command)
            })
            .ok_or_else(|| anyhow::anyhow!("the command is not an action translation"))??;
        let mut outcome = crate::action::ActionOutcome::default();
        for action in actions {
            outcome = self.apply_action(action)?;
        }
        Ok(outcome)
    }

    /// The protocol entry point: `change_viewport` documents that its panel becomes the focused
    /// panel, so its translation is a focus change followed by the navigation.
    pub(crate) fn apply_automation_viewport_operation(
        &mut self,
        panel_id: PanelId,
        operation: AutomationViewportOperation,
    ) -> Result<serde_json::Value> {
        self.apply_viewport_operation(panel_id, operation, true)
    }

    /// Pans, zooms or fits `panel_id`. With `focus` false the focused panel is left alone.
    pub(crate) fn apply_viewport_operation(
        &mut self,
        panel_id: PanelId,
        operation: AutomationViewportOperation,
        focus: bool,
    ) -> Result<serde_json::Value> {
        let navigation_started = Instant::now();
        anyhow::ensure!(
            self.session.panels.get(panel_id).is_some(),
            "panel {} was not found",
            panel_id.get()
        );
        let navigation_state = self.automation_navigation_state(panel_id);
        let outcome = if focus {
            self.apply_command_translation(&rgv_automation::AutomationCommand::ChangeViewport {
                panel_id,
                operation,
            })?
        } else {
            self.apply_action(crate::action::translate::change_viewport(
                panel_id, operation,
            ))?
        };
        let next = self.session.panels.panel(panel_id).viewport;
        let interval = outcome.interval.unwrap_or_else(|| next.interval());
        if let Some(runner) = self.automation.as_mut() {
            runner.begin_navigation(panel_id, interval, &navigation_state, navigation_started);
        }
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "locus": automation_locus(&self.session.panels.panel(panel_id).contig_name, interval),
            "contig": self.session.panels.panel(panel_id).contig_name,
            "start": interval.start(),
            "end": interval.end(),
            "detail_level": next.detail_level(),
            "workspace_revision": self.session.workspace_revision,
        }))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_option(
        &mut self,
        scope: &AutomationOptionScope,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        self.apply_automation_option_for(scope, key, value, None)
    }

    /// The alignment track a per-track setting addresses: the one the command names, which
    /// must be an open alignment track, else the primary alignment.
    pub(crate) fn automation_alignment_target(&self, named: Option<TrackId>) -> Result<TrackId> {
        automation_alignment_target(named, self.primary_alignment_track_id(), |track_id| {
            self.session
                .alignment_tracks
                .iter()
                .any(|track| track.config.id == track_id)
        })
    }

    /// [`Self::apply_automation_option`] with the alignment track that the per-track keys
    /// (`show_reads`, `vertical_density`, `vertical_scroll_points`) address. `None` keeps their
    /// historical meaning, the primary alignment.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_option_for(
        &mut self,
        scope: &AutomationOptionScope,
        key: &str,
        value: &serde_json::Value,
        alignment_track: Option<TrackId>,
    ) -> Result<()> {
        let alignment_track = self.automation_alignment_target(alignment_track)?;
        match *scope {
            AutomationOptionScope::Workspace if key == "show_reads" => {
                let visible = automation_bool(value, key)?;
                self.apply_track_intents(&[TrackIntent::SetNodeVisibility {
                    target: PresentationNodeId::Track {
                        channel: ui::TrackPane::Reads,
                        track_id: alignment_track,
                    },
                    visible,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationOptionScope::Workspace => {
                // A thin translation: every other workspace key is an alignment settings edit.
                let actions = crate::action::translate::alignment_option_actions(
                    &self.session,
                    workspace_alignment_option(key, value)?,
                    Some(alignment_track),
                )?;
                for action in actions {
                    self.apply_action(action)?;
                }
                self.rebuild_all_panel_displays();
            }
            AutomationOptionScope::Track { track_id } => {
                let source_exists = self
                    .session
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == track_id)
                    || self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .any(|track| track.id == track_id);
                anyhow::ensure!(source_exists, "track {} was not found", track_id.get());
                let presentation_intent = match key {
                    "visible" => Some(TrackIntent::SetNodeVisibility {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        visible: automation_bool(value, key)?,
                    }),
                    "collapsed" => Some(TrackIntent::SetNodeVisibility {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        visible: !automation_bool(value, key)?,
                    }),
                    "height" => Some(TrackIntent::ResizeNode {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        preferred_height: automation_f32(value, key)?,
                    }),
                    _ => None,
                };
                if let Some(intent) = presentation_intent {
                    self.apply_track_intents(&[intent])?;
                    self.rebuild_all_panel_displays();
                    self.mark_display_dirty("explicit display invalidation");
                    return Ok(());
                }
                // A thin translation: a rename or sample association is a label edit.
                let alignment = self
                    .session
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == track_id);
                let edit = match key {
                    "name" => TrackLabelEdit::Name(automation_name(value, key)?),
                    "sample_key" => {
                        TrackLabelEdit::SampleKey(automation_optional_name(value, key)?)
                    }
                    _ if alignment => anyhow::bail!("unknown alignment-track option {key}"),
                    _ => anyhow::bail!("unknown auxiliary-track option {key}"),
                };
                for action in
                    crate::action::translate::track_label_actions(&self.session, track_id, edit)?
                {
                    self.apply_action(action)?;
                }
                self.rebuild_all_panel_displays();
            }
            AutomationOptionScope::Panel { panel_id } => {
                // The panel-level `vertical_scroll_points` field mirrors the primary track only.
                let mirrors_primary = alignment_track == self.primary_alignment_track_id();
                let primary_track_id = alignment_track;
                if self.session.panels.get(panel_id).is_none() {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                }
                match key {
                    "vertical_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        let panel = self
                            .session
                            .panels
                            .get_mut(panel_id)
                            .expect("panel existence was checked above");
                        let maximum = panel
                            .scroll_display_cache
                            .as_ref()
                            .and_then(|cache| {
                                cache
                                    .read_panels
                                    .iter()
                                    .find(|read_panel| read_panel.track_id == primary_track_id)
                            })
                            .map_or(0.0, |read_panel| {
                                (panel
                                    .read_panel_scroll_state(primary_track_id)
                                    .content_bottom_points
                                    - read_panel.clip_bottom)
                                    .max(0.0)
                            });
                        let scroll = scroll.min(maximum);
                        if mirrors_primary {
                            panel.vertical_scroll_points = scroll;
                        }
                        panel
                            .read_panel_scrolls
                            .entry(primary_track_id)
                            .or_default()
                            .scroll_points = scroll;
                    }
                    "annotation_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        self.session.panels.annotation_scroll_points =
                            scroll.min(crate::workspace::panels::maximum_annotation_scroll_points(
                                &self.session.panels,
                            ));
                    }
                    _ => anyhow::bail!("unknown panel option {key}"),
                }
                self.rebuild_all_panel_displays();
            }
        }
        self.mark_display_dirty("explicit display invalidation");
        Ok(())
    }

    /// Edits the transcript settings of a gene-model track as one track-settings action, as
    /// the track settings window does.
    fn apply_automation_gene_model_setting(
        &mut self,
        track_id: TrackId,
        edit: impl FnOnce(&mut rgv_core::FeatureTrackSettings),
    ) -> Result<()> {
        let track = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| anyhow::anyhow!("track {} was not found", track_id.get()))?;
        anyhow::ensure!(
            track.kind == rgv_core::TrackKind::GeneModel,
            "track {} is not a gene-model track",
            track_id.get()
        );
        let mut config = track.clone();
        edit(&mut config.feature);
        if config != *track {
            self.apply_action(crate::action::Action::SetTrackConfigs {
                tracks: vec![config],
            })?;
        }
        self.rebuild_all_panel_displays();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_setting(
        &mut self,
        setting: &AutomationSetting,
    ) -> Result<serde_json::Value> {
        let requested = serde_json::to_value(setting)?;
        let before = serde_json::to_value(self.automation_workspace_snapshot())?;
        let mut deferred_shell_change = false;
        match setting {
            AutomationSetting::WindowSize { width, height } => {
                let current = self
                    .window
                    .inner_size()
                    .to_logical::<f64>(self.window.scale_factor());
                deferred_shell_change = (current.width - f64::from(*width)).abs() >= 0.5
                    || (current.height - f64::from(*height)).abs() >= 0.5;
                let _ = self.window.request_inner_size(winit::dpi::LogicalSize::new(
                    f64::from(*width),
                    f64::from(*height),
                ));
                self.window.request_redraw();
            }
            AutomationSetting::InterfaceScalePercent { value } => {
                let scale = f32::from(*value) / 100.0;
                deferred_shell_change = (self.egui_context.zoom_factor() - scale).abs() > 0.001;
                self.egui_context.set_zoom_factor(scale);
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::ReadBrowsingBufferPercent { .. }
            | AutomationSetting::TotalCacheBudgetMib { .. }
            | AutomationSetting::TrackPipeline { .. } => {
                // A thin translation: a preference setting is one preferences action, applied
                // exactly as the Settings window applies it.
                let action =
                    crate::action::translate::preference_setting_action(&self.session, setting)
                        .expect("every preference setting translates to preferences");
                deferred_shell_change = self.apply_action(action)?.changed;
            }
            AutomationSetting::PropertiesPanelExpanded { value } => {
                let id = egui::Id::new("properties-panel-expanded");
                let current = self
                    .egui_context
                    .data(|data| data.get_temp::<bool>(id))
                    .unwrap_or(true);
                deferred_shell_change = current != *value;
                self.egui_context
                    .data_mut(|data| data.insert_temp(id, *value));
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::PropertiesPanelWidth { value } => {
                let id = egui::Id::new("properties-panel");
                let current = egui::containers::panel::PanelState::load(&self.egui_context, id)
                    .map_or(340.0, |state| state.outer_rect.width());
                deferred_shell_change = (current - value).abs() >= 0.5;
                let rect = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(*value, self.workspace_layout.properties.height().max(1.0)),
                );
                self.egui_context.data_mut(|data| {
                    data.insert_persisted(
                        id,
                        egui::containers::panel::PanelState { outer_rect: rect },
                    );
                });
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::PropertiesPanelResizeActive { value } => {
                deferred_shell_change = self.automation_properties_resize_active != *value;
                self.automation_properties_resize_active = *value;
                if *value {
                    // A held sash may pause long enough for inspection or a
                    // native capture. Gesture lifecycle, not elapsed wall
                    // time, decides when the final resolution query is due.
                    self.pending_resolution_query_refresh = None;
                } else if deferred_shell_change {
                    self.schedule_resolution_query_refresh();
                }
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::ShowReads { value, track_id } => self.apply_automation_option_for(
                &AutomationOptionScope::Workspace,
                "show_reads",
                &serde_json::json!(value),
                *track_id,
            )?,
            AutomationSetting::ShowReference { .. }
            | AutomationSetting::ShowCoverage { .. }
            | AutomationSetting::ShowJunctions { .. }
            | AutomationSetting::ViewAsPairs { .. }
            | AutomationSetting::AlignmentDensity { .. }
            | AutomationSetting::IncludeSecondary { .. }
            | AutomationSetting::IncludeSupplementary { .. }
            | AutomationSetting::CoverageScaleTransform { .. }
            | AutomationSetting::ReadBaseDisplayMode { .. }
            | AutomationSetting::ShadeReadBasesByQuality { .. }
            | AutomationSetting::ShowModifiedBases { .. }
            | AutomationSetting::OverlappingMatePolicy { .. }
            | AutomationSetting::ModifiedBaseColorMode { .. }
            | AutomationSetting::AlignmentColorMode { .. }
            | AutomationSetting::AlignmentViewFilter { .. }
            | AutomationSetting::AlignmentHighlightFilter { .. }
            | AutomationSetting::ReadStackingMode { .. }
            | AutomationSetting::CompleteReadLimit { .. } => {
                // A thin translation: the alignment settings are one settings action.
                self.apply_command_translation(&rgv_automation::AutomationCommand::Configure {
                    setting: setting.clone(),
                })?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::RebuildStableReadArchitecture {
                panel_id,
                track_ids,
            } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                anyhow::ensure!(
                    self.session.panels.get(*panel_id).is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                for track in self
                    .session
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    anyhow::ensure!(
                        track.config.filters.read_stacking_mode == ReadStackingMode::Stable,
                        "Rebuild stable architecture requires stable read stacking for track {}",
                        track.config.id.get()
                    );
                    let projection = track.projection_mut(*panel_id);
                    projection.advance_scene_generation();
                }
                self.request_all_alignment_queries();
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationSectionExpanded { .. }
            | AutomationSetting::PresentationNodeVisible { .. }
            | AutomationSetting::PresentationNodeHeight { .. }
            | AutomationSetting::PresentationGroupExpanded { .. }
            | AutomationSetting::PresentationGroupVisible { .. }
            | AutomationSetting::PresentationCompositeVisible { .. }
            | AutomationSetting::PresentationGroup { .. }
            | AutomationSetting::PresentationUngroup { .. }
            | AutomationSetting::ReadPaneHeight { .. } => {
                // A thin translation: the arrangement settings are track intents.
                let intents = crate::action::translate::setting_track_intents(setting)
                    .expect("every arrangement setting translates to track intents");
                self.apply_action(crate::action::Action::ApplyTrackIntents { intents })?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::TrackHeight { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "height",
                &serde_json::json!(value),
            )?,
            AutomationSetting::TrackCollapsed { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "collapsed",
                &serde_json::json!(value),
            )?,
            AutomationSetting::TrackVisible { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "visible",
                &serde_json::json!(value),
            )?,
            AutomationSetting::TrackTranscripts { track_id, value } => {
                self.apply_automation_gene_model_setting(*track_id, |settings| {
                    settings.transcripts = *value;
                })?;
            }
            AutomationSetting::TrackProteinCodingOnly { track_id, value } => {
                self.apply_automation_gene_model_setting(*track_id, |settings| {
                    settings.protein_coding_only = *value;
                })?;
            }
            AutomationSetting::VerticalScrollPoints {
                panel_id,
                value,
                track_id,
            } => self.apply_automation_option_for(
                &AutomationOptionScope::Panel {
                    panel_id: *panel_id,
                },
                "vertical_scroll_points",
                &serde_json::json!(value),
                *track_id,
            )?,
            AutomationSetting::AnnotationScrollPoints { panel_id, value } => self
                .apply_automation_option(
                    &AutomationOptionScope::Panel {
                        panel_id: *panel_id,
                    },
                    "annotation_scroll_points",
                    &serde_json::json!(value),
                )?,
            AutomationSetting::ReadStackScrollPoints { panel_id, value } => {
                let Some(panel) = self.session.panels.get_mut(*panel_id) else {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                };
                let maximum = (panel.pane_layout.reads.content_bottom
                    - panel.pane_layout.reads.pane_rect.bottom())
                .max(0.0);
                panel.read_stack_scroll_points = value.clamp(0.0, maximum);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadPanelScrollPoints {
                panel_id,
                track_id,
                value,
            } => {
                self.ensure_automation_alignment_tracks(&[*track_id])?;
                let Some(panel) = self.session.panels.get_mut(*panel_id) else {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                };
                let maximum = panel
                    .scroll_display_cache
                    .as_ref()
                    .and_then(|cache| {
                        cache
                            .read_panels
                            .iter()
                            .find(|read_panel| read_panel.track_id == *track_id)
                    })
                    .map_or(0.0, |read_panel| {
                        (panel
                            .read_panel_scroll_state(*track_id)
                            .content_bottom_points
                            - read_panel.clip_bottom)
                            .max(0.0)
                    });
                panel
                    .read_panel_scrolls
                    .entry(*track_id)
                    .or_default()
                    .scroll_points = value.clamp(0.0, maximum);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadPanelDensity {
                panel_id,
                track_id,
                value,
            } => {
                self.ensure_automation_alignment_tracks(&[*track_id])?;
                anyhow::ensure!(
                    self.session.panels.get(*panel_id).is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                self.set_read_panel_density(*panel_id, *track_id, *value);
                self.invalidate_automation_alignment_layouts(Some(&[*track_id]), false);
                self.rebuild_all_panel_displays();
            }
        }
        self.bump_workspace_revision();
        let workspace = self.automation_workspace_snapshot();
        let after = serde_json::to_value(&workspace)?;
        let effective = match setting {
            AutomationSetting::VerticalScrollPoints {
                panel_id,
                track_id: None,
                ..
            } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.vertical_scroll_points)),
            AutomationSetting::VerticalScrollPoints {
                panel_id,
                track_id: Some(track_id),
                ..
            } => self.session.panels.get(*panel_id).map(|panel| {
                serde_json::json!(panel.read_panel_scroll_state(*track_id).scroll_points)
            }),
            AutomationSetting::AnnotationScrollPoints { panel_id, .. } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.annotation_scroll_points)),
            AutomationSetting::ReadStackScrollPoints { panel_id, .. } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.read_stack_scroll_points)),
            AutomationSetting::ReadPanelScrollPoints {
                panel_id, track_id, ..
            } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .and_then(|panel| {
                    panel
                        .read_panel_states
                        .iter()
                        .find(|state| state.track_id == *track_id)
                })
                .map(|state| serde_json::json!(state.scroll_points)),
            AutomationSetting::ReadPanelDensity {
                panel_id, track_id, ..
            } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .and_then(|panel| {
                    panel
                        .read_panel_states
                        .iter()
                        .find(|state| state.track_id == *track_id)
                })
                .map(|state| serde_json::json!(state.effective_density)),
            AutomationSetting::TrackHeight { track_id, .. } => workspace
                .tracks
                .iter()
                .find(|track| track.id == *track_id)
                .map(|track| serde_json::json!(track.height)),
            AutomationSetting::CompleteReadLimit { track_ids, .. } => workspace
                .tracks
                .iter()
                .find(|track| track_ids.contains(&track.id))
                .and_then(|track| track.alignment.as_ref())
                .map(|alignment| serde_json::json!(alignment.requested_complete_read_limit)),
            _ => Some(
                requested
                    .get("value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            ),
        }
        .unwrap_or(serde_json::Value::Null);
        let requested_value = requested
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let clamped = requested_value != serde_json::Value::Null && requested_value != effective;
        Ok(serde_json::json!({
            "setting": requested.get("setting").cloned().unwrap_or(serde_json::Value::Null),
            "requested": requested_value,
            "effective": effective,
            "changed": before != after || deferred_shell_change,
            "clamped": clamped,
            "reason": clamped.then_some("requested value was constrained by the rendered layout"),
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.session.workspace_revision,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_track_settings_address_the_named_alignment_track_and_default_to_the_primary() {
        let (primary, second) = (TrackId::new(1), TrackId::new(4));
        let open = |track_id: TrackId| track_id == primary || track_id == second;
        assert_eq!(
            automation_alignment_target(None, primary, open).unwrap(),
            primary
        );
        assert_eq!(
            automation_alignment_target(Some(second), primary, open).unwrap(),
            second
        );
        let error = automation_alignment_target(Some(TrackId::new(9)), primary, open).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("alignment track 9 was not found")
        );
    }
}
