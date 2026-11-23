pub(crate) mod alignment_settings;
mod diagnostics;
mod frame;
pub(crate) mod gestures;
pub(crate) mod icons;
pub(crate) mod inspectors;
pub(crate) mod interaction;
mod menu;
mod navigation;
pub(crate) mod presented_text;
pub(crate) mod shell;
mod status;
pub(crate) mod system_dialogs;
pub(crate) mod track_settings;
mod tracks;
pub(crate) mod workspace_dialogs;

#[cfg_attr(not(feature = "diagnostics"), allow(unused_imports))]
pub(crate) use diagnostics::{
    ManualInteractionTraceAction, ManualInteractionTraceSnapshot, ManualInteractionTraceState,
    show_diagnostics, toggled_diagnostics,
};
pub use menu::{SEARCHABLE_COMMANDS, UiCommand, command_matches};
pub use navigation::{GenomeOverviewData, OverviewInteraction, paint_genome_context};
pub use status::status_bar_with_badge;
pub use tracks::{
    AlignmentSubtrack, PaneItemLayout, PaneLayoutSnapshot, ScrollbarInteraction, ScrollbarOverview,
    ScrollbarOverviewBin, TRACK_HEADER_HEIGHT, TRACK_SECTION_HEADER_HEIGHT, TrackFrame,
    TrackFrameKey, TrackGroupChrome, TrackPane, TrackSectionChrome, TrackSubtrack, TrackUiAction,
    WorkspacePaneLayout, frame_content_y_at_screen_y, genomic_text_color, paint_genomic_text,
    paint_read_panel_scrollbar, paint_read_stack_scrollbar,
    paint_track_viewport_chrome_with_sections, paint_workspace_annotation_scrollbar,
    resized_annotation_track_height, resized_read_pane_height, resized_read_track_height,
    scrollbar_fraction_at_grab, scrollbar_geometry_for_density_at_fraction_in_viewport,
    scrollbar_geometry_for_density_in_viewport, scrollbar_offset_at_grab, scrollbar_overview,
};

