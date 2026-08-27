//! Font-independent application icons.
//!
//! Icon-only controls must be painted from geometry instead of being encoded
//! as Unicode button text. The default egui text fonts do not promise glyph
//! coverage for arrows, dingbats, or miscellaneous technical symbols, and
//! platform font fallback is neither portable nor deterministic.

use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, Widget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppIcon {
    MoveLeft,
    MoveRight,
    Close,
    Previous,
    Next,
    Home,
    ZoomOut,
    ZoomIn,
    ChevronLeft,
    ChevronRight,
    Settings,
    StatusDot,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IconButton {
    icon: AppIcon,
    accessible_label: &'static str,
    compact: bool,
}

impl IconButton {
    pub(crate) const fn new(icon: AppIcon, accessible_label: &'static str) -> Self {
        Self {
            icon,
            accessible_label,
            compact: false,
        }
    }

    pub(crate) const fn small(mut self) -> Self {
        self.compact = true;
        self
    }
}

impl Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let side = if self.compact {
            18.0
        } else {
            ui.spacing().interact_size.y
        };
        let response = egui::Button::new("").min_size(Vec2::splat(side)).ui(ui);
        let visuals = ui.style().interact(&response);
        let icon_rect = response.rect.shrink(if self.compact { 3.0 } else { 4.0 });
        paint_icon(ui.painter(), icon_rect, self.icon, visuals.fg_stroke.color);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                self.accessible_label,
            )
        });
        response
    }
}

pub(crate) fn icon_swatch(ui: &mut Ui, color: Color32, accessible_label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    paint_icon(ui.painter(), rect, AppIcon::StatusDot, color);
    response
        .on_hover_text(accessible_label)
        .on_hover_cursor(egui::CursorIcon::Default)
}

pub(crate) fn paint_icon(painter: &Painter, target: Rect, icon: AppIcon, color: Color32) {
    let side = target.width().min(target.height()).max(1.0);
    let rect = Rect::from_center_size(target.center(), Vec2::splat(side));
    let unit = side / 20.0;
    let point = |x: f32, y: f32| -> Pos2 {
        rect.center() + Vec2::new((x - 10.0) * unit, (y - 10.0) * unit)
    };
    let stroke = Stroke::new((1.7 * unit).clamp(1.25, 2.0), color);
    let line = |from: (f32, f32), to: (f32, f32)| {
        painter.line_segment([point(from.0, from.1), point(to.0, to.1)], stroke);
    };

    match icon {
        AppIcon::MoveLeft => {
            line((4.0, 10.0), (17.0, 10.0));
            line((4.0, 10.0), (9.0, 5.0));
            line((4.0, 10.0), (9.0, 15.0));
        }
        AppIcon::MoveRight => {
            line((3.0, 10.0), (16.0, 10.0));
            line((16.0, 10.0), (11.0, 5.0));
            line((16.0, 10.0), (11.0, 15.0));
        }
        AppIcon::Close => {
            line((5.0, 5.0), (15.0, 15.0));
            line((15.0, 5.0), (5.0, 15.0));
        }
        AppIcon::Previous => {
            painter.add(egui::epaint::Shape::convex_polygon(
                vec![point(14.0, 4.0), point(5.0, 10.0), point(14.0, 16.0)],
                color,
                Stroke::NONE,
            ));
        }
        AppIcon::Next => {
            painter.add(egui::epaint::Shape::convex_polygon(
                vec![point(6.0, 4.0), point(15.0, 10.0), point(6.0, 16.0)],
                color,
                Stroke::NONE,
            ));
        }
        AppIcon::Home => {
            line((3.0, 10.0), (10.0, 4.0));
            line((10.0, 4.0), (17.0, 10.0));
            line((5.0, 9.0), (5.0, 17.0));
            line((5.0, 17.0), (15.0, 17.0));
            line((15.0, 17.0), (15.0, 9.0));
            line((9.0, 17.0), (9.0, 12.0));
            line((9.0, 12.0), (12.5, 12.0));
            line((12.5, 12.0), (12.5, 17.0));
        }
        AppIcon::ZoomOut => line((4.0, 10.0), (16.0, 10.0)),
        AppIcon::ZoomIn => {
            line((4.0, 10.0), (16.0, 10.0));
            line((10.0, 4.0), (10.0, 16.0));
        }
        AppIcon::ChevronLeft => {
            line((13.0, 4.0), (7.0, 10.0));
            line((7.0, 10.0), (13.0, 16.0));
        }
        AppIcon::ChevronRight => {
            line((7.0, 4.0), (13.0, 10.0));
            line((13.0, 10.0), (7.0, 16.0));
        }
        AppIcon::Settings => {
            for y in [5.0, 10.0, 15.0] {
                line((3.0, y), (17.0, y));
            }
            for (x, y) in [(7.0, 5.0), (13.0, 10.0), (8.5, 15.0)] {
                painter.circle_filled(point(x, y), 2.0 * unit, color);
            }
        }
        AppIcon::StatusDot => {
            painter.circle_filled(rect.center(), side * 0.38, color);
        }
    }
}

#[cfg(test)]
mod tests {
    fn rust_sources_below(directory: &std::path::Path, sources: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("application source directory is readable")
        {
            let path = entry.expect("application source entry is readable").path();
            if path.is_dir() {
                rust_sources_below(&path, sources);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.ends_with("ui/icons.rs")
            {
                sources.push(path);
            }
        }
    }

    #[test]
    fn icon_only_controls_do_not_regress_to_font_glyphs() {
        let forbidden = [
            "Button::new(\"←\")",
            "Button::new(\"→\")",
            "Button::new(\"×\")",
            "Button::new(\"◀\")",
            "Button::new(\"▶\")",
            "button(\"⌂\")",
            "button(\"−\")",
            "button(\"+\")",
            "button(\"›\")",
            "button(\"‹\")",
            "\"⚙\"",
            "\"●\"",
            "⌘",
            "↔",
            "↕",
            "↑",
            "↓",
            "→",
        ];
        let mut sources = Vec::new();
        rust_sources_below(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut sources,
        );
        for path in sources {
            let source = std::fs::read_to_string(&path).expect("application source is readable");
            for glyph_control in forbidden {
                assert!(
                    !source.contains(glyph_control),
                    "{} uses the font-backed icon control {glyph_control}",
                    path.display()
                );
            }
        }
    }
}
