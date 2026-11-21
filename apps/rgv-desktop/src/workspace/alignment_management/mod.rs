#[cfg(test)]
mod alignment_gpu_policy_tests;
mod dispatch;
mod gpu_budget;
mod gpu_bundle;
mod gpu_prefetch;
mod gpu_publication;
#[cfg(test)]
mod gpu_publication_retry_tests;
mod interactions;
mod scene_authority;
mod scene_clip;
mod scene_frame;
mod scene_requests;
mod sources;
mod speculation;

#[cfg(test)]
pub(crate) use gpu_publication::{
    alignment_gpu_bundle_error_is_deferred, alignment_read_camera_capture_error_is_deferred,
    alignment_summary_camera_capture_error_is_deferred,
};
pub(crate) use gpu_publication::{
    alignment_scene_snapshot_satisfies_contract, alignment_scene_staging_frame_is_current,
};
pub(crate) use scene_authority::{
    alignment_scene_progress_frame_defers_to_incumbent, alignment_scene_viewport_for_layout_width,
    retained_alignment_scene_can_present, uncovered_vertical_candidate_retains_committed_pixels,
};
pub(crate) use scene_clip::{
    alignment_clip_gpu_scissor, alignment_scene_covered_clip, alignment_scene_frame_for_ui,
    alignment_scene_gpu_scissor,
};
#[cfg(test)]
pub(crate) use scene_clip::{
    alignment_scene_frame_for_ui_against_retained_interval,
    alignment_scene_intervals_share_authority,
};
