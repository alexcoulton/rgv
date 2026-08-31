use super::*;

pub(crate) const INDEX_ONLY_READ_NOTICE: &str = "zoom in to see read data";

/// Quantizes horizontal scale into four-fold presentation bands whose boundaries are centred at
/// 2×4ⁿ bases per pixel. A normal two-fold zoom therefore usually remains in one class, while a
/// genuinely different read-packing scale receives independent stable row history.
pub(crate) fn alignment_row_scale_class(bases_per_pixel: f64) -> u8 {
    if !bases_per_pixel.is_finite() || bases_per_pixel <= 0.0 {
        return 0;
    }
    let exponent = bases_per_pixel.log2().floor().clamp(-126.0, 126.0) as i32;
    let class = (exponent - 1).div_euclid(2) + 64;
    u8::try_from(class.clamp(0, i32::from(u8::MAX))).unwrap_or_default()
}

#[cfg(test)]
mod scale_class_tests {
    use super::alignment_row_scale_class;

    #[test]
    fn row_scale_classes_keep_motivating_two_fold_zoom_together() {
        assert_eq!(
            alignment_row_scale_class(161.0),
            alignment_row_scale_class(322.0)
        );
        assert_ne!(
            alignment_row_scale_class(6.5),
            alignment_row_scale_class(161.0)
        );
        assert_ne!(
            alignment_row_scale_class(127.0),
            alignment_row_scale_class(128.0)
        );
    }
}

pub(crate) fn index_only_read_notice(mapped_record_count: Option<u64>) -> String {
    mapped_record_count.map_or_else(
        || INDEX_ONLY_READ_NOTICE.to_owned(),
        |count| {
            format!(
                "{count} indexed alignment{} on this contig — zoom in to view individual reads",
                if count == 1 { "" } else { "s" }
            )
        },
    )
}

/// Builds the visible read-pane empty state used while chromosome-scale
/// overview is served from the alignment index alone. This deliberately does
/// not depend on an alignment query or layout: overview mode creates neither.
pub(crate) fn index_only_read_display(
    width: f32,
    top: f32,
    bottom: f32,
    mapped_record_count: Option<u64>,
) -> DisplayList {
    let mut display = DisplayList::default();
    let notice_top = top + 5.0;
    let notice_height = 32.0_f32.min((bottom - notice_top - 5.0).max(0.0));
    let notice_width = (width - 24.0).max(0.0);
    if notice_width <= 0.0 || notice_height <= 0.0 {
        return display;
    }
    display.rectangles.push(RectPrimitive {
        x: 12.0,
        y: notice_top,
        width: notice_width,
        height: notice_height,
        color: Rgba::new(0.92, 0.94, 0.97, 0.95),
        object_id: 0,
        shape: Default::default(),
    });
    display.rectangles.push(RectPrimitive {
        x: 12.0,
        y: notice_top,
        width: 3.0_f32.min(notice_width),
        height: notice_height,
        color: Rgba::new(0.25, 0.45, 0.72, 0.90),
        object_id: 0,
        shape: Default::default(),
    });
    display.texts.push(rgv_layout::TextPrimitive {
        x: width * 0.5,
        y: notice_top + notice_height * 0.5,
        text: index_only_read_notice(mapped_record_count),
        font_size: 12.0,
        color: Rgba::new(0.25, 0.29, 0.35, 1.0),
    });
    display
}

/// Non-interactive read-pane status used while no current query may be
/// presented. It intentionally owns no object IDs, so Checking, failure, and
/// unavailable states cannot expose stale read interaction.
pub(crate) fn read_state_display(
    width: f32,
    top: f32,
    bottom: f32,
    message: impl Into<String>,
) -> DisplayList {
    let mut display = DisplayList::default();
    let notice_top = top + 5.0;
    let notice_height = 32.0_f32.min((bottom - notice_top - 5.0).max(0.0));
    let notice_width = (width - 24.0).max(0.0);
    if notice_width <= 0.0 || notice_height <= 0.0 {
        return display;
    }
    display.rectangles.push(RectPrimitive {
        x: 12.0,
        y: notice_top,
        width: notice_width,
        height: notice_height,
        color: Rgba::new(0.94, 0.95, 0.97, 0.96),
        object_id: 0,
        shape: Default::default(),
    });
    display.rectangles.push(RectPrimitive {
        x: 12.0,
        y: notice_top,
        width: 3.0_f32.min(notice_width),
        height: notice_height,
        color: Rgba::new(0.43, 0.49, 0.59, 0.90),
        object_id: 0,
        shape: Default::default(),
    });
    display.texts.push(rgv_layout::TextPrimitive {
        x: width * 0.5,
        y: notice_top + notice_height * 0.5,
        text: message.into(),
        font_size: 12.0,
        color: Rgba::new(0.27, 0.30, 0.36, 1.0),
    });
    display
}

