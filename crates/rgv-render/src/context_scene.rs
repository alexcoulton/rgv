use std::collections::{HashMap, HashSet};

use bytemuck::{Pod, Zeroable};
use rgv_layout::RectPrimitive;

const CONTEXT_SCENE_SHADER: &str = include_str!("context_scene.wgsl");
const CAMERA_RING_SIZE: usize = 3;

/// Stable GPU ownership key for one contextual lane in one panel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContextSceneGpuLaneKey {
    pub scope: u64,
    pub lane: u64,
}

/// Identifies a set of lanes whose replacement geometry must be admitted together. The scope is
/// normally a panel identity; `group` is stable only within that scope.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContextSceneGpuCoherenceKey {
    pub scope: u64,
    pub group: u64,
}

impl ContextSceneGpuCoherenceKey {
    #[must_use]
    pub const fn new(scope: u64, group: u64) -> Self {
        Self { scope, group }
    }
}

impl ContextSceneGpuLaneKey {
    #[must_use]
    pub const fn new(scope: u64, lane: u64) -> Self {
        Self { scope, lane }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContextSceneGpuScissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ContextSceneGpuScissor {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Camera-only state for a resident lane. Source geometry remains in the logical-pixel space of
/// the viewport for which it was laid out. Pan and zoom update only this affine transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextSceneGpuCamera {
    pub source_exact_start: f64,
    pub source_bases_per_pixel: f64,
    pub target_exact_start: f64,
    pub target_bases_per_pixel: f64,
    pub panel_left: f32,
    pub frame_top: f32,
    pub vertical_scroll: f32,
    pub scissor: ContextSceneGpuScissor,
    pub selected_object_id: Option<u32>,
    pub hovered_object_id: Option<u32>,
}

/// Previously committed geometry which may be retained if a desired replacement cannot be
/// admitted. It is explicit rather than inferred from renderer residency: GPU buffers are a
/// disposable derivative and never become semantic authority.
#[derive(Clone, Copy, Debug)]
pub struct ContextSceneGpuRetainedLaneRequest<'a> {
    pub content_revision: u64,
    pub camera: ContextSceneGpuCamera,
    pub rectangles: &'a [RectPrimitive],
}

/// One independently admissible lane request. `content_revision` changes only when semantic
/// geometry changes; camera movement must keep it stable.
#[derive(Clone, Copy, Debug)]
pub struct ContextSceneGpuLaneRequest<'a> {
    pub key: ContextSceneGpuLaneKey,
    pub content_revision: u64,
    pub camera: ContextSceneGpuCamera,
    pub rectangles: &'a [RectPrimitive],
    pub retained: Option<ContextSceneGpuRetainedLaneRequest<'a>>,
    pub coherence_group: Option<ContextSceneGpuCoherenceKey>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct ContextRectangleInstance {
    origin: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
    object_id: u32,
    padding: [u32; 3],
}

impl From<RectPrimitive> for ContextRectangleInstance {
    fn from(value: RectPrimitive) -> Self {
        Self {
            origin: [value.x, value.y],
            size: [value.width, value.height],
            color: [
                value.color.red,
                value.color.green,
                value.color.blue,
                value.color.alpha,
            ],
            object_id: value.object_id,
            padding: [value.shape as u32, 0, 0],
        }
    }
}

impl ContextRectangleInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x4,
            3 => Uint32,
            4 => Uint32x3
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct ContextCameraUniform {
    screen_size: [f32; 2],
    x_scale: f32,
    x_offset: f32,
    y_offset: f32,
    selected_object_id: u32,
    hovered_object_id: u32,
    padding: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContextSceneGpuPrepareStats {
    pub requested_lanes: usize,
    pub admitted_lanes: usize,
    pub reused_lanes: usize,
    pub rejected_lanes: usize,
    pub uploaded_instances: usize,
    pub uploaded_bytes: usize,
    pub camera_writes: usize,
    pub resident_lanes: usize,
    pub resident_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextSceneGpuRejectionReason {
    Budget,
    DeviceLimit,
    InvalidCamera,
    InvalidRevision,
    InvalidRequest,
    CoherenceGroup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextSceneGpuRejection {
    pub key: ContextSceneGpuLaneKey,
    pub content_revision: u64,
    pub reason: ContextSceneGpuRejectionReason,
    pub required_bytes: usize,
    pub budget_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContextSceneGpuDraw {
    key: ContextSceneGpuLaneKey,
    content_revision: u64,
    camera_slot: usize,
    scissor: ContextSceneGpuScissor,
}

/// Immutable draw authority returned by one prepare pass. A rejected candidate may name the
/// explicitly supplied retained revision, but the renderer never invents or implicitly borrows
/// stale pixels from its resident cache.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextSceneGpuFrame {
    draws: Vec<ContextSceneGpuDraw>,
    publications: Vec<(ContextSceneGpuLaneKey, u64)>,
}

impl ContextSceneGpuFrame {
    pub fn active_revisions(&self) -> impl Iterator<Item = (ContextSceneGpuLaneKey, u64)> + '_ {
        self.publications.iter().copied()
    }

    #[must_use]
    pub fn contains(&self, key: ContextSceneGpuLaneKey, content_revision: u64) -> bool {
        self.publications.contains(&(key, content_revision))
    }

    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.publications.len()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextSceneGpuPrepareReport {
    pub frame: ContextSceneGpuFrame,
    pub rejections: Vec<ContextSceneGpuRejection>,
    pub stats: ContextSceneGpuPrepareStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextSceneGpuCameraCaptureError {
    /// The requested lane set or one content revision differs from resident immutable geometry.
    /// The caller must run an explicit content-admission transaction before camera capture.
    ContentReplacementRequired {
        requested_lanes: usize,
        resident_lanes: usize,
    },
    InvalidCamera {
        key: ContextSceneGpuLaneKey,
    },
}

struct ResidentContextLane {
    content_revision: u64,
    instance_buffer: wgpu::Buffer,
    instance_count: u32,
    camera_buffers: [wgpu::Buffer; CAMERA_RING_SIZE],
    camera_bind_groups: [wgpu::BindGroup; CAMERA_RING_SIZE],
    allocated_bytes: usize,
}

#[derive(Clone, Copy)]
struct RequestedContextLaneContent<'a> {
    key: ContextSceneGpuLaneKey,
    content_revision: u64,
    camera: ContextSceneGpuCamera,
    rectangles: &'a [RectPrimitive],
}

#[derive(Clone, Copy)]
struct PreparedContextLane<'a> {
    content: RequestedContextLaneContent<'a>,
    uniform: Option<ContextCameraUniform>,
    required_bytes: usize,
}

struct PreparedContextUnit<'a> {
    lanes: Vec<PreparedContextLane<'a>>,
    required_bytes: usize,
}

pub struct ContextSceneGpuRenderer {
    pipeline: wgpu::RenderPipeline,
    camera_layout: wgpu::BindGroupLayout,
    resident: HashMap<ContextSceneGpuLaneKey, ResidentContextLane>,
    camera_sequence: u64,
    last_prepare_stats: ContextSceneGpuPrepareStats,
}

impl ContextSceneGpuRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RGV contextual scene shader"),
            source: wgpu::ShaderSource::Wgsl(CONTEXT_SCENE_SHADER.into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV contextual lane camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV contextual scene pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV contextual scene pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[ContextRectangleInstance::vertex_buffer_layout()],
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
        Self {
            pipeline,
            camera_layout,
            resident: HashMap::new(),
            camera_sequence: 0,
            last_prepare_stats: ContextSceneGpuPrepareStats::default(),
        }
    }

    /// Admits independent lanes and explicit coherence groups. Already-ready lanes are reserved
    /// first; a replacement that exceeds the contextual budget keeps its supplied committed
    /// revision without disturbing unrelated siblings.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_size: [f32; 2],
        requests: &[ContextSceneGpuLaneRequest<'_>],
        maximum_resident_bytes: usize,
    ) -> ContextSceneGpuPrepareReport {
        self.camera_sequence = self.camera_sequence.saturating_add(1);
        let camera_slot = (self.camera_sequence as usize) % CAMERA_RING_SIZE;
        let mut report = ContextSceneGpuPrepareReport {
            stats: ContextSceneGpuPrepareStats {
                requested_lanes: requests.len(),
                ..ContextSceneGpuPrepareStats::default()
            },
            ..ContextSceneGpuPrepareReport::default()
        };

        // A lane key is one publication authority. Duplicate requests would make budget order
        // and the resulting frame ambiguous, so reject the duplicate without disturbing the
        // first request.
        let mut requested_keys = HashSet::new();
        let mut valid_indices = Vec::with_capacity(requests.len());
        for (index, request) in requests.iter().enumerate() {
            if requested_keys.insert(request.key) {
                valid_indices.push(index);
            } else {
                report.rejections.push(ContextSceneGpuRejection {
                    key: request.key,
                    content_revision: request.content_revision,
                    reason: ContextSceneGpuRejectionReason::InvalidRequest,
                    required_bytes: 0,
                    budget_bytes: maximum_resident_bytes,
                });
            }
        }
        self.resident.retain(|key, _| requested_keys.contains(key));

        // Form the transitive coherence units supplied by the desktop. Ungrouped lanes are units
        // of one. Request order is stable and therefore makes low-budget admission deterministic.
        let mut units = Vec::<Vec<usize>>::new();
        let mut grouped_units = HashMap::<ContextSceneGpuCoherenceKey, usize>::new();
        for index in valid_indices {
            if let Some(group) = requests[index].coherence_group {
                if let Some(unit) = grouped_units.get(&group).copied() {
                    units[unit].push(index);
                } else {
                    grouped_units.insert(group, units.len());
                    units.push(vec![index]);
                }
            } else {
                units.push(vec![index]);
            }
        }

        // Reserve complete previously committed coherence units first. New work may replace its
        // own retained allocation, but can never evict an unrelated ready lane merely because it
        // appeared earlier in the request list.
        let mut choices = (0..units.len())
            .map(|_| None)
            .collect::<Vec<Option<PreparedContextUnit<'_>>>>();
        let mut admitted_bytes = 0_usize;
        for (unit_index, indices) in units.iter().enumerate() {
            if !indices
                .iter()
                .all(|index| requests[*index].retained.is_some())
            {
                continue;
            }
            let retained = prepare_context_unit(
                device,
                screen_size,
                requests,
                indices,
                true,
                maximum_resident_bytes,
            );
            let Some(Ok(retained)) = retained else {
                continue;
            };
            if admitted_bytes.saturating_add(retained.required_bytes) <= maximum_resident_bytes {
                admitted_bytes = admitted_bytes.saturating_add(retained.required_bytes);
                choices[unit_index] = Some(retained);
            }
        }

        // Upgrade retained units, then admit first publications, as atomic units. A failed
        // upgrade leaves its prior choice intact and emits typed candidate rejections.
        for (unit_index, indices) in units.iter().enumerate() {
            let desired = prepare_context_unit(
                device,
                screen_size,
                requests,
                indices,
                false,
                maximum_resident_bytes,
            );
            let Some(desired) = desired else {
                unreachable!("every request has desired contextual content")
            };
            let desired = match desired {
                Ok(desired) => desired,
                Err(rejections) => {
                    report.rejections.extend(rejections);
                    continue;
                }
            };
            let previous_bytes = choices[unit_index]
                .as_ref()
                .map_or(0, |choice| choice.required_bytes);
            let proposed_bytes = admitted_bytes
                .saturating_sub(previous_bytes)
                .saturating_add(desired.required_bytes);
            if proposed_bytes <= maximum_resident_bytes {
                admitted_bytes = proposed_bytes;
                choices[unit_index] = Some(desired);
                continue;
            }
            let chosen = choices[unit_index].as_ref();
            let group_has_replacement = indices.iter().any(|index| {
                requests[*index].retained.is_some_and(|retained| {
                    retained.content_revision != requests[*index].content_revision
                })
            });
            for lane in &desired.lanes {
                if chosen.is_some_and(|chosen| {
                    chosen.lanes.iter().any(|retained| {
                        retained.content.key == lane.content.key
                            && retained.content.content_revision == lane.content.content_revision
                    })
                }) {
                    continue;
                }
                let request = &requests[indices
                    .iter()
                    .copied()
                    .find(|index| requests[*index].key == lane.content.key)
                    .expect("prepared lane came from its coherence unit")];
                let reason = if group_has_replacement
                    && request.coherence_group.is_some()
                    && request.retained.is_some_and(|retained| {
                        retained.content_revision == request.content_revision
                    }) {
                    ContextSceneGpuRejectionReason::CoherenceGroup
                } else {
                    ContextSceneGpuRejectionReason::Budget
                };
                report.rejections.push(ContextSceneGpuRejection {
                    key: lane.content.key,
                    content_revision: lane.content.content_revision,
                    reason,
                    required_bytes: lane.required_bytes,
                    budget_bytes: maximum_resident_bytes,
                });
            }
        }

        let mut admitted_keys = HashSet::new();
        for unit in choices.into_iter().flatten() {
            for prepared in unit.lanes {
                let content = prepared.content;
                let exact_resident = self
                    .resident
                    .get(&content.key)
                    .is_some_and(|lane| lane.content_revision == content.content_revision);
                if exact_resident {
                    report.stats.reused_lanes = report.stats.reused_lanes.saturating_add(1);
                } else {
                    let instances = content
                        .rectangles
                        .iter()
                        .copied()
                        .map(ContextRectangleInstance::from)
                        .collect::<Vec<_>>();
                    let lane = create_resident_lane(
                        device,
                        queue,
                        &self.camera_layout,
                        content.content_revision,
                        &instances,
                        prepared.required_bytes,
                    );
                    report.stats.admitted_lanes = report.stats.admitted_lanes.saturating_add(1);
                    report.stats.uploaded_instances = report
                        .stats
                        .uploaded_instances
                        .saturating_add(instances.len());
                    report.stats.uploaded_bytes = report
                        .stats
                        .uploaded_bytes
                        .saturating_add(std::mem::size_of_val(instances.as_slice()));
                    self.resident.insert(content.key, lane);
                }
                admitted_keys.insert(content.key);
                report
                    .frame
                    .publications
                    .push((content.key, content.content_revision));

                if let Some(uniform) = prepared.uniform {
                    let lane = self
                        .resident
                        .get(&content.key)
                        .expect("an admitted contextual lane is resident");
                    queue.write_buffer(
                        &lane.camera_buffers[camera_slot],
                        0,
                        bytemuck::bytes_of(&uniform),
                    );
                    report.stats.camera_writes = report.stats.camera_writes.saturating_add(1);
                    report.frame.draws.push(ContextSceneGpuDraw {
                        key: content.key,
                        content_revision: content.content_revision,
                        camera_slot,
                        scissor: content.camera.scissor,
                    });
                }
            }
        }
        self.resident.retain(|key, _| admitted_keys.contains(key));
        report.stats.rejected_lanes = report.rejections.len();
        report.stats.resident_lanes = self.resident.len();
        report.stats.resident_bytes = self.resident_bytes();
        self.last_prepare_stats = report.stats;
        report
    }

    /// Captures only camera/scissor/interaction state for an exactly resident contextual content
    /// set. It does not inspect rectangle arrays, calculate admission bytes, form coherence
    /// groups, create buffers, evict lanes, or upload immutable instances.
    pub fn capture_camera_frame(
        &mut self,
        queue: &wgpu::Queue,
        screen_size: [f32; 2],
        requests: &[ContextSceneGpuLaneRequest<'_>],
    ) -> Result<ContextSceneGpuPrepareReport, ContextSceneGpuCameraCaptureError> {
        let exact_content = requests.len() == self.resident.len()
            && requests.iter().all(|request| {
                self.resident
                    .get(&request.key)
                    .is_some_and(|lane| lane.content_revision == request.content_revision)
            })
            && requests
                .iter()
                .map(|request| request.key)
                .collect::<HashSet<_>>()
                .len()
                == requests.len();
        if !exact_content {
            return Err(
                ContextSceneGpuCameraCaptureError::ContentReplacementRequired {
                    requested_lanes: requests.len(),
                    resident_lanes: self.resident.len(),
                },
            );
        }

        self.camera_sequence = self.camera_sequence.saturating_add(1);
        let camera_slot = (self.camera_sequence as usize) % CAMERA_RING_SIZE;
        let mut report = ContextSceneGpuPrepareReport {
            stats: ContextSceneGpuPrepareStats {
                requested_lanes: requests.len(),
                reused_lanes: requests.len(),
                resident_lanes: self.resident.len(),
                resident_bytes: self.resident_bytes(),
                ..ContextSceneGpuPrepareStats::default()
            },
            ..ContextSceneGpuPrepareReport::default()
        };
        for request in requests {
            report
                .frame
                .publications
                .push((request.key, request.content_revision));
            if request.camera.scissor.is_empty() {
                continue;
            }
            let Some(uniform) = context_camera_uniform(screen_size, request.camera) else {
                return Err(ContextSceneGpuCameraCaptureError::InvalidCamera { key: request.key });
            };
            let lane = self
                .resident
                .get(&request.key)
                .expect("exact contextual content residency was proven");
            queue.write_buffer(
                &lane.camera_buffers[camera_slot],
                0,
                bytemuck::bytes_of(&uniform),
            );
            report.stats.camera_writes = report.stats.camera_writes.saturating_add(1);
            report.frame.draws.push(ContextSceneGpuDraw {
                key: request.key,
                content_revision: request.content_revision,
                camera_slot,
                scissor: request.camera.scissor,
            });
        }
        self.last_prepare_stats = report.stats;
        Ok(report)
    }

    pub fn render<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass ContextSceneGpuFrame,
    ) {
        pass.set_pipeline(&self.pipeline);
        for draw in &frame.draws {
            let Some(lane) = self.resident.get(&draw.key) else {
                continue;
            };
            if lane.content_revision != draw.content_revision || lane.instance_count == 0 {
                continue;
            }
            pass.set_scissor_rect(
                draw.scissor.x,
                draw.scissor.y,
                draw.scissor.width,
                draw.scissor.height,
            );
            pass.set_bind_group(0, &lane.camera_bind_groups[draw.camera_slot], &[]);
            pass.set_vertex_buffer(0, lane.instance_buffer.slice(..));
            pass.draw(0..6, 0..lane.instance_count);
        }
    }

    #[must_use]
    pub const fn last_prepare_stats(&self) -> ContextSceneGpuPrepareStats {
        self.last_prepare_stats
    }

    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.resident.values().fold(0_usize, |total, lane| {
            total.saturating_add(lane.allocated_bytes)
        })
    }

