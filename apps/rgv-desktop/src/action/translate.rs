//! How the two front ends phrase an intent as an [`Action`].
//!
//! These are the only places a keyboard step or an automation command is turned into an action,
//! so the tests can hold the window and the protocol to the same result.

use super::{Action, ViewportCommit};
use crate::workspace::navigation::split_locus_expression;
use anyhow::Result;
use rgv_automation::{
    AutomationCommand, AutomationHistoryNavigation, AutomationSelection, AutomationSetting,
    AutomationViewportOperation,
};
use rgv_core::{PanelId, TrackIntent};

/// `change_viewport` from the automation protocol: one complete, recorded navigation.
pub(crate) const fn change_viewport(
    panel_id: PanelId,
    operation: AutomationViewportOperation,
) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation,
        commit: ViewportCommit::Discrete,
    }
}

/// `navigate_history` from the automation protocol.
pub(crate) const fn navigate_history(
    panel_id: PanelId,
    step: AutomationHistoryNavigation,
) -> Action {
    Action::NavigateHistory { panel_id, step }
}

/// An arrow-key or overview pan step: a fraction of the panel width, positive to the right.
pub(crate) const fn keyboard_pan(panel_id: PanelId, fraction: f64) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation: AutomationViewportOperation::PanFraction { fraction },
        commit: ViewportCommit::Step,
    }
}

/// A `+`/`-` or toolbar zoom step about the panel centre. The window speaks in span
/// multipliers (0.8 zooms in); the protocol's zoom factor is the reciprocal magnification.
pub(crate) fn keyboard_zoom(panel_id: PanelId, span_factor: f64) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation: AutomationViewportOperation::Zoom {
            factor: span_factor.recip(),
            anchor_position: None,
        },
        commit: ViewportCommit::Step,
    }
}

/// The locus box: one locus retargets `focused`; several lay the workspace out as those loci.
pub(crate) fn locus_box(focused: PanelId, expression: &str) -> Result<Action> {
    let loci = split_locus_expression(expression)?
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(Action::GoToLoci {
        panel_id: focused,
        loci,
    })
}

/// The panel strip's deferred requests for one frame, in the order they are applied: a move,
/// then a close, else an activation.
pub(crate) fn panel_strip_requests(
    panel_to_move: Option<(PanelId, usize)>,
    panel_to_close: Option<PanelId>,
    panel_to_activate: Option<PanelId>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    if let Some((panel_id, index)) = panel_to_move {
        actions.push(Action::MovePanel { panel_id, index });
    }
    if let Some(panel_id) = panel_to_close {
        actions.push(Action::ClosePanel { panel_id });
    } else if let Some(panel_id) = panel_to_activate {
        actions.push(Action::ActivatePanel { panel_id });
    }
    actions
}

/// The arrangement settings of the automation protocol as track intents. `None` for settings
/// that are not track-arrangement edits.
pub(crate) fn setting_track_intents(setting: &AutomationSetting) -> Option<Vec<TrackIntent>> {
    Some(vec![match setting {
        AutomationSetting::PresentationSectionExpanded { channel, value } => {
            TrackIntent::SetSectionExpanded {
                channel: *channel,
                expanded: *value,
            }
        }
        AutomationSetting::PresentationNodeVisible { target, value } => {
            TrackIntent::SetNodeVisibility {
                target: *target,
                visible: *value,
            }
        }
        AutomationSetting::PresentationNodeHeight { target, value } => TrackIntent::ResizeNode {
            target: *target,
            preferred_height: *value,
        },
        AutomationSetting::PresentationGroupExpanded { target, value } => {
            TrackIntent::SetGroupExpanded {
                target: *target,
                expanded: *value,
            }
        }
        AutomationSetting::PresentationGroupVisible { target, value } => {
            TrackIntent::SetGroupVisibility {
                target: *target,
                visible: *value,
            }
        }
        AutomationSetting::PresentationCompositeVisible {
            channel,
            composite_id,
            value,
        } => TrackIntent::SetCompositeVisibility {
            channel: *channel,
            composite_id: *composite_id,
            visible: *value,
        },
        AutomationSetting::PresentationGroup { channel, track_ids } => TrackIntent::GroupNodes {
            channel: *channel,
            members: track_ids.clone(),
        },
        AutomationSetting::PresentationUngroup { target } => {
            TrackIntent::Ungroup { target: *target }
        }
        AutomationSetting::ReadPaneHeight { value } => {
            TrackIntent::ResizeReadPane { height: *value }
        }
        _ => return None,
    }])
}

/// The automation commands that are pure action translations, as the actions they apply.
/// `focused` is the panel with keyboard focus when the command arrives; commands that name a
/// panel never read it. `None` for commands that are queries, waits, file work, or not yet on
/// the action path.
///
/// `change_viewport` and `navigate_history` document that their panel becomes the focused
/// panel, so they translate to a focus change followed by the navigation.
pub(crate) fn automation_command_actions(
    command: &AutomationCommand,
    focused: PanelId,
) -> Option<Result<Vec<Action>>> {
    Some(match command {
        AutomationCommand::Goto { loci } => {
            locus_box(focused, &loci.join(" ")).map(|action| vec![action])
        }
        AutomationCommand::ChangeViewport {
            panel_id,
            operation,
        } => Ok(vec![
            Action::ActivatePanel {
                panel_id: *panel_id,
            },
            change_viewport(*panel_id, *operation),
        ]),
        AutomationCommand::NavigateHistory { panel_id, action } => Ok(vec![
            Action::ActivatePanel {
                panel_id: *panel_id,
            },
            navigate_history(*panel_id, *action),
        ]),
        AutomationCommand::AddPanel { locus } => Ok(vec![Action::AddPanel {
            locus: locus.clone(),
        }]),
        AutomationCommand::ClosePanel { panel_id } => Ok(vec![Action::ClosePanel {
            panel_id: *panel_id,
        }]),
        AutomationCommand::RemoveTrack { track_id } => Ok(vec![Action::RemoveTracks {
            track_ids: vec![*track_id],
        }]),
        AutomationCommand::Select {
            selection: AutomationSelection::Panel { panel_id },
        } => Ok(vec![Action::ActivatePanel {
            panel_id: *panel_id,
        }]),
        AutomationCommand::Configure { setting } => Ok(vec![Action::ApplyTrackIntents {
            intents: setting_track_intents(setting)?,
        }]),
        _ => return None,
    })
}
