use crate::action::Action;
use crate::ui::TrackFrameKey;
use crate::workspace::navigation::{
    annotation_search_hit_locus, annotation_search_hit_primary_label,
    locus_expression_is_navigable, preferred_annotation_search_hit,
};
use crate::workspace::tracks::replace_panel_track_selection;
use crate::{Viewer, ui};
use anyhow::{Context as _, Result};
use rgv_automation::AutomationHistoryNavigation;
use rgv_core::PanelId;
use rgv_core::{ContigId, NavigationEntry, Viewport};
use rgv_engine::{TrackSearchHit, TrackSearchResults};

const NO_SEARCHABLE_TRACKS: &str = "No searchable annotation tracks are open";

/// What the locus box says about a name it searched for, shown beside the box until the box is
/// edited or the view navigates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocusSearchNote {
    /// The query as submitted; the note shows only while the box still holds it.
    pub(crate) query: String,
    pub(crate) text: String,
    /// A failure (also kept in the notification log), not a hint.
    pub(crate) failure: bool,
}

/// What submitting a searched name from the locus box does.
#[derive(Debug, PartialEq)]
pub(crate) enum LocusSearchOutcome<'a> {
    /// One feature answers the query: go there.
    Navigate(&'a TrackSearchHit),
    /// Nothing to go to: say why beside the box.
    Note(LocusSearchNote),
}

/// Decides between navigating and explaining, for results of a search for `query`.
pub(crate) fn locus_search_outcome<'a>(
    results: &'a TrackSearchResults,
    query: &str,
) -> LocusSearchOutcome<'a> {
    if let Some(hit) = preferred_annotation_search_hit(results, query) {
        return LocusSearchOutcome::Navigate(hit);
    }
    let query = query.trim();
    let note = if results.hits.is_empty() {
        let reason = if results.index_truncated {
            "; gene search covers only part of an open track (see Notifications)"
        } else if results.scan_truncated {
            "; the search was bounded, so a longer name may find it"
        } else {
            ""
        };
        LocusSearchNote {
            query: query.to_owned(),
            text: format!("No annotation matches for {query}{reason}"),
            failure: true,
        }
    } else {
        LocusSearchNote {
            query: query.to_owned(),
            text: format!(
                "{} matches for {query}; choose one from the list",
                results.hits.len()
            ),
            failure: false,
        }
    };
    LocusSearchOutcome::Note(note)
}

/// The note for a search submitted while no annotation track can be searched.
pub(crate) fn no_searchable_tracks_note(query: &str) -> LocusSearchNote {
    LocusSearchNote {
        query: query.trim().to_owned(),
        text: NO_SEARCHABLE_TRACKS.to_owned(),
        failure: true,
    }
}

/// The note to show beside a locus box holding `editor`: only while it still holds the query
/// the note answers, so editing the box (or navigating, which rewrites it) hides it.
pub(crate) fn visible_locus_search_note<'a>(
    note: Option<&'a LocusSearchNote>,
    editor: &str,
) -> Option<&'a LocusSearchNote> {
    note.filter(|note| note.query.eq_ignore_ascii_case(editor.trim()))
}

impl Viewer {
    /// Moves the focused panel to an explicit viewport, possibly on another contig.
    pub(crate) fn navigate_to_viewport(
        &mut self,
        viewport: Viewport,
        contig_name: String,
        contig_length: u64,
    ) {
        let panel_id = self.session.panels.active().id;
        if let Err(error) = self.apply_action(Action::RetargetPanel {
            panel_id,
            viewport,
            contig_name,
            contig_length,
        }) {
            self.report_error(error.to_string());
        }
    }