    pub fn discard_all(&mut self) {
        self.resident.clear();
        self.last_prepare_stats = ContextSceneGpuPrepareStats::default();
    }
}

fn prepare_context_unit<'a>(
    device: &wgpu::Device,
    screen_size: [f32; 2],
    requests: &'a [ContextSceneGpuLaneRequest<'a>],
    indices: &[usize],
    retained: bool,
    maximum_resident_bytes: usize,
) -> Option<Result<PreparedContextUnit<'a>, Vec<ContextSceneGpuRejection>>> {
    let mut lanes = Vec::with_capacity(indices.len());
    let mut rejections = Vec::new();
    for index in indices {
        let request = &requests[*index];
        let content = if retained {
            let retained = request.retained?;
            RequestedContextLaneContent {
                key: request.key,
                content_revision: retained.content_revision,
                camera: retained.camera,
                rectangles: retained.rectangles,
            }
        } else {
            RequestedContextLaneContent {
                key: request.key,
                content_revision: request.content_revision,
                camera: request.camera,
                rectangles: request.rectangles,
            }
        };
        match prepare_context_lane(device, screen_size, content, maximum_resident_bytes) {
            Ok(prepared) => lanes.push(prepared),
            Err(rejection) => rejections.push(rejection),
        }
    }
    if !rejections.is_empty() {
        let rejected_keys = rejections
            .iter()
            .map(|rejection| rejection.key)
            .collect::<HashSet<_>>();
        if indices.len() > 1 {
            for index in indices {
                let request = &requests[*index];
                if !rejected_keys.contains(&request.key) {
                    let content_revision = if retained {
                        request
                            .retained
                            .expect("a retained coherence unit is complete")
                            .content_revision
                    } else {
                        request.content_revision
                    };
                    rejections.push(ContextSceneGpuRejection {
                        key: request.key,
                        content_revision,
                        reason: ContextSceneGpuRejectionReason::CoherenceGroup,
                        required_bytes: resident_lane_bytes(if retained {
                            request
                                .retained
                                .expect("a retained coherence unit is complete")
                                .rectangles
                                .len()
                        } else {
                            request.rectangles.len()
                        })
                        .unwrap_or(usize::MAX),
                        budget_bytes: maximum_resident_bytes,
                    });
                }
            }
        }
        return Some(Err(rejections));
    }
    let required_bytes = lanes.iter().fold(0_usize, |total, lane| {
        total.saturating_add(lane.required_bytes)
    });
    Some(Ok(PreparedContextUnit {
        lanes,
        required_bytes,
    }))
}

