//! Desktop side of alignment tracks: the pane-layout lookups that need the window's layout,
//! and the `Viewer` operations over the session's alignment controllers. The controllers,
//! projections, filters and scene-contract decisions live in `rgv_session::alignment::track`.

pub(crate) use rgv_session::alignment::track::*;

use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY, SYNTHETIC_ALIGNMENT_TRACK_ID, Viewer, ui};
use anyhow::Result;
use rgv_alignment_scene::{
    AlignmentCoherenceGroupId, AlignmentDerivedLaneCapture, AlignmentDerivedLaneOptions,
    AlignmentPanelSceneError, CpuClipRect, ReferenceRevision,
};
#[cfg(test)]
use rgv_core::{DetailLevel, MAXIMUM_BASE_MARK_BASES_PER_PIXEL};
use rgv_core::{PanelId, ReferenceDictionary, SessionFilters, TrackId, TrackSource};
use rgv_hts::ModificationCode;
#[cfg(test)]
use rgv_layout::AlignmentColorMode;
use rgv_layout::MAX_HIDDEN_MODIFICATION_CODES;

/// Resolves scene-owned derived lanes from the already-final pane layout. The lane's stable
/// `AlignmentSubtrack` identity—not its user-facing label—selects semantics, and the returned clip
/// is intersected with the final scrolled item clip used by gutter hit testing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn alignment_scene_derived_lane_captures(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
    filters: &SessionFilters,
    reference_backed_alleles: bool,
    sashimi_group: Option<AlignmentCoherenceGroupId>,
    selected_pileup_position: Option<u64>,
    selected_modification_position: Option<u64>,
) -> Result<Vec<AlignmentDerivedLaneCapture>, AlignmentPanelSceneError> {
    let Some(item) = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Annotation
    }) else {
        return Ok(Vec::new());
    };
    if !item.effective_content_visible || item.clip_rect.height() <= 0.0 {
        return Ok(Vec::new());
    }
    let mut captures = Vec::new();
    for subtrack in &item.frame.subtracks {
        if subtrack.collapsed {
            continue;
        }
        let Some(kind) = subtrack.alignment_subtrack else {
            continue;
        };
        let top = (subtrack.top - layout.scroll_points).max(item.clip_rect.top());
        let bottom = (subtrack.bottom - layout.scroll_points).min(item.clip_rect.bottom());
        if bottom <= top {
            continue;
        }
        let options = match kind {
            ui::AlignmentSubtrack::Coverage if filters.show_coverage => {
                AlignmentDerivedLaneOptions::Coverage(alignment_scene_coverage_lane_options(
                    filters,
                    reference_backed_alleles,
                    selected_pileup_position,
                ))
            }
            ui::AlignmentSubtrack::Sashimi if filters.show_junctions => {
                let Some(options) = alignment_scene_sashimi_lane_options(filters, sashimi_group)
                else {
                    // A Shared lane cannot truthfully enter an epoch without its explicit group.
                    continue;
                };
                AlignmentDerivedLaneOptions::Sashimi(options)
            }
            ui::AlignmentSubtrack::Modifications if filters.show_modification_frequency => {
                AlignmentDerivedLaneOptions::Modifications(
                    alignment_scene_modification_lane_options(
                        filters,
                        selected_modification_position,
                    ),
                )
            }
            ui::AlignmentSubtrack::Reference
            | ui::AlignmentSubtrack::Reads
            | ui::AlignmentSubtrack::Coverage
            | ui::AlignmentSubtrack::Sashimi
            | ui::AlignmentSubtrack::Modifications => continue,
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
pub(crate) fn alignment_scene_read_clip(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
) -> Option<CpuClipRect> {
    let item = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Reads
    })?;
    let reads_visible = item.effective_content_visible
        && item.frame.subtracks.iter().any(|subtrack| {
            subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Reads) && !subtrack.collapsed
        });
    let content = reads_visible.then_some(item.content_rect).flatten()?;
    (content.height() > 0.0).then_some(CpuClipRect {
        x: horizontal_x,
        y: f64::from(content.top()),
        width: horizontal_width,
        height: f64::from(content.height()),
    })
}

