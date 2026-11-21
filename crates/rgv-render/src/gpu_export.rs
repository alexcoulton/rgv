//! Offscreen GPU rendering with pixel readback.
//!
//! This is the export path that matches the screen exactly: the same pipelines, shaders and
//! instance buffers that draw the presented frame draw into an offscreen colour target at an
//! arbitrary pixel scale, and the result is read back into an [`RgbaImage`]. When the scaled
//! frame is larger than the device's maximum texture dimension the frame is rendered as a grid
//! of tiles which are stitched on the CPU.
//!
//! The renderers in this crate project logical points into normalised device coordinates with a
//! `screen_size` uniform and clip with physical-pixel scissors, both captured when a frame is
//! prepared. An export therefore prepares its own frames, once per [`GpuExportTile`]:
//!
//! * `screen_size_points` is [`GpuExportTile::screen_size_points`];
//! * every point-space origin (camera clip rectangles, context-scene offsets, display-list
//!   coordinates) is shifted by minus [`GpuExportTile::origin_points`];
//! * every scissor is [`GpuExportTile::scissor`] of the lane's rectangle in frame points.
//!
//! For an export that fits one texture there is a single tile whose origin is zero, so the frame
//! is prepared exactly as for the screen with the export scale standing in for the window scale
//! factor.

use std::path::Path;

use image::RgbaImage;
use rgv_layout::{DisplayList, RectPrimitive};
use thiserror::Error;

use crate::RectangleRenderer;

/// wgpu requires `bytes_per_row` of a texture-to-buffer copy to be a multiple of this.
const COPY_ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
const BYTES_PER_PIXEL: u32 = 4;

#[derive(Debug, Error)]
pub enum GpuExportError {
    #[error("GPU export scale must be a positive finite number")]
    InvalidScale,
    #[error("GPU export of {width}x{height} pixels exceeds the {maximum}-pixel edge limit")]
    TooLarge {
        width: u64,
        height: u64,
        maximum: u32,
    },
    #[error("GPU export tile edge must be at least one pixel")]
    InvalidTileEdge,
    #[error("GPU export cannot read back colour target format {0:?}")]
    UnsupportedFormat(wgpu::TextureFormat),
    #[error("GPU export tile does not belong to this target's layout")]
    ForeignTile,
    #[error("GPU export was refused by the device: {0}")]
    Device(String),
    #[error("GPU export readback could not be mapped: {0}")]
    Readback(String),
    #[error("GPU export draw callback failed: {0}")]
    Draw(String),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

/// One rectangular piece of an export frame, in physical pixels of the stitched image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuExportTile {
    pub index: usize,
    pub origin_pixels: [u32; 2],
    pub size_pixels: [u32; 2],
    /// Physical pixels per logical point.
    pub scale: f32,
}

impl GpuExportTile {
    /// The `screen_size_points` a frame captured for this tile must use, so that one logical
    /// point covers exactly `scale` pixels.
    #[must_use]
    pub fn screen_size_points(&self) -> [f32; 2] {
        [
            (f64::from(self.size_pixels[0]) / f64::from(self.scale)) as f32,
            (f64::from(self.size_pixels[1]) / f64::from(self.scale)) as f32,
        ]
    }

    /// The tile's top-left corner in logical points of the whole frame. Subtract it from every
    /// point-space origin of a frame captured for this tile. Zero for an untiled export.
    #[must_use]
    pub fn origin_points(&self) -> [f64; 2] {
        [
            f64::from(self.origin_pixels[0]) / f64::from(self.scale),
            f64::from(self.origin_pixels[1]) / f64::from(self.scale),
        ]
    }

    /// Converts a `[x, y, width, height]` rectangle in logical points of the whole frame into a
    /// physical-pixel scissor inside this tile. Returns `None` when the rectangle does not touch
    /// the tile, in which case the lane must not be drawn into it.
    ///
    /// Edges are rounded to the nearest pixel, the same rule the desktop applies when it turns a
    /// lane rectangle into a window scissor.
    #[must_use]
    pub fn scissor(&self, rect_points: [f32; 4]) -> Option<[u32; 4]> {
        let scale = f64::from(self.scale);
        let edge = |points: f32, origin: u32, size: u32| {
            let pixels = (f64::from(points) * scale).round() - f64::from(origin);
            pixels.clamp(0.0, f64::from(size)) as u32
        };
        let left = edge(rect_points[0], self.origin_pixels[0], self.size_pixels[0]);
        let right = edge(
            rect_points[0] + rect_points[2],
            self.origin_pixels[0],
            self.size_pixels[0],
        );
        let top = edge(rect_points[1], self.origin_pixels[1], self.size_pixels[1]);
        let bottom = edge(
            rect_points[1] + rect_points[3],
            self.origin_pixels[1],
            self.size_pixels[1],
        );
        (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
    }
}

/// Pixel geometry of one export: the stitched image size and how it is cut into tiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuExportLayout {
    size_pixels: [u32; 2],
    scale: f32,
    tile_edge: u32,
}

