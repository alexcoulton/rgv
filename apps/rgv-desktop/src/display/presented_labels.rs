//! Read-level text taken from the presented alignment GPU bundle.
//!
//! Mismatch base letters, event-length labels, clip ghost letters and junction counts are CPU
//! text projected into the label lists of one camera capture, while the blocks they belong to are
//! GPU instances drawn through that same capture. The two stay aligned only if both come from the
//! same captured frame. Panel display lists are rebuilt at times unrelated to captures (before
//! the egui pass, after it, after a camera-only capture or not), so they never carry this text.
//! Instead it is assembled from the bundle the GPU pass is about to draw, at paint time; see
//! `Viewer::submit_ui_frame`. Exports read it the same way.
//!
//! The read-hover highlight is laid out here too, from the same bundle and cameras (it is never
//! exported), and hovering a label (an insertion chip, an event length) is hit-tested against the
//! labels of the presented frame.

use crate::{AlignmentGpuFrameBundle, Viewer, alignment_gpu_bundle_is_current};
use rgv_alignment_scene::{AlignmentComponent, CpuProjectedPrimitive, is_insertion_label};
use rgv_core::PanelId;
use rgv_layout::{RectPrimitive, RectShape, Rgba, TextPrimitive};
use rgv_render::AlignmentSummaryGpuJunctionLabel;

/// Styles one projected read label (base letter, event length, clip ghost letter) as text
/// centred on its cell.
pub(crate) fn alignment_text_primitive(label: &CpuProjectedPrimitive) -> Option<TextPrimitive> {
    let text = label.text.as_deref()?;
    let color = match label.component {
        AlignmentComponent::Base { base, .. } => match base.to_ascii_uppercase() {
            // Dark ink has the stronger contrast on the bright A/G fills; white remains clearer
            // on the darker C/T fills. N and other ambiguous symbols use white on neutral grey.
            b'A' | b'G' => Rgba::new(0.06, 0.07, 0.08, 0.96),
            _ => Rgba::new(1.0, 1.0, 1.0, 0.98),
        },
        // An insertion's label sits on its purple chip (see `alignment_label_backdrop`); white
        // reads on the light theme's deep purple, and the dark theme maps it to near-black on
        // its lighter purple.
        component if is_insertion_label(component) => Rgba::new(1.0, 1.0, 1.0, 1.0),
        // Soft-clipped ghost bases sit on a faint tint of their read (or on the canvas): a
        // muted mid-grey stays legible on either theme and reads as "not aligned".
        AlignmentComponent::ClipGhostBase { .. } => Rgba::new(0.42, 0.43, 0.47, 0.95),
        _ => Rgba::new(0.08, 0.09, 0.11, 0.94),
    };
    let font_size = if matches!(
        label.component,
        AlignmentComponent::Base { .. } | AlignmentComponent::ClipGhostBase { .. }
    ) {
        (label.width.min(label.height) as f32 - 1.0).clamp(8.0, 12.0)
    } else {
        (label.height as f32 - 2.0).clamp(8.0, 12.0)
    };
    Some(TextPrimitive {
        x: (label.x + label.width * 0.5) as f32,
        y: (label.y + label.height * 0.5) as f32,
        text: text.to_owned(),
        font_size,
        color,
    })
}

/// The chip an insertion's label is written on: the label's box (the read's height, just beside
/// the I-beam; see `place_insertion_label` in `rgv-alignment-scene`) filled with `fill`, the
/// palette's insertion colour. `None` for every other label.
pub(crate) fn alignment_label_backdrop(
    label: &CpuProjectedPrimitive,
    fill: Rgba,
) -> Option<RectPrimitive> {
    if !is_insertion_label(label.component) || label.text.is_none() {
        return None;
    }
    Some(RectPrimitive {
        x: label.x as f32,
        y: label.y as f32,
        width: label.width as f32,
        height: label.height as f32,
        color: fill,
        object_id: 0,
        shape: RectShape::Rectangle,
    })
}