impl Viewer {
    pub(crate) fn compatible_alignment_reference_revision(
        &self,
        alignment: &ReferenceDictionary,
    ) -> Option<ReferenceRevision> {
        self.session
            .compatible_alignment_reference_revision(alignment)
    }

    pub(crate) fn alignment_reference_incompatibility(
        &self,
        alignment: &ReferenceDictionary,
    ) -> Option<String> {
        let reference = self.session.reference_dictionary.as_ref()?;
        match alignment_reference_compatibility(alignment, reference, &self.session.contig_aliases)
        {
            AlignmentReferenceCompatibility::Compatible => None,
            AlignmentReferenceCompatibility::Incompatible(reason) => Some(reason),
        }
    }

    pub(crate) fn primary_alignment_filters(&self) -> &SessionFilters {
        self.session
            .alignment_tracks
            .first()
            .map_or(&self.session.empty_alignment.filters, |track| {
                &track.config.filters
            })
    }

    pub(crate) fn primary_alignment_filters_mut(&mut self) -> &mut SessionFilters {
        self.session
            .alignment_tracks
            .first_mut()
            .map_or(&mut self.session.empty_alignment.filters, |track| {
                &mut track.config.filters
            })
    }

    pub(crate) fn set_alignment_density_for_track(
        &mut self,
        track_id: TrackId,
        density: f32,
    ) -> bool {
        let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
        let filters = if track_id == SYNTHETIC_ALIGNMENT_TRACK_ID
            && self.session.alignment_tracks.is_empty()
        {
            &mut self.session.empty_alignment.filters
        } else if let Some(track) = self
            .session
            .alignment_tracks
            .iter_mut()
            .find(|track| track.config.id == track_id)
        {
            &mut track.config.filters
        } else {
            return false;
        };
        let changed = (filters.alignment_density - density).abs() > f32::EPSILON;
        filters.alignment_density = density;
        for panel in &mut self.session.panels.panels {
            if let Some(state) = panel.read_panel_scrolls.get_mut(&track_id) {
                state.density = None;
            }
        }
        if changed {
            self.invalidate_navigation_contract("read density changed");
        }
        changed
    }

    pub(crate) fn alignment_density_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> f32 {
        self.session
            .panels
            .get(panel_id)
            .and_then(|panel| panel.read_panel_scrolls.get(&track_id))
            .and_then(|state| state.density)
            .unwrap_or_else(|| self.alignment_filters_for_track(track_id).alignment_density)
    }

    pub(crate) fn set_read_panel_density(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        density: f32,
    ) -> bool {
        let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
        let default_density = self.alignment_filters_for_track(track_id).alignment_density;
        let Some(panel) = self.session.panels.get_mut(panel_id) else {
            return false;
        };
        let state = panel.read_panel_scrolls.entry(track_id).or_default();
        let previous = state.density.unwrap_or(default_density);
        state.density = Some(density);
        let changed = (previous - density).abs() > f32::EPSILON;
        if changed {
            self.invalidate_navigation_contract("read density changed");
        }
        changed
    }

    pub(crate) fn primary_filter_editor(&self) -> &FilterEditor {
        self.session
            .alignment_tracks
            .first()
            .map_or(&self.session.empty_alignment.filter_editor, |track| {
                &track.filter_editor
            })
    }

    pub(crate) fn primary_filter_editor_mut(&mut self) -> &mut FilterEditor {
        self.session
            .alignment_tracks
            .first_mut()
            .map_or(&mut self.session.empty_alignment.filter_editor, |track| {
                &mut track.filter_editor
            })
    }

    pub(crate) fn primary_active_filters(&self) -> &ActiveFilters {
        self.session
            .alignment_tracks
            .first()
            .map_or(&self.session.empty_alignment.active_filters, |track| {
                &track.active_filters
            })
    }

    pub(crate) fn primary_active_filters_mut(&mut self) -> &mut ActiveFilters {
        self.session
            .alignment_tracks
            .first_mut()
            .map_or(&mut self.session.empty_alignment.active_filters, |track| {
                &mut track.active_filters
            })
    }

    pub(crate) fn primary_alignment_name(&self) -> &str {
        self.session
            .alignment_tracks
            .first()
            .map_or(&self.session.empty_alignment.name, |track| {
                &track.config.name
            })
    }

