//! The desktop's active colour theme.
//!
//! [`rgv_render::palette::Theme`] defines each theme once: the alignment palette the GPU
//! renderers and the exports resolve against, the canvas colour and the chrome colours. This
//! module holds which theme is active and gives egui code two ways to use it:
//!
//! * named colours ([`DesktopTheme::canvas`], [`DesktopTheme::chrome_fill`], ...) for the canvas
//!   clear colour and the egui visuals;
//! * [`adapt`], which maps a colour authored for the light theme to its counterpart in the
//!   active theme. Under a light theme it is the identity, so chrome painted through it is
//!   pixel-identical to what it was before themes existed. Under a dark theme it mirrors the
//!   colour's lightness and keeps its hue, which turns near-white fills into near-canvas fills
//!   and dark text into light text without a hand-written dark value for every literal.
//!
//! The active theme is process-wide because the desktop has one window and its painting code
//! is a tree of free functions that share only an `egui::Ui`.

use std::sync::atomic::{AtomicU8, Ordering};

use egui::Color32;
use rgv_core::InterfaceTheme;
use rgv_render::palette::{AlignmentPalette, Theme, ThemeId};

static ACTIVE_THEME: AtomicU8 = AtomicU8::new(0);

pub(crate) const fn theme_id(theme: InterfaceTheme) -> ThemeId {
    match theme {
        InterfaceTheme::Light => ThemeId::Light,
        InterfaceTheme::Dark => ThemeId::Dark,
        InterfaceTheme::LightColorBlind => ThemeId::LightColorBlind,
        InterfaceTheme::DarkColorBlind => ThemeId::DarkColorBlind,
    }
}

pub(crate) const INTERFACE_THEMES: [InterfaceTheme; 4] = [
    InterfaceTheme::Light,
    InterfaceTheme::Dark,
    InterfaceTheme::LightColorBlind,
    InterfaceTheme::DarkColorBlind,
];

pub(crate) const fn theme_label(theme: InterfaceTheme) -> &'static str {
    theme_id(theme).label()
}

const fn encode(id: ThemeId) -> u8 {
    match id {
        ThemeId::Light => 0,
        ThemeId::Dark => 1,
        ThemeId::LightColorBlind => 2,
        ThemeId::DarkColorBlind => 3,
    }
}

const fn decode(value: u8) -> ThemeId {
    match value {
        1 => ThemeId::Dark,
        2 => ThemeId::LightColorBlind,
        3 => ThemeId::DarkColorBlind,
        _ => ThemeId::Light,
    }
}

/// Makes `theme` the theme every later [`active`] and [`adapt`] call sees.
pub(crate) fn set_active(theme: InterfaceTheme) {
    ACTIVE_THEME.store(encode(theme_id(theme)), Ordering::Relaxed);
}

/// The active theme.
pub(crate) fn active() -> DesktopTheme {
    DesktopTheme::new(decode(ACTIVE_THEME.load(Ordering::Relaxed)))
}

/// A colour authored for the light theme, as the active theme shows it.
pub(crate) fn adapt(color: Color32) -> Color32 {
    adapt_for(
        decode(ACTIVE_THEME.load(Ordering::Relaxed)).is_dark(),
        color,
    )
}

