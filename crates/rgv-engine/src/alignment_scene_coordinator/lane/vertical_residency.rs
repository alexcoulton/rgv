use std::{collections::BTreeSet, num::NonZeroU32, ops::Range, time::Instant};

use rgv_alignment_scene::{
    AlignmentPresentationStore, AlignmentPresentationStoreError, AlignmentSceneSnapshot,
    CanonicalRowBlock, GeometryLod, ReservationKind,
};

use crate::{SourceIntentKind, SourceOwnerId};

use super::{
    AlignmentPresentationPump, AlignmentSceneCoordinatorConfigError,
    AlignmentSceneCoordinatorError, AlignmentSceneSourceLane, AlignmentVerticalResidency,
    AlignmentVerticalResidencyPumpOutcome, AlignmentVerticalResidencyPumpTelemetry,
    AllocationPressure, VerticalReadResidency, VerticalReadResidencyAuthority, elapsed_nanoseconds,
    presentation_owner, usize_as_u64,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn vertical_residency_state(
        &self,
        scene: &AlignmentSceneSnapshot,
    ) -> AlignmentVerticalResidency {
        if !self.policy.full_vertical_residency
            || !supports_full_vertical_residency(self.contract.presentation.geometry_lod)
        {
            return AlignmentVerticalResidency::ViewportBounded;
        }
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
        let prepared_rows = self.warmed_vertical_row_count(scene);
        if prepared_rows == total_rows {
            AlignmentVerticalResidency::Complete { total_rows }
        } else if self.vertical_residency_budget_limited {
            AlignmentVerticalResidency::BudgetLimited {
                prepared_rows,
                total_rows,
            }
        } else if self.vertical_residency_failed {
            AlignmentVerticalResidency::Unavailable {
                prepared_rows,
                total_rows,
            }
        } else {
            AlignmentVerticalResidency::Preparing {
                prepared_rows,
                total_rows,
            }
        }
    }

    pub(crate) fn vertical_residency_authority(
        &self,
        scene: &AlignmentSceneSnapshot,
    ) -> VerticalReadResidencyAuthority {
        VerticalReadResidencyAuthority {
            evidence_revision: scene.reads().evidence().revision(),
            atlas_revision: scene.reads().atlas().revision(),
            presentation: scene.reads().presentation(),
            prepared_interval: scene.prepared_interval(),
            geometry_tile_bases: self.contract.geometry_tile_bases,
            row_block_rows: self.contract.row_block_rows,
        }
    }

    pub(crate) fn observe_scene_for_vertical_residency(&mut self, scene: &AlignmentSceneSnapshot) {
        let authority = self.vertical_residency_authority(scene);
        let residency = self
            .vertical_read_residency
            .get_or_insert_with(|| VerticalReadResidency {
                authority: authority.clone(),
                warmed_blocks: BTreeSet::new(),
            });
        if residency.authority != authority {
            *residency = VerticalReadResidency {
                authority,
                warmed_blocks: BTreeSet::new(),
            };
            self.vertical_residency_failed = false;
        }
        residency
            .warmed_blocks
            .extend(scene.reads().page_keys().iter().map(|key| key.row_block()));
    }

    pub(crate) fn warmed_vertical_row_count(&self, scene: &AlignmentSceneSnapshot) -> u32 {
        let authority = self.vertical_residency_authority(scene);
        let total_rows = scene.reads().atlas().row_count();
        let Some(residency) = self
            .vertical_read_residency
            .as_ref()
            .filter(|residency| residency.authority == authority)
        else {
            return scene
                .reads()
                .prepared_rows()
                .end
                .saturating_sub(scene.reads().prepared_rows().start);
        };
        residency
            .warmed_blocks
            .iter()
            .filter_map(|block| block.range(total_rows).ok())
            .fold(0_u32, |rows, range| {
                rows.saturating_add(range.end.saturating_sub(range.start))
            })
    }

    /// Reconciles the lane's exact row-block directory with semantic cache ownership after a
    /// bounded eviction pass. Published snapshot leases remain valid independently; this method
    /// only subtracts blocks whose complete reusable page set is no longer cached.
    pub(crate) fn reconcile_vertical_residency_cache(
        &mut self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> usize {
        let Some(scene) = presentation
            .prepared_for(presentation_owner(owner))
            .cloned()
        else {
            let removed = self
                .vertical_read_residency
                .as_ref()
                .map_or(0, |residency| residency.warmed_blocks.len());
            self.vertical_read_residency = None;
            return removed;
        };
        let authority = self.vertical_residency_authority(&scene);
        let Some(residency) = self
            .vertical_read_residency
            .as_mut()
            .filter(|residency| residency.authority == authority)
        else {
            return 0;
        };
        let before = residency.warmed_blocks.len();
        residency.warmed_blocks.retain(|row_block| {
            presentation
                .read_row_block_is_fully_cached(
                    scene.reads().evidence(),
                    scene.reads().atlas(),
                    authority.presentation,
                    authority.prepared_interval,
                    *row_block,
                    self.contract.contig_length,
                    authority.geometry_tile_bases,
                )
                .unwrap_or(false)
        });
        before.saturating_sub(residency.warmed_blocks.len())
    }

    pub(crate) fn needs_vertical_residency(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        if !self.policy.full_vertical_residency
            || self.optional_work_paused
            || self.vertical_residency_budget_limited
            || self.vertical_residency_failed
            || !supports_full_vertical_residency(self.contract.presentation.geometry_lod)
        {
            return false;
        }
        let Some(desired) = &self.desired else {
            return false;
        };
        if !desired.camera.has_visible_rows() {
            return false;
        }
        let Some(scene) = presentation.prepared_for(presentation_owner(owner)) else {
            return false;
        };
        let (available_presentation, _) =
            self.available_presentation_contract(desired.source_horizon);
        if !self
            .frame(owner, presentation)
            .is_some_and(|frame| frame.visible_complete())
            || scene.reads().presentation() != available_presentation
            || !self.scene_roots_satisfy_contract(scene)
            || !scene
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
            || !scene
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
        {
            return false;
        }
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
        self.warmed_vertical_row_count(scene) < total_rows
    }

    pub(crate) fn expand_vertical_residency_once<E>(
        &mut self,
        owner: SourceOwnerId,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<AlignmentPresentationPump, AlignmentSceneCoordinatorError<E>> {
        let pump_started = Instant::now();
        let desired = self
            .desired
            .clone()
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let current = presentation
            .prepared_for(presentation_owner(owner))
            .cloned()
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let total_rows = u32::try_from(current.reads().atlas().row_count())
            .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
        self.observe_scene_for_vertical_residency(&current);
        let authority = self.vertical_residency_authority(&current);
        let target_block = next_unwarmed_vertical_block(
            self.vertical_read_residency
                .as_ref()
                .filter(|residency| residency.authority == authority)
                .map(|residency| &residency.warmed_blocks)
                .expect("observing the current scene establishes vertical residency authority"),
            total_rows,
            self.contract.row_block_rows,
            desired.camera.visible_rows(),
        )
        .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let target_rows = target_block
            .range(current.reads().atlas().row_count())
            .map_err(AlignmentPresentationStoreError::from)?;
        self.telemetry.vertical_residency_checkpoints = self
            .telemetry
            .vertical_residency_checkpoints
            .saturating_add(1);
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let prepare_started = Instant::now();
        let prepared = presentation.warm_read_row_block(
            current.reads().evidence(),
            current.reads().atlas(),
            presentation_contract,
            current.prepared_interval(),
            target_block,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            reference,
            ReservationKind::Normal,
        );
        let prepare_elapsed_nanoseconds = elapsed_nanoseconds(prepare_started);
        let candidate = match prepared {
            Ok(candidate) => candidate,
            Err(error) if error.allocation_pressure().is_some() => {
                let pressure = error
                    .allocation_pressure()
                    .expect("the guarded presentation error has allocation pressure");
                self.vertical_residency_budget_limited = true;
                self.telemetry.vertical_residency_budget_limits = self
                    .telemetry
                    .vertical_residency_budget_limits
                    .saturating_add(1);
                self.record_vertical_residency_pump(
                    pump_started,
                    prepare_elapsed_nanoseconds,
                    &current,
                    target_rows,
                    total_rows,
                    None,
                    None,
                    None,
                    AlignmentVerticalResidencyPumpOutcome::BudgetLimited,
                );
                return Ok(AlignmentPresentationPump {
                    owner,
                    published_scene: None,
                    intent: SourceIntentKind::Speculative,
                    budget_limited: true,
                    failed: false,
                    completed: false,
                    pressure: Some(pressure),
                });
            }
            Err(_error) => {
                // Full-height warming is optional once the visible scene is complete. Latch one
                // failure for this authority so a deterministic invariant error cannot consume a
                // core in an unbounded retry loop or fail the foreground navigation transaction.
                self.vertical_residency_failed = true;
                self.telemetry.vertical_residency_failures =
                    self.telemetry.vertical_residency_failures.saturating_add(1);
                self.record_vertical_residency_pump(
                    pump_started,
                    prepare_elapsed_nanoseconds,
                    &current,
                    target_rows,
                    total_rows,
                    None,
                    None,
                    None,
                    AlignmentVerticalResidencyPumpOutcome::Failed,
                );
                return Ok(AlignmentPresentationPump {
                    owner,
                    published_scene: None,
                    intent: SourceIntentKind::Speculative,
                    budget_limited: false,
                    failed: true,
                    completed: false,
                    pressure: None,
                });
            }
        };
        self.vertical_read_residency
            .as_mut()
            .expect("a successful warm retains its observed authority")
            .warmed_blocks
            .insert(candidate.row_block);
        self.telemetry.vertical_residency_blocks_warmed = self
            .telemetry
            .vertical_residency_blocks_warmed
            .saturating_add(1);
        let completed = self.warmed_vertical_row_count(&current) == total_rows;
        if completed {
            self.telemetry.vertical_residency_completions = self
                .telemetry
                .vertical_residency_completions
                .saturating_add(1);
        }
        self.record_vertical_residency_pump(
            pump_started,
            prepare_elapsed_nanoseconds,
            &current,
            target_rows,
            total_rows,
            Some(usize_as_u64(candidate.page_occurrences)),
            Some(usize_as_u64(candidate.cache_misses)),
            None,
            if completed {
                AlignmentVerticalResidencyPumpOutcome::Completed
            } else {
                AlignmentVerticalResidencyPumpOutcome::Warmed
            },
        );
        Ok(AlignmentPresentationPump {
            owner,
            published_scene: None,
            intent: SourceIntentKind::Speculative,
            budget_limited: false,
            failed: false,
            completed,
            pressure: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_vertical_residency_pump(
        &mut self,
        pump_started: Instant,
        prepare_elapsed_nanoseconds: u64,
        current: &AlignmentSceneSnapshot,
        target_prepared_rows: Range<u32>,
        total_rows: u32,
        target_read_pages: Option<u64>,
        target_read_page_cache_misses: Option<u64>,
        target_summary_pages: Option<u64>,
        outcome: AlignmentVerticalResidencyPumpOutcome,
    ) {
        let current_read_pages = usize_as_u64(current.reads().pages().len());
        let current_summary_pages = usize_as_u64(current.summary_pages().len());
        if let Some(pages) = target_read_pages {
            self.telemetry.vertical_residency_read_page_occurrences = self
                .telemetry
                .vertical_residency_read_page_occurrences
                .saturating_add(pages);
        }
        if let Some(misses) = target_read_page_cache_misses {
            self.telemetry.vertical_residency_read_page_cache_misses = self
                .telemetry
                .vertical_residency_read_page_cache_misses
                .saturating_add(misses);
        }
        if let Some(pages) = target_summary_pages {
            self.telemetry.vertical_residency_summary_page_occurrences = self
                .telemetry
                .vertical_residency_summary_page_occurrences
                .saturating_add(pages);
        }
        let pump_elapsed_nanoseconds = elapsed_nanoseconds(pump_started);
        self.telemetry.vertical_residency_prepare_nanoseconds = self
            .telemetry
            .vertical_residency_prepare_nanoseconds
            .saturating_add(prepare_elapsed_nanoseconds);
        self.telemetry.vertical_residency_pump_nanoseconds = self
            .telemetry
            .vertical_residency_pump_nanoseconds
            .saturating_add(pump_elapsed_nanoseconds);
        self.telemetry.last_vertical_residency_pump =
            Some(AlignmentVerticalResidencyPumpTelemetry {
                checkpoint: self.telemetry.vertical_residency_checkpoints,
                current_scene: current.revision(),
                published_scene: None,
                current_prepared_rows: current.reads().prepared_rows().into(),
                target_prepared_rows: target_prepared_rows.into(),
                total_rows,
                current_read_pages,
                current_summary_pages,
                target_read_pages,
                target_summary_pages,
                target_read_page_cache_misses,
                prepare_elapsed_nanoseconds,
                pump_elapsed_nanoseconds,
                outcome,
            });
    }
}

pub(crate) const fn supports_full_vertical_residency(lod: GeometryLod) -> bool {
    matches!(lod, GeometryLod::Span | GeometryLod::Read)
}

/// Chooses one independently keyed row block nearest the live camera. Previously warmed blocks
/// need not form a contiguous prefix or interval, so changing vertical direction never rebuilds
/// or republishes the blocks already in the cache. Distance ties favour the usual top-to-bottom
/// browsing direction.
pub(crate) fn next_unwarmed_vertical_block(
    warmed: &BTreeSet<CanonicalRowBlock>,
    total_rows: u32,
    block_rows: NonZeroU32,
    visible: Range<u32>,
) -> Option<CanonicalRowBlock> {
    if total_rows == 0 {
        return None;
    }
    let rows = block_rows.get();
    let block_count = total_rows.div_ceil(rows);
    let visible_middle = visible
        .start
        .saturating_add(visible.end.saturating_sub(visible.start) / 2)
        .min(total_rows.saturating_sub(1));
    let visible_block = visible_middle / rows;
    (0..block_count)
        .filter(|coordinate| !warmed.contains(&CanonicalRowBlock::new(*coordinate, block_rows)))
        .min_by_key(|coordinate| {
            (
                coordinate.abs_diff(visible_block),
                u8::from(*coordinate < visible_block),
                *coordinate,
            )
        })
        .map(|coordinate| CanonicalRowBlock::new(coordinate, block_rows))
}
