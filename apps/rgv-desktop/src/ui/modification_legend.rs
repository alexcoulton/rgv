//! The read stack's base-modification legend: a compact chip at the right of a read panel's
//! header strip (the row above its first read, beside the track's name in the gutter), listing
//! the modification codes present in the loaded reads and the colours they are drawn in. It
//! never covers a read; its hover text explains that colour strength is the call probability.
//! An export of the window draws the same chip at the same place.

use std::collections::HashMap;
use std::sync::Arc;

use rgv_alignment_scene::{AlignmentSceneSnapshot, GeometryLod};
use rgv_core::{PanelId, TrackId};
use rgv_hts::ModificationCode;
use rgv_layout::{DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive};
use rgv_render::modification_style::{
    MODIFICATION_LEGEND_HELP, ModificationLegendEntry, modification_legend,
};
use rgv_render::palette::{AlignmentPalette, PaletteToken};

use crate::Viewer;

pub(crate) const LEGEND_FONT_SIZE: f32 = 10.5;
const CHIP_HEIGHT: f32 = 16.0;
const SWATCH_SIZE: f32 = 9.0;
/// A generous estimate of one character's advance in the legend font, shared by the window and
/// the export so both lay the chip out identically.
const CHARACTER_WIDTH: f32 = LEGEND_FONT_SIZE * 0.6;
const PADDING: f32 = 6.0;
const SWATCH_GAP: f32 = 3.0;
const ITEM_GAP: f32 = 9.0;
/// Space kept between the chip and the right edge of the panel column.
const RIGHT_MARGIN: f32 = 8.0;
/// Space the chip leaves at the left of the header strip.
const MINIMUM_LEFT_ROOM: f32 = 24.0;
/// Records whose `MM` headers are read to learn the codes present when no summary has them.
const CODE_SCAN_RECORDS: usize = 10_000;
/// The swatch outline, so a pale colour stays visible on a pale chip.
const SWATCH_OUTLINE: [f32; 4] = [0.45, 0.45, 0.48, 1.0];

/// One placed legend item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModificationLegendItem {
    pub(crate) swatch: egui::Rect,
    pub(crate) token: PaletteToken,
    pub(crate) label: String,
    pub(crate) label_center: egui::Pos2,
}

/// A legend chip laid out in a header strip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModificationLegendLayout {
    pub(crate) chip: egui::Rect,
    pub(crate) items: Vec<ModificationLegendItem>,
}

fn label_width(label: &str) -> f32 {
    label.chars().count() as f32 * CHARACTER_WIDTH
}

/// Lays out `entries` as a chip right-aligned in `header`, the read panel's header strip in the
/// genomic column, vertically centred in it. `None` when there is nothing to show or the strip
/// is too narrow or too short to hold the whole chip: a partial legend would mislead.
pub(crate) fn modification_legend_layout(
    entries: &[ModificationLegendEntry],
    header: egui::Rect,
) -> Option<ModificationLegendLayout> {
    if entries.is_empty() || header.height() < CHIP_HEIGHT {
        return None;
    }
    let content = entries
        .iter()
        .map(|entry| SWATCH_SIZE + SWATCH_GAP + label_width(&entry.label))
        .sum::<f32>()
        + ITEM_GAP * (entries.len() - 1) as f32;
    let width = content + 2.0 * PADDING;
    let right = header.right() - RIGHT_MARGIN;
    let left = right - width;
    if left < header.left() + MINIMUM_LEFT_ROOM {
        return None;
    }
    let chip = egui::Rect::from_min_size(
        egui::pos2(left, header.center().y - CHIP_HEIGHT * 0.5),
        egui::vec2(width, CHIP_HEIGHT),
    );
    let mut x = chip.left() + PADDING;
    let items = entries
        .iter()
        .map(|entry| {
            let swatch = egui::Rect::from_min_size(
                egui::pos2(x, chip.center().y - SWATCH_SIZE * 0.5),
                egui::vec2(SWATCH_SIZE, SWATCH_SIZE),
            );
            let text_left = swatch.right() + SWATCH_GAP;
            let text_width = label_width(&entry.label);
            x = text_left + text_width + ITEM_GAP;
            ModificationLegendItem {
                swatch,
                token: entry.token,
                label: entry.label.clone(),
                label_center: egui::pos2(text_left + text_width * 0.5, chip.center().y),
            }
        })
        .collect();
    Some(ModificationLegendLayout { chip, items })
}

/// The modification codes present in one presented scene, remembered per panel and track until
/// the scene changes.
#[derive(Debug, Default)]
pub(crate) struct ModificationLegendCodes {
    entries: HashMap<(PanelId, TrackId), (Arc<AlignmentSceneSnapshot>, Vec<ModificationCode>)>,
}

