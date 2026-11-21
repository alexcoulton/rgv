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
use rgv_core::{
    ContigAliases, ContigId, GenomicInterval, NavigationEntry, ReferenceDictionary, Viewport,
};
use rgv_engine::TrackSearchHit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LociNavigationPlan {
    /// Move only the active panel; every other panel keeps its locus.
    RetargetActivePanel,
    /// Lay the workspace out as exactly the listed loci, one panel each.
    ReplaceWorkspacePanels,
}

/// Entering a single locus navigates the active panel. Only an explicit multi-locus expression
/// redefines the set of panels.
pub(crate) const fn loci_navigation_plan(locus_count: usize) -> LociNavigationPlan {
    if locus_count <= 1 {
        LociNavigationPlan::RetargetActivePanel
    } else {
        LociNavigationPlan::ReplaceWorkspacePanels
    }
}

/// The active panel's contig, used to revisit history without a dictionary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryPanelContig<'a> {
    pub(crate) id: ContigId,
    pub(crate) name: &'a str,
    pub(crate) length: u64,
}

/// Re-expresses a history entry for the panel: interval, contig name, length.
///
/// With a dictionary the contig is found by *name*, so the entry lands on the
/// contig the user visited however that dictionary numbers it. Without one
/// every contig shares the placeholder id and only the name tells them apart;
/// a contig other than the current one gets the same provisional length a
/// typed standalone locus does.
pub(crate) fn resolve_history_entry(
    entry: &NavigationEntry,
    dictionary: Option<&ReferenceDictionary>,
    aliases: &ContigAliases,
    current: HistoryPanelContig<'_>,
) -> Result<(GenomicInterval, String, u64)> {
    if let Some(dictionary) = dictionary {
        let interval = entry.resolve(dictionary, aliases).with_context(|| {
            format!(
                "{}:{}-{} is not on the open reference",
                entry.contig_name,
                entry.start.saturating_add(1),
                entry.end
            )
        })?;
        let contig = dictionary
            .get(interval.contig())
            .context("reference dictionary is inconsistent")?;
        return Ok((interval, contig.name().to_owned(), contig.length()));
    }
    let interval = entry
        .interval(current.id)
        .context("navigation history entry is empty")?;
    let length = if entry.contig_name == current.name {
        current.length.max(entry.end)
    } else {
        entry
            .end
            .saturating_mul(2)
            .max(entry.end.saturating_add(1_000_000))
    };
    Ok((interval, entry.contig_name.clone(), length))
}

impl Viewer {
    /// Moves the focused panel to an explicit viewport, possibly on another contig.
    pub(crate) fn navigate_to_viewport(
        &mut self,
        viewport: Viewport,
        contig_name: String,
        contig_length: u64,
    ) {
        let panel_id = self.panels.active().id;
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
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref());
        locus_expression_is_navigable(expression, dictionary, &self.contig_aliases)
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
        let Some(engine) = &self.track_engine else {
            "No searchable annotation tracks are open".clone_into(&mut self.data_status);
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
        let Some(engine) = &self.track_engine else {
            "No searchable annotation tracks are open".clone_into(&mut self.data_status);
            return;
        };
        self.annotation_navigation_generation =
            self.annotation_navigation_generation.saturating_add(1);
        let interval = self.panels.active().viewport.interval();
        let position = if forward {
            interval.end()
        } else {
            interval.start()
        };
        if let Err(error) = engine.try_adjacent_feature(
            self.annotation_navigation_generation,
            self.panels.active_mut().contig_name.clone(),
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
            self.data_status = format!("No annotation matches for {query}");
        } else {
            self.data_status = format!(
                "{} annotation matches for {query}; choose one from the search results",
                self.annotation_search_results.hits.len()
            );
        }
    }

