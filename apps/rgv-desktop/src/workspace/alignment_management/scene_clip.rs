use rgv_alignment_scene::{AlignmentCamera, CpuClipRect};
use rgv_core::{GenomicInterval, Viewport};
use rgv_engine::{
    AlignmentCoordinatorFrame, AlignmentIndividualReadCapability, AlignmentVerticalResidency,
};
use rgv_render::AlignmentGpuScissor;

/// Narrows a lane rectangle to the exact horizontal portion owned by a retained snapshot. The
/// integer coverage certificate may begin before or end after a fractional viewport edge, so the
/// result is clamped in camera space rather than treating the enclosing interval as fully live.
pub(crate) fn alignment_scene_covered_clip(
    viewport: Viewport,
    clip: CpuClipRect,
    covered: GenomicInterval,
) -> Option<CpuClipRect> {
    if covered.contig() != viewport.interval().contig() {
        return None;
    }
    let exact_start = viewport.exact_start();
    let exact_end = exact_start + viewport.span();
    let covered_start = (covered.start() as f64).max(exact_start);
    let covered_end = (covered.end() as f64).min(exact_end);
    if covered_start >= covered_end {
        return None;
    }
    let bases_per_pixel = viewport.bases_per_pixel();
    let clip_right = clip.x + clip.width;
    let left = (clip.x + (covered_start - exact_start) / bases_per_pixel).clamp(clip.x, clip_right);
    let right = (clip.x + (covered_end - exact_start) / bases_per_pixel).clamp(left, clip_right);
    (right > left).then_some(CpuClipRect {
        x: left,
        y: clip.y,
        width: right - left,
        height: clip.height,
    })
}

pub(crate) fn alignment_scene_gpu_scissor(
    camera: AlignmentCamera,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> Option<AlignmentGpuScissor> {
    alignment_clip_gpu_scissor(camera.clip(), scale_factor, surface_width, surface_height)
}

pub(crate) fn alignment_clip_gpu_scissor(
    clip: CpuClipRect,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> Option<AlignmentGpuScissor> {
    let scale = scale_factor.max(f32::EPSILON) as f64;
    let left = (clip.x * scale)
        .round()
        .clamp(0.0, f64::from(surface_width)) as u32;
    let top = (clip.y * scale)
        .round()
        .clamp(0.0, f64::from(surface_height)) as u32;
    let right = ((clip.x + clip.width) * scale)
        .round()
        .clamp(f64::from(left), f64::from(surface_width)) as u32;
    let bottom = ((clip.y + clip.height) * scale)
        .round()
        .clamp(f64::from(top), f64::from(surface_height)) as u32;
    (right > left && bottom > top).then_some(AlignmentGpuScissor {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// A retained snapshot from a discontinuous locus is not a row-count or render authority for the
/// new camera. Source workers may truthfully attach that old immutable snapshot while acquiring
/// the replacement; the desktop keeps the progress frame but strips the irrelevant scene.
pub(crate) fn alignment_scene_frame_for_ui(
    frame: AlignmentCoordinatorFrame,
) -> AlignmentCoordinatorFrame {
    let retained_interval = frame
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.reads().atlas().interval());
    alignment_scene_frame_for_ui_against_retained_interval(frame, retained_interval)
}

/// Normalizes a worker progress frame against the interval carried by its retained snapshot. This
/// decomposition keeps the discontinuous-locus rule directly testable without constructing the
/// heavyweight immutable scene graph.
pub(crate) fn alignment_scene_frame_for_ui_against_retained_interval(
    mut frame: AlignmentCoordinatorFrame,
    retained_interval: Option<GenomicInterval>,
) -> AlignmentCoordinatorFrame {
    let camera_refusal_is_terminal = frame.individual_reads_refused_for_camera();
    let relevant = retained_interval.is_none_or(|interval| {
        alignment_scene_intervals_share_authority(interval, frame.camera.interval())
    });
    if !relevant {
        frame.snapshot = None;
        frame.row_activity = None;
        frame.covered_interval = None;
        frame.covered_rows = 0..0;
        frame.requested_rows = frame.camera.visible_rows();
        // The requirement describes the new desired camera, not the discarded old snapshot. It
        // must survive so the desktop can request and install the exact FASTA horizon.
        frame.scene_contracts_current = false;
        frame.reference_products_current = false;
        if !camera_refusal_is_terminal {
            frame.individual_reads = AlignmentIndividualReadCapability::Pending;
        }
        frame.vertical_residency = AlignmentVerticalResidency::ViewportBounded;
    }
    frame
}

pub(crate) fn alignment_scene_intervals_share_authority(
    atlas: GenomicInterval,
    camera: GenomicInterval,
) -> bool {
    atlas.contig() == camera.contig() && atlas.overlaps(camera)
}
