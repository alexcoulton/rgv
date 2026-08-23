#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn verify_automation_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let batch_artifacts = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.media_type == "application/vnd.rgv.automation-batch+json")
        .collect::<Vec<_>>();
    ensure!(
        batch_artifacts.len() == 3,
        "MF-15 expected three generated automation batches"
    );
    let mut export_kinds = Vec::new();
    let mut snapshot_formats = Vec::new();
    let mut controlled_missing_source = false;
    for artifact in batch_artifacts {
        let batch = AutomationBatch::read(root.join(&artifact.path))?;
        for command in batch.commands {
            match command {
                AutomationCommand::Export { kind, .. } => {
                    export_kinds.push(kind);
                }
                AutomationCommand::Snapshot { format, .. } => {
                    snapshot_formats.push(format);
                }
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local { path },
                    ..
                } if path.contains("does-not-exist.bam") => controlled_missing_source = true,
                _ => {}
            }
        }
    }
    ensure!(
        [
            AutomationExportKind::Alignments,
            AutomationExportKind::Pileup,
            AutomationExportKind::SelectedEvidence,
            AutomationExportKind::Modifications,
            AutomationExportKind::Junctions,
            AutomationExportKind::Variants,
            AutomationExportKind::Quantitative,
            AutomationExportKind::Regions,
            AutomationExportKind::TrackNames,
        ]
        .iter()
        .all(|kind| export_kinds.contains(kind))
            && snapshot_formats.contains(&AutomationSnapshotFormat::Png)
            && snapshot_formats.contains(&AutomationSnapshotFormat::Svg)
            && controlled_missing_source,
        "MF-15 batches do not cover every export, both snapshot formats, and a controlled failure"
    );
    Ok(())
}

pub(crate) fn verify_preconfigured_specialist_projects(
    root: &Path,
    manifest: &FixtureManifest,
) -> Result<()> {
    let bisulfite = project_document(root, manifest, "mf-04-bisulfite")?;
    let bisulfite_modes = bisulfite
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Bisulfite(context) => Some(context),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        bisulfite_modes.len() == 6
            && [
                BisulfiteContext::Cg,
                BisulfiteContext::Chg,
                BisulfiteContext::Chh,
                BisulfiteContext::Hcg,
                BisulfiteContext::Gch,
                BisulfiteContext::Wcg,
            ]
            .iter()
            .all(|mode| bisulfite_modes.contains(mode)),
        "MF-04 does not preconfigure all six bisulfite contexts"
    );

    let smrt = project_document(root, manifest, "mf-05-smrt-kinetics")?;
    let smrt_modes = smrt
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => Some(mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        smrt_modes.len() == 6
            && [
                SmrtKineticsMode::SubreadIpd,
                SmrtKineticsMode::SubreadPulseWidth,
                SmrtKineticsMode::CcsForwardIpd,
                SmrtKineticsMode::CcsForwardPulseWidth,
                SmrtKineticsMode::CcsReverseIpd,
                SmrtKineticsMode::CcsReversePulseWidth,
            ]
            .iter()
            .all(|mode| smrt_modes.contains(mode)),
        "MF-05 does not preconfigure all six SMRT kinetics modes"
    );

    let sbx = project_document(root, manifest, "mf-06-sbx")?;
    let sbx_tail_modes = sbx
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Sbx(options) => Some(options.simplex_tail_mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        sbx_tail_modes.len() == 3
            && [
                SbxSimplexTailMode::Off,
                SbxSimplexTailMode::Highlight,
                SbxSimplexTailMode::Hide,
            ]
            .iter()
            .all(|mode| sbx_tail_modes.contains(mode)),
        "MF-06 does not preconfigure all SBX tail modes"
    );

    let ultima = project_document(root, manifest, "mf-07-ultima")?;
    let ultima_modes = ultima
        .alignment_tracks
        .iter()
        .filter_map(|track| match track.filters.alignment_color_mode {
            AlignmentColorMode::Ultima(options) => Some(options.quality_mode),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        ultima_modes.len() == 2
            && [UltimaQualityMode::Directional, UltimaQualityMode::Minimum]
                .iter()
                .all(|mode| ultima_modes.contains(mode)),
        "MF-07 does not preconfigure both Ultima quality modes"
    );
    Ok(())
}