    /// Whether the expression should navigate directly and not be sent to
    /// ID/annotation search (see [`locus_expression_is_navigable`]).
    pub(crate) fn locus_expression_is_coordinates(&self, expression: &str) -> bool {
        let dictionary = self
            .session
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.session.reference_dictionary.as_ref());
        locus_expression_is_navigable(expression, dictionary, &self.session.contig_aliases)
    }

    pub(crate) fn navigate_to_locus(&mut self, panel_id: PanelId, locus: &str) -> Result<()> {
        self.apply_action(Action::GoToLocus {
            panel_id,
            locus: locus.to_owned(),
        })?;
        self.locus_search_note = None;
        Ok(())
    }

    pub(crate) fn request_annotation_search(
        &mut self,
        query: &str,
        navigate_when_ready: bool,
        include_variant_ids: bool,
    ) {
        let query = query.trim();
        if query.is_empty() || query.len() > 256 {
            return;
        }
        let Some(engine) = &self.session.track_engine else {
            if navigate_when_ready {
                let note = no_searchable_tracks_note(query);
                self.report_warning(note.text.clone());
                self.locus_search_note = Some(note);
            } else {
                NO_SEARCHABLE_TRACKS.clone_into(&mut self.session.data_status);
            }
            return;
        };
        self.annotation_search_generation = self.annotation_search_generation.saturating_add(1);
        if navigate_when_ready {
            self.pending_annotation_navigation = Some(query.to_owned());
        }
        match engine.try_search(
            self.annotation_search_generation,
            query.to_owned(),
            50,
            include_variant_ids,
        ) {
            Ok(()) => {}
            Err(error) => {
                self.pending_annotation_navigation = None;
                self.report_error(error.to_string());
            }
        }
    }

    pub(crate) fn request_adjacent_feature(&mut self, forward: bool) {
        let Some(engine) = &self.session.track_engine else {
            NO_SEARCHABLE_TRACKS.clone_into(&mut self.session.data_status);
            return;
        };
        self.annotation_navigation_generation =
            self.annotation_navigation_generation.saturating_add(1);
        let interval = self.session.panels.active().viewport.interval();
        let position = if forward {
            interval.end()
        } else {
            interval.start()
        };
        if let Err(error) = engine.try_adjacent_feature(
            self.annotation_navigation_generation,
            self.session.panels.active_mut().contig_name.clone(),
            position,
            forward,
        ) {
            self.report_error(error.to_string());
        }
    }

    /// Navigates to the one feature a locus-box search found, or says beside the box (and, for
    /// a failure, in the notification log) why it did not.
    pub(crate) fn submit_annotation_search_results(&mut self, query: &str) {
        match locus_search_outcome(&self.annotation_search_results, query) {
            LocusSearchOutcome::Navigate(hit) => {
                let hit = hit.clone();
                self.navigate_to_annotation_hit(&hit);
            }
            LocusSearchOutcome::Note(note) => {
                if note.failure {
                    self.report_warning(note.text.clone());
                } else {
                    self.session.data_status.clone_from(&note.text);
                }
                self.locus_search_note = Some(note);
            }
        }
    }

    pub(crate) fn navigate_to_annotation_hit(&mut self, hit: &TrackSearchHit) {
        let locus = annotation_search_hit_locus(hit);
        match self.navigate_to_locus(self.session.panels.active().id, &locus) {
            Ok(()) => {
                replace_panel_track_selection(
                    self.session.panels.active_mut(),
                    TrackFrameKey::Auxiliary(hit.track_id),
                    ui::TrackPane::Annotation,
                );
                self.locus_editor = locus;
                self.session.data_status = format!(
                    "{} · {} · {}",
                    annotation_search_hit_primary_label(hit),
                    hit.track_name,
                    hit.record.feature_type.as_deref().unwrap_or("feature")
                );
                // A hit the track does not draw as itself still navigates; say why it is not
                // drawn.
                if let Some(note) = self
                    .session
                    .tracks
                    .tracks()
                    .iter()
                    .find(|track| track.id == hit.track_id)
                    .and_then(|track| {
                        crate::inspection::gene_transcripts::search_hit_presentation_note(
                            track,
                            &hit.record,
                        )
                    })
                {
                    self.session.data_status = format!("{} · {note}", self.session.data_status);
                }
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    /// The locus box and `goto`: one locus retargets the focused panel; several redefine the
    /// workspace as exactly those loci.
    pub(crate) fn navigate_to_loci(&mut self, expression: &str) -> Result<()> {
        let action =
            crate::action::translate::locus_box(self.session.panels.active().id, expression)?;
        self.apply_action(action)?;
        self.locus_search_note = None;
        Ok(())
    }

    pub(crate) fn navigate_to_chain_segment(&mut self, index: usize) -> Result<()> {
        let segment = self
            .session
            .panels
            .active()
            .selected_chain
            .as_ref()
            .and_then(|chain| chain.segments.get(index))
            .context("supplementary-chain segment is unavailable")?;
        let locus = format!(
            "{}:{}-{}",
            segment.reference_name,
            segment.start + 1,
            segment.end
        );
        self.navigate_to_locus(self.session.panels.active().id, &locus)?;
        if let Some(chain) = &mut self.session.panels.active_mut().selected_chain {
            chain.active_segment = index;
        }
        Ok(())
    }

    pub(crate) fn navigate_history_back(&mut self) {
        self.navigate_focused_history(AutomationHistoryNavigation::Back);
    }

    pub(crate) fn navigate_history_forward(&mut self) {
        self.navigate_focused_history(AutomationHistoryNavigation::Forward);
    }

    /// Back/Forward from the window. Reaching the end of history is silent; an entry that no
    /// longer resolves is reported.
    fn navigate_focused_history(&mut self, step: AutomationHistoryNavigation) {
        let panel_id = self.session.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateHistory { panel_id, step }) {
            self.report_error(error.to_string());
        }
    }

    pub(crate) fn navigate_home(&mut self) {
        let panel_id = self.session.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateHome { panel_id }) {
            self.report_error(error.to_string());
        }
    }

    /// Opens the first 100 kb of a contig chosen from the assembly's list.
    pub(crate) fn navigate_to_contig(&mut self, contig_id: ContigId) {
        let Some(contig) = self.session.assembly.dictionary.get(contig_id) else {
            self.commit_navigation_gesture();
            return;
        };
        let length = contig.length();
        if length == 0 {
            self.commit_navigation_gesture();
            return;
        }
        // The assembly numbers contigs independently of the navigation
        // dictionary, so carry the choice across by name.
        let entry = NavigationEntry {
            contig_name: contig.name().to_owned(),
            start: 0,
            end: length.min(100_000),
        };
        let panel_id = self.session.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateToEntry {
            panel_id,
            entry,
            record: true,
        }) {
            self.report_error(error.to_string());
        }
    }

    pub(crate) fn remember_current_locus(&mut self, panel_id: PanelId) {
        let interval = self.session.panels.panel(panel_id).viewport.interval();
        let locus = format!(
            "{}:{}-{}",
            self.session.panels.panel(panel_id).contig_name,
            interval.start() + 1,
            interval.end()
        );
        // The locus box is a focused-panel control; another panel's move must not overwrite it.
        if self.session.panels.is_active(panel_id) {
            self.locus_editor.clone_from(&locus);
        }
        self.session.settings.remember_locus(locus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::TrackId;
    use rgv_hts::{FeatureRecord, Strand};

    fn hit(matched_term: &str, start: u64) -> TrackSearchHit {
        TrackSearchHit {
            track_id: TrackId::new(1),
            track_name: "genes.gtf.gz".to_owned(),
            matched_term: matched_term.to_owned(),
            record: FeatureRecord {
                contig: "chr3".to_owned(),
                start,
                end: start + 100,
                name: Some(matched_term.to_uppercase()),
                feature_type: Some("gene".to_owned()),
                score: None,
                strand: Strand::Forward,
                blocks: Vec::new(),
                coding_interval: None,
                peak: None,
                attributes: Vec::new(),
            },
        }
    }

    fn results(hits: Vec<TrackSearchHit>) -> TrackSearchResults {
        TrackSearchResults {
            query: "PBRM1".to_owned(),
            hits,
            ..TrackSearchResults::default()
        }
    }

    fn note(outcome: LocusSearchOutcome<'_>) -> LocusSearchNote {
        match outcome {
            LocusSearchOutcome::Note(note) => note,
            LocusSearchOutcome::Navigate(hit) => panic!("navigated to {hit:?}"),
        }
    }

    #[test]
    fn one_exact_match_navigates() {
        let found = results(vec![hit("pbrm1", 10), hit("pbrm1-201", 20)]);
        assert_eq!(
            locus_search_outcome(&found, " PBRM1 "),
            LocusSearchOutcome::Navigate(&found.hits[0])
        );
    }

    #[test]
    fn no_match_is_a_failure_note_that_says_why_when_the_search_was_partial() {
        let none = note(locus_search_outcome(&results(Vec::new()), "PBRM1 "));
        assert_eq!(
            none,
            LocusSearchNote {
                query: "PBRM1".to_owned(),
                text: "No annotation matches for PBRM1".to_owned(),
                failure: true,
            }
        );
        let partial = TrackSearchResults {
            index_truncated: true,
            ..results(Vec::new())
        };
        assert_eq!(
            note(locus_search_outcome(&partial, "PBRM1")).text,
            "No annotation matches for PBRM1; gene search covers only part of an open track \
             (see Notifications)"
        );
        let bounded = TrackSearchResults {
            scan_truncated: true,
            ..results(Vec::new())
        };
        assert!(
            note(locus_search_outcome(&bounded, "PBRM1"))
                .text
                .ends_with("the search was bounded, so a longer name may find it")
        );
    }

    #[test]
    fn several_matches_are_a_neutral_hint_to_choose() {
        let ambiguous = results(vec![hit("pbrm1", 10), hit("pbrm1", 900)]);
        assert_eq!(
            note(locus_search_outcome(&ambiguous, "PBRM1")),
            LocusSearchNote {
                query: "PBRM1".to_owned(),
                text: "2 matches for PBRM1; choose one from the list".to_owned(),
                failure: false,
            }
        );
    }

    #[test]
    fn no_searchable_track_is_a_failure_note() {
        let note = no_searchable_tracks_note(" PBRM1");
        assert_eq!(note.query, "PBRM1");
        assert_eq!(note.text, NO_SEARCHABLE_TRACKS);
        assert!(note.failure);
    }

    #[test]
    fn the_note_shows_only_while_the_box_holds_its_query() {
        let note = no_searchable_tracks_note("PBRM1");
        assert_eq!(visible_locus_search_note(Some(&note), "PBRM1"), Some(&note));
        assert_eq!(
            visible_locus_search_note(Some(&note), " pbrm1 "),
            Some(&note)
        );
        // Edited, or rewritten by navigation.
        assert_eq!(visible_locus_search_note(Some(&note), "PBRM"), None);
        assert_eq!(visible_locus_search_note(Some(&note), "chr3:1-100"), None);
        assert_eq!(visible_locus_search_note(None, "PBRM1"), None);
    }
}