impl ModificationLegendCodes {
    fn codes(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        snapshot: &Arc<AlignmentSceneSnapshot>,
    ) -> Vec<ModificationCode> {
        if let Some((cached, codes)) = self.entries.get(&(panel_id, track_id))
            && Arc::ptr_eq(cached, snapshot)
        {
            return codes.clone();
        }
        let mut codes = std::collections::BTreeSet::new();
        for tile in snapshot.summaries().modification_tiles() {
            codes.extend(tile.summary().series.iter().map(|series| series.key.code));
        }
        if codes.is_empty()
            && let Ok(declared) = snapshot.reads().declared_modification_codes(
                CODE_SCAN_RECORDS,
                rgv_layout::MAX_HIDDEN_MODIFICATION_CODES,
            )
        {
            codes.extend(declared);
        }
        let codes = codes.into_iter().collect::<Vec<_>>();
        self.entries
            .insert((panel_id, track_id), (Arc::clone(snapshot), codes.clone()));
        codes
    }

    /// Forgets scenes no longer presented.
    fn retain(&mut self, live: &[(PanelId, TrackId)]) {
        self.entries.retain(|key, _| live.contains(key));
    }
}

/// The legend of one read panel, placed.
pub(crate) struct PlacedModificationLegend {
    pub(crate) panel_id: PanelId,
    pub(crate) track_id: TrackId,
    pub(crate) layout: ModificationLegendLayout,
}

impl Viewer {
    /// The modification legend of every presented read panel that draws modification colours,
    /// placed in its header strip within `genomic`.
    pub(crate) fn read_panel_modification_legends(
        &self,
        genomic: egui::Rect,
    ) -> Vec<PlacedModificationLegend> {
        let mut legends = Vec::new();
        let mut live = Vec::new();
        let mut cache = self.modification_legend_codes.borrow_mut();
        for panel in self.session.panels.iter() {
            let Some(display) = panel.scroll_display_cache.as_ref() else {
                continue;
            };
            let column = self.panel_rect(panel.id);
            for read_panel in &display.read_panels {
                if !read_panel.density_camera {
                    continue;
                }
                let Some(snapshot) = self
                    .presented_alignment_lane(panel.id, read_panel.track_id)
                    .and_then(|presented| presented.lane.snapshot.clone())
                else {
                    continue;
                };
                let presentation = snapshot.reads().presentation();
                let style = presentation.style;
                if !style.show_modified_bases
                    || !matches!(
                        presentation.geometry_lod,
                        GeometryLod::Read | GeometryLod::Base
                    )
                {
                    continue;
                }
                // The header strip must be wholly in view: a strip scrolled under the pane top
                // would put the chip over reads.
                let header_top = read_panel.content_top - super::TRACK_HEADER_HEIGHT;
                if header_top < read_panel.clip_top - 0.5
                    || read_panel.content_top > read_panel.clip_bottom
                {
                    continue;
                }
                let header = egui::Rect::from_min_max(
                    egui::pos2(column.left(), header_top),
                    egui::pos2(column.right(), read_panel.content_top),
                )
                .intersect(genomic);
                live.push((panel.id, read_panel.track_id));
                let codes = cache
                    .codes(panel.id, read_panel.track_id, &snapshot)
                    .into_iter()
                    .filter(|code| !style.hidden_modification_codes.contains(&Some(*code)))
                    .collect::<Vec<_>>();
                let entries = modification_legend(&codes, style.modified_base_color_mode);
                if let Some(layout) = modification_legend_layout(&entries, header) {
                    legends.push(PlacedModificationLegend {
                        panel_id: panel.id,
                        track_id: read_panel.track_id,
                        layout,
                    });
                }
            }
        }
        cache.retain(&live);
        legends
    }

    /// Paints every read panel's modification legend over the canvas, with its explanation on
    /// hover.
    pub(crate) fn paint_modification_legends(&self, ui: &mut egui::Ui, genomic: egui::Rect) {
        let theme = crate::theme::active();
        let palette = theme.palette();
        let painter = ui.painter().with_clip_rect(genomic);
        let font = egui::FontId::proportional(LEGEND_FONT_SIZE);
        for legend in self.read_panel_modification_legends(genomic) {
            let layout = &legend.layout;
            painter.rect(
                layout.chip,
                3.0,
                theme.chrome_fill(),
                egui::Stroke::new(1.0, theme.chrome_fill_strong()),
                egui::StrokeKind::Inside,
            );
            for item in &layout.items {
                painter.rect_filled(item.swatch.expand(0.75), 1.0, color32(SWATCH_OUTLINE));
                painter.rect_filled(item.swatch, 1.0, color32(palette.color(item.token)));
                painter.text(
                    item.label_center,
                    egui::Align2::CENTER_CENTER,
                    &item.label,
                    font.clone(),
                    theme.text(),
                );
            }
            ui.interact(
                layout.chip,
                egui::Id::new((
                    "modification-legend",
                    legend.panel_id.get(),
                    legend.track_id.get(),
                )),
                egui::Sense::hover(),
            )
            .on_hover_text(MODIFICATION_LEGEND_HELP);
        }
    }
}

