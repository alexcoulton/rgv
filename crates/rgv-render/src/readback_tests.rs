//! GPU readback goldens: every pipeline is rendered into an offscreen texture on a real (or
//! software) adapter, the pixels are read back, and they are compared with an image rasterised
//! on the CPU from the crate's projection oracles.
//!
//! # Tolerance
//!
//! * Geometry: a pixel is compared unless its centre lies within [`EDGE_BAND`] pixels of a shape
//!   boundary, where the rasteriser's coverage decision legitimately depends on the last bit of
//!   an f32 projection. Everything else must agree, so an edge displaced by more than
//!   `EDGE_BAND` of a pixel fails the test.
//! * Colour: compared channels must agree within [`CHANNEL_TOLERANCE`] of 255 (8-bit target
//!   quantisation at every blend step plus f32 interpolation).
//!
//! # Adapters
//!
//! [`test_gpu`] uses a hardware adapter when one exists and the fallback (software) adapter
//! otherwise. With no adapter at all the tests print a notice and pass vacuously; set
//! `RGV_REQUIRE_GPU_TESTS=1` to turn that skip into a failure.

use image::RgbaImage;

use crate::gpu_export::{GpuExportLayout, GpuExportTarget};

mod context;
mod export;
mod rectangle;
mod scene;
mod shaders;
mod summary;

/// Colour target used by the goldens: linear 8-bit, so readback bytes are `round(colour * 255)`.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Half-width, in pixels, of the band around a shape boundary that is not compared.
pub(crate) const EDGE_BAND: f64 = 0.02;
/// Largest accepted per-channel difference, out of 255.
pub(crate) const CHANNEL_TOLERANCE: u8 = 2;

pub(crate) struct TestGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub description: String,
}

/// Creates a headless device, preferring hardware and falling back to a software adapter.
pub(crate) fn test_gpu() -> Option<TestGpu> {
    let instance = wgpu::Instance::default();
    let adapter = [false, true]
        .into_iter()
        .find_map(|force_fallback_adapter| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter,
                compatible_surface: None,
            }))
            .ok()
        });
    let Some(adapter) = adapter else {
        return skipped("no wgpu adapter (hardware or fallback) is available");
    };
    let info = adapter.get_info();
    let description = format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend);
    match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("RGV GPU readback test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    })) {
        Ok((device, queue)) => {
            eprintln!("rgv-render GPU readback test running on {description}");
            Some(TestGpu {
                device,
                queue,
                description,
            })
        }
        Err(error) => skipped(&format!("{description} refused a device: {error}")),
    }
}

fn skipped(reason: &str) -> Option<TestGpu> {
    assert!(
        std::env::var_os("RGV_REQUIRE_GPU_TESTS").is_none_or(|value| value == "0"),
        "RGV_REQUIRE_GPU_TESTS is set but {reason}"
    );
    eprintln!("rgv-render GPU readback test SKIPPED: {reason}");
    None
}

/// Renders one pass into a white `size_pixels` target and reads it back.
pub(crate) fn render_image<'pass, E: std::fmt::Display>(
    gpu: &TestGpu,
    size_pixels: [u32; 2],
    draw: impl FnOnce(&mut wgpu::RenderPass<'pass>) -> Result<(), E>,
) -> RgbaImage {
    let layout = GpuExportLayout::new(size_pixels[0], size_pixels[1], 1.0, u32::MAX).unwrap();
    let mut target = GpuExportTarget::new(&gpu.device, layout, FORMAT).unwrap();
    let tile = layout.tiles()[0];
    target
        .render_tile(&gpu.device, &gpu.queue, &tile, wgpu::Color::WHITE, draw)
        .unwrap();
    target.into_image()
}

/// What the oracle expects one shape to contribute at one pixel centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Shade {
    /// The fragment is not produced (outside the quad or discarded).
    Outside,
    /// Source colour blended over the target with standard alpha blending.
    Colour([f64; 4]),
    /// The decision is within tolerance of a boundary; the pixel is excluded from comparison.
    Uncertain,
}

/// Axis-aligned quad in pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Quad {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Quad {
    /// Classifies a pixel centre against the quad's edges: `Some(true)` inside, `Some(false)`
    /// outside, `None` within [`EDGE_BAND`] of an edge that separates inside from outside.
    pub(crate) fn classify(&self, x: f64, y: f64) -> Option<bool> {
        let (right, bottom) = (self.x + self.width, self.y + self.height);
        let outside =
            |low: f64, high: f64, value: f64| value < low - EDGE_BAND || value >= high + EDGE_BAND;
        if outside(self.x, right, x) || outside(self.y, bottom, y) {
            return Some(false);
        }
        let near = |edge: f64, value: f64| (value - edge).abs() < EDGE_BAND;
        if near(self.x, x) || near(right, x) || near(self.y, y) || near(bottom, y) {
            return None;
        }
        Some(true)
    }
}

