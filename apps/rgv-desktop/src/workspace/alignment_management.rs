use super::super::*;

// A scheduling quantum, not a residency cap. Prepared pages continue across redraws until the
// complete immutable scene is warm or the shared ledger declines the next batch.
const ALIGNMENT_GPU_PREFETCH_PAGES_PER_FRAME: usize = 6;

impl Viewer {
    pub(crate) fn apply_live_cache_budget(&mut self, cache_budget_mib: usize) {
        let total_bytes = cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        self.cache_coordinator.total_budget_bytes = total_bytes;
        self.rebalance_live_cache_coordinator();
    }

    /// Reconciles every currently measurable alignment-scene tier against one
    /// user-selected ceiling. Each tier may borrow all presently unused
    /// capacity; decoded engines divide their pool from observed residency
    /// rather than retaining a startup-time equal split.
    pub(crate) fn rebalance_live_cache_coordinator(&mut self) {
        let total_budget_bytes = self
            .settings
            .cache_budget_mib
            .saturating_mul(1024 * 1024)
            .max(1);
        self.cache_coordinator.total_budget_bytes = total_budget_bytes;
        if let Err(error) = self.alignment_scene_ledger.set_budget(total_budget_bytes) {
            self.data_status = format!("Could not apply alignment-scene budget: {error}");
        }
        if let Err(error) = self
            .alignment_scene_ledger
            .set_transient_budget(total_budget_bytes)
        {
            self.data_status = format!("Could not apply alignment working-memory budget: {error}");
        }

        let mut layout_resident_bytes = self.layout_cache.stats().resident_weight;
        let mut row_scene_resident_bytes = self.row_scene_resident_bytes();
        let mut gpu_slab_resident_bytes = self
            .rectangle_renderer
            .last_prepare_stats()
            .slab_resident_bytes;

        // Derived state alone can exceed a newly lowered ceiling. Retire only
        // inactive/LRU objects here; active layout and GPU state remain held by
        // their presentation owners even if their cache entries are removed.
        let derived_resident_bytes = layout_resident_bytes
            .saturating_add(row_scene_resident_bytes)
            .saturating_add(gpu_slab_resident_bytes);
        if derived_resident_bytes > total_budget_bytes {
            let layout_ceiling = total_budget_bytes
                .saturating_sub(row_scene_resident_bytes)
                .saturating_sub(gpu_slab_resident_bytes)
                .max(1);
            self.layout_cache.set_maximum_weight(
                NonZeroUsize::new(layout_ceiling).expect("layout ceiling is non-zero"),
            );
            layout_resident_bytes = self.layout_cache.stats().resident_weight;

            let gpu_ceiling = total_budget_bytes
                .saturating_sub(row_scene_resident_bytes)
                .saturating_sub(layout_resident_bytes)
                .max(1);
            self.rectangle_renderer.set_maximum_slab_bytes(gpu_ceiling);
            gpu_slab_resident_bytes = self
                .rectangle_renderer
                .last_prepare_stats()
                .slab_resident_bytes;

            let row_ceiling = total_budget_bytes
                .saturating_sub(layout_resident_bytes)
                .saturating_sub(gpu_slab_resident_bytes);
            self.trim_inactive_row_scenes(row_ceiling);
            row_scene_resident_bytes = self.row_scene_resident_bytes();
        }

        let non_decoded_resident_bytes = layout_resident_bytes
            .saturating_add(row_scene_resident_bytes)
            .saturating_add(gpu_slab_resident_bytes);
        let decoded_allowance_bytes = total_budget_bytes
            .saturating_sub(non_decoded_resident_bytes)
            .max(self.alignment_tracks.len().max(1));
        let mut first_error = None;
        let allowance =
            NonZeroUsize::new(total_budget_bytes).expect("the selected scene budget is non-zero");
        for track in &mut self.alignment_tracks {
            if track.cache_budget_bytes == allowance {
                continue;
            }
            if let Some(engine) = &track.scene_engine {
                match engine.try_reduce_budget(total_budget_bytes) {
                    Ok(()) => track.cache_budget_bytes = allowance,
                    Err(error) => {
                        first_error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
        }

        // When current residency fits, every derived tier can borrow the same
        // free headroom. A growth is observed and rebalanced before the next
        // display admission. Under pressure, current derived state is pinned
        // while decoded actors first discard speculative margins.
        let decoded_resident_bytes = self.decoded_cache_resident_bytes();
        let measured_resident_bytes = decoded_resident_bytes
            .saturating_add(layout_resident_bytes)
            .saturating_add(row_scene_resident_bytes)
            .saturating_add(gpu_slab_resident_bytes);
        let free_bytes = total_budget_bytes.saturating_sub(measured_resident_bytes);
        let layout_allowance_bytes = layout_resident_bytes.saturating_add(free_bytes).max(1);
        let row_scene_allowance_bytes = row_scene_resident_bytes.saturating_add(free_bytes).max(1);
        let gpu_slab_allowance_bytes = gpu_slab_resident_bytes.saturating_add(free_bytes).max(1);
        self.layout_cache.set_maximum_weight(
            NonZeroUsize::new(layout_allowance_bytes).expect("layout allowance is non-zero"),
        );
        self.rectangle_renderer
            .set_maximum_slab_bytes(gpu_slab_allowance_bytes);
        self.cache_budgets = CacheBudgets {
            remote_blocks: 0,
            decoded_alignments: decoded_allowance_bytes,
            coverage_summaries: 0,
            layouts: layout_allowance_bytes,
            gpu_buffers: gpu_slab_allowance_bytes,
        };
        self.cache_coordinator = LiveCacheCoordinatorSnapshot {
            total_budget_bytes,
            measured_resident_bytes,
            decoded_resident_bytes,
            layout_resident_bytes,
            row_scene_resident_bytes,
            gpu_slab_resident_bytes,
            decoded_allowance_bytes,
            layout_allowance_bytes,
            row_scene_allowance_bytes,
            gpu_slab_allowance_bytes,
            over_budget_bytes: measured_resident_bytes.saturating_sub(total_budget_bytes),
            clamp_reason: (measured_resident_bytes > total_budget_bytes)
                .then_some("decoded eviction settling or authoritative foreground pinned"),
            rebalances: self.cache_coordinator.rebalances.saturating_add(1),
        };
        if let Some(error) = first_error {
            self.data_status = format!("Could not apply cache budget: {error}");
        }
    }

    pub(crate) fn decoded_cache_resident_bytes(&self) -> usize {
        self.alignment_tracks
            .iter()
            .filter_map(|track| track.cache_stats)
            .fold(0_usize, |total, stats| {
                total.saturating_add(stats.resident_weight)
            })
    }

    pub(crate) fn row_scene_resident_bytes(&mut self) -> usize {
        let mut scenes = self
            .alignment_tracks
            .iter()
            .flat_map(|track| track.projections.values())
            .flat_map(|projection| projection.row_scenes.iter().cloned())
            .collect::<Vec<_>>();
        scenes.sort_unstable_by_key(|scene| Arc::as_ptr(scene) as usize);
        scenes.dedup_by(|left, right| Arc::ptr_eq(left, right));
        let key = scenes
            .iter()
            .map(|scene| (Arc::as_ptr(scene) as usize, scene.residency_revision()))
            .collect::<Vec<_>>();
        if let Some((cached_key, bytes)) = self.row_scene_residency_cache.as_ref()
            && cached_key == &key
        {
            return *bytes;
        }
        let bytes = AlignmentRowScene::estimated_resident_bytes(scenes.iter().map(Arc::as_ref));
        self.row_scene_residency_cache = Some((key, bytes));
        bytes
    }

    fn trim_inactive_row_scenes(&mut self, maximum_bytes: usize) {
        while self.row_scene_resident_bytes() > maximum_bytes {
            let mut removed = false;
            'tracks: for track in &mut self.alignment_tracks {
                for projection in track.projections.values_mut() {
                    if projection.row_scenes.len() > 1 {
                        projection.row_scenes.remove(0);
                        removed = true;
                        break 'tracks;
                    }
                }
            }
            if !removed {
                break;
            }
        }
    }

    pub(crate) fn request_alignment_query(&mut self) {
        let panel_id = self.panels.active().id;
        for track in &mut self.alignment_tracks {
            track.projection_mut(panel_id).pending_query =
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.pending_query)
            });
        self.dispatch_alignment_query();
    }