/// [`adapt`] for an explicit theme darkness: the identity when `dark` is false.
pub(crate) fn adapt_for(dark: bool, color: Color32) -> Color32 {
    if !dark {
        return color;
    }
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    let [red, green, blue] = mirror_lightness([red, green, blue]);
    Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

/// Lightness of the dark canvas and of the brightest dark-theme text, as HSL lightness. White
/// maps to the first and black to the second, linearly in between.
const DARK_LIGHTNESS_OF_WHITE: f32 = 0.118;
const DARK_LIGHTNESS_OF_BLACK: f32 = 0.94;

/// Mirrors the HSL lightness of an sRGB colour onto the dark theme's range, keeping hue and
/// saturation.
fn mirror_lightness(rgb: [u8; 3]) -> [u8; 3] {
    let [red, green, blue] = rgb.map(|channel| f32::from(channel) / 255.0);
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let lightness = (maximum + minimum) * 0.5;
    let chroma = maximum - minimum;
    let mirrored =
        DARK_LIGHTNESS_OF_BLACK + (DARK_LIGHTNESS_OF_WHITE - DARK_LIGHTNESS_OF_BLACK) * lightness;
    if chroma <= f32::EPSILON {
        let grey = (mirrored * 255.0).round() as u8;
        return [grey, grey, grey];
    }
    let saturation = chroma / (1.0 - (2.0 * lightness - 1.0).abs()).max(f32::EPSILON);
    let hue = if (maximum - red).abs() <= f32::EPSILON {
        ((green - blue) / chroma).rem_euclid(6.0)
    } else if (maximum - green).abs() <= f32::EPSILON {
        (blue - red) / chroma + 2.0
    } else {
        (red - green) / chroma + 4.0
    };
    let new_chroma = (1.0 - (2.0 * mirrored - 1.0).abs()) * saturation.min(1.0);
    let secondary = new_chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hue as u32 {
        0 => (new_chroma, secondary, 0.0),
        1 => (secondary, new_chroma, 0.0),
        2 => (0.0, new_chroma, secondary),
        3 => (0.0, secondary, new_chroma),
        4 => (secondary, 0.0, new_chroma),
        _ => (new_chroma, 0.0, secondary),
    };
    let offset = mirrored - new_chroma * 0.5;
    [r, g, b].map(|channel| ((channel + offset).clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// One theme as the desktop uses it: the shared [`Theme`] plus `Color32` views of its chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DesktopTheme {
    theme: Theme,
}

const fn opaque([red, green, blue]: [u8; 3]) -> Color32 {
    Color32::from_rgb(red, green, blue)
}

impl DesktopTheme {
    pub(crate) fn new(id: ThemeId) -> Self {
        Self {
            theme: Theme::new(id),
        }
    }

    pub(crate) const fn is_dark(&self) -> bool {
        self.theme.id.is_dark()
    }

    /// The palette the alignment renderers and exports resolve against.
    pub(crate) const fn palette(&self) -> &AlignmentPalette {
        &self.theme.palette
    }

    /// Clear colour of the genomic canvas render pass.
    pub(crate) fn canvas_clear(&self) -> wgpu::Color {
        let [r, g, b, a] = self.theme.canvas_clear();
        wgpu::Color { r, g, b, a }
    }

    pub(crate) const fn canvas(&self) -> Color32 {
        opaque(self.theme.canvas)
    }

    pub(crate) const fn chrome_fill(&self) -> Color32 {
        opaque(self.theme.chrome_fill)
    }

    pub(crate) const fn chrome_fill_strong(&self) -> Color32 {
        opaque(self.theme.chrome_fill_strong)
    }

    pub(crate) const fn text(&self) -> Color32 {
        opaque(self.theme.text)
    }

    pub(crate) const fn accent(&self) -> Color32 {
        opaque(self.theme.accent)
    }

    /// A light-theme colour as this theme shows it; see [`adapt`].
    pub(crate) fn adapt(&self, color: Color32) -> Color32 {
        adapt_for(self.is_dark(), color)
    }

    /// The `(base, dark-mode)` egui visuals of this theme, before accessibility adjustments.
    pub(crate) fn visuals(&self) -> egui::Visuals {
        if !self.is_dark() {
            return egui::Visuals::light();
        }
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = self.chrome_fill();
        visuals.window_fill = self.chrome_fill();
        visuals.extreme_bg_color = self.canvas();
        visuals.faint_bg_color = self.chrome_fill_strong();
        visuals.hyperlink_color = self.accent();
        visuals.widgets.noninteractive.fg_stroke.color = self.text();
        visuals.selection.bg_fill = self.accent().gamma_multiply(0.55);
        visuals
    }

    pub(crate) const fn egui_theme(&self) -> egui::Theme {
        if self.is_dark() {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lightness(color: Color32) -> f32 {
        let [red, green, blue, _] = color.to_srgba_unmultiplied();
        let channels = [red, green, blue].map(|channel| f32::from(channel) / 255.0);
        let maximum = channels.iter().copied().fold(0.0, f32::max);
        let minimum = channels.iter().copied().fold(1.0, f32::min);
        (maximum + minimum) * 0.5
    }

    #[test]
    fn light_themes_leave_every_colour_exactly_as_authored() {
        for color in [
            Color32::WHITE,
            Color32::BLACK,
            Color32::from_gray(250),
            Color32::from_rgb(45, 92, 175),
            Color32::from_rgba_unmultiplied(75, 91, 112, 42),
        ] {
            assert_eq!(adapt_for(false, color), color);
            for id in [ThemeId::Light, ThemeId::LightColorBlind] {
                assert_eq!(DesktopTheme::new(id).adapt(color), color);
            }
        }
        assert_eq!(DesktopTheme::new(ThemeId::Light).canvas(), Color32::WHITE);
        assert_eq!(
            DesktopTheme::new(ThemeId::Light).canvas_clear(),
            wgpu::Color::WHITE
        );
    }

    #[test]
    fn dark_adaptation_mirrors_lightness_and_keeps_hue_and_alpha() {
        let theme = DesktopTheme::new(ThemeId::Dark);
        // White chrome lands on the canvas colour; dark text becomes light text.
        let white = theme.adapt(Color32::WHITE);
        assert!((lightness(white) - lightness(theme.canvas())).abs() < 0.02);
        assert!(lightness(theme.adapt(Color32::from_gray(45))) > 0.75);
        assert!(lightness(theme.adapt(Color32::BLACK)) > 0.9);
        // Lightness order is reversed, so separations in the light chrome survive.
        let greys = [250, 236, 190, 125, 55, 30].map(|grey| theme.adapt(Color32::from_gray(grey)));
        for pair in greys.windows(2) {
            assert!(lightness(pair[0]) < lightness(pair[1]));
        }
        // The accent blue stays blue and readable on the dark canvas.
        let [red, green, blue, alpha] = theme
            .adapt(Color32::from_rgba_unmultiplied(45, 92, 175, 200))
            .to_srgba_unmultiplied();
        assert!(blue > green && green > red);
        assert_eq!(alpha, 200);
        assert!(lightness(Color32::from_rgb(red, green, blue)) > 0.45);
        // A pale tinted fill becomes a dark fill of the same tint.
        let [red, green, blue, _] = theme
            .adapt(Color32::from_rgb(234, 241, 248))
            .to_srgba_unmultiplied();
        assert!(blue >= green && green >= red && blue < 80);
    }

    #[test]
    fn every_theme_has_readable_chrome_and_matching_egui_visuals() {
        for theme in INTERFACE_THEMES {
            let desktop = DesktopTheme::new(theme_id(theme));
            assert_eq!(desktop.visuals().dark_mode, desktop.is_dark());
            assert_eq!(desktop.egui_theme() == egui::Theme::Dark, desktop.is_dark());
            let contrast = (lightness(desktop.text()) - lightness(desktop.canvas())).abs();
            assert!(contrast > 0.6, "{theme:?} text contrast {contrast}");
            assert_ne!(desktop.chrome_fill(), desktop.canvas());
            assert_eq!(
                *desktop.palette(),
                AlignmentPalette::for_theme(theme_id(theme))
            );
            assert!(!theme_label(theme).is_empty());
        }
        assert_eq!(theme_id(InterfaceTheme::default()), ThemeId::Light);
        assert_eq!(
            decode(encode(ThemeId::DarkColorBlind)),
            ThemeId::DarkColorBlind
        );
    }

    /// DOC-6: the dark-mode branches in the inspectors key off `ui.visuals().dark_mode`, which
    /// was always false while the desktop forced `Visuals::light()`. The theme setting now
    /// decides it, with or without the high-contrast adjustments.
    #[test]
    fn dark_themes_turn_on_egui_dark_mode_and_light_themes_keep_the_legacy_visuals() {
        for theme in INTERFACE_THEMES {
            let desktop = DesktopTheme::new(theme_id(theme));
            for high_contrast in [false, true] {
                let visuals = crate::themed_visuals(&desktop, high_contrast);
                assert_eq!(visuals.dark_mode, desktop.is_dark(), "{theme:?}");
                if high_contrast {
                    let ink = visuals.override_text_color.unwrap();
                    let contrast = (lightness(ink) - lightness(visuals.extreme_bg_color)).abs();
                    assert!(contrast > 0.8, "{theme:?} high contrast {contrast}");
                }
            }
            if !desktop.is_dark() {
                assert_eq!(
                    crate::themed_visuals(&desktop, false),
                    egui::Visuals::light()
                );
                let high = crate::themed_visuals(&desktop, true);
                assert_eq!(high.override_text_color, Some(Color32::BLACK));
                assert_eq!(high.extreme_bg_color, Color32::WHITE);
            }
        }
    }

    #[test]
    fn dark_exports_get_the_canvas_underneath_and_light_exports_are_untouched() {
        use rgv_layout::{DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive};
        let rectangle = |x: f32, color: Rgba| RectPrimitive {
            x,
            y: 1.0,
            width: 2.0,
            height: 3.0,
            color,
            object_id: 0,
            shape: RectShape::Rectangle,
        };
        let mut generic = DisplayList::default();
        generic
            .rectangles
            .push(rectangle(1.0, Rgba::new(0.2, 0.4, 0.6, 1.0)));
        generic.texts.push(TextPrimitive {
            x: 4.0,
            y: 5.0,
            text: "chr1".into(),
            color: Rgba::new(0.1, 0.1, 0.1, 1.0),
            font_size: 10.0,
        });
        let alignment = [rectangle(7.0, Rgba::new(0.9, 0.1, 0.1, 1.0))];

        let light = crate::automation::export::themed_export_display_list(
            generic.clone(),
            &alignment,
            40,
            30,
            &DesktopTheme::new(ThemeId::Light),
        );
        assert_eq!(light.rectangles, [generic.rectangles[0], alignment[0]]);
        assert_eq!(light.texts, generic.texts);

        let theme = DesktopTheme::new(ThemeId::Dark);
        let dark = crate::automation::export::themed_export_display_list(
            generic.clone(),
            &alignment,
            40,
            30,
            &theme,
        );
        assert_eq!(dark.rectangles.len(), 3);
        let canvas = dark.rectangles[0];
        assert_eq!(
            (canvas.x, canvas.y, canvas.width, canvas.height),
            (0.0, 0.0, 40.0, 30.0)
        );
        assert_eq!(
            crate::ui::alignment_settings::rgba_to_color32(canvas.color),
            theme.canvas()
        );
        assert_eq!(dark.rectangles[1..], [generic.rectangles[0], alignment[0]]);
        // Dark text authored for a white canvas is exported light.
        assert!(dark.texts[0].color.red > 0.8);
    }
}
