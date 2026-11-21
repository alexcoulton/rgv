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
        panel.visit_history(interval);
        self.viewport_changed();
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
        self.panels.active_mut().visit_history(interval);
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
            Err(error) => self.report_error(error.to_string()),
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

    /// The dictionary panel viewports are expressed in: the selected (else
    /// primary) alignment header, else the reference. `None` for a workspace
    /// of standalone tracks.
    pub(crate) fn navigation_dictionary(&self) -> Option<&ReferenceDictionary> {
        self.panels
            .active()
            .selected_alignment_track
            .and_then(|id| self.alignment_dictionary_for_track(id))
            .or_else(|| self.primary_alignment_dictionary())
            .or(self.reference_dictionary.as_ref())
    }

    pub(crate) fn navigate_history_back(&mut self) {
        // Record an unsettled gesture first, so Back returns to where it started.
        self.commit_navigation_gesture();
        if let Some(entry) = self.panels.active_mut().navigation_history.back().cloned() {
            self.navigate_to_history_entry_reporting(&entry);
        }
    }

    pub(crate) fn navigate_history_forward(&mut self) {
        self.commit_navigation_gesture();
        if let Some(entry) = self
            .panels
            .active_mut()
            .navigation_history
            .forward()
            .cloned()
        {
            self.navigate_to_history_entry_reporting(&entry);
        }
    }

    pub(crate) fn navigate_home(&mut self) {
        self.commit_navigation_gesture();
        let home = self.panels.active().home.clone();
        self.panels
            .active_mut()
            .navigation_history
            .visit(home.clone());
        self.navigate_to_history_entry_reporting(&home);
    }

    fn navigate_to_history_entry_reporting(&mut self, entry: &NavigationEntry) {
        if let Err(error) = self.navigate_to_history_entry(entry) {
            self.report_error(error.to_string());
        }
    }

    /// Moves the active panel to a history entry without recording a visit,
    /// re-resolving the entry's contig by name.
    pub(crate) fn navigate_to_history_entry(
        &mut self,
        entry: &NavigationEntry,
    ) -> Result<GenomicInterval> {
        let panel = self.panels.active();
        let (interval, contig_name, contig_length) = resolve_history_entry(
            entry,
            self.navigation_dictionary(),
            &self.contig_aliases,
            HistoryPanelContig {
                id: panel.viewport.interval().contig(),
                name: &panel.contig_name,
                length: panel.contig_length,
            },
        )?;
        let viewport = Viewport::new(interval, panel.viewport.pixel_width())?;
        let panel = self.panels.active_mut();
        panel.contig_name = contig_name;
        panel.contig_length = contig_length;
        panel.viewport = viewport;
        self.viewport_changed();
        Ok(interval)
    }

    /// Opens the first 100 kb of a contig chosen from the assembly's list.
    pub(crate) fn navigate_to_contig(&mut self, contig_id: ContigId) {
        self.commit_navigation_gesture();
        let Some(contig) = self.assembly.dictionary.get(contig_id) else {
            return;
        };
        let length = contig.length();
        if length == 0 {
            return;
        }
        // The assembly numbers contigs independently of the navigation
        // dictionary, so carry the choice across by name.
        let entry = NavigationEntry {
            contig_name: contig.name().to_owned(),
            start: 0,
            end: length.min(100_000),
        };
        match self.navigate_to_history_entry(&entry) {
            Ok(interval) => self.panels.active_mut().visit_history(interval),
            Err(error) => self.report_error(error.to_string()),
        }
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