    pub(crate) fn request_alignment_scene_camera_update(&mut self, track_id: TrackId) {
        let panel_id = self.panels.active().id;
        let Some(track) = self
            .alignment_tracks
            .iter_mut()
            .find(|track| track.config.id == track_id)
        else {
            return;
        };
        if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
            return;
        }
        let projection = track.projection_mut(panel_id);
        projection.advance_query_generation();
        projection.pending_query = true;
        self.panels.active_mut().pending_queries.alignment = true;
        self.dispatch_alignment_query();
    }

    fn alignment_scene_camera_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        contig: ContigId,
        contig_length: u64,
    ) -> Option<AlignmentCamera> {
        let panel = self.panels.get(panel_id)?;
        let panel_rect = self.panel_column_rect(panel_id)?;
        let frame = panel.track_frames.iter().find(|frame| {
            frame.channel == ui::TrackPane::Reads
                && frame.key == TrackFrameKey::Alignment(track_id)
                && frame.visible
                && !frame.collapsed
        })?;
        let top = frame
            .frozen_bottom
            .unwrap_or(frame.top + crate::display::READ_PANE_HEADER_HEIGHT)
            .max(panel_rect.top());
        let bottom = frame.bottom.min(panel_rect.bottom());
        if bottom <= top || panel_rect.width() <= 0.0 {
            return None;
        }
        let density = self.alignment_density_for_panel_track(panel_id, track_id);
        let row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
        let read_height = (5.0 * density).clamp(1.5, (row_height - 1.0).max(1.5));
        let row_scroll = panel
            .read_panel_scroll_state(track_id)
            .effective_scroll_points()
            / row_height;
        let viewport = panel.viewport.remapped_contig(contig, contig_length).ok()?;
        AlignmentCamera::from_viewport(
            viewport,
            f64::from(row_scroll),
            f64::from(row_height),
            f64::from(read_height),
            CpuClipRect {
                x: f64::from(panel_rect.left()),
                y: f64::from(top),
                width: f64::from(panel_rect.width()),
                height: f64::from(bottom - top),
            },
        )
        .ok()
    }

    /// Reconciles scene cameras after a desktop layout pass. Pane resizing, density changes,
    /// track expansion, and window-scale changes alter only camera/clip state; they must not wait
    /// for an unrelated genomic navigation transaction or rebuild alignment evidence.
    pub(crate) fn refresh_alignment_scene_cameras_after_layout(&mut self) {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut changed_by_panel = Vec::<(PanelId, Vec<TrackId>)>::new();
        for panel_id in panel_ids {
            let Some(panel) = self.panels.get(panel_id) else {
                continue;
            };
            let contig_name = panel.contig_name.clone();
            let changed = self
                .alignment_tracks
                .iter()
                .filter_map(|track| {
                    if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
                        return None;
                    }
                    let metadata = track.scene_metadata.as_ref()?;
                    let contig = metadata.dictionary.id_by_name(&contig_name)?;
                    let contig_length = metadata.dictionary.get(contig)?.length();
                    let camera = self.alignment_scene_camera_for_panel_track(
                        panel_id,
                        track.config.id,
                        contig,
                        contig_length,
                    )?;
                    track
                        .projection(panel_id)
                        .and_then(|projection| projection.desired_scene_camera)
                        .filter(|current| *current == camera)
                        .is_none()
                        .then_some(track.config.id)
                })
                .collect::<Vec<_>>();
            if !changed.is_empty() {
                changed_by_panel.push((panel_id, changed));
            }
        }
        if changed_by_panel.is_empty() {
            return;
        }

        let active_id = self.panels.active().id;
        for (panel_id, track_ids) in changed_by_panel {
            for track_id in track_ids {
                let Some(track) = self
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == track_id)
                else {
                    continue;
                };
                let projection = track.projection_mut(panel_id);
                projection.advance_query_generation();
                projection.pending_query = true;
            }
            self.panels.activate(panel_id);
            self.panels.active_mut().pending_queries.alignment = true;
            self.dispatch_alignment_query();
        }
        self.panels.activate(active_id);
    }

    /// Captures every visible alignment lane from one immutable panel-scene epoch and prepares
    /// its disposable GPU derivative. The source worker is never touched here: held pan and
    /// vertical scroll can call this on every input event using already-published pages.
    pub(crate) fn prepare_alignment_scene_frame(&mut self) {
        #[derive(Clone)]
        struct OwnedGpuRequest {
            scene: Arc<AlignmentPresentationScene>,
            camera_sequence: u64,
            camera: AlignmentCamera,
            scissor: AlignmentGpuScissor,
            interaction: AlignmentInteractionState,
        }

        self.alignment_scene_camera_revision = self
            .alignment_scene_camera_revision
            .saturating_add(1)
            .max(1);
        let camera_revision = self.alignment_scene_camera_revision;
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut captured = HashMap::new();
        for panel_id in panel_ids {
            let Some(panel) = self.panels.get(panel_id) else {
                continue;
            };
            let contig_name = panel.contig_name.clone();
            let individual_reads_visible = panel.viewport.detail_level() != DetailLevel::Overview;
            let cameras = self
                .alignment_tracks
                .iter()
                .filter_map(|track| {
                    if !individual_reads_visible {
                        return None;
                    }
                    let projection = track.projection(panel_id)?;
                    if projection.index_only_overview || projection.scene_owner.is_none() {
                        return None;
                    }
                    let metadata = track.scene_metadata.as_ref()?;
                    let contig = metadata.dictionary.id_by_name(&contig_name)?;
                    let contig_length = metadata.dictionary.get(contig)?.length();
                    self.alignment_scene_camera_for_panel_track(
                        panel_id,
                        track.config.id,
                        contig,
                        contig_length,
                    )
                    .map(|camera| (track.config.id, camera))
                })
                .collect::<Vec<_>>();
            let track_ids = cameras
                .iter()
                .map(|(track_id, _)| *track_id)
                .collect::<Vec<_>>();
            let store = self
                .alignment_panel_scene_stores
                .entry(panel_id)
                .or_default();
            store.replace_visible_tracks(track_ids.iter().copied());
            for track_id in &track_ids {
                let Some(frame) = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == *track_id)
                    .and_then(|track| track.projection(panel_id))
                    .and_then(|projection| projection.scene_frame.as_ref())
                else {
                    continue;
                };
                if let Err(error) =
                    store.admit(*track_id, frame.camera_sequence, frame.snapshot.clone())
                {
                    self.data_status = format!("Could not admit alignment scene: {error}");
                }
            }
            match store.capture(panel_id, camera_revision, cameras) {
                Ok(frame) => {
                    captured.insert(panel_id, frame);
                }
                Err(error) => {
                    self.data_status = format!("Could not capture alignment scene: {error}");
                }
            }
        }
        self.alignment_panel_frames = captured;

        let scale = self.window.scale_factor() as f32;
        let screen_size_points = [
            self.surface_config.width as f32 / scale.max(f32::EPSILON),
            self.surface_config.height as f32 / scale.max(f32::EPSILON),
        ];
        let mut owned = Vec::<OwnedGpuRequest>::new();
        for (panel_id, frame) in &self.alignment_panel_frames {
            for lane in frame.lanes.iter() {
                let Some(snapshot) = &lane.snapshot else {
                    continue;
                };
                let Some(scissor) = alignment_scene_gpu_scissor(
                    lane.camera,
                    scale,
                    self.surface_config.width,
                    self.surface_config.height,
                ) else {
                    continue;
                };
                let interaction = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == lane.track_id)
                    .and_then(|track| track.projection(*panel_id))
                    .map_or_else(AlignmentInteractionState::default, |projection| {
                        projection.scene_interaction.clone()
                    });
                owned.push(OwnedGpuRequest {
                    scene: Arc::clone(snapshot.reads()),
                    camera_sequence: camera_revision,
                    camera: lane.camera,
                    scissor,
                    interaction,
                });
            }
        }
        if owned.is_empty() {
            self.alignment_gpu_frame = None;
            self.alignment_gpu_residency = None;
            self.alignment_gpu_prefetch_pending = false;
            return;
        }
        let requests = owned
            .iter()
            .map(|request| AlignmentGpuSceneRequest {
                scene: &request.scene,
                camera_sequence: request.camera_sequence,
                camera: request.camera,
                screen_size_points,
                scissor: request.scissor,
                interaction: &request.interaction,
            })
            .collect::<Vec<_>>();
        let requested_scene_revisions = requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        let scene_authority_changed = self.alignment_gpu_frame.as_ref().is_none_or(|frame| {
            frame.scene_revisions.as_ref() != requested_scene_revisions.as_slice()
        });
        if scene_authority_changed {
            self.alignment_gpu_residency = None;
        }
        match self.alignment_gpu_renderer.prepare_with_admission(
            &self.device,
            &self.queue,
            &requests,
            &self.alignment_scene_ledger,
            camera_revision,
        ) {
            Ok((frame, prepare, mandatory)) => {
                self.alignment_gpu_frame = Some(frame);
                if scene_authority_changed
                    || mandatory.admitted_pages > 0
                    || self.alignment_gpu_prefetch_pending
                {
                    match self
                        .alignment_gpu_renderer
                        .prefetch_prepared_scene_pages_bounded(
                            &self.device,
                            &self.queue,
                            &requests,
                            &self.alignment_scene_ledger,
                            camera_revision,
                            NonZeroUsize::new(ALIGNMENT_GPU_PREFETCH_PAGES_PER_FRAME)
                                .expect("alignment GPU prefetch quantum is non-zero"),
                        ) {
                        Ok(prefetch) => {
                            self.alignment_gpu_prefetch_pending = prefetch.missing_pages_remain;
                            tracing::debug!(
                                mandatory_pages = prepare.mandatory_pages,
                                mandatory_uploaded_bytes = mandatory.uploaded_bytes,
                                prepared_pages = prefetch.prepared_pages,
                                missing_pages_before = prefetch.missing_pages_before,
                                prefetched_pages = prefetch.admission.admitted_pages,
                                prefetched_bytes = prefetch.admission.uploaded_bytes,
                                remaining_pages = prefetch.remaining_missing_pages,
                                "advanced bounded alignment GPU prefetch"
                            );
                            let complete_vertical_scene = requests.iter().all(|request| {
                                let total_rows = u32::try_from(request.scene.atlas().row_count())
                                    .unwrap_or(u32::MAX);
                                let rows = request.scene.prepared_rows();
                                rows.start == 0 && rows.end == total_rows
                            });
                            if prefetch.is_complete()
                                && complete_vertical_scene
                                && self.alignment_gpu_residency.is_none()
                            {
                                match self
                                    .alignment_gpu_renderer
                                    .pin_prepared_scene_pages(&requests)
                                {
                                    Ok(lease) => {
                                        if let Some(request) = requests.first() {
                                            self.data_status = format!(
                                                "{} complete alignments · all {} rows ready for instant browsing",
                                                request.scene.evidence().record_count(),
                                                request.scene.atlas().row_count()
                                            );
                                        }
                                        self.alignment_gpu_residency = Some(lease);
                                    }
                                    Err(error) => tracing::debug!(
                                        %error,
                                        "complete alignment GPU residency was not yet pinnable"
                                    ),
                                }
                            }
                            if self.alignment_gpu_prefetch_pending {
                                self.window.request_redraw();
                            }
                        }
                        Err(error) => {
                            self.alignment_gpu_prefetch_pending = false;
                            tracing::debug!(
                                %error,
                                "bounded alignment GPU prefetch yielded to the live budget"
                            );
                        }
                    }
                }
            }
            Err(error) => {
                // A frame with stale camera uniforms is not a truthful fallback. The immutable
                // panel frame remains resident and can be retried after pressure is relieved.
                self.alignment_gpu_frame = None;
                self.alignment_gpu_prefetch_pending = false;
                self.data_status = format!("Could not prepare alignment scene: {error}");
            }
        }
    }

    pub(crate) fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId) {
        let (hovered, selected) = self.panels.get(panel_id).map_or((None, None), |panel| {
            (panel.hovered_alignment_hit, panel.selected_alignment_hit)
        });
        for track in &mut self.alignment_tracks {
            let hovered = hovered
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            let selected = selected
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            track.projection_mut(panel_id).scene_interaction =
                AlignmentInteractionState::new(hovered, selected, std::iter::empty());
        }
    }

    pub(crate) fn alignment_scene_snapshot_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<&Arc<AlignmentSceneSnapshot>> {
        self.alignment_panel_frames
            .get(&panel_id)?
            .lane(track_id)?
            .snapshot
            .as_ref()
    }

    pub(crate) fn alignment_scene_record_for_hit(
        &self,
        panel_id: PanelId,
        hit: AlignmentHit,
    ) -> Option<AlignmentRecord> {
        let snapshot = self.alignment_scene_snapshot_for_panel_track(panel_id, hit.track)?;
        snapshot.inspect_retained(hit.handle).ok().flatten()
    }

    fn take_alignment_scene_owner(&mut self) -> SourceOwnerId {
        self.alignment_scene_owner_clock = self
            .alignment_scene_owner_clock
            .checked_add(1)
            .expect("alignment scene owner identity space was exhausted");
        SourceOwnerId::new(self.alignment_scene_owner_clock)
    }

    fn take_alignment_scene_request_token(&mut self) -> AlignmentSceneRequestToken {
        self.alignment_scene_request_clock = self
            .alignment_scene_request_clock
            .checked_add(1)
            .expect("alignment scene request identity space was exhausted");
        AlignmentSceneRequestToken::new(self.alignment_scene_request_clock)
    }

    /// Sends the UI's current camera to the unified source actor and changes no pixels itself.
    /// Existing immutable scene pages are captured against this camera immediately by
    /// `prepare_alignment_scene_frame`; source acquisition remains asynchronous and latest-wins.
    pub(crate) fn dispatch_alignment_query(&mut self) {
        if !self.panels.active().pending_queries.alignment {
            return;
        }
        if self.alignment_tracks.is_empty() {
            self.panels.active_mut().pending_queries.alignment = false;
            return;
        }
        let viewport = self.panels.active().viewport;
        let resolution = AlignmentQueryResolution::for_viewport(viewport);
        let panel_id = self.panels.active().id;
        if !resolution.requires_alignment_records() {
            let mut settled = 0_usize;
            let mut retired_tracks = Vec::new();
            for track in &mut self.alignment_tracks {
                match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => {}
                    AlignmentTrackLifecycle::Ready => {
                        let track_id = track.config.id;
                        let owner = track
                            .projection(panel_id)
                            .and_then(|projection| projection.scene_owner);
                        let mut removal_retry = false;
                        if let (Some(engine), Some(owner)) = (track.scene_engine.as_ref(), owner) {
                            match engine.try_remove_lane(owner) {
                                Ok(()) => {}
                                Err(EngineQueueError::Full) => removal_retry = true,
                                Err(error @ EngineQueueError::Disconnected) => {
                                    track.lifecycle =
                                        AlignmentTrackLifecycle::Failed(error.to_string());
                                }
                            }
                        }
                        let projection = track.projection_mut(panel_id);
                        if !removal_retry {
                            projection.scene_owner = None;
                            projection.scene_contig = None;
                            projection.scene_contract = None;
                            projection.scene_policy = None;
                        }
                        projection.desired_scene_camera = None;
                        projection.desired_scene_request_token = None;
                        projection.scene_frame = None;
                        projection.scene_frame_request_token = None;
                        projection.settle_index_only_overview(resolution);
                        // A full control queue delays source-lane retirement, never visual
                        // retirement. Keep only the owner token so the normal pending-query retry
                        // can remove it after the worker drains.
                        projection.pending_query = removal_retry;
                        retired_tracks.push(track_id);
                        settled = settled.saturating_add(1);
                    }
                    AlignmentTrackLifecycle::Failed(_) => {
                        let projection = track.projection_mut(panel_id);
                        projection.pending_query = false;
                        projection.index_only_overview = false;
                    }
                }
            }
            let primary_track_id = self.primary_alignment_track_id();
            for track_id in retired_tracks {
                let panel = self.panels.active_mut();
                panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .retire_scene_authority();
                if panel
                    .pending_read_reveal
                    .is_some_and(|(pending_track, _)| pending_track == track_id)
                {
                    panel.pending_read_reveal = None;
                }
                if track_id == primary_track_id {
                    panel.vertical_scroll_points = 0.0;
                    panel.vertical_content_origin_points = 0.0;
                    panel.latent_vertical_scroll_fraction = None;
                    panel.content_bottom_points = 0.0;
                }
            }
            self.panels.active_mut().pending_queries.alignment =
                self.alignment_tracks.iter().any(|track| {
                    track
                        .projection(panel_id)
                        .is_some_and(|projection| projection.pending_query)
                });
            if settled > 0 {
                self.data_status = format!(
                    "Index-only overview for {settled} alignment track(s); no records requested"
                );
            }
            self.prepare_alignment_scene_frame();
            return;
        }
        let contig_name = self.panels.active().contig_name.clone();
        let mut sent = 0_usize;
        for track_index in 0..self.alignment_tracks.len() {
            if matches!(
                self.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Opening
            ) {
                continue;
            }
            if matches!(
                self.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Failed(_)
            ) {
                self.alignment_tracks[track_index]
                    .projection_mut(panel_id)
                    .pending_query = false;
                continue;
            }
            if !self.alignment_tracks[track_index]
                .projection(panel_id)
                .is_some_and(|projection| projection.pending_query)
            {
                continue;
            }
            let Some(metadata) = self.alignment_tracks[track_index].scene_metadata.clone() else {
                continue;
            };
            let Some((contig, contig_length)) = metadata
                .dictionary
                .id_by_name(&contig_name)
                .and_then(|contig| {
                    metadata
                        .dictionary
                        .get(contig)
                        .map(|entry| (contig, entry.length()))
                })
            else {
                let message = format!(
                    "{} is not available in this alignment",
                    self.panels.active().contig_name
                );
                self.alignment_tracks[track_index].mark_unavailable(panel_id, message);
                continue;
            };
            let track_id = self.alignment_tracks[track_index].config.id;
            let existing_owner = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_owner);
            let existing_contig = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contig);
            let existing_contract = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contract.as_ref())
                .cloned();
            let existing_policy = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_policy);
            let target_viewport = match viewport.remapped_contig(contig, contig_length) {
                Ok(viewport) => viewport,
                Err(_) => continue,
            };
            let old_atlas_interval = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_frame.as_ref())
                .and_then(|frame| frame.snapshot.as_ref())
                .map(|snapshot| snapshot.reads().atlas().interval());
            let contig_changed = existing_contig.is_some_and(|current| current != contig);
            let atlas_disjoint = old_atlas_interval.is_some_and(|interval| {
                interval.contig() != contig || !interval.overlaps(target_viewport.interval())
            });
            let authority_discontinuity = contig_changed || atlas_disjoint;
            let primary_track_id = self.primary_alignment_track_id();
            let scroll_reset = {
                let panel = self.panels.active_mut();
                let reset = panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .bind_scene_contig(contig, authority_discontinuity);
                if reset {
                    panel.pending_read_reveal = None;
                    if track_id == primary_track_id {
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                        panel.content_bottom_points = 0.0;
                    }
                }
                reset
            };
            if authority_discontinuity || scroll_reset {
                let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
                projection.scene_frame = None;
                projection.scene_frame_request_token = None;
                projection.desired_scene_camera = None;
                projection.desired_scene_request_token = None;
                if let Some(store) = self.alignment_panel_scene_stores.get_mut(&panel_id)
                    && let Err(error) = store.clear_lane(track_id)
                {
                    self.data_status = format!("Could not reset alignment scene lane: {error}");
                }
            }
            let Some(camera) = self.alignment_scene_camera_for_panel_track(
                panel_id,
                track_id,
                contig,
                contig_length,
            ) else {
                // Layout is rebuilt independently of source opening. Keep this request pending
                // until a non-empty read-pane frame exists rather than inventing camera bounds.
                continue;
            };
            let promoted_template = self
                .promoted_read
                .filter(|(candidate, _)| *candidate == track_id)
                .map(|(_, hash)| hash);
            let contract = alignment_scene_build_contract(
                &self.alignment_tracks[track_index].config,
                &metadata,
                contig,
                contig_length,
                viewport,
                promoted_template,
                self.reference_revision,
            );
            let policy = alignment_scene_preparation_policy(
                self.settings.read_browsing_buffer_percent,
                camera,
            );
            let complete_read_limit = self.alignment_tracks[track_index]
                .config
                .maximum_alignments
                .max(1);
            let mut queue_error = None;
            let must_insert = existing_owner.is_none() || existing_contig != Some(contig);
            let owner = if must_insert {
                self.take_alignment_scene_owner()
            } else {
                existing_owner.expect("an existing alignment scene lane has an owner")
            };
            let request_token = self.take_alignment_scene_request_token();
            let contract_changed = existing_contract
                .as_ref()
                .is_none_or(|existing| !alignment_scene_contract_equivalent(existing, &contract));
            let limit_changed =
                self.alignment_tracks[track_index].scene_complete_read_limit != complete_read_limit;
            let mut limit_queued = false;
            let mut lane_removed = false;
            let mut lane_inserted = false;
            let mut contract_replaced = false;
            let mut policy_replaced = false;
            let Some(engine) = self.alignment_tracks[track_index].scene_engine.as_ref() else {
                let track = &mut self.alignment_tracks[track_index];
                track.lifecycle = AlignmentTrackLifecycle::Failed(
                    "Unified alignment scene source is unavailable; no legacy fallback was used"
                        .to_owned(),
                );
                track.projection_mut(panel_id).pending_query = false;
                continue;
            };
            if limit_changed {
                match engine.try_set_source_control(LocalBamSourceControl::SetCompleteReadLimit(
                    NonZeroUsize::new(complete_read_limit)
                        .expect("Complete read limit is non-zero"),
                )) {
                    Ok(()) => limit_queued = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && existing_owner.is_some() && existing_contig != Some(contig)
            {
                match engine.try_remove_lane(existing_owner.expect("checked existing owner")) {
                    Ok(()) => lane_removed = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && must_insert {
                match engine.try_insert_lane_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                    policy,
                ) {
                    Ok(()) => lane_inserted = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && contract_changed {
                match engine.try_replace_contract_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                ) {
                    Ok(()) => contract_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && existing_policy != Some(policy) {
                match engine.try_set_preparation_policy_for_request(owner, request_token, policy) {
                    Ok(()) => policy_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none()
                && let Err(error) =
                    engine.try_update_camera_for_request(owner, request_token, camera)
            {
                queue_error = Some(error);
            }
            {
                let track = &mut self.alignment_tracks[track_index];
                if limit_queued {
                    track.scene_complete_read_limit = complete_read_limit;
                }
                let projection = track.projection_mut(panel_id);
                if lane_inserted {
                    projection.scene_owner = Some(owner);
                    projection.scene_contig = Some(contig);
                    projection.scene_contract = Some(contract.clone());
                    projection.scene_policy = Some(policy);
                } else if lane_removed {
                    // A bounded control queue can accept removal and reject the subsequent insert.
                    // Make the retry state explicit; never pretend the old-contig lane remains.
                    projection.scene_owner = None;
                    projection.scene_contig = None;
                    projection.scene_contract = None;
                    projection.scene_policy = None;
                }
                if contract_replaced {
                    projection.scene_contract = Some(contract);
                }
                if policy_replaced {
                    projection.scene_policy = Some(policy);
                }
            }
            match queue_error {
                None => {
                    let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
                    projection.pending_query = false;
                    projection.begin_checking(Instant::now());
                    projection.last_query_resolution = Some(resolution);
                    projection.desired_scene_camera = Some(camera);
                    projection.desired_scene_request_token = Some(request_token);
                    projection.unavailable = None;
                    projection.index_only_overview = false;
                    sent = sent.saturating_add(1);
                }
                Some(EngineQueueError::Full) => {}
                Some(EngineQueueError::Disconnected) => {
                    let track = &mut self.alignment_tracks[track_index];
                    track.projection_mut(panel_id).pending_query = false;
                    track.lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.pending_query)
            });
        if sent > 0 {
            self.data_status = format!("Updating {sent} alignment scene(s)…");
        } else if self.panels.active().pending_queries.alignment {
            "Alignment queues busy; retrying the latest locus…".clone_into(&mut self.data_status);
        }
        // Camera-only movement is visible before the worker produces another event. The capture
        // keeps the latest immutable snapshot and computes its truthful intersection with the
        // live viewport.
        self.prepare_alignment_scene_frame();
    }

    pub(crate) fn alignment_track_limit_reached(&mut self) -> bool {
        if self.alignment_tracks.len() >= MAXIMUM_ALIGNMENT_TRACKS {
            self.data_status =
                format!("A workspace supports at most {MAXIMUM_ALIGNMENT_TRACKS} alignment tracks");
            return true;
        }
        false
    }

    pub(crate) fn add_alignment_track(
        &mut self,
        source: TrackSource,
        opened_name: &str,
        remote_index_url: Option<String>,
    ) {
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let next_id = configs
            .iter()
            .map(|track| track.id.get())
            .chain(self.tracks.tracks().iter().map(|track| track.id.get()))
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        let mut config =
            SessionAlignmentTrack::new(TrackId::new(next_id), opened_name.to_owned(), source);
        config.remote_index_url = remote_index_url;
        config.maximum_alignments = config
            .maximum_alignments
            .min(self.workspace_alignment_limit)
            .max(1);
        configs.push(config);
        match self.replace_alignment_track_controllers(configs) {
            Ok(()) => {
                self.invalidate_navigation_preview_contract("alignment source set changed");
                self.begin_all_panel_refresh_transactions();
                self.panels.active_mut().pending_queries.alignment = false;
                self.synthetic = false;
                self.data_status =
                    format!("Opening {opened_name} and rebalancing alignment-track caches…");
                self.rebuild_genomic_display();
                self.display_dirty = true;
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn open_alignment_source(&mut self, path: &Path) {
        self.open_alignment_source_with_remote_index(path, None);
    }

    pub(crate) fn open_remote_source(&mut self, request: RemoteSourceRequest) -> Result<()> {
        match request.kind {
            RemoteSourceKind::Alignment => {
                if let Some(reference_url) = request.reference_url {
                    self.open_reference_source_with_remote_index(
                        Path::new(&reference_url),
                        request.reference_index_url,
                    );
                }
                self.open_alignment_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::Reference => {
                self.open_reference_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::IndexedTrack => {
                self.add_track_source(Path::new(&request.source_url))?;
            }
        }
        Ok(())
    }

    pub(crate) fn open_alignment_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        if self.alignment_track_limit_reached() {
            return;
        }
        let source = track_source(path);
        let opened_name = source_display_name(Some(&source), "Alignments");
        if !self.alignment_tracks.is_empty() {
            self.add_alignment_track(source, &opened_name, remote_index_url);
            return;
        }
        let source_changed = self.primary_alignment_source() != Some(&source);
        let name = if source_changed {
            opened_name
        } else {
            self.primary_alignment_name().to_owned()
        };
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let config = upsert_primary_alignment_config(
            &mut configs,
            self.tracks.tracks(),
            source.clone(),
            &name,
        );
        config.remote_index_url = remote_index_url;
        config.filters = self.primary_alignment_filters().clone();
        config.maximum_alignments = self
            .primary_alignment_limit()
            .min(self.workspace_alignment_limit)
            .max(1);
        let cache_bytes = alignment_cache_share(
            self.cache_budgets.decoded_alignments,
            self.alignment_tracks.len().max(1),
        );
        let controller = spawn_alignment_track_controller(
            configs.remove(0),
            cache_bytes,
            self.alignment_scene_ledger.clone(),
            self.reference_source.as_ref().map(source_path),
            self.remote_reference_index_url.clone(),
            &self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        );
        match controller {
            Ok(controller) => {
                let interval = self.panels.active().viewport.interval();
                self.panels.active_mut().requested_locus = Some(format!(
                    "{}:{}-{}",
                    self.panels.active().contig_name,
                    interval.start() + 1,
                    interval.end()
                ));
                self.panels.active().generation_clock.advance();
                if self.alignment_tracks.is_empty() {
                    self.alignment_tracks.push(controller);
                } else {
                    self.alignment_tracks[0] = controller;
                }
                self.invalidate_navigation_preview_contract("alignment source changed");
                self.begin_all_panel_refresh_transactions();
                self.reconcile_track_presentation();
                self.panels.active_mut().pending_queries.alignment = false;
                self.panels.active_mut().selected_chain = None;
                self.panels.active_mut().display_list = DisplayList::default();
                self.layout_cache.clear();
                self.synthetic = false;
                self.data_status = format!("Opening {}…", source.redacted_label());
                self.display_dirty = true;
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn open_reference_source(&mut self, path: &Path) {
        self.open_reference_source_with_remote_index(path, None);
    }

    pub(crate) fn open_reference_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        self.reference_open_error = None;
        self.remote_reference_index_url = remote_index_url;
        let source = track_source(path);
        let source_label = source.redacted_label();
        let engine = ReferenceEngine::spawn(8);
        if let Err(error) = engine.try_open_with_options(
            path.to_path_buf(),
            reference_open_options(self.remote_reference_index_url.clone()),
        ) {
            self.data_status = error.to_string();
            return;
        }
        self.reference_source = Some(source);
        self.reference_dictionary = None;
        self.reference_revision = None;
        self.reference_remote_io_stats = None;
        self.reference_engine = Some(engine);
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            if let Some(panel) = self.panels.get_mut(id) {
                let interval = panel.viewport.interval();
                panel.requested_locus = Some(format!(
                    "{}:{}-{}",
                    panel.contig_name,
                    interval.start().saturating_add(1),
                    interval.end()
                ));
                panel.generation_clock.advance();
                panel.reference_slice = None;
                panel.reference_slice_revision = None;
                panel.reference_request_revision = None;
                panel.reference_request_interval = None;
                panel.reference_installs_queued.clear();
                panel.reference_settled_generation = None;
                panel.reference_query_error_generation = None;
                panel.pending_queries.reference = false;
            }
        }
        if !self.alignment_tracks.is_empty() {
            let configs = self
                .alignment_tracks
                .iter()
                .map(|track| track.config.clone())
                .collect::<Vec<_>>();
            if let Err(error) = self.replace_alignment_track_controllers(configs) {
                self.data_status = error.to_string();
                return;
            }
        }
        self.synthetic = false;
        self.primary_alignment_filters_mut().show_reference = true;
        self.data_status = format!("Opening reference genome {source_label}…");
        self.invalidate_navigation_preview_contract("reference source changed");
        self.begin_all_panel_refresh_transactions();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
    }

    pub(crate) fn open_genome_configuration(&mut self, path: &Path) -> Result<()> {
        let configuration = GenomeConfiguration::read(path)?;
        let cytoband_path = match &configuration.cytobands {
            Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
            Some(TrackSource::Remote { .. }) => {
                anyhow::bail!("remote cytoband files are not yet supported")
            }
            None => None,
        };
        let annotations = configuration
            .default_annotations
            .iter()
            .map(|annotation| {
                track_spec_kind(annotation.kind, &annotation.source, annotation.format)
                    .map(|kind| {
                        (
                            source_path(&annotation.source),
                            kind,
                            annotation.kind,
                            annotation.name.clone(),
                            annotation.format,
                        )
                    })
                    .with_context(|| format!("unsupported default annotation {}", annotation.name))
            })
            .collect::<Result<Vec<_>>>()?;

        self.assembly_name.clone_from(&configuration.name);
        self.assembly_identity
            .clone_from(&configuration.assembly_identity);
        self.assembly
            .identity
            .clone_from(&configuration.assembly_identity);
        self.assembly.default_annotation_count = configuration.default_annotations.len();
        self.contig_aliases
            .clone_from(&configuration.contig_aliases);
        self.cytoband_path = cytoband_path;
        let reference = source_path(&configuration.reference);
        self.open_reference_source(&reference);
        for (annotation_path, kind, track_kind, name, format) in annotations {
            self.add_configured_track_source(&annotation_path, kind, track_kind, name, format)?;
        }
        self.data_status = format!("Opening genome configuration {}…", path.display());
        Ok(())
    }

    pub(crate) fn replace_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
    ) -> Result<()> {
        let cache_bytes =
            alignment_cache_share(self.cache_budgets.decoded_alignments, configs.len());
        let reference_path = self.reference_source.as_ref().map(source_path);
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut controllers = Vec::with_capacity(configs.len());
        for config in configs {
            controllers.push(spawn_alignment_track_controller(
                config,
                cache_bytes,
                self.alignment_scene_ledger.clone(),
                reference_path.clone(),
                self.remote_reference_index_url.clone(),
                &panel_ids,
            )?);
        }
        let replaced = std::mem::replace(&mut self.alignment_tracks, controllers);
        for controller in replaced {
            self.retire_alignment_track_controller(controller);
        }
        self.reset_alignment_scene_panel_authority();
        self.reconcile_track_presentation();
        Ok(())
    }

    fn reset_alignment_scene_panel_authority(&mut self) {
        let track_ids = self
            .alignment_tracks
            .iter()
            .filter(|track| track.scene_engine.is_some())
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        self.alignment_panel_scene_stores = self
            .panels
            .iter()
            .map(|panel| {
                (
                    panel.id,
                    AlignmentPanelSceneStore::new(track_ids.iter().copied()),
                )
            })
            .collect();
        self.alignment_panel_frames.clear();
        self.alignment_gpu_frame = None;
        self.alignment_gpu_residency = None;
        self.alignment_gpu_prefetch_pending = false;
    }

    pub(crate) fn retire_alignment_track_controller(
        &mut self,
        controller: AlignmentTrackController,
    ) {
        for projection in controller.projections.values() {
            projection.advance_query_generation();
        }
        let AlignmentTrackController { scene_engine, .. } = controller;
        if let Some(task) = scene_engine.and_then(AlignmentSceneEngine::retire) {
            self.alignment_retirement_tasks.push(task);
        }
    }

    pub(crate) fn activate_first_alignment_track(&mut self) -> Result<()> {
        if let Some((config, filter_editor, active_filters)) =
            self.alignment_tracks.first().map(|track| {
                (
                    track.config.clone(),
                    track.filter_editor.clone(),
                    track.active_filters.clone(),
                )
            })
        {
            self.apply_primary_alignment_config(Some(&config), self.workspace_alignment_limit)?;
            let primary = self
                .alignment_tracks
                .first_mut()
                .expect("primary alignment still exists");
            primary.filter_editor = filter_editor;
            primary.active_filters = active_filters;
        } else {
            self.apply_primary_alignment_config(None, self.workspace_alignment_limit)?;
        }
        self.layout_cache.clear();
        Ok(())
    }

    pub(crate) fn remove_alignment_track(&mut self, id: TrackId) -> Result<bool> {
        if id == SYNTHETIC_ALIGNMENT_TRACK_ID {
            return Ok(false);
        }
        let Some(index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == id)
        else {
            return Ok(false);
        };
        let removed_was_primary = index == 0;
        let removed_track = self.alignment_tracks.remove(index);
        self.retire_alignment_track_controller(removed_track);
        self.reset_alignment_scene_panel_authority();
        self.track_presentation.remove_source(id);
        self.normalize_alignment_track_groups();
        if self
            .promoted_read
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.promoted_read = None;
        }
        self.metadata_selected_tracks.remove(&id);
        if matches!(
            self.open_panel,
            Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(track_id))) if track_id == id
        ) {
            self.open_panel = None;
        }
        if self.resizing_annotation_track.is_some_and(
            |(key, _)| matches!(key, TrackFrameKey::Alignment(track_id) if track_id == id),
        ) {
            self.resizing_annotation_track = None;
        }
        if self
            .resizing_read_track
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.resizing_read_track = None;
        }
        if self
            .vertical_overview_drag
            .is_some_and(|drag| drag.track_id == id)
        {
            self.vertical_overview_drag = None;
        }
        if removed_was_primary {
            self.activate_first_alignment_track()?;
        }
        for panel in self.panels.iter_mut() {
            clear_removed_alignment_panel_state(panel, id, removed_was_primary);
        }
        for track in &mut self.alignment_tracks {
            for projection in track.projections.values_mut() {
                projection.advance_query_generation();
                projection.pending_query =
                    !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
            }
        }
        self.invalidate_navigation_preview_contract("alignment source removed");
        self.begin_all_panel_refresh_transactions();
        self.request_all_alignment_queries();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(true)
    }
}

pub(crate) fn alignment_scene_gpu_scissor(
    camera: AlignmentCamera,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> Option<AlignmentGpuScissor> {
    let clip = camera.clip();
    let scale = scale_factor.max(f32::EPSILON) as f64;
    let left = (clip.x * scale)
        .round()
        .clamp(0.0, f64::from(surface_width)) as u32;
    let top = (clip.y * scale)
        .round()
        .clamp(0.0, f64::from(surface_height)) as u32;
    let right = ((clip.x + clip.width) * scale)
        .round()
        .clamp(f64::from(left), f64::from(surface_width)) as u32;
    let bottom = ((clip.y + clip.height) * scale)
        .round()
        .clamp(f64::from(top), f64::from(surface_height)) as u32;
    (right > left && bottom > top).then_some(AlignmentGpuScissor {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// A retained snapshot from a discontinuous locus is not a row-count or render authority for the
/// new camera. Source workers may truthfully attach that old immutable snapshot while acquiring
/// the replacement; the desktop keeps the progress frame but strips the irrelevant scene.
pub(crate) fn alignment_scene_frame_for_ui(
    frame: AlignmentCoordinatorFrame,
) -> AlignmentCoordinatorFrame {
    let retained_interval = frame
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.reads().atlas().interval());
    alignment_scene_frame_for_ui_against_retained_interval(frame, retained_interval)
}

/// Normalizes a worker progress frame against the interval carried by its retained snapshot. This
/// decomposition keeps the discontinuous-locus rule directly testable without constructing the
/// heavyweight immutable scene graph.
pub(crate) fn alignment_scene_frame_for_ui_against_retained_interval(
    mut frame: AlignmentCoordinatorFrame,
    retained_interval: Option<GenomicInterval>,
) -> AlignmentCoordinatorFrame {
    let relevant = retained_interval.is_none_or(|interval| {
        alignment_scene_intervals_share_authority(interval, frame.camera.interval())
    });
    if !relevant {
        frame.snapshot = None;
        frame.covered_interval = None;
        frame.covered_rows = 0..0;
        frame.requested_rows = frame.camera.visible_rows();
        // The requirement describes the new desired camera, not the discarded old snapshot. It
        // must survive so the desktop can request and install the exact FASTA horizon.
        frame.scene_contracts_current = false;
        frame.individual_reads = AlignmentIndividualReadCapability::Pending;
        frame.vertical_residency = AlignmentVerticalResidency::ViewportBounded;
    }
    frame
}

pub(crate) fn alignment_scene_intervals_share_authority(
    atlas: GenomicInterval,
    camera: GenomicInterval,
) -> bool {
    atlas.contig() == camera.contig() && atlas.overlaps(camera)
}