fn prepare_context_lane<'a>(
    device: &wgpu::Device,
    screen_size: [f32; 2],
    content: RequestedContextLaneContent<'a>,
    maximum_resident_bytes: usize,
) -> Result<PreparedContextLane<'a>, ContextSceneGpuRejection> {
    if content.content_revision == 0 {
        return Err(ContextSceneGpuRejection {
            key: content.key,
            content_revision: content.content_revision,
            reason: ContextSceneGpuRejectionReason::InvalidRevision,
            required_bytes: 0,
            budget_bytes: maximum_resident_bytes,
        });
    }
    let Some(required_bytes) = resident_lane_bytes(content.rectangles.len()) else {
        return Err(ContextSceneGpuRejection {
            key: content.key,
            content_revision: content.content_revision,
            reason: ContextSceneGpuRejectionReason::DeviceLimit,
            required_bytes: usize::MAX,
            budget_bytes: maximum_resident_bytes,
        });
    };
    let buffer_bytes = required_bytes.saturating_sub(camera_ring_bytes());
    let device_buffer_limit =
        usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
    if buffer_bytes > device_buffer_limit || content.rectangles.len() > u32::MAX as usize {
        return Err(ContextSceneGpuRejection {
            key: content.key,
            content_revision: content.content_revision,
            reason: ContextSceneGpuRejectionReason::DeviceLimit,
            required_bytes,
            budget_bytes: maximum_resident_bytes,
        });
    }
    let uniform = if content.camera.scissor.is_empty() {
        None
    } else {
        Some(context_camera_uniform(screen_size, content.camera).ok_or(
            ContextSceneGpuRejection {
                key: content.key,
                content_revision: content.content_revision,
                reason: ContextSceneGpuRejectionReason::InvalidCamera,
                required_bytes,
                budget_bytes: maximum_resident_bytes,
            },
        )?)
    };
    Ok(PreparedContextLane {
        content,
        uniform,
        required_bytes,
    })
}