/// The label chips of every panel accepted by the caller, from the same label frames as
/// [`presented_label_texts`], in the same panel order.
pub(crate) fn presented_label_backdrops(
    read_frames: &[(PanelId, &[CpuProjectedPrimitive])],
    panels: impl IntoIterator<Item = PanelId>,
    fill: Rgba,
) -> Vec<RectPrimitive> {
    let mut backdrops = Vec::new();
    for panel_id in panels {
        backdrops.extend(
            read_frames
                .iter()
                .filter(|(owner, _)| *owner == panel_id)
                .flat_map(|(_, labels)| labels.iter())
                .filter_map(|label| alignment_label_backdrop(label, fill)),
        );
    }
    backdrops
}

/// The chip behind one junction label: its collision box, filled so the count stays legible
/// over the arcs it overlaps.
pub(crate) fn junction_label_backdrop(
    label: &AlignmentSummaryGpuJunctionLabel,
    fill: Rgba,
) -> RectPrimitive {
    let [left, top, right, bottom] = label.chip;
    RectPrimitive {
        x: left as f32,
        y: top as f32,
        width: (right - left).max(0.0) as f32,
        height: (bottom - top).max(0.0) as f32,
        color: fill,
        object_id: 0,
        shape: RectShape::Rectangle,
    }
}

/// The junction-label chips of every panel accepted by the caller, in panel order.
pub(crate) fn presented_junction_label_backdrops(
    labels: &[AlignmentSummaryGpuJunctionLabel],
    panels: impl IntoIterator<Item = PanelId>,
    fill: Rgba,
) -> Vec<RectPrimitive> {
    let mut backdrops = Vec::new();
    for panel_id in panels {
        backdrops.extend(
            labels
                .iter()
                .filter(|label| label.coherence_scope == panel_id.get())
                .map(|label| junction_label_backdrop(label, fill)),
        );
    }
    backdrops
}

/// Styles one junction-count label of the summary frame.
fn junction_text_primitive(label: &AlignmentSummaryGpuJunctionLabel) -> TextPrimitive {
    TextPrimitive {
        x: label.x as f32,
        y: label.y as f32,
        text: label.text.clone(),
        font_size: 10.0,
        color: Rgba::new(0.10, 0.10, 0.12, 0.94),
    }
}

/// The read text of one panel: every label of every label frame whose read authority belongs to
/// `panel_id`, in request order. `frames` pairs each label frame of one captured
/// `AlignmentGpuFrame` with the panel of its read authority.
pub(crate) fn panel_read_label_texts<'a>(
    frames: impl IntoIterator<Item = (PanelId, &'a [CpuProjectedPrimitive])>,
    panel_id: PanelId,
) -> impl Iterator<Item = TextPrimitive> {
    frames
        .into_iter()
        .filter(move |(owner, _)| *owner == panel_id)
        .flat_map(|(_, labels)| labels.iter())
        .filter_map(alignment_text_primitive)
}

/// The junction-count text of one panel, selected by the summary draw's coherence scope.
pub(crate) fn panel_junction_label_texts(
    labels: &[AlignmentSummaryGpuJunctionLabel],
    panel_id: PanelId,
) -> impl Iterator<Item = TextPrimitive> + '_ {
    labels
        .iter()
        .filter(move |label| label.coherence_scope == panel_id.get())
        .map(junction_text_primitive)
}

/// Assembles, for each panel in order, its junction labels then its read labels, from one
/// presented bundle. This is the only producer of read-level text: painting and exports both
/// call it, so neither can show text from a different capture than the one the GPU draws.
pub(crate) fn presented_label_texts(
    read_frames: &[(PanelId, &[CpuProjectedPrimitive])],
    junction_labels: &[AlignmentSummaryGpuJunctionLabel],
    panels: impl IntoIterator<Item = PanelId>,
) -> Vec<TextPrimitive> {
    let mut texts = Vec::new();
    for panel_id in panels {
        texts.extend(panel_junction_label_texts(junction_labels, panel_id));
        texts.extend(panel_read_label_texts(
            read_frames.iter().copied(),
            panel_id,
        ));
    }
    texts
}

