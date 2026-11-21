use crate::ui::{TrackFrame, TrackFrameKey};
use rgv_core::{
    GenomicInterval, PanelId, QuantitativeScaleMode, TrackConfig, TrackId, TrackKind, Viewport,
};
use rgv_layout::DisplayList;
use rgv_render::{
    ContextSceneGpuFrame, ContextSceneGpuLaneKey, ContextSceneGpuRejection,
    ContextSceneGpuRejectionReason,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

/// Stable identity of one non-alignment genomic lane. Overlay members retain their own key even
/// when several members share one presentation frame, so readiness and replacement never become
/// panel-wide authority again.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ContextLaneKey {
    Reference,
    Auxiliary(TrackId),
}

impl ContextLaneKey {
    pub(crate) const fn numeric_id(self) -> u64 {
        match self {
            Self::Reference => 0,
            Self::Auxiliary(track_id) => track_id.get().saturating_add(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextLaneReadiness {
    Ready,
    Retained,
    RetainedUnavailable,
    Pending,
    Unavailable,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextLaneGpuReadiness {
    Pending,
    Ready,
    Unavailable,
}

impl ContextLaneGpuReadiness {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Pending => "GPU pending",
            Self::Ready => "GPU ready",
            Self::Unavailable => "GPU unavailable",
        }
    }
}

impl ContextLaneReadiness {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Retained => "retained",
            Self::RetainedUnavailable => "retained (source unavailable)",
            Self::Pending => "pending",
            Self::Unavailable => "unavailable",
            Self::Hidden => "hidden",
        }
    }
}

/// Immutable, lane-local semantic geometry. Horizontal coordinates are expressed in the source
/// viewport's logical pixels and vertical coordinates are relative to the lane frame. A camera
/// change therefore needs only one affine transform; parsing, row packing, and label placement do
/// not run on the input path.
#[derive(Clone, Debug)]
pub(crate) struct ContextLaneLease {
    pub(crate) key: ContextLaneKey,
    pub(crate) revision: u64,
    pub(crate) generation: u64,
    pub(crate) source_viewport: Viewport,
    pub(crate) frame_key: TrackFrameKey,
    pub(crate) display: Arc<DisplayList>,
    /// Finite text anchors sorted by immutable source x. Camera projection binary-searches this
    /// directory instead of cloning/filtering every off-screen label in the buffered lane.
    text_order: Arc<[usize]>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextLaneDraft {
    pub(crate) key: ContextLaneKey,
    pub(crate) generation: u64,
    pub(crate) source_viewport: Viewport,
    pub(crate) frame_key: TrackFrameKey,
    pub(crate) frame_top: f32,
    pub(crate) display: DisplayList,
}

#[derive(Clone, Copy)]
pub(crate) struct ContextLaneGpuPublication<'a> {
    pub(crate) desired: &'a ContextLaneLease,
    pub(crate) retained: Option<&'a ContextLaneLease>,
    pub(crate) coherence_group: Option<u64>,
}

/// Lanes whose geometry is meaningful only when it was derived from the same source generation
/// and buffered interval. Overlay, shared-scale, and arithmetic groups use this contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContextLaneCoherenceGroup {
    pub(crate) id: String,
    pub(crate) members: BTreeSet<ContextLaneKey>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextLaneState {
    pub(crate) readiness: ContextLaneReadiness,
    pub(crate) requested_interval: Option<GenomicInterval>,
    pub(crate) failed: bool,
    pub(crate) lease: Option<Arc<ContextLaneLease>>,
    pub(crate) gpu_readiness: ContextLaneGpuReadiness,
    pub(crate) gpu_revision: Option<u64>,
    pub(crate) gpu_failure: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PanelContextScene {
    pub(crate) lanes: BTreeMap<ContextLaneKey, ContextLaneState>,
    pub(crate) coherence_groups: BTreeMap<String, BTreeSet<ContextLaneKey>>,
    pub(crate) projected: DisplayList,
    pub(crate) camera_text_anchors_examined: usize,
    pub(crate) camera_text_anchors_available: usize,
    candidates: BTreeMap<ContextLaneKey, Arc<ContextLaneLease>>,
    eligible_candidates: BTreeSet<ContextLaneKey>,
    coherence_components: BTreeMap<ContextLaneKey, u64>,
    next_revision: u64,
}

impl PanelContextScene {
    pub(crate) fn remove_auxiliary(&mut self, removed: &HashSet<TrackId>) {
        self.lanes.retain(|key, _| {
            !matches!(key, ContextLaneKey::Auxiliary(track_id) if removed.contains(track_id))
        });
        self.candidates.retain(|key, _| {
            !matches!(key, ContextLaneKey::Auxiliary(track_id) if removed.contains(track_id))
        });
        self.eligible_candidates.retain(
            |key| !matches!(key, ContextLaneKey::Auxiliary(track_id) if removed.contains(track_id)),
        );
        self.coherence_components.retain(|key, _| {
            !matches!(key, ContextLaneKey::Auxiliary(track_id) if removed.contains(track_id))
        });
        self.coherence_groups.retain(|_, members| {
            members.retain(|key| {
                !matches!(key, ContextLaneKey::Auxiliary(track_id) if removed.contains(track_id))
            });
            members.len() > 1
        });
    }

    pub(crate) fn publish(&mut self, mut draft: ContextLaneDraft) -> Arc<ContextLaneLease> {
        localize_display_vertical(&mut draft.display, draft.frame_top);
        let previous = self.candidates.get(&draft.key).or_else(|| {
            self.lanes
                .get(&draft.key)
                .and_then(|state| state.lease.as_ref())
        });
        if let Some(previous) = previous
            && previous.source_viewport == draft.source_viewport
            && previous.frame_key == draft.frame_key
            && previous.display.as_ref() == &draft.display
        {
            if previous.generation == draft.generation {
                return Arc::clone(previous);
            }
            return Arc::new(ContextLaneLease {
                key: draft.key,
                revision: previous.revision,
                generation: draft.generation,
                source_viewport: draft.source_viewport,
                frame_key: draft.frame_key,
                display: Arc::clone(&previous.display),
                text_order: Arc::clone(&previous.text_order),
            });
        }
        self.next_revision = self.next_revision.saturating_add(1).max(1);
        let text_order = context_text_order(&draft.display);
        Arc::new(ContextLaneLease {
            key: draft.key,
            revision: self.next_revision,
            generation: draft.generation,
            source_viewport: draft.source_viewport,
            frame_key: draft.frame_key,
            display: Arc::new(draft.display),
            text_order,
        })
    }

    /// Atomically reconciles only the lanes named by the current presentation. A missing or slow
    /// lane retains its own prior lease when useful; it cannot replace another lane's geometry or
    /// readiness.
    pub(crate) fn reconcile(
        &mut self,
        generation: u64,
        target: Viewport,
        visible: &BTreeMap<ContextLaneKey, (TrackFrameKey, Option<GenomicInterval>)>,
        drafts: Vec<ContextLaneDraft>,
        pending: &BTreeSet<ContextLaneKey>,
        unavailable: &BTreeSet<ContextLaneKey>,
        coherence_groups: &[ContextLaneCoherenceGroup],
    ) {
        for draft in drafts {
            let key = draft.key;
            let lease = self.publish(draft);
            self.candidates.insert(key, lease);
        }
        self.candidates.retain(|key, candidate| {
            visible.contains_key(key) && candidate.generation == generation
        });
        self.coherence_groups = coherence_groups
            .iter()
            .filter_map(|group| {
                let members = group
                    .members
                    .iter()
                    .copied()
                    .filter(|member| visible.contains_key(member))
                    .collect::<BTreeSet<_>>();
                (members.len() > 1).then(|| (group.id.clone(), members))
            })
            .collect();
        self.coherence_components = coherence_components(&self.coherence_groups);

        let mut blocked = BTreeSet::new();
        for members in self.coherence_groups.values() {
            let leases = members
                .iter()
                .filter_map(|key| self.candidates.get(key))
                .collect::<Vec<_>>();
            let coherent = leases.len() == members.len()
                && leases.first().is_some_and(|first| {
                    leases.iter().all(|lease| {
                        lease.generation == generation
                            && lease.generation == first.generation
                            && lease.source_viewport == first.source_viewport
                    })
                });
            if !coherent {
                blocked.extend(members.iter().copied());
            }
        }
        self.eligible_candidates = self
            .candidates
            .keys()
            .filter(|key| !blocked.contains(key))
            .copied()
            .collect();

        for (key, state) in &mut self.lanes {
            if !visible.contains_key(key) {
                state.readiness = ContextLaneReadiness::Hidden;
                state.requested_interval = None;
            }
        }
        for (key, (_, requested_interval)) in visible {
            let state = self.lanes.entry(*key).or_insert(ContextLaneState {
                readiness: ContextLaneReadiness::Pending,
                requested_interval: *requested_interval,
                failed: false,
                lease: None,
                gpu_readiness: ContextLaneGpuReadiness::Pending,
                gpu_revision: None,
                gpu_failure: None,
            });
            state.requested_interval = *requested_interval;
            state.failed = unavailable.contains(key);
            if self.eligible_candidates.contains(key)
                && let Some(candidate) = self.candidates.get(key)
                && state
                    .lease
                    .as_ref()
                    .map(|lease| (lease.revision, lease.generation))
                    != Some((candidate.revision, candidate.generation))
            {
                state.gpu_failure = None;
            }
            state.readiness = if let Some(lease) = state.lease.as_ref() {
                if lease
                    .source_viewport
                    .interval()
                    .contains_interval(target.interval())
                {
                    if state.failed {
                        ContextLaneReadiness::RetainedUnavailable
                    } else {
                        ContextLaneReadiness::Ready
                    }
                } else if lease.source_viewport.interval().overlaps(target.interval()) {
                    if state.failed {
                        ContextLaneReadiness::RetainedUnavailable
                    } else {
                        ContextLaneReadiness::Retained
                    }
                } else if self.eligible_candidates.contains(key) || pending.contains(key) {
                    ContextLaneReadiness::Pending
                } else if state.failed {
                    ContextLaneReadiness::Unavailable
                } else {
                    ContextLaneReadiness::Unavailable
                }
            } else if self.eligible_candidates.contains(key) || pending.contains(key) {
                ContextLaneReadiness::Pending
            } else {
                ContextLaneReadiness::Unavailable
            };
        }
    }

    pub(crate) fn update_target_readiness(&mut self, target: Viewport) {
        for (key, state) in &mut self.lanes {
            if state.readiness == ContextLaneReadiness::Hidden {
                continue;
            }
            state.readiness = match state.lease.as_ref() {
                Some(lease)
                    if lease
                        .source_viewport
                        .interval()
                        .contains_interval(target.interval()) =>
                {
                    if state.failed {
                        ContextLaneReadiness::RetainedUnavailable
                    } else {
                        ContextLaneReadiness::Ready
                    }
                }
                Some(lease) if lease.source_viewport.interval().overlaps(target.interval()) => {
                    if state.failed {
                        ContextLaneReadiness::RetainedUnavailable
                    } else {
                        ContextLaneReadiness::Retained
                    }
                }
                _ if state.failed => ContextLaneReadiness::Unavailable,
                _ if self.eligible_candidates.contains(key)
                    || state.requested_interval.is_some() =>
                {
                    ContextLaneReadiness::Pending
                }
                _ => ContextLaneReadiness::Unavailable,
            };
        }
    }

    pub(crate) fn project_content(&mut self, target: Viewport, frames: &[TrackFrame]) {
        let mut projected = DisplayList::default();
        for state in self.lanes.values() {
            if matches!(
                state.readiness,
                ContextLaneReadiness::Hidden | ContextLaneReadiness::Unavailable
            ) {
                continue;
            }
            let Some(lease) = state.lease.as_ref() else {
                continue;
            };
            if state.gpu_readiness != ContextLaneGpuReadiness::Ready
                || state.gpu_revision != Some(lease.revision)
            {
                continue;
            }
            let Some(frame) = frames.iter().find(|frame| frame.key == lease.frame_key) else {
                continue;
            };
            if !lease.source_viewport.interval().overlaps(target.interval()) {
                continue;
            }
            project_context_lane_display(
                lease.display.as_ref(),
                &lease.text_order,
                lease.source_viewport,
                target,
                frame.top,
                &mut projected,
            );
        }
        self.projected = projected;
    }

    /// Reprojects only camera-dependent contextual text. Immutable rectangles are already
    /// transformed by `ContextSceneGpuRenderer::capture_camera_frame`; rebuilding their CPU
    /// display list during a held pan would duplicate the native camera path and feed the generic
    /// rectangle compositor unnecessarily.
    pub(crate) fn project_camera_text(&mut self, target: Viewport, frames: &[TrackFrame]) {
        let mut projected = DisplayList::default();
        let mut examined = 0_usize;
        let mut available = 0_usize;
        for state in self.lanes.values() {
            if matches!(
                state.readiness,
                ContextLaneReadiness::Hidden | ContextLaneReadiness::Unavailable
            ) {
                continue;
            }
            let Some(lease) = state.lease.as_ref() else {
                continue;
            };
            if state.gpu_readiness != ContextLaneGpuReadiness::Ready
                || state.gpu_revision != Some(lease.revision)
            {
                continue;
            }
            let Some(frame) = frames.iter().find(|frame| frame.key == lease.frame_key) else {
                continue;
            };
            if !lease.source_viewport.interval().overlaps(target.interval()) {
                continue;
            }
            available = available.saturating_add(lease.text_order.len());
            examined = examined.saturating_add(project_context_lane_text(
                lease.display.as_ref(),
                &lease.text_order,
                lease.source_viewport,
                target,
                frame.top,
                &mut projected,
            ));
        }
        self.projected = projected;
        self.camera_text_anchors_examined = examined;
        self.camera_text_anchors_available = available;
    }

    pub(crate) fn gpu_publications(&self) -> impl Iterator<Item = ContextLaneGpuPublication<'_>> {
        self.lanes.iter().filter_map(|(key, state)| {
            if state.readiness == ContextLaneReadiness::Hidden {
                return None;
            }
            let candidate = self
                .eligible_candidates
                .contains(key)
                .then(|| self.candidates.get(key))
                .flatten()
                .map(Arc::as_ref);
            let desired = candidate.or_else(|| state.lease.as_deref())?;
            Some(ContextLaneGpuPublication {
                desired,
                retained: state.lease.as_deref(),
                coherence_group: self.coherence_components.get(key).copied(),
            })
        })
    }

    /// Commits source candidates only after their exact GPU revisions are admitted. Coherent
    /// groups advance together; a rejected candidate retains the preceding lane publication.
    pub(crate) fn apply_gpu_publication(
        &mut self,
        panel_id: PanelId,
        target: Viewport,
        frame: &ContextSceneGpuFrame,
        rejections: &[ContextSceneGpuRejection],
    ) -> bool {
        let desired = self
            .gpu_publications()
            .map(|publication| {
                (
                    publication.desired.key,
                    (publication.desired.revision, publication.coherence_group),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut commit = BTreeSet::new();
        for key in &self.eligible_candidates {
            let Some(candidate) = self.candidates.get(key) else {
                continue;
            };
            let admitted = if let Some(component) = self.coherence_components.get(key).copied() {
                self.coherence_components
                    .iter()
                    .filter(|(_, member_component)| **member_component == component)
                    .all(|(member, _)| {
                        desired.get(member).is_some_and(|(revision, _)| {
                            frame.contains(
                                ContextSceneGpuLaneKey::new(panel_id.get(), member.numeric_id()),
                                *revision,
                            )
                        })
                    })
            } else {
                frame.contains(
                    ContextSceneGpuLaneKey::new(panel_id.get(), key.numeric_id()),
                    candidate.revision,
                )
            };
            if admitted {
                commit.insert(*key);
            }
        }

        let mut changed = false;
        for key in commit {
            let Some(candidate) = self.candidates.remove(&key) else {
                continue;
            };
            self.eligible_candidates.remove(&key);
            let Some(state) = self.lanes.get_mut(&key) else {
                continue;
            };
            changed |= state.lease.as_ref().is_none_or(|lease| {
                lease.revision != candidate.revision || lease.generation != candidate.generation
            });
            state.lease = Some(candidate);
        }
        self.update_target_readiness(target);

        for (key, state) in &mut self.lanes {
            if state.readiness == ContextLaneReadiness::Hidden {
                continue;
            }
            let gpu_key = ContextSceneGpuLaneKey::new(panel_id.get(), key.numeric_id());
            let candidate_rejection = self.candidates.get(key).and_then(|candidate| {
                rejections.iter().find(|rejection| {
                    rejection.key == gpu_key && rejection.content_revision == candidate.revision
                })
            });
            let committed = state.lease.as_ref();
            let committed_ready =
                committed.is_some_and(|lease| frame.contains(gpu_key, lease.revision));
            let committed_rejection = committed.and_then(|lease| {
                rejections.iter().find(|rejection| {
                    rejection.key == gpu_key && rejection.content_revision == lease.revision
                })
            });
            let (next_readiness, next_revision, next_failure) = if committed_ready {
                (
                    ContextLaneGpuReadiness::Ready,
                    committed.map(|lease| lease.revision),
                    candidate_rejection.map(|rejection| {
                        format!(
                            "retained the committed contextual lane: {}",
                            context_gpu_failure(*rejection)
                        )
                    }),
                )
            } else if let Some(rejection) = candidate_rejection.or(committed_rejection) {
                (
                    ContextLaneGpuReadiness::Unavailable,
                    None,
                    Some(context_gpu_failure(*rejection)),
                )
            } else {
                (ContextLaneGpuReadiness::Pending, None, None)
            };
            changed |= state.gpu_readiness != next_readiness
                || state.gpu_revision != next_revision
                || state.gpu_failure != next_failure;
            state.gpu_readiness = next_readiness;
            state.gpu_revision = next_revision;
            state.gpu_failure = next_failure;
        }
        changed
    }
}

fn coherence_components(
    groups: &BTreeMap<String, BTreeSet<ContextLaneKey>>,
) -> BTreeMap<ContextLaneKey, u64> {
    let mut components = Vec::<BTreeSet<ContextLaneKey>>::new();
    for members in groups.values() {
        let mut merged = members.clone();
        let mut index = 0;
        while index < components.len() {
            if components[index].is_disjoint(&merged) {
                index += 1;
            } else {
                merged.append(&mut components.remove(index));
            }
        }
        components.push(merged);
    }
    let mut by_lane = BTreeMap::new();
    for members in components {
        let component = members
            .iter()
            .map(|member| member.numeric_id())
            .min()
            .expect("a contextual coherence group is non-empty");
        for member in members {
            by_lane.insert(member, component);
        }
    }
    by_lane
}

fn context_gpu_failure(rejection: ContextSceneGpuRejection) -> String {
    match rejection.reason {
        ContextSceneGpuRejectionReason::Budget => format!(
            "contextual GPU budget could not admit {} bytes under the {} byte limit",
            rejection.required_bytes, rejection.budget_bytes
        ),
        ContextSceneGpuRejectionReason::DeviceLimit => format!(
            "contextual geometry requires {} bytes and exceeds the GPU device limit",
            rejection.required_bytes
        ),
        ContextSceneGpuRejectionReason::InvalidCamera => {
            "contextual GPU camera contract is invalid".to_owned()
        }
        ContextSceneGpuRejectionReason::InvalidRevision => {
            "contextual GPU content revision is invalid".to_owned()
        }
        ContextSceneGpuRejectionReason::InvalidRequest => {
            "contextual GPU lane request duplicated an ownership key".to_owned()
        }
        ContextSceneGpuRejectionReason::CoherenceGroup => {
            "contextual GPU coherence peer could not be admitted".to_owned()
        }
    }
}

fn localize_display_vertical(display: &mut DisplayList, frame_top: f32) {
    for rectangle in &mut display.rectangles {
        rectangle.y -= frame_top;
    }
    for text in &mut display.texts {
        text.y -= frame_top;
    }
}

fn context_text_order(display: &DisplayList) -> Arc<[usize]> {
    let mut order = display
        .texts
        .iter()
        .enumerate()
        .filter_map(|(index, text)| text.x.is_finite().then_some(index))
        .collect::<Vec<_>>();
    order.sort_unstable_by(|left, right| {
        display.texts[*left]
            .x
            .total_cmp(&display.texts[*right].x)
            .then_with(|| display.texts[*left].y.total_cmp(&display.texts[*right].y))
            .then_with(|| left.cmp(right))
    });
    order.into()
}

pub(crate) fn project_context_lane_display(
    source: &DisplayList,
    text_order: &[usize],
    source_viewport: Viewport,
    target: Viewport,
    frame_top: f32,
    output: &mut DisplayList,
) {
    let source_bases_per_pixel = source_viewport.bases_per_pixel();
    let target_bases_per_pixel = target.bases_per_pixel();
    let target_width = target.pixel_width() as f32;
    if !source_bases_per_pixel.is_finite()
        || !target_bases_per_pixel.is_finite()
        || source_bases_per_pixel <= 0.0
        || target_bases_per_pixel <= 0.0
    {
        return;
    }
    let project_x = |x: f32| {
        let genomic = source_viewport.exact_start() + f64::from(x) * source_bases_per_pixel;
        ((genomic - target.exact_start()) / target_bases_per_pixel) as f32
    };
    output
        .rectangles
        .extend(source.rectangles.iter().filter_map(|source_rectangle| {
            let mut rectangle = *source_rectangle;
            let projected_start = project_x(rectangle.x);
            let projected_end = project_x(rectangle.x + rectangle.width);
            rectangle.x = projected_start.min(projected_end);
            rectangle.width = (projected_end - projected_start).abs();
            rectangle.y += frame_top;
            let right = rectangle.x + rectangle.width;
            if right <= 0.0 || rectangle.x >= target_width {
                return None;
            }
            let clipped_left = rectangle.x < 0.0;
            let clipped_right = right > target_width;
            if clipped_left {
                rectangle.width += rectangle.x;
                rectangle.x = 0.0;
            }
            if clipped_right {
                rectangle.width = target_width - rectangle.x;
            }
            if (matches!(
                rectangle.shape.direction(),
                Some(rgv_layout::Direction::Left)
            ) && clipped_left)
                || (matches!(
                    rectangle.shape.direction(),
                    Some(rgv_layout::Direction::Right)
                ) && clipped_right)
            {
                rectangle.shape = rgv_layout::RectShape::Rectangle;
            }
            (rectangle.width > 0.0).then_some(rectangle)
        }));
    project_context_lane_text(
        source,
        text_order,
        source_viewport,
        target,
        frame_top,
        output,
    );
}

fn project_context_lane_text(
    source: &DisplayList,
    text_order: &[usize],
    source_viewport: Viewport,
    target: Viewport,
    frame_top: f32,
    output: &mut DisplayList,
) -> usize {
    let source_bases_per_pixel = source_viewport.bases_per_pixel();
    let target_bases_per_pixel = target.bases_per_pixel();
    let target_width = target.pixel_width() as f32;
    if !source_bases_per_pixel.is_finite()
        || !target_bases_per_pixel.is_finite()
        || source_bases_per_pixel <= 0.0
        || target_bases_per_pixel <= 0.0
    {
        return 0;
    }
    let source_x_for_target = |target_x: f64| {
        (target.exact_start() + target_x * target_bases_per_pixel - source_viewport.exact_start())
            / source_bases_per_pixel
    };
    let minimum_source_x = source_x_for_target(-128.0) as f32;
    let maximum_source_x = source_x_for_target(f64::from(target_width) + 128.0) as f32;
    let first = text_order.partition_point(|index| source.texts[*index].x < minimum_source_x);
    let last = text_order.partition_point(|index| source.texts[*index].x <= maximum_source_x);
    let candidates = &text_order[first.min(last)..last];
    output.texts.extend(candidates.iter().filter_map(|index| {
        let text = &source.texts[*index];
        let genomic = source_viewport.exact_start() + f64::from(text.x) * source_bases_per_pixel;
        let mut text = text.clone();
        text.x = ((genomic - target.exact_start()) / target_bases_per_pixel) as f32;
        text.y += frame_top;
        (text.x >= -128.0 && text.x <= target_width + 128.0).then_some(text)
    }));
    candidates.len()
}

/// Uses the same user-facing side-buffer convention as alignment scenes: 100% means one visible
/// viewport on each side. The optional cap is used by bounded sources such as FASTA.
pub(crate) fn buffered_context_interval(
    viewport: Viewport,
    contig_length: u64,
    buffer_percent: u16,
    maximum_bases: Option<u64>,
) -> GenomicInterval {
    let visible = viewport.interval();
    let side = ((viewport.span() * f64::from(buffer_percent) / 100.0).ceil() as u64).max(1);
    let start = visible.start().saturating_sub(side);
    let end = visible.end().saturating_add(side).min(contig_length.max(1));
    let mut interval = GenomicInterval::new(
        visible.contig(),
        start.min(contig_length.saturating_sub(1)),
        end.max(start.saturating_add(1)).min(contig_length.max(1)),
    )
    .expect("a buffered viewport remains non-empty");
    if let Some(maximum) = maximum_bases.filter(|maximum| *maximum > 0)
        && interval.len() > maximum
        && maximum >= visible.len()
    {
        let spare = maximum - visible.len();
        let before = spare / 2;
        let after = spare - before;
        let mut bounded_start = visible.start().saturating_sub(before);
        let mut bounded_end = visible.end().saturating_add(after).min(contig_length);
        if bounded_end - bounded_start < maximum {
            bounded_start = bounded_end.saturating_sub(maximum);
            bounded_end = bounded_start.saturating_add(maximum).min(contig_length);
        }
        interval = GenomicInterval::new(visible.contig(), bounded_start, bounded_end)
            .expect("a capped buffered viewport remains non-empty");
    }
    interval
}

pub(crate) fn prepared_context_viewport(interval: GenomicInterval, target: Viewport) -> Viewport {
    let pixel_width = interval.len() as f64 / target.bases_per_pixel();
    Viewport::new(interval, pixel_width.max(1.0))
        .expect("prepared contextual viewport has finite positive scale")
}

pub(crate) fn context_lane_coherence_groups(
    tracks: &[TrackConfig],
    visible: &BTreeMap<ContextLaneKey, (TrackFrameKey, Option<GenomicInterval>)>,
) -> Vec<ContextLaneCoherenceGroup> {
    let visible_track_ids = visible
        .keys()
        .filter_map(|key| match key {
            ContextLaneKey::Reference => None,
            ContextLaneKey::Auxiliary(track_id) => Some(*track_id),
        })
        .collect::<BTreeSet<_>>();
    let mut groups = BTreeMap::<String, BTreeSet<ContextLaneKey>>::new();
    for track in tracks
        .iter()
        .filter(|track| visible_track_ids.contains(&track.id))
    {
        let lane = ContextLaneKey::Auxiliary(track.id);
        if track.kind == TrackKind::Quantitative
            && let Some(group) = track.quantitative.overlay_group
        {
            groups
                .entry(format!("overlay:{group}"))
                .or_default()
                .insert(lane);
        }
        if matches!(track.kind, TrackKind::Quantitative | TrackKind::CopyNumber)
            && track.quantitative.scale_mode == QuantitativeScaleMode::Shared
        {
            groups
                .entry(format!(
                    "shared-scale:{:?}:{:?}:{:?}",
                    track.kind, track.quantitative.scale_group, track.quantitative.scale_transform,
                ))
                .or_default()
                .insert(lane);
        }
        if track.kind == TrackKind::Quantitative
            && let Some(operand) = track.quantitative.operand_track
            && visible_track_ids.contains(&operand)
        {
            let members = groups
                .entry(format!("arithmetic:{}", track.id.get()))
                .or_default();
            members.insert(lane);
            members.insert(ContextLaneKey::Auxiliary(operand));
        }
    }
    groups
        .into_iter()
        .filter_map(|(id, members)| {
            (members.len() > 1).then_some(ContextLaneCoherenceGroup { id, members })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_layout::{RectPrimitive, Rgba};
    use rgv_render::{
        ContextSceneGpuCamera, ContextSceneGpuLaneRequest, ContextSceneGpuRenderer,
        ContextSceneGpuRetainedLaneRequest, ContextSceneGpuScissor,
    };

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("RGV contextual desktop transaction test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    fn viewport(start: u64, end: u64, pixels: f64) -> Viewport {
        Viewport::new(
            GenomicInterval::new(rgv_core::ContigId::new(0), start, end).unwrap(),
            pixels,
        )
        .unwrap()
    }

    #[test]
    fn buffered_interval_uses_one_side_viewport_at_one_hundred_percent() {
        let target = viewport(1_000, 2_000, 1_000.0);
        assert_eq!(
            buffered_context_interval(target, 10_000, 100, None),
            GenomicInterval::new(rgv_core::ContigId::new(0), 0, 3_000).unwrap()
        );
        assert_eq!(
            buffered_context_interval(target, 10_000, 400, Some(2_000)),
            GenomicInterval::new(rgv_core::ContigId::new(0), 500, 2_500).unwrap()
        );
    }

    #[test]
    fn overlapping_coherence_groups_form_one_transitive_gpu_unit() {
        let first = ContextLaneKey::Auxiliary(TrackId::new(3));
        let bridge = ContextLaneKey::Auxiliary(TrackId::new(8));
        let last = ContextLaneKey::Auxiliary(TrackId::new(13));
        let components = coherence_components(&BTreeMap::from([
            ("overlay".to_owned(), BTreeSet::from([first, bridge])),
            ("arithmetic".to_owned(), BTreeSet::from([bridge, last])),
        ]));
        assert_eq!(components[&first], components[&bridge]);
        assert_eq!(components[&bridge], components[&last]);
    }

    #[test]
    fn projected_lane_preserves_genomic_mapping_without_relayout() {
        let source_viewport = viewport(0, 3_000, 3_000.0);
        let target = viewport(1_000, 2_000, 1_000.0);
        let source = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 1_100.0,
                y: 5.0,
                width: 200.0,
                height: 8.0,
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                object_id: 7,
                shape: rgv_layout::RectShape::Rectangle,
            }],
            texts: vec![rgv_layout::TextPrimitive {
                x: 1_200.0,
                y: 9.0,
                text: "GENE".to_owned(),
                font_size: 10.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
            }],
        };
        let mut projected = DisplayList::default();
        let text_order = context_text_order(&source);
        project_context_lane_display(
            &source,
            &text_order,
            source_viewport,
            target,
            40.0,
            &mut projected,
        );
        assert_eq!(projected.rectangles[0].x, 100.0);
        assert_eq!(projected.rectangles[0].width, 200.0);
        assert_eq!(projected.rectangles[0].y, 45.0);
        assert_eq!(projected.texts[0].x, 200.0);
        assert_eq!(projected.texts[0].y, 49.0);
    }

    #[test]
    fn camera_text_projection_does_not_rebuild_native_rectangles() {
        let source_viewport = viewport(0, 3_000, 3_000.0);
        let target = viewport(1_000, 2_000, 1_000.0);
        let source = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 1_100.0,
                y: 5.0,
                width: 200.0,
                height: 8.0,
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                object_id: 7,
                shape: rgv_layout::RectShape::Rectangle,
            }],
            texts: vec![rgv_layout::TextPrimitive {
                x: 1_200.0,
                y: 9.0,
                text: "GENE".to_owned(),
                font_size: 10.0,
                color: Rgba::new(0.0, 0.0, 0.0, 1.0),
            }],
        };
        let mut projected = DisplayList::default();
        let text_order = context_text_order(&source);
        let examined = project_context_lane_text(
            &source,
            &text_order,
            source_viewport,
            target,
            50.0,
            &mut projected,
        );
        assert_eq!(examined, 1);
        assert!(projected.rectangles.is_empty());
        assert_eq!(projected.texts.len(), 1);
        assert_eq!(projected.texts[0].x, 200.0);
        assert_eq!(projected.texts[0].y, 59.0);
    }

