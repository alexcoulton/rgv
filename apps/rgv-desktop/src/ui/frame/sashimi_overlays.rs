//! Sashimi lane chrome painted over the GPU arcs: the "N weak junctions hidden" note, the
//! clearable splice-site / transcript filter chip, and the hovered junction's tooltip.

use crate::Viewer;
use crate::ui::frame::ShellFrame;
use crate::workspace::alignment_management::sashimi_presentation::{
    junction_tooltip_lines, site_filter_label,
};
use rgv_alignment_scene::AlignmentDerivedLaneOptions;

/// The note text for weak junctions a lane hides.
pub(crate) fn weak_junctions_note(hidden: usize) -> Option<String> {
    match hidden {
        0 => None,
        1 => Some("1 weak junction hidden".to_owned()),
        count => Some(format!("{count} weak junctions hidden")),
    }
}

impl Viewer {
    /// Paints every presented Sashimi lane's notes and filter chip, and the junction tooltip.
    /// A click on a filter chip clears that panel's junction filters (applied after the frame).
    pub(super) fn paint_sashimi_overlays(
        &self,
        ui: &mut egui::Ui,
        st: &mut ShellFrame,
        genomic: egui::Rect,
    ) {
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            return;
        };
        let painter = ui.painter().with_clip_rect(genomic);
        let note_color = crate::theme::adapt(egui::Color32::from_gray(96));
        let chip_fill =
            crate::theme::adapt(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 215));
        let font = egui::FontId::proportional(10.5);
        for (panel_id, frame) in &epoch.panel_frames {
            let Some(panel) = self.session.panels.get(*panel_id) else {
                continue;
            };
            for lane in frame.lanes.iter() {
                let Some(presentation) = self.sashimi_presentation(*panel_id, lane.track_id) else {
                    continue;
                };
                for derived in lane.derived_lanes.iter() {
                    if !matches!(derived.options, AlignmentDerivedLaneOptions::Sashimi(_)) {
                        continue;
                    }
                    let clip = egui::Rect::from_min_size(
                        egui::pos2(derived.clip.x as f32, derived.clip.y as f32),
                        egui::vec2(derived.clip.width as f32, derived.clip.height as f32),
                    );
                    let visible = clip.intersect(genomic);
                    if visible.height() < 14.0 || visible.width() < 120.0 {
                        continue;
                    }
                    let table = &presentation.table;
                    if let Some(note) = weak_junctions_note(table.weak_hidden()) {
                        let galley = painter.layout_no_wrap(note, font.clone(), note_color);
                        let origin = egui::pos2(
                            visible.right() - galley.size().x - 6.0,
                            visible.top() + 2.0,
                        );
                        painter.rect_filled(
                            egui::Rect::from_min_size(origin, galley.size()).expand(2.0),
                            3.0,
                            chip_fill,
                        );
                        painter.galley(origin, galley, note_color);
                    }
                    let filter_label = table
                        .site_filter()
                        .map(|site| site_filter_label(&panel.contig_name, site))
                        .or_else(|| {
                            panel
                                .sashimi_intron_filter
                                .as_ref()
                                .filter(|filter| filter.track_id == lane.track_id)
                                .map(|filter| {
                                    format!("Showing junctions of {} — clear", filter.label)
                                })
                        });
                    if let Some(label) = filter_label {
                        let response = egui::Area::new(egui::Id::new((
                            "sashimi-filter-chip",
                            panel_id.get(),
                            lane.track_id.get(),
                        )))
                        .order(egui::Order::Foreground)
                        .fixed_pos(egui::pos2(visible.left() + 6.0, visible.top() + 2.0))
                        .show(ui.ctx(), |ui| {
                            ui.add(
                                egui::Button::new(egui::RichText::new(label).size(10.5))
                                    .small()
                                    .corner_radius(8.0),
                            )
                            .on_hover_text("Show every junction again (Esc)")
                        })
                        .inner;
                        if response.clicked() {
                            st.sashimi_clear_filters = Some(*panel_id);
                        }
                    }
                }
            }
        }
        self.paint_junction_tooltip(ui, genomic);
    }

    fn paint_junction_tooltip(&self, ui: &egui::Ui, genomic: egui::Rect) {
        let panel = self.session.panels.active();
        let Some(hover) = panel.hovered_junction_arc else {
            return;
        };
        let Some(pointer) = ui.ctx().input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if !genomic.contains(pointer) {
            return;
        }
        let annotation = self
            .sashimi_hover_annotation
            .as_ref()
            .filter(|(key, _)| *key == (hover.key.track_id, hover.key.start, hover.key.end))
            .and_then(|(_, text)| text.as_deref());
        let count = if hover.evidence.depth == 1 {
            "1 read".to_owned()
        } else {
            format!("{} reads", hover.evidence.depth)
        };
        let mut lines = junction_tooltip_lines(
            &panel.contig_name,
            &hover.evidence,
            &hover.presentation,
            &count,
            annotation,
        );
        if let Some(site) = hover.splice_site {
            lines.push(format!(
                "Click to show only junctions at {}",
                crate::workspace::alignment_management::sashimi_presentation::grouped_position(
                    site.one_based_position()
                )
            ));
        }
        let tooltip_painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("sashimi-junction-tooltip"),
        ));
        let text_color = crate::theme::adapt(egui::Color32::from_gray(24));
        let galleys = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let font = if index == 0 {
                    egui::FontId::monospace(11.0)
                } else {
                    egui::FontId::proportional(11.0)
                };
                tooltip_painter.layout_no_wrap(line, font, text_color)
            })
            .collect::<Vec<_>>();
        let width = galleys
            .iter()
            .map(|galley| galley.size().x)
            .fold(0.0_f32, f32::max);
        let height = galleys
            .iter()
            .map(|galley| galley.size().y + 1.0)
            .sum::<f32>();
        let padding = egui::vec2(8.0, 6.0);
        let size = egui::vec2(width, height) + padding * 2.0;
        let screen = ui.ctx().content_rect();
        let mut origin = pointer + egui::vec2(14.0, 16.0);
        if origin.x + size.x > screen.right() {
            origin.x = pointer.x - size.x - 10.0;
        }
        if origin.y + size.y > screen.bottom() {
            origin.y = pointer.y - size.y - 10.0;
        }
        let rect = egui::Rect::from_min_size(origin, size);
        tooltip_painter.rect(
            rect,
            4.0,
            crate::theme::adapt(egui::Color32::from_rgba_unmultiplied(252, 252, 250, 245)),
            egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(170))),
            egui::StrokeKind::Inside,
        );
        let mut y = rect.top() + padding.y;
        for galley in galleys {
            let line_height = galley.size().y + 1.0;
            tooltip_painter.galley(egui::pos2(rect.left() + padding.x, y), galley, text_color);
            y += line_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weak_junction_note_counts_hidden_junctions() {
        assert_eq!(weak_junctions_note(0), None);
        assert_eq!(
            weak_junctions_note(1).as_deref(),
            Some("1 weak junction hidden")
        );
        assert_eq!(
            weak_junctions_note(28).as_deref(),
            Some("28 weak junctions hidden")
        );
    }
}
