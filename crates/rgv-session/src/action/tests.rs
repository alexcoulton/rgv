//! The action path, driven without a window: explicit panel identity, the two front ends
//! leaving the same document, and undo round trips.
//!
//! "The same document" is decided on the serialised session projection: the panel part comes
//! from the function the desktop's `session_document` uses, and the track arrangement is the session's
//! `presentation` value itself.

use super::history::{UndoEntry, UndoHistory};
use super::reduce::{PanelContext, Reduced, reduce_panel_action};
use super::translate::{
    automation_command_actions, keyboard_pan, keyboard_zoom, locus_box, panel_strip_requests,
};
use super::{Action, Effect, ViewportCommit};
use crate::panels::{PanelState as ViewPanelController, PanelWorkspace, session_panel_documents};
use rgv_automation::{
    AutomationCommand, AutomationHistoryNavigation, AutomationSelection, AutomationSetting,
    AutomationViewportOperation,
};
use rgv_core::{
    ContigAliases, ContigId, GenomicInterval, PanelId, PresentationNodeId, ReferenceDictionary,
    SessionAlignmentTrack, TrackChannel, TrackConfig, TrackId, TrackIntent, TrackKind, TrackSource,
    Viewport, WorkspacePresentation,
};
use std::time::Instant;

const A: PanelId = PanelId::new(1);
const B: PanelId = PanelId::new(2);

fn dictionary() -> ReferenceDictionary {
    ReferenceDictionary::from_contigs([
        ("chr1".to_owned(), 1_000_000),
        ("chr2".to_owned(), 500_000),
    ])
}

fn panel(id: PanelId, start: u64, end: u64) -> ViewPanelController {
    let interval = GenomicInterval::new(ContigId::new(0), start, end).unwrap();
    ViewPanelController::new(
        id,
        Viewport::new(interval, 600.0).unwrap(),
        None,
        "chr1".to_owned(),
        1_000_000,
    )
}

/// Panels A (focused) and B, side by side on chr1.
fn two_panels() -> PanelWorkspace {
    let mut panels = PanelWorkspace::new(panel(A, 10_000, 20_000));
    assert!(panels.push(panel(B, 300_000, 310_000)));
    assert_eq!(panels.active().id, A);
    panels
}

struct Harness {
    dictionary: ReferenceDictionary,
    aliases: ContigAliases,
}

impl Harness {
    fn new() -> Self {
        Self {
            dictionary: dictionary(),
            aliases: ContigAliases::default(),
        }
    }

    fn reduce(&self, panels: &mut PanelWorkspace, action: &Action) -> anyhow::Result<Reduced> {
        let canvas_width_for = |count: usize| 1_200.0 / count as f32;
        let ctx = PanelContext {
            dictionary: Some(&self.dictionary),
            fallback_dictionary: &self.dictionary,
            aliases: &self.aliases,
            canvas_width_for: &canvas_width_for,
            content_top: 0.0,
        };
        reduce_panel_action(panels, &ctx, action, &mut |_| {})
    }

    fn apply(&self, panels: &mut PanelWorkspace, actions: &[Action]) -> Vec<Action> {
        let mut inverse = Vec::new();
        for action in actions {
            let mut reduced = self.reduce(panels, action).unwrap();
            reduced.inverse.append(&mut inverse);
            inverse = reduced.inverse;
        }
        inverse
    }
}

/// The serialised panel part of the session document.
fn panel_document(panels: &PanelWorkspace) -> String {
    let (locus, saved, active_panel) = session_panel_documents(panels);
    serde_json::to_string(&serde_json::json!({
        "locus": locus,
        "panels": saved,
        "active_panel": active_panel,
    }))
    .unwrap()
}

fn one_panel_document(panels: &PanelWorkspace, id: PanelId) -> String {
    let (_, saved, _) = session_panel_documents(panels);
    serde_json::to_string(saved.iter().find(|panel| panel.id == id).unwrap()).unwrap()
}

// ----- ARCH-4: explicit panel identity ------------------------------------------------------