    #[test]
    fn camera_text_projection_binary_searches_visible_anchors() {
        let source_viewport = viewport(0, 100_000, 100_000.0);
        let target = viewport(50_000, 50_100, 100.0);
        let source = DisplayList {
            rectangles: Vec::new(),
            texts: (0..10_000)
                .map(|index| rgv_layout::TextPrimitive {
                    x: (index * 10) as f32,
                    y: 5.0,
                    text: format!("G{index}"),
                    font_size: 10.0,
                    color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                })
                .collect(),
        };
        let text_order = context_text_order(&source);
        let mut projected = DisplayList::default();
        let examined = project_context_lane_text(
            &source,
            &text_order,
            source_viewport,
            target,
            0.0,
            &mut projected,
        );
        assert!(!projected.texts.is_empty());
        assert!(examined < source.texts.len() / 20);
        assert!(
            projected
                .texts
                .iter()
                .all(|text| text.x >= -128.0 && text.x <= 228.0)
        );
    }

    fn draft(key: ContextLaneKey, generation: u64, object_id: u32) -> ContextLaneDraft {
        ContextLaneDraft {
            key,
            generation,
            source_viewport: viewport(0, 3_000, 3_000.0),
            frame_key: match key {
                ContextLaneKey::Reference => TrackFrameKey::Reference,
                ContextLaneKey::Auxiliary(track_id) => TrackFrameKey::Auxiliary(track_id),
            },
            frame_top: 40.0,
            display: DisplayList {
                rectangles: vec![RectPrimitive {
                    x: 100.0,
                    y: 45.0,
                    width: 20.0,
                    height: 8.0,
                    color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                    object_id,
                    shape: rgv_layout::RectShape::Rectangle,
                }],
                texts: Vec::new(),
            },
        }
    }

