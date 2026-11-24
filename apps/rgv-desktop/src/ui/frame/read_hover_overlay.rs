//! The read-stack hover tooltip, painted in the egui pass over the canvas (it follows the
//! pointer, not the reads), and the facts it states. The highlight is painted from the
//! presented frame at submit time; see `Viewer::presented_read_hover_highlights`.

use crate::Viewer;
use crate::inspection::read_hover_facts::scene_locus_facts;
use crate::ui::read_hover::{
    JunctionFacts, LocusFacts, PairFacts, ReadHover, TooltipCard, TooltipContext, TooltipLine,
    tooltip_copy_text, tooltip_lines,
};
use rgv_alignment_scene::{AlignmentComponent, AlignmentSceneSnapshot};
use rgv_hts::AlignmentRecord;
use std::time::Instant;

impl Viewer {
    /// Moves the read hover to `next` (found under the pointer at `position`) and asks for the
    /// frame that starts its fade or tooltip delay; the egui pass schedules the rest.
    #[allow(clippy::large_types_passed_by_value)]
    pub(crate) fn set_read_hover(&mut self, next: Option<ReadHover>, position: Option<u64>) {
        let reduced_motion = self.session.settings.reduced_motion;
        let had_tooltip = self.read_hover.tooltip(Instant::now()).is_some();
        if self
            .read_hover
            .set(Instant::now(), next, position, reduced_motion)
            || (had_tooltip && position.is_some())
        {
            self.window.request_redraw();
        }
    }

    /// Whether a showing tooltip should be held because the pointer still rests over the
    /// window although the window stopped receiving it (covered, or another window focused).
    pub(crate) fn read_hover_is_covered(&self) -> bool {
        self.read_hover.tooltip(Instant::now()).is_some()
            && crate::ui::read_hover::system_pointer_over(&self.window)
    }

    /// A gesture began: the hover highlight, its read's tint and the tooltip disappear at once
    /// and stay away until the pointer moves again.
    pub(crate) fn suspend_read_hover(&mut self) {
        let shown = self.read_hover.suspend();
        let focused = self.session.panels.active().id;
        let changed = crate::ui::interaction::retarget_alignment_hover(
            &mut self.session.panels,
            focused,
            None,
        );
        for panel_id in &changed {
            self.synchronize_alignment_scene_interactions(*panel_id);
        }
        if !changed.is_empty() {
            self.prepare_alignment_scene_frame();
        }
        if shown || !changed.is_empty() {
            self.window.request_redraw();
        }
    }

    /// The tooltip of the current hover, built once per target.
    fn read_hover_tooltip_lines(&self, hover: &ReadHover) -> Vec<TooltipLine> {
        let target = hover.target;
        let Some(panel) = self.session.panels.get(hover.panel) else {
            return Vec::new();
        };
        let Some(scene) = self
            .alignment_scene_snapshot_for_panel_track(hover.panel, target.hit.track)
            .filter(|scene| scene.reads().revision() == target.hit.scene_revision)
            .cloned()
        else {
            return Vec::new();
        };
        let reference = panel.reference_slice.as_deref().filter(|slice| {
            scene.reads().presentation().reference == panel.reference_slice_revision
                && slice.interval.contig() == scene.prepared_interval().contig()
        });
        let label = matches!(target.hit.component, AlignmentComponent::Label { .. })
            .then(|| self.presented_label_text(hover.panel, &target))
            .flatten();
        let position = self.read_hover.position();
        self.read_hover_tooltip
            .borrow_mut()
            .lines(&target, position, |cache| {
                let record = cache.record(&target, || {
                    scene.inspect_retained(target.hit.handle).ok().flatten()
                });
                let facts = self.read_hover_facts(hover, &scene, record.as_deref());
                tooltip_lines(
                    &target,
                    TooltipContext {
                        record: record.as_deref(),
                        reference,
                        contig: &panel.contig_name,
                        position,
                        label: label.as_deref(),
                    },
                    &facts,
                )
            })
    }

    /// The scene's locus facts for the hovered target, plus the Sashimi junction of a splice
    /// gap and the pair of a pair connector.
    fn read_hover_facts(
        &self,
        hover: &ReadHover,
        scene: &AlignmentSceneSnapshot,
        record: Option<&AlignmentRecord>,
    ) -> LocusFacts {
        let target = hover.target;
        let mut facts = scene_locus_facts(scene, &target, record);
        match target.hit.component {
            AlignmentComponent::ReferenceSkip { cigar_index } => {
                facts.junction = record
                    .and_then(|record| {
                        crate::ui::read_hover::operation_origin(record, cigar_index as usize)
                    })
                    .and_then(|(start, _, length)| {
                        self.junction_facts(hover, scene, start, start + u64::from(length))
                    });
            }
            AlignmentComponent::PairConnector { .. } => {
                facts.pair = record.and_then(|record| self.pair_facts(hover, scene, record));
            }
            _ => {}
        }
        facts
    }

