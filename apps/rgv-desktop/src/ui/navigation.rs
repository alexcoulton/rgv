use egui::{Align2, Color32, FontId, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use rgv_core::{CytobandStain, GenomeAssembly, Viewport};
use rgv_layout::{
    CoordinateRulerOptions, format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};

use super::TRACK_GUTTER_WIDTH;

pub const GENOME_CONTEXT_HEIGHT: f32 = 79.0;
pub const GENOME_OVERVIEW_BINS: usize = 256;
const CONTIG_LABEL_ROW_HEIGHT: f32 = 14.0;
const GENOME_CONTEXT_BAR_HEIGHT: f32 = 10.0;
const GENOME_CONTEXT_BAR_GAP: f32 = 3.0;
const GENOME_CONTEXT_SPAN_LABEL_Y: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct GenomeContextRows {
    label: Rect,
    ideogram: Option<Rect>,
    overview: Rect,
    span_label_y: f32,
}

fn genome_context_rows(content: Rect, top: f32, has_cytobands: bool) -> GenomeContextRows {
    let label = Rect::from_min_max(
        pos2(content.left(), top),
        pos2(content.right(), top + CONTIG_LABEL_ROW_HEIGHT),
    );
    let first_bar = Rect::from_min_size(
        pos2(content.left() + 8.0, label.bottom()),
        vec2((content.width() - 16.0).max(1.0), GENOME_CONTEXT_BAR_HEIGHT),
    );
    let ideogram = has_cytobands.then_some(first_bar);
    let overview_top = ideogram.map_or(first_bar.top(), |rect| {
        rect.bottom() + GENOME_CONTEXT_BAR_GAP
    });
    let overview = Rect::from_min_size(pos2(first_bar.left(), overview_top), first_bar.size());
    GenomeContextRows {
        label,
        ideogram,
        overview,
        span_label_y: top + GENOME_CONTEXT_SPAN_LABEL_Y,
    }
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn paint_genome_context(
    ui: &mut Ui,
    assembly: &GenomeAssembly,
    contig_name: &str,
    contig_length: u64,
    viewport: Viewport,
    overview_data: &GenomeOverviewData,
    id_salt: u64,
    show_gutter: bool,
) -> OverviewInteraction {
    let rect = ui.max_rect();
    let gutter_width = if show_gutter {
        TRACK_GUTTER_WIDTH.min((rect.width() * 0.35).max(72.0))
    } else {
        0.0
    };
    let content = Rect::from_min_max(pos2(rect.left() + gutter_width, rect.top()), rect.max);
    let contig = viewport.interval().contig();
    let has_cytobands = assembly.cytobands_for(contig).next().is_some();
    let rows = genome_context_rows(content, rect.top(), has_cytobands);
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    if show_gutter {
        painter.line_segment(
            [
                pos2(content.left(), rect.top()),
                pos2(content.left(), rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_gray(190)),
        );
    }
    painter.text(
        pos2(rect.left() + 8.0, rows.label.center().y),
        Align2::LEFT_CENTER,
        contig_name,
        FontId::proportional(11.0),
        Color32::from_gray(55),
    );

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
    if let Some(ideogram_rect) = rows.ideogram {
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
    }
    painter.rect_filled(overview_rect, 1.0, Color32::from_gray(238));
    painter.rect_stroke(
        overview_rect,
        1.0,
        Stroke::new(0.75, Color32::from_gray(130)),
        StrokeKind::Inside,
    );
    paint_genome_overview_data(painter, overview_rect, overview_data);
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

    let ruler = layout_coordinate_ruler(viewport, CoordinateRulerOptions::default());
    let ruler_left = content.left();
    let baseline_y = rect.bottom() - 7.0;
    painter.line_segment(
        [
            pos2(ruler_left, baseline_y),
            pos2(content.right(), baseline_y),
        ],
        Stroke::new(1.0, Color32::from_gray(95)),
    );
    painter.text(
        pos2(content.center().x, rows.span_label_y),
        Align2::CENTER_CENTER,
        &ruler.span_label,
        FontId::proportional(11.0),
        Color32::from_gray(45),
    );
    for tick in ruler.ticks {
        let x = ruler_left + tick.x;
        let height = if tick.major { 8.0 } else { 4.0 };
        painter.line_segment(
            [pos2(x, baseline_y), pos2(x, baseline_y - height)],
            Stroke::new(1.0, Color32::from_gray(100)),
        );
        if let Some(label) = tick.label {
            painter.text(
                pos2(x + 3.0, baseline_y - 10.0),
                Align2::LEFT_BOTTOM,
                label,
                FontId::proportional(10.0),
                Color32::from_gray(55),
            );
        }
    }
    painter.text(
        pos2(rect.left() + 8.0, rect.bottom() - 10.0),
        Align2::LEFT_CENTER,
        format!("{} bp", format_genomic_position(contig_length)),
        FontId::proportional(10.0),
        Color32::from_gray(105),
    );

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
    fn ideogram_pointer_maps_across_the_full_contig() {
        let rect = Rect::from_min_max(pos2(100.0, 0.0), pos2(1_100.0, 20.0));
        assert_eq!(ideogram_position_at_pointer(rect, 100.0, 10_000), 0);
        assert_eq!(ideogram_position_at_pointer(rect, 600.0, 10_000), 5_000);
        assert_eq!(ideogram_position_at_pointer(rect, 1_100.0, 10_000), 9_999);
    }

    #[test]
    fn genome_context_omits_missing_cytobands_and_reserves_the_contig_label_row() {
        let content = Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, GENOME_CONTEXT_HEIGHT));
        let without_cytobands = genome_context_rows(content, content.top(), false);

        assert!(without_cytobands.ideogram.is_none());
        assert_eq!(
            without_cytobands.overview.top(),
            without_cytobands.label.bottom()
        );
        assert!(without_cytobands.overview.bottom() < without_cytobands.span_label_y);

        let with_cytobands = genome_context_rows(content, content.top(), true);
        let ideogram = with_cytobands.ideogram.unwrap();
        assert_eq!(ideogram.top(), with_cytobands.label.bottom());
        assert!(ideogram.bottom() < with_cytobands.overview.top());
        assert!(with_cytobands.overview.bottom() < with_cytobands.span_label_y);
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
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, GENOME_CONTEXT_HEIGHT),
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
                    true,
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.contains("Coverage overview for chr1"))
                && node.numeric_value() == Some(100.0)
        }));
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