fn color32([red, green, blue, alpha]: [f32; 4]) -> egui::Color32 {
    crate::ui::alignment_settings::rgba_to_color32(Rgba::new(red, green, blue, alpha))
}

/// The chips of `legends` as export primitives: chip and swatches in `palette` and `chip_fill`,
/// labels in the light theme's text colour (an export recolours text for a dark theme, as the
/// window does).
pub(crate) fn modification_legend_display(
    legends: &[PlacedModificationLegend],
    palette: &AlignmentPalette,
    chip_fill: [f32; 4],
) -> DisplayList {
    let rgba = |[red, green, blue, alpha]: [f32; 4]| Rgba::new(red, green, blue, alpha);
    let rectangle = |rect: egui::Rect, color: [f32; 4]| RectPrimitive {
        x: rect.left(),
        y: rect.top(),
        width: rect.width(),
        height: rect.height(),
        color: rgba(color),
        object_id: 0,
        shape: RectShape::Rectangle,
    };
    let [red, green, blue] = rgv_render::palette::Theme::new(rgv_render::palette::ThemeId::Light)
        .text
        .map(|channel| f32::from(channel) / 255.0);
    let mut display = DisplayList::default();
    for legend in legends {
        display
            .rectangles
            .push(rectangle(legend.layout.chip, chip_fill));
        for item in &legend.layout.items {
            display
                .rectangles
                .push(rectangle(item.swatch.expand(0.75), SWATCH_OUTLINE));
            display
                .rectangles
                .push(rectangle(item.swatch, palette.color(item.token)));
            display.texts.push(TextPrimitive {
                x: item.label_center.x,
                y: item.label_center.y,
                text: item.label.clone(),
                font_size: LEGEND_FONT_SIZE,
                color: Rgba::new(red, green, blue, 1.0),
            });
        }
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<ModificationLegendEntry> {
        modification_legend(
            &[
                ModificationCode::Letter(b'm'),
                ModificationCode::Letter(b'a'),
            ],
            rgv_core::ModifiedBaseColorMode::ByType,
        )
    }

    #[test]
    fn the_legend_chip_sits_inside_the_header_strip_and_never_over_reads() {
        let header = egui::Rect::from_min_max(egui::pos2(100.0, 200.0), egui::pos2(900.0, 222.0));
        let layout = modification_legend_layout(&entries(), header).unwrap();
        assert!(header.contains_rect(layout.chip), "{:?}", layout.chip);
        // The first read row starts at the strip's bottom edge.
        assert!(layout.chip.bottom() <= header.bottom());
        assert!((layout.chip.right() - (header.right() - RIGHT_MARGIN)).abs() < 1.0e-3);
        assert_eq!(
            layout
                .items
                .iter()
                .map(|item| (item.label.as_str(), item.token))
                .collect::<Vec<_>>(),
            vec![
                ("5mC", PaletteToken::MODIFICATION_5MC),
                ("6mA", PaletteToken::MODIFICATION_6MA),
                ("unmodified", PaletteToken::MODIFICATION_UNMODIFIED),
            ]
        );
        for pair in layout.items.windows(2) {
            let first_label_right = pair[0].label_center.x + label_width(&pair[0].label) * 0.5;
            assert!(first_label_right < pair[1].swatch.left());
        }
        for item in &layout.items {
            assert!(layout.chip.contains_rect(item.swatch));
            assert!(layout.chip.contains(item.label_center));
        }
        // Too narrow for the whole chip: no partial legend.
        let narrow = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(120.0, 22.0));
        assert!(modification_legend_layout(&entries(), narrow).is_none());
        // Nothing to explain.
        assert!(modification_legend_layout(&[], header).is_none());
    }

    #[test]
    fn the_exported_legend_matches_the_window_layout() {
        let header = egui::Rect::from_min_max(egui::pos2(100.0, 200.0), egui::pos2(900.0, 222.0));
        let layout = modification_legend_layout(&entries(), header).unwrap();
        let palette = AlignmentPalette::dark();
        let display = modification_legend_display(
            &[PlacedModificationLegend {
                panel_id: PanelId::new(1),
                track_id: TrackId::new(1),
                layout: layout.clone(),
            }],
            &palette,
            [0.1, 0.1, 0.1, 1.0],
        );
        assert_eq!(display.texts.len(), 3);
        assert_eq!(display.rectangles.len(), 1 + 2 * 3);
        assert_eq!(display.rectangles[0].x, layout.chip.left());
        let swatch = &display.rectangles[2];
        assert_eq!(
            [
                swatch.color.red,
                swatch.color.green,
                swatch.color.blue,
                swatch.color.alpha
            ],
            palette.color(PaletteToken::MODIFICATION_5MC)
        );
        assert_eq!(display.texts[2].text, "unmodified");
    }
}
