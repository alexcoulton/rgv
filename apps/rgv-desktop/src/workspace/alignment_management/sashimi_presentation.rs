//! Per-lane Sashimi presentation: resolved junction strands, weak-junction hiding, splice-site
//! totals and the splice-site / transcript filters.
//!
//! The table is built on the UI thread from the junctions of the lane's published scene, the
//! reference motifs already cached for them, and the track and panel settings. It is rebuilt
//! only when one of those inputs changes (a new scene, a motif answer, a setting), never per
//! frame. Missing motifs are requested from the reference actor here and arrive later; the
//! presentation is then rebuilt and the junction pages whose drawing changed are repacked.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use rgv_alignment_scene::{
    AlignmentDerivedLaneOptions, AlignmentPanelSceneFrame, AlignmentSceneSnapshot,
    SummaryPageFamily, SummaryRevision, SummarySemanticComponent,
};
use rgv_core::{GenomicInterval, PanelId, SessionFilters, TrackId};
use rgv_model::{
    JunctionEvidence, SashimiPresentationOptions, SashimiPresentationTable, SpliceSite,
};

use crate::Viewer;

/// A Sashimi lane's transcript filter: only junctions matching these introns are drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SashimiIntronFilter {
    pub(crate) track_id: TrackId,
    pub(crate) introns: Arc<BTreeSet<(u64, u64)>>,
    pub(crate) label: String,
}

#[derive(Clone, Debug, PartialEq)]
struct SashimiPresentationInput {
    summary_revision: SummaryRevision,
    prepared_interval: GenomicInterval,
    motif_generation: u64,
    reference_revision: Option<rgv_alignment_scene::ReferenceRevision>,
    options: SashimiPresentationOptions,
}

/// The resolved presentation of one Sashimi lane of one panel.
#[derive(Clone, Debug)]
pub(crate) struct SashimiLanePresentation {
    input: SashimiPresentationInput,
    pub(crate) table: Arc<SashimiPresentationTable>,
    /// Some motifs could not be requested yet (a full reference queue); ask again next frame.
    motifs_deferred: bool,
}

/// The weak-junction, strand and filter options a track's settings and the panel's filters ask
/// for.
pub(crate) fn sashimi_presentation_options(
    filters: &SessionFilters,
    site_filter: Option<SpliceSite>,
    intron_filter: Option<Arc<BTreeSet<(u64, u64)>>>,
) -> SashimiPresentationOptions {
    SashimiPresentationOptions {
        minimum_depth: filters.minimum_junction_depth.max(1),
        relative_threshold_percent: filters.junction_relative_threshold_percent,
        show_all: filters.show_all_junctions,
        library: filters.junction_library_strandedness,
        site_filter,
        intron_filter,
    }
}

/// Every junction of a scene's prepared junction pages (a junction appears once).
pub(crate) fn scene_junctions(scene: &AlignmentSceneSnapshot) -> Vec<JunctionEvidence> {
    let mut junctions = Vec::new();
    for page in scene.summary_pages_for(SummaryPageFamily::Junctions, scene.prepared_interval()) {
        for primitive in page.primitives() {
            if let SummarySemanticComponent::Junction(junction) = primitive.component {
                junctions.push(junction.evidence);
            }
        }
    }
    junctions.sort_unstable_by_key(|junction| (junction.start, junction.end));
    junctions.dedup_by_key(|junction| (junction.start, junction.end));
    junctions
}

/// Introns of a transcript from its exon blocks: the gaps between consecutive sorted blocks.
pub(crate) fn transcript_introns(blocks: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    let mut exons = blocks.to_vec();
    exons.sort_unstable();
    exons.dedup();
    exons
        .windows(2)
        .filter_map(|pair| {
            let [(_, left_end), (right_start, _)] = pair else {
                return None;
            };
            (right_start > left_end).then_some((*left_end, *right_start))
        })
        .collect()
}

