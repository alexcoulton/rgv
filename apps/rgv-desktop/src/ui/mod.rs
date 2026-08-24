pub(crate) mod alignment_settings;
mod diagnostics;
mod frame;
pub(crate) mod gestures;
pub(crate) mod inspectors;
pub(crate) mod interaction;
mod menu;
mod navigation;
pub(crate) mod shell;
mod status;
pub(crate) mod system_dialogs;
pub(crate) mod track_settings;
mod tracks;
pub(crate) mod workspace_dialogs;

pub use diagnostics::{show_diagnostics, toggled_diagnostics};
pub use menu::{SEARCHABLE_COMMANDS, UiCommand, command_matches};
pub use navigation::{
    GENOME_CONTEXT_HEIGHT, GenomeOverviewData, OverviewInteraction, paint_genome_context,
};
pub use status::status_bar;
pub use tracks::{
    AlignmentSubtrack, ScrollbarInteraction, ScrollbarOverview, TrackFrame, TrackFrameKey,
    TrackSubtrack, TrackUiAction, keyboard_reorder_index, paint_genomic_text,
    paint_track_viewport_chrome, paint_vertical_scrollbar, resized_track_height,
    scrollbar_fraction_at_grab, scrollbar_geometry_for_density,
    scrollbar_geometry_for_density_at_fraction, scrollbar_offset_at_grab, scrollbar_overview,
    track_drop_index,
};

pub const MENU_HEIGHT: f32 = 24.0;
pub const NAVIGATION_HEIGHT: f32 = 38.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const TRACK_GUTTER_WIDTH: f32 = 168.0;
pub const SCROLLBAR_WIDTH: f32 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasGeometry {
    pub viewport: egui::Rect,
    pub gutter: egui::Rect,
    pub genomic: egui::Rect,
    pub scrollbar: egui::Rect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalScissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CanvasGeometry {
    pub fn from_viewport(viewport: egui::Rect) -> Self {
        let gutter_width = TRACK_GUTTER_WIDTH.min((viewport.width() * 0.35).max(72.0));
        let scrollbar_width = SCROLLBAR_WIDTH.min((viewport.width() * 0.1).max(14.0));
        let gutter = egui::Rect::from_min_max(
            viewport.min,
            egui::pos2(viewport.left() + gutter_width, viewport.bottom()),
        );
        let scrollbar = egui::Rect::from_min_max(
            egui::pos2(viewport.right() - scrollbar_width, viewport.top()),
            viewport.max,
        );
        let genomic = egui::Rect::from_min_max(
            egui::pos2(gutter.right(), viewport.top()),
            egui::pos2(scrollbar.left(), viewport.bottom()),
        );
        Self {
            viewport,
            gutter,
            genomic,
            scrollbar,
        }
    }

    pub fn initial(window_width: f32, window_height: f32) -> Self {
        let top = MENU_HEIGHT + NAVIGATION_HEIGHT + GENOME_CONTEXT_HEIGHT;
        let bottom = (window_height - STATUS_HEIGHT).max(top + 1.0);
        Self::from_viewport(egui::Rect::from_min_max(
            egui::pos2(0.0, top),
            egui::pos2(window_width.max(1.0), bottom),
        ))
    }

    pub fn approximately_equals(self, other: Self) -> bool {
        [
            self.genomic.left() - other.genomic.left(),
            self.genomic.top() - other.genomic.top(),
            self.genomic.width() - other.genomic.width(),
            self.genomic.height() - other.genomic.height(),
        ]
        .into_iter()
        .all(|difference| difference.abs() < 0.5)
    }

    pub fn physical_scissor(
        self,
        scale_factor: f32,
        surface_width: u32,
        surface_height: u32,
    ) -> PhysicalScissor {
        let scale = scale_factor.max(f32::EPSILON);
        let left = (self.genomic.left() * scale)
            .round()
            .clamp(0.0, surface_width as f32) as u32;
        let top = (self.genomic.top() * scale)
            .round()
            .clamp(0.0, surface_height as f32) as u32;
        let right = (self.genomic.right() * scale)
            .round()
            .clamp(left as f32, surface_width as f32) as u32;
        let bottom = (self.genomic.bottom() * scale)
            .round()
            .clamp(top as f32, surface_height as f32) as u32;
        PhysicalScissor {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_geometry_reserves_gutter_and_scrollbar() {
        let geometry = CanvasGeometry::initial(1_280.0, 800.0);
        assert!((geometry.gutter.width() - TRACK_GUTTER_WIDTH).abs() < f32::EPSILON);
        assert!((geometry.scrollbar.width() - SCROLLBAR_WIDTH).abs() < f32::EPSILON);
        assert!(geometry.genomic.width() > 1_000.0);
        assert!((geometry.genomic.top() - geometry.viewport.top()).abs() < f32::EPSILON);
    }

    #[test]
    fn physical_scissor_is_consistent_at_standard_and_retina_scale() {
        let geometry = CanvasGeometry::initial(1_280.0, 800.0);
        let standard = geometry.physical_scissor(1.0, 1_280, 800);
        let retina = geometry.physical_scissor(2.0, 2_560, 1_600);
        assert_eq!(retina.x, standard.x * 2);
        assert_eq!(retina.y, standard.y * 2);
        assert_eq!(retina.width, standard.width * 2);
        assert_eq!(retina.height, standard.height * 2);
    }
}