fn create_resident_lane(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    camera_layout: &wgpu::BindGroupLayout,
    content_revision: u64,
    instances: &[ContextRectangleInstance],
    allocated_bytes: usize,
) -> ResidentContextLane {
    let instance_capacity = instances.len().max(1).next_power_of_two();
    let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("RGV contextual lane instances"),
        size: (instance_capacity.saturating_mul(std::mem::size_of::<ContextRectangleInstance>()))
            as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    if !instances.is_empty() {
        queue.write_buffer(&instance_buffer, 0, bytemuck::cast_slice(instances));
    }
    let camera_buffers = std::array::from_fn(|_| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RGV contextual lane camera"),
            size: std::mem::size_of::<ContextCameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });
    let camera_bind_groups = std::array::from_fn(|index| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV contextual lane camera bind group"),
            layout: camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffers[index].as_entire_binding(),
            }],
        })
    });
    ResidentContextLane {
        content_revision,
        instance_buffer,
        instance_count: u32::try_from(instances.len()).unwrap_or(u32::MAX),
        camera_buffers,
        camera_bind_groups,
        allocated_bytes,
    }
}

fn resident_lane_bytes(instance_count: usize) -> Option<usize> {
    let instance_capacity = instance_count.max(1).checked_next_power_of_two()?;
    instance_capacity
        .checked_mul(std::mem::size_of::<ContextRectangleInstance>())?
        .checked_add(camera_ring_bytes())
}

