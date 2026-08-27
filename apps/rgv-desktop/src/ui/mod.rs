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
pub use navigation::{GenomeOverviewData, OverviewInteraction, paint_genome_context};
pub use status::status_bar;
pub use tracks::{
    AlignmentSubtrack, PaneItemLayout, PaneLayoutSnapshot, ScrollbarInteraction, ScrollbarOverview,
    TrackFrame, TrackFrameKey, TrackGroupChrome, TrackPane, TrackSubtrack, TrackUiAction,
    WorkspacePaneLayout, frame_content_y_at_screen_y, paint_genomic_text,
    paint_read_panel_scrollbar, paint_read_stack_scrollbar,
    paint_track_viewport_chrome_with_groups, paint_workspace_annotation_scrollbar,
    resized_annotation_track_height, resized_read_pane_height, resized_read_track_height,
    scrollbar_fraction_at_grab, scrollbar_geometry_for_density_at_fraction_in_viewport,
    scrollbar_geometry_for_density_in_viewport, scrollbar_offset_at_grab, scrollbar_overview,
};

pub const MENU_HEIGHT: f32 = 24.0;
pub const NAVIGATION_HEIGHT: f32 = 38.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const TRACK_GUTTER_WIDTH: f32 = 168.0;
pub const SCROLLBAR_WIDTH: f32 = 22.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CytobandAvailability {
    Loading,
    Present,
    #[default]
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GenomeContextMetrics {
    pub metadata_height: f32,
    pub bar_height: f32,
    pub row_gap: f32,
    pub span_height: f32,
    pub ruler_height: f32,
    pub horizontal_padding: f32,
}

impl Default for GenomeContextMetrics {
    fn default() -> Self {
        Self {
            metadata_height: 16.0,
            bar_height: 10.0,
            row_gap: 3.0,
            span_height: 16.0,
            ruler_height: 22.0,
            horizontal_padding: 8.0,
        }
    }
}

impl GenomeContextMetrics {
    pub fn measured(ui: &egui::Ui) -> Self {
        let metadata_font = egui::FontId::proportional(11.0);
        let ruler_font = egui::FontId::proportional(10.0);
        let (metadata_text_height, ruler_text_height) = ui.fonts_mut(|fonts| {
            (
                fonts.row_height(&metadata_font),
                fonts.row_height(&ruler_font),
            )
        });
        Self {
            metadata_height: (metadata_text_height + 4.0).ceil(),
            span_height: (metadata_text_height + 4.0).ceil(),
            ruler_height: (ruler_text_height + 12.0).ceil(),
            ..Self::default()
        }
    }

    pub fn context_height(self, has_cytoband_row: bool) -> f32 {
        self.metadata_height
            + self.bar_height
            + self.row_gap
            + self.span_height
            + self.ruler_height
            + if has_cytoband_row {
                self.bar_height + self.row_gap
            } else {
                0.0
            }
    }

    pub fn rows(self, panel: egui::Rect, has_cytoband_row: bool) -> GenomeContextRows {
        let metadata =
            egui::Rect::from_min_size(panel.min, egui::vec2(panel.width(), self.metadata_height));
        let bar_left = panel.left() + self.horizontal_padding;
        let bar_width = (panel.width() - self.horizontal_padding * 2.0).max(1.0);
        let mut top = metadata.bottom();
        let cytoband = has_cytoband_row.then(|| {
            let rect = egui::Rect::from_min_size(
                egui::pos2(bar_left, top),
                egui::vec2(bar_width, self.bar_height),
            );
            top = rect.bottom() + self.row_gap;
            rect
        });
        let overview = egui::Rect::from_min_size(
            egui::pos2(bar_left, top),
            egui::vec2(bar_width, self.bar_height),
        );
        top = overview.bottom() + self.row_gap;
        let span = egui::Rect::from_min_size(
            egui::pos2(panel.left(), top),
            egui::vec2(panel.width(), self.span_height),
        );
        top = span.bottom();
        let ruler = egui::Rect::from_min_max(
            egui::pos2(panel.left(), top),
            egui::pos2(panel.right(), panel.bottom()),
        );
        GenomeContextRows {
            metadata,
            cytoband,
            overview,
            span,
            ruler,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GenomeContextRows {
    pub metadata: egui::Rect,
    pub cytoband: Option<egui::Rect>,
    pub overview: egui::Rect,
    pub span: egui::Rect,
    pub ruler: egui::Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspacePanelLayout {
    pub panel_id: rgv_core::PanelId,
    pub column: egui::Rect,
    pub header: egui::Rect,
    pub track_column: egui::Rect,
    pub header_genomic: egui::Rect,
    pub header_scrollbar: egui::Rect,
    pub canvas: egui::Rect,
    pub scrollbar: egui::Rect,
    pub context_rows: GenomeContextRows,
    pub cytobands: CytobandAvailability,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceLayoutSnapshot {
    pub revision: u64,
    pub metrics: GenomeContextMetrics,
    pub workspace: egui::Rect,
    pub genome_context: egui::Rect,
    pub track_viewport: egui::Rect,
    pub context_gutter: egui::Rect,
    pub track_gutter: egui::Rect,
    pub panels: Vec<WorkspacePanelLayout>,
    pub has_cytoband_row: bool,
}

impl WorkspaceLayoutSnapshot {
    pub fn compute(
        workspace: egui::Rect,
        panel_ids: &[rgv_core::PanelId],
        cytobands: &[CytobandAvailability],
        metrics: GenomeContextMetrics,
        revision: u64,
    ) -> Self {
        assert!(
            !panel_ids.is_empty(),
            "a workspace must contain a locus panel"
        );
        assert_eq!(
            panel_ids.len(),
            cytobands.len(),
            "cytoband state must be supplied for every locus panel"
        );
        let has_cytoband_row = cytobands
            .iter()
            .any(|state| *state != CytobandAvailability::Absent);
        let context_height = metrics
            .context_height(has_cytoband_row)
            .min((workspace.height() - 1.0).max(1.0));
        let genome_context = egui::Rect::from_min_max(
            workspace.min,
            egui::pos2(workspace.right(), workspace.top() + context_height),
        );
        let track_viewport = egui::Rect::from_min_max(
            egui::pos2(workspace.left(), genome_context.bottom()),
            workspace.max,
        );
        let canvas_geometry = CanvasGeometry::from_viewport(track_viewport);
        let context_gutter = egui::Rect::from_min_max(
            genome_context.min,
            egui::pos2(canvas_geometry.gutter.right(), genome_context.bottom()),
        );
        let panels = panel_ids
            .iter()
            .zip(cytobands)
            .enumerate()
            .map(|(index, (panel_id, cytobands))| {
                let canvas = workspace_genomic_rect(canvas_geometry, index, panel_ids.len());
                let scrollbar = workspace_scrollbar_rect(canvas_geometry, index, panel_ids.len());
                let track_column = egui::Rect::from_min_max(
                    canvas.min,
                    egui::pos2(scrollbar.right(), canvas.bottom()),
                );
                let header = egui::Rect::from_min_max(
                    egui::pos2(canvas.left(), genome_context.top()),
                    egui::pos2(scrollbar.right(), genome_context.bottom()),
                );
                let column = egui::Rect::from_min_max(header.min, track_column.max);
                let header_genomic = egui::Rect::from_min_max(
                    header.min,
                    egui::pos2(canvas.right(), header.bottom()),
                );
                let header_scrollbar = egui::Rect::from_min_max(
                    egui::pos2(header_genomic.right(), header.top()),
                    header.max,
                );
                WorkspacePanelLayout {
                    panel_id: *panel_id,
                    column,
                    header,
                    track_column,
                    header_genomic,
                    header_scrollbar,
                    canvas,
                    scrollbar,
                    context_rows: metrics.rows(header_genomic, has_cytoband_row),
                    cytobands: *cytobands,
                }
            })
            .collect();
        Self {
            revision,
            metrics,
            workspace,
            genome_context,
            track_viewport,
            context_gutter,
            track_gutter: canvas_geometry.gutter,
            panels,
            has_cytoband_row,
        }
    }

    pub fn canvas_geometry(&self) -> CanvasGeometry {
        CanvasGeometry::from_viewport(self.track_viewport)
    }

    pub fn panel(&self, panel_id: rgv_core::PanelId) -> Option<&WorkspacePanelLayout> {
        self.panels.iter().find(|panel| panel.panel_id == panel_id)
    }
}

/// Full horizontal cell owned by one workspace, including its scrollbar.
pub fn workspace_column_rect(geometry: CanvasGeometry, index: usize, count: usize) -> egui::Rect {
    let count = count.max(1);
    let left_edge = geometry.gutter.right();
    let right_edge = geometry.viewport.right();
    let width = (right_edge - left_edge) / count as f32;
    let left = left_edge + index.min(count - 1) as f32 * width;
    let right = if index + 1 >= count {
        right_edge
    } else {
        left + width
    };
    egui::Rect::from_min_max(
        egui::pos2(left, geometry.viewport.top()),
        egui::pos2(right, geometry.viewport.bottom()),
    )
}

pub fn workspace_genomic_rect(geometry: CanvasGeometry, index: usize, count: usize) -> egui::Rect {
    let column = workspace_column_rect(geometry, index, count);
    egui::Rect::from_min_max(
        column.min,
        egui::pos2(
            (column.right() - SCROLLBAR_WIDTH).max(column.left() + 1.0),
            column.bottom(),
        ),
    )
}

pub fn workspace_scrollbar_rect(
    geometry: CanvasGeometry,
    index: usize,
    count: usize,
) -> egui::Rect {
    let column = workspace_column_rect(geometry, index, count);
    let genomic = workspace_genomic_rect(geometry, index, count);
    egui::Rect::from_min_max(egui::pos2(genomic.right(), column.top()), column.max)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasGeometry {
    pub viewport: egui::Rect,
    pub gutter: egui::Rect,
    pub genomic: egui::Rect,
    pub annotation_scrollbar: egui::Rect,
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
        // The annotation and read controls occupy upper and lower segments of
        // this one right-hand column. Their vertical split is data-dependent
        // and is applied by the frame UI once the pane layout is available.
        let annotation_scrollbar = scrollbar;
        let genomic = egui::Rect::from_min_max(
            egui::pos2(gutter.right(), viewport.top()),
            egui::pos2(scrollbar.left(), viewport.bottom()),
        );
        Self {
            viewport,
            gutter,
            genomic,
            annotation_scrollbar,
            scrollbar,
        }
    }

    #[cfg(test)]
    pub fn initial(window_width: f32, window_height: f32) -> Self {
        let top =
            MENU_HEIGHT + NAVIGATION_HEIGHT + GenomeContextMetrics::default().context_height(false);
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
        assert!((geometry.annotation_scrollbar.width() - SCROLLBAR_WIDTH).abs() < f32::EPSILON);
        assert_eq!(geometry.annotation_scrollbar, geometry.scrollbar);
        assert!(geometry.genomic.width() > 1_000.0);
        assert!((geometry.genomic.top() - geometry.viewport.top()).abs() < f32::EPSILON);
    }

    #[test]
    fn split_workspaces_each_reserve_their_own_scrollbar() {
        let geometry = CanvasGeometry::initial(1_280.0, 800.0);
        let first = workspace_genomic_rect(geometry, 0, 2);
        let first_scrollbar = workspace_scrollbar_rect(geometry, 0, 2);
        let second = workspace_genomic_rect(geometry, 1, 2);
        let second_scrollbar = workspace_scrollbar_rect(geometry, 1, 2);

        assert_eq!(first.right(), first_scrollbar.left());
        assert_eq!(first_scrollbar.right(), second.left());
        assert_eq!(second.right(), second_scrollbar.left());
        assert_eq!(second_scrollbar.right(), geometry.viewport.right());
        assert_eq!(first_scrollbar.width(), SCROLLBAR_WIDTH);
        assert_eq!(second_scrollbar.width(), SCROLLBAR_WIDTH);
        assert_eq!(first.width(), second.width());
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

    #[test]
    fn workspace_layout_uses_identical_header_and_canvas_columns() {
        let workspace = egui::Rect::from_min_max(egui::pos2(0.0, 62.0), egui::pos2(1_250.0, 776.0));
        let panel_ids = [rgv_core::PanelId::new(1), rgv_core::PanelId::new(2)];
        let layout = WorkspaceLayoutSnapshot::compute(
            workspace,
            &panel_ids,
            &[CytobandAvailability::Absent, CytobandAvailability::Absent],
            GenomeContextMetrics::default(),
            4,
        );

        assert_eq!(layout.revision, 4);
        assert!(!layout.has_cytoband_row);
        assert_eq!(layout.panels.len(), 2);
        assert_eq!(layout.genome_context.bottom(), layout.track_viewport.top());
        assert_eq!(layout.context_gutter.right(), layout.track_gutter.right());
        for panel in &layout.panels {
            assert_eq!(panel.header_genomic.left(), panel.canvas.left());
            assert_eq!(panel.header_genomic.right(), panel.canvas.right());
            assert_eq!(panel.header_scrollbar.left(), panel.scrollbar.left());
            assert_eq!(panel.header_scrollbar.right(), panel.scrollbar.right());
            assert_eq!(
                panel.context_rows.metadata.top(),
                layout.genome_context.top()
            );
            assert_eq!(
                panel.context_rows.ruler.bottom(),
                layout.genome_context.bottom()
            );
        }
        assert_eq!(
            layout.panels[0].column.right(),
            layout.panels[1].column.left()
        );
        assert_eq!(layout.panels[1].column.right(), workspace.right());
    }

    #[test]
    fn workspace_layout_adds_one_shared_cytoband_row_for_mixed_panels() {
        let workspace = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_280.0, 800.0));
        let panel_ids = [rgv_core::PanelId::new(1), rgv_core::PanelId::new(2)];
        let metrics = GenomeContextMetrics::default();
        let absent = WorkspaceLayoutSnapshot::compute(
            workspace,
            &panel_ids,
            &[CytobandAvailability::Absent, CytobandAvailability::Absent],
            metrics,
            1,
        );
        let mixed = WorkspaceLayoutSnapshot::compute(
            workspace,
            &panel_ids,
            &[CytobandAvailability::Present, CytobandAvailability::Absent],
            metrics,
            2,
        );

        assert!(mixed.has_cytoband_row);
        assert_eq!(
            mixed.genome_context.height() - absent.genome_context.height(),
            metrics.bar_height + metrics.row_gap
        );
        assert!(
            mixed
                .panels
                .iter()
                .all(|panel| panel.context_rows.cytoband.is_some())
        );
        assert_eq!(
            mixed.panels[0].context_rows.overview.top(),
            mixed.panels[1].context_rows.overview.top()
        );
        assert_eq!(mixed.panels[0].cytobands, CytobandAvailability::Present);
        assert_eq!(mixed.panels[1].cytobands, CytobandAvailability::Absent);
    }

    #[test]
    fn workspace_layout_matrix_never_overlaps_rows_or_columns() {
        let metrics = GenomeContextMetrics::default();
        for width in [640.0, 960.0, 1_280.0, 2_560.0] {
            for panel_count in 1..=4 {
                let panel_ids = (1..=panel_count)
                    .map(|id| rgv_core::PanelId::new(id as u64))
                    .collect::<Vec<_>>();
                for cytobands in [
                    vec![CytobandAvailability::Absent; panel_count],
                    vec![CytobandAvailability::Present; panel_count],
                ] {
                    let workspace =
                        egui::Rect::from_min_size(egui::pos2(0.0, 62.0), egui::vec2(width, 700.0));
                    let layout = WorkspaceLayoutSnapshot::compute(
                        workspace, &panel_ids, &cytobands, metrics, 1,
                    );
                    assert!(
                        layout.panels.windows(2).all(|pair| (pair[0].column.right()
                            - pair[1].column.left())
                        .abs()
                            < 0.01)
                    );
                    for panel in &layout.panels {
                        let mut rows = vec![panel.context_rows.metadata];
                        rows.extend(panel.context_rows.cytoband);
                        rows.extend([
                            panel.context_rows.overview,
                            panel.context_rows.span,
                            panel.context_rows.ruler,
                        ]);
                        assert!(
                            rows.windows(2)
                                .all(|pair| pair[0].bottom() <= pair[1].top())
                        );
                        assert!(
                            rows.iter()
                                .all(|row| layout.genome_context.contains_rect(*row))
                        );
                    }
                }
            }
        }
    }
}
