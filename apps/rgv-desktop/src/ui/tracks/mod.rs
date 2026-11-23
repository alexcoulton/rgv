mod chrome;
mod geometry;
mod gutter;
mod model;
mod resize;
mod scrollbar;
#[cfg(test)]
mod tests;

pub use chrome::paint_track_viewport_chrome_with_sections;
pub use geometry::{frame_content_y_at_screen_y, genomic_text_color, paint_genomic_text};
pub use gutter::{TrackGroupChrome, TrackGutterResult, TrackSectionChrome, TrackUiAction};
pub use model::{
    AlignmentSubtrack, PaneItemLayout, PaneLayoutSnapshot, TRACK_HEADER_HEIGHT,
    TRACK_SECTION_HEADER_HEIGHT, TrackFrame, TrackFrameKey, TrackPane, TrackSubtrack,
    WorkspacePaneLayout,
};
pub use resize::{
    resized_annotation_track_height, resized_read_pane_height, resized_read_track_height,
};
pub use scrollbar::{
    ScrollbarInteraction, ScrollbarOverview, ScrollbarOverviewBin, paint_read_panel_scrollbar,
    paint_read_stack_scrollbar, paint_workspace_annotation_scrollbar, scrollbar_fraction_at_grab,
    scrollbar_geometry_for_density_at_fraction_in_viewport,
    scrollbar_geometry_for_density_in_viewport, scrollbar_offset_at_grab, scrollbar_overview,
};
