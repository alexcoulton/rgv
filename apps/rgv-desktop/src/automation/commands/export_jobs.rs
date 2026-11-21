use crate::app::tasks::{ExportTask, NativeWindowCaptureTask};
use crate::automation::{
    automation_alignments_export, automation_inspector_export, automation_junctions_export,
    automation_modifications_export, automation_pileup_export, automation_quantitative_export,
    automation_snapshot_metadata, automation_track_names_export, automation_variants_export,
    create_output_parent,
};
use crate::inspection::auxiliary::gwas_inspector_fields;
use crate::inspection::model::InspectorSnapshot;
use crate::inspection::rna::{AutomationAlignmentExportAuthority, AutomationAlignmentExportTrack};
use crate::inspection::variant::{variant_evidence_export_tsv, variant_genotype_export_tsv};
use crate::workspace_lists::region_bed_export;
use crate::{Viewer, ui};
use anyhow::{Context as _, Result};
use rgv_automation::{AutomationExportKind, AutomationSnapshotFormat, AutomationSnapshotScope};
use rgv_engine::{AlignmentIndividualReadCapability, TrackSnapshotData};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_automation_snapshot(
        &mut self,
        command_index: usize,
        path: PathBuf,
        scope: AutomationSnapshotScope,
        format: AutomationSnapshotFormat,
        scale: f32,
    ) -> Result<()> {
        if scope == AutomationSnapshotScope::NativeWindow {
            anyhow::ensure!(
                format == AutomationSnapshotFormat::Png,
                "native window snapshots support PNG only"
            );
            anyhow::ensure!(
                self.pending_native_window_capture.is_none(),
                "a native window capture is already in progress"
            );
            anyhow::ensure!(
                !self.export_tasks.iter().any(|task| task.path == path),
                "an export to {} is already in progress",
                path.display()
            );
            let width = self.surface_config.width;
            let height = self.surface_config.height;
            let navigation_transaction_active = self
                .panels
                .iter()
                .any(|panel| panel.navigation_presentation.is_unsettled());
            self.pending_native_window_capture = Some(NativeWindowCaptureTask {
                path,
                automation_command: command_index,
                automation_data: serde_json::json!({
                    "width": width,
                    "height": height,
                    "scope": scope,
                    "scope_description": "native window client area",
                    "truncated": false,
                    "application_instance_id": self.application_instance_id,
                    "workspace_revision": self.workspace_revision,
                    "navigation_transaction_active": navigation_transaction_active,
                    "selection_id": self.automation_selection_snapshot().map(|selection| selection.selection_id),
                    "evidence_revision": self.automation_selection_snapshot().map(|selection| selection.evidence_revision),
                }),
            });
            self.mark_display_dirty("explicit display invalidation");
            self.window.request_redraw();
            return Ok(());
        }
        let capture = self.capture_semantic_export(scope)?;
        let width = capture.width;
        let height = capture.height;
        let scope_label = capture.scope_label.clone();
        let mut metadata = automation_snapshot_metadata(
            &self.assembly_name,
            &self.panels,
            &self.alignment_tracks,
            &self.tracks,
            &self.track_presentation,
            &scope_label,
        );
        let _ = write!(
            metadata,
            "; application instance {}; workspace revision {}",
            self.application_instance_id, self.workspace_revision
        );
        if let Some(selection) = self.automation_selection_snapshot() {
            let _ = write!(
                metadata,
                "; selection {}; evidence revision {}",
                selection.selection_id, selection.evidence_revision
            );
        }
        let truncated = scope_label.contains("truncated");
        // `scale` is the command's PNG pixel density; it defaults to the historical one pixel per
        // point (REND-9) and was validated with the batch.
        let completion_data = serde_json::json!({
            "width": width,
            "height": height,
            "scope": scope,
            "scope_description": scope_label,
            "truncated": truncated,
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "selection_id": self.automation_selection_snapshot().map(|selection| selection.selection_id),
            "evidence_revision": self.automation_selection_snapshot().map(|selection| selection.evidence_revision),
        });
        let gpu = self.export_gpu();
        self.start_automation_export_job_with_data(
            command_index,
            path,
            completion_data,
            move |path| capture.write(path, format, &metadata, scale, gpu),
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_automation_export(
        &mut self,
        command_index: usize,
        path: PathBuf,
        kind: AutomationExportKind,
    ) -> Result<()> {
        let assembly = self.assembly_name.clone();
        let contig = self.panels.active().contig_name.clone();
        let interval = self.panels.active().viewport.interval();
        match kind {
            AutomationExportKind::Regions => {
                let regions = self.regions_of_interest.clone();
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, region_bed_export(&regions))
                })
            }
            AutomationExportKind::TrackNames => {
                let contents = automation_track_names_export(
                    &self.assembly_name,
                    &self.panels,
                    &self.alignment_tracks,
                    &self.tracks,
                    &self.track_presentation,
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::Alignments
            | AutomationExportKind::FilteredAlignments
            | AutomationExportKind::EligibleAlignments
            | AutomationExportKind::Pileup
            | AutomationExportKind::Modifications
            | AutomationExportKind::Junctions => {
                let presentation_committed = self
                    .panels
                    .active()
                    .navigation_presentation
                    .is_interactive();
                anyhow::ensure!(
                    presentation_committed,
                    "the target viewport is still being checked"
                );
                let mut tracks = Vec::new();
                for track in self.alignment_tracks.iter().filter(|track| {
                    self.track_presentation
                        .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                        .unwrap_or(false)
                        || self
                            .track_presentation
                            .track_content_exposed(ui::TrackPane::Reads, track.config.id)
                            .unwrap_or(false)
                }) {
                    anyhow::ensure!(
                        track.scene_engine.is_some(),
                        "alignment source '{}' is not supported by the unified scene backend",
                        track.config.name
                    );
                    let presented = self
                        .presented_alignment_lane(self.panels.active().id, track.config.id)
                        .context("the committed alignment scene is unavailable")?;
                    anyhow::ensure!(
                        presented.lane.summary_current()
                            && presented.source.semantic_target_complete,
                        "the committed alignment scene has not settled for the current viewport"
                    );
                    let snapshot = presented
                        .lane
                        .snapshot
                        .as_ref()
                        .context("the committed alignment scene has no semantic snapshot")?;
                    anyhow::ensure!(
                        snapshot.prepared_interval().contains_interval(interval),
                        "the committed alignment scene does not cover the export interval"
                    );
                    if matches!(
                        kind,
                        AutomationExportKind::Alignments
                            | AutomationExportKind::FilteredAlignments
                            | AutomationExportKind::EligibleAlignments
                    ) {
                        anyhow::ensure!(
                            matches!(
                                presented.source.individual_reads,
                                AlignmentIndividualReadCapability::Complete { scene_revision }
                                    if scene_revision == snapshot.revision()
                            ) && presented.read_authority.is_some(),
                            "complete reads unavailable: the committed alignment scene refused individual reads"
                        );
                    }
                    tracks.push(AutomationAlignmentExportTrack {
                        id: track.config.id,
                        name: track.config.name.clone(),
                        complete_read_limit: track.scene_complete_read_limit,
                        authority: AutomationAlignmentExportAuthority {
                            snapshot: Arc::clone(snapshot),
                        },
                    });
                }
                anyhow::ensure!(
                    !tracks.is_empty(),
                    "no visible settled alignment data is available to export"
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    let contents = match kind {
                        AutomationExportKind::Alignments
                        | AutomationExportKind::FilteredAlignments => automation_alignments_export(
                            &assembly, &contig, interval, &tracks, false,
                        ),
                        AutomationExportKind::EligibleAlignments => automation_alignments_export(
                            &assembly, &contig, interval, &tracks, true,
                        ),
                        AutomationExportKind::Pileup => {
                            automation_pileup_export(&assembly, &contig, interval, &tracks)
                        }
                        AutomationExportKind::Modifications => {
                            automation_modifications_export(&assembly, &contig, interval, &tracks)
                        }
                        AutomationExportKind::Junctions => {
                            automation_junctions_export(&assembly, &contig, interval, &tracks)
                        }
                        _ => unreachable!(),
                    }
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::Variants | AutomationExportKind::Quantitative => {
                let visible_ids = self
                    .tracks
                    .tracks()
                    .iter()
                    .filter(|track| {
                        self.track_presentation
                            .track_content_exposed(ui::TrackPane::Annotation, track.id)
                            .unwrap_or(false)
                    })
                    .map(|track| track.id)
                    .collect::<BTreeSet<_>>();
                let snapshots = self
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .filter(|snapshot| visible_ids.contains(&snapshot.id))
                    .cloned()
                    .collect::<Vec<_>>();
                let has_compatible = snapshots.iter().any(|snapshot| match kind {
                    AutomationExportKind::Variants => {
                        matches!(snapshot.data, TrackSnapshotData::Variants(_))
                    }
                    AutomationExportKind::Quantitative => {
                        matches!(snapshot.data, TrackSnapshotData::Quantitative(_))
                    }
                    _ => false,
                });
                anyhow::ensure!(
                    has_compatible,
                    "no visible settled data is available to export"
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    let contents = if kind == AutomationExportKind::Variants {
                        automation_variants_export(&assembly, &contig, interval, &snapshots)
                    } else {
                        automation_quantitative_export(&assembly, &contig, interval, &snapshots)
                    };
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::SelectedEvidence => {
                if let Some(retained) = self.panels.active().retained_evidence.clone()
                    && !retained.export.is_empty()
                {
                    anyhow::ensure!(
                        !retained.selection.stale,
                        "selected evidence is stale; refresh it explicitly before export"
                    );
                    let mut digest = String::with_capacity(64);
                    for byte in Sha256::digest(retained.export.as_bytes()) {
                        let _ = write!(digest, "{byte:02x}");
                    }
                    let data = serde_json::json!({
                        "selection_id": retained.selection.selection_id,
                        "evidence_revision": retained.selection.evidence_revision,
                        "workspace_revision": self.workspace_revision,
                        "bytes": retained.export.len(),
                        "sha256": digest,
                    });
                    return self.start_automation_export_job_with_data(
                        command_index,
                        path,
                        data,
                        move |path| std::fs::write(path, retained.export),
                    );
                }
                let contents = if let Some(inspector) = self.variant_inspector_snapshot() {
                    let evidence = self.variant_alignment_evidence(&inspector);
                    format!(
                        "{}\n{}",
                        variant_genotype_export_tsv(&inspector),
                        variant_evidence_export_tsv(&inspector, &evidence)
                    )
                } else if let Some(inspector) = self.gwas_inspector_snapshot() {
                    automation_inspector_export(&InspectorSnapshot {
                        fields: gwas_inspector_fields(&inspector),
                        auxiliary: Vec::new(),
                        event_sequence: None,
                        cigar: None,
                        read_sequence: None,
                    })
                } else {
                    let inspector = self
                        .inspector_snapshot()
                        .or_else(|| self.pileup_inspector_snapshot())
                        .or_else(|| self.modification_inspector_snapshot())
                        .or_else(|| self.junction_inspector_snapshot())
                        .context("no selected evidence is available to export")?;
                    automation_inspector_export(&inspector)
                };
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, contents)
                })
            }
        }
    }

    pub(crate) fn start_automation_export_job(
        &mut self,
        command_index: usize,
        path: PathBuf,
        job: impl FnOnce(PathBuf) -> std::io::Result<()> + Send + 'static,
    ) -> Result<()> {
        self.start_automation_export_job_with_data(command_index, path, serde_json::json!({}), job)
    }

    pub(crate) fn start_automation_export_job_with_data(
        &mut self,
        command_index: usize,
        path: PathBuf,
        completion_data: serde_json::Value,
        job: impl FnOnce(PathBuf) -> std::io::Result<()> + Send + 'static,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.export_tasks.iter().any(|task| task.path == path),
            "an export to {} is already in progress",
            path.display()
        );
        let display_path = path.display().to_string();
        let worker = std::thread::Builder::new()
            .name("rgv-automation-export".to_owned())
            .spawn({
                let path = path.clone();
                move || {
                    create_output_parent(&path)?;
                    job(path)
                }
            })?;
        self.export_tasks.push(ExportTask {
            path,
            automation_command: Some(command_index),
            automation_data: Some(completion_data),
            worker,
        });
        self.data_status = format!("Automation is exporting {display_path}…");
        Ok(())
    }

    pub(crate) fn start_automation_session_save(
        &mut self,
        command_index: usize,
        path: PathBuf,
    ) -> Result<()> {
        self.try_start_session_save(path, Some(command_index))
    }
}