    fn commit_candidates(scene: &mut PanelContextScene, keys: &[ContextLaneKey]) {
        for key in keys {
            assert!(scene.eligible_candidates.remove(key));
            let candidate = scene.candidates.remove(key).unwrap();
            let state = scene.lanes.get_mut(key).unwrap();
            state.gpu_readiness = ContextLaneGpuReadiness::Ready;
            state.gpu_revision = Some(candidate.revision);
            state.lease = Some(candidate);
        }
    }

    fn gpu_camera(lease: &ContextLaneLease) -> ContextSceneGpuCamera {
        ContextSceneGpuCamera {
            source_exact_start: lease.source_viewport.exact_start(),
            source_bases_per_pixel: lease.source_viewport.bases_per_pixel(),
            target_exact_start: 1_000.0,
            target_bases_per_pixel: 1.0,
            panel_left: 0.0,
            frame_top: 40.0,
            vertical_scroll: 0.0,
            scissor: ContextSceneGpuScissor {
                x: 0,
                y: 0,
                width: 1_000,
                height: 100,
            },
            selected_object_id: None,
            hovered_object_id: None,
        }
    }

    #[test]
    fn coherence_group_retains_its_prior_epoch_until_every_member_is_ready() {
        let left = ContextLaneKey::Auxiliary(TrackId::new(1));
        let right = ContextLaneKey::Auxiliary(TrackId::new(2));
        let independent = ContextLaneKey::Auxiliary(TrackId::new(3));
        let source = viewport(0, 3_000, 3_000.0);
        let target = viewport(1_000, 2_000, 1_000.0);
        let visible = [left, right, independent]
            .into_iter()
            .map(|key| {
                let ContextLaneKey::Auxiliary(track_id) = key else {
                    unreachable!()
                };
                (
                    key,
                    (TrackFrameKey::Auxiliary(track_id), Some(source.interval())),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let group = ContextLaneCoherenceGroup {
            id: "overlay:1".to_owned(),
            members: BTreeSet::from([left, right]),
        };
        let mut scene = PanelContextScene::default();
        scene.reconcile(
            1,
            target,
            &visible,
            vec![
                draft(left, 1, 1),
                draft(right, 1, 2),
                draft(independent, 1, 3),
            ],
            &BTreeSet::new(),
            &BTreeSet::new(),
            std::slice::from_ref(&group),
        );
        commit_candidates(&mut scene, &[left, right, independent]);
        let initial_left = scene.lanes[&left].lease.as_ref().unwrap().revision;
        let initial_right = scene.lanes[&right].lease.as_ref().unwrap().revision;

        scene.reconcile(
            2,
            target,
            &visible,
            vec![draft(left, 2, 11), draft(independent, 2, 13)],
            &BTreeSet::new(),
            &BTreeSet::new(),
            std::slice::from_ref(&group),
        );
        assert_eq!(
            scene.lanes[&left].lease.as_ref().unwrap().revision,
            initial_left
        );
        assert_eq!(
            scene.lanes[&right].lease.as_ref().unwrap().revision,
            initial_right
        );
        assert_eq!(scene.candidates[&independent].generation, 2);
        assert!(scene.eligible_candidates.contains(&independent));
        assert!(!scene.eligible_candidates.contains(&left));
        commit_candidates(&mut scene, &[independent]);

        scene.reconcile(
            2,
            target,
            &visible,
            vec![draft(right, 2, 12)],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[group],
        );
        assert!(scene.eligible_candidates.contains(&left));
        assert!(scene.eligible_candidates.contains(&right));
        assert_eq!(scene.lanes[&left].lease.as_ref().unwrap().generation, 1);
        assert_eq!(scene.lanes[&right].lease.as_ref().unwrap().generation, 1);
        commit_candidates(&mut scene, &[left, right]);
        assert_eq!(scene.lanes[&left].lease.as_ref().unwrap().generation, 2);
        assert_eq!(scene.lanes[&right].lease.as_ref().unwrap().generation, 2);
        assert_ne!(
            scene.lanes[&left].lease.as_ref().unwrap().revision,
            initial_left
        );
        assert_ne!(
            scene.lanes[&right].lease.as_ref().unwrap().revision,
            initial_right
        );
    }

    #[test]
    fn gpu_rejection_retains_the_committed_lane_until_the_candidate_is_admitted() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let panel_id = PanelId::new(1);
        let key = ContextLaneKey::Auxiliary(TrackId::new(1));
        let source = viewport(0, 3_000, 3_000.0);
        let target = viewport(1_000, 2_000, 1_000.0);
        let visible = BTreeMap::from([(
            key,
            (
                TrackFrameKey::Auxiliary(TrackId::new(1)),
                Some(source.interval()),
            ),
        )]);
        let mut scene = PanelContextScene::default();
        let mut renderer =
            ContextSceneGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        scene.reconcile(
            1,
            target,
            &visible,
            vec![draft(key, 1, 1)],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
        );
        let first = {
            let publication = scene.gpu_publications().next().unwrap();
            renderer.prepare(
                &device,
                &queue,
                [1_000.0, 500.0],
                &[ContextSceneGpuLaneRequest {
                    key: ContextSceneGpuLaneKey::new(panel_id.get(), key.numeric_id()),
                    content_revision: publication.desired.revision,
                    camera: gpu_camera(publication.desired),
                    rectangles: &publication.desired.display.rectangles,
                    retained: None,
                    coherence_group: None,
                }],
                usize::MAX,
            )
        };
        let original_budget = first.stats.resident_bytes;
        assert!(scene.apply_gpu_publication(panel_id, target, &first.frame, &first.rejections));
        let original_revision = scene.lanes[&key].lease.as_ref().unwrap().revision;

        let mut replacement = draft(key, 2, 2);
        replacement
            .display
            .rectangles
            .push(replacement.display.rectangles[0]);
        scene.reconcile(
            2,
            target,
            &visible,
            vec![replacement],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
        );
        let rejected = {
            let publication = scene.gpu_publications().next().unwrap();
            let retained = publication.retained.unwrap();
            renderer.prepare(
                &device,
                &queue,
                [1_000.0, 500.0],
                &[ContextSceneGpuLaneRequest {
                    key: ContextSceneGpuLaneKey::new(panel_id.get(), key.numeric_id()),
                    content_revision: publication.desired.revision,
                    camera: gpu_camera(publication.desired),
                    rectangles: &publication.desired.display.rectangles,
                    retained: Some(ContextSceneGpuRetainedLaneRequest {
                        content_revision: retained.revision,
                        camera: gpu_camera(retained),
                        rectangles: &retained.display.rectangles,
                    }),
                    coherence_group: None,
                }],
                original_budget,
            )
        };
        assert!(scene.apply_gpu_publication(
            panel_id,
            target,
            &rejected.frame,
            &rejected.rejections,
        ));
        assert_eq!(
            scene.lanes[&key].lease.as_ref().unwrap().revision,
            original_revision
        );
        assert_eq!(
            scene.lanes[&key].gpu_readiness,
            ContextLaneGpuReadiness::Ready
        );
        assert!(
            scene.lanes[&key]
                .gpu_failure
                .as_deref()
                .is_some_and(|failure| failure.contains("retained the committed"))
        );

        let admitted = {
            let publication = scene.gpu_publications().next().unwrap();
            let retained = publication.retained.unwrap();
            renderer.prepare(
                &device,
                &queue,
                [1_000.0, 500.0],
                &[ContextSceneGpuLaneRequest {
                    key: ContextSceneGpuLaneKey::new(panel_id.get(), key.numeric_id()),
                    content_revision: publication.desired.revision,
                    camera: gpu_camera(publication.desired),
                    rectangles: &publication.desired.display.rectangles,
                    retained: Some(ContextSceneGpuRetainedLaneRequest {
                        content_revision: retained.revision,
                        camera: gpu_camera(retained),
                        rectangles: &retained.display.rectangles,
                    }),
                    coherence_group: None,
                }],
                usize::MAX,
            )
        };
        assert!(scene.apply_gpu_publication(
            panel_id,
            target,
            &admitted.frame,
            &admitted.rejections,
        ));
        assert_eq!(scene.lanes[&key].lease.as_ref().unwrap().generation, 2);
        assert_ne!(
            scene.lanes[&key].lease.as_ref().unwrap().revision,
            original_revision
        );
        assert!(scene.lanes[&key].gpu_failure.is_none());
    }
}
