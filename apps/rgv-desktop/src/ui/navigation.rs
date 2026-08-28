use egui::{Align2, Color32, FontId, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use rgv_core::{CytobandStain, GenomeAssembly, Viewport};
use rgv_layout::{
    CoordinateRulerOptions, format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};

use super::{
    CytobandAvailability, RULER_BASELINE_BOTTOM_INSET, RULER_LABEL_BASELINE_GAP,
    WorkspacePanelLayout,
};

pub const GENOME_OVERVIEW_BINS: usize = 256;

fn ruler_tick_label_rect(ruler: Rect, x: f32, size: egui::Vec2) -> Rect {
    let baseline_y = ruler.bottom() - RULER_BASELINE_BOTTOM_INSET;
    Rect::from_min_size(
        pos2(x + 3.0, baseline_y - RULER_LABEL_BASELINE_GAP - size.y),
        size,
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenomeOverviewData {
    /// Exact depth loaded from alignment records.
    pub bins: Vec<f32>,
    /// Bins for which record-derived depth is authoritative, including zero.
    pub observed: Vec<bool>,
    /// Coarse occupancy inferred from BAM/CRAM index chunks. Index bins locate
    /// candidate file chunks, not the exact span of the records in them.
    pub estimated_bins: Vec<f32>,
}

impl Default for GenomeOverviewData {
    fn default() -> Self {
        Self {
            bins: vec![0.0; GENOME_OVERVIEW_BINS],
            observed: vec![false; GENOME_OVERVIEW_BINS],
            estimated_bins: vec![0.0; GENOME_OVERVIEW_BINS],
        }
    }
}

impl GenomeOverviewData {
    /// Adds a stable whole-contig occupancy estimate. Each source is
    /// logarithmically normalized before merging so file compression and track
    /// size do not allow one alignment track to erase the shape of another.
    pub fn add_index_density(&mut self, densities: &[u64]) {
        if densities.is_empty() || self.bins.is_empty() {
            return;
        }
        let maximum = densities.iter().copied().max().unwrap_or_default().max(1) as f32;
        let denominator = maximum.ln_1p();
        for target in 0..self.bins.len() {
            let source_start = target * densities.len() / self.bins.len();
            let source_end = ((target + 1) * densities.len())
                .div_ceil(self.bins.len())
                .min(densities.len());
            let density = densities[source_start..source_end]
                .iter()
                .copied()
                .max()
                .unwrap_or_default() as f32;
            self.estimated_bins[target] =
                self.estimated_bins[target].max(density.ln_1p() / denominator);
        }
    }

    pub fn merge_max(&mut self, other: &Self) {
        for ((density, observed), (other_density, other_observed)) in self
            .bins
            .iter_mut()
            .zip(&mut self.observed)
            .zip(other.bins.iter().zip(&other.observed))
        {
            *density = density.max(*other_density);
            *observed |= *other_observed;
        }
        for (estimate, other_estimate) in self.estimated_bins.iter_mut().zip(&other.estimated_bins)
        {
            *estimate = estimate.max(*other_estimate);
        }
    }

    pub fn add_depths(
        &mut self,
        contig_length: u64,
        interval: rgv_core::GenomicInterval,
        depths: impl ExactSizeIterator<Item = u32>,
    ) {
        if contig_length == 0 || interval.is_empty() || depths.len() == 0 {
            return;
        }
        let observed_start = overview_bin(interval.start(), contig_length, self.bins.len());
        let observed_end = overview_bin(
            interval.end().saturating_sub(1),
            contig_length,
            self.bins.len(),
        );
        for observed in &mut self.observed[observed_start..=observed_end] {
            *observed = true;
        }
        let depth_count = depths.len();
        for (index, depth) in depths.enumerate() {
            let numerator = (index as u128 * 2 + 1) * u128::from(interval.len());
            let denominator = depth_count as u128 * 2;
            let position = interval
                .start()
                .saturating_add(u64::try_from(numerator / denominator).unwrap_or(u64::MAX));
            let bin = overview_bin(position, contig_length, self.bins.len());
            self.bins[bin] = self.bins[bin].max(depth as f32);
        }
    }

    pub fn normalize_logarithmically(&mut self) {
        let maximum = self.bins.iter().copied().fold(0.0_f32, f32::max).ln_1p();
        if maximum <= f32::EPSILON {
            return;
        }
        for bin in &mut self.bins {
            *bin = bin.ln_1p() / maximum;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct OverviewInteraction {
    pub activate_requested: bool,
    pub clicked_position: Option<u64>,
    pub pointer_position: Option<u64>,
    pub pointer_x: Option<f64>,
    pub pointer_y: Option<f64>,
    pub press_position: Option<u64>,
    pub press_x: Option<f64>,
    pub press_y: Option<f64>,
    pub press_handle_fraction: Option<f64>,
    pub pointer_down: bool,
    pub drag_started: bool,
    pub dragging: bool,
    pub drag_stopped: bool,
    pub show_all: bool,
    pub keyboard_pan_fraction: Option<f64>,
    pub keyboard_zoom_factor: Option<f64>,
}

fn elided_text(painter: &egui::Painter, value: &str, font: &FontId, maximum_width: f32) -> String {
    if painter
        .layout_no_wrap(value.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x
        <= maximum_width
    {
        return value.to_owned();
    }
    let mut characters = value.chars().collect::<Vec<_>>();
    while !characters.is_empty() {
        characters.pop();
        let candidate = format!("{}…", characters.iter().collect::<String>());
        if painter
            .layout_no_wrap(candidate.clone(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= maximum_width
        {
            return candidate;
        }
    }
    String::new()
}

#[derive(Clone, Debug)]
struct MetadataTextLayout {
    name: String,
    name_rect: Rect,
    length: Option<String>,
    length_rect: Option<Rect>,
}

fn layout_metadata_text(
    painter: &egui::Painter,
    row: Rect,
    contig_name: &str,
    contig_length: u64,
    font: &FontId,
) -> MetadataTextLayout {
    let left = row.left() + 8.0_f32.min(row.width() * 0.5);
    let right = row.right() - 8.0_f32.min(row.width() * 0.5);
    let length_label = format!("{} bp", format_genomic_position(contig_length));
    let length_size = painter
        .layout_no_wrap(length_label.clone(), font.clone(), Color32::from_gray(90))
        .size();
    let complete_name_width = painter
        .layout_no_wrap(contig_name.to_owned(), font.clone(), Color32::from_gray(55))
        .size()
        .x;
    let available_width = (right - left).max(0.0);
    // The contig identity is the primary label. Suppress the total length
    // before forcing the name through elision merely to retain a lower-priority
    // value.
    let show_length = complete_name_width + length_size.x + 12.0 <= available_width;
    let name_available = if show_length {
        available_width - length_size.x - 12.0
    } else {
        available_width
    }
    .max(0.0);
    let name = elided_text(painter, contig_name, font, name_available);
    let name_size = painter
        .layout_no_wrap(name.clone(), font.clone(), Color32::from_gray(55))
        .size();
    let name_rect = Rect::from_min_size(pos2(left, row.center().y - name_size.y * 0.5), name_size);
    let (length, length_rect) = if show_length {
        (
            Some(length_label),
            Some(Rect::from_min_size(
                pos2(right - length_size.x, row.center().y - length_size.y * 0.5),
                length_size,
            )),
        )
    } else {
        (None, None)
    };
    MetadataTextLayout {
        name,
        name_rect,
        length,
        length_rect,
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn paint_genome_context(
    ui: &mut Ui,
    assembly: &GenomeAssembly,
    contig_name: &str,
    contig_length: u64,
    viewport: Viewport,
    overview_data: &GenomeOverviewData,
    id_salt: u64,
    layout: &WorkspacePanelLayout,
) -> OverviewInteraction {
    let rect = layout.header_genomic;
    let rows = layout.context_rows;
    let contig = viewport.interval().contig();
    let has_cytobands = assembly.cytobands_for(contig).next().is_some();
    debug_assert_eq!(
        has_cytobands,
        layout.cytobands == CytobandAvailability::Present
    );
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    let metadata_font = FontId::proportional(11.0);
    let panel_label = format!("Panel {id_salt}");
    let panel_font = FontId::proportional(10.0);
    let panel_size = painter
        .layout_no_wrap(panel_label.clone(), panel_font.clone(), Color32::WHITE)
        .size();
    let panel_label_rect = Rect::from_min_size(
        pos2(
            rows.metadata.left(),
            rows.metadata.center().y - panel_size.y * 0.5,
        ),
        panel_size,
    );
    painter.text(
        panel_label_rect.left_center(),
        Align2::LEFT_CENTER,
        &panel_label,
        panel_font,
        Color32::from_rgb(45, 92, 175),
    );
    let metadata_text_row = Rect::from_min_max(
        pos2(
            (panel_label_rect.right() + 9.0).min(rows.metadata.right()),
            rows.metadata.top(),
        ),
        rows.metadata.max,
    );
    let length_label = format!("{} bp", format_genomic_position(contig_length));
    let metadata = layout_metadata_text(
        &painter,
        metadata_text_row,
        contig_name,
        contig_length,
        &metadata_font,
    );
    if !metadata.name.is_empty() {
        painter.text(
            metadata.name_rect.left_center(),
            Align2::LEFT_CENTER,
            &metadata.name,
            metadata_font.clone(),
            Color32::from_gray(55),
        );
    }
    if let (Some(length), Some(rect)) = (&metadata.length, metadata.length_rect) {
        painter.text(
            rect.right_center(),
            Align2::RIGHT_CENTER,
            length,
            metadata_font.clone(),
            Color32::from_gray(90),
        );
    }
    let metadata_id = egui::Id::new(("genome-context-metadata", id_salt));
    let metadata_accessibility_label =
        format!("{panel_label}; Contig {contig_name}; total length {length_label}");
    let metadata_response = ui.interact(rows.metadata, metadata_id, egui::Sense::click());
    metadata_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            &metadata_accessibility_label,
        )
    });
    ui.ctx().accesskit_node_builder(metadata_id, |node| {
        node.set_role(egui::accesskit::Role::Button);
        node.set_label(metadata_accessibility_label.clone());
        node.set_bounds(egui::accesskit::Rect {
            x0: f64::from(rows.metadata.left()),
            y0: f64::from(rows.metadata.top()),
            x1: f64::from(rows.metadata.right()),
            y1: f64::from(rows.metadata.bottom()),
        });
    });
    let activate_requested = metadata_response.clicked();
    metadata_response.on_hover_text(format!(
        "Activate {panel_label} · {contig_name} · {length_label}"
    ));

    let overview_rect = rows.overview;
    let ideogram = layout_ideogram(
        assembly,
        contig,
        contig_length,
        viewport.interval(),
        overview_rect.width(),
    );
    debug_assert_eq!(ideogram.has_cytobands, has_cytobands);
    let overview_response = ui
        .interact(
            overview_rect,
            egui::Id::new(("genome-context-overview", id_salt)),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::AllScroll)
        .on_hover_text(
            "Read overview: blue is exact loaded coverage; grey ticks are coarse index estimates · drag left/right to pan · drag down/up to zoom in/out · double-click to show all",
        );
    let visible_interval = viewport.interval();
    overview_response.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            visible_interval.start() as f64,
            format!(
                "Coverage overview for {contig_name}; visible bases {} through {}. Arrow left and right pan, arrow down and up zoom in and out, Home shows the full contig",
                visible_interval.start().saturating_add(1),
                visible_interval.end()
            ),
        )
    });
    let mut keyboard_pan_fraction = None;
    let mut keyboard_zoom_factor = None;
    let mut keyboard_show_all = false;
    if overview_response.has_focus() {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                keyboard_pan_fraction = Some(-0.1);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                keyboard_pan_fraction = Some(0.1);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                keyboard_zoom_factor = Some(0.8);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                keyboard_zoom_factor = Some(1.25);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                keyboard_show_all = true;
            }
        });
    }
    if layout.cytobands == CytobandAvailability::Present
        && let Some(ideogram_rect) = rows.cytoband
    {
        for segment in &ideogram.segments {
            let segment_rect = Rect::from_min_size(
                pos2(ideogram_rect.left() + segment.x, ideogram_rect.top()),
                vec2(segment.width, ideogram_rect.height()),
            );
            painter.rect_filled(segment_rect, 0.0, stain_color(segment.stain));
            painter.rect_stroke(
                segment_rect,
                0.0,
                Stroke::new(0.75, Color32::from_gray(95)),
                StrokeKind::Inside,
            );
            if segment.width >= 38.0 && !segment.name.is_empty() {
                painter.text(
                    segment_rect.center(),
                    Align2::CENTER_CENTER,
                    &segment.name,
                    FontId::proportional(9.0),
                    Color32::from_gray(35),
                );
            }
        }
    } else if layout.cytobands == CytobandAvailability::Loading
        && let Some(cytoband_rect) = rows.cytoband
    {
        painter.text(
            cytoband_rect.left_center(),
            Align2::LEFT_CENTER,
            "Loading cytobands…",
            FontId::proportional(9.0),
            Color32::from_gray(105),
        );
    }
    painter.rect_filled(overview_rect, 1.0, Color32::from_gray(238));
    painter.rect_stroke(
        overview_rect,
        1.0,
        Stroke::new(0.75, Color32::from_gray(130)),
        StrokeKind::Inside,
    );
    paint_genome_overview_data(&painter, overview_rect, overview_data);
    let visible = Rect::from_min_size(
        pos2(
            overview_rect.left() + ideogram.visible_x,
            overview_rect.top() - 1.0,
        ),
        vec2(ideogram.visible_width, overview_rect.height() + 2.0),
    );
    painter.rect_stroke(
        visible,
        1.0,
        Stroke::new(2.0, Color32::from_rgb(45, 92, 175)),
        StrokeKind::Outside,
    );
    if overview_response.has_focus() {
        painter.rect_stroke(
            overview_rect.expand(3.0),
            2.0,
            Stroke::new(2.0, Color32::BLACK),
            StrokeKind::Outside,
        );
    }

    let ruler_viewport = viewport
        .resized(rows.ruler.width().max(1.0) as f64)
        .unwrap_or(viewport);
    let largest_tick_label = format_genomic_position(viewport.interval().end().max(contig_length));
    let ruler_font = FontId::proportional(10.0);
    let tick_label_width = painter
        .layout_no_wrap(
            largest_tick_label,
            ruler_font.clone(),
            Color32::from_gray(55),
        )
        .size()
        .x;
    let ruler = layout_coordinate_ruler(
        ruler_viewport,
        CoordinateRulerOptions {
            minimum_major_spacing: f64::from((tick_label_width + 12.0).max(48.0)),
            ..CoordinateRulerOptions::default()
        },
    );
    let ruler_left = rows.ruler.left();
    let baseline_y = rows.ruler.bottom() - RULER_BASELINE_BOTTOM_INSET;
    painter.line_segment(
        [
            pos2(ruler_left, baseline_y),
            pos2(rows.ruler.right(), baseline_y),
        ],
        Stroke::new(1.0, Color32::from_gray(95)),
    );
    painter.text(
        rows.span.center(),
        Align2::CENTER_CENTER,
        &ruler.span_label,
        FontId::proportional(11.0),
        Color32::from_gray(45),
    );
    let mut previous_label_right = f32::NEG_INFINITY;
    for tick in ruler.ticks {
        let x = ruler_left + tick.x;
        let height = if tick.major { 8.0 } else { 4.0 };
        painter.line_segment(
            [pos2(x, baseline_y), pos2(x, baseline_y - height)],
            Stroke::new(1.0, Color32::from_gray(100)),
        );
        if let Some(label) = tick.label {
            let galley = painter.layout_no_wrap(label, ruler_font.clone(), Color32::from_gray(55));
            let label_rect = ruler_tick_label_rect(rows.ruler, x, galley.size());
            if label_rect.left() >= previous_label_right + 6.0
                && label_rect.right() <= rows.ruler.right()
                && rows.ruler.contains_rect(label_rect)
            {
                painter.galley(label_rect.min, galley, Color32::from_gray(55));
                previous_label_right = label_rect.right();
            }
        }
    }

    let pointer_position = overview_response.interact_pointer_pos();
    let drag_delta = overview_response.drag_delta();
    let press_point = pointer_position.map(|value| value - drag_delta);
    let handle_hit_rect = visible.expand2(vec2(6.0, 3.0));
    let press_handle_fraction = press_point
        .filter(|point| handle_hit_rect.contains(*point))
        .map(|point| {
            if visible.contains(point) {
                f64::from(
                    ((point.x - visible.left()) / visible.width().max(f32::EPSILON))
                        .clamp(0.0, 1.0),
                )
            } else {
                0.5
            }
        });
    let clicked_on_handle = overview_response.clicked()
        && pointer_position.is_some_and(|pointer| handle_hit_rect.contains(pointer));
    OverviewInteraction {
        activate_requested,
        clicked_position: (overview_response.clicked()
            && !overview_response.double_clicked()
            && !clicked_on_handle)
            .then_some(pointer_position)
            .flatten()
            .map(|value| ideogram_position_at_pointer(overview_rect, value.x, contig_length)),
        pointer_position: pointer_position
            .map(|value| ideogram_position_at_pointer(overview_rect, value.x, contig_length)),
        pointer_x: pointer_position.map(|value| f64::from(value.x)),
        pointer_y: pointer_position.map(|value| f64::from(value.y)),
        press_position: press_point
            .map(|value| ideogram_position_at_pointer(overview_rect, value.x, contig_length)),
        press_x: press_point.map(|value| f64::from(value.x)),
        press_y: press_point.map(|value| f64::from(value.y)),
        press_handle_fraction,
        pointer_down: overview_response.is_pointer_button_down_on(),
        drag_started: overview_response.drag_started(),
        dragging: overview_response.dragged(),
        drag_stopped: overview_response.drag_stopped(),
        show_all: overview_response.double_clicked() || keyboard_show_all,
        keyboard_pan_fraction,
        keyboard_zoom_factor,
    }
}

fn paint_genome_overview_data(painter: &egui::Painter, rect: Rect, overview: &GenomeOverviewData) {
    if overview.bins.is_empty()
        || overview.bins.len() != overview.observed.len()
        || overview.bins.len() != overview.estimated_bins.len()
    {
        return;
    }
    let bin_width = rect.width() / overview.bins.len() as f32;
    // Keep index-only occupancy visibly distinct and deliberately slight. A
    // BAI bin can cover 16 kb even when its reads occupy only a few bases, so
    // painting it as full-height coverage materially exaggerates data extent.
    for (index, estimate) in overview.estimated_bins.iter().copied().enumerate() {
        if estimate <= f32::EPSILON || overview.observed[index] {
            continue;
        }
        let height = 1.0 + estimate.clamp(0.0, 1.0) * 2.0;
        painter.rect_filled(
            Rect::from_min_max(
                pos2(
                    rect.left() + index as f32 * bin_width,
                    rect.bottom() - height,
                ),
                pos2(
                    rect.left() + (index + 1) as f32 * bin_width + 0.5,
                    rect.bottom(),
                ),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(85, 101, 108, 105),
        );
    }
    for (index, density) in overview.bins.iter().copied().enumerate() {
        if density <= f32::EPSILON {
            continue;
        }
        let height = (2.0 + density.clamp(0.0, 1.0) * (rect.height() - 2.0)).min(rect.height());
        painter.rect_filled(
            Rect::from_min_max(
                pos2(
                    rect.left() + index as f32 * bin_width,
                    rect.bottom() - height,
                ),
                pos2(
                    rect.left() + (index + 1) as f32 * bin_width + 0.5,
                    rect.bottom(),
                ),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(16, 105, 145, 190),
        );
    }
}

fn overview_bin(position: u64, contig_length: u64, bins: usize) -> usize {
    if bins == 0 || contig_length == 0 {
        return 0;
    }
    usize::try_from(
        u128::from(position.min(contig_length.saturating_sub(1))) * bins as u128
            / u128::from(contig_length),
    )
    .unwrap_or(usize::MAX)
    .min(bins - 1)
}

fn ideogram_position_at_pointer(rect: Rect, pointer_x: f32, contig_length: u64) -> u64 {
    let fraction = ((pointer_x - rect.left()) / rect.width().max(f32::EPSILON)).clamp(0.0, 1.0);
    (f64::from(fraction) * contig_length.saturating_sub(1) as f64).round() as u64
}

fn stain_color(stain: CytobandStain) -> Color32 {
    match stain {
        CytobandStain::Negative => Color32::WHITE,
        CytobandStain::Positive25 => Color32::from_gray(205),
        CytobandStain::Positive50 => Color32::from_gray(155),
        CytobandStain::Positive75 => Color32::from_gray(95),
        CytobandStain::Positive100 => Color32::from_gray(30),
        CytobandStain::Centromere => Color32::from_rgb(210, 62, 55),
        CytobandStain::Variable => Color32::from_gray(190),
        CytobandStain::Stalk => Color32::from_rgb(175, 190, 205),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_panel_contig_length_is_measured_inside_metadata_and_clear_of_the_ruler() {
        let context = egui::Context::default();
        let metrics = super::super::GenomeContextMetrics::default();
        let workspace = super::super::WorkspaceLayoutSnapshot::compute(
            super::super::WorkspaceLayoutInput::new(
                Rect::from_min_max(pos2(0.0, 62.0), pos2(940.0, 776.0)),
                Rect::from_min_max(pos2(940.0, 62.0), pos2(1_280.0, 776.0)),
                true,
                vec![
                    super::super::WorkspacePanelLayoutInput {
                        panel_id: rgv_core::PanelId::new(1),
                        contig_name: "chr1".to_owned(),
                        contig_length: 3_000_000,
                        cytobands: super::super::CytobandAvailability::Absent,
                    },
                    super::super::WorkspacePanelLayoutInput {
                        panel_id: rgv_core::PanelId::new(2),
                        contig_name: "chr2".to_owned(),
                        contig_length: 3_000_000,
                        cytobands: super::super::CytobandAvailability::Absent,
                    },
                ],
                metrics,
                2.0,
                1.0,
            ),
            1,
        );
        let rows = workspace.panels[1].context_rows;
        let mut measured = None;
        let _ = context.run_ui(Default::default(), |ui| {
            measured = Some(layout_metadata_text(
                ui.painter(),
                rows.metadata,
                "chr2",
                3_000_000,
                &FontId::proportional(11.0),
            ));
        });
        let measured = measured.expect("metadata was measured");
        let length_rect = measured
            .length_rect
            .expect("3,000,000 bp fits in the second panel metadata row");
        let ruler_baseline = rows.ruler.bottom() - RULER_BASELINE_BOTTOM_INSET;

        assert_eq!(measured.length.as_deref(), Some("3,000,000 bp"));
        assert!(rows.metadata.contains_rect(measured.name_rect));
        assert!(rows.metadata.contains_rect(length_rect));
        assert!(!measured.name_rect.intersects(length_rect));
        assert!(length_rect.bottom() <= rows.metadata.bottom());
        assert!(rows.metadata.bottom() <= rows.ruler.top());
        assert!(length_rect.bottom() < ruler_baseline);
    }

    #[test]
    fn measured_ruler_row_contains_its_tick_labels() {
        let context = egui::Context::default();
        let mut geometry = None;
        let _ = context.run_ui(Default::default(), |ui| {
            let metrics = super::super::GenomeContextMetrics::measured(ui);
            let panel =
                Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, metrics.context_height(false)));
            let ruler = metrics.rows(panel, false).ruler;
            let galley = ui.painter().layout_no_wrap(
                "1,000,000".to_owned(),
                FontId::proportional(10.0),
                Color32::from_gray(55),
            );
            let label = ruler_tick_label_rect(ruler, ruler.left() + 100.0, galley.size());
            geometry = Some((ruler, label));
        });
        let (ruler, label) = geometry.expect("ruler geometry was measured");

        assert!(ruler.contains_rect(label));
    }

    #[test]
    fn constrained_metadata_suppresses_length_before_eliding_the_contig_name() {
        let context = egui::Context::default();
        let row = Rect::from_min_size(egui::Pos2::ZERO, vec2(100.0, 16.0));
        let mut measured = None;
        let _ = context.run_ui(Default::default(), |ui| {
            measured = Some(layout_metadata_text(
                ui.painter(),
                row,
                "chrVeryLongAlternateContigName",
                3_000_000,
                &FontId::proportional(11.0),
            ));
        });
        let measured = measured.expect("metadata was measured");

        assert!(measured.length.is_none());
        assert!(measured.length_rect.is_none());
        assert!(!measured.name.is_empty());
        assert!(row.contains_rect(measured.name_rect));
    }

    #[test]
    fn coverage_overview_click_uses_the_reflowed_snapshot_row() {
        let dictionary = rgv_core::ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 10_000),
            ("chr2".to_owned(), 10_000),
        ]);
        let assembly = GenomeAssembly::from_dictionary("test", dictionary);
        let interval =
            rgv_core::GenomicInterval::new(rgv_core::ContigId::new(1), 100, 200).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let metrics = super::super::GenomeContextMetrics::default();
        let layout = |workspace_right: f32, revision| {
            super::super::WorkspaceLayoutSnapshot::compute(
                super::super::WorkspaceLayoutInput::new(
                    Rect::from_min_max(pos2(0.0, 62.0), pos2(workspace_right, 776.0)),
                    Rect::from_min_max(pos2(workspace_right, 62.0), pos2(1_280.0, 776.0)),
                    true,
                    vec![
                        super::super::WorkspacePanelLayoutInput {
                            panel_id: rgv_core::PanelId::new(1),
                            contig_name: "chr1".to_owned(),
                            contig_length: 10_000,
                            cytobands: super::super::CytobandAvailability::Absent,
                        },
                        super::super::WorkspacePanelLayoutInput {
                            panel_id: rgv_core::PanelId::new(2),
                            contig_name: "chr2".to_owned(),
                            contig_length: 10_000,
                            cytobands: super::super::CytobandAvailability::Absent,
                        },
                    ],
                    metrics,
                    2.0,
                    1.0,
                ),
                revision,
            )
        };
        let previous = layout(1_250.0, 1);
        let current = layout(760.0, 2);
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(1_280.0, 800.0));
        fn paint_once(
            context: &egui::Context,
            input: egui::RawInput,
            assembly: &GenomeAssembly,
            viewport: Viewport,
            panel: &WorkspacePanelLayout,
        ) -> OverviewInteraction {
            let mut interaction = OverviewInteraction::default();
            let _ = context.run_ui(input, |ui| {
                interaction = paint_genome_context(
                    ui,
                    &assembly,
                    "chr2",
                    10_000,
                    viewport,
                    &GenomeOverviewData::default(),
                    2,
                    panel,
                );
            });
            interaction
        }
        let _ = paint_once(
            &context,
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            &assembly,
            viewport,
            &previous.panels[1],
        );
        let overview = current.panels[1].context_rows.overview;
        let pointer = pos2(
            overview.left() + overview.width() * 0.75,
            overview.center().y,
        );
        let _ = paint_once(
            &context,
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            },
            &assembly,
            viewport,
            &current.panels[1],
        );
        let interaction = paint_once(
            &context,
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            &assembly,
            viewport,
            &current.panels[1],
        );
        assert!(interaction.pointer_down);
        let interaction = paint_once(
            &context,
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            &assembly,
            viewport,
            &current.panels[1],
        );

        assert!(
            interaction
                .clicked_position
                .is_some_and(|position| { (7_450..=7_550).contains(&position) })
        );
    }

    #[test]
    fn ideogram_pointer_maps_across_the_full_contig() {
        let rect = Rect::from_min_max(pos2(100.0, 0.0), pos2(1_100.0, 20.0));
        assert_eq!(ideogram_position_at_pointer(rect, 100.0, 10_000), 0);
        assert_eq!(ideogram_position_at_pointer(rect, 600.0, 10_000), 5_000);
        assert_eq!(ideogram_position_at_pointer(rect, 1_100.0, 10_000), 9_999);
    }

    #[test]
    fn coverage_overview_exposes_a_named_accessibility_slider() {
        let dictionary = rgv_core::ReferenceDictionary::from_contigs([("chr1".to_owned(), 10_000)]);
        let assembly = GenomeAssembly::from_dictionary("test", dictionary);
        let interval =
            rgv_core::GenomicInterval::new(rgv_core::ContigId::new(0), 100, 200).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let metrics = super::super::GenomeContextMetrics::default();
        let context_height = metrics.context_height(false);
        let workspace = super::super::WorkspaceLayoutSnapshot::compute(
            super::super::WorkspaceLayoutInput::new(
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, context_height + 400.0)),
                Rect::from_min_size(
                    egui::pos2(900.0, 0.0),
                    egui::vec2(340.0, context_height + 400.0),
                ),
                true,
                vec![super::super::WorkspacePanelLayoutInput {
                    panel_id: rgv_core::PanelId::new(1),
                    contig_name: "chr1".to_owned(),
                    contig_length: 10_000,
                    cytobands: super::super::CytobandAvailability::Absent,
                }],
                metrics,
                1.0,
                1.0,
            ),
            1,
        );
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, context_height + 400.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = paint_genome_context(
                    ui,
                    &assembly,
                    "chr1",
                    10_000,
                    viewport,
                    &GenomeOverviewData::default(),
                    1,
                    &workspace.panels[0],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let slider = update.nodes.iter().find_map(|(_, node)| {
            (node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.contains("Coverage overview for chr1"))
                && node.numeric_value() == Some(100.0))
            .then_some(node)
        });
        let slider = slider.expect("coverage overview is present in the accessibility tree");
        let bounds = slider.bounds().expect("coverage overview has exact bounds");
        let overview = workspace.panels[0].context_rows.overview;
        assert!((bounds.x0 - f64::from(overview.left())).abs() < 0.01);
        assert!((bounds.y0 - f64::from(overview.top())).abs() < 0.01);
        assert!((bounds.x1 - f64::from(overview.right())).abs() < 0.01);
        assert!((bounds.y1 - f64::from(overview.bottom())).abs() < 0.01);
        let labels = update
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label())
            .collect::<Vec<_>>();
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Contig chr1; total length 10,000 bp")),
            "accessibility labels: {labels:?}"
        );
    }

    #[test]
    fn genome_overview_maps_and_normalizes_loaded_coverage() {
        let interval =
            rgv_core::GenomicInterval::new(rgv_core::ContigId::new(0), 100, 400).unwrap();
        let mut overview = GenomeOverviewData {
            bins: vec![0.0; 10],
            observed: vec![false; 10],
            estimated_bins: vec![0.0; 10],
        };
        overview.add_depths(1_000, interval, [0, 10, 100].into_iter());
        overview.normalize_logarithmically();

        assert!(overview.observed[1..4].iter().all(|observed| *observed));
        assert_eq!(overview.observed.iter().filter(|value| **value).count(), 3);
        assert!(overview.bins[1].abs() < f32::EPSILON);
        assert!(overview.bins[2] > 0.0 && overview.bins[2] < 1.0);
        assert!((overview.bins[3] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn index_density_populates_the_complete_overview_independently_of_viewport() {
        let mut overview = GenomeOverviewData::default();
        let mut density = vec![0_u64; GENOME_OVERVIEW_BINS];
        density[16] = 10;
        density[192] = 1_000;

        overview.add_index_density(&density);

        assert!(overview.observed.iter().all(|observed| !*observed));
        assert!(overview.estimated_bins[16] > 0.0);
        assert!((overview.estimated_bins[192] - 1.0).abs() < f32::EPSILON);
        assert!(overview.estimated_bins[100].abs() < f32::EPSILON);
    }

    #[test]
    fn exact_loaded_depth_masks_a_coarse_index_estimate() {
        let interval =
            rgv_core::GenomicInterval::new(rgv_core::ContigId::new(0), 100, 400).unwrap();
        let mut overview = GenomeOverviewData {
            bins: vec![0.0; 10],
            observed: vec![false; 10],
            estimated_bins: vec![1.0; 10],
        };

        overview.add_depths(1_000, interval, [0, 10, 0].into_iter());

        assert!(overview.observed[1..4].iter().all(|observed| *observed));
        assert!(overview.bins[1].abs() < f32::EPSILON);
        assert_eq!(overview.bins[2], 10.0);
        assert!(overview.bins[3].abs() < f32::EPSILON);
        assert_eq!(overview.estimated_bins, vec![1.0; 10]);
    }
}
