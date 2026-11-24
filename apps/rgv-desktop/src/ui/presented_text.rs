//! Text painted from the presented alignment GPU bundle, after the egui pass.
//!
//! The egui pass records its shapes before the frame's commit phase and second display rebuild,
//! either of which may capture a new alignment camera frame. Read-level text recorded in that
//! pass would therefore describe an older capture than the one the GPU pass draws. Instead the
//! pass only records a [`PresentedTextSlot`] where that text belongs in z-order, and
//! `Viewer::submit_ui_frame` replaces the slot, after the last capture of the frame, with text
//! laid out from the bundle `render_composed_frame_target` is about to draw.
//!
//! Laying text out after the pass cannot use egui's own font atlas: glyphs rasterised then would
//! only reach the GPU with the next frame's texture delta. This painter owns a separate font
//! atlas and texture, so every glyph it uses is uploaded with the same frame.

use egui::epaint::text::{FontDefinitions, Fonts, TextOptions};
use egui::epaint::{
    ClippedPrimitive, ClippedShape, FontId, ImageDelta, PaintCallback, Primitive, Shape,
    TessellationOptions, Tessellator,
};
use egui::{Align2, Color32, Rect, TextureId, pos2};
use rgv_layout::{RectPrimitive, Rgba, TextPrimitive};
use std::sync::Arc;

/// The egui texture holding [`PresentedTextPainter`]'s font atlas.
pub(crate) const PRESENTED_TEXT_TEXTURE: TextureId = TextureId::User(0x5247_565f_5445_5854);

/// Marks where presented text is painted in the egui shape order. It never reaches a renderer:
/// [`splice_presented_text`] removes every slot.
#[derive(Debug)]
pub(crate) struct PresentedTextSlot;

pub(crate) fn presented_text_slot(rect: Rect) -> Shape {
    Shape::Callback(PaintCallback {
        rect,
        callback: Arc::new(PresentedTextSlot),
    })
}

fn is_presented_text_slot(primitive: &ClippedPrimitive) -> bool {
    matches!(
        &primitive.primitive,
        Primitive::Callback(callback) if callback.callback.downcast_ref::<PresentedTextSlot>().is_some()
    )
}

/// Replaces the first slot in `paint_jobs` with `text` and removes any other slot. Returns false,
/// leaving `paint_jobs` unchanged, when the pass recorded no slot (no genomic canvas was shown).
pub(crate) fn splice_presented_text(
    paint_jobs: &mut Vec<ClippedPrimitive>,
    text: Vec<ClippedPrimitive>,
) -> bool {
    let Some(index) = paint_jobs.iter().position(is_presented_text_slot) else {
        return false;
    };
    paint_jobs.splice(index..=index, text);
    paint_jobs.retain(|primitive| !is_presented_text_slot(primitive));
    true
}

/// Font inputs of the egui context the text must match.
pub(crate) struct PresentedTextFonts<D> {
    pub(crate) options: TextOptions,
    pub(crate) definitions: D,
    pub(crate) tessellation: TessellationOptions,
}

/// One frame of presented text: a font-atlas update to upload before drawing (if any) and the
/// primitives to splice into the egui paint jobs.
pub(crate) struct PresentedTextFrame {
    pub(crate) texture_delta: Option<ImageDelta>,
    pub(crate) primitives: Vec<ClippedPrimitive>,
}

#[derive(Default)]
pub(crate) struct PresentedTextPainter {
    fonts: Option<Fonts>,
}

impl PresentedTextPainter {
    /// Forgets the atlas, so the next frame uploads it in full. Call when the GPU renderer that
    /// held [`PRESENTED_TEXT_TEXTURE`] is replaced.
    pub(crate) fn reset(&mut self) {
        self.fonts = None;
    }