pub const MENU_HEIGHT: f32 = 24.0;
pub const NAVIGATION_HEIGHT: f32 = 38.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const TRACK_GUTTER_WIDTH: f32 = 168.0;
pub const SCROLLBAR_WIDTH: f32 = 22.0;
pub(crate) const RULER_BASELINE_BOTTOM_INSET: f32 = 3.0;
pub(crate) const RULER_LABEL_BASELINE_GAP: f32 = 10.0;

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
            ruler_height: 25.0,
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
            ruler_height: (ruler_text_height
                + RULER_BASELINE_BOTTOM_INSET
                + RULER_LABEL_BASELINE_GAP)
                .ceil(),
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
        let horizontal_padding = self.horizontal_padding.min(panel.width() * 0.5).max(0.0);
        let bar_left = panel.left() + horizontal_padding;
        let bar_width = (panel.width() - horizontal_padding * 2.0).max(0.0);
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
pub struct WorkspacePanelLayoutInput {
    pub panel_id: rgv_core::PanelId,
    pub contig_name: String,
    pub contig_length: u64,
    pub cytobands: CytobandAvailability,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceLayoutInput {
    /// Final shell rectangle after the menu, toolbar, status bar, and
    /// Properties panel have all been reserved.
    pub workspace: egui::Rect,
    /// Exact Properties rectangle used to produce `workspace`. Keeping this
    /// boundary in the immutable input makes shell/header agreement auditable.
    pub properties: egui::Rect,
    pub properties_expanded: bool,
    pub panels: Vec<WorkspacePanelLayoutInput>,
    pub metrics: GenomeContextMetrics,
    pub requested_track_gutter_width: f32,
    pub minimum_track_gutter_width: f32,
    pub requested_scrollbar_width: f32,
    pub minimum_scrollbar_width: f32,
    /// Native pixels per platform point. Logical geometry stays in points,
    /// but the committed scale is needed to audit physical clipping.
    pub display_scale: f32,
    /// The active egui zoom/accessibility scale used when measuring `metrics`.
    pub accessibility_scale: f32,
}

impl WorkspaceLayoutInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace: egui::Rect,
        properties: egui::Rect,
        properties_expanded: bool,
        panels: Vec<WorkspacePanelLayoutInput>,
        metrics: GenomeContextMetrics,
        display_scale: f32,
        accessibility_scale: f32,
    ) -> Self {
        Self {
            workspace,
            properties,
            properties_expanded,
            panels,
            metrics,
            requested_track_gutter_width: TRACK_GUTTER_WIDTH,
            minimum_track_gutter_width: 72.0,
            requested_scrollbar_width: SCROLLBAR_WIDTH,
            minimum_scrollbar_width: 14.0,
            display_scale,
            accessibility_scale,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspacePanelLayout {
    pub panel_id: rgv_core::PanelId,
    pub contig_name: String,
    pub contig_length: u64,
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
    pub properties: egui::Rect,
    pub properties_expanded: bool,
    pub display_scale: f32,
    pub accessibility_scale: f32,
    pub requested_track_gutter_width: f32,
    pub minimum_track_gutter_width: f32,
    pub requested_scrollbar_width: f32,
    pub minimum_scrollbar_width: f32,
    pub genome_context: egui::Rect,
    pub track_viewport: egui::Rect,
    pub context_gutter: egui::Rect,
    pub track_gutter: egui::Rect,
    pub panels: Vec<WorkspacePanelLayout>,
    pub has_cytoband_row: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkspaceHorizontalMetrics {
    gutter_width: f32,
    column_width: f32,
    scrollbar_width: f32,
    canvas_width: f32,
}

fn workspace_horizontal_metrics(
    workspace_width: f32,
    panel_count: usize,
    requested_gutter_width: f32,
    minimum_gutter_width: f32,
    requested_scrollbar_width: f32,
    minimum_scrollbar_width: f32,
) -> WorkspaceHorizontalMetrics {
    let panel_count = panel_count.max(1);
    let maximum_gutter_width =
        (workspace_width - panel_count as f32 * (minimum_scrollbar_width.max(0.0) + 1.0)).max(0.0);
    let gutter_width = requested_gutter_width
        .max(0.0)
        .min(maximum_gutter_width)
        .max(minimum_gutter_width.max(0.0).min(maximum_gutter_width));
    let available_panel_width = (workspace_width - gutter_width).max(0.0);
    let column_width = available_panel_width / panel_count as f32;
    let maximum_scrollbar_width = (column_width - 1.0).max(0.0);
    let scrollbar_width = requested_scrollbar_width
        .max(0.0)
        .min(maximum_scrollbar_width)
        .max(
            minimum_scrollbar_width
                .max(0.0)
                .min(maximum_scrollbar_width),
        );
    WorkspaceHorizontalMetrics {
        gutter_width,
        column_width,
        scrollbar_width,
        canvas_width: (column_width - scrollbar_width).max(0.0),
    }
}

impl WorkspaceLayoutSnapshot {
    pub fn compute(input: WorkspaceLayoutInput, revision: u64) -> Self {
        assert!(
            !input.panels.is_empty(),
            "a workspace must contain a locus panel"
        );
        let WorkspaceLayoutInput {
            workspace,
            properties,
            properties_expanded,
            panels: panel_inputs,
            metrics,
            requested_track_gutter_width,
            minimum_track_gutter_width,
            requested_scrollbar_width,
            minimum_scrollbar_width,
            display_scale,
            accessibility_scale,
        } = input;
        let has_cytoband_row = panel_inputs
            .iter()
            .any(|panel| panel.cytobands != CytobandAvailability::Absent);
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
        let panel_count = panel_inputs.len();
        let horizontal = workspace_horizontal_metrics(
            workspace.width(),
            panel_count,
            requested_track_gutter_width,
            minimum_track_gutter_width,
            requested_scrollbar_width,
            minimum_scrollbar_width,
        );
        let track_gutter = egui::Rect::from_min_max(
            track_viewport.min,
            egui::pos2(
                track_viewport.left() + horizontal.gutter_width,
                track_viewport.bottom(),
            ),
        );
        let context_gutter = egui::Rect::from_min_max(
            genome_context.min,
            egui::pos2(track_gutter.right(), genome_context.bottom()),
        );
        let panels = panel_inputs
            .into_iter()
            .enumerate()
            .map(|(index, panel_input)| {
                let column_left = track_gutter.right() + index as f32 * horizontal.column_width;
                let column_right = if index + 1 == panel_count {
                    workspace.right()
                } else {
                    column_left + horizontal.column_width
                };
                let canvas_right = column_right - horizontal.scrollbar_width;
                let canvas = egui::Rect::from_min_max(
                    egui::pos2(column_left, track_viewport.top()),
                    egui::pos2(canvas_right, track_viewport.bottom()),
                );
                let scrollbar = egui::Rect::from_min_max(
                    egui::pos2(canvas_right, track_viewport.top()),
                    egui::pos2(column_right, track_viewport.bottom()),
                );
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
                    panel_id: panel_input.panel_id,
                    contig_name: panel_input.contig_name,
                    contig_length: panel_input.contig_length,
                    column,
                    header,
                    track_column,
                    header_genomic,
                    header_scrollbar,
                    canvas,
                    scrollbar,
                    context_rows: metrics.rows(header_genomic, has_cytoband_row),
                    cytobands: panel_input.cytobands,
                }
            })
            .collect();
        Self {
            revision,
            metrics,
            workspace,
            properties,
            properties_expanded,
            display_scale,
            accessibility_scale,
            requested_track_gutter_width,
            minimum_track_gutter_width,
            requested_scrollbar_width,
            minimum_scrollbar_width,
            genome_context,
            track_viewport,
            context_gutter,
            track_gutter,
            panels,
            has_cytoband_row,
        }
    }

    pub fn canvas_geometry(&self) -> CanvasGeometry {
        let last_panel = self
            .panels
            .last()
            .expect("a committed workspace contains a locus panel");
        CanvasGeometry {
            viewport: self.track_viewport,
            gutter: self.track_gutter,
            genomic: egui::Rect::from_min_max(
                egui::pos2(self.track_gutter.right(), self.track_viewport.top()),
                egui::pos2(last_panel.canvas.right(), self.track_viewport.bottom()),
            ),
            annotation_scrollbar: last_panel.scrollbar,
            scrollbar: last_panel.scrollbar,
        }
    }

    pub fn panel(&self, panel_id: rgv_core::PanelId) -> Option<&WorkspacePanelLayout> {
        self.panels.iter().find(|panel| panel.panel_id == panel_id)
    }

    pub fn panel_at(&self, point: egui::Pos2) -> Option<&WorkspacePanelLayout> {
        self.panels
            .iter()
            .find(|panel| panel.track_column.contains(point))
    }

    pub fn projected_canvas_width(&self, panel_count: usize) -> f32 {
        workspace_horizontal_metrics(
            self.workspace.width(),
            panel_count,
            self.requested_track_gutter_width,
            self.minimum_track_gutter_width,
            self.requested_scrollbar_width,
            self.minimum_scrollbar_width,
        )
        .canvas_width
    }

    pub fn physical_genomic_scissor(
        &self,
        scale_factor: f32,
        surface_width: u32,
        surface_height: u32,
    ) -> PhysicalScissor {
        physical_scissor_for_rect(
            self.canvas_geometry().genomic,
            scale_factor,
            surface_width,
            surface_height,
        )
    }

    pub(crate) fn physical_rect_scissor(
        &self,
        rect: egui::Rect,
        scale_factor: f32,
        surface_width: u32,
        surface_height: u32,
    ) -> PhysicalScissor {
        physical_scissor_for_rect(rect, scale_factor, surface_width, surface_height)
    }
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
    #[cfg(test)]
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
}

fn physical_scissor_for_rect(
    rect: egui::Rect,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> PhysicalScissor {
    let scale = scale_factor.max(f32::EPSILON);
    let left = (rect.left() * scale)
        .round()
        .clamp(0.0, surface_width as f32) as u32;
    let top = (rect.top() * scale)
        .round()
        .clamp(0.0, surface_height as f32) as u32;
    let right = (rect.right() * scale)
        .round()
        .clamp(left as f32, surface_width as f32) as u32;
    let bottom = (rect.bottom() * scale)
        .round()
        .clamp(top as f32, surface_height as f32) as u32;
    PhysicalScissor {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_input(
        workspace: egui::Rect,
        cytobands: &[CytobandAvailability],
        metrics: GenomeContextMetrics,
    ) -> WorkspaceLayoutInput {
        let properties = egui::Rect::from_min_max(
            egui::pos2(workspace.right(), workspace.top()),
            egui::pos2(workspace.right() + 340.0, workspace.bottom()),
        );
        WorkspaceLayoutInput::new(
            workspace,
            properties,
            true,
            cytobands
                .iter()
                .enumerate()
                .map(|(index, cytobands)| WorkspacePanelLayoutInput {
                    panel_id: rgv_core::PanelId::new(index as u64 + 1),
                    contig_name: format!("chr{}", index + 1),
                    contig_length: 3_000_000,
                    cytobands: *cytobands,
                })
                .collect(),
            metrics,
            2.0,
            1.0,
        )
    }

    fn shell_layout_input(
        window_width: f32,
        properties_width: f32,
        properties_expanded: bool,
        cytobands: &[CytobandAvailability],
        metrics: GenomeContextMetrics,
        display_scale: f32,
        accessibility_scale: f32,
    ) -> WorkspaceLayoutInput {
        let shell_top = 62.0;
        let shell_bottom = 776.0;
        let workspace_right = (window_width - properties_width).max(1.0);
        let workspace = egui::Rect::from_min_max(
            egui::pos2(0.0, shell_top),
            egui::pos2(workspace_right, shell_bottom),
        );
        let properties = egui::Rect::from_min_max(
            egui::pos2(workspace_right, shell_top),
            egui::pos2(window_width, shell_bottom),
        );
        let mut input = WorkspaceLayoutInput::new(
            workspace,
            properties,
            properties_expanded,
            cytobands
                .iter()
                .enumerate()
                .map(|(index, cytobands)| WorkspacePanelLayoutInput {
                    panel_id: rgv_core::PanelId::new(index as u64 + 1),
                    contig_name: if index == 0 {
                        "chrVeryLongAlternateContigNameForElision".to_owned()
                    } else {
                        format!("chr{}", index + 1)
                    },
                    contig_length: if index == 0 { u64::MAX / 2 } else { 80 },
                    cytobands: *cytobands,
                })
                .collect(),
            metrics,
            display_scale,
            accessibility_scale,
        );
        input.minimum_track_gutter_width = 72.0;
        input
    }

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
        let workspace = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_280.0, 800.0));
        let layout = WorkspaceLayoutSnapshot::compute(
            layout_input(
                workspace,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                GenomeContextMetrics::default(),
            ),
            1,
        );
        let first = layout.panels[0].canvas;
        let first_scrollbar = layout.panels[0].scrollbar;
        let second = layout.panels[1].canvas;
        let second_scrollbar = layout.panels[1].scrollbar;

        assert_eq!(first.right(), first_scrollbar.left());
        assert_eq!(first_scrollbar.right(), second.left());
        assert_eq!(second.right(), second_scrollbar.left());
        assert_eq!(second_scrollbar.right(), workspace.right());
        assert_eq!(first_scrollbar.width(), SCROLLBAR_WIDTH);
        assert_eq!(second_scrollbar.width(), SCROLLBAR_WIDTH);
        assert_eq!(first.width(), second.width());
    }

    #[test]
    fn physical_scissor_is_consistent_at_standard_and_retina_scale() {
        let layout = WorkspaceLayoutSnapshot::compute(
            shell_layout_input(
                1_280.0,
                340.0,
                true,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                GenomeContextMetrics::default(),
                2.0,
                1.0,
            ),
            1,
        );
        let standard = layout.physical_genomic_scissor(1.0, 1_280, 800);
        let retina = layout.physical_genomic_scissor(2.0, 2_560, 1_600);
        assert_eq!(retina.x, standard.x * 2);
        assert_eq!(retina.y, standard.y * 2);
        assert_eq!(retina.width, standard.width * 2);
        assert_eq!(retina.height, standard.height * 2);
        assert_eq!(standard.x, layout.panels[0].canvas.left().round() as u32);
        assert_eq!(
            standard.x + standard.width,
            layout.panels.last().unwrap().canvas.right().round() as u32
        );
    }

    #[test]
    fn workspace_layout_uses_identical_header_and_canvas_columns() {
        let workspace = egui::Rect::from_min_max(egui::pos2(0.0, 62.0), egui::pos2(1_250.0, 776.0));
        let layout = WorkspaceLayoutSnapshot::compute(
            layout_input(
                workspace,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                GenomeContextMetrics::default(),
            ),
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
        let metrics = GenomeContextMetrics::default();
        let absent = WorkspaceLayoutSnapshot::compute(
            layout_input(
                workspace,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                metrics,
            ),
            1,
        );
        let mixed = WorkspaceLayoutSnapshot::compute(
            layout_input(
                workspace,
                &[CytobandAvailability::Present, CytobandAvailability::Absent],
                metrics,
            ),
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
        for panel in &mixed.panels {
            let cytoband = panel.context_rows.cytoband.unwrap();
            assert_eq!(cytoband.left(), panel.context_rows.overview.left());
            assert_eq!(cytoband.right(), panel.context_rows.overview.right());
        }
    }

    #[test]
    fn workspace_layout_matrix_never_overlaps_rows_or_columns() {
        let default_metrics = GenomeContextMetrics::default();
        let enlarged_metrics = GenomeContextMetrics {
            metadata_height: default_metrics.metadata_height * 1.5,
            span_height: default_metrics.span_height * 1.5,
            ruler_height: default_metrics.ruler_height * 1.5,
            bar_height: default_metrics.bar_height * 1.25,
            row_gap: default_metrics.row_gap * 1.5,
            ..default_metrics
        };
        for window_width in [640.0, 800.0, 1_280.0, 2_560.0] {
            for panel_count in 1..=4 {
                let mut mixed = vec![CytobandAvailability::Absent; panel_count];
                mixed[0] = CytobandAvailability::Present;
                let mut loading = vec![CytobandAvailability::Absent; panel_count];
                loading[0] = CytobandAvailability::Loading;
                for cytobands in [
                    vec![CytobandAvailability::Absent; panel_count],
                    vec![CytobandAvailability::Present; panel_count],
                    mixed,
                    loading,
                ] {
                    for (properties_width, properties_expanded) in [
                        (30.0, false),
                        (280.0, true),
                        (340.0, true),
                        (400.0, true),
                        (520.0, true),
                    ] {
                        for (metrics, accessibility_scale) in
                            [(default_metrics, 1.0), (enlarged_metrics, 1.5)]
                        {
                            let input = shell_layout_input(
                                window_width,
                                properties_width,
                                properties_expanded,
                                &cytobands,
                                metrics,
                                2.0,
                                accessibility_scale,
                            );
                            let layout = WorkspaceLayoutSnapshot::compute(input, 1);
                            assert_eq!(layout.workspace.right(), layout.properties.left());
                            assert_eq!(
                                layout.properties.width(),
                                properties_width.min(window_width - 1.0)
                            );
                            assert_eq!(layout.properties_expanded, properties_expanded);
                            assert_eq!(layout.accessibility_scale, accessibility_scale);
                            assert_eq!(layout.genome_context.bottom(), layout.track_viewport.top());
                            assert!(layout.panels.windows(2).all(|pair| {
                                (pair[0].column.right() - pair[1].column.left()).abs() < 0.01
                            }));
                            assert_eq!(layout.panels[0].column.left(), layout.track_gutter.right());
                            assert_eq!(
                                layout.panels.last().unwrap().column.right(),
                                layout.workspace.right()
                            );
                            assert!(
                                (layout.projected_canvas_width(panel_count)
                                    - layout.panels[0].canvas.width())
                                .abs()
                                    < 0.01
                            );
                            for panel in &layout.panels {
                                assert_eq!(panel.header_genomic.left(), panel.canvas.left());
                                assert_eq!(panel.header_genomic.right(), panel.canvas.right());
                                assert_eq!(panel.header_scrollbar.left(), panel.scrollbar.left());
                                assert_eq!(panel.header_scrollbar.right(), panel.scrollbar.right());
                                assert_eq!(panel.canvas.top(), layout.track_viewport.top());
                                assert_eq!(panel.canvas.bottom(), layout.track_viewport.bottom());
                                let mut rows = vec![panel.context_rows.metadata];
                                rows.extend(panel.context_rows.cytoband);
                                rows.extend([
                                    panel.context_rows.overview,
                                    panel.context_rows.span,
                                    panel.context_rows.ruler,
                                ]);
                                assert!(
                                    rows.windows(2)
                                        .all(|pair| { pair[0].bottom() <= pair[1].top() })
                                );
                                assert!(
                                    rows.iter()
                                        .all(|row| { layout.genome_context.contains_rect(*row) })
                                );
                                assert!(
                                    panel.context_rows.metadata.bottom()
                                        <= panel.context_rows.ruler.top()
                                );
                                assert_eq!(
                                    layout
                                        .panel_at(panel.canvas.center())
                                        .map(|hit| hit.panel_id),
                                    Some(panel.panel_id)
                                );
                                assert!(layout.panel_at(panel.header.center()).is_none());
                                for scale in [1.0, 2.0] {
                                    let scissor = layout.physical_rect_scissor(
                                        panel.canvas,
                                        scale,
                                        (window_width * scale).ceil() as u32,
                                        (800.0 * scale).ceil() as u32,
                                    );
                                    assert!(scissor.width > 0);
                                    assert!(scissor.height > 0);
                                    assert!(
                                        scissor.x + scissor.width
                                            <= (window_width * scale).ceil() as u32
                                    );
                                    assert!(
                                        scissor.y + scissor.height <= (800.0 * scale).ceil() as u32
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cytoband_loading_transitions_reflow_once_to_each_stable_state() {
        let metrics = GenomeContextMetrics::default();
        let loading = WorkspaceLayoutSnapshot::compute(
            shell_layout_input(
                1_280.0,
                340.0,
                true,
                &[CytobandAvailability::Loading, CytobandAvailability::Absent],
                metrics,
                2.0,
                1.0,
            ),
            1,
        );
        let present = WorkspaceLayoutSnapshot::compute(
            shell_layout_input(
                1_280.0,
                340.0,
                true,
                &[CytobandAvailability::Present, CytobandAvailability::Absent],
                metrics,
                2.0,
                1.0,
            ),
            2,
        );
        let absent = WorkspaceLayoutSnapshot::compute(
            shell_layout_input(
                1_280.0,
                340.0,
                true,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                metrics,
                2.0,
                1.0,
            ),
            3,
        );

        assert_eq!(
            loading.genome_context.height(),
            present.genome_context.height()
        );
        assert_eq!(loading.track_viewport.top(), present.track_viewport.top());
        assert_eq!(
            loading.panels[1].context_rows.cytoband,
            present.panels[1].context_rows.cytoband
        );
        assert_eq!(
            loading.genome_context.height() - absent.genome_context.height(),
            metrics.bar_height + metrics.row_gap
        );
        assert!(
            absent
                .panels
                .iter()
                .all(|panel| panel.context_rows.cytoband.is_none())
        );
    }

    #[test]
    fn compatibility_geometry_is_a_one_way_projection_of_the_snapshot() {
        let layout = WorkspaceLayoutSnapshot::compute(
            shell_layout_input(
                1_280.0,
                340.0,
                true,
                &[CytobandAvailability::Absent, CytobandAvailability::Absent],
                GenomeContextMetrics::default(),
                2.0,
                1.0,
            ),
            7,
        );
        let geometry = layout.canvas_geometry();
        assert_eq!(geometry.viewport, layout.track_viewport);
        assert_eq!(geometry.gutter, layout.track_gutter);
        assert_eq!(geometry.scrollbar, layout.panels.last().unwrap().scrollbar);
        assert_eq!(geometry.genomic.left(), layout.panels[0].canvas.left());
        assert_eq!(
            geometry.genomic.right(),
            layout.panels.last().unwrap().canvas.right()
        );
        assert_eq!(
            layout.panels[0].contig_name,
            "chrVeryLongAlternateContigNameForElision"
        );
        assert_eq!(layout.panels[0].contig_length, u64::MAX / 2);
        assert_eq!(layout.panels[1].contig_length, 80);
    }
}
