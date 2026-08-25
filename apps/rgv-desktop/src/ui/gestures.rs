use super::super::*;

pub(crate) fn wheel_primary_axis(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            let axis = if y.abs() > f32::EPSILON { y } else { x };
            axis * 40.0
        }
        MouseScrollDelta::PixelDelta(position) => {
            let x = position.x as f32;
            let y = position.y as f32;
            if y.abs() > f32::EPSILON { y } else { x }
        }
    }
}

pub(crate) fn overview_drag_step(
    viewport: Viewport,
    anchor_fraction: f64,
    horizontal_drag_pixels: f64,
    vertical_drag_pixels: f64,
    contig_length: u64,
) -> Option<Viewport> {
    let zoom_factor = (-vertical_drag_pixels * 0.012).exp();
    let zoomed = viewport
        .zoom_at(
            viewport.pixel_width() * anchor_fraction.clamp(0.0, 1.0),
            zoom_factor,
            contig_length,
        )
        .ok()?;
    let pan_gain = overview_pan_gain(zoomed.span(), contig_length);
    zoomed
        .pan_pixels(horizontal_drag_pixels * pan_gain, contig_length)
        .ok()
}

pub(crate) fn overview_drag_origin(
    viewport: Viewport,
    pointer_position: u64,
    handle_fraction: Option<f64>,
    contig_length: u64,
) -> (Viewport, f64) {
    if let Some(fraction) = handle_fraction {
        return (viewport, fraction.clamp(0.0, 1.0));
    }
    let interval = viewport.interval();
    if interval.contains(pointer_position) {
        return (
            viewport,
            ((pointer_position as f64 - viewport.exact_start()) / viewport.span()).clamp(0.0, 1.0),
        );
    }
    viewport
        .centered_on(pointer_position, contig_length)
        .map_or((viewport, 0.5), |centered| (centered, 0.5))
}

pub(crate) fn retained_data_overlaps(loaded: GenomicInterval, viewport: GenomicInterval) -> bool {
    loaded.overlaps(viewport)
}

pub(crate) fn extend_alignment_layers(
    pinned: &mut DisplayList,
    scrollable: &mut DisplayList,
    layout: &AlignmentLayout,
) {
    debug_assert!(layout.pinned_rectangle_count <= layout.display_list.rectangles.len());
    debug_assert!(layout.pinned_text_count <= layout.display_list.texts.len());
    let rectangle_boundary = layout
        .pinned_rectangle_count
        .min(layout.display_list.rectangles.len());
    let text_boundary = layout
        .pinned_text_count
        .min(layout.display_list.texts.len());
    pinned.rectangles.extend(
        layout.display_list.rectangles[..rectangle_boundary]
            .iter()
            .copied(),
    );
    pinned
        .texts
        .extend(layout.display_list.texts[..text_boundary].iter().cloned());
    scrollable.rectangles.extend(
        layout.display_list.rectangles[rectangle_boundary..]
            .iter()
            .copied(),
    );
    scrollable
        .texts
        .extend(layout.display_list.texts[text_boundary..].iter().cloned());
}

pub(crate) fn fill_lone_alignment_track(
    frames: &mut [TrackFrame],
    content_bottom: f32,
    viewport_bottom: f32,
) -> f32 {
    if frames.len() != 1
        || !matches!(frames[0].key, TrackFrameKey::Alignment(_))
        || content_bottom >= viewport_bottom
    {
        return content_bottom;
    }
    let frame = &mut frames[0];
    frame.bottom = viewport_bottom;
    if let Some(reads) = frame.subtracks.last_mut().filter(|reads| !reads.collapsed) {
        reads.bottom = viewport_bottom;
    }
    viewport_bottom
}

pub(crate) fn overview_pan_gain(span: f64, contig_length: u64) -> f64 {
    let visible_fraction = span / contig_length.max(1) as f64;
    2.5 + 5.5 * visible_fraction.clamp(0.0, 1.0).sqrt()
}

pub(crate) fn density_from_horizontal_drag(origin: f32, drag_pixels: f32) -> f32 {
    (origin * (drag_pixels * 0.012).exp()).clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY)
}

/// Extra vertical world space introduced by the two-axis density navigator.
/// At minimum density the control is an ordinary scrollbar. Expanding density
/// creates a real camera range instead of merely drawing a smaller, inert thumb.
pub(crate) fn vertical_density_camera_padding(viewport_height: f32, density: f32) -> f32 {
    let scale = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY) / MIN_VERTICAL_DENSITY;
    viewport_height.max(0.0) * (scale - 1.0).max(0.0)
}

pub(crate) fn vertical_camera_maximum(
    actual_content_bottom: f32,
    viewport_bottom: f32,
    viewport_height: f32,
    density: f32,
) -> f32 {
    (actual_content_bottom + vertical_density_camera_padding(viewport_height, density)
        - viewport_bottom)
        .max(0.0)
}

pub(crate) fn vertical_overview_step(
    density: f32,
    scroll: f32,
    anchor_y: f32,
    horizontal_drag_pixels: f32,
    vertical_drag_pixels: f32,
) -> (f32, f32) {
    let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
    let next_density = density_from_horizontal_drag(density, horizontal_drag_pixels);
    let density_ratio = next_density / density.max(f32::EPSILON);
    let anchored_scroll = (scroll + anchor_y) * density_ratio - anchor_y;
    let next_scroll =
        anchored_scroll + vertical_drag_pixels * vertical_overview_pan_gain(next_density);
    (next_density, next_scroll.max(0.0))
}

pub(crate) fn vertical_overview_pan_gain(density: f32) -> f32 {
    let compact_fraction = ((MAX_VERTICAL_DENSITY
        - density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY))
        / (MAX_VERTICAL_DENSITY - MIN_VERTICAL_DENSITY))
        .clamp(0.0, 1.0);
    2.0 + 6.0 * compact_fraction.sqrt()
}

pub(crate) fn reconcile_vertical_scroll(
    current_scroll: f32,
    previous_maximum: f32,
    maximum: f32,
    latent_fraction: Option<f32>,
    gesture_fraction: Option<f32>,
) -> (f32, Option<f32>) {
    let previous_maximum = previous_maximum.max(0.0);
    let maximum = maximum.max(0.0);
    let restored_fraction = (previous_maximum <= f32::EPSILON && maximum > f32::EPSILON)
        .then_some(latent_fraction)
        .flatten();
    let active_fraction = gesture_fraction.or(restored_fraction);
    let scroll = active_fraction.map_or_else(
        || current_scroll.clamp(0.0, maximum),
        |fraction| fraction.clamp(0.0, 1.0) * maximum,
    );
    let retained_fraction = if maximum > f32::EPSILON {
        Some((scroll / maximum).clamp(0.0, 1.0))
    } else {
        gesture_fraction
            .or(latent_fraction)
            .map(|fraction| fraction.clamp(0.0, 1.0))
    };
    (scroll, retained_fraction)
}

pub(crate) fn auxiliary_track_scale(density: f32) -> f32 {
    let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
    if density <= 1.0 {
        density
    } else {
        density.sqrt()
    }
}
