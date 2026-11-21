use crate::automation::commands::evidence::{automation_locus, bounded_automation_text};
use crate::automation::{
    automation_bool, automation_f32, automation_inspector_export, automation_name,
    automation_optional_name, automation_optional_u64, automation_u8, automation_u32,
    automation_u64,
};
use crate::inspection::model::{InspectorSnapshot, RetainedEvidenceSnapshot};
use crate::workspace::alignments::expanded_workspace_alignment_limit;
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY, Viewer, ui};
use anyhow::Result;
use rgv_automation::{
    AutomationEvidenceField, AutomationOptionScope, AutomationSelectionGranularity,
    AutomationSelectionKind, AutomationSetting, AutomationViewportOperation,
};
use rgv_core::{
    AlignmentViewPredicate, CoverageScaleTransform, PanelId, PresentationNodeId, ReadStackingMode,
    TrackId, TrackIntent,
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

impl Viewer {
    pub(crate) fn bump_workspace_revision(&mut self) -> u64 {
        self.workspace_revision = self.workspace_revision.saturating_add(1);
        self.workspace_revision
    }

    pub(crate) fn mark_retained_evidence_stale(&mut self, reason: &str) {
        for panel in self.panels.iter_mut() {
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
            self.panels.active_mut().retained_evidence = None;
            return;
        };
        selection.selection_id = uuid::Uuid::new_v4().to_string();
        selection.evidence_revision = self.next_evidence_revision;
        selection.workspace_revision_at_selection = self.workspace_revision;
        selection.granularity = granularity;
        if let Some(track_id) = selection.track_id
            && let Some((authority, generation)) = self
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
        self.panels.active_mut().retained_evidence = Some(RetainedEvidenceSnapshot {
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
        let focused = self.panels.active().id;
        let actions = crate::action::translate::automation_command_actions(command, focused)
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
            self.panels.get(panel_id).is_some(),
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
        let next = self.panels.panel(panel_id).viewport;
        let interval = outcome.interval.unwrap_or_else(|| next.interval());
        if let Some(runner) = self.automation.as_mut() {
            runner.begin_navigation(panel_id, interval, &navigation_state, navigation_started);
        }
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "locus": automation_locus(&self.panels.panel(panel_id).contig_name, interval),
            "contig": self.panels.panel(panel_id).contig_name,
            "start": interval.start(),
            "end": interval.end(),
            "detail_level": next.detail_level(),
            "workspace_revision": self.workspace_revision,
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
            self.alignment_tracks
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
            AutomationOptionScope::Workspace => {
                let mut query_changed = false;
                match key {
                    "show_reference" => {
                        self.primary_alignment_filters_mut().show_reference =
                            automation_bool(value, key)?;
                    }
                    "show_coverage" => {
                        self.primary_alignment_filters_mut().show_coverage =
                            automation_bool(value, key)?;
                    }
                    "show_reads" => {
                        let visible = automation_bool(value, key)?;
                        self.apply_track_intents(&[TrackIntent::SetNodeVisibility {
                            target: PresentationNodeId::Track {
                                channel: ui::TrackPane::Reads,
                                track_id: alignment_track,
                            },
                            visible,
                        }])?;
                    }
                    "show_junctions" => {
                        self.primary_alignment_filters_mut().show_junctions =
                            automation_bool(value, key)?;
                    }
                    "show_junction_labels" => {
                        self.primary_alignment_filters_mut().show_junction_labels =
                            automation_bool(value, key)?;
                    }
                    "show_coverage_center_line" => {
                        self.primary_alignment_filters_mut()
                            .show_coverage_center_line = automation_bool(value, key)?;
                    }
                    "view_as_pairs" => {
                        self.primary_alignment_filters_mut().view_as_pairs =
                            automation_bool(value, key)?;
                    }
                    "quality_weighted_alleles" => {
                        self.primary_alignment_filters_mut()
                            .quality_weighted_alleles = automation_bool(value, key)?;
                    }
                    "shade_read_bases_by_quality" => {
                        self.primary_alignment_filters_mut()
                            .shade_read_bases_by_quality = automation_bool(value, key)?;
                    }
                    "show_modified_bases" => {
                        self.primary_alignment_filters_mut().show_modified_bases =
                            automation_bool(value, key)?;
                    }
                    "show_modification_frequency" => {
                        let requested = automation_bool(value, key)?;
                        query_changed = requested
                            != self.primary_alignment_filters().show_modification_frequency;
                        self.primary_alignment_filters_mut()
                            .show_modification_frequency = requested;
                    }
                    "minimum_mapping_quality" => {
                        let requested = automation_optional_u64(value, key)?
                            .map(|value| automation_u8(value, key))
                            .transpose()?;
                        query_changed = requested
                            != self
                                .primary_active_filters()
                                .alignment
                                .minimum_mapping_quality;
                        self.primary_active_filters_mut()
                            .alignment
                            .minimum_mapping_quality = requested;
                        self.primary_filter_editor_mut().minimum_mapq =
                            requested.unwrap_or_default();
                    }
                    "include_secondary" => {
                        let requested = automation_bool(value, key)?;
                        query_changed =
                            requested != self.primary_active_filters().include_secondary;
                        self.primary_alignment_filters_mut().include_secondary = requested;
                        self.primary_active_filters_mut().include_secondary = requested;
                        self.primary_filter_editor_mut().include_secondary = requested;
                    }
                    "include_supplementary" => {
                        let requested = automation_bool(value, key)?;
                        query_changed =
                            requested != self.primary_active_filters().include_supplementary;
                        self.primary_alignment_filters_mut().include_supplementary = requested;
                        self.primary_active_filters_mut().include_supplementary = requested;
                        self.primary_filter_editor_mut().include_supplementary = requested;
                    }
                    "minimum_base_quality" => {
                        self.primary_alignment_filters_mut().minimum_base_quality =
                            automation_u8(automation_u64(value, key)?, key)?;
                    }
                    "minimum_allele_fraction" => {
                        let fraction = automation_f32(value, key)?;
                        anyhow::ensure!(
                            (0.0..=1.0).contains(&fraction),
                            "{key} must be between 0 and 1"
                        );
                        self.primary_alignment_filters_mut().minimum_allele_fraction = fraction;
                    }
                    "vertical_density" => {
                        let density = automation_f32(value, key)?;
                        anyhow::ensure!(
                            (MIN_VERTICAL_DENSITY..=MAX_VERTICAL_DENSITY).contains(&density),
                            "{key} must be between {MIN_VERTICAL_DENSITY} and {MAX_VERTICAL_DENSITY}"
                        );
                        self.set_alignment_density_for_track(alignment_track, density);
                    }
                    "coverage_scale_maximum" => {
                        let maximum = automation_optional_u64(value, key)?
                            .map(|value| automation_u32(value, key))
                            .transpose()?;
                        anyhow::ensure!(maximum != Some(0), "{key} must be positive or null");
                        self.primary_alignment_filters_mut().coverage_scale_maximum = maximum;
                    }
                    "coverage_scale_transform" => {
                        let requested = automation_name(value, key)?;
                        self.primary_alignment_filters_mut()
                            .coverage_scale_transform = match requested.as_str() {
                            "linear" => CoverageScaleTransform::Linear,
                            "square_root" => CoverageScaleTransform::SquareRoot,
                            _ => anyhow::bail!("{key} must be linear or square_root"),
                        };
                    }
                    "minimum_indel_size" => {
                        let minimum = automation_u32(automation_u64(value, key)?, key)?;
                        anyhow::ensure!(minimum > 0, "{key} must be positive");
                        self.primary_alignment_filters_mut().minimum_indel_size = minimum;
                    }
                    "minimum_event_label_size" => {
                        let minimum = automation_u32(automation_u64(value, key)?, key)?;
                        anyhow::ensure!(minimum > 0, "{key} must be positive");
                        self.primary_alignment_filters_mut()
                            .minimum_event_label_size = minimum;
                    }
                    _ => anyhow::bail!("unknown workspace option {key}"),
                }
                self.invalidate_automation_alignment_layouts(None, query_changed);
                if query_changed {
                    self.request_all_panel_queries();
                }
                self.rebuild_all_panel_displays();
            }
            AutomationOptionScope::Track { track_id } => {
                let source_exists = self
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == track_id)
                    || self
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
                let primary_id = self.primary_alignment_track_id();
                if let Some(track) = self
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == track_id)
                {
                    match key {
                        "name" => track.config.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.config.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown alignment-track option {key}"),
                    }
                    if track_id == primary_id {
                        self.activate_first_alignment_track()?;
                    }
                    self.rebuild_all_panel_displays();
                } else if let Some(track) = self
                    .tracks
                    .tracks_mut()
                    .iter_mut()
                    .find(|track| track.id == track_id)
                {
                    match key {
                        "name" => track.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown auxiliary-track option {key}"),
                    }
                    self.rebuild_all_panel_displays();
                }
            }
            AutomationOptionScope::Panel { panel_id } => {
                // The panel-level `vertical_scroll_points` field mirrors the primary track only.
                let mirrors_primary = alignment_track == self.primary_alignment_track_id();
                let primary_track_id = alignment_track;
                if self.panels.get(panel_id).is_none() {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                }
                match key {
                    "vertical_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        let panel = self
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
                        self.panels.annotation_scroll_points =
                            scroll.min(self.panels.maximum_annotation_scroll_points());
                    }
                    _ => anyhow::bail!("unknown panel option {key}"),
                }
                self.rebuild_all_panel_displays();
            }
        }
        self.mark_display_dirty("explicit display invalidation");
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
            AutomationSetting::ReadBrowsingBufferPercent { value } => {
                deferred_shell_change = self.settings.read_browsing_buffer_percent != *value;
                self.settings.read_browsing_buffer_percent = *value;
                if deferred_shell_change {
                    self.persist_settings();
                    self.request_all_panel_queries();
                }
            }
            AutomationSetting::TotalCacheBudgetMib { value } => {
                deferred_shell_change = self.settings.cache_budget_mib != *value;
                self.settings.cache_budget_mib = *value;
                if deferred_shell_change {
                    self.apply_live_cache_budget(*value);
                    self.persist_settings();
                    self.request_all_panel_queries();
                }
            }
            AutomationSetting::TrackPipeline { value } => {
                deferred_shell_change = self.settings.track_pipeline != *value;
                if deferred_shell_change {
                    self.settings.track_pipeline = *value;
                    self.persist_settings();
                    self.apply_track_pipeline_setting();
                }
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
            AutomationSetting::ShowReference { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_reference",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowCoverage { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_coverage",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowReads { value, track_id } => self.apply_automation_option_for(
                &AutomationOptionScope::Workspace,
                "show_reads",
                &serde_json::json!(value),
                *track_id,
            )?,
            AutomationSetting::ShowJunctions { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_junctions",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ViewAsPairs { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "view_as_pairs",
                &serde_json::json!(value),
            )?,
            AutomationSetting::AlignmentDensity { value, track_id } => self
                .apply_automation_option_for(
                    &AutomationOptionScope::Workspace,
                    "vertical_density",
                    &serde_json::json!(value),
                    *track_id,
                )?,
            AutomationSetting::IncludeSecondary { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "include_secondary",
                &serde_json::json!(value),
            )?,
            AutomationSetting::IncludeSupplementary { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "include_supplementary",
                &serde_json::json!(value),
            )?,
            AutomationSetting::CoverageScaleTransform { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "coverage_scale_transform",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ReadBaseDisplayMode { value } => {
                self.primary_alignment_filters_mut().read_base_display_mode = *value;
                self.invalidate_automation_alignment_layouts(None, false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ShadeReadBasesByQuality { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "shade_read_bases_by_quality",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowModifiedBases { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_modified_bases",
                &serde_json::json!(value),
            )?,
            AutomationSetting::OverlappingMatePolicy { value } => {
                let changed = self.primary_alignment_filters().overlapping_mate_policy != *value;
                self.primary_alignment_filters_mut().overlapping_mate_policy = *value;
                self.invalidate_automation_alignment_layouts(None, changed);
                if changed {
                    self.request_all_panel_queries();
                }
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ModifiedBaseColorMode { value } => {
                self.primary_alignment_filters_mut()
                    .modified_base_color_mode = *value;
                self.invalidate_automation_alignment_layouts(None, false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::AlignmentColorMode { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.alignment_color_mode = *value;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::AlignmentViewFilter { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                anyhow::ensure!(
                    self.reference_source.is_some()
                        || !value
                            .all
                            .iter()
                            .any(AlignmentViewPredicate::requires_reference),
                    "reference-backed alignment filters require a matching reference FASTA"
                );
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.alignment_view_filter = value.clone();
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), true);
                self.request_all_panel_queries();
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::AlignmentHighlightFilter { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                anyhow::ensure!(
                    self.reference_source.is_some()
                        || !value
                            .all
                            .iter()
                            .any(AlignmentViewPredicate::requires_reference),
                    "reference-backed alignment highlights require a matching reference FASTA"
                );
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.alignment_highlight_filter = value.clone();
                }
                self.refresh_alignment_highlight_interactions(track_ids);
                self.invalidate_automation_alignment_layouts(Some(track_ids), false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadStackingMode { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.read_stacking_mode = *value;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::CompleteReadLimit { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                let (workspace_limit, requested_limit) =
                    expanded_workspace_alignment_limit(self.workspace_alignment_limit, *value);
                self.workspace_alignment_limit = workspace_limit;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.maximum_alignments = requested_limit;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), true);
                self.request_all_panel_queries();
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::RebuildStableReadArchitecture {
                panel_id,
                track_ids,
            } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                anyhow::ensure!(
                    self.panels.get(*panel_id).is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                for track in self
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
                let Some(panel) = self.panels.get_mut(*panel_id) else {
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
                let Some(panel) = self.panels.get_mut(*panel_id) else {
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
                    self.panels.get(*panel_id).is_some(),
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
            } => self.panels.get(*panel_id).map(|panel| {
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
            "workspace_revision": self.workspace_revision,
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