#[derive(Clone, Copy, Debug)]
struct AlignmentAnnotationGeometry {
    coverage_top: f32,
    coverage_height: f32,
    junction_top: f32,
    junction_height: f32,
    modification_top: f32,
    modification_height: f32,
}

fn apply_alignment_annotation_height(
    options: &mut AlignmentAnnotationGeometry,
    frame_top: f32,
    natural_bottom: f32,
    requested_height: Option<f32>,
) -> f32 {
    let Some(requested_height) = requested_height else {
        return natural_bottom;
    };
    let header_height = 22.0;
    let content_top = frame_top + header_height;
    let natural_content_height = (natural_bottom - content_top).max(1.0);
    let target_height = requested_height.clamp(80.0, 600.0);
    let target_content_height = (target_height - header_height).max(1.0);
    let scale = target_content_height / natural_content_height;
    let scale_top = |top: f32| content_top + (top - content_top) * scale;

    options.coverage_top = scale_top(options.coverage_top);
    options.coverage_height = (options.coverage_height * scale).max(8.0);
    options.junction_top = scale_top(options.junction_top);
    options.junction_height = (options.junction_height * scale).max(8.0);
    options.modification_top = scale_top(options.modification_top);
    options.modification_height = (options.modification_height * scale).max(8.0);
    frame_top + target_height
}