#[test]
fn an_operation_on_panel_b_changes_only_b_and_leaves_focus_on_a() {
    let harness = Harness::new();
    let operations = [
        Action::GoToLocus {
            panel_id: B,
            locus: "chr2:1001-2000".to_owned(),
        },
        Action::ChangeViewport {
            panel_id: B,
            operation: AutomationViewportOperation::PanFraction { fraction: 0.5 },
            commit: ViewportCommit::Discrete,
        },
        Action::ChangeViewport {
            panel_id: B,
            operation: AutomationViewportOperation::Zoom {
                factor: 2.0,
                anchor_position: None,
            },
            commit: ViewportCommit::Step,
        },
        Action::NavigateHistory {
            panel_id: B,
            step: AutomationHistoryNavigation::Back,
        },
        Action::NavigateHome { panel_id: B },
    ];
    let mut panels = two_panels();
    let a_before = one_panel_document(&panels, A);
    let a_history_before = panels.panel(A).navigation_history.len();
    for operation in &operations {
        let b_before = one_panel_document(&panels, B);
        let reduced = harness.reduce(&mut panels, operation).unwrap();
        assert!(reduced.outcome.changed, "{operation:?} moved panel B");
        assert_ne!(one_panel_document(&panels, B), b_before, "{operation:?}");
        assert_eq!(one_panel_document(&panels, A), a_before, "{operation:?}");
        assert_eq!(panels.active().id, A, "{operation:?} must not move focus");
        assert!(
            reduced.effects.iter().all(|effect| matches!(
                effect,
                Effect::ViewportChanged { panel_id, .. } if *panel_id == B
            )),
            "{operation:?} asks the host to refresh panel B only"
        );
    }
    assert_eq!(panels.panel(A).navigation_history.len(), a_history_before);
}

#[test]
fn an_action_naming_a_missing_panel_fails_without_touching_the_focused_panel() {
    let harness = Harness::new();
    let mut panels = two_panels();
    let before = panel_document(&panels);
    let missing = PanelId::new(9);
    for action in [
        Action::GoToLocus {
            panel_id: missing,
            locus: "chr2:1-100".to_owned(),
        },
        Action::NavigateHome { panel_id: missing },
        Action::ChangeViewport {
            panel_id: missing,
            operation: AutomationViewportOperation::PanFraction { fraction: 0.1 },
            commit: ViewportCommit::Discrete,
        },
    ] {
        assert!(harness.reduce(&mut panels, &action).is_err(), "{action:?}");
        assert_eq!(panel_document(&panels), before);
    }
}

// ----- ARCH-7: the window and automation leave the same document -----------------------------

fn automation(panels: &PanelWorkspace, command: &AutomationCommand) -> Vec<Action> {
    automation_command_actions(command, panels.active().id)
        .expect("the command is on the action path")
        .unwrap()
}

/// Applies an automation command to one copy of the workspace and the window's requests to
/// another, and requires the same serialised document from both.
fn assert_same_panel_document(
    command: &AutomationCommand,
    window: &dyn Fn(&PanelWorkspace) -> Vec<Action>,
) {
    let harness = Harness::new();
    let mut by_automation = two_panels();
    let mut by_window = two_panels();
    let before = panel_document(&by_automation);
    let actions = automation(&by_automation, command);
    harness.apply(&mut by_automation, &actions);
    let requests = window(&by_window);
    harness.apply(&mut by_window, &requests);
    assert_eq!(
        panel_document(&by_automation),
        panel_document(&by_window),
        "{command:?}"
    );
    assert_ne!(
        panel_document(&by_automation),
        before,
        "{command:?} is not a no-op"
    );
}