impl Viewer {
    /// The current presentation of one panel's Sashimi lane, if it has one.
    pub(crate) fn sashimi_presentation(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<&SashimiLanePresentation> {
        self.sashimi_presentations.get(&(panel_id, track_id))
    }

    /// Rebuilds the presentation of every Sashimi lane whose inputs changed, and asks the
    /// reference actor for motifs it does not know yet. Lanes no longer captured are forgotten.
    pub(crate) fn refresh_sashimi_presentations(
        &mut self,
        panel_frames: &HashMap<PanelId, AlignmentPanelSceneFrame>,
    ) {
        let mut live = BTreeSet::new();
        for (panel_id, frame) in panel_frames {
            for lane in frame.lanes.iter() {
                if !lane.derived_lanes.iter().any(|derived| {
                    matches!(derived.options, AlignmentDerivedLaneOptions::Sashimi(_))
                }) {
                    continue;
                }
                let Some(scene) = lane.snapshot.as_ref() else {
                    continue;
                };
                live.insert((*panel_id, lane.track_id));
                self.refresh_sashimi_presentation(*panel_id, lane.track_id, scene);
            }
        }
        self.sashimi_presentations
            .retain(|key, _| live.contains(key));
    }

    fn refresh_sashimi_presentation(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        scene: &Arc<AlignmentSceneSnapshot>,
    ) {
        let filters = self.alignment_filters_for_track(track_id);
        let (site_filter, intron_filter) =
            self.session
                .panels
                .get(panel_id)
                .map_or((None, None), |panel| {
                    (
                        panel
                            .sashimi_site_filter
                            .filter(|(track, _)| *track == track_id)
                            .map(|(_, site)| site),
                        panel
                            .sashimi_intron_filter
                            .as_ref()
                            .filter(|filter| filter.track_id == track_id)
                            .map(|filter| Arc::clone(&filter.introns)),
                    )
                });
        let reference_revision = self.session.reference_revision;
        let input = SashimiPresentationInput {
            summary_revision: scene.summary_revision(),
            prepared_interval: scene.prepared_interval(),
            motif_generation: self.session.splice_motifs.generation(),
            reference_revision,
            options: sashimi_presentation_options(&filters, site_filter, intron_filter),
        };
        let current = self.sashimi_presentations.get(&(panel_id, track_id));
        if current.is_some_and(|current| current.input == input && !current.motifs_deferred) {
            return;
        }
        let unchanged = current.is_some_and(|current| current.input == input);
        let junctions = scene_junctions(scene);
        let contig = scene.prepared_interval().contig();
        // Motif lookups run on the reference actor; this only queues the introns not yet known.
        let mut motifs_deferred = false;
        if let Ok(Some(contig_name)) = self.session.compatible_reference_contig_for_panel(panel_id)
        {
            let introns = junctions
                .iter()
                .map(|junction| (junction.start, junction.end))
                .collect::<Vec<_>>();
            let engine = self.session.reference_engine.as_ref();
            let (_, deferred) = self.session.splice_motifs.request_missing(
                engine,
                reference_revision,
                contig,
                &contig_name,
                introns,
            );
            motifs_deferred = deferred > 0;
        }
        if unchanged {
            if let Some(current) = self.sashimi_presentations.get_mut(&(panel_id, track_id)) {
                current.motifs_deferred = motifs_deferred;
            }
            return;
        }
        let motifs = &self.session.splice_motifs;
        let table = SashimiPresentationTable::build(
            &junctions,
            |start, end| motifs.motif(reference_revision, contig, start, end),
            &input.options,
        );
        // A request may have reset the cache for a new FASTA; key on what the table saw.
        let input = SashimiPresentationInput {
            motif_generation: self.session.splice_motifs.generation(),
            ..input
        };
        self.sashimi_presentations.insert(
            (panel_id, track_id),
            SashimiLanePresentation {
                input,
                table: Arc::new(table),
                motifs_deferred,
            },
        );
    }
}

/// How close (in points) the pointer must be to an arc's end to pick its splice site.
pub(crate) const SASHIMI_SITE_ACQUISITION_POINTS: f64 = 6.0;

/// The splice site at the arc end nearest `x`, when `x` is within
/// [`SASHIMI_SITE_ACQUISITION_POINTS`] of it. An arc too short to tell its ends from its body
/// (under four acquisition radii) has none, so it can still be selected.
pub(crate) fn splice_site_at_arc_end(
    arc_x: f64,
    arc_width: f64,
    x: f64,
    start: u64,
    end: u64,
) -> Option<SpliceSite> {
    if !(arc_width >= 4.0 * SASHIMI_SITE_ACQUISITION_POINTS && x.is_finite()) {
        return None;
    }
    let left = (x - arc_x).abs();
    let right = (x - (arc_x + arc_width)).abs();
    if left.min(right) > SASHIMI_SITE_ACQUISITION_POINTS {
        return None;
    }
    Some(if left <= right {
        SpliceSite::start_of(start)
    } else {
        SpliceSite::end_of(end)
    })
}

/// `12,345,678`.
pub(crate) fn grouped_position(position: u64) -> String {
    let digits = position.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The chip text of an active splice-site filter.
pub(crate) fn site_filter_label(contig_name: &str, site: SpliceSite) -> String {
    format!(
        "Showing junctions at {contig_name}:{} — clear",
        grouped_position(site.one_based_position())
    )
}

/// The hover tooltip of one junction: coordinates, intron length, count, strand and its source,
/// motif, annotation, and the share of each splice site.
pub(crate) fn junction_tooltip_lines(
    contig_name: &str,
    evidence: &rgv_model::JunctionEvidence,
    presentation: &rgv_model::JunctionPresentation,
    count: &str,
    annotation: Option<&str>,
) -> Vec<String> {
    use rgv_model::{JunctionStrand, JunctionStrandSource, SpliceMotif, format_psi};
    let strand = match presentation.strand {
        JunctionStrand::Positive => "+",
        JunctionStrand::Negative => "−",
        JunctionStrand::Unknown => "unknown",
    };
    let mut lines = vec![
        format!(
            "{contig_name}:{}-{}",
            grouped_position(evidence.start.saturating_add(1)),
            grouped_position(evidence.end)
        ),
        format!(
            "Intron {} bp · {count}",
            grouped_position(evidence.end.saturating_sub(evidence.start))
        ),
    ];
    let mut strand_line = if presentation.source == JunctionStrandSource::Unknown {
        format!("Strand {strand}")
    } else {
        format!("Strand {strand} (from {})", presentation.source.label())
    };
    if presentation.conflicting_tags {
        use std::fmt::Write as _;
        let _ = write!(
            strand_line,
            " · tags disagree: + {} / − {}, drawn by majority",
            evidence.tagged_positive_depth, evidence.tagged_negative_depth
        );
    }
    lines.push(strand_line);
    lines.push(match presentation.motif {
        SpliceMotif::Unavailable => "Motif unavailable (no reference or not read yet)".to_owned(),
        SpliceMotif::NonCanonical => "Motif non-canonical".to_owned(),
        motif => format!("Motif {}", motif.transcript_label()),
    });
    if let Some(annotation) = annotation {
        lines.push(annotation.to_owned());
    }
    let (first, second) = presentation.donor_acceptor_psi(evidence.depth);
    let part = |share: Option<f64>| share.map_or_else(|| "–".to_owned(), format_psi);
    lines.push(if presentation.strand == JunctionStrand::Unknown {
        format!(
            "PSI start {} · end {}",
            part(presentation.start_psi(evidence.depth)),
            part(presentation.end_psi(evidence.depth))
        )
    } else {
        format!("PSI donor {} · acceptor {}", part(first), part(second))
    });
    lines
}

impl Viewer {
    /// Sets (or with `None`, clears) the splice-site filter of one Sashimi lane, and redraws
    /// from the published scene.
    pub(crate) fn set_sashimi_site_filter(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        site: Option<SpliceSite>,
    ) {
        let Some(panel) = self.session.panels.get_mut(panel_id) else {
            return;
        };
        if !panel.set_sashimi_site_filter(track_id, site) {
            return;
        }
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("sashimi splice-site filter changed");
        self.window.request_redraw();
    }

    /// Shows only the junctions of a transcript's introns in one Sashimi lane.
    pub(crate) fn set_sashimi_intron_filter(
        &mut self,
        panel_id: PanelId,
        filter: Option<SashimiIntronFilter>,
    ) {
        let Some(panel) = self.session.panels.get_mut(panel_id) else {
            return;
        };
        if panel.sashimi_intron_filter == filter {
            return;
        }
        panel.sashimi_intron_filter = filter;
        panel.hovered_junction_arc = None;
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("sashimi transcript filter changed");
        self.window.request_redraw();
    }

    /// Clears the active panel's Sashimi filters. Returns whether one was active.
    pub(crate) fn clear_active_sashimi_filters(&mut self) -> bool {
        self.clear_sashimi_filters(self.session.panels.active().id)
    }

    /// Clears a panel's Sashimi splice-site and transcript filters. Returns whether one was
    /// active.
    pub(crate) fn clear_sashimi_filters(&mut self, panel_id: PanelId) -> bool {
        let Some(panel) = self.session.panels.get_mut(panel_id) else {
            return false;
        };
        if !panel.clear_sashimi_filters() {
            return false;
        }
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("sashimi junction filter cleared");
        self.window.request_redraw();
        true
    }

    /// Remembers whether the hovered junction is annotated, so the tooltip does not walk the
    /// gene annotation every frame.
    pub(crate) fn refresh_sashimi_hover_annotation(&mut self) {
        let panel = self.session.panels.active();
        let Some(hover) = panel.hovered_junction_arc else {
            self.sashimi_hover_annotation = None;
            return;
        };
        let key = (hover.key.track_id, hover.key.start, hover.key.end);
        if self
            .sashimi_hover_annotation
            .as_ref()
            .is_some_and(|(current, _)| *current == key)
        {
            return;
        }
        let context = crate::inspection::rna::junction_annotation_context(
            &self.session.tracks,
            &panel.track_snapshots,
            &panel.track_snapshot_intervals,
            panel.viewport.interval(),
        );
        let text = if context.lookup.source_tracks == 0 {
            None
        } else {
            match context.lookup.status(hover.key.start, hover.key.end) {
                rgv_layout::JunctionAnnotationStatus::Annotated => {
                    Some("Annotated junction".to_owned())
                }
                rgv_layout::JunctionAnnotationStatus::Novel => {
                    Some("Novel (not in the loaded annotation)".to_owned())
                }
                rgv_layout::JunctionAnnotationStatus::Unknown => None,
            }
        };
        self.sashimi_hover_annotation = Some((key, text));
    }

    /// The selected feature of a panel when it is a spliced transcript: its label and introns.
    pub(crate) fn selected_transcript_introns(
        &self,
        panel_id: PanelId,
    ) -> Option<(String, BTreeSet<(u64, u64)>)> {
        let panel = self.session.panels.get(panel_id)?;
        let selection = panel.selected_feature?;
        let record = panel
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                rgv_engine::TrackSnapshotData::Features(records) => {
                    records.get(selection.record_index)
                }
                _ => None,
            })?;
        let introns = transcript_introns(&record.blocks);
        if introns.is_empty() {
            return None;
        }
        let label = record
            .attributes
            .iter()
            .find(|(key, _)| key == "transcript_id")
            .map(|(_, value)| value.clone())
            .or_else(|| record.name.clone())
            .unwrap_or_else(|| "the selected transcript".to_owned());
        Some((label, introns))
    }