    pub(crate) fn navigate_to_annotation_hit(&mut self, hit: &TrackSearchHit) {
        let locus = annotation_search_hit_locus(hit);
        match self.navigate_to_locus(self.panels.active().id, &locus) {
            Ok(()) => {
                replace_panel_track_selection(
                    self.panels.active_mut(),
                    TrackFrameKey::Auxiliary(hit.track_id),
                    ui::TrackPane::Annotation,
                );
                self.locus_editor = locus;
                self.data_status = format!(
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
        let action = crate::action::translate::locus_box(self.panels.active().id, expression)?;
        self.apply_action(action).map(|_| ())
    }

    pub(crate) fn navigate_to_chain_segment(&mut self, index: usize) -> Result<()> {
        let segment = self
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
        self.navigate_to_locus(self.panels.active().id, &locus)?;
        if let Some(chain) = &mut self.panels.active_mut().selected_chain {
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
        let panel_id = self.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateHistory { panel_id, step }) {
            self.report_error(error.to_string());
        }
    }

    pub(crate) fn navigate_home(&mut self) {
        let panel_id = self.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateHome { panel_id }) {
            self.report_error(error.to_string());
        }
    }

    /// Opens the first 100 kb of a contig chosen from the assembly's list.
    pub(crate) fn navigate_to_contig(&mut self, contig_id: ContigId) {
        let Some(contig) = self.assembly.dictionary.get(contig_id) else {
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
        let panel_id = self.panels.active().id;
        if let Err(error) = self.apply_action(Action::NavigateToEntry {
            panel_id,
            entry,
            record: true,
        }) {
            self.report_error(error.to_string());
        }
    }

    pub(crate) fn remember_current_locus(&mut self, panel_id: PanelId) {
        let interval = self.panels.panel(panel_id).viewport.interval();
        let locus = format!(
            "{}:{}-{}",
            self.panels.panel(panel_id).contig_name,
            interval.start() + 1,
            interval.end()
        );
        // The locus box is a focused-panel control; another panel's move must not overwrite it.
        if self.panels.is_active(panel_id) {
            self.locus_editor.clone_from(&locus);
        }
        self.settings.remember_locus(locus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::navigation::{resolve_initial_locus, standalone_locus};
    use crate::workspace::panels::ViewPanelController;
    use rgv_core::PanelId;

    /// A panel opened at `locus`, as `navigate_to_locus` leaves it.
    fn panel_at(dictionary: Option<&ReferenceDictionary>, locus: &str) -> ViewPanelController {
        let (interval, name, length) = dictionary.map_or_else(
            || standalone_locus(locus).unwrap(),
            |dictionary| {
                resolve_initial_locus(dictionary, &ContigAliases::default(), Some(locus)).unwrap()
            },
        );
        ViewPanelController::new(
            PanelId::new(1),
            Viewport::new(interval, 1_000.0).unwrap(),
            None,
            name,
            length,
        )
    }

    /// The pure part of `navigate_to_locus`.
    fn visit(
        panel: &mut ViewPanelController,
        dictionary: Option<&ReferenceDictionary>,
        locus: &str,
    ) {
        let next = panel_at(dictionary, locus);
        panel.viewport = next.viewport;
        panel.contig_name = next.contig_name;
        panel.contig_length = next.contig_length;
        panel.visit_history(panel.viewport.interval());
    }

    fn revisit(
        panel: &ViewPanelController,
        entry: &NavigationEntry,
        dictionary: Option<&ReferenceDictionary>,
    ) -> (GenomicInterval, String, u64) {
        resolve_history_entry(
            entry,
            dictionary,
            &ContigAliases::default(),
            HistoryPanelContig {
                id: panel.viewport.interval().contig(),
                name: &panel.contig_name,
                length: panel.contig_length,
            },
        )
        .unwrap()
    }

    #[test]
    fn without_a_dictionary_two_contigs_are_two_entries_and_back_returns_to_the_first() {
        // APP-2: every standalone contig is id zero, so interval-keyed history
        // collapsed chr1:1-100000 and chr2:1-100000 into one entry.
        let mut panel = panel_at(None, "chr1:1-100000");
        visit(&mut panel, None, "chr2:1-100000");
        assert_eq!(panel.navigation_history.len(), 2);
        assert!(panel.navigation_history.can_go_back());
        assert_eq!(panel.contig_name, "chr2");

        let entry = panel.navigation_history.back().cloned().unwrap();
        let (interval, name, _) = revisit(&panel, &entry, None);
        assert_eq!(name, "chr1");
        assert_eq!((interval.start(), interval.end()), (0, 100_000));

        // Revisiting the same locus on the same contig is still one entry.
        let mut panel = panel_at(None, "chr1:1-100000");
        visit(&mut panel, None, "chr1:1-100000");
        assert_eq!(panel.navigation_history.len(), 1);
    }

    #[test]
    fn back_lands_on_the_named_contig_when_dictionaries_are_ordered_differently() {
        let assembly = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 1_000_000),
            ("chr2".to_owned(), 900_000),
        ]);
        let alignment = ReferenceDictionary::from_contigs([
            ("chr2".to_owned(), 900_000),
            ("chr1".to_owned(), 1_000_000),
        ]);
        let mut panel = panel_at(Some(&alignment), "chr1:1-100000");
        let visited = panel.viewport.interval();
        visit(&mut panel, Some(&alignment), "chr2:1-100000");

        let entry = panel.navigation_history.back().cloned().unwrap();
        let (interval, name, length) = revisit(&panel, &entry, Some(&alignment));
        assert_eq!((name.as_str(), length), ("chr1", 1_000_000));
        assert_eq!(interval, visited);
        // The defect: the visited id read through the assembly names chr2.
        assert_eq!(assembly.get(visited.contig()).unwrap().name(), "chr2");

        // The same entry re-expressed for a differently ordered dictionary
        // (the source was swapped meanwhile) still names chr1.
        let (interval, name, _) = revisit(&panel, &entry, Some(&assembly));
        assert_eq!(name, "chr1");
        assert_eq!(interval.contig(), assembly.id_by_name("chr1").unwrap());

        let gone = NavigationEntry {
            contig_name: "chr9".to_owned(),
            start: 0,
            end: 10,
        };
        assert!(
            resolve_history_entry(
                &gone,
                Some(&alignment),
                &ContigAliases::default(),
                HistoryPanelContig {
                    id: ContigId::new(0),
                    name: "chr2",
                    length: 900_000,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn a_single_locus_never_replaces_the_other_panels() {
        assert_eq!(
            loci_navigation_plan(1),
            LociNavigationPlan::RetargetActivePanel
        );
        assert_eq!(
            loci_navigation_plan(2),
            LociNavigationPlan::ReplaceWorkspacePanels
        );
        assert_eq!(
            loci_navigation_plan(5),
            LociNavigationPlan::ReplaceWorkspacePanels
        );
    }
}