#[test]
fn navigation_commands_and_window_navigation_leave_the_same_document() {
    // `goto` and the locus box, one locus: the focused panel moves.
    assert_same_panel_document(
        &AutomationCommand::Goto {
            loci: vec!["chr2:1001-2000".to_owned()],
        },
        &|panels| vec![locus_box(panels.active().id, "chr2:1001-2000").unwrap()],
    );
    // `change_viewport` pan and the arrow key. Automation focuses its panel first; the
    // window's key acts on the focused panel, so the request is preceded by the click that
    // focuses it.
    assert_same_panel_document(
        &AutomationCommand::ChangeViewport {
            panel_id: B,
            operation: AutomationViewportOperation::PanFraction { fraction: 0.1 },
        },
        &|_| {
            let mut requests = panel_strip_requests(None, None, Some(B));
            requests.push(keyboard_pan(B, 0.1));
            requests
        },
    );
    // `change_viewport` zoom and the `+` key.
    assert_same_panel_document(
        &AutomationCommand::ChangeViewport {
            panel_id: A,
            operation: AutomationViewportOperation::Zoom {
                factor: 1.25,
                anchor_position: None,
            },
        },
        &|_| vec![keyboard_zoom(A, 0.8)],
    );
}

#[test]
fn history_commands_and_the_back_button_leave_the_same_document() {
    let harness = Harness::new();
    let visit = Action::GoToLocus {
        panel_id: A,
        locus: "chr2:1001-2000".to_owned(),
    };
    let mut by_automation = two_panels();
    let mut by_window = two_panels();
    harness.apply(&mut by_automation, std::slice::from_ref(&visit));
    harness.apply(&mut by_window, std::slice::from_ref(&visit));
    let away = panel_document(&by_automation);

    let command = AutomationCommand::NavigateHistory {
        panel_id: A,
        action: AutomationHistoryNavigation::Back,
    };
    let actions = automation(&by_automation, &command);
    harness.apply(&mut by_automation, &actions);
    // The Back button steps the focused panel's history.
    harness.apply(
        &mut by_window,
        &[Action::NavigateHistory {
            panel_id: A,
            step: AutomationHistoryNavigation::Back,
        }],
    );
    assert_eq!(panel_document(&by_automation), panel_document(&by_window));
    assert_ne!(panel_document(&by_automation), away);
    assert_eq!(
        panel_document(&by_automation),
        panel_document(&two_panels())
    );
}

#[test]
fn panel_commands_and_the_panel_strip_leave_the_same_document() {
    // Close.
    assert_same_panel_document(&AutomationCommand::ClosePanel { panel_id: B }, &|_| {
        panel_strip_requests(None, Some(B), None)
    });
    // Focus.
    assert_same_panel_document(
        &AutomationCommand::Select {
            selection: AutomationSelection::Panel { panel_id: B },
        },
        &|_| panel_strip_requests(None, None, Some(B)),
    );
    // Add: automation's `add_panel` and the window's "open in new panel" are the same action.
    assert_same_panel_document(
        &AutomationCommand::AddPanel {
            locus: "chr2:5001-6000".to_owned(),
        },
        &|_| {
            vec![Action::AddPanel {
                locus: "chr2:5001-6000".to_owned(),
            }]
        },
    );
    // A multi-locus `goto` and the same text typed into the locus box redefine the panels.
    assert_same_panel_document(
        &AutomationCommand::Goto {
            loci: vec![
                "chr1:101-200".to_owned(),
                "chr2:301-400".to_owned(),
                "chr2:1001-2000".to_owned(),
            ],
        },
        &|panels| {
            vec![
                locus_box(
                    panels.active().id,
                    "chr1:101-200 chr2:301-400 chr2:1001-2000",
                )
                .unwrap(),
            ]
        },
    );
}

#[test]
fn a_multi_locus_goto_validates_every_locus_before_replacing_any_panel() {
    let harness = Harness::new();
    let mut panels = two_panels();
    let before = panel_document(&panels);
    let action = locus_box(A, "chr2:1-100 chrNope:1-100").unwrap();
    assert!(harness.reduce(&mut panels, &action).is_err());
    assert_eq!(panel_document(&panels), before);
}

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

#[test]
fn queries_and_file_commands_are_not_action_translations() {
    for command in [
        AutomationCommand::DescribeWorkspace,
        AutomationCommand::DescribeSelection,
        AutomationCommand::WaitUntilSettled {
            timeout_millis: 1_000,
        },
        AutomationCommand::Configure {
            setting: AutomationSetting::ShowReads {
                value: true,
                track_id: None,
            },
        },
    ] {
        assert!(
            automation_command_actions(&command, A).is_none(),
            "{command:?}"
        );
    }
}

