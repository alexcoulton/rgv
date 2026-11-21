//! Coverage-pyramid depth lane: instance layout, CPU oracle and the GPU draw path.
//!
//! A [`DepthPyramidLane`] is a set of disjoint segments, each a run of bins of one resident
//! pyramid tile. A tile is uploaded once as a world-coordinate page of twelve-byte instances
//! (minimum, mean, maximum per bin; the bin's position is its index) and drawn for any camera
//! by changing a uniform. Each bin is three stacked bands (see `alignment_depth_pyramid.wgsl`):
//! solid up to the minimum, medium up to the mean, light up to the maximum, all in the
//! palette's summary-depth colour.
//!
//! The lane's level is chosen so a bin is at least one pixel wide; a pixel column then shows
//! exactly one bin and nothing a bin records can fall between pixel centres.
//!
//! # GPU memory
//!
//! GPU pages of this lane are small (12 KiB for a full tile) and are kept in a
//! least-recently-used cache with its own byte cap
//! ([`ALIGNMENT_DEPTH_PYRAMID_DEFAULT_GPU_BYTES`]). Once the renderer is given the workspace's
//! allocation ledger ([`AlignmentDepthPyramidGpuRenderer::set_ledger`]) every page buffer and
//! the page table are also charged to it as [`AllocationClass::GpuPage`], by exactly the bytes
//! they hold, through the [`PagedGpuCache`] the summary and track-tile renderers use; the GPU
//! owner's accounting then includes this lane. A page that does not fit the GPU budget even
//! after this renderer's idle pages are evicted is not uploaded: its segment is left out of
//! the frame and counted in [`AlignmentDepthPyramidGpuPrepareStats::pages_refused`] and, per
//! lane, in [`AlignmentDepthPyramidGpuFrame::pages_refused_by_request`]. Without
//! a ledger the pages are bounded by the byte cap alone, as before.

use std::sync::atomic::AtomicU64;

use rgv_alignment_scene::{
    AllocationClass, AllocationLedger, DepthPyramidLane, DepthPyramidSegment, ReservationKind,
};
use rgv_model::CoverageTile;

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

/// Opacity of the band between a bin's minimum and its mean, relative to the depth colour.
pub const ALIGNMENT_DEPTH_PYRAMID_MEAN_BAND_ALPHA: f32 = 0.72;
/// Opacity of the band between a bin's mean and its maximum.
pub const ALIGNMENT_DEPTH_PYRAMID_ENVELOPE_BAND_ALPHA: f32 = 0.36;
/// Default cap of resident GPU pages of one renderer: 4 MiB, about 340 full tiles.
pub const ALIGNMENT_DEPTH_PYRAMID_DEFAULT_GPU_BYTES: usize = 4 << 20;

/// Widest span one draw addresses with page-local offsets; wider tiles are drawn in chunks.
const MAXIMUM_DRAW_BASES: u64 = 1 << 29;
const VERTICES_PER_BIN: u32 = 18;

/// One pyramid bin: twelve bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentDepthPyramidGpuInstance {
    pub minimum: u32,
    pub mean: f32,
    pub maximum: u32,
}

impl AlignmentDepthPyramidGpuInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
            wgpu::vertex_attr_array![0 => Uint32, 1 => Float32, 2 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

/// Packs every bin of `tile` in order; bin `i` is instance `i`.
#[must_use]
pub fn pack_alignment_depth_pyramid_tile(
    tile: &CoverageTile,
) -> Vec<AlignmentDepthPyramidGpuInstance> {
    tile.bins()
        .iter()
        .enumerate()
        .map(|(index, bin)| {
            let bases = tile.bin_span(index).map_or(0, |(start, end)| end - start);
            AlignmentDepthPyramidGpuInstance {
                minimum: bin.minimum_depth,
                mean: bin.mean_depth(bases) as f32,
                maximum: bin.maximum_depth,
            }
        })
        .collect()
}

/// Which of a bin's three bands a projected rectangle is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentDepthPyramidBand {
    /// Baseline to the bin's minimum depth.
    Minimum,
    /// Minimum to mean.
    Mean,
    /// Mean to maximum.
    Envelope,
}

