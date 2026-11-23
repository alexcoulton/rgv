//! One borrow-scoped egui frame transaction.
//!
//! Widget families and their typed drafts/actions live in sibling modules. This
//! module deliberately keeps the remaining shell composition and action commit
//! in one transaction so egui closures never retain application borrows across
//! engine polling or GPU submission.
//!
//! The transaction has three stages, each in its own child modules: `prepare`
//! snapshots application state into a [`ShellFrame`], `show` (with `menus`,
//! `toolbar`, `canvas` and `track_settings_window`) runs the egui pass against
//! it, and `commit` (with the `apply_*` modules) applies the result.

use crate::diagnostics::ManualDiagnosticFramePhase;
use crate::ui::{AlignmentSubtrack, TrackUiAction};
use crate::workspace::panels::{
    ReadPanelDisplayCache, ReadPanelScrollState, ReadRowCoordinateAuthority,
    current_read_row_authority,
};
use crate::{
    CachedGenomeOverview, GenomeOverviewCacheKey, GenomeOverviewTrackCacheKey, Viewer, ui,
};
use rgv_core::{PanelId, PresentationNodeId, SessionFilters, TrackId};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod apply_lists;
mod apply_requests;
mod apply_track_settings;
mod canvas;
mod commit;
mod menus;
mod prepare;
mod shell_dialogs;
mod show;
mod state;
mod toolbar;
mod track_settings_window;

use state::{
    AlignmentSettingsFrame, CommandDraft, FrameSnapshot, InspectorDraft, LayoutFrame,
    NavigationDraft, PanelContext, ShellFrame, TrackControlsDraft, WorkspaceListsDraft,
};

fn alignment_subtrack_visible(filters: &SessionFilters, subtrack: AlignmentSubtrack) -> bool {
    match subtrack {
        AlignmentSubtrack::Reference => filters.show_reference,
        AlignmentSubtrack::Coverage => filters.show_coverage,
        AlignmentSubtrack::Sashimi => filters.show_junctions,
        AlignmentSubtrack::Modifications => filters.show_modification_frequency,
        AlignmentSubtrack::Reads => filters.show_reads,
    }
}

fn alignment_subtrack_menu(
    ui: &mut egui::Ui,
    track_id: TrackId,
    filters: &SessionFilters,
    read_content_visible: bool,
    include_reference: bool,
    actions: &mut Vec<TrackUiAction>,
) {
    ui.label("Subtracks");
    for subtrack in AlignmentSubtrack::ALL {
        if subtrack == AlignmentSubtrack::Reference && !include_reference {
            continue;
        }
        let mut visible = if subtrack == AlignmentSubtrack::Reads {
            read_content_visible
        } else {
            alignment_subtrack_visible(filters, subtrack)
        };
        if ui.checkbox(&mut visible, subtrack.label()).changed() {
            if subtrack == AlignmentSubtrack::Reads {
                actions.push(TrackUiAction::SetNodeVisibility(
                    PresentationNodeId::Track {
                        channel: ui::TrackPane::Reads,
                        track_id,
                    },
                    visible,
                ));
            } else {
                actions.push(TrackUiAction::ToggleAlignmentSubtrack(track_id, subtrack));
            }
        }
    }
}

#[derive(Clone)]
struct ReadPanelScrollbarDraft {
    track_id: TrackId,
    /// Physical panel allocation from the live pane layout. Scene cache clips may lag this rect
    /// and are intentionally never copied into scrollbar geometry.
    allocation: egui::Rect,
    maximum: f32,
    offset: f32,
    density: f32,
    preview_fraction: Option<f32>,
    provisional: bool,
    overview: ui::ScrollbarOverview,
    interaction: ui::ScrollbarInteraction,
}

#[derive(Clone)]
struct WorkspaceScrollbarsDraft {
    index: usize,
    panel_id: PanelId,
    annotation_maximum: f32,
    annotation_offset: f32,
    annotation_overview: Option<ui::ScrollbarOverview>,
    annotation_clip_top: f32,
    annotation_clip_bottom: f32,
    annotation_interaction: ui::ScrollbarInteraction,
    read_stack_maximum: f32,
    read_stack_offset: f32,
    read_stack_interaction: ui::ScrollbarInteraction,
    read_clip_top: f32,
    read_panels: Vec<ReadPanelScrollbarDraft>,
}