/// Query-independent annotation-lane layout for the unified scene.
///
/// Visibility and geometry are presentation contracts, so Coverage, Sashimi and modification
/// rows must exist before (and independently from) any source publication. Their eventual pixels
/// are projected from the captured scene snapshot; this function deliberately reads no
/// query-local evidence object and invents no data-availability decision from an empty summary.
pub(crate) fn alignment_scene_annotation_frame(
    config: &SessionAlignmentTrack,
    viewport: Viewport,
    frame_top: f32,
    requested_height: Option<f32>,
) -> TrackFrame {
    let filters = &config.filters;
    let heights = alignment_annotation_lane_heights(filters);
    let content_top = frame_top + 22.0;
    let mut lane_top = content_top;
    let mut options = AlignmentAnnotationGeometry {
        coverage_top: lane_top,
        coverage_height: heights.coverage,
        junction_top: lane_top,
        junction_height: heights.junction,
        modification_top: lane_top,
        modification_height: heights.modification,
    };
    lane_top += if filters.show_coverage {
        heights.coverage + 6.0
    } else {
        COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT
    };
    options.junction_top = lane_top;
    options.junction_height = heights.junction;
    lane_top += if filters.show_junctions {
        heights.junction + 6.0
    } else {
        COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT
    };
    options.modification_top = lane_top;
    options.modification_height = heights.modification;
    let modifications_supported = viewport.detail_level() != DetailLevel::Overview;
    lane_top += if filters.show_modification_frequency && modifications_supported {
        heights.modification + 8.0
    } else if modifications_supported {
        COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT
    } else {
        0.0
    };
    let frame_bottom = apply_alignment_annotation_height(
        &mut options,
        frame_top,
        lane_top.max(content_top),
        requested_height,
    );

    let mut subtracks = Vec::with_capacity(3);
    if filters.show_coverage {
        subtracks.push(TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Coverage),
            label: "Coverage".to_owned(),
            detail: None,
            top: options.coverage_top,
            bottom: options.coverage_top + options.coverage_height,
            collapsed: false,
            composite_member: None,
        });
    } else {
        subtracks.push(collapsed_alignment_subtrack(
            AlignmentSubtrack::Coverage,
            options.coverage_top,
        ));
    }
    if filters.show_junctions {
        subtracks.push(TrackSubtrack {
            alignment_subtrack: Some(AlignmentSubtrack::Sashimi),
            label: "Sashimi".to_owned(),
            detail: None,
            top: options.junction_top,
            bottom: options.junction_top + options.junction_height,
            collapsed: false,
            composite_member: None,
        });
    } else {
        subtracks.push(collapsed_alignment_subtrack(
            AlignmentSubtrack::Sashimi,
            options.junction_top,
        ));
    }
    if modifications_supported {
        if filters.show_modification_frequency {
            subtracks.push(TrackSubtrack {
                alignment_subtrack: Some(AlignmentSubtrack::Modifications),
                label: "Modifications".to_owned(),
                detail: None,
                top: options.modification_top,
                bottom: options.modification_top + options.modification_height,
                collapsed: false,
                composite_member: None,
            });
        } else {
            subtracks.push(collapsed_alignment_subtrack(
                AlignmentSubtrack::Modifications,
                options.modification_top,
            ));
        }
    }
    TrackFrame {
        key: TrackFrameKey::Alignment(config.id),
        channel: ui::TrackPane::Annotation,
        name: config.name.clone(),
        kind: TrackKind::Alignment,
        source_label: Some(config.source.redacted_label()),
        top: frame_top,
        bottom: frame_bottom.max(frame_top + 22.0),
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadPanelKey {
    Track(TrackId),
    Group(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReadPanelBounds {
    pub(crate) key: ReadPanelKey,
    pub(crate) top: f32,
    pub(crate) bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReadPanelContent {
    HeaderOnly,
    Expanded { preferred_height: Option<f32> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReadPanelSpec {
    pub(crate) key: ReadPanelKey,
    pub(crate) content: ReadPanelContent,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadPaneLayout {
    pub(crate) bounds: Vec<ReadPanelBounds>,
    pub(crate) content_bottom: f32,
}

impl ReadPanelSpec {
    pub(crate) const fn header(key: ReadPanelKey) -> Self {
        Self {
            key,
            content: ReadPanelContent::HeaderOnly,
        }
    }

    pub(crate) const fn expanded(key: ReadPanelKey, preferred_height: Option<f32>) -> Self {
        Self {
            key,
            content: ReadPanelContent::Expanded { preferred_height },
        }
    }
}

/// Pure lower-pane layout. A sole read panel occupies the complete pane; with
/// multiple rows, preferred heights are literal pixel sizes and unused space
/// remains bottom slack. If headers plus minimum content cannot fit, rows
/// retain their interaction minimum and `content_bottom` extends beyond the
/// pane; the caller owns the conventional parent overflow camera.
pub(crate) fn read_pane_layout(
    tracks: &[ReadPanelSpec],
    pane_top: f32,
    pane_bottom: f32,
) -> ReadPaneLayout {
    if tracks.is_empty() || pane_bottom <= pane_top {
        return ReadPaneLayout {
            bounds: Vec::new(),
            content_bottom: pane_top,
        };
    }
    const MINIMUM_EXPANDED_PANEL_HEIGHT: f32 = 40.0;

    let total = pane_bottom - pane_top;
    let header_total = tracks
        .iter()
        .filter(|spec| matches!(spec.content, ReadPanelContent::HeaderOnly))
        .count() as f32
        * READ_PANE_HEADER_HEIGHT;
    let expanded_count = tracks
        .iter()
        .filter(|spec| matches!(spec.content, ReadPanelContent::Expanded { .. }))
        .count();
    let explicit_total = tracks
        .iter()
        .filter_map(|spec| match spec.content {
            ReadPanelContent::Expanded {
                preferred_height: Some(height),
            } => Some(height.max(MINIMUM_EXPANDED_PANEL_HEIGHT)),
            ReadPanelContent::HeaderOnly
            | ReadPanelContent::Expanded {
                preferred_height: None,
            } => None,
        })
        .sum::<f32>();
    let flexible_count = tracks
        .iter()
        .filter(|spec| {
            matches!(
                spec.content,
                ReadPanelContent::Expanded {
                    preferred_height: None
                }
            )
        })
        .count();
    let flexible_default = if flexible_count == 0 {
        MINIMUM_EXPANDED_PANEL_HEIGHT
    } else {
        ((total - header_total - explicit_total).max(0.0) / flexible_count as f32)
            .max(MINIMUM_EXPANDED_PANEL_HEIGHT)
    };
    let mut desired_expanded_heights = tracks
        .iter()
        .filter_map(|spec| match spec.content {
            ReadPanelContent::HeaderOnly => None,
            ReadPanelContent::Expanded { preferred_height } => Some(
                preferred_height
                    .unwrap_or(flexible_default)
                    .max(MINIMUM_EXPANDED_PANEL_HEIGHT),
            ),
        })
        .collect::<Vec<_>>();
    if tracks.len() == 1 && expanded_count == 1 {
        // There is no sibling boundary to preserve or resize. Ignore a stale
        // saved height and let the only read panel follow the pane divider and
        // window size automatically.
        desired_expanded_heights[0] = total.max(MINIMUM_EXPANDED_PANEL_HEIGHT);
    }
    let minimum_total = header_total + expanded_count as f32 * MINIMUM_EXPANDED_PANEL_HEIGHT;
    let desired_total = header_total + desired_expanded_heights.iter().sum::<f32>();
    if desired_total > total && total >= minimum_total {
        let desired_extra = desired_expanded_heights
            .iter()
            .map(|height| height - MINIMUM_EXPANDED_PANEL_HEIGHT)
            .sum::<f32>();
        let available_extra = (total - minimum_total).max(0.0);
        if desired_extra > f32::EPSILON {
            for height in &mut desired_expanded_heights {
                *height = MINIMUM_EXPANDED_PANEL_HEIGHT
                    + (*height - MINIMUM_EXPANDED_PANEL_HEIGHT) * available_extra / desired_extra;
            }
        }
    }
    let mut expanded_index = 0_usize;
    let mut top = pane_top;
    let bounds = tracks
        .iter()
        .map(|spec| {
            let height = if matches!(spec.content, ReadPanelContent::Expanded { .. }) {
                let height = desired_expanded_heights
                    .get(expanded_index)
                    .copied()
                    .unwrap_or(0.0);
                expanded_index += 1;
                height
            } else {
                READ_PANE_HEADER_HEIGHT
            };
            let bottom = top + height;
            let bounds = ReadPanelBounds {
                key: spec.key,
                top,
                bottom,
            };
            top = bottom;
            bounds
        })
        .collect();
    ReadPaneLayout {
        bounds,
        content_bottom: top,
    }
}

#[cfg(test)]
pub(crate) fn read_panel_bounds(
    tracks: &[ReadPanelSpec],
    pane_top: f32,
    pane_bottom: f32,
) -> Vec<ReadPanelBounds> {
    read_pane_layout(tracks, pane_top, pane_bottom).bounds
}

pub(crate) fn read_panel_bounds_for_group(
    bounds: &[ReadPanelBounds],
    group: u32,
) -> Option<ReadPanelBounds> {
    bounds
        .iter()
        .find(|bounds| bounds.key == ReadPanelKey::Group(group))
        .copied()
}

pub(crate) fn materialized_alignment_row_range(
    reads_top: f32,
    row_height: f32,
    clip_top: f32,
    clip_bottom: f32,
    effective_scroll: f32,
    total_rows: usize,
    overscan: usize,
) -> (usize, usize) {
    if total_rows == 0 || row_height <= f32::EPSILON || clip_bottom <= clip_top {
        return (0, 0);
    }
    let world_top = clip_top + effective_scroll;
    let world_bottom = clip_bottom + effective_scroll;
    let first = (((world_top - reads_top) / row_height).floor().max(0.0) as usize).min(total_rows);
    let end = (((world_bottom - reads_top) / row_height).ceil().max(0.0) as usize).min(total_rows);
    (
        first.saturating_sub(overscan),
        end.saturating_add(overscan).min(total_rows),
    )
}

pub(crate) fn parse_style_tag(value: &str) -> Option<[u8; 2]> {
    let bytes: [u8; 2] = value.as_bytes().try_into().ok()?;
    (bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric()).then_some(bytes)
}

pub(crate) fn persisted_style_tag(
    color: AlignmentColorMode,
    group: AlignmentGroupMode,
    sort: AlignmentSortMode,
) -> String {
    let tag = match (color, group, sort) {
        (AlignmentColorMode::Tag(tag), _, _)
        | (_, AlignmentGroupMode::Tag(tag), _)
        | (_, _, AlignmentSortMode::Tag(tag)) => tag,
        _ => *b"NM",
    };
    String::from_utf8_lossy(&tag).into_owned()
}