// ----- UI-14: undo and redo ------------------------------------------------------------------

/// A window-less stand-in for `Viewer::apply_action` / `undo_action` / `redo_action` over the
/// panel workspace: the same reducer, the same history, the same inverse bookkeeping.
struct Session {
    harness: Harness,
    panels: PanelWorkspace,
    history: UndoHistory,
}

impl Session {
    fn new() -> Self {
        Self {
            harness: Harness::new(),
            panels: two_panels(),
            history: UndoHistory::default(),
        }
    }

    fn perform(&mut self, action: Action) -> Option<UndoEntry> {
        let label = action.label();
        let reduced = self.harness.reduce(&mut self.panels, &action).unwrap();
        (!reduced.inverse.is_empty()).then(|| UndoEntry {
            label,
            redo: vec![action],
            undo: reduced.inverse,
            coalesce: None,
        })
    }

    fn apply(&mut self, action: Action) {
        if let Some(entry) = self.perform(action) {
            self.history.record(entry, Instant::now());
        }
    }

    fn undo(&mut self) -> bool {
        let Some(entry) = self.history.take_undo() else {
            return false;
        };
        for action in entry.undo.clone() {
            self.perform(action);
        }
        self.history.push_redo(entry);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(entry) = self.history.take_redo() else {
            return false;
        };
        let mut undo = Vec::new();
        for action in entry.redo.clone() {
            if let Some(mut redone) = self.perform(action) {
                redone.undo.append(&mut undo);
                undo = redone.undo;
            }
        }
        self.history.record_redone(UndoEntry { undo, ..entry });
        true
    }

    fn document(&self) -> String {
        panel_document(&self.panels)
    }
}

#[test]
fn panel_edits_round_trip_through_undo_and_redo() {
    let edits = [
        Action::AddPanel {
            locus: "chr2:5001-6000".to_owned(),
        },
        Action::ClosePanel { panel_id: A },
        Action::ClosePanel { panel_id: B },
        Action::MovePanel {
            panel_id: B,
            index: 0,
        },
    ];
    for edit in edits {
        let mut session = Session::new();
        let before = session.document();
        session.apply(edit.clone());
        let after = session.document();
        assert_ne!(after, before, "{edit:?}");

        assert!(session.undo(), "{edit:?}");
        assert_eq!(session.document(), before, "undo of {edit:?}");
        assert!(session.redo(), "{edit:?}");
        assert_eq!(session.document(), after, "redo of {edit:?}");
        assert!(session.undo(), "a redone edit can be undone again");
        assert_eq!(session.document(), before, "second undo of {edit:?}");
        assert!(!session.undo(), "nothing else was recorded");
    }
}

#[test]
fn a_reopened_panel_keeps_its_identity_place_and_focus() {
    let mut session = Session::new();
    session.apply(Action::ClosePanel { panel_id: A });
    assert_eq!(session.panels.active().id, B);
    assert!(session.undo());
    let order = session
        .panels
        .iter()
        .map(|panel| panel.id)
        .collect::<Vec<_>>();
    assert_eq!(order, vec![A, B]);
    assert_eq!(session.panels.active().id, A, "the closed panel had focus");
}

#[test]
fn a_sequence_of_edits_unwinds_in_reverse_and_replays_in_order() {
    let mut session = Session::new();
    let mut documents = vec![session.document()];
    for edit in [
        Action::AddPanel {
            locus: "chr2:5001-6000".to_owned(),
        },
        Action::MovePanel {
            panel_id: PanelId::new(3),
            index: 0,
        },
        Action::ClosePanel { panel_id: B },
    ] {
        session.apply(edit);
        documents.push(session.document());
    }
    for expected in documents.iter().rev().skip(1) {
        assert!(session.undo());
        assert_eq!(&session.document(), expected);
    }
    assert!(!session.undo());
    for expected in documents.iter().skip(1) {
        assert!(session.redo());
        assert_eq!(&session.document(), expected);
    }
    assert!(!session.redo());
}