impl Viewer {
    /// The bundle the alignment GPU pass draws this frame, under exactly the condition
    /// `render_composed_frame_target` uses to draw it.
    pub(crate) fn drawable_alignment_gpu_bundle(&self) -> Option<&AlignmentGpuFrameBundle> {
        self.alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .filter(|bundle| {
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            })
    }

    /// Each label frame of the presented read frame, with the panel of its read authority.
    fn presented_read_label_frames(
        bundle: &AlignmentGpuFrameBundle,
    ) -> Vec<(PanelId, &[CpuProjectedPrimitive])> {
        bundle
            .reads
            .as_ref()
            .map(|frame| {
                bundle
                    .read_authorities
                    .iter()
                    .zip(frame.label_frames())
                    .map(|(authority, labels)| (authority.panel_id, &*labels.labels))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// The panels, in order, accepted by `include`.
    fn included_panels(&self, include: impl Fn(PanelId) -> bool) -> impl Iterator<Item = PanelId> {
        self.session
            .panels
            .iter()
            .map(|panel| panel.id)
            .filter(move |panel_id| include(*panel_id))
    }

    /// Read-level text of the panels accepted by `include`, taken from the bundle the GPU pass
    /// draws right now. Call it after the last capture of a frame; see `submit_ui_frame`.
    pub(crate) fn presented_alignment_label_texts(
        &self,
        include: impl Fn(PanelId) -> bool,
    ) -> Vec<TextPrimitive> {
        let Some(bundle) = self.drawable_alignment_gpu_bundle() else {
            return Vec::new();
        };
        let junction_labels = bundle
            .summaries
            .as_ref()
            .map(|frame| self.alignment_summary_gpu_renderer.junction_labels(frame))
            .unwrap_or_default();
        presented_label_texts(
            &Self::presented_read_label_frames(bundle),
            &junction_labels,
            self.included_panels(include),
        )
    }

    /// The insertion label chips beneath [`Self::presented_alignment_label_texts`], from the same
    /// bundle, in the active theme's insertion colour (already a theme colour: not adapted).
    pub(crate) fn presented_alignment_label_backdrops(
        &self,
        include: impl Fn(PanelId) -> bool,
    ) -> Vec<RectPrimitive> {
        let Some(bundle) = self.drawable_alignment_gpu_bundle() else {
            return Vec::new();
        };
        let [red, green, blue, alpha] = crate::theme::active()
            .palette()
            .color(rgv_render::palette::PaletteToken::INSERTION);
        let junction_labels = bundle
            .summaries
            .as_ref()
            .map(|frame| self.alignment_summary_gpu_renderer.junction_labels(frame))
            .unwrap_or_default();
        // A canvas-coloured chip, slightly translucent, keeps a count legible over other arcs.
        let [chip_red, chip_green, chip_blue, chip_alpha] =
            crate::theme::adapt(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 225))
                .to_normalized_gamma_f32();
        let mut backdrops = presented_junction_label_backdrops(
            &junction_labels,
            self.included_panels(&include),
            Rgba::new(chip_red, chip_green, chip_blue, chip_alpha),
        );
        backdrops.extend(presented_label_backdrops(
            &Self::presented_read_label_frames(bundle),
            self.included_panels(include),
            Rgba::new(red, green, blue, alpha),
        ));
        backdrops
    }

    /// The presented label frame (and its read authority's clip) of `track_id` in `panel_id`,
    /// when it shows scene `scene_revision`.
    fn presented_read_label_frame(
        bundle: &AlignmentGpuFrameBundle,
        panel_id: PanelId,
        track_id: rgv_core::TrackId,
        scene_revision: rgv_alignment_scene::SceneRevision,
    ) -> Option<(
        &rgv_alignment_scene::CpuAlignmentLabelFrame,
        rgv_alignment_scene::CpuClipRect,
    )> {
        let frame = bundle.reads.as_ref()?;
        bundle
            .read_authorities
            .iter()
            .zip(frame.label_frames())
            .find(|(authority, labels)| {
                authority.panel_id == panel_id
                    && authority.track_id == track_id
                    && authority.scene_revision == scene_revision
                    && labels.scene_revision == scene_revision
            })
            .map(|(authority, labels)| (labels, authority.clip))
    }

    /// The label of the presented frame drawn at `(x, y)` in `panel_id` that hovering explains
    /// (insertion chips, event lengths, read names and tags; base letters are their cells), as a
    /// hover target with its text.
    pub(crate) fn presented_label_at(
        &self,
        panel_id: PanelId,
        x: f64,
        y: f64,
    ) -> Option<(
        rgv_alignment_scene::AlignmentHoverTarget,
        std::sync::Arc<str>,
    )> {
        let bundle = self.drawable_alignment_gpu_bundle()?;
        let frame = bundle.reads.as_ref()?;
        bundle
            .read_authorities
            .iter()
            .zip(frame.label_frames())
            .filter(|(authority, labels)| {
                authority.panel_id == panel_id
                    && labels.scene_revision == authority.scene_revision
                    && authority.clip.contains(x, y)
            })
            .find_map(|(authority, labels)| {
                let label = labels.labels.iter().rev().find(|label| {
                    matches!(label.component, AlignmentComponent::Label { .. })
                        && x >= label.x
                        && x < label.x + label.width
                        && y >= label.y
                        && y < label.y + label.height
                })?;
                Some((
                    rgv_alignment_scene::AlignmentHoverTarget {
                        hit: rgv_alignment_scene::AlignmentHit {
                            track: authority.track_id,
                            scene_revision: authority.scene_revision,
                            handle: label.handle,
                            component: label.component,
                        },
                        primitive: rgv_alignment_scene::AlignmentSemanticPrimitive {
                            start_offset: 0,
                            end_offset: 0,
                            row_offset: label.row,
                            handle: label.handle,
                            component: label.component,
                            geometry: label.geometry,
                            style: label.style,
                            draw_order: label.draw_order,
                        },
                    },
                    std::sync::Arc::clone(label.text.as_ref()?),
                ))
            })
    }

    /// The text of the hovered label `target` in the presented frame of `panel_id`.
    pub(crate) fn presented_label_text(
        &self,
        panel_id: PanelId,
        target: &rgv_alignment_scene::AlignmentHoverTarget,
    ) -> Option<std::sync::Arc<str>> {
        let bundle = self.drawable_alignment_gpu_bundle()?;
        let (labels, _) = Self::presented_read_label_frame(
            bundle,
            panel_id,
            target.hit.track,
            target.hit.scene_revision,
        )?;
        labels
            .labels
            .iter()
            .find(|label| {
                label.handle == target.hit.handle && label.component == target.hit.component
            })
            .and_then(|label| label.text.clone())
    }

    /// The highlight of every hovered target at `now`, laid out through the camera of the label
    /// frame the GPU pass draws with it, so it moves with the reads exactly as their text does.
    /// Empty during a gesture (the hover is suspended then anyway).
    pub(crate) fn presented_read_hover_highlights(
        &self,
        now: std::time::Instant,
        genomic: egui::Rect,
    ) -> Vec<egui::epaint::ClippedShape> {
        let Some(bundle) = self.drawable_alignment_gpu_bundle() else {
            return Vec::new();
        };
        if self.dragging {
            return Vec::new();
        }
        let reduced_motion = self.session.settings.reduced_motion;
        // The theme's hover blue, the colour the GPU pass gives a hovered read.
        let color = crate::ui::read_hover::palette_color32(
            crate::theme::active()
                .palette()
                .color(rgv_render::palette::PaletteToken::INTERACTION_HOVERED),
        );
        let mut shapes = Vec::new();
        for (hover, alpha) in self.read_hover.highlights(now, reduced_motion) {
            if !self
                .session
                .panels
                .get(hover.panel)
                .is_some_and(|panel| panel.navigation_presentation.is_interactive())
            {
                continue;
            }
            let target = hover.target;
            let Some((labels, clip)) = Self::presented_read_label_frame(
                bundle,
                hover.panel,
                target.hit.track,
                target.hit.scene_revision,
            ) else {
                continue;
            };
            let camera = labels.camera;
            let projected = if matches!(target.hit.component, AlignmentComponent::Label { .. }) {
                labels
                    .labels
                    .iter()
                    .find(|label| {
                        label.handle == target.hit.handle && label.component == target.hit.component
                    })
                    .cloned()
            } else {
                target.project(camera)
            };
            let Some(projected) = projected else {
                continue;
            };
            let clip = egui::Rect::from_min_size(
                egui::pos2(clip.x as f32, clip.y as f32),
                egui::vec2(clip.width as f32, clip.height as f32),
            )
            .intersect(genomic);
            shapes.extend(crate::ui::read_hover::highlight_shapes(
                |gap| rgv_alignment_scene::hover_outline(&projected, camera, gap),
                alpha,
                clip,
                color,
            ));
        }
        shapes
    }

    /// Lays out every panel's read-level text from the bundle the GPU pass draws this frame and
    /// splices it into `paint_jobs` at the slot the workspace canvas recorded, uploading any new
    /// glyphs first. Must run after the frame's last alignment capture and before
    /// `render_composed_frame_target`.
    pub(crate) fn paint_presented_alignment_text(
        &mut self,
        paint_jobs: &mut Vec<egui::ClippedPrimitive>,
        pixels_per_point: f32,
    ) {
        let texts = self.presented_alignment_label_texts(|_| true);
        let backdrops = self.presented_alignment_label_backdrops(|_| true);
        let genomic = self.workspace_layout.canvas_geometry().genomic;
        let highlights = self.presented_read_hover_highlights(std::time::Instant::now(), genomic);
        let context = self.egui_context.clone();
        let (options, tessellation) = (
            context.fonts(|fonts| *fonts.options()),
            context.tessellation_options(|options| *options),
        );
        let frame = self.presented_text_painter.paint(
            &backdrops,
            &texts,
            highlights,
            genomic,
            pixels_per_point,
            crate::ui::presented_text::PresentedTextFonts {
                options,
                definitions: || context.fonts(|fonts| fonts.definitions().clone()),
                tessellation,
            },
            crate::ui::genomic_text_color,
        );
        if let Some(delta) = &frame.texture_delta {
            self.egui_renderer.update_texture(
                &self.device,
                &self.queue,
                crate::ui::presented_text::PRESENTED_TEXT_TEXTURE,
                delta,
            );
        }
        crate::ui::presented_text::splice_presented_text(paint_jobs, frame.primitives);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_alignment_scene::{
        AlignmentBodyColorToken, AlignmentGeometryToken, AlignmentHandle,
        AlignmentInteractionToken, AlignmentSourceRevision, AlignmentStyleToken, CpuPrimitiveShape,
        RevisionFingerprint, SourceFingerprint,
    };
    use std::sync::Arc;

    fn handle() -> AlignmentHandle {
        AlignmentHandle::new(
            AlignmentSourceRevision::new(
                SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                RevisionFingerprint::new(3, 4),
            ),
            rgv_hts::AlignmentSourcePosition::bam_virtual_offset(42),
        )
    }

    fn base_label(x: f64, y: f64, base: u8) -> CpuProjectedPrimitive {
        CpuProjectedPrimitive {
            x,
            y,
            width: 10.0,
            height: 12.0,
            shape: CpuPrimitiveShape::TextLabel,
            geometry: AlignmentGeometryToken::Rectangle,
            handle: handle(),
            component: AlignmentComponent::Base {
                cigar_index: 0,
                read_index: 3,
                base,
                quality: None,
                classification: rgv_model::AlignedBaseClassification::Substitution,
            },
            style: AlignmentStyleToken {
                flags: 0,
                mapping_quality: None,
                operation: None,
                body: AlignmentBodyColorToken::Neutral,
            },
            interaction: AlignmentInteractionToken::default(),
            draw_order: 0,
            text: Some(Arc::from(char::from(base).to_string())),
            row: 0,
            arrow: None,
            insertion_mark: None,
        }
    }

    fn junction(scope: u64, text: &str) -> AlignmentSummaryGpuJunctionLabel {
        AlignmentSummaryGpuJunctionLabel {
            coherence_scope: scope,
            lane_identity: 9,
            evidence: rgv_model::JunctionEvidence::default(),
            x: 50.0,
            y: 60.0,
            text: text.to_owned(),
            chip: [40.0, 54.0, 60.0, 66.0],
        }
    }

    #[test]
    fn junction_labels_sit_on_a_chip_of_their_collision_box() {
        let (a, b) = (PanelId::new(1), PanelId::new(2));
        let labels = [junction(a.get(), "12"), junction(b.get(), "99")];
        let fill = Rgba::new(1.0, 1.0, 1.0, 0.9);
        let chips = presented_junction_label_backdrops(&labels, [a], fill);
        assert_eq!(chips.len(), 1);
        assert_eq!(
            (chips[0].x, chips[0].y, chips[0].width, chips[0].height),
            (40.0, 54.0, 20.0, 12.0)
        );
        assert_eq!(chips[0].color, fill);
    }

    #[test]
    fn a_panel_receives_exactly_the_label_frames_of_its_own_read_authorities() {
        let (a, b) = (PanelId::new(1), PanelId::new(2));
        let first = [base_label(0.0, 0.0, b'A'), base_label(10.0, 0.0, b'C')];
        let second = [base_label(100.0, 40.0, b'G')];
        let third = [base_label(200.0, 80.0, b'T')];
        let frames = [(a, &first[..]), (b, &second[..]), (a, &third[..])];

        let texts = panel_read_label_texts(frames.iter().copied(), a).collect::<Vec<_>>();
        let expected = first
            .iter()
            .chain(&third)
            .filter_map(alignment_text_primitive)
            .collect::<Vec<_>>();

        assert_eq!(texts, expected);
        assert_eq!(
            texts
                .iter()
                .map(|text| text.text.as_str())
                .collect::<Vec<_>>(),
            ["A", "C", "T"]
        );
        // Centred on the projected cell of the very frame that was passed in.
        assert_eq!((texts[0].x, texts[0].y), (5.0, 6.0));
        assert_eq!(
            panel_read_label_texts(frames.iter().copied(), b)
                .map(|text| text.text)
                .collect::<Vec<_>>(),
            ["G"]
        );
        assert_eq!(
            panel_read_label_texts(frames.iter().copied(), PanelId::new(3)).count(),
            0
        );
    }

    #[test]
    fn labels_follow_whichever_frame_is_presented() {
        // Two successive captures of the same reads under different cameras: the assembled text
        // is a pure function of the frame handed in, so a capture made after any display rebuild
        // is what gets painted.
        let panel = PanelId::new(1);
        let before = [base_label(0.0, 0.0, b'A')];
        let after = [base_label(0.0, 37.0, b'A')];
        let paint = |labels: &[CpuProjectedPrimitive]| {
            presented_label_texts(&[(panel, labels)], &[], [panel])
        };
        assert!((paint(&before)[0].y - 6.0).abs() < f32::EPSILON);
        assert!((paint(&after)[0].y - 43.0).abs() < f32::EPSILON);
    }

    #[test]
    fn junction_labels_are_scoped_to_their_panel_and_precede_read_text() {
        let (a, b) = (PanelId::new(1), PanelId::new(2));
        let reads = [base_label(0.0, 0.0, b'G')];
        let junctions = [junction(a.get(), "12"), junction(b.get(), "99")];

        let texts = presented_label_texts(&[(a, &reads[..])], &junctions, [a, b]);

        assert_eq!(
            texts
                .iter()
                .map(|text| text.text.as_str())
                .collect::<Vec<_>>(),
            ["12", "G", "99"]
        );
        assert!((texts[0].font_size - 10.0).abs() < f32::EPSILON);
        assert!(
            presented_label_texts(&[(a, &reads[..])], &junctions, [b])
                .iter()
                .all(|text| text.text == "99")
        );
    }

    /// An insertion's label is drawn as white text centred on a chip of the insertion colour,
    /// both laid out from the very label frame handed in, so they move with the geometry of the
    /// capture the GPU draws.
    #[test]
    fn insertion_labels_are_white_text_on_a_chip_from_the_presented_frame() {
        let panel = PanelId::new(1);
        let mut chip = base_label(105.0, 20.0, b'A');
        chip.width = 16.32;
        chip.height = 11.0;
        chip.component = AlignmentComponent::Label {
            kind: rgv_alignment_scene::AlignmentLabelKind::ShortInsertion,
            id: rgv_alignment_scene::ContractFingerprint::new(9),
        };
        chip.text = Some(Arc::from("+T"));
        let fill = Rgba::new(0.55, 0.20, 0.70, 1.0);
        let letter = base_label(200.0, 20.0, b'C');
        for dy in [0.0, 37.0] {
            let mut moved = chip.clone();
            moved.y += dy;
            let labels = [moved, letter.clone()];
            let texts = presented_label_texts(&[(panel, &labels[..])], &[], [panel]);
            assert_eq!(texts[0].text, "+T");
            assert_eq!(texts[0].color, Rgba::new(1.0, 1.0, 1.0, 1.0));
            assert!((texts[0].x - (105.0 + 16.32 * 0.5) as f32).abs() < 1.0e-4);
            assert!((texts[0].y - (20.0 + dy as f32 + 5.5)).abs() < 1.0e-4);
            let backdrops = presented_label_backdrops(&[(panel, &labels[..])], [panel], fill);
            // Only the insertion label has a chip, exactly its box.
            assert_eq!(backdrops.len(), 1);
            let backdrop = backdrops[0];
            for (actual, expected) in [
                (backdrop.x, 105.0),
                (backdrop.y, 20.0 + dy as f32),
                (backdrop.width, 16.32),
                (backdrop.height, 11.0),
            ] {
                assert!((actual - expected).abs() < 1.0e-4, "{backdrop:?}");
            }
            assert_eq!(backdrop.color, fill);
            assert!(
                presented_label_backdrops(&[(panel, &labels[..])], [PanelId::new(2)], fill)
                    .is_empty()
            );
        }
    }

    #[test]
    fn unlabelled_primitives_produce_no_text() {
        let mut silent = base_label(0.0, 0.0, b'A');
        silent.text = None;
        assert!(alignment_text_primitive(&silent).is_none());
    }

    fn rust_sources(root: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).expect("desktop source directory is readable") {
            let path = entry.expect("desktop source entry is readable").path();
            if path.is_dir() {
                rust_sources(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push(path);
            }
        }
    }

    /// A display list rebuilt before a capture would carry the previous capture's text, which is
    /// how read letters used to slide against their blocks. No display-list producer may read a
    /// presented frame's labels: this module, which runs at paint and export time, is the only
    /// one (diagnostics only count them).
    #[test]
    fn only_paint_time_assembly_reads_presented_label_frames() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        rust_sources(&root, &mut sources);
        let readers = sources
            .iter()
            .filter(|path| {
                let source = std::fs::read_to_string(path).expect("desktop source is readable");
                [
                    "label_frames()",
                    ".junction_labels(",
                    "alignment_text_primitive",
                ]
                .iter()
                .any(|needle| source.contains(needle))
            })
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("source lies under the source root")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            readers.into_iter().collect::<Vec<_>>(),
            [
                "diagnostics/manual_diagnostic_instrumentation.rs",
                "display/presented_labels.rs",
            ]
        );
    }
}