impl AlignmentDepthPyramidBand {
    /// Opacity multiplier applied to the palette's summary-depth colour.
    #[must_use]
    pub const fn alpha(self) -> f32 {
        match self {
            Self::Minimum => 1.0,
            Self::Mean => ALIGNMENT_DEPTH_PYRAMID_MEAN_BAND_ALPHA,
            Self::Envelope => ALIGNMENT_DEPTH_PYRAMID_ENVELOPE_BAND_ALPHA,
        }
    }
}

/// One band of one bin as the GPU draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentDepthPyramidProjectedBand {
    pub band: AlignmentDepthPyramidBand,
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    /// Half-open base span of the bin.
    pub start: u64,
    pub end: u64,
}

/// CPU oracle of `depth_pyramid_vertex`: the bands of the bin `instance` spanning
/// `[start, end)`, in draw order. Bands of zero height and bins outside the lane are omitted.
#[must_use]
pub fn project_alignment_depth_pyramid_bin(
    instance: AlignmentDepthPyramidGpuInstance,
    start: u64,
    end: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> Vec<AlignmentDepthPyramidProjectedBand> {
    let clip = camera.clip();
    let project = |position: u64| {
        clip.x + (position as f64 - camera.exact_start()) / camera.bases_per_pixel()
    };
    let left = project(start).max(clip.x);
    let right = project(end).min(clip.x + clip.width);
    if right <= left {
        return Vec::new();
    }
    let height = |depth: f64| {
        let fraction = (depth / f64::from(scale_maximum.max(1))).min(1.0);
        clip.height
            * match transform {
                CoverageScaleTransform::Linear => fraction,
                CoverageScaleTransform::SquareRoot => fraction.sqrt(),
            }
    };
    let mut top_maximum = height(f64::from(instance.maximum));
    if instance.maximum > 0 {
        top_maximum = top_maximum.max(1.0);
    }
    let top_minimum = height(f64::from(instance.minimum)).min(top_maximum);
    let top_mean = height(f64::from(instance.mean)).clamp(top_minimum, top_maximum);
    let baseline = clip.y + clip.height;
    [
        (AlignmentDepthPyramidBand::Minimum, 0.0, top_minimum),
        (AlignmentDepthPyramidBand::Mean, top_minimum, top_mean),
        (AlignmentDepthPyramidBand::Envelope, top_mean, top_maximum),
    ]
    .into_iter()
    .filter(|(_, low, high)| high > low)
    .map(|(band, low, high)| AlignmentDepthPyramidProjectedBand {
        band,
        bounds: AlignmentSummaryGpuProjectedBounds {
            x: left,
            y: baseline - high,
            width: right - left,
            height: high - low,
        },
        start,
        end,
    })
    .collect()
}

/// One run of consecutive bins of one tile that a camera can see.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VisibleRun {
    /// Index of the run's first bin in its tile.
    first_bin: usize,
    bins: usize,
    /// Base position of the run's first bin.
    start: u64,
    /// Base position just past the run's last bin (clipped to the contig).
    end: u64,
    bin_bases: u64,
}

/// The bins of `segment` that intersect the camera, cut into runs no wider than one draw may
/// address. Shared by the oracle and the GPU path, so both visit exactly the same bins.
fn visible_runs(segment: &DepthPyramidSegment, camera: CpuSummaryCamera) -> Vec<VisibleRun> {
    let tile = segment.tile.tile();
    let (tile_start, tile_end) = tile.span();
    let bin_bases = tile.bin_bases();
    let camera_start = camera.exact_start().floor().max(0.0) as u64;
    let camera_end = (camera.exact_start() + camera.span()).ceil().max(0.0) as u64;
    let start = segment.start.max(tile_start).max(camera_start);
    let end = segment.end.min(tile_end).min(camera_end);
    if start >= end {
        return Vec::new();
    }
    let first = ((start - tile_start) / bin_bases) as usize;
    let last = ((end - tile_start).div_ceil(bin_bases) as usize).min(tile.bins().len());
    let step = ((MAXIMUM_DRAW_BASES / bin_bases).max(1)) as usize;
    (first..last)
        .step_by(step)
        .map(|run_first| {
            let bins = step.min(last - run_first);
            let run_start = tile_start + run_first as u64 * bin_bases;
            VisibleRun {
                first_bin: run_first,
                bins,
                start: run_start,
                end: (run_start + bins as u64 * bin_bases).min(tile_end),
                bin_bases,
            }
        })
        .collect()
}

