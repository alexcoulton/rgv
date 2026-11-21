use rgv_layout::{DisplayList, RectPrimitive, RectShape, Rgba};

use super::{
    FORMAT, Oracle, Quad, Shade, rectangle_fragment_coverage, render_image, shade_with_probe,
    test_gpu,
};
use crate::RectangleRenderer;

const SELECTED_TINT: ([f64; 3], f64) = ([0.16, 0.42, 0.92], 0.58);
const HOVERED_TINT: ([f64; 3], f64) = ([0.28, 0.52, 0.94], 0.26);

/// Paints one screen-space rectangle instance the way the shared rectangle fragment stage does.
/// `rectangle` is in logical points; `scale` is pixels per point.
pub(super) fn paint_rectangle(
    oracle: &mut Oracle,
    rectangle: RectPrimitive,
    scale: f64,
    scissor: [u32; 4],
    selected: Option<u32>,
    hovered: Option<u32>,
) {
    let origin = [f64::from(rectangle.x), f64::from(rectangle.y)];
    let size = [f64::from(rectangle.width), f64::from(rectangle.height)];
    let quad = Quad {
        x: origin[0] * scale,
        y: origin[1] * scale,
        width: size[0] * scale,
        height: size[1] * scale,
    };
    let base = [
        f64::from(rectangle.color.red),
        f64::from(rectangle.color.green),
        f64::from(rectangle.color.blue),
        f64::from(rectangle.color.alpha),
    ];
    let tint = [(selected, SELECTED_TINT), (hovered, HOVERED_TINT)]
        .into_iter()
        .find(|(id, _)| rectangle.object_id != 0 && *id == Some(rectangle.object_id))
        .map(|(_, tint)| tint);
    oracle.paint(quad, scissor, |x, y| match quad.classify(x, y) {
        Some(false) => Shade::Outside,
        None => Shade::Uncertain,
        Some(true) => shade_with_probe(x, y, |x, y| {
            let local = [x / scale - origin[0], y / scale - origin[1]];
            let coverage = rectangle_fragment_coverage(rectangle.shape as u32, local, size)?;
            Some(match tint {
                Some((target, amount)) => [
                    base[0] + (target[0] - base[0]) * amount,
                    base[1] + (target[1] - base[1]) * amount,
                    base[2] + (target[2] - base[2]) * amount,
                    1.0,
                ],
                None => [base[0], base[1], base[2], base[3] * coverage],
            })
        }),
    });
}

pub(super) fn shape_gallery() -> Vec<RectPrimitive> {
    let rectangle = |x, y, width, height, color, object_id, shape| RectPrimitive {
        x,
        y,
        width,
        height,
        color,
        object_id,
        shape,
    };
    let red = Rgba::new(0.86, 0.12, 0.10, 1.0);
    let blue = Rgba::new(0.10, 0.25, 0.85, 0.5);
    let green = Rgba::new(0.15, 0.62, 0.22, 1.0);
    let arc = Rgba::new(0.84, 0.30, 0.38, 0.82);
    vec![
        rectangle(3.3, 4.6, 20.4, 7.2, red, 1, RectShape::Rectangle),
        // Translucent, overlapping the first: blending order matters.
        rectangle(12.7, 8.1, 18.0, 9.5, blue, 2, RectShape::Rectangle),
        rectangle(
            40.25,
            5.0,
            40.0,
            10.0,
            green,
            3,
            RectShape::DirectionalTerminalRight,
        ),
        rectangle(
            90.5,
            5.5,
            37.0,
            9.0,
            green,
            4,
            RectShape::DirectionalTerminalLeft,
        ),
        // Selected and hovered objects.
        rectangle(
            40.25,
            20.0,
            40.0,
            10.0,
            green,
            5,
            RectShape::DirectionalTerminalRight,
        ),
        rectangle(90.5, 20.5, 37.0, 9.0, green, 6, RectShape::Rectangle),
        rectangle(4.2, 22.3, 6.0, 6.0, red, 7, RectShape::ChevronLeft),
        rectangle(14.6, 22.3, 6.0, 6.0, red, 8, RectShape::ChevronRight),
        rectangle(24.1, 21.7, 8.0, 8.0, red, 9, RectShape::BreakendJoinBefore),
        rectangle(132.4, 21.7, 8.0, 8.0, red, 10, RectShape::BreakendJoinAfter),
        rectangle(6.5, 36.0, 60.0, 24.0, arc, 11, RectShape::JunctionArcUp),
        rectangle(72.0, 36.5, 70.0, 20.0, arc, 12, RectShape::JunctionArcDown),
        rectangle(
            6.5,
            64.0,
            60.0,
            24.0,
            arc,
            13,
            RectShape::JunctionArcHoverUp,
        ),
        rectangle(
            72.0,
            64.5,
            70.0,
            26.0,
            arc,
            14,
            RectShape::JunctionArcSelectedDown,
        ),
        // Narrower than one pixel at 1x: either drawn in full or not at all.
        rectangle(150.3, 4.0, 0.6, 80.0, red, 15, RectShape::Rectangle),
    ]
}

#[test]
fn rectangle_pipeline_pixels_match_the_cpu_oracle_at_integer_and_fractional_scales() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let screen = [160_u32, 96];
    let rectangles = shape_gallery();
    let display = DisplayList {
        rectangles: rectangles.clone(),
        texts: Vec::new(),
    };
    let mut renderer = RectangleRenderer::new(&gpu.device, FORMAT);
    for scale in [1.0_f64, 2.0, 2.5] {
        let size = [
            (f64::from(screen[0]) * scale) as u32,
            (f64::from(screen[1]) * scale) as u32,
        ];
        renderer.prepare(
            &gpu.device,
            &gpu.queue,
            [screen[0] as f32, screen[1] as f32],
            &display,
            Some(5),
            Some(6),
        );
        assert_eq!(renderer.dropped_rectangles(), 0);
        let image = render_image(&gpu, size, |pass| {
            renderer.render(pass);
            Ok::<(), std::convert::Infallible>(())
        });
        let mut oracle = Oracle::new(size);
        for rectangle in &rectangles {
            paint_rectangle(
                &mut oracle,
                *rectangle,
                scale,
                [0, 0, size[0], size[1]],
                Some(5),
                Some(6),
            );
        }
        let comparison = oracle.assert_matches(&image, &format!("rectangle pipeline at {scale}x"));
        eprintln!(
            "rectangle pipeline at {scale}x on {}: {} pixels compared, {} painted, {} boundary \
             pixels excluded",
            gpu.description, comparison.compared, comparison.painted, comparison.uncertain
        );

        // The neutral pass ignores selection and hover.
        let neutral = render_image(&gpu, size, |pass| {
            renderer.render_unhighlighted(pass);
            Ok::<(), std::convert::Infallible>(())
        });
        let mut oracle = Oracle::new(size);
        for rectangle in &rectangles {
            paint_rectangle(
                &mut oracle,
                *rectangle,
                scale,
                [0, 0, size[0], size[1]],
                None,
                None,
            );
        }
        oracle.assert_matches(&neutral, &format!("neutral rectangle pipeline at {scale}x"));
    }
}