#[test]
fn navigation_and_focus_are_not_undo_steps() {
    let mut session = Session::new();
    for action in [
        Action::GoToLocus {
            panel_id: B,
            locus: "chr2:1001-2000".to_owned(),
        },
        keyboard_pan(A, 0.25),
        Action::ActivatePanel { panel_id: B },
        Action::NavigateHome { panel_id: B },
    ] {
        session.apply(action);
    }
    assert_eq!(session.history.undo_len(), 0);
    // Back, not Undo, returns a panel to where it was.
    let moved = session.document();
    session.apply(Action::NavigateHistory {
        panel_id: B,
        step: AutomationHistoryNavigation::Back,
    });
    assert_ne!(session.document(), moved);
}

#[test]
fn a_multi_locus_goto_is_one_undo_step_that_restores_the_panel_set() {
    let mut session = Session::new();
    session.apply(locus_box(A, "chr1:101-200 chr2:301-400 chr2:1001-2000").unwrap());
    assert_eq!(session.panels.len(), 3);
    assert_eq!(session.history.undo_len(), 1);
    assert!(session.undo());
    let order = session
        .panels
        .iter()
        .map(|panel| panel.id)
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![A, B],
        "the replaced panel is back, the added ones gone"
    );
    assert_eq!(
        one_panel_document(&session.panels, B),
        one_panel_document(&two_panels(), B)
    );
}

#[test]
fn track_arrangement_edits_round_trip_through_their_snapshot_inverse() {
    // `Viewer` records `RestorePresentation(before)` as the inverse of `ApplyTrackIntents`,
    // and the inverse of that restore is a restore of the state it replaced.
    let before = presentation();
    let mut current = before.clone();
    current
        .apply_intents(&[
            TrackIntent::SetNodeVisibility {
                target: PresentationNodeId::Track {
                    channel: TrackChannel::Annotation,
                    track_id: TrackId::new(4),
                },
                visible: false,
            },
            TrackIntent::ResizeReadPane { height: 420.0 },
        ])
        .unwrap();
    let after = current.clone();
    assert_ne!(
        presentation_document(&after),
        presentation_document(&before)
    );

    let undo = Action::RestorePresentation {
        presentation: Box::new(before.clone()),
    };
    let Action::RestorePresentation { presentation } = &undo else {
        unreachable!()
    };
    let redo_snapshot = std::mem::replace(&mut current, (**presentation).clone());
    assert_eq!(
        presentation_document(&current),
        presentation_document(&before)
    );
    current = redo_snapshot;
    assert_eq!(
        presentation_document(&current),
        presentation_document(&after)
    );
}

#[test]
fn a_divider_drag_coalesces_but_toggles_and_moves_do_not() {
    use super::history::track_intents_coalesce_key;
    let genes = PresentationNodeId::Track {
        channel: TrackChannel::Annotation,
        track_id: TrackId::new(3),
    };
    let peaks = PresentationNodeId::Track {
        channel: TrackChannel::Annotation,
        track_id: TrackId::new(4),
    };
    let resize = |target, preferred_height| TrackIntent::ResizeNode {
        target,
        preferred_height,
    };
    let first = track_intents_coalesce_key(&[resize(genes, 80.0)]);
    assert!(first.is_some());
    assert_eq!(first, track_intents_coalesce_key(&[resize(genes, 96.0)]));
    assert_ne!(first, track_intents_coalesce_key(&[resize(peaks, 96.0)]));
    assert!(track_intents_coalesce_key(&[TrackIntent::ResizeReadPane { height: 300.0 }]).is_some());
    assert_eq!(
        track_intents_coalesce_key(&[TrackIntent::SetNodeVisibility {
            target: genes,
            visible: false,
        }]),
        None
    );
    assert_eq!(
        track_intents_coalesce_key(&[TrackIntent::MoveNode {
            target: genes,
            before: Some(peaks),
        }]),
        None
    );
}