/// CPU oracle of a whole lane: every band the GPU draws for `lane` under `camera`, in draw
/// order. Bands of different bins never overlap.
#[must_use]
pub fn project_alignment_depth_pyramid_lane(
    lane: &DepthPyramidLane,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> Vec<AlignmentDepthPyramidProjectedBand> {
    let mut bands = Vec::new();
    for segment in lane.segments() {
        let instances = pack_alignment_depth_pyramid_tile(segment.tile.tile());
        for run in visible_runs(segment, camera) {
            for offset in 0..run.bins {
                let start = run.start + offset as u64 * run.bin_bases;
                let end = (start + run.bin_bases).min(run.end);
                bands.extend(project_alignment_depth_pyramid_bin(
                    instances[run.first_bin + offset],
                    start,
                    end,
                    camera,
                    scale_maximum,
                    transform,
                ));
            }
        }
    }
    bands
}

/// One coverage-pyramid lane to draw.
#[derive(Clone, Copy, Debug)]
pub struct AlignmentDepthPyramidGpuRequest<'a> {
    pub lane: &'a DepthPyramidLane,
    pub camera: CpuSummaryCamera,
    pub screen_size_points: [f32; 2],
    pub scissor: AlignmentGpuScissor,
    /// Depth drawn at full lane height; usually the lane's maximum over the visible span.
    pub scale_maximum: u32,
    pub transform: CoverageScaleTransform,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct DepthPyramidGpuUniform {
    screen_size: [f32; 2],
    clip_origin: [f32; 2],
    clip_size: [f32; 2],
    points_per_base: [f32; 2],
    page_start_whole: i32,
    page_start_fraction: f32,
    covered_clip_x: [f32; 2],
    scale_maximum: f32,
    transform: u32,
    bin_bases: u32,
    page_bases: u32,
}

#[derive(Clone, Debug)]
struct PreparedPyramidDraw {
    page: AllocationId,
    first_instance: u32,
    instances: u32,
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
}

/// Draw list captured by [`AlignmentDepthPyramidGpuRenderer::prepare`].
#[derive(Clone, Debug)]
pub struct AlignmentDepthPyramidGpuFrame {
    draws: Vec<PreparedPyramidDraw>,
    revision: u64,
    device_generation: u64,
    /// Pages refused per request, in request order.
    refused_by_request: Vec<usize>,
}

impl AlignmentDepthPyramidGpuFrame {
    /// Pages the ledger refused for each request of the `prepare` call that built this frame,
    /// indexed like its `requests` slice. A refused page's segment is missing from that
    /// request's lane only; the sum over requests is
    /// [`AlignmentDepthPyramidGpuPrepareStats::pages_refused`].
    ///
    /// A page is refused where it is first needed: a later request of the same frame that
    /// needs the same page is refused it too, and is counted too.
    #[must_use]
    pub fn pages_refused_by_request(&self) -> &[usize] {
        &self.refused_by_request
    }

    /// Pages refused for request `index` of the `prepare` call; zero for an index it did not
    /// have. A lane whose request has zero refusals is drawn whole whatever another lane of
    /// the same frame was refused.
    #[must_use]
    pub fn request_pages_refused(&self, index: usize) -> usize {
        self.refused_by_request.get(index).copied().unwrap_or(0)
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }

    /// Bins drawn by this frame.
    #[must_use]
    pub fn bin_count(&self) -> usize {
        self.draws.iter().map(|draw| draw.instances as usize).sum()
    }
}

/// What one `prepare` did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentDepthPyramidGpuPrepareStats {
    pub pages_uploaded: usize,
    pub uploaded_bytes: usize,
    pub pages_reused: usize,
    pub pages_evicted: usize,
    pub draws: usize,
    /// Pages the shared ledger had no room for; their segments are not drawn this frame.
    /// Always zero without a ledger.
    pub pages_refused: usize,
}