    /// The junction `start..end` as the Sashimi lane presents it (strand and its source, motif)
    /// and as its arc's tooltip counts it.
    fn junction_facts(
        &self,
        hover: &ReadHover,
        scene: &AlignmentSceneSnapshot,
        start: u64,
        end: u64,
    ) -> Option<JunctionFacts> {
        use rgv_model::{JunctionStrand, JunctionStrandSource, SpliceMotif};
        let evidence =
            crate::workspace::alignment_management::sashimi_presentation::scene_junctions(scene)
                .into_iter()
                .find(|junction| junction.start == start && junction.end == end)?;
        let presentation = self
            .sashimi_presentation(hover.panel, hover.target.hit.track)
            .map_or_else(
                || rgv_model::JunctionPresentation::from_evidence(&evidence),
                |presentation| presentation.table.resolve(&evidence),
            );
        let strand = match presentation.strand {
            JunctionStrand::Positive => "+",
            JunctionStrand::Negative => "−",
            JunctionStrand::Unknown => "unknown",
        };
        let strand = if presentation.source == JunctionStrandSource::Unknown {
            strand.to_owned()
        } else {
            format!("{strand} ({})", presentation.source.label())
        };
        let motif = match presentation.motif {
            SpliceMotif::Unavailable => "motif unavailable".to_owned(),
            SpliceMotif::NonCanonical => "non-canonical motif".to_owned(),
            motif => motif.transcript_label().to_owned(),
        };
        let panel = self.session.panels.get(hover.panel)?;
        let context = crate::inspection::rna::junction_annotation_context(
            &self.session.tracks,
            &panel.track_snapshots,
            &panel.track_snapshot_intervals,
            panel.viewport.interval(),
        );
        let annotation = (context.lookup.source_tracks > 0)
            .then(|| match context.lookup.status(start, end) {
                rgv_layout::JunctionAnnotationStatus::Annotated => Some("annotated".to_owned()),
                rgv_layout::JunctionAnnotationStatus::Novel => Some("novel".to_owned()),
                rgv_layout::JunctionAnnotationStatus::Unknown => None,
            })
            .flatten();
        Some(JunctionFacts {
            depth: evidence.depth,
            motif,
            strand,
            annotation,
        })
    }

    /// The read's pair as the read inspector classifies it.
    fn pair_facts(
        &self,
        hover: &ReadHover,
        scene: &AlignmentSceneSnapshot,
        record: &AlignmentRecord,
    ) -> Option<PairFacts> {
        let (contig, start) = record.mate_contig.zip(record.mate_start)?;
        let panel = self.session.panels.get(hover.panel)?;
        let bounds = scene.reads().atlas().layout_contract().pair_length_bounds;
        let pair = rgv_hts::pair_evidence(record, panel.viewport.interval(), bounds);
        let name = self
            .alignment_dictionary_for_track(hover.target.hit.track)
            .and_then(|dictionary| dictionary.get(contig))
            .map_or_else(
                || format!("tid {}", contig.get()),
                |contig| contig.name().to_owned(),
            );
        Some(PairFacts {
            insert_size: pair.absolute_template_length,
            mate: format!(
                "{name}:{}",
                crate::workspace::alignment_management::sashimi_presentation::grouped_position(
                    start + 1
                )
            ),
            orientation: crate::inspection::inspector_controller::pair_orientation_label(
                pair.orientation,
            ),
            classification: crate::inspection::inspector_controller::pair_classification_label(
                pair.classification,
            ),
        })
    }

    /// Paints the hovered target's tooltip beside the pointer once its delay has passed, and
    /// asks egui for exactly the frames the hover still needs: every frame while a highlight
    /// fades, one when a tooltip delay ends, none once all is settled.
    pub(super) fn paint_read_hover_tooltip(&self, ui: &egui::Ui, genomic: egui::Rect) {
        let now = Instant::now();
        let reduced_motion = self.session.settings.reduced_motion;
        if let Some(delay) = self.read_hover.next_repaint(now, reduced_motion) {
            ui.ctx().request_repaint_after(delay);
        }
        let Some(hover) = self.read_hover.tooltip(now) else {
            return;
        };
        if self.dragging
            || self.session.panels.active().hovered_junction_arc.is_some()
            || !self
                .session
                .panels
                .get(hover.panel)
                .is_some_and(|panel| panel.navigation_presentation.is_interactive())
        {
            return;
        }
        // A held hover (the window covered or unfocused over a resting pointer) has no egui
        // pointer: it stays where it was last placed.
        let live = ui.ctx().input(|input| input.pointer.hover_pos());
        let Some(pointer) = live.or_else(|| {
            self.read_hover_held
                .then(|| self.read_hover_anchor.get())
                .flatten()
        }) else {
            return;
        };
        if !genomic.contains(pointer) {
            return;
        }
        self.read_hover_anchor.set(Some(pointer));
        let lines = self.read_hover_tooltip_lines(&hover);
        let Some(card) = TooltipCard::new(&lines) else {
            return;
        };
        // Copy the tooltip unless a text field (which copies its own selection) has focus.
        let copy = ui.ctx().input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
        }) && ui.ctx().memory(|memory| memory.focused().is_none());
        if copy {
            ui.ctx().copy_text(tooltip_copy_text(&lines));
        }
        let swatch =
            crate::ui::read_hover::palette_color32(crate::theme::active().palette().resolve(
                rgv_render::semantic_alignment_color(
                    hover.target.primitive.component,
                    hover.target.primitive.style,
                ),
            ));
        paint_tooltip_card(ui.ctx(), pointer, &card, swatch);
    }
}