impl GpuExportLayout {
    /// Lays out a `width_points` x `height_points` frame at `scale` pixels per point, cut into
    /// tiles no larger than `maximum_tile_edge` pixels on either edge. The stitched image obeys
    /// the same [`crate::MAX_EXPORT_PNG_EDGE`] limit as the CPU exporter.
    pub fn new(
        width_points: u32,
        height_points: u32,
        scale: f32,
        maximum_tile_edge: u32,
    ) -> Result<Self, GpuExportError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(GpuExportError::InvalidScale);
        }
        if maximum_tile_edge == 0 {
            return Err(GpuExportError::InvalidTileEdge);
        }
        let edge = |points: u32| (f64::from(points) * f64::from(scale)).round().max(1.0);
        let (width, height) = (edge(width_points), edge(height_points));
        let maximum = crate::MAX_EXPORT_PNG_EDGE;
        if width > f64::from(maximum) || height > f64::from(maximum) {
            return Err(GpuExportError::TooLarge {
                width: width as u64,
                height: height as u64,
                maximum,
            });
        }
        Ok(Self {
            size_pixels: [width as u32, height as u32],
            scale,
            tile_edge: maximum_tile_edge,
        })
    }

    /// Lays out an export whose tiles are as large as `device` allows.
    pub fn for_device(
        device: &wgpu::Device,
        width_points: u32,
        height_points: u32,
        scale: f32,
    ) -> Result<Self, GpuExportError> {
        Self::new(
            width_points,
            height_points,
            scale,
            device.limits().max_texture_dimension_2d,
        )
    }

    #[must_use]
    pub const fn size_pixels(&self) -> [u32; 2] {
        self.size_pixels
    }

    #[must_use]
    pub const fn scale(&self) -> f32 {
        self.scale
    }

    #[must_use]
    pub const fn is_tiled(&self) -> bool {
        self.size_pixels[0] > self.tile_edge || self.size_pixels[1] > self.tile_edge
    }

    /// Largest tile this layout produces, which is the size of the offscreen texture.
    #[must_use]
    pub fn tile_extent(&self) -> [u32; 2] {
        [
            self.size_pixels[0].min(self.tile_edge),
            self.size_pixels[1].min(self.tile_edge),
        ]
    }

    #[must_use]
    pub fn tile_count(&self) -> usize {
        let [columns, rows] = self.tile_grid();
        columns as usize * rows as usize
    }

    fn tile_grid(&self) -> [u32; 2] {
        [
            self.size_pixels[0].div_ceil(self.tile_edge),
            self.size_pixels[1].div_ceil(self.tile_edge),
        ]
    }

    /// Tiles in row-major order. Together they cover every pixel of the image exactly once.
    #[must_use]
    pub fn tiles(&self) -> Vec<GpuExportTile> {
        let [columns, rows] = self.tile_grid();
        let mut tiles = Vec::with_capacity(self.tile_count());
        for row in 0..rows {
            for column in 0..columns {
                let origin = [column * self.tile_edge, row * self.tile_edge];
                tiles.push(GpuExportTile {
                    index: tiles.len(),
                    origin_pixels: origin,
                    size_pixels: [
                        (self.size_pixels[0] - origin[0]).min(self.tile_edge),
                        (self.size_pixels[1] - origin[1]).min(self.tile_edge),
                    ],
                    scale: self.scale,
                });
            }
        }
        tiles
    }
}

/// Offscreen colour target plus the stitched CPU image its tiles are read back into.
pub struct GpuExportTarget {
    layout: GpuExportLayout,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
    image: RgbaImage,
    rendered_tiles: usize,
}

