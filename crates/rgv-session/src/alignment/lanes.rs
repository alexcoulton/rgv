//! Alignment scene lane demand from the device-free lane layout: which derived lanes and which
//! read clip a panel shows for one track, and the preparation camera the scene source is asked
//! for. Moved from the desktop's pane-layout lookups; they read
//! [`PanelLaneLayout`](crate::lane_layout::PanelLaneLayout) instead of widget-toolkit rectangles.

use crate::alignment::track::{
    ActiveFilters, alignment_scene_coverage_lane_options,
    alignment_scene_modification_lane_options, alignment_scene_sashimi_group,
    alignment_scene_sashimi_lane_options, alignment_scene_selected_position,
};
use crate::lane_layout::{PaneLanes, PanelLaneLayout, SublaneKind};
use crate::panels::Panel;
use crate::session::Session;
use anyhow::{Context as _, Result};
use rgv_alignment_scene::{
    AlignmentCamera, AlignmentCoherenceGroupId, AlignmentDerivedLaneCapture,
    AlignmentDerivedLaneOptions, AlignmentPanelLaneCapture, AlignmentPanelSceneError,
    AlignmentSceneCamera, CpuClipRect,
};
use rgv_core::{ContigId, DetailLevel, PanelId, SessionFilters, TrackId};

/// Tallest read row a density setting may produce, in logical points.
pub const MAX_ALIGNMENT_ROW_HEIGHT: f32 = 64.0;

/// Track id the primary alignment settings answer to while no alignment source is open.
pub const SYNTHETIC_ALIGNMENT_TRACK_ID: TrackId = TrackId::new(0);

/// Resolves scene-owned derived lanes from the already-final pane layout. The lane's stable
/// [`SublaneKind`] identity—not its user-facing label—selects semantics, and the returned clip
/// is intersected with the final scrolled item clip used by gutter hit testing.
#[allow(clippy::too_many_arguments)]
pub fn alignment_scene_derived_lane_captures(
    layout: &PaneLanes,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
    filters: &SessionFilters,
    reference_backed_alleles: bool,
    sashimi_group: Option<AlignmentCoherenceGroupId>,
    selected_pileup_position: Option<u64>,
    selected_modification_position: Option<u64>,
) -> Result<Vec<AlignmentDerivedLaneCapture>, AlignmentPanelSceneError> {
    let Some(item) = layout
        .lanes
        .iter()
        .find(|item| item.is_alignment(track_id, rgv_core::TrackChannel::Annotation))
    else {
        return Ok(Vec::new());
    };
    if !item.effective_content_visible || item.clip_rect.height() <= 0.0 {
        return Ok(Vec::new());
    }
    let mut captures = Vec::new();
    for subtrack in &item.sublanes {
        if subtrack.collapsed {
            continue;
        }
        let Some(kind) = subtrack.kind else {
            continue;
        };
        let top = (subtrack.top - layout.scroll_points).max(item.clip_rect.top);
        let bottom = (subtrack.bottom - layout.scroll_points).min(item.clip_rect.bottom);
        if bottom <= top {
            continue;
        }
        let options = match kind {
            SublaneKind::Coverage if filters.show_coverage => {
                AlignmentDerivedLaneOptions::Coverage(alignment_scene_coverage_lane_options(
                    filters,
                    reference_backed_alleles,
                    selected_pileup_position,
                ))
            }
            SublaneKind::Sashimi if filters.show_junctions => {
                let Some(options) = alignment_scene_sashimi_lane_options(filters, sashimi_group)
                else {
                    // A Shared lane cannot truthfully enter an epoch without its explicit group.
                    continue;
                };
                AlignmentDerivedLaneOptions::Sashimi(options)
            }
            SublaneKind::Modifications if filters.show_modification_frequency => {
                AlignmentDerivedLaneOptions::Modifications(
                    alignment_scene_modification_lane_options(
                        filters,
                        selected_modification_position,
                    ),
                )
            }
            SublaneKind::Reference
            | SublaneKind::Reads
            | SublaneKind::Coverage
            | SublaneKind::Sashimi
            | SublaneKind::Modifications => continue,
        };
        captures.push(AlignmentDerivedLaneCapture::new(
            CpuClipRect {
                x: horizontal_x,
                y: f64::from(top),
                width: horizontal_width,
                height: f64::from(bottom - top),
            },
            options,
        )?);
    }
    Ok(captures)
}

/// Actual read-pixel clip from the canonical Reads pane. A hidden lane returns `None`; callers
/// must not manufacture a read render camera merely to keep summary acquisition alive.
pub fn alignment_scene_read_clip(
    layout: &PaneLanes,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
) -> Option<CpuClipRect> {
    let item = layout
        .lanes
        .iter()
        .find(|item| item.is_alignment(track_id, rgv_core::TrackChannel::Reads))?;
    let reads_visible = item.effective_content_visible
        && item
            .sublanes
            .iter()
            .any(|subtrack| subtrack.kind == Some(SublaneKind::Reads) && !subtrack.collapsed);
    let content = reads_visible.then_some(item.content_rect).flatten()?;
    (content.height() > 0.0).then_some(CpuClipRect {
        x: horizontal_x,
        y: f64::from(content.top),
        width: horizontal_width,
        height: f64::from(content.height()),
    })
}

