//! Deferred navigation, export, settings, file and cache requests.

use super::*;

impl Viewer {
    pub(super) fn apply_locus_search_and_panel_changes(&mut self, st: &mut ShellFrame) {
        self.locus_editor = std::mem::take(&mut st.nav.locus_editor);
        if st.nav.locus_search_changed
            && !st.nav.navigate_requested
            && self.locus_editor.trim().chars().count() >= 2
            && !self.locus_expression_is_coordinates(&self.locus_editor)
        {
            let query = self.locus_editor.clone();
            self.request_annotation_search(&query, false, false);
        }
        if let Some((id, index)) = st.nav.panel_to_move {
            self.move_view_panel(id, index);
        }
        if let Some(id) = st.nav.panel_to_close {
            self.close_view_panel(id);
        } else if let Some(id) = st.nav.panel_to_activate {
            self.activate_view_panel(id);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_inspector_exports(&mut self, st: &mut ShellFrame) {
        if st.inspect.variant_genotypes_export_requested
            && let Some(inspector) = &st.snapshot.inspectors.variant_inspector
        {
            let record = &inspector.query.records[inspector.record_index];
            let file_name = variant_export_file_name(record, "variant-genotypes");
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(file_name)
                .add_filter("Tab-separated values", &["tsv"])
                .save_file()
            {
                self.start_export(path, variant_genotype_export_tsv(inspector));
            }
        }
        if st.inspect.variant_evidence_export_requested
            && let Some(inspector) = &st.snapshot.inspectors.variant_inspector
        {
            let record = &inspector.query.records[inspector.record_index];
            let file_name = variant_export_file_name(record, "alignment-evidence");
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(file_name)
                .add_filter("Tab-separated values", &["tsv"])
                .save_file()
            {
                self.start_export(
                    path,
                    variant_evidence_export_tsv(
                        inspector,
                        &st.snapshot.inspectors.variant_alignment_evidence,
                    ),
                );
            }
        }
        if st.inspect.junction_bed_export_requested {
            let bundle = self.sashimi_export_bundle();
            let selected_id = self
                .panels
                .active()
                .selected_alignment_track
                .unwrap_or_else(|| self.primary_alignment_track_id());
            if let Some(track) = bundle
                .tracks
                .iter()
                .find(|track| track.id == selected_id)
                .cloned()
                .or_else(|| bundle.tracks.first().cloned())
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name(rna_export_file_name(&track.name, "junctions", "bed"))
                    .add_filter("BED", &["bed"])
                    .save_file()
            {
                let contig = bundle.contig_name;
                let viewport = bundle.viewport.interval();
                let annotations = bundle.annotations;
                self.start_export_job(path, move |path| {
                    std::fs::write(
                        path,
                        junction_bed_export(&track, &contig, viewport, &annotations),
                    )
                });
            }
        }
        if st.inspect.sashimi_png_export_requested {
            let bundle = self.sashimi_export_bundle();
            if !bundle.tracks.is_empty()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("rgv-sashimi.png")
                    .add_filter("PNG image", &["png"])
                    .save_file()
            {
                let scale =
                    crate::automation::export::interactive_export_scale(self.window.scale_factor());
                self.start_export_job(path, move |path| {
                    let (display, width, height) = sashimi_export_display(&bundle);
                    export_png_scaled(&display, width, height, scale, path)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                });
            }
        }
        if st.inspect.sashimi_svg_export_requested {
            let bundle = self.sashimi_export_bundle();
            if !bundle.tracks.is_empty()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("rgv-sashimi.svg")
                    .add_filter("SVG image", &["svg"])
                    .save_file()
            {
                self.start_export_job(path, move |path| {
                    let (display, width, height) = sashimi_export_display(&bundle);
                    std::fs::write(path, sashimi_svg_export(&display, width, height, &bundle))
                });
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_navigation_requests(&mut self, st: &mut ShellFrame) {
        if let Some((track_id, position)) = st.inspect.variant_pileup_requested {
            self.select_variant_alignment_evidence(track_id, position, false);
        } else if let Some((track_id, position)) = st.inspect.variant_modification_requested {
            self.select_variant_alignment_evidence(track_id, position, true);
        }
        if let Some((contig, position)) = st
            .inspect
            .variant_partner_requested
            .take()
            .or(st.inspect.variant_endpoint_requested.take())
            .or(st.inspect.interaction_endpoint_requested.take())
        {
            if let Err(error) = self.open_genomic_position_in_new_panel(&contig, position) {
                self.report_error(error.to_string());
            }
        } else if st.inspect.mate_panel_requested {
            if let Err(error) = self.open_selected_mate_in_new_panel() {
                self.report_error(error.to_string());
            }
        } else if st.inspect.chain_all_panels_requested {
            match self.open_all_chain_segments_in_panels() {
                Ok((opened, requested)) if opened < requested => {
                    self.data_status = format!(
                        "Opened {opened} of {requested} supplementary segments; the {MAXIMUM_VIEW_PANELS}-panel workspace cap was reached"
                    );
                }
                Ok((opened, _)) => {
                    self.data_status = format!("Opened {opened} supplementary segment panels");
                }
                Err(error) => self.report_error(error.to_string()),
            }
        } else if let Some(index) = st.inspect.chain_panel_requested {
            if let Err(error) = self.open_chain_segment_in_new_panel(index) {
                self.report_error(error.to_string());
            }
        } else if let Some(index) = st.inspect.chain_navigation_requested {
            if let Err(error) = self.navigate_to_chain_segment(index) {
                self.report_error(error.to_string());
            }
        } else if let Some(contig) = st.nav.requested_contig {
            self.navigate_to_contig(contig);
        } else if st.nav.navigate_back_requested {
            self.navigate_history_back();
        } else if st.nav.navigate_forward_requested {
            self.navigate_history_forward();
        } else if st.nav.navigate_home_requested {
            self.navigate_home();
        } else if st.nav.zoom_in_requested {
            self.keyboard_zoom(0.8);
        } else if st.nav.zoom_out_requested {
            self.keyboard_zoom(1.25);
        } else if let Some(span) = st.nav.requested_span {
            let factor = span as f64 / self.panels.active().viewport.span();
            if let Ok(viewport) = self.panels.active_mut().viewport.zoom_at(
                self.panels.active_mut().viewport.pixel_width() * 0.5,
                factor,
                self.panels.active_mut().contig_length,
            ) {
                // A slider drag reports a new span every frame; it is one history entry.
                self.panels.active_mut().viewport = viewport;
                self.viewport_step_changed();
            }
        }
        if let Some(hit) = st.nav.annotation_hit_requested.take() {
            self.navigate_to_annotation_hit(&hit);
        }
        if st.nav.previous_feature_requested {
            self.request_adjacent_feature(false);
        } else if st.nav.next_feature_requested {
            self.request_adjacent_feature(true);
        }
        if st.nav.navigate_requested {
            let locus = self.locus_editor.clone();
            if self.locus_expression_is_coordinates(&locus) {
                if let Err(error) = self.navigate_to_loci(&locus) {
                    self.report_error(error.to_string());
                } else {
                    self.persist_settings();
                }
            } else if self
                .annotation_search_results
                .query
                .trim()
                .eq_ignore_ascii_case(locus.trim())
                && self.annotation_search_results.variant_ids_included
            {
                self.submit_annotation_search_results(&locus);
            } else {
                self.request_annotation_search(&locus, true, true);
            }
        }
    }

    pub(super) fn apply_settings_changes(&mut self, st: &mut ShellFrame) {
        let autosave_schedule_changed = self.settings.autosave_enabled
            != st.commands.autosave_enabled
            || self.settings.autosave_interval_seconds != st.commands.autosave_interval_seconds;
        let accessibility_changed = self.settings.interface_scale_percent
            != st.commands.interface_scale_percent
            || self.settings.high_contrast != st.commands.high_contrast
            || self.settings.reduced_motion != st.commands.reduced_motion;
        let read_browsing_buffer_changed =
            self.settings.read_browsing_buffer_percent != st.commands.read_browsing_buffer_percent;
        let cache_budget_changed = self.settings.cache_budget_mib != st.commands.cache_budget_mib;
        let settings_changed = cache_budget_changed
            || read_browsing_buffer_changed
            || self.settings.diagnostics_opt_in != st.commands.diagnostics_opt_in
            || autosave_schedule_changed
            || self.settings.autosave_retention != st.commands.autosave_retention
            || accessibility_changed;
        self.settings.cache_budget_mib = st.commands.cache_budget_mib;
        self.settings.read_browsing_buffer_percent = st.commands.read_browsing_buffer_percent;
        self.settings.diagnostics_opt_in = st.commands.diagnostics_opt_in;
        self.settings.autosave_enabled = st.commands.autosave_enabled;
        self.settings.autosave_interval_seconds = st.commands.autosave_interval_seconds;
        self.settings.autosave_retention = st.commands.autosave_retention;
        self.settings.interface_scale_percent = st.commands.interface_scale_percent;
        self.settings.high_contrast = st.commands.high_contrast;
        self.settings.reduced_motion = st.commands.reduced_motion;
        if accessibility_changed {
            apply_accessibility_preferences(&self.egui_context, &self.settings);
            self.mark_display_dirty("explicit display invalidation");
        }
        if autosave_schedule_changed {
            self.schedule_next_autosave();
        }
        if cache_budget_changed {
            self.apply_live_cache_budget(st.commands.cache_budget_mib);
        }
        if read_browsing_buffer_changed || cache_budget_changed {
            self.request_all_panel_queries();
        }
        if st.commands.clear_recent_requested {
            self.settings.clear_recent();
        }
        if settings_changed || st.commands.clear_recent_requested {
            self.persist_settings();
        }
        if let Some(source) = st.commands.recent_alignment_requested.take() {
            self.open_alignment_source(&source_path(&source));
        }
        if let Some(path) = st.commands.recent_session_requested.take() {
            self.handle_dropped_file(Path::new(&path));
        }
        if let Some(locus) = st.commands.recent_locus_requested.take()
            && let Err(error) = self.navigate_to_locus(&locus)
        {
            self.report_error(error.to_string());
        }
    }

    /// Open/save/export requests that go through native file dialogs, plus genome requests.
    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_file_requests(&mut self, st: &mut ShellFrame) {
        if st.commands.open_alignment_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Alignment", &["bam", "cram"])
                .pick_file()
        {
            self.open_alignment_source(&path);
        }
        if st.commands.open_reference_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Reference FASTA or 2bit", &["fa", "fasta", "fna", "2bit"])
                .pick_file()
        {
            self.open_reference_source(&path);
        }
        if st.commands.open_genome_config_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("RGV or IGV genome description", &["json", "rgvgenome"])
                .pick_file()
            && let Err(error) = self.open_genome_configuration(&path)
        {
            self.report_error(error.to_string());
        }
        self.hosted_genomes.filter = std::mem::take(&mut st.commands.hosted_genome_filter);
        if st.commands.refresh_hosted_genomes_requested {
            self.start_hosted_genome_list_refresh();
        }
        if let Some(genome) = st.commands.hosted_genome_requested.take() {
            self.start_genome_load(genome);
        }
        if st.commands.restore_default_genes_requested
            && let Err(error) = self.restore_default_genes()
        {
            self.report_error(error.to_string());
        }
        if st.commands.open_landmark_folder_requested
            && let Err(error) = self.open_landmark_resource_folder()
        {
            self.report_error(error.to_string());
        }
        if let Some(bundle_id) = st.inspect.show_all_transcripts_requested.take()
            && let Err(error) = self.show_all_transcripts(&bundle_id)
        {
            self.report_error(error.to_string());
        }
        if let Some(bundle_id) = st.inspect.return_to_representative_genes_requested.take()
            && let Err(error) = self.return_to_representative_genes(&bundle_id)
        {
            self.report_error(error.to_string());
        }
        if st.commands.add_track_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Genomic track",
                    &[
                        "bed",
                        "gff",
                        "gff3",
                        "gtf",
                        "vcf",
                        "bcf",
                        "bw",
                        "bigwig",
                        "tdf",
                        "bedgraph",
                        "bdg",
                        "wig",
                        "wiggle",
                        "bedpe",
                        "interact",
                        "bb",
                        "bigbed",
                        "biggenepred",
                        "bignarrowpeak",
                        "seg",
                        "cbs",
                        "maf",
                        "genepred",
                        "gp",
                        "refflat",
                        "narrowpeak",
                        "broadpeak",
                        "psl",
                        "linear",
                        "logistic",
                        "assoc",
                        "qassoc",
                        "gwas",
                        "gz",
                    ],
                )
                .pick_file()
            && let Err(error) = self.add_track_source(&path)
        {
            self.report_error(error.to_string());
        }
        if let Some(path) = st.commands.recovery_requested.take() {
            self.request_session_load(path, SessionLoadKind::Recovery);
        }
        if st.commands.open_session_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Genome viewer session", &["json", "xml"])
                .pick_file()
        {
            self.handle_dropped_file(&path);
        }
        if st.commands.save_session_requested {
            if let Some(path) = self.current_session_path.clone() {
                self.start_session_save(path);
            } else {
                st.commands.save_session_as_requested = true;
            }
        }
        if st.commands.save_session_as_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-project.json")
                .add_filter("RGV project", &["json"])
                .save_file()
        {
            self.start_session_save(path);
        }
        if st.commands.export_png_requested
            && self
                .panels
                .iter()
                .any(|panel| panel.navigation_presentation.is_unsettled())
        {
            "The target viewport is still being checked; export is available after it settles"
                .clone_into(&mut self.data_status);
        } else if st.commands.export_png_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-view.png")
                .add_filter("PNG image", &["png"])
                .save_file()
        {
            // REND-9: match the display's pixel density instead of one pixel per point.
            let scale =
                crate::automation::export::interactive_export_scale(self.window.scale_factor());
            match self.capture_semantic_export(AutomationSnapshotScope::VisibleWorkspace) {
                Ok(capture) => self.start_export_job(path, move |path| {
                    capture.write(
                        path,
                        AutomationSnapshotFormat::Png,
                        "RGV manual export",
                        scale,
                    )
                }),
                Err(error) => self.report_error(error.to_string()),
            }
        }
    }

    pub(super) fn apply_cache_clear(&mut self, st: &mut ShellFrame) {
        if st.commands.clear_cache_requested {
            self.alignment_presented_epoch = None;
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_publication_retry = None;
            self.alignment_capacity_reclaim = None;
            self.alignment_gpu_renderer.discard_device_resources();
            self.alignment_summary_gpu_renderer
                .discard_device_resources();
            self.context_scene_gpu_renderer.discard_all();
            self.context_scene_gpu_frame = ContextSceneGpuFrame::default();
            self.alignment_gpu_speculation_gate.reset();
            let budget = self
                .settings
                .cache_budget_mib
                .saturating_mul(1024 * 1024)
                .max(1);
            for track in &mut self.alignment_tracks {
                if let Some(engine) = &track.scene_engine {
                    if let Err(error) = engine.try_clear_cache_budget(budget) {
                        crate::app::shell_state::report_error_to(
                            &mut self.shell.notifications,
                            &mut self.data_status,
                            error.to_string(),
                        );
                    } else {
                        track.scene_budget_retry = None;
                    }
                }
            }
        }
        if st.commands.clear_cache_requested {
            // Dropping the native derivative is not itself a navigation transaction. Reissue
            // every current alignment target with a fresh request token so cache clearing cannot
            // leave a formerly committed projection beside an empty presented epoch. The source
            // actor may reuse the exact still-pinned current scene, but all speculative/cache
            // residency is rebuilt under the post-clear budget policy.
            self.request_all_alignment_queries();
            self.mark_display_dirty("explicit display invalidation");
        }
    }
}