const fn camera_ring_bytes() -> usize {
    CAMERA_RING_SIZE * std::mem::size_of::<ContextCameraUniform>()
}

fn context_camera_uniform(
    screen_size: [f32; 2],
    camera: ContextSceneGpuCamera,
) -> Option<ContextCameraUniform> {
    if !camera.source_exact_start.is_finite()
        || !camera.source_bases_per_pixel.is_finite()
        || camera.source_bases_per_pixel <= 0.0
        || !camera.target_exact_start.is_finite()
        || !camera.target_bases_per_pixel.is_finite()
        || camera.target_bases_per_pixel <= 0.0
    {
        return None;
    }
    let x_scale = camera.source_bases_per_pixel / camera.target_bases_per_pixel;
    let x_offset = f64::from(camera.panel_left)
        + (camera.source_exact_start - camera.target_exact_start) / camera.target_bases_per_pixel;
    if !x_scale.is_finite() || !x_offset.is_finite() {
        return None;
    }
    Some(ContextCameraUniform {
        screen_size: [screen_size[0].max(1.0), screen_size[1].max(1.0)],
        x_scale: x_scale as f32,
        x_offset: x_offset as f32,
        y_offset: camera.frame_top - camera.vertical_scroll,
        selected_object_id: camera.selected_object_id.unwrap_or_default(),
        hovered_object_id: camera.hovered_object_id.unwrap_or_default(),
        padding: 0,
    })
}

