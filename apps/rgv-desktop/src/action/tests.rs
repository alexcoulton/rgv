//! The window's track-arrangement requests and the automation protocol's arrangement settings
//! leave the same document. The rest of the action tests live with the reducer in `rgv-session`.

use super::Action;
use super::translate::automation_command_actions;
use crate::app::actions::stateless_track_intent;
use crate::ui::TrackUiAction;
use rgv_automation::{AutomationCommand, AutomationSetting};
use rgv_core::{
    PanelId, PresentationNodeId, SessionAlignmentTrack, TrackChannel, TrackConfig, TrackId,
    TrackIntent, TrackKind, TrackSource, WorkspacePresentation,
};

const A: PanelId = PanelId::new(1);

fn presentation() -> WorkspacePresentation {
    let source = |path: &str| TrackSource::Local {
        path: path.to_owned(),
    };
    let alignments = [
        SessionAlignmentTrack::new(TrackId::new(1), "Tumour".to_owned(), source("tumour.bam")),
        SessionAlignmentTrack::new(TrackId::new(2), "Normal".to_owned(), source("normal.bam")),
    ];
    let tracks = [
        TrackConfig::new(
            TrackId::new(3),
            "Genes".to_owned(),
            TrackKind::GeneModel,
            source("genes.gff3.gz"),
        ),
        TrackConfig::new(
            TrackId::new(4),
            "Peaks".to_owned(),
            TrackKind::Feature,
            source("peaks.bed.gz"),
        ),
    ];
    WorkspacePresentation::from_legacy(&alignments, &tracks)
}

fn presentation_document(presentation: &WorkspacePresentation) -> String {
    serde_json::to_string(presentation).unwrap()
}

fn setting_intents(setting: AutomationSetting) -> Vec<TrackIntent> {
    let actions = automation_command_actions(&AutomationCommand::Configure { setting }, A)
        .expect("arrangement settings are on the action path")
        .unwrap();
    match actions.as_slice() {
        [Action::ApplyTrackIntents { intents }] => intents.clone(),
        other => panic!("unexpected translation {other:?}"),
    }
}

#[test]
fn track_arrangement_settings_and_gutter_requests_leave_the_same_document() {
    let genes = PresentationNodeId::Track {
        channel: TrackChannel::Annotation,
        track_id: TrackId::new(3),
    };
    let reads = PresentationNodeId::Track {
        channel: TrackChannel::Reads,
        track_id: TrackId::new(2),
    };
    let cases = [
        (
            AutomationSetting::PresentationNodeVisible {
                target: genes,
                value: false,
            },
            TrackUiAction::SetNodeVisibility(genes, false),
        ),
        (
            AutomationSetting::PresentationNodeVisible {
                target: reads,
                value: false,
            },
            TrackUiAction::SetNodeVisibility(reads, false),
        ),
    ];
    for (setting, request) in cases {
        let mut by_automation = presentation();
        let mut by_window = presentation();
        let before = presentation_document(&by_automation);
        by_automation
            .apply_intents(&setting_intents(setting.clone()))
            .unwrap();
        let intent = stateless_track_intent(&request).expect("the request is a track intent");
        by_window.apply_intents(&[intent]).unwrap();
        assert_eq!(
            presentation_document(&by_automation),
            presentation_document(&by_window),
            "{setting:?}"
        );
        assert_ne!(presentation_document(&by_automation), before, "{setting:?}");
    }
}