    /// Lays out `texts` centred on their anchors, in monospace, clipped to `clip`, exactly as
    /// [`crate::ui::paint_genomic_text`] would inside the pass, over `backdrops` (label chips,
    /// already in their final colour), and `overlays` (the read-hover highlight) over the text.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint(
        &mut self,
        backdrops: &[RectPrimitive],
        texts: &[TextPrimitive],
        overlays: Vec<ClippedShape>,
        clip: Rect,
        pixels_per_point: f32,
        fonts: PresentedTextFonts<impl FnOnce() -> FontDefinitions>,
        color: impl Fn(Rgba) -> Color32,
    ) -> PresentedTextFrame {
        let PresentedTextFonts {
            options,
            definitions,
            tessellation,
        } = fonts;
        let atlas_fonts = self
            .fonts
            .get_or_insert_with(|| Fonts::new(options, definitions()));
        // Flushes unused galleys, follows option changes, and starts a fresh atlas (uploaded in
        // full below) once the current one is nearly full.
        atlas_fonts.begin_pass(options);
        let shapes = {
            let mut view = atlas_fonts.with_pixels_per_point(pixels_per_point);
            let chips = backdrops.iter().map(|backdrop| {
                let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                ClippedShape {
                    clip_rect: clip,
                    shape: Shape::rect_filled(
                        Rect::from_min_size(
                            pos2(backdrop.x, backdrop.y),
                            egui::vec2(backdrop.width, backdrop.height),
                        ),
                        1.5,
                        Color32::from_rgba_unmultiplied(
                            channel(backdrop.color.red),
                            channel(backdrop.color.green),
                            channel(backdrop.color.blue),
                            channel(backdrop.color.alpha),
                        ),
                    ),
                }
            });
            let mut shapes = chips.collect::<Vec<_>>();
            shapes.extend(texts.iter().filter_map(|text| {
                let color = color(text.color);
                let galley = view.layout(
                    text.text.clone(),
                    FontId::monospace(text.font_size),
                    color,
                    f32::INFINITY,
                );
                if galley.is_empty() {
                    return None;
                }
                let rect = Align2::CENTER_CENTER.anchor_size(pos2(text.x, text.y), galley.size());
                Some(ClippedShape {
                    clip_rect: clip,
                    shape: Shape::galley(rect.min, galley, color),
                })
            }));
            shapes.extend(overlays);
            shapes
        };
        let texture_delta = atlas_fonts.font_image_delta();
        let atlas = atlas_fonts.texture_atlas();
        let mut primitives = Tessellator::new(
            pixels_per_point,
            tessellation,
            atlas.size(),
            atlas.prepared_discs(),
        )
        .tessellate_shapes(shapes);
        for primitive in &mut primitives {
            if let Primitive::Mesh(mesh) = &mut primitive.primitive {
                mesh.texture_id = PRESENTED_TEXT_TEXTURE;
            }
        }
        PresentedTextFrame {
            texture_delta,
            primitives,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::Mesh;

    fn mesh(texture_id: TextureId) -> ClippedPrimitive {
        ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Mesh(Mesh::with_texture(texture_id)),
        }
    }

    fn slot() -> ClippedPrimitive {
        let Shape::Callback(callback) = presented_text_slot(Rect::EVERYTHING) else {
            unreachable!("the slot is a callback shape");
        };
        ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Callback(callback),
        }
    }

    fn texture_ids(jobs: &[ClippedPrimitive]) -> Vec<Option<TextureId>> {
        jobs.iter()
            .map(|job| match &job.primitive {
                Primitive::Mesh(mesh) => Some(mesh.texture_id),
                Primitive::Callback(_) => None,
            })
            .collect()
    }

    fn fonts() -> PresentedTextFonts<impl FnOnce() -> FontDefinitions> {
        PresentedTextFonts {
            options: TextOptions::default(),
            definitions: FontDefinitions::default,
            tessellation: TessellationOptions::default(),
        }
    }

    fn text(text: &str, x: f32, y: f32) -> TextPrimitive {
        TextPrimitive {
            x,
            y,
            text: text.to_owned(),
            font_size: 11.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        }
    }

    #[test]
    fn presented_text_takes_the_slot_position_in_the_shape_order() {
        let below = TextureId::Managed(0);
        let above = TextureId::Managed(7);
        let mut jobs = vec![mesh(below), slot(), mesh(above), slot()];

        assert!(splice_presented_text(
            &mut jobs,
            vec![mesh(PRESENTED_TEXT_TEXTURE), mesh(PRESENTED_TEXT_TEXTURE)]
        ));

        assert_eq!(
            texture_ids(&jobs),
            [
                Some(below),
                Some(PRESENTED_TEXT_TEXTURE),
                Some(PRESENTED_TEXT_TEXTURE),
                Some(above)
            ]
        );
    }

    #[test]
    fn without_a_slot_nothing_is_painted() {
        let mut jobs = vec![mesh(TextureId::Managed(0))];
        assert!(!splice_presented_text(
            &mut jobs,
            vec![mesh(PRESENTED_TEXT_TEXTURE)]
        ));
        assert_eq!(texture_ids(&jobs), [Some(TextureId::Managed(0))]);
    }

    #[test]
    fn text_is_drawn_from_its_own_atlas_uploaded_in_the_same_frame() {
        let mut painter = PresentedTextPainter::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let first = painter.paint(
            &[],
            &[text("A", 20.0, 30.0)],
            Vec::new(),
            clip,
            2.0,
            fonts(),
            |_| Color32::BLACK,
        );
        let delta = first
            .texture_delta
            .expect("a new atlas is uploaded in full");
        assert!(delta.pos.is_none());
        assert!(!first.primitives.is_empty());
        for primitive in &first.primitives {
            assert_eq!(primitive.clip_rect, clip);
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                panic!("text tessellates to meshes");
            };
            assert_eq!(mesh.texture_id, PRESENTED_TEXT_TEXTURE);
            let bounds = mesh.calc_bounds();
            // Centred on its anchor, as `paint_genomic_text` centres it.
            assert!((bounds.center().x - 20.0).abs() < 2.0);
            assert!((bounds.center().y - 30.0).abs() < 4.0);
        }

        // A glyph at a size never laid out before is rasterised now and its texels are part of
        // this frame's update, not deferred to a later one.
        let resized = TextPrimitive {
            font_size: 9.37,
            ..text("7", 50.0, 60.0)
        };
        let second = painter.paint(&[], &[resized], Vec::new(), clip, 2.0, fonts(), |_| {
            Color32::BLACK
        });
        assert!(second.texture_delta.is_some());
        // Nothing new: nothing to upload.
        let third = painter.paint(
            &[],
            &[text("A", 80.0, 30.0)],
            Vec::new(),
            clip,
            2.0,
            fonts(),
            |_| Color32::BLACK,
        );
        assert!(third.texture_delta.is_none());

        painter.reset();
        let after_reset = painter.paint(
            &[],
            &[text("A", 20.0, 30.0)],
            Vec::new(),
            clip,
            2.0,
            fonts(),
            |_| Color32::BLACK,
        );
        assert!(
            after_reset
                .texture_delta
                .is_some_and(|delta| delta.pos.is_none())
        );
    }

    /// A label chip is painted in its own colour, beneath the text laid out over it, in the
    /// same atlas texture.
    #[test]
    fn label_chips_are_painted_beneath_their_text() {
        let mut painter = PresentedTextPainter::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let backdrop = RectPrimitive {
            x: 100.0,
            y: 20.0,
            width: 16.0,
            height: 11.0,
            color: Rgba::new(0.55, 0.2, 0.7, 1.0),
            object_id: 0,
            shape: rgv_layout::RectShape::Rectangle,
        };
        let frame = painter.paint(
            &[backdrop],
            &[text("+T", 108.0, 25.5)],
            Vec::new(),
            clip,
            2.0,
            fonts(),
            |_| Color32::WHITE,
        );
        let purple = Color32::from_rgba_unmultiplied(140, 51, 179, 255);
        let vertices = frame
            .primitives
            .iter()
            .flat_map(|primitive| match &primitive.primitive {
                Primitive::Mesh(mesh) => {
                    assert_eq!(mesh.texture_id, PRESENTED_TEXT_TEXTURE);
                    mesh.vertices.clone()
                }
                Primitive::Callback(_) => Vec::new(),
            })
            .collect::<Vec<_>>();
        let first_text = vertices
            .iter()
            .position(|vertex| vertex.color == Color32::WHITE)
            .expect("the text is painted");
        // The chip's fill (its anti-aliased rim fades to transparent) precedes all text.
        let chip_vertices = vertices[..first_text]
            .iter()
            .filter(|vertex| vertex.color == purple)
            .collect::<Vec<_>>();
        assert!(!chip_vertices.is_empty());
        assert!(
            vertices[..first_text]
                .iter()
                .all(|vertex| vertex.color == purple || vertex.color.a() == 0)
        );
        let (left, right) = chip_vertices
            .iter()
            .fold((f32::MAX, f32::MIN), |(l, r), v| {
                (l.min(v.pos.x), r.max(v.pos.x))
            });
        assert!(
            (left - 100.0).abs() < 1.0 && (right - 116.0).abs() < 1.0,
            "{left}..{right}"
        );
    }
}
