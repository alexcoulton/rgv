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
use rgv_engine::TrackSearchHit;

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
        })
        .map(|_| ())
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
            "No searchable annotation tracks are open".clone_into(&mut self.session.data_status);
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
            "No searchable annotation tracks are open".clone_into(&mut self.session.data_status);
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

    pub(crate) fn submit_annotation_search_results(&mut self, query: &str) {
        if let Some(hit) =
            preferred_annotation_search_hit(&self.annotation_search_results, query).cloned()
        {
            self.navigate_to_annotation_hit(&hit);
        } else if self.annotation_search_results.hits.is_empty() {
            self.session.data_status = format!("No annotation matches for {query}");
        } else {
            self.session.data_status = format!(
                "{} annotation matches for {query}; choose one from the search results",
                self.annotation_search_results.hits.len()
            );
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
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    /// The locus box and `goto`: one locus retargets the focused panel; several redefine the
    /// workspace as exactly those loci.
    pub(crate) fn navigate_to_loci(&mut self, expression: &str) -> Result<()> {
        let action =
            crate::action::translate::locus_box(self.session.panels.active().id, expression)?;
        self.apply_action(action).map(|_| ())
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