#[test]
fn region_and_review_edits_round_trip_and_typing_is_one_step() {
    use super::reduce::reduce_workspace_lists;
    use rgv_core::{RegionOfInterest, ReviewItem, ReviewStatus, SessionLocus};

    let locus = |start: u64| SessionLocus {
        contig: "chr1".to_owned(),
        start,
        end: start + 100,
    };
    let region = |id: u64, name: &str| RegionOfInterest {
        id,
        name: name.to_owned(),
        locus: locus(id * 1_000),
        color: [45, 92, 175, 48],
    };
    let item = |id: u64, notes: &str| ReviewItem {
        id,
        label: format!("item {id}"),
        locus: locus(id * 5_000),
        status: ReviewStatus::Pending,
        notes: notes.to_owned(),
        assignment: None,
    };
    let document = |regions: &Vec<RegionOfInterest>, review: &Vec<ReviewItem>| {
        serde_json::to_string(&serde_json::json!({
            "regions_of_interest": regions,
            "review_items": review,
        }))
        .unwrap()
    };

    let mut regions = vec![region(1, "Region 1")];
    let mut review = vec![item(1, "")];
    let before = document(&regions, &review);

    // Add a region and a review item: a structural edit.
    let (change, undo) = reduce_workspace_lists(
        &mut regions,
        &mut review,
        vec![region(1, "Region 1"), region(2, "Region 2")],
        vec![item(1, ""), item(2, "")],
    );
    assert!(change.regions_changed && change.review_changed && !change.in_place);
    let after = document(&regions, &review);
    assert_ne!(after, before);

    // Undo, then redo with the inverse the undo returned.
    let Some(Action::SetWorkspaceLists {
        regions: undo_regions,
        review_items: undo_review,
    }) = undo
    else {
        panic!("a changed list has an inverse");
    };
    let (_, redo) = reduce_workspace_lists(&mut regions, &mut review, undo_regions, undo_review);
    assert_eq!(document(&regions, &review), before);
    let Some(Action::SetWorkspaceLists {
        regions: redo_regions,
        review_items: redo_review,
    }) = redo
    else {
        panic!("an undone edit has an inverse");
    };
    reduce_workspace_lists(&mut regions, &mut review, redo_regions, redo_review);
    assert_eq!(document(&regions, &review), after);

    // Typing a note keeps every item in place: it coalesces. Committing the same lists again
    // changes nothing and earns no undo step.
    let (typing, inverse) = reduce_workspace_lists(
        &mut regions,
        &mut review,
        vec![region(1, "Region 1"), region(2, "Renamed")],
        vec![item(1, "looks real"), item(2, "")],
    );
    assert!(typing.in_place && inverse.is_some());
    let unchanged_regions = regions.clone();
    let unchanged_review = review.clone();
    let (idle, inverse) = reduce_workspace_lists(
        &mut regions,
        &mut review,
        unchanged_regions,
        unchanged_review,
    );
    assert!(!idle.regions_changed && !idle.review_changed && inverse.is_none());
}

// ----- Zoom limit: every reducer entry path spans at least `MIN_VISIBLE_BASES` ---------------

/// Start, end and whole-base span of a panel's view.
fn span_of(panels: &PanelWorkspace, id: PanelId) -> (u64, u64, u64) {
    let viewport = panels.get(id).unwrap().viewport;
    let span = viewport.span();
    assert!(
        (span - span.round()).abs() < 1.0e-9,
        "fractional span {span}"
    );
    (
        viewport.interval().start(),
        viewport.interval().end(),
        span.round() as u64,
    )
}