fn pane_vertical_bounds(layout: &ui::PaneLayoutSnapshot, fallback: f32) -> (f32, f32) {
    let top = layout.pane_rect.top();
    let bottom = layout.pane_rect.bottom();
    if top.is_finite() && bottom.is_finite() && bottom >= top {
        (top, bottom)
    } else {
        (fallback, fallback)
    }
}

fn read_panel_scrollbar_rect(
    full_scrollbar: egui::Rect,
    left: f32,
    allocation: egui::Rect,
) -> egui::Rect {
    let left = left.clamp(full_scrollbar.left(), full_scrollbar.right());
    let top = allocation
        .top()
        .clamp(full_scrollbar.top(), full_scrollbar.bottom());
    let bottom = allocation.bottom().clamp(top, full_scrollbar.bottom());
    egui::Rect::from_min_max(
        egui::pos2(left, top),
        egui::pos2(full_scrollbar.right(), bottom),
    )
}

fn read_panel_scrollbar_draft(
    track_id: TrackId,
    allocation: egui::Rect,
    state: ReadPanelScrollState,
    density: f32,
    density_drag_fraction: Option<f32>,
    cached: Option<&ReadPanelDisplayCache>,
    current_alignment_target_revision: u64,
    committed_row_target_still_exact: bool,
) -> ReadPanelScrollbarDraft {
    let presented_authority = cached.map_or(ReadRowCoordinateAuthority::Absent, |read_panel| {
        current_read_row_authority(
            read_panel.presentation_target_revision,
            current_alignment_target_revision,
            committed_row_target_still_exact,
            read_panel.row_coordinate_authority,
        )
    });
    let maximum = cached.map_or(0.0, |_read_panel| {
        crate::display::read_panel_scrollbar_maximum(
            state,
            allocation.bottom(),
            presented_authority,
        )
    });
    let preview_fraction = density_drag_fraction.or_else(|| {
        (maximum <= f32::EPSILON)
            .then_some(state.latent_scroll_fraction)
            .flatten()
    });
    let provisional = presented_authority.is_pending();
    ReadPanelScrollbarDraft {
        track_id,
        allocation,
        maximum,
        offset: state.scroll_points.clamp(0.0, maximum),
        density,
        preview_fraction,
        provisional,
        overview: cached
            .map(|read_panel| read_panel.overview.clone())
            .unwrap_or_default(),
        interaction: ui::ScrollbarInteraction::default(),
    }
}

