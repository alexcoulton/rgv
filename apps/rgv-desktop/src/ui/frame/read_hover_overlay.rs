//! The read-stack hover tooltip, painted in the egui pass over the canvas (it follows the
//! pointer, not the reads), and the facts it states. The highlight is painted from the
//! presented frame at submit time; see `Viewer::presented_read_hover_highlights`.

use crate::Viewer;
use crate::inspection::read_hover_facts::scene_locus_facts;
use crate::ui::read_hover::{
    JunctionFacts, LocusFacts, PairFacts, ReadHover, TooltipContext, TooltipLine, tooltip_lines,
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
        let Some(pointer) = ui.ctx().input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if !genomic.contains(pointer) {
            return;
        }
        let lines = self.read_hover_tooltip_lines(&hover);
        if lines.is_empty() {
            return;
        }
        let tooltip_painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("read-hover-tooltip"),
        ));
        let text_color = crate::theme::adapt(egui::Color32::from_gray(24));
        let galleys = lines
            .into_iter()
            .map(|line| {
                let font = if line.monospace {
                    egui::FontId::monospace(11.0)
                } else {
                    egui::FontId::proportional(11.0)
                };
                tooltip_painter.layout_no_wrap(line.text, font, text_color)
            })
            .collect::<Vec<_>>();
        let width = galleys
            .iter()
            .map(|galley| galley.size().x)
            .fold(0.0_f32, f32::max);
        let height = galleys
            .iter()
            .map(|galley| galley.size().y + 1.0)
            .sum::<f32>();
        let padding = egui::vec2(8.0, 6.0);
        let size = egui::vec2(width, height) + padding * 2.0;
        let rect = crate::ui::read_hover::tooltip_rect(pointer, size, ui.ctx().content_rect());
        tooltip_painter.rect(
            rect,
            4.0,
            crate::theme::adapt(egui::Color32::from_rgba_unmultiplied(252, 252, 250, 245)),
            egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(170))),
            egui::StrokeKind::Inside,
        );
        let mut y = rect.top() + padding.y;
        for galley in galleys {
            let line_height = galley.size().y + 1.0;
            tooltip_painter.galley(egui::pos2(rect.left() + padding.x, y), galley, text_color);
            y += line_height;
        }
    }
}