/// CPU oracle for the shader's affine transform.
#[must_use]
pub fn project_context_scene_rectangle(
    rectangle: RectPrimitive,
    camera: ContextSceneGpuCamera,
) -> Option<RectPrimitive> {
    let uniform = context_camera_uniform([1.0, 1.0], camera)?;
    let mut projected = rectangle;
    projected.x = rectangle.x.mul_add(uniform.x_scale, uniform.x_offset);
    projected.width = rectangle.width * uniform.x_scale;
    projected.y = rectangle.y + uniform.y_offset;
    Some(projected)
}

#[cfg(test)]
mod tests {
    use rgv_layout::{RectShape, Rgba};

    use super::*;

    fn rectangle() -> RectPrimitive {
        RectPrimitive {
            x: 1_100.0,
            y: 5.0,
            width: 200.0,
            height: 8.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 7,
            shape: RectShape::DirectionalTerminalRight,
        }
    }

    fn camera() -> ContextSceneGpuCamera {
        ContextSceneGpuCamera {
            source_exact_start: 0.0,
            source_bases_per_pixel: 1.0,
            target_exact_start: 1_000.0,
            target_bases_per_pixel: 1.0,
            panel_left: 40.0,
            frame_top: 70.0,
            vertical_scroll: 30.0,
            scissor: ContextSceneGpuScissor {
                x: 0,
                y: 0,
                width: 1_000,
                height: 500,
            },
            selected_object_id: None,
            hovered_object_id: None,
        }
    }

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("RGV contextual scene test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    #[test]
    fn camera_oracle_preserves_genomic_and_lane_mapping() {
        let projected = project_context_scene_rectangle(rectangle(), camera()).unwrap();
        assert_eq!(projected.x, 140.0);
        assert_eq!(projected.width, 200.0);
        assert_eq!(projected.y, 45.0);
        assert_eq!(projected.height, 8.0);
        assert_eq!(projected.shape, RectShape::DirectionalTerminalRight);
    }

    #[test]
    fn zoom_is_a_camera_transform_not_a_geometry_change() {
        let mut zoomed = camera();
        zoomed.target_exact_start = 1_050.0;
        zoomed.target_bases_per_pixel = 0.5;
        let projected = project_context_scene_rectangle(rectangle(), zoomed).unwrap();
        assert_eq!(projected.x, 140.0);
        assert_eq!(projected.width, 400.0);
    }

    #[test]
    fn resident_charge_includes_preallocated_camera_ring() {
        assert_eq!(resident_lane_bytes(0), resident_lane_bytes(1));
        assert!(resident_lane_bytes(2).unwrap() > camera_ring_bytes());
        assert_eq!(
            resident_lane_bytes(3),
            resident_lane_bytes(4),
            "geometry buffers charge their actual power-of-two allocation",
        );
    }

    #[test]
    fn invalid_camera_is_rejected_without_projection() {
        let mut invalid = camera();
        invalid.target_bases_per_pixel = 0.0;
        assert!(project_context_scene_rectangle(rectangle(), invalid).is_none());
    }

    #[test]
    fn camera_only_navigation_reuses_geometry_and_rejection_is_lane_local() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let mut renderer =
            ContextSceneGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let first_rectangles = [rectangle()];
        let first_key = ContextSceneGpuLaneKey::new(1, 1);
        let initial = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[ContextSceneGpuLaneRequest {
                key: first_key,
                content_revision: 1,
                camera: camera(),
                rectangles: &first_rectangles,
                retained: None,
                coherence_group: None,
            }],
            1024 * 1024,
        );
        assert_eq!(initial.stats.admitted_lanes, 1);
        assert!(initial.stats.uploaded_bytes > 0);
        assert!(initial.frame.contains(first_key, 1));

