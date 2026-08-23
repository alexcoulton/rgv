use super::super::*;

impl Viewer {
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
                self.panels.active_mut().selected_track =
                    Some(TrackFrameKey::Auxiliary(hit.track_id));
                self.locus_editor = locus;
                self.data_status = format!(
                    "{} · {} · {}",
                    hit.record.name.as_deref().unwrap_or(&hit.matched_term),
                    hit.track_name,
                    hit.record.feature_type.as_deref().unwrap_or("feature")
                );
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn navigate_to_loci(&mut self, expression: &str) -> Result<()> {
        let loci = split_locus_expression(expression)?;
        anyhow::ensure!(
            self.canvas_geometry.genomic.width() / loci.len() as f32 >= 140.0,
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
        if let Some(interval) = self.panels.active_mut().navigation_history.back() {
            self.navigate_to_history_interval(interval);
        }
    }

    pub(crate) fn navigate_history_forward(&mut self) {
        if let Some(interval) = self.panels.active_mut().navigation_history.forward() {
            self.navigate_to_history_interval(interval);
        }
    }

    pub(crate) fn navigate_home(&mut self) {
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