impl<P: Panel> Session<P> {
    /// The first alignment track, or the synthetic id while none is open.
    pub fn primary_alignment_track_id(&self) -> TrackId {
        self.alignment_tracks
            .first()
            .map_or(SYNTHETIC_ALIGNMENT_TRACK_ID, |track| track.config.id)
    }

    pub fn primary_alignment_filters(&self) -> &SessionFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.filters, |track| &track.config.filters)
    }

    pub fn primary_active_filters(&self) -> &ActiveFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.active_filters, |track| {
                &track.active_filters
            })
    }

    /// The primary track's filters with the last successfully applied filter clauses.
    pub fn live_session_filters(&self) -> SessionFilters {
        let mut filters = self.primary_alignment_filters().clone();
        filters.minimum_mapping_quality = self
            .primary_active_filters()
            .alignment
            .minimum_mapping_quality;
        filters.include_secondary = self.primary_active_filters().include_secondary;
        filters.include_supplementary = self.primary_active_filters().include_supplementary;
        // The editor may contain an unapplied or invalid draft. The canonical
        // configuration already contains the last successfully applied clauses.
        filters
    }

    pub fn alignment_filters_for_track(&self, id: TrackId) -> SessionFilters {
        if id == self.primary_alignment_track_id() {
            return self.live_session_filters();
        }
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == id)
            .map_or_else(SessionFilters::default, |track| {
                track.config.filters.clone()
            })
    }

    pub fn alignment_density_for_panel_track(&self, panel_id: PanelId, track_id: TrackId) -> f32 {
        self.panels
            .get(panel_id)
            .and_then(|panel| panel.state().read_panel_scrolls.get(&track_id))
            .and_then(|state| state.density)
            .unwrap_or_else(|| self.alignment_filters_for_track(track_id).alignment_density)
    }

    /// Resolves the complete live presentation demand for one track from the final lane layout.
    /// A read camera exists only for actual read pixels; a derived-only track receives a genomic
    /// preparation camera with no fabricated rows or clip.
    pub fn alignment_scene_lane_capture_for_panel_track(
        &self,
        layout: &PanelLaneLayout,
        panel_id: PanelId,
        track_id: TrackId,
        contig: ContigId,
        contig_length: u64,
        reference_backed_alleles: bool,
    ) -> Result<Option<AlignmentPanelLaneCapture>> {
        let Some(panel) = self.panels.get(panel_id) else {
            return Ok(None);
        };
        let state = panel.state();
        if state.viewport.detail_level() == DetailLevel::Overview {
            return Ok(None);
        }
        let panel_rect = layout.canvas;
        if panel_rect.width() <= 0.0 {
            return Ok(None);
        }
        let Some(track) = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
        else {
            return Ok(None);
        };
        let filters = self.alignment_filters_for_track(track_id);
        let sashimi_group = alignment_scene_sashimi_group(&filters);
        let selected_track = panel.selected_alignment_track();
        let selected_pileup_position = alignment_scene_selected_position(
            selected_track,
            track_id,
            panel.selected_pileup_position(),
        );
        let selected_modification_position = alignment_scene_selected_position(
            selected_track,
            track_id,
            panel.selected_modification_position(),
        );
        let derived_lanes = alignment_scene_derived_lane_captures(
            &layout.annotation,
            track_id,
            f64::from(panel_rect.left),
            f64::from(panel_rect.width()),
            &filters,
            reference_backed_alleles,
            sashimi_group,
            selected_pileup_position,
            selected_modification_position,
        )?;
        let viewport = state
            .viewport
            .remapped_contig(contig, contig_length)
            .context("could not remap the alignment scene camera")?;
        let viewport = viewport
            .resized(f64::from(panel_rect.width()))
            .ok()
            .context("could not reconcile the alignment scene camera with the live panel width")?;
        let read_camera = alignment_scene_read_clip(
            &layout.reads,
            track_id,
            f64::from(panel_rect.left),
            f64::from(panel_rect.width()),
        )
        .map(|clip| {
            let density = self.alignment_density_for_panel_track(panel_id, track_id);
            let row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            let read_height = (5.0 * density).clamp(1.5, (row_height - 1.0).max(1.5));
            let row_scroll = state
                .read_panel_scroll_state(track_id)
                .effective_scroll_points()
                / row_height;
            AlignmentCamera::from_viewport(
                viewport,
                f64::from(row_scroll),
                f64::from(row_height),
                f64::from(read_height),
                clip,
            )
        })
        .transpose()
        .context("could not construct the alignment read camera")?;
        if read_camera.is_none() && derived_lanes.is_empty() {
            return Ok(None);
        }
        let preparation_camera = read_camera.map_or_else(
            || AlignmentSceneCamera::without_visible_rows(viewport),
            AlignmentSceneCamera::from,
        );
        AlignmentPanelLaneCapture::new(
            track.config.id,
            preparation_camera,
            read_camera,
            derived_lanes,
        )
        .map(Some)
        .map_err(Into::into)
    }
}