/// CPU image built from projection oracles with the GPU's rules: a fragment exists where the
/// pixel centre is inside the quad and the scissor, and it is alpha-blended in draw order into
/// an 8-bit target.
pub(crate) struct Oracle {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 3]>,
    uncertain: Vec<bool>,
    painted: Vec<bool>,
}

impl Oracle {
    pub(crate) fn new(size_pixels: [u32; 2]) -> Self {
        let count = size_pixels[0] as usize * size_pixels[1] as usize;
        Self {
            width: size_pixels[0],
            height: size_pixels[1],
            pixels: vec![[255; 3]; count],
            uncertain: vec![false; count],
            painted: vec![false; count],
        }
    }

    /// Paints one instance. `bounds` limits the pixels visited, `scissor` is the physical-pixel
    /// `[x, y, width, height]` scissor of the draw, and `shade` receives pixel centres.
    pub(crate) fn paint(
        &mut self,
        bounds: Quad,
        scissor: [u32; 4],
        shade: impl Fn(f64, f64) -> Shade,
    ) {
        let clamp = |value: f64, limit: u32| value.clamp(0.0, f64::from(limit)) as u32;
        let left = clamp((bounds.x - 1.0).floor(), self.width).max(scissor[0]);
        let top = clamp((bounds.y - 1.0).floor(), self.height).max(scissor[1]);
        let right = clamp((bounds.x + bounds.width + 1.0).ceil(), self.width)
            .min(scissor[0].saturating_add(scissor[2]));
        let bottom = clamp((bounds.y + bounds.height + 1.0).ceil(), self.height)
            .min(scissor[1].saturating_add(scissor[3]));
        for y in top..bottom {
            for x in left..right {
                let index = (y * self.width + x) as usize;
                match shade(f64::from(x) + 0.5, f64::from(y) + 0.5) {
                    Shade::Outside => {}
                    Shade::Uncertain => self.uncertain[index] = true,
                    Shade::Colour(colour) => {
                        let alpha = colour[3].clamp(0.0, 1.0);
                        for (target, source) in self.pixels[index].iter_mut().zip(colour) {
                            let destination = f64::from(*target) / 255.0;
                            let blended =
                                source.clamp(0.0, 1.0) * alpha + destination * (1.0 - alpha);
                            *target = (blended * 255.0).round() as u8;
                        }
                        self.painted[index] = true;
                    }
                }
            }
        }
    }

    /// Whether the pixel lies in a boundary band and is therefore excluded from comparison.
    pub(crate) fn is_uncertain(&self, x: u32, y: u32) -> bool {
        self.uncertain[(y * self.width + x) as usize]
    }

    /// Compares every certain pixel with the GPU image.
    pub(crate) fn compare(&self, image: &RgbaImage) -> Comparison {
        assert_eq!(image.dimensions(), (self.width, self.height));
        let mut comparison = Comparison::default();
        for y in 0..self.height {
            for x in 0..self.width {
                let index = (y * self.width + x) as usize;
                if self.uncertain[index] {
                    comparison.uncertain += 1;
                    continue;
                }
                comparison.compared += 1;
                comparison.painted += usize::from(self.painted[index]);
                let actual = image.get_pixel(x, y).0;
                let expected = self.pixels[index];
                let differs = (0..3)
                    .any(|channel| actual[channel].abs_diff(expected[channel]) > CHANNEL_TOLERANCE)
                    || actual[3] != 255;
                if differs {
                    comparison.mismatches.push((x, y, expected, actual));
                }
            }
        }
        comparison
    }

    /// Asserts the GPU image equals the oracle within the documented tolerance, that the scene
    /// actually drew something, and that the excluded boundary band is a small minority.
    pub(crate) fn assert_matches(&self, image: &RgbaImage, label: &str) -> Comparison {
        let comparison = self.compare(image);
        assert!(
            comparison.mismatches.is_empty(),
            "{label}: {} of {} compared pixels differ from the CPU oracle; first (x, y, expected \
             rgb, actual rgba): {:?}",
            comparison.mismatches.len(),
            comparison.compared,
            &comparison.mismatches[..comparison.mismatches.len().min(12)]
        );
        assert!(
            comparison.painted > 0,
            "{label}: the oracle painted no comparable pixel"
        );
        assert!(
            comparison.uncertain * 4 <= comparison.painted + comparison.uncertain,
            "{label}: {} boundary pixels excluded against {} painted",
            comparison.uncertain,
            comparison.painted
        );
        comparison
    }
}

#[derive(Debug, Default)]
pub(crate) struct Comparison {
    pub compared: usize,
    pub painted: usize,
    pub uncertain: usize,
    pub mismatches: Vec<(u32, u32, [u8; 3], [u8; 4])>,
}