impl GpuExportTarget {
    /// Allocates the offscreen texture and readback buffer for `layout`. `format` must be the
    /// colour target format the participating renderers were created with (normally the window
    /// surface format), so the exported bytes are the bytes the surface would have received.
    ///
    /// # Errors
    ///
    /// Fails for a format that is not an 8-bit RGBA/BGRA format, and when the device refuses the
    /// allocation (validation or out of memory) instead of raising an uncaptured device error.
    pub fn new(
        device: &wgpu::Device,
        layout: GpuExportLayout,
        format: wgpu::TextureFormat,
    ) -> Result<Self, GpuExportError> {
        if !matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm
                | wgpu::TextureFormat::Rgba8UnormSrgb
                | wgpu::TextureFormat::Bgra8Unorm
                | wgpu::TextureFormat::Bgra8UnormSrgb
        ) {
            return Err(GpuExportError::UnsupportedFormat(format));
        }
        let [width, height] = layout.tile_extent();
        let padded_bytes_per_row = padded_bytes_per_row(width);
        let (texture, readback) = with_error_scopes(device, || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("RGV GPU export target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV GPU export readback"),
                size: u64::from(padded_bytes_per_row) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            (texture, readback)
        })?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let [image_width, image_height] = layout.size_pixels();
        Ok(Self {
            layout,
            format,
            texture,
            view,
            readback,
            padded_bytes_per_row,
            image: RgbaImage::new(image_width, image_height),
            rendered_tiles: 0,
        })
    }

    #[must_use]
    pub const fn layout(&self) -> &GpuExportLayout {
        &self.layout
    }

    #[must_use]
    pub const fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Number of tiles rendered so far; the image is complete at [`GpuExportLayout::tile_count`].
    #[must_use]
    pub const fn rendered_tiles(&self) -> usize {
        self.rendered_tiles
    }

    /// Clears the offscreen target to `clear`, lets `draw` record into one render pass whose
    /// attachment is exactly `tile.size_pixels` (the viewport and default scissor are set to the
    /// tile), submits, and copies the tile's pixels into the stitched image.
    ///
    /// Frames replayed by `draw` must have been captured for this tile; see the module
    /// documentation. `draw` borrows the renderers for `'pass`, which is why the pass lifetime is
    /// a parameter rather than a higher-ranked bound.
    ///
    /// # Errors
    ///
    /// Returns the `draw` error, any validation or out-of-memory error the device raised while
    /// the tile was encoded, and a failed readback mapping. The stitched image is unchanged on
    /// error.
    pub fn render_tile<'pass, E: std::fmt::Display>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tile: &GpuExportTile,
        clear: wgpu::Color,
        draw: impl FnOnce(&mut wgpu::RenderPass<'pass>) -> Result<(), E>,
    ) -> Result<(), GpuExportError> {
        let [width, height] = tile.size_pixels;
        let extent = self.layout.tile_extent();
        let image_size = self.layout.size_pixels();
        if width == 0
            || height == 0
            || width > extent[0]
            || height > extent[1]
            || tile.origin_pixels[0].saturating_add(width) > image_size[0]
            || tile.origin_pixels[1].saturating_add(height) > image_size[1]
        {
            return Err(GpuExportError::ForeignTile);
        }
        let submitted = with_error_scopes(device, || {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("RGV GPU export encoder"),
            });
            let drawn = {
                // The pass is detached from the encoder borrow so that `'pass` can be chosen by
                // the caller; it is dropped (ended) at the end of this block, before the encoder
                // is used again.
                let mut pass: wgpu::RenderPass<'pass> = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("RGV GPU export pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                pass.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, width, height);
                draw(&mut pass)
            };
            if let Err(error) = drawn {
                return Err(GpuExportError::Draw(error.to_string()));
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(self.padded_bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            Ok(())
        })?;
        submitted?;

        self.read_back_tile(device, tile)?;
        self.rendered_tiles += 1;
        Ok(())
    }

    /// Maps the readback buffer and copies the tile's rows into the stitched image.
    fn read_back_tile(
        &mut self,
        device: &wgpu::Device,
        tile: &GpuExportTile,
    ) -> Result<(), GpuExportError> {
        let [width, height] = tile.size_pixels;
        let image_width = self.layout.size_pixels()[0] as usize;
        let mapped_bytes = u64::from(self.padded_bytes_per_row) * u64::from(height);
        let slice = self.readback.slice(..mapped_bytes);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| GpuExportError::Readback(error.to_string()))?;
        // `get_mapped_range` on a buffer whose mapping failed aborts the process, so the
        // mapping result is checked first.
        match receiver.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(GpuExportError::Readback(error.to_string())),
            Err(_) => {
                return Err(GpuExportError::Readback(
                    "the device did not complete the mapping".to_owned(),
                ));
            }
        }
        {
            let data = slice.get_mapped_range();
            let swap_red_blue = matches!(
                self.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            let pixels: &mut [u8] = &mut self.image;
            for row in 0..height as usize {
                let source_start = row * self.padded_bytes_per_row as usize;
                let source = &data[source_start..source_start + (width * BYTES_PER_PIXEL) as usize];
                let destination_start = ((tile.origin_pixels[1] as usize + row) * image_width
                    + tile.origin_pixels[0] as usize)
                    * BYTES_PER_PIXEL as usize;
                let destination = &mut pixels[destination_start..destination_start + source.len()];
                destination.copy_from_slice(source);
                if swap_red_blue {
                    for pixel in destination.chunks_exact_mut(BYTES_PER_PIXEL as usize) {
                        pixel.swap(0, 2);
                    }
                }
            }
        }
        self.readback.unmap();
        Ok(())
    }

    /// The stitched image. Pixels of tiles that were never rendered are transparent black.
    #[must_use]
    pub fn into_image(self) -> RgbaImage {
        self.image
    }

    /// Encodes the stitched image as a PNG at `path`.
    pub fn save_png(self, path: impl AsRef<Path>) -> Result<(), GpuExportError> {
        self.image
            .save_with_format(path, image::ImageFormat::Png)
            .map_err(GpuExportError::from)
    }
}