#[derive(Debug)]
struct GpuPyramidPage {
    key: AllocationId,
    buffer: wgpu::Buffer,
    bytes: usize,
    last_used: u64,
    /// Ledger charge of `buffer`; `None` when the renderer has no ledger.
    allocation: Option<AllocationLease>,
}

impl CachedGpuPage for GpuPyramidPage {
    type Key = AllocationId;

    fn key(&self) -> AllocationId {
        self.key
    }

    fn last_used_revision(&self) -> u64 {
        self.last_used
    }

    fn set_last_used_revision(&mut self, revision: u64) {
        self.last_used = revision;
    }

    fn allocation(&self) -> Option<&AllocationLease> {
        self.allocation.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentDepthPyramidGpuError {
    #[error(
        "depth pyramid frame belongs to device generation {frame}; renderer is at generation {renderer}"
    )]
    DeviceGenerationMismatch { frame: u64, renderer: u64 },
    #[error("a depth pyramid page disappeared before render: {0:?}")]
    MissingPage(AllocationId),
    #[error("depth pyramid frame was captured before the renderer's latest prepare")]
    StaleFrame,
    #[error("depth pyramid camera is not a positive finite scale")]
    InvalidCamera,
}

/// Draws coverage-pyramid lanes. One per window, beside the summary renderer.
#[derive(Debug)]
pub struct AlignmentDepthPyramidGpuRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_stride: usize,
    uniform_buffer: Option<wgpu::Buffer>,
    uniform_bind_group: Option<wgpu::BindGroup>,
    uniform_capacity: usize,
    palette: crate::palette::PaletteBinding,
    cache: PagedGpuCache<GpuPyramidPage>,
    ledger: Option<AllocationLedger>,
    keys_inspected: AtomicU64,
    page_bytes: usize,
    page_byte_limit: usize,
    revision: u64,
    device_generation: u64,
}

impl AlignmentDepthPyramidGpuRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self::with_palette(
            device,
            target_format,
            &crate::palette::AlignmentPalette::default(),
        )
    }

    #[must_use]
    pub fn with_palette(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        palette: &crate::palette::AlignmentPalette,
    ) -> Self {
        let shader = crate::shaders::ALIGNMENT_DEPTH_PYRAMID.create_module(device);
        let palette_layout = crate::palette::PaletteBinding::layout(device);
        let palette = crate::palette::PaletteBinding::new(device, &palette_layout, palette);
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV alignment depth pyramid uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(size_of::<DepthPyramidGpuUniform>() as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV alignment depth pyramid pipeline layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&palette_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV alignment depth pyramid pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("depth_pyramid_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[AlignmentDepthPyramidGpuInstance::vertex_buffer_layout()],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let alignment = device.limits().min_uniform_buffer_offset_alignment.max(1) as usize;
        Self {
            pipeline,
            uniform_layout,
            uniform_stride: size_of::<DepthPyramidGpuUniform>()
                .div_ceil(alignment)
                .saturating_mul(alignment),
            uniform_buffer: None,
            uniform_bind_group: None,
            uniform_capacity: 0,
            palette,
            cache: PagedGpuCache::default(),
            ledger: None,
            keys_inspected: AtomicU64::new(0),
            page_bytes: 0,
            page_byte_limit: ALIGNMENT_DEPTH_PYRAMID_DEFAULT_GPU_BYTES,
            revision: 0,
            device_generation: 1,
        }
    }

    /// The palette the next frame is drawn with.
    #[must_use]
    pub const fn palette(&self) -> &crate::palette::AlignmentPalette {
        self.palette.palette()
    }

    /// Switches the palette; no page is rebuilt. Returns whether it was uploaded.
    pub fn set_palette(
        &mut self,
        queue: &wgpu::Queue,
        palette: &crate::palette::AlignmentPalette,
    ) -> bool {
        self.palette.set(queue, palette)
    }

    /// Changes the cap of resident GPU pages. Pages of the latest frame are never evicted, so
    /// the cap is exceeded only while one frame alone needs more.
    pub const fn set_page_byte_limit(&mut self, bytes: usize) {
        self.page_byte_limit = bytes;
    }

    /// Charges this renderer's GPU pages and page table to `ledger` from now on: pass the
    /// workspace's shared ledger, so the pages are accounted to the GPU owner with every
    /// other page. Pages already resident were uploaded uncharged and are dropped (they are
    /// uploaded again, charged, when next drawn); frames captured before this call no longer
    /// render.
    pub fn set_ledger(&mut self, ledger: AllocationLedger) {
        self.discard_device_resources();
        self.ledger = Some(ledger);
    }

    /// Bytes this renderer currently has charged to the ledger: every page buffer and the
    /// page table. Zero without a ledger.
    pub fn ledger_charged_bytes(&self) -> Result<usize, LedgerError> {
        self.cache.charged_bytes()
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.cache.pages.len()
    }

    /// Bytes of instance buffers currently resident.
    #[must_use]
    pub const fn resident_page_bytes(&self) -> usize {
        self.page_bytes
    }

    /// Drops every device resource that can be rebuilt (after device loss, or under memory
    /// pressure). Frames captured before this call no longer render.
    pub fn discard_device_resources(&mut self) {
        self.cache.clear();
        self.page_bytes = 0;
        self.uniform_buffer = None;
        self.uniform_bind_group = None;
        self.uniform_capacity = 0;
        self.device_generation = self.device_generation.saturating_add(1);
    }

    /// Uploads the pages the lanes need (once per tile) and captures their draws.
    #[allow(clippy::too_many_lines)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentDepthPyramidGpuRequest<'_>],
    ) -> Result<
        (
            AlignmentDepthPyramidGpuFrame,
            AlignmentDepthPyramidGpuPrepareStats,
        ),
        AlignmentDepthPyramidGpuError,
    > {
        self.revision = self.revision.saturating_add(1);
        let revision = self.revision;
        let mut stats = AlignmentDepthPyramidGpuPrepareStats::default();
        let mut draws = Vec::new();
        let mut uniforms = Vec::<DepthPyramidGpuUniform>::new();
        let mut refused_by_request = vec![0_usize; requests.len()];
        for (request_index, request) in requests.iter().enumerate() {
            let camera = request.camera;
            let bases_per_pixel = camera.bases_per_pixel();
            if !bases_per_pixel.is_finite() || bases_per_pixel <= 0.0 {
                return Err(AlignmentDepthPyramidGpuError::InvalidCamera);
            }
            let clip = camera.clip();
            for segment in request.lane.segments() {
                let runs = visible_runs(segment, camera);
                if runs.is_empty() {
                    continue;
                }
                let page = segment.tile.allocation().id();
                if let Some(index) = self.cache.page_index(page) {
                    if self.cache.pages[index].last_used != revision {
                        stats.pages_reused += 1;
                        self.cache.touch_page_at(index, revision);
                    }
                } else {
                    let instances = pack_alignment_depth_pyramid_tile(segment.tile.tile());
                    let bytes = size_of_val(instances.as_slice());
                    let Ok(allocation) = self.charge_page(bytes, revision, &mut stats) else {
                        stats.pages_refused += 1;
                        refused_by_request[request_index] += 1;
                        continue;
                    };
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("RGV alignment depth pyramid page"),
                        size: bytes.max(size_of::<AlignmentDepthPyramidGpuInstance>()) as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&instances));
                    let index = self
                        .cache
                        .pages
                        .binary_search_by_key(&page, CachedGpuPage::key)
                        .unwrap_or_else(|index| index);
                    self.cache.pages.insert(
                        index,
                        GpuPyramidPage {
                            key: page,
                            buffer,
                            bytes,
                            last_used: revision,
                            allocation,
                        },
                    );
                    self.cache.reclaim_index.insert((revision, page));
                    self.page_bytes = self.page_bytes.saturating_add(bytes);
                    stats.pages_uploaded += 1;
                    stats.uploaded_bytes += bytes;
                }
                for run in runs {
                    let (page_start_whole, page_start_fraction) =
                        crate::shaders::split_camera_origin(run.start, camera.exact_start());
                    let uniform_offset = uniforms.len().saturating_mul(self.uniform_stride) as u32;
                    uniforms.push(DepthPyramidGpuUniform {
                        screen_size: [
                            request.screen_size_points[0].max(1.0),
                            request.screen_size_points[1].max(1.0),
                        ],
                        clip_origin: [clip.x as f32, clip.y as f32],
                        clip_size: [clip.width as f32, clip.height as f32],
                        points_per_base: split_f64(1.0 / bases_per_pixel),
                        page_start_whole,
                        page_start_fraction,
                        covered_clip_x: [clip.x as f32, (clip.x + clip.width) as f32],
                        scale_maximum: request.scale_maximum.max(1) as f32,
                        transform: match request.transform {
                            CoverageScaleTransform::Linear => 0,
                            CoverageScaleTransform::SquareRoot => 1,
                        },
                        bin_bases: run.bin_bases as u32,
                        page_bases: (run.end - run.start) as u32,
                    });
                    draws.push(PreparedPyramidDraw {
                        page,
                        first_instance: run.first_bin as u32,
                        instances: run.bins as u32,
                        uniform_offset,
                        scissor: request.scissor,
                    });
                }
            }
        }
        stats.draws = draws.len();

        if !uniforms.is_empty() {
            if self.uniform_capacity < uniforms.len() || self.uniform_buffer.is_none() {
                let capacity = uniforms.len().next_power_of_two();
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV alignment depth pyramid uniforms"),
                    size: capacity.saturating_mul(self.uniform_stride) as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.uniform_bind_group =
                    Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("RGV alignment depth pyramid uniform bind group"),
                        layout: &self.uniform_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &buffer,
                                offset: 0,
                                size: NonZeroU64::new(size_of::<DepthPyramidGpuUniform>() as u64),
                            }),
                        }],
                    }));
                self.uniform_buffer = Some(buffer);
                self.uniform_capacity = capacity;
            }
            let mut bytes = vec![0_u8; uniforms.len().saturating_mul(self.uniform_stride)];
            for (index, uniform) in uniforms.iter().enumerate() {
                let offset = index * self.uniform_stride;
                bytes[offset..offset + size_of::<DepthPyramidGpuUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(uniform));
            }
            if let Some(buffer) = &self.uniform_buffer {
                queue.write_buffer(buffer, 0, &bytes);
            }
        }

        // Least recently used pages go first; nothing this frame draws is evicted.
        if self.page_bytes > self.page_byte_limit {
            let mut victims = Vec::new();
            let mut remaining = self.page_bytes;
            // The reclaim index is the LRU order.
            for (last_used, id) in &self.cache.reclaim_index {
                if remaining <= self.page_byte_limit {
                    break;
                }
                if *last_used == revision {
                    continue;
                }
                if let Some(page) = self.cache.page(*id) {
                    remaining = remaining.saturating_sub(page.bytes);
                    victims.push(*id);
                }
            }
            stats.pages_evicted += victims.len();
            self.cache.remove_reclaim_candidates(&victims);
            self.page_bytes = remaining;
            if let Some(ledger) = &self.ledger {
                // A failure leaves the larger (still exactly charged) table in place.
                let _ = self.cache.reclaim_sparse_page_table(ledger, revision);
            }
        }
        Ok((
            AlignmentDepthPyramidGpuFrame {
                draws,
                revision,
                device_generation: self.device_generation,
                refused_by_request,
            },
            stats,
        ))
    }

    /// Charges one page of `bytes` (and room for it in the page table) to the ledger. When
    /// the GPU budget is full, this renderer's pages that the current frame does not draw are
    /// evicted, least recently used first, and the charge is tried once more. `Ok(None)`
    /// without a ledger.
    fn charge_page(
        &mut self,
        bytes: usize,
        revision: u64,
        stats: &mut AlignmentDepthPyramidGpuPrepareStats,
    ) -> Result<Option<AllocationLease>, PageTableError> {
        let Some(ledger) = self.ledger.clone() else {
            return Ok(None);
        };
        let attempt = |cache: &mut PagedGpuCache<GpuPyramidPage>| {
            cache.ensure_page_table_capacity(
                &ledger,
                cache.pages.len() + 1,
                ReservationKind::Normal,
                revision,
            )?;
            if bytes == 0 {
                return Ok(None);
            }
            Ok(Some(
                ledger
                    .reserve(AllocationClass::GpuPage, bytes, ReservationKind::Normal)?
                    .register(bytes, revision)?,
            ))
        };
        match attempt(&mut self.cache) {
            Err(PageTableError::Ledger(LedgerError::BudgetExceeded {
                requested, limit, ..
            })) if requested <= limit => {
                let drawn = self
                    .cache
                    .pages
                    .iter()
                    .filter(|page| page.last_used == revision)
                    .map(|page| page.key)
                    .collect::<Vec<_>>();
                let before = self.cache.pages.len();
                self.cache.evict_unpinned_pages_for_excluding(
                    &ledger,
                    requested,
                    limit,
                    &drawn,
                    revision,
                    &self.keys_inspected,
                )?;
                stats.pages_evicted += before - self.cache.pages.len();
                self.page_bytes = self.cache.pages.iter().map(|page| page.bytes).sum();
                attempt(&mut self.cache)
            }
            other => other,
        }
    }

    /// Replays the latest prepared frame.
    pub fn render<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentDepthPyramidGpuFrame,
    ) -> Result<(), AlignmentDepthPyramidGpuError> {
        self.render_within(pass, frame, [u32::MAX; 2])
    }

    /// [`Self::render`] with every scissor intersected with a target of `target_size` physical
    /// pixels; draws wholly outside it are skipped.
    pub fn render_within<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentDepthPyramidGpuFrame,
        target_size: [u32; 2],
    ) -> Result<(), AlignmentDepthPyramidGpuError> {
        if frame.device_generation != self.device_generation {
            return Err(AlignmentDepthPyramidGpuError::DeviceGenerationMismatch {
                frame: frame.device_generation,
                renderer: self.device_generation,
            });
        }
        if frame.draws.is_empty() {
            return Ok(());
        }
        // The uniform buffer holds the latest prepare's cameras only.
        if frame.revision != self.revision {
            return Err(AlignmentDepthPyramidGpuError::StaleFrame);
        }
        let Some(bind_group) = &self.uniform_bind_group else {
            return Err(AlignmentDepthPyramidGpuError::StaleFrame);
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, self.palette.bind_group(), &[]);
        let stride = size_of::<AlignmentDepthPyramidGpuInstance>() as u64;
        for draw in &frame.draws {
            let page = self
                .cache
                .page(draw.page)
                .ok_or(AlignmentDepthPyramidGpuError::MissingPage(draw.page))?;
            let Some([x, y, width, height]) = crate::clamp_scissor_to_target(
                [
                    draw.scissor.x,
                    draw.scissor.y,
                    draw.scissor.width,
                    draw.scissor.height,
                ],
                target_size,
            ) else {
                continue;
            };
            pass.set_scissor_rect(x, y, width, height);
            pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
            // The slice starts at the run's first bin, so `instance_index` counts from it on
            // every backend.
            let first = u64::from(draw.first_instance) * stride;
            let last = first + u64::from(draw.instances) * stride;
            pass.set_vertex_buffer(0, page.buffer.slice(first..last));
            pass.draw(0..VERTICES_PER_BIN, 0..draw.instances);
        }
        Ok(())
    }
}
