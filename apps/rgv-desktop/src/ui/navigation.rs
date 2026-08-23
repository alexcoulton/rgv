use egui::{Align2, Color32, FontId, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use rgv_core::{CytobandStain, GenomeAssembly, Viewport};
use rgv_layout::{
    CoordinateRulerOptions, format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};

use super::TRACK_GUTTER_WIDTH;

pub const GENOME_CONTEXT_HEIGHT: f32 = 62.0;

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

#[allow(clippy::too_many_lines)]
pub fn paint_genome_context(
    ui: &mut Ui,
    assembly: &GenomeAssembly,
    contig_name: &str,
    contig_length: u64,
    viewport: Viewport,
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
        pos2(rect.left() + 8.0, rect.top() + 11.0),
        Align2::LEFT_CENTER,
        contig_name,
        FontId::proportional(12.0),
        Color32::from_gray(55),
    );

    let ideogram_rect = Rect::from_min_size(
        pos2(
            content.left() + 8.0,
            rect.top() + if show_gutter { 5.0 } else { 18.0 },
        ),
        vec2((content.width() - 16.0).max(1.0), 13.0),
    );
    let ideogram = layout_ideogram(
        assembly,
        viewport.interval().contig(),
        contig_length,
        viewport.interval(),
        ideogram_rect.width(),
    );
    let ideogram_response = ui
        .interact(
            ideogram_rect,
            egui::Id::new(("genome-context-ideogram", id_salt)),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::AllScroll)
        .on_hover_text(
            "Drag left/right to pan · drag down/up to zoom in/out · double-click to show all",
        );
    let visible_interval = viewport.interval();
    ideogram_response.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            visible_interval.start() as f64,
            format!(
                "Genome overview for {contig_name}; visible bases {} through {}. Arrow left and right pan, arrow down and up zoom in and out, Home shows the full contig",
                visible_interval.start().saturating_add(1),
                visible_interval.end()
            ),
        )
    });
    let mut keyboard_pan_fraction = None;
    let mut keyboard_zoom_factor = None;
    let mut keyboard_show_all = false;
    if ideogram_response.has_focus() {
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
    let visible = Rect::from_min_size(
        pos2(
            ideogram_rect.left() + ideogram.visible_x,
            ideogram_rect.top() - 1.0,
        ),
        vec2(ideogram.visible_width, ideogram_rect.height() + 2.0),
    );
    painter.rect_stroke(
        visible,
        1.0,
        Stroke::new(2.0, Color32::from_rgb(45, 92, 175)),
        StrokeKind::Outside,
    );
    if ideogram_response.has_focus() {
        painter.rect_stroke(
            ideogram_rect.expand(3.0),
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
        pos2(content.center().x, rect.top() + 25.0),
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

    let pointer_position = ideogram_response.interact_pointer_pos();
    let drag_delta = ideogram_response.drag_delta();
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
    let clicked_on_handle = ideogram_response.clicked()
        && pointer_position.is_some_and(|pointer| handle_hit_rect.contains(pointer));
    OverviewInteraction {
        clicked_position: (ideogram_response.clicked()
            && !ideogram_response.double_clicked()
            && !clicked_on_handle)
            .then_some(pointer_position)
            .flatten()
            .map(|value| ideogram_position_at_pointer(ideogram_rect, value.x, contig_length)),
        pointer_position: pointer_position
            .map(|value| ideogram_position_at_pointer(ideogram_rect, value.x, contig_length)),
        pointer_x: pointer_position.map(|value| f64::from(value.x)),
        pointer_y: pointer_position.map(|value| f64::from(value.y)),
        press_position: press_point
            .map(|value| ideogram_position_at_pointer(ideogram_rect, value.x, contig_length)),
        press_x: press_point.map(|value| f64::from(value.x)),
        press_y: press_point.map(|value| f64::from(value.y)),
        press_handle_fraction,
        pointer_down: ideogram_response.is_pointer_button_down_on(),
        drag_started: ideogram_response.drag_started(),
        dragging: ideogram_response.dragged(),
        drag_stopped: ideogram_response.drag_stopped(),
        show_all: ideogram_response.double_clicked() || keyboard_show_all,
        keyboard_pan_fraction,
        keyboard_zoom_factor,
    }
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
    fn genome_overview_exposes_a_named_accessibility_slider() {
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
                let _ = paint_genome_context(ui, &assembly, "chr1", 10_000, viewport, 1, true);
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.contains("Genome overview for chr1"))
                && node.numeric_value() == Some(100.0)
        }));
    }
}
