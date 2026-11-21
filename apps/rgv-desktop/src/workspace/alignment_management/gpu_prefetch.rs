use crate::{AlignmentGpuPreparedAuthority, AlignmentGpuResidencyBundle, Viewer};
use rgv_render::{AlignmentGpuSceneRequest, AlignmentSummaryGpuSceneRequest};
use std::sync::Arc;
use std::time::Instant;

impl Viewer {
    /// Advances speculative pages for the already-published camera authority. This path never
    /// recaptures a panel epoch, replaces a foreground frame, or increments the camera revision;
    /// repeated redraw quanta therefore accumulate rather than retiring one another.
    pub(crate) fn advance_current_alignment_gpu_prefetch(&mut self) {
        if self.alignment_optional_work_must_yield() {
            // Preserve the retargetable cursors and already useful overlap, but do not request a
            // further redraw from optional work. Input/source/GPU completion will naturally wake
            // the render loop and resume warming after foreground ownership settles.
            return;
        }
        let target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            self.alignment_gpu_prefetch.clear();
            return;
        };
        let Some(bundle) = epoch.gpu_bundle.as_ref() else {
            self.alignment_gpu_prefetch.clear();
            return;
        };
        if bundle.target_revision != target_revision {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let publication_attempt_sequence = bundle.publication_attempt_sequence;
        let published_has_reads = bundle.reads.is_some();
        let published_has_summaries = bundle.summaries.is_some();

        let scale = self.window.scale_factor() as f32;
        let screen_size_points = [
            self.surface_config.width as f32 / scale.max(f32::EPSILON),
            self.surface_config.height as f32 / scale.max(f32::EPSILON),
        ];
        let (mut owned_reads, mut owned_summaries, _) = self.collect_alignment_gpu_requests(
            &epoch.panel_frames,
            &bundle.lane_authorities,
            publication_attempt_sequence,
            scale,
        );
        owned_reads.sort_by_key(|request| (request.panel_id, request.track_id));
        owned_summaries.sort_by_key(|request| (request.panel_id, request.track_id));
        let read_requests = owned_reads
            .iter()
            .map(|request| request.borrowed(screen_size_points))
            .collect::<Vec<_>>();
        let summary_requests = owned_summaries
            .iter()
            .map(|request| request.borrowed(publication_attempt_sequence, screen_size_points))
            .collect::<Vec<_>>();
        if published_has_reads != !read_requests.is_empty()
            || published_has_summaries != !summary_requests.is_empty()
        {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let read_revisions = read_requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        let summary_revisions = summary_requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        let published_matches = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .is_some_and(|bundle| {
                bundle.reads.as_ref().is_none_or(|frame| {
                    frame.scene_revisions.as_ref() == read_revisions.as_slice()
                        && frame
                            .camera_sequences
                            .iter()
                            .all(|sequence| *sequence == publication_attempt_sequence)
                }) && bundle.summaries.as_ref().is_none_or(|frame| {
                    frame.scene_revisions.as_ref() == summary_revisions.as_slice()
                        && frame
                            .camera_sequences
                            .iter()
                            .all(|sequence| *sequence == publication_attempt_sequence)
                })
            });
        if !published_matches {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        // This is the exact manifest minted by the content replacement and carried by every
        // compatible camera-only publication. Rebuilding it here would make every redraw scale
        // with the complete ACTB prepared scene before doing one bounded prefetch quantum.
        let authority = bundle.prepared_authority.clone();
        if self.alignment_gpu_speculation_gate.authority.as_ref() != Some(&authority) {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let optional_pin_ceiling = match self.alignment_gpu_ledger.progress() {
            Ok(progress) => progress.budget,
            Err(error) => {
                tracing::debug!(%error, "could not inspect optional GPU residency ceiling");
                self.alignment_gpu_prefetch.clear();
                return;
            }
        };
        if self.alignment_gpu_prefetch.reads && self.alignment_gpu_prefetch.read_cursor.is_none() {
            match self
                .alignment_gpu_renderer
                .begin_prepared_scene_prefetch_cursor(
                    &read_requests,
                    Arc::clone(&authority.read_pages),
                    optional_pin_ceiling,
                ) {
                Ok(cursor) => self.alignment_gpu_prefetch.read_cursor = Some(cursor),
                Err(error) => {
                    tracing::debug!(%error, "could not begin bounded read GPU prefetch");
                    self.alignment_gpu_prefetch.clear();
                    self.alignment_gpu_speculation_gate
                        .suppress_for_foreground_pressure();
                    return;
                }
            }
        }
        if self.alignment_gpu_prefetch.summaries
            && self.alignment_gpu_prefetch.summary_cursor.is_none()
        {
            match self
                .alignment_summary_gpu_renderer
                .begin_prepared_scene_prefetch_cursor(
                    &summary_requests,
                    Arc::clone(&authority.summary_pages),
                    optional_pin_ceiling,
                ) {
                Ok(cursor) => self.alignment_gpu_prefetch.summary_cursor = Some(cursor),
                Err(error) => {
                    tracing::debug!(%error, "could not begin bounded summary GPU prefetch");
                    self.alignment_gpu_prefetch.clear();
                    self.alignment_gpu_speculation_gate
                        .suppress_for_foreground_pressure();
                    return;
                }
            }
        }
        self.advance_alignment_gpu_prefetch(
            &read_requests,
            &summary_requests,
            authority,
            publication_attempt_sequence,
            optional_pin_ceiling,
        );
    }

    pub(super) fn alignment_optional_work_must_yield(&self) -> bool {
        self.alignment_gpu_publication_retry.is_some()
            || self.alignment_capacity_reclaim.is_some()
            || self.session.panels.iter().any(|panel| {
                panel.navigation_query_schedule.gesture_active
                    || panel.navigation_query_schedule.pending.is_some()
            })
            || self.alignment_tracks.iter().any(|track| {
                track
                    .projections
                    .values()
                    .any(|projection| projection.navigation.phase().owner().is_some())
            })
    }

    pub(super) fn advance_alignment_gpu_prefetch(
        &mut self,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        authority: AlignmentGpuPreparedAuthority,
        publication_attempt_sequence: u64,
        optional_pin_ceiling: usize,
    ) {
        if !self.alignment_gpu_speculation_gate.allows_prefetch() {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate
                .finish_cycle(false, false);
            return;
        }

        let mut pressure = false;
        let maximum_pages = self.alignment_gpu_prefetch.maximum_pages();
        let summaries_next = self.alignment_gpu_prefetch.summaries_next;
        let advance_reads = self.alignment_gpu_prefetch.reads
            && (!self.alignment_gpu_prefetch.summaries || !summaries_next);
        let advance_summaries = self.alignment_gpu_prefetch.summaries
            && (!self.alignment_gpu_prefetch.reads || summaries_next);
        let quantum_started = Instant::now();
        let mut processed_pages = 0_usize;
        let mut attempted_quantum = false;
        let read_authority_complete = read_requests.iter().all(|request| {
            let total_rows = u32::try_from(request.scene.atlas().row_count()).unwrap_or(u32::MAX);
            let rows = request.scene.prepared_rows();
            rows.start == 0 && rows.end == total_rows
        });
        if advance_reads {
            attempted_quantum = true;
            let cursor = self
                .alignment_gpu_prefetch
                .read_cursor
                .as_mut()
                .expect("pending read prefetch has an initialized cursor");
            match self
                .alignment_gpu_renderer
                .advance_prepared_scene_prefetch_cursor(
                    &self.device,
                    &self.queue,
                    cursor,
                    &self.alignment_gpu_ledger,
                    publication_attempt_sequence,
                    maximum_pages,
                ) {
                Ok(prefetch) => {
                    processed_pages =
                        processed_pages.saturating_add(prefetch.admission.requested_pages);
                    self.alignment_gpu_prefetch.reads = prefetch.missing_pages_remain;
                    if prefetch.is_complete() {
                        let cursor = self
                            .alignment_gpu_prefetch
                            .read_cursor
                            .take()
                            .expect("completed read prefetch retains its cursor");
                        if cursor.retention_limited() {
                            self.alignment_gpu_prefetch.read_retention_limited = true;
                            tracing::debug!(
                                retained_bytes = cursor.retained_bytes(),
                                optional_pin_ceiling,
                                "read GPU warm-up completed in exact demand-paged mode"
                            );
                        } else if read_authority_complete {
                            match self
                                .alignment_gpu_renderer
                                .finish_prepared_scene_prefetch_cursor(cursor)
                            {
                                Ok(lease) => {
                                    self.alignment_gpu_prefetch.read_residency = Some(lease)
                                }
                                Err(error) => {
                                    pressure = true;
                                    tracing::debug!(%error, "could not finalize read GPU residency");
                                }
                            }
                        } else {
                            // Progressive semantic preparation has not yet exposed every row, so
                            // this exact authority is demand-paged by definition even if every
                            // currently prepared page fitted beneath the optional pin ceiling.
                            self.alignment_gpu_prefetch.read_retention_limited = true;
                        }
                    }
                }
                Err(error) => {
                    pressure = true;
                    tracing::debug!(%error, "read GPU prefetch yielded to foreground capacity");
                }
            }
        }

        if advance_summaries && !pressure {
            attempted_quantum = true;
            let cursor = self
                .alignment_gpu_prefetch
                .summary_cursor
                .as_mut()
                .expect("pending summary prefetch has an initialized cursor");
            match self
                .alignment_summary_gpu_renderer
                .advance_prepared_scene_prefetch_cursor(
                    &self.device,
                    &self.queue,
                    cursor,
                    &self.alignment_gpu_ledger,
                    publication_attempt_sequence,
                    maximum_pages,
                ) {
                Ok(prefetch) => {
                    processed_pages =
                        processed_pages.saturating_add(prefetch.admission.requested_pages);
                    self.alignment_gpu_prefetch.summaries = prefetch.missing_pages_remain;
                    if prefetch.is_complete() {
                        let cursor = self
                            .alignment_gpu_prefetch
                            .summary_cursor
                            .take()
                            .expect("completed summary prefetch retains its cursor");
                        if cursor.retention_limited() {
                            self.alignment_gpu_prefetch.summary_retention_limited = true;
                            tracing::debug!(
                                retained_bytes = cursor.retained_bytes(),
                                optional_pin_ceiling,
                                "summary GPU warm-up completed in exact demand-paged mode"
                            );
                        } else {
                            match self
                                .alignment_summary_gpu_renderer
                                .finish_prepared_scene_prefetch_cursor(cursor)
                            {
                                Ok(lease) => {
                                    self.alignment_gpu_prefetch.summary_residency = Some(lease)
                                }
                                Err(error) => {
                                    pressure = true;
                                    tracing::debug!(%error, "could not finalize summary GPU residency");
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    pressure = true;
                    tracing::debug!(%error, "summary GPU prefetch yielded to foreground capacity");
                }
            }
        }

        if attempted_quantum {
            self.alignment_gpu_prefetch
                .observe_quantum(quantum_started.elapsed(), processed_pages);
            self.alignment_gpu_prefetch.summaries_next = !summaries_next;
        }

        if pressure {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            return;
        }
        let reads_finished = read_requests.is_empty()
            || self.alignment_gpu_prefetch.read_residency.is_some()
            || self.alignment_gpu_prefetch.read_retention_limited;
        let summaries_finished = summary_requests.is_empty()
            || self.alignment_gpu_prefetch.summary_residency.is_some()
            || self.alignment_gpu_prefetch.summary_retention_limited;
        let retention_limited = self.alignment_gpu_prefetch.read_retention_limited
            || self.alignment_gpu_prefetch.summary_retention_limited;
        if reads_finished
            && summaries_finished
            && !retention_limited
            && self
                .alignment_gpu_residency_bundle
                .as_ref()
                .is_none_or(|resident| resident.authority != authority)
        {
            let reads = self.alignment_gpu_prefetch.read_residency.take();
            let summaries = self.alignment_gpu_prefetch.summary_residency.take();
            if let Some(request) = read_requests.first() {
                self.session.data_status = format!(
                    "{} complete alignments · all {} rows ready for instant browsing",
                    request.scene.evidence().record_count(),
                    request.scene.atlas().row_count()
                );
            }
            self.alignment_gpu_residency_bundle = Some(AlignmentGpuResidencyBundle {
                authority,
                _reads: reads,
                _summaries: summaries,
            });
        } else if !self.alignment_gpu_prefetch.pending() {
            // A progressively prepared read scene may not yet span every atlas row. Do not retain
            // a partial all-scene residency claim; the next content epoch will restart from its
            // own exact authority.
            self.alignment_gpu_prefetch.read_residency = None;
            self.alignment_gpu_prefetch.summary_residency = None;
            if retention_limited {
                self.report_warning("Complete alignments remain available on demand; full-stack instant residency exceeds the current cache budget");
            }
        }
        let pending = self.alignment_gpu_prefetch.pending();
        self.alignment_gpu_speculation_gate
            .finish_cycle(pending, retention_limited);
        if pending {
            self.window.request_redraw();
        }
    }
}
