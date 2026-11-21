use super::super::*;

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

impl Viewer {
    pub(crate) fn navigate_to_viewport(
        &mut self,
        viewport: Viewport,
        contig_name: String,
        contig_length: u64,
    ) {
        self.commit_navigation_gesture();
        let interval = viewport.interval();
        let panel = self.panels.active_mut();
        panel.viewport = viewport;
        panel.contig_name = contig_name;
        panel.contig_length = contig_length;
        panel.navigation_history.visit(interval);
        self.viewport_changed();
    }

    /// Whether every whitespace-separated item is a coordinate locus or a known contig name,
    /// so the expression should navigate directly and not be sent to feature search.
    pub(crate) fn locus_expression_is_coordinates(&self, expression: &str) -> bool {
        let dictionary = self
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref());
        let mut items = expression.split_ascii_whitespace().peekable();
        items.peek().is_some()
            && items.all(|item| {
                item.contains(':')
                    || dictionary.is_some_and(|dictionary| {
                        resolve_typed_contig(dictionary, &self.contig_aliases, item)
                            .ok()
                            .flatten()
                            .is_some()
                    })
            })
    }

    pub(crate) fn navigate_to_locus(&mut self, locus: &str) -> Result<()> {
        let (interval, contig_name, contig_length) = if let Some(dictionary) = self
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref())
        {
            resolve_initial_locus(dictionary, &self.contig_aliases, Some(locus))?
        } else {
            standalone_locus(locus)?
        };
        self.commit_navigation_gesture();
        self.panels.active_mut().viewport =
            Viewport::new(interval, self.panels.active().viewport.pixel_width())?;
        self.panels.active_mut().contig_name = contig_name;
        self.panels.active_mut().contig_length = contig_length;
        self.panels.active_mut().navigation_history.visit(interval);
        self.viewport_changed();
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
                self.data_status = error.to_string();
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
            self.data_status = error.to_string();
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
        match self.navigate_to_locus(&locus) {
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
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn navigate_to_loci(&mut self, expression: &str) -> Result<()> {
        let loci = split_locus_expression(expression)?;
        if loci_navigation_plan(loci.len()) == LociNavigationPlan::RetargetActivePanel {
            // One locus is a navigation of the panel being looked at; the other panels and
            // their loci are left alone.
            return self.navigate_to_locus(loci[0]);
        }
        anyhow::ensure!(
            self.workspace_layout.projected_canvas_width(loci.len()) >= 140.0,
            "the window is too narrow to display {} readable panels",
            loci.len()
        );

        // Validate the complete expression before replacing any current panel.
        let dictionary = self
            .panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref());
        for locus in &loci {
            if let Some(dictionary) = dictionary {
                resolve_initial_locus(dictionary, &self.contig_aliases, Some(locus))?;
            } else {
                standalone_locus(locus)?;
            }
        }

        let first_panel_id = self.panels.active().id;
        self.navigate_to_locus(loci[0])?;
        let obsolete = self
            .panels
            .iter()
            .map(|panel| panel.id)
            .filter(|id| *id != first_panel_id)
            .collect::<Vec<_>>();
        for id in obsolete {
            self.close_view_panel(id);
        }
        for locus in loci.iter().skip(1) {
            self.add_view_panel(locus)?;
        }
        self.activate_view_panel(first_panel_id);
        self.synchronize_panel_widths();
        self.request_all_panel_queries();
        Ok(())
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
        self.navigate_to_locus(&locus)?;
        if let Some(chain) = &mut self.panels.active_mut().selected_chain {
            chain.active_segment = index;
        }
        Ok(())
    }

    pub(crate) fn navigate_history_back(&mut self) {
        // Record an unsettled gesture first, so Back returns to where it started.
        self.commit_navigation_gesture();
        if let Some(interval) = self.panels.active_mut().navigation_history.back() {
            self.navigate_to_history_interval(interval);
        }
    }

    pub(crate) fn navigate_history_forward(&mut self) {
        self.commit_navigation_gesture();
        if let Some(interval) = self.panels.active_mut().navigation_history.forward() {
            self.navigate_to_history_interval(interval);
        }
    }

    pub(crate) fn navigate_home(&mut self) {
        self.commit_navigation_gesture();
        let home = self.panels.active().home_interval;
        self.panels.active_mut().navigation_history.visit(home);
        self.navigate_to_history_interval(home);
    }

    pub(crate) fn navigate_to_history_interval(&mut self, interval: GenomicInterval) {
        if let Some(contig) = self.assembly.dictionary.get(interval.contig()) {
            self.panels.active_mut().contig_name = contig.name().to_owned();
            self.panels.active_mut().contig_length = contig.length();
        }
        match Viewport::new(interval, self.panels.active_mut().viewport.pixel_width()) {
            Ok(viewport) => {
                self.panels.active_mut().viewport = viewport;
                self.viewport_changed();
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn navigate_to_contig(&mut self, contig_id: ContigId) {
        self.commit_navigation_gesture();
        let Some(contig) = self.assembly.dictionary.get(contig_id) else {
            return;
        };
        let length = contig.length();
        if length == 0 {
            return;
        }
        let interval = GenomicInterval::new(contig_id, 0, length.min(100_000))
            .expect("a non-empty contig produces a valid default interval");
        self.panels.active_mut().contig_name = contig.name().to_owned();
        self.panels.active_mut().contig_length = length;
        self.panels.active_mut().navigation_history.visit(interval);
        self.navigate_to_history_interval(interval);
    }

    pub(crate) fn remember_current_locus(&mut self) {
        let interval = self.panels.active().viewport.interval();
        let locus = format!(
            "{}:{}-{}",
            self.panels.active().contig_name,
            interval.start() + 1,
            interval.end()
        );
        self.locus_editor.clone_from(&locus);
        self.settings.remember_locus(locus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