        let mut offscreen_camera = camera();
        offscreen_camera.scissor.height = 0;
        let offscreen = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[ContextSceneGpuLaneRequest {
                key: first_key,
                content_revision: 1,
                camera: offscreen_camera,
                rectangles: &first_rectangles,
                retained: Some(ContextSceneGpuRetainedLaneRequest {
                    content_revision: 1,
                    camera: offscreen_camera,
                    rectangles: &first_rectangles,
                }),
                coherence_group: None,
            }],
            initial.stats.resident_bytes,
        );
        assert!(offscreen.frame.contains(first_key, 1));
        assert_eq!(offscreen.stats.camera_writes, 0);
        assert_eq!(offscreen.stats.uploaded_bytes, 0);

        let mut panned_camera = camera();
        panned_camera.target_exact_start += 25.5;
        let camera_only = renderer
            .capture_camera_frame(
                &queue,
                [1_000.0, 500.0],
                &[ContextSceneGpuLaneRequest {
                    key: first_key,
                    content_revision: 1,
                    camera: panned_camera,
                    // Camera capture deliberately never reads semantic geometry.
                    rectangles: &[],
                    retained: None,
                    coherence_group: None,
                }],
            )
            .unwrap();
        assert_eq!(camera_only.stats.reused_lanes, 1);
        assert_eq!(camera_only.stats.uploaded_instances, 0);
        assert_eq!(camera_only.stats.uploaded_bytes, 0);
        assert_eq!(camera_only.stats.camera_writes, 1);

        assert!(matches!(
            renderer.capture_camera_frame(
                &queue,
                [1_000.0, 500.0],
                &[ContextSceneGpuLaneRequest {
                    key: first_key,
                    content_revision: 2,
                    camera: panned_camera,
                    rectangles: &[],
                    retained: None,
                    coherence_group: None,
                }],
            ),
            Err(ContextSceneGpuCameraCaptureError::ContentReplacementRequired { .. })
        ));

        let second_rectangles = [rectangle(), rectangle()];
        let second_key = ContextSceneGpuLaneKey::new(1, 2);
        let isolated_pressure = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[
                ContextSceneGpuLaneRequest {
                    key: first_key,
                    content_revision: 1,
                    camera: panned_camera,
                    rectangles: &first_rectangles,
                    retained: None,
                    coherence_group: None,
                },
                ContextSceneGpuLaneRequest {
                    key: second_key,
                    content_revision: 1,
                    camera: camera(),
                    rectangles: &second_rectangles,
                    retained: None,
                    coherence_group: None,
                },
            ],
            initial.stats.resident_bytes,
        );
        assert!(isolated_pressure.frame.contains(first_key, 1));
        assert!(!isolated_pressure.frame.contains(second_key, 1));
        assert_eq!(isolated_pressure.rejections.len(), 1);
        assert_eq!(isolated_pressure.rejections[0].key, second_key);

        let larger_replacement = [rectangle(), rectangle()];
        let retained_replacement = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[ContextSceneGpuLaneRequest {
                key: first_key,
                content_revision: 2,
                camera: panned_camera,
                rectangles: &larger_replacement,
                retained: Some(ContextSceneGpuRetainedLaneRequest {
                    content_revision: 1,
                    camera: panned_camera,
                    rectangles: &first_rectangles,
                }),
                coherence_group: None,
            }],
            initial.stats.resident_bytes,
        );
        assert!(retained_replacement.frame.contains(first_key, 1));
        assert!(!retained_replacement.frame.contains(first_key, 2));
        assert_eq!(retained_replacement.stats.uploaded_bytes, 0);
        assert_eq!(retained_replacement.rejections.len(), 1);
        assert_eq!(
            retained_replacement.rejections[0].reason,
            ContextSceneGpuRejectionReason::Budget
        );

        let lowered_budget = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[ContextSceneGpuLaneRequest {
                key: first_key,
                content_revision: 1,
                camera: panned_camera,
                rectangles: &first_rectangles,
                retained: Some(ContextSceneGpuRetainedLaneRequest {
                    content_revision: 1,
                    camera: panned_camera,
                    rectangles: &first_rectangles,
                }),
                coherence_group: None,
            }],
            initial.stats.resident_bytes.saturating_sub(1),
        );
        assert!(!lowered_budget.frame.contains(first_key, 1));
        assert_eq!(lowered_budget.stats.resident_bytes, 0);
        assert!(
            lowered_budget.stats.resident_bytes <= initial.stats.resident_bytes.saturating_sub(1)
        );
        assert_eq!(lowered_budget.rejections.len(), 1);
        assert_eq!(
            lowered_budget.rejections[0].reason,
            ContextSceneGpuRejectionReason::Budget
        );
    }

    #[test]
    fn coherence_replacement_is_all_or_nothing_and_panel_scope_is_part_of_identity() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let mut renderer =
            ContextSceneGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let rectangles = [rectangle()];
        let left = ContextSceneGpuLaneKey::new(1, 7);
        let right = ContextSceneGpuLaneKey::new(1, 8);
        let other_panel = ContextSceneGpuLaneKey::new(2, 7);
        let initial = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[
                ContextSceneGpuLaneRequest {
                    key: left,
                    content_revision: 1,
                    camera: camera(),
                    rectangles: &rectangles,
                    retained: None,
                    coherence_group: None,
                },
                ContextSceneGpuLaneRequest {
                    key: right,
                    content_revision: 1,
                    camera: camera(),
                    rectangles: &rectangles,
                    retained: None,
                    coherence_group: None,
                },
                ContextSceneGpuLaneRequest {
                    key: other_panel,
                    content_revision: 1,
                    camera: camera(),
                    rectangles: &rectangles,
                    retained: None,
                    coherence_group: None,
                },
            ],
            usize::MAX,
        );
        assert!(initial.frame.contains(left, 1));
        assert!(initial.frame.contains(right, 1));
        assert!(initial.frame.contains(other_panel, 1));
        let lane_bytes = initial.stats.resident_bytes / 3;
        let group = ContextSceneGpuCoherenceKey::new(1, 7);
        let larger = [rectangle(), rectangle()];
        let replacement = renderer.prepare(
            &device,
            &queue,
            [1_000.0, 500.0],
            &[
                ContextSceneGpuLaneRequest {
                    key: left,
                    content_revision: 2,
                    camera: camera(),
                    rectangles: &larger,
                    retained: Some(ContextSceneGpuRetainedLaneRequest {
                        content_revision: 1,
                        camera: camera(),
                        rectangles: &rectangles,
                    }),
                    coherence_group: Some(group),
                },
                ContextSceneGpuLaneRequest {
                    key: right,
                    content_revision: 2,
                    camera: camera(),
                    rectangles: &larger,
                    retained: Some(ContextSceneGpuRetainedLaneRequest {
                        content_revision: 1,
                        camera: camera(),
                        rectangles: &rectangles,
                    }),
                    coherence_group: Some(group),
                },
                ContextSceneGpuLaneRequest {
                    key: other_panel,
                    content_revision: 1,
                    camera: camera(),
                    rectangles: &rectangles,
                    retained: Some(ContextSceneGpuRetainedLaneRequest {
                        content_revision: 1,
                        camera: camera(),
                        rectangles: &rectangles,
                    }),
                    coherence_group: None,
                },
            ],
            lane_bytes.saturating_mul(3),
        );
        assert!(replacement.frame.contains(left, 1));
        assert!(replacement.frame.contains(right, 1));
        assert!(!replacement.frame.contains(left, 2));
        assert!(!replacement.frame.contains(right, 2));
        assert!(replacement.frame.contains(other_panel, 1));
        assert_eq!(replacement.rejections.len(), 2);
        assert!(
            replacement
                .rejections
                .iter()
                .all(|rejection| { rejection.reason == ContextSceneGpuRejectionReason::Budget })
        );
    }
}