/// Renders a whole export in one call: for every tile, `prepare` builds what the tile needs
/// (typically the renderers' frames, captured with the tile's transform) and `draw` replays it
/// into the tile's pass.
///
/// `context` is whatever owns the renderers. `prepare` receives it mutably, because capturing a
/// frame admits pages and writes uniforms; `draw` receives it shared for the duration of the
/// pass, together with the value `prepare` returned.
///
/// # Errors
///
/// See [`GpuExportTarget::new`] and [`GpuExportTarget::render_tile`]; a `prepare` error is
/// reported as [`GpuExportError::Draw`].
#[allow(clippy::too_many_arguments)]
pub fn render_gpu_export<C, P, E: std::fmt::Display>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: GpuExportLayout,
    format: wgpu::TextureFormat,
    clear: wgpu::Color,
    context: &mut C,
    mut prepare: impl FnMut(&mut C, &GpuExportTile) -> Result<P, E>,
    mut draw: impl for<'pass> FnMut(&'pass C, &'pass P, &mut wgpu::RenderPass<'pass>) -> Result<(), E>,
) -> Result<RgbaImage, GpuExportError> {
    let mut target = GpuExportTarget::new(device, layout, format)?;
    for tile in layout.tiles() {
        let prepared =
            prepare(context, &tile).map_err(|error| GpuExportError::Draw(error.to_string()))?;
        let shared: &C = context;
        target.render_tile(device, queue, &tile, clear, |pass| {
            draw(shared, &prepared, pass)
        })?;
    }
    Ok(target.into_image())
}

/// Colour target of [`render_display_list_gpu`]: linear 8-bit, so a display-list colour is written
/// as `round(colour * 255)`, the same bytes the CPU exporter produces.
const DISPLAY_LIST_EXPORT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Renders `display_list`, laid out in a `width` x `height` logical-point frame, through the
/// rectangle pipeline at `scale` pixels per point and returns the pixels. Shapes (arrow heads,
/// brackets, anti-aliased junction arcs) are produced by the same shader as on screen; text is
/// rasterised on the CPU over the result exactly as [`crate::export_png_scaled`] does, because
/// the window draws its text outside this crate.
///
/// Exports larger than the device's maximum texture dimension are rendered as tiles.
///
/// # Errors
///
/// See [`GpuExportLayout::new`], [`GpuExportTarget::new`] and [`GpuExportTarget::render_tile`].
pub fn render_display_list_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    display_list: &DisplayList,
    width: u32,
    height: u32,
    scale: f32,
) -> Result<RgbaImage, GpuExportError> {
    let layout = GpuExportLayout::for_device(device, width, height, scale)?;
    render_display_list_with_layout(device, queue, display_list, layout)
}