    /// A click on an arc's end, or on the coverage at a drawn splice site, filters that lane to
    /// the junctions using the site. Returns whether the click did so.
    pub(crate) fn filter_sashimi_at_cursor(&mut self) -> bool {
        let panel_id = self.session.panels.active().id;
        if let Some(hover) = self.junction_arc_hover_at_cursor()
            && let Some(site) = hover.splice_site
        {
            self.set_sashimi_site_filter(panel_id, hover.key.track_id, Some(site));
            self.session.data_status =
                site_filter_label(&self.session.panels.active().contig_name.clone(), site)
                    .replace(" — clear", "; press Esc to clear");
            return true;
        }
        let Some((track_id, _)) = self.pileup_position_at_cursor() else {
            return false;
        };
        let scale = self.window.scale_factor();
        let x = self.cursor.x / scale;
        let panel_rect = self.active_panel_rect();
        let viewport = self.session.panels.active().viewport;
        let position = viewport.genomic_at_pixel(x - f64::from(panel_rect.left()));
        let tolerance = 3.0 * viewport.bases_per_pixel();
        let Some(site) = self
            .sashimi_presentation(panel_id, track_id)
            .and_then(|presentation| {
                presentation
                    .table
                    .nearest_drawn_splice_site(position, tolerance)
            })
        else {
            return false;
        };
        self.set_sashimi_site_filter(panel_id, track_id, Some(site));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arc_end_picks_its_splice_site_and_the_body_does_not() {
        // A 100-point arc from x = 50 to x = 150 for the intron [1_000, 2_000).
        assert_eq!(
            splice_site_at_arc_end(50.0, 100.0, 53.0, 1_000, 2_000),
            Some(SpliceSite::start_of(1_000))
        );
        assert_eq!(
            splice_site_at_arc_end(50.0, 100.0, 148.0, 1_000, 2_000),
            Some(SpliceSite::end_of(2_000))
        );
        assert_eq!(
            splice_site_at_arc_end(50.0, 100.0, 100.0, 1_000, 2_000),
            None
        );
        // Too short to tell an end from the body.
        assert_eq!(splice_site_at_arc_end(50.0, 20.0, 51.0, 1_000, 2_000), None);
        assert_eq!(
            site_filter_label("chr3", SpliceSite::start_of(149_371_867)),
            "Showing junctions at chr3:149,371,868 — clear"
        );
    }

    #[test]
    fn the_tooltip_names_strand_source_motif_and_both_site_shares() {
        let evidence = rgv_model::JunctionEvidence {
            start: 149_371_867,
            end: 149_375_000,
            depth: 30,
            ..rgv_model::JunctionEvidence::default()
        };
        let presentation = rgv_model::JunctionPresentation {
            strand: rgv_model::JunctionStrand::Negative,
            source: rgv_model::JunctionStrandSource::Motif,
            motif: rgv_model::SpliceMotif::CtAc,
            start_site_total: 40,
            end_site_total: 30,
            ..rgv_model::JunctionPresentation::default()
        };
        let lines = junction_tooltip_lines("chr3", &evidence, &presentation, "30 reads", None);
        assert_eq!(lines[0], "chr3:149,371,868-149,375,000");
        assert_eq!(lines[1], "Intron 3,133 bp · 30 reads");
        assert_eq!(lines[2], "Strand − (from motif)");
        assert_eq!(lines[3], "Motif GT-AG");
        // Minus strand: the donor is the end (30 of 30), the acceptor the start (30 of 40).
        assert_eq!(lines[4], "PSI donor 100% · acceptor 75%");
    }

    #[test]
    fn transcript_introns_are_the_gaps_between_sorted_exons() {
        let introns = transcript_introns(&[(500, 600), (100, 200), (300, 400), (300, 400)]);
        assert_eq!(
            introns.into_iter().collect::<Vec<_>>(),
            vec![(200, 300), (400, 500)]
        );
    }

    #[test]
    fn options_follow_track_settings_and_panel_filters() {
        let mut filters = SessionFilters::default();
        let defaults = sashimi_presentation_options(&filters, None, None);
        assert_eq!(defaults.minimum_depth, 2);
        assert!((defaults.relative_threshold_percent - 1.0).abs() < f32::EPSILON);
        assert!(!defaults.show_all);
        filters.show_all_junctions = true;
        filters.junction_library_strandedness = rgv_core::LibraryStrandedness::FirstOfPairAntisense;
        let site = SpliceSite::start_of(149_371_867);
        let options = sashimi_presentation_options(&filters, Some(site), None);
        assert!(options.show_all);
        assert_eq!(options.site_filter, Some(site));
        assert_eq!(
            options.library,
            rgv_core::LibraryStrandedness::FirstOfPairAntisense
        );
    }
}