    pub(crate) fn primary_alignment_visible(&self) -> bool {
        self.session
            .alignment_tracks
            .first()
            .map_or(self.session.empty_alignment.visible, |track| {
                track.config.visible
            })
    }

    pub(crate) fn primary_alignment_collapsed(&self) -> bool {
        self.session
            .alignment_tracks
            .first()
            .map_or(self.session.empty_alignment.collapsed, |track| {
                track.config.collapsed
            })
    }

    pub(crate) fn primary_alignment_limit(&self) -> usize {
        self.session
            .alignment_tracks
            .first()
            .map_or(self.session.empty_alignment.maximum_alignments, |track| {
                track.config.maximum_alignments
            })
    }

    pub(crate) fn primary_alignment_source(&self) -> Option<&TrackSource> {
        self.session
            .alignment_tracks
            .first()
            .map(|track| &track.config.source)
    }

    pub(crate) fn primary_hidden_modification_codes(&self) -> Vec<ModificationCode> {
        self.primary_alignment_filters()
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value))
            .take(MAX_HIDDEN_MODIFICATION_CODES)
            .collect()
    }

    pub(crate) fn set_primary_alignment_name(&mut self, name: String) {
        if let Some(track) = self.session.alignment_tracks.first_mut() {
            track.config.name = name;
        } else {
            self.session.empty_alignment.name = name;
        }
    }

    pub(crate) fn set_primary_alignment_visible(&mut self, visible: bool) {
        if let Some(track) = self.session.alignment_tracks.first_mut() {
            track.config.visible = visible;
        } else {
            self.session.empty_alignment.visible = visible;
        }
    }

    pub(crate) fn set_primary_alignment_collapsed(&mut self, collapsed: bool) {
        if let Some(track) = self.session.alignment_tracks.first_mut() {
            track.config.collapsed = collapsed;
        } else {
            self.session.empty_alignment.collapsed = collapsed;
        }
    }

    pub(crate) fn set_primary_alignment_limit(&mut self, maximum_alignments: usize) {
        let (workspace_limit, maximum_alignments) = expanded_workspace_alignment_limit(
            self.session.workspace_alignment_limit,
            maximum_alignments,
        );
        self.session.workspace_alignment_limit = workspace_limit;
        if let Some(track) = self.session.alignment_tracks.first_mut() {
            track.config.maximum_alignments = maximum_alignments;
        } else {
            self.session.empty_alignment.maximum_alignments = maximum_alignments;
        }
    }
}

#[cfg(test)]
pub(crate) fn alignment_query_needs_reference(
    color_mode: AlignmentColorMode,
    detail_level: DetailLevel,
    bases_per_pixel: f64,
) -> bool {
    bases_per_pixel <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
        || (detail_level == DetailLevel::Reads && alignment_color_mode_needs_reference(color_mode))
}

#[cfg(test)]
pub(crate) fn updated_alignment_selection(
    selected: &[TrackId],
    id: TrackId,
    additive: bool,
) -> Vec<TrackId> {
    if !additive {
        return vec![id];
    }
    let mut next = selected.to_vec();
    if let Some(index) = next.iter().position(|candidate| *candidate == id) {
        next.remove(index);
    } else {
        next.push(id);
    }
    next
}

#[cfg(test)]
pub(crate) fn alignment_range_selection(
    track_order: &[TrackId],
    anchor: TrackId,
    clicked: TrackId,
) -> Vec<TrackId> {
    let Some(anchor_index) = track_order.iter().position(|id| *id == anchor) else {
        return vec![clicked];
    };
    let Some(clicked_index) = track_order.iter().position(|id| *id == clicked) else {
        return vec![clicked];
    };
    let (start, end) = if anchor_index <= clicked_index {
        (anchor_index, clicked_index)
    } else {
        (clicked_index, anchor_index)
    };
    track_order[start..=end].to_vec()
}

#[cfg(test)]
pub(crate) fn move_ordered_item<T>(
    items: &mut Vec<T>,
    old_index: usize,
    target_index: usize,
) -> bool {
    if old_index >= items.len() {
        return false;
    }
    let new_index = target_index.min(items.len().saturating_sub(1));
    if old_index == new_index {
        return false;
    }
    let item = items.remove(old_index);
    items.insert(new_index, item);
    true
}