/// First and one-past-last column of `row` whose pixel differs from white, if any.
pub(crate) fn painted_columns(image: &RgbaImage, row: u32) -> Option<(u32, u32)> {
    let painted = |x: u32| image.get_pixel(x, row).0 != [255, 255, 255, 255];
    let first = (0..image.width()).find(|x| painted(*x))?;
    let last = (0..image.width()).rev().find(|x| painted(*x))?;
    Some((first, last + 1))
}

/// Column of the first pixel whose centre is at or right of `edge`: where a quad whose left
/// edge is `edge` starts, and where one whose right edge is `edge` stops.
pub(crate) fn first_column_at_or_after(edge: f64) -> i64 {
    (edge - 0.5).ceil() as i64
}

/// Whether `edge` is far enough from every pixel centre for its column to be unambiguous.
pub(crate) fn edge_is_decidable(edge: f64) -> bool {
    ((edge - 0.5) - (edge - 0.5).round()).abs() >= EDGE_BAND
}

/// Mirror of the shape rules in the shared rectangle fragment stage (`rectangle.wgsl` and
/// `context_scene.wgsl`), evaluated at a point local to an instance of `size` points.
/// Returns the alpha multiplier, `Some(0.0)`-free: `None` means the fragment is discarded.
pub(crate) fn rectangle_fragment_coverage(
    shape: u32,
    local: [f64; 2],
    size: [f64; 2],
) -> Option<f64> {
    if (7..=12).contains(&shape) {
        let base_stroke = (1.0 + size[1] * 0.075).clamp(1.25, 4.5);
        let expansion = match shape {
            9 | 10 => 1.5,
            11 | 12 => 2.0,
            _ => 0.0,
        };
        let stroke = base_stroke + expansion * 2.0;
        let inner_height = (size[1] - base_stroke).max(0.0);
        let fraction = (local[0] / size[0].max(0.0001)).clamp(0.0, 1.0);
        let parabola = 4.0 * (fraction - 0.5) * (fraction - 0.5);
        let curve_y = if shape.is_multiple_of(2) {
            size[1] - base_stroke * 0.5 - inner_height * parabola
        } else {
            base_stroke * 0.5 + inner_height * parabola
        };
        let slope = 8.0 * inner_height * (fraction - 0.5) / size[0].max(1.0);
        let distance = (local[1] - curve_y).abs() / (1.0 + slope * slope).sqrt();
        let coverage = 1.0
            - smoothstep(
                (stroke * 0.5 - 0.75).max(0.0),
                stroke * 0.5 + 0.75,
                distance,
            );
        return (coverage > 0.0).then_some(coverage);
    }
    if shape == 5 || shape == 6 {
        let bar_width = size[0].min(2.0);
        let stem_half_height = (size[1] * 0.14).max(size[1].min(1.0)).min(size[1] * 0.5);
        let in_bar = if shape == 6 {
            local[0] >= size[0] - bar_width
        } else {
            local[0] <= bar_width
        };
        let in_stem = (local[1] - size[1] * 0.5).abs() <= stem_half_height;
        return (in_bar || in_stem).then_some(1.0);
    }
    if shape != 0 {
        let head_width = if shape == 1 || shape == 2 {
            (size[1] * 0.55).min(size[0] * 0.35)
        } else {
            size[0]
        };
        let distance_from_tip = if shape == 2 || shape == 4 || shape == 6 {
            size[0] - local[0]
        } else {
            local[0]
        };
        if distance_from_tip < head_width {
            let half_height = size[1] * 0.5;
            let allowed = half_height * (distance_from_tip / head_width).clamp(0.0, 1.0);
            if (local[1] - half_height).abs() > allowed {
                return None;
            }
        }
    }
    Some(1.0)
}

pub(crate) fn smoothstep(edge0: f64, edge1: f64, value: f64) -> f64 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Shades a pixel centre with a coverage function that has hard internal boundaries: the pixel
/// is uncertain when the function's hard decision changes within [`EDGE_BAND`] of the centre.
pub(crate) fn shade_with_probe(
    x: f64,
    y: f64,
    colour: impl Fn(f64, f64) -> Option<[f64; 4]>,
) -> Shade {
    let centre = colour(x, y);
    for (dx, dy) in [
        (-EDGE_BAND, 0.0),
        (EDGE_BAND, 0.0),
        (0.0, -EDGE_BAND),
        (0.0, EDGE_BAND),
        (-EDGE_BAND, -EDGE_BAND),
        (EDGE_BAND, -EDGE_BAND),
        (-EDGE_BAND, EDGE_BAND),
        (EDGE_BAND, EDGE_BAND),
    ] {
        if colour(x + dx, y + dy).is_some() != centre.is_some() {
            return Shade::Uncertain;
        }
    }
    centre.map_or(Shade::Outside, Shade::Colour)
}