#[test]
fn a_one_base_locus_is_widened_to_the_zoom_limit_and_reported() {
    let harness = Harness::new();
    let mut panels = two_panels();
    let reduced = harness
        .reduce(
            &mut panels,
            &Action::GoToLocus {
                panel_id: A,
                locus: "chr1:52694-52695".to_owned(),
            },
        )
        .unwrap();
    // The two requested bases (52693..52695) sit centred in a 10-base view.
    assert_eq!(span_of(&panels, A), (52_689, 52_699, 10));
    let reported = reduced.outcome.interval.unwrap();
    assert_eq!((reported.start(), reported.end()), (52_689, 52_699));
    // History records the effective view, so Back/Forward land on it again.
    let entry = panels.get(A).unwrap().navigation_history.current();
    assert_eq!((entry.start, entry.end), (52_689, 52_699));

    // A single position opens its own window, clamped at the contig end; that is already wider
    // than the zoom limit.
    harness
        .reduce(
            &mut panels,
            &Action::GoToLocus {
                panel_id: A,
                locus: "chr2:500000".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(span_of(&panels, A), (499_979, 500_000, 21));
    harness
        .reduce(
            &mut panels,
            &Action::GoToLocus {
                panel_id: A,
                locus: "chr1:1-2".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(span_of(&panels, A), (0, 10, 10));
}

#[test]
fn automation_zoom_fit_and_window_cameras_stop_at_the_zoom_limit() {
    let harness = Harness::new();
    let mut panels = two_panels();
    for _ in 0..20 {
        harness
            .reduce(
                &mut panels,
                &Action::ChangeViewport {
                    panel_id: A,
                    operation: AutomationViewportOperation::Zoom {
                        factor: 4.0,
                        anchor_position: Some(15_000),
                    },
                    commit: ViewportCommit::Step,
                },
            )
            .unwrap();
    }
    let viewport = panels.get(A).unwrap().viewport;
    assert!((viewport.span() - 10.0).abs() < 1.0e-9);
    // The anchor stays on screen at the same relative place it started (the centre).
    assert!((viewport.genomic_at_pixel(300.0) - 15_000.0).abs() < 1.0e-6);

    harness
        .reduce(
            &mut panels,
            &Action::ChangeViewport {
                panel_id: A,
                operation: AutomationViewportOperation::FitInterval {
                    start: 1_000,
                    end: 1_001,
                },
                commit: ViewportCommit::Discrete,
            },
        )
        .unwrap();
    assert_eq!(span_of(&panels, A).2, 10);

    // A narrow camera computed by the window is widened about its centre.
    let narrow = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 7_000, 7_002).unwrap(),
        600.0,
    )
    .unwrap();
    harness
        .reduce(
            &mut panels,
            &Action::SetViewport {
                panel_id: B,
                viewport: narrow,
                commit: ViewportCommit::Gesture,
            },
        )
        .unwrap();
    assert_eq!(span_of(&panels, B), (6_996, 7_006, 10));
}

#[test]
fn added_panels_and_history_entries_respect_the_zoom_limit() {
    let harness = Harness::new();
    let mut panels = two_panels();
    let added = harness
        .reduce(
            &mut panels,
            &Action::AddPanel {
                locus: "chr2:100-100".to_owned(),
            },
        )
        .unwrap()
        .outcome
        .panel_id
        .unwrap();
    assert_eq!(span_of(&panels, added).2, 10);
    // A legacy one-base history entry is revisited at the limit.
    let entry = rgv_core::NavigationEntry {
        contig_name: "chr1".to_owned(),
        start: 900,
        end: 901,
    };
    harness
        .reduce(
            &mut panels,
            &Action::NavigateToEntry {
                panel_id: A,
                entry,
                record: true,
            },
        )
        .unwrap();
    assert_eq!(span_of(&panels, A), (896, 906, 10));
}

#[test]
fn a_saved_view_narrower_than_the_limit_loads_widened() {
    let session = rgv_core::SessionDocument::new(rgv_core::SessionLocus {
        contig: "chr3".to_owned(),
        start: 52_694_305,
        end: 52_694_307,
    });
    let workspace =
        PanelWorkspace::<ViewPanelController>::from_session(&session, 1_400.0, 0.0).unwrap();
    let viewport = workspace.active().viewport;
    assert_eq!(
        (viewport.interval().start(), viewport.interval().end()),
        (52_694_301, 52_694_311)
    );
    // The requested locus is kept as typed; the dictionary resolution widens it again.
    assert_eq!(
        workspace.active().requested_locus.as_deref(),
        Some("chr3:52694306-52694307")
    );
}