/// Paints `card` beside `anchor` (the pointer) in the theme's popup style: a shadowed panel
/// with a colour swatch of the hovered element before its heading, a quieter subtitle, a
/// hairline above the body and a small footer hint.
#[allow(clippy::too_many_lines)]
fn paint_tooltip_card(
    ctx: &egui::Context,
    anchor: egui::Pos2,
    card: &TooltipCard,
    swatch: egui::Color32,
) {
    const SWATCH: f32 = 10.0;
    const SWATCH_GAP: f32 = 6.0;
    const LINE_GAP: f32 = 2.0;
    const SECTION_GAP: f32 = 6.0;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("read-hover-tooltip"),
    ));
    let visuals = ctx.global_style().visuals.clone();
    let font = |line: &TooltipLine, size: f32| {
        if line.monospace {
            egui::FontId::monospace(size)
        } else {
            egui::FontId::proportional(size)
        }
    };
    let heading = painter.layout_no_wrap(
        card.heading.text.clone(),
        font(&card.heading, 13.0),
        visuals.strong_text_color(),
    );
    let subtitle = card.subtitle.as_ref().map(|subtitle| {
        painter.layout_no_wrap(
            subtitle.clone(),
            egui::FontId::proportional(11.0),
            visuals.weak_text_color(),
        )
    });
    let body = card
        .body
        .iter()
        .map(|line| {
            painter.layout_no_wrap(line.text.clone(), font(line, 11.5), visuals.text_color())
        })
        .collect::<Vec<_>>();
    let hint = painter.layout_no_wrap(
        card.hint.clone(),
        egui::FontId::proportional(10.0),
        visuals.weak_text_color(),
    );
    let heading_width = SWATCH + SWATCH_GAP + heading.size().x;
    let width = std::iter::once(heading_width)
        .chain(subtitle.iter().map(|galley| galley.size().x))
        .chain(body.iter().map(|galley| galley.size().x))
        .chain(std::iter::once(hint.size().x))
        .fold(0.0_f32, f32::max);
    let body_height = body
        .iter()
        .map(|galley| galley.size().y + LINE_GAP)
        .sum::<f32>();
    let height = heading.size().y
        + subtitle
            .as_ref()
            .map_or(0.0, |galley| LINE_GAP + galley.size().y)
        + if body.is_empty() {
            0.0
        } else {
            2.0 * SECTION_GAP + body_height
        }
        + SECTION_GAP
        + hint.size().y;
    let padding = egui::vec2(10.0, 8.0);
    let size = egui::vec2(width, height) + padding * 2.0;
    let rect = crate::ui::read_hover::tooltip_rect(anchor, size, ctx.content_rect());
    let corner_radius = visuals.menu_corner_radius;
    painter.add(visuals.popup_shadow.as_shape(rect, corner_radius));
    painter.rect(
        rect,
        corner_radius,
        visuals.window_fill,
        visuals.window_stroke,
        egui::StrokeKind::Inside,
    );
    let left = rect.left() + padding.x;
    let mut y = rect.top() + padding.y;
    // Swatch, centred on the heading's first line, with a hairline so pale colours still show.
    let swatch_rect = egui::Rect::from_center_size(
        egui::pos2(left + SWATCH * 0.5, y + heading.size().y * 0.5),
        egui::vec2(SWATCH, SWATCH),
    );
    painter.rect(
        swatch_rect,
        2.0,
        swatch,
        egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
    let heading_height = heading.size().y;
    painter.galley(
        egui::pos2(left + SWATCH + SWATCH_GAP, y),
        heading,
        visuals.strong_text_color(),
    );
    y += heading_height;
    if let Some(subtitle) = subtitle {
        y += LINE_GAP;
        let height = subtitle.size().y;
        painter.galley(egui::pos2(left, y), subtitle, visuals.weak_text_color());
        y += height;
    }
    if !body.is_empty() {
        y += SECTION_GAP;
        painter.line_segment(
            [egui::pos2(left, y), egui::pos2(rect.right() - padding.x, y)],
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        );
        y += SECTION_GAP;
        for galley in body {
            let height = galley.size().y;
            painter.galley(egui::pos2(left, y), galley, visuals.text_color());
            y += height + LINE_GAP;
        }
    }
    y += SECTION_GAP;
    painter.galley(egui::pos2(left, y), hint, visuals.weak_text_color());
}