pub(crate) fn render_display_list_with_layout(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    display_list: &DisplayList,
    layout: GpuExportLayout,
) -> Result<RgbaImage, GpuExportError> {
    let mut target = GpuExportTarget::new(device, layout, DISPLAY_LIST_EXPORT_FORMAT)?;
    // One renderer draws at most `maximum_rectangles_per_frame` instances; larger lists are
    // split across renderers that draw into the same pass, in display-list order.
    let chunk_size = RectangleRenderer::maximum_rectangles_per_frame().max(1);
    let mut renderers = Vec::new();
    let mut tile_list = DisplayList::default();
    for tile in layout.tiles() {
        let origin = tile.origin_points();
        let size = tile.screen_size_points();
        let (origin_x, origin_y) = (origin[0] as f32, origin[1] as f32);
        tile_list.rectangles.clear();
        tile_list.rectangles.extend(
            display_list
                .rectangles
                .iter()
                .filter(|rectangle| {
                    !layout.is_tiled()
                        || (rectangle.x < origin_x + size[0]
                            && rectangle.x + rectangle.width > origin_x
                            && rectangle.y < origin_y + size[1]
                            && rectangle.y + rectangle.height > origin_y)
                })
                .map(|rectangle| RectPrimitive {
                    x: rectangle.x - origin_x,
                    y: rectangle.y - origin_y,
                    ..*rectangle
                }),
        );
        let chunks = tile_list.rectangles.chunks(chunk_size).collect::<Vec<_>>();
        while renderers.len() < chunks.len() {
            renderers.push(RectangleRenderer::new(device, DISPLAY_LIST_EXPORT_FORMAT));
        }
        for (renderer, chunk) in renderers.iter_mut().zip(&chunks) {
            let chunk_list = DisplayList {
                rectangles: chunk.to_vec(),
                texts: Vec::new(),
            };
            renderer.prepare(device, queue, size, &chunk_list, None, None);
        }
        let active = &renderers[..chunks.len()];
        target.render_tile(device, queue, &tile, wgpu::Color::WHITE, |pass| {
            for renderer in active {
                renderer.render_unhighlighted(pass);
            }
            Ok::<(), std::convert::Infallible>(())
        })?;
    }
    let mut image = target.into_image();
    for text in &display_list.texts {
        crate::rasterize_text(&mut image, text, layout.scale());
    }
    Ok(image)
}

/// Which rasteriser produced an export written by [`export_png_scaled_prefer_gpu`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PngExportBackend {
    /// Rendered by the GPU rectangle pipeline; pixels match the screen.
    Gpu,
    /// Rendered by the CPU rasteriser, either because no device was supplied or because the GPU
    /// export failed for the stated reason.
    Cpu { gpu_failure: Option<String> },
}

/// Exports `display_list` as a PNG through the GPU when a device is available and through the CPU
/// rasteriser ([`crate::export_png_scaled`]) otherwise. A GPU failure (device lost, allocation
/// refused, unsupported size) falls back to the CPU path rather than failing the export, and is
/// reported in the returned backend.
///
/// # Errors
///
/// Only the errors of the CPU path: an unusable scale or size, or a failure to encode or write
/// the file.
pub fn export_png_scaled_prefer_gpu(
    gpu: Option<(&wgpu::Device, &wgpu::Queue)>,
    display_list: &DisplayList,
    width: u32,
    height: u32,
    scale: f32,
    path: impl AsRef<Path>,
) -> Result<PngExportBackend, image::ImageError> {
    let gpu_failure = match gpu {
        Some((device, queue)) => {
            match render_display_list_gpu(device, queue, display_list, width, height, scale) {
                Ok(image) => {
                    image.save_with_format(path, image::ImageFormat::Png)?;
                    return Ok(PngExportBackend::Gpu);
                }
                Err(error) => Some(error.to_string()),
            }
        }
        None => None,
    };
    crate::export_png_scaled(display_list, width, height, scale, path)?;
    Ok(PngExportBackend::Cpu { gpu_failure })
}

fn padded_bytes_per_row(width: u32) -> u32 {
    (width * BYTES_PER_PIXEL).div_ceil(COPY_ROW_ALIGNMENT) * COPY_ROW_ALIGNMENT
}

/// Runs `operation` with validation and out-of-memory error scopes pushed, so that a refused
/// allocation or an invalid command becomes an `Err` here instead of reaching the device's
/// uncaptured-error handler (which by default panics).
fn with_error_scopes<T>(
    device: &wgpu::Device,
    operation: impl FnOnce() -> T,
) -> Result<T, GpuExportError> {
    let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = operation();
    let validation = resolve_now(validation.pop());
    let out_of_memory = resolve_now(out_of_memory.pop());
    match validation.or(out_of_memory) {
        Some(error) => Err(GpuExportError::Device(error.to_string())),
        None => Ok(value),
    }
}

/// Resolves a native wgpu future. On native backends error-scope futures are ready as soon as
/// they are created; the loop only guards against a backend that needs another poll.
pub(crate) fn resolve_now<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests;