/// Rebase semantic genomic text from the display list that was committed at
/// the start of an egui pass onto the shell geometry computed during that
/// pass. GPU rectangles are rebuilt before submission; this affine rebase
/// keeps egui-rendered text on that same geometry revision without replaying
/// pointer input through a second UI pass. Text belonging to a removed panel
/// is deliberately omitted, and a newly added panel receives text on the next
/// display-list rebuild.
fn remap_genomic_texts_for_layout(
    texts: &[rgv_layout::TextPrimitive],
    previous: &ui::WorkspaceLayoutSnapshot,
    current: &ui::WorkspaceLayoutSnapshot,
) -> Vec<rgv_layout::TextPrimitive> {
    if previous == current {
        return texts.to_vec();
    }
    let vertical_delta = current.track_viewport.top() - previous.track_viewport.top();
    texts
        .iter()
        .filter_map(|text| {
            let previous_panel = previous
                .panels
                .iter()
                .find(|panel| text.x >= panel.canvas.left() && text.x <= panel.canvas.right())?;
            let current_panel = current.panel(previous_panel.panel_id)?;
            let fraction = if previous_panel.canvas.width() > f32::EPSILON {
                ((text.x - previous_panel.canvas.left()) / previous_panel.canvas.width())
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut remapped = text.clone();
            remapped.x = current_panel.canvas.left() + fraction * current_panel.canvas.width();
            remapped.y += vertical_delta;
            Some(remapped)
        })
        .collect()
}

impl Viewer {
    fn genome_overview_for_panel(
        &mut self,
        panel_id: PanelId,
        contig_name: &str,
        contig_length: u64,
    ) -> ui::GenomeOverviewData {
        let key = GenomeOverviewCacheKey {
            contig_name: contig_name.to_owned(),
            contig_length,
            tracks: self
                .session
                .alignment_tracks
                .iter()
                .map(|track| GenomeOverviewTrackCacheKey {
                    track_id: track.config.id,
                    index_overview_identity: track
                        .index_overview
                        .as_ref()
                        .map(|overview| Arc::as_ptr(overview) as usize),
                    summary_revision: self
                        .presented_alignment_lane(panel_id, track.config.id)
                        .and_then(|presented| presented.lane.snapshot.as_ref())
                        .map(|snapshot| snapshot.summary_revision()),
                })
                .collect(),
        };
        if let Some(cached) = self.genome_overview_cache.get(&panel_id)
            && cached.key == key
        {
            return cached.data.clone();
        }

        let mut genome_overview = ui::GenomeOverviewData::default();
        let mut scene_overview = ui::GenomeOverviewData::default();
        for track in &self.session.alignment_tracks {
            let index_bins = track
                .dictionary
                .as_ref()
                .and_then(|dictionary| dictionary.id_by_name(contig_name))
                .and_then(|contig| {
                    track
                        .index_overview
                        .as_ref()
                        .and_then(|overview| overview.contig_bins(contig))
                });
            if let Some(index_bins) = index_bins {
                genome_overview.add_index_density(index_bins);
            }
            if let Some(snapshot) = self
                .presented_alignment_lane(panel_id, track.config.id)
                .and_then(|presented| presented.lane.snapshot.as_ref())
            {
                let interval = snapshot.prepared_interval();
                scene_overview.add_depths(
                    contig_length,
                    interval,
                    snapshot.summaries().overview_depths().iter().copied(),
                );
            }
        }
        scene_overview.normalize_logarithmically();
        genome_overview.merge_max(&scene_overview);
        self.genome_overview_recomputations = self.genome_overview_recomputations.saturating_add(1);
        self.genome_overview_cache.insert(
            panel_id,
            CachedGenomeOverview {
                key,
                data: genome_overview.clone(),
            },
        );
        self.diagnostics.event(
            "ui",
            "genome_overview_recomputed",
            serde_json::json!({
                "panel_id": panel_id.get(),
                "contig": contig_name,
                "recomputation_count": self.genome_overview_recomputations,
            }),
        );
        genome_overview
    }

    pub(super) fn render_shell_frame(
        &mut self,
        raw_input: egui::RawInput,
    ) -> (egui::FullOutput, Duration) {
        let mut shell_preparation_started = Instant::now();
        let snapshot = self.prepare_frame_snapshot();
        let inspect = self.prepare_inspector_draft();
        let align = self.prepare_alignment_settings();
        let tracks = self.prepare_track_controls(align.primary_alignment_id);
        let commands = self.prepare_command_draft();
        let layout = self.prepare_layout(
            snapshot.active_panel_id,
            &align.bulk_alignment_ids,
            align.bulk_alignment_pane,
        );
        let nav = self.prepare_navigation_draft();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellPreparation,
            shell_preparation_started.elapsed(),
        );
        let panel_contexts = self.prepare_panel_contexts();
        shell_preparation_started = Instant::now();
        let lists = self.prepare_workspace_lists_draft();
        let (properties_expanded_id, properties_expanded) = self.prepare_properties_panel_state();
        let (alignment_context_menu, alignment_context_menu_view) =
            self.prepare_alignment_context_menu();
        let mut frame = ShellFrame {
            snapshot,
            inspect,
            align,
            tracks,
            commands,
            layout,
            nav,
            lists,
            panel_contexts,
            properties_expanded_id,
            properties_expanded,
            alignment_context_menu,
            alignment_context_menu_view,
            alignment_context_action: None,
            feature_context_menu: self.feature_context_menu.clone(),
            gene_transcripts_action: None,
        };
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellPreparation,
            shell_preparation_started.elapsed(),
        );
        let shell_ui_started = Instant::now();
        let full_output = self
            .egui_context
            .run_ui(raw_input, |ui| self.show_shell_ui(ui, &mut frame));
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellUiRun,
            shell_ui_started.elapsed(),
        );
        let repaint_delay = self.apply_shell_frame(frame, &full_output);
        (full_output, repaint_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_layout::Rgba;

    fn text_layout(
        workspace_right: f32,
        cytobands: ui::CytobandAvailability,
        revision: u64,
    ) -> ui::WorkspaceLayoutSnapshot {
        let workspace =
            egui::Rect::from_min_max(egui::pos2(0.0, 62.0), egui::pos2(workspace_right, 776.0));
        ui::WorkspaceLayoutSnapshot::compute(
            ui::WorkspaceLayoutInput::new(
                workspace,
                egui::Rect::from_min_max(
                    egui::pos2(workspace_right, 62.0),
                    egui::pos2(1_280.0, 776.0),
                ),
                true,
                vec![ui::WorkspacePanelLayoutInput {
                    panel_id: PanelId::new(1),
                    contig_name: "chr1".to_owned(),
                    contig_length: 3_000_000,
                    cytobands,
                }],
                ui::GenomeContextMetrics::default(),
                2.0,
                1.0,
            ),
            revision,
        )
    }

    #[test]
    fn same_frame_genomic_text_rebases_to_the_new_committed_layout() {
        let previous = text_layout(1_250.0, ui::CytobandAvailability::Absent, 1);
        let current = text_layout(940.0, ui::CytobandAvailability::Present, 2);
        let old_canvas = previous.panels[0].canvas;
        let new_canvas = current.panels[0].canvas;
        let original = rgv_layout::TextPrimitive {
            x: old_canvas.left() + old_canvas.width() * 0.25,
            y: previous.track_viewport.top() + 40.0,
            text: "A".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        };

        let remapped = remap_genomic_texts_for_layout(&[original], &previous, &current);

        assert_eq!(remapped.len(), 1);
        assert!((remapped[0].x - (new_canvas.left() + new_canvas.width() * 0.25)).abs() < 0.01);
        assert!((remapped[0].y - (current.track_viewport.top() + 40.0)).abs() < 0.01);
    }

    #[test]
    fn same_frame_text_for_a_non_panel_region_is_not_left_interactive_or_visible() {
        let previous = text_layout(1_250.0, ui::CytobandAvailability::Absent, 1);
        let current = text_layout(940.0, ui::CytobandAvailability::Absent, 2);
        let outside = rgv_layout::TextPrimitive {
            x: previous.track_gutter.center().x,
            y: previous.track_viewport.top() + 20.0,
            text: "stale".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        };

        assert!(remap_genomic_texts_for_layout(&[outside], &previous, &current).is_empty());
    }

    #[test]
    fn pane_snapshot_supplies_section_chrome_bounds_without_a_scroll_cache() {
        let mut layout = ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation);
        layout.pane_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 220.0), egui::pos2(800.0, 540.0));

        assert_eq!(pane_vertical_bounds(&layout, 100.0), (220.0, 540.0));
        assert_eq!(
            pane_vertical_bounds(
                &ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation),
                100.0,
            ),
            (100.0, 100.0)
        );
    }

    #[test]
    fn read_scrollbar_rail_uses_live_allocation_instead_of_a_stale_scene_clip() {
        let full = egui::Rect::from_min_max(egui::pos2(980.0, 100.0), egui::pos2(1_000.0, 700.0));
        let live_allocation =
            egui::Rect::from_min_max(egui::pos2(0.0, 260.0), egui::pos2(960.0, 650.0));
        let rail = read_panel_scrollbar_rect(full, full.left(), live_allocation);

        assert_eq!(rail.top(), 260.0);
        assert_eq!(rail.bottom(), 650.0);
        assert_eq!(rail.left(), full.left());
        assert_eq!(rail.right(), full.right());
        assert!(
            rail.contains(egui::pos2(990.0, 600.0)),
            "the lower live panel remains interactive even if a prior scene clip ended earlier"
        );
    }

    #[test]
    fn live_read_allocation_keeps_an_empty_rail_when_scene_cache_is_absent() {
        let track_id = TrackId::new(91);
        let allocation = egui::Rect::from_min_max(egui::pos2(0.0, 240.0), egui::pos2(900.0, 660.0));
        let state = ReadPanelScrollState {
            scroll_points: 480.0,
            content_bottom_points: 2_000.0,
            latent_scroll_fraction: Some(0.4),
            ..ReadPanelScrollState::default()
        };

        let draft =
            read_panel_scrollbar_draft(track_id, allocation, state, 1.0, None, None, 0, false);

        assert_eq!(draft.track_id, track_id);
        assert_eq!(draft.allocation, allocation);
        assert_eq!(draft.maximum, 0.0);
        assert_eq!(draft.offset, 0.0);
        assert!(draft.overview.bins.is_empty());
        assert_eq!(draft.preview_fraction, Some(0.4));
    }
}
