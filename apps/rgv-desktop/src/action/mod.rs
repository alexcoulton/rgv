//! The action model lives in `rgv-session` (see `rgv_session::action`). This module re-exports
//! it at its old paths and keeps `apply`, the half that drives engines, layout and the window.

mod alignment_settings;
pub(crate) mod apply;
mod preferences;
mod track_load;
mod track_settings;
#[cfg(test)]
mod tests;

pub(crate) use rgv_session::action::{
    Action, ActionOutcome, AlignmentTrackLabel, Effect, Preferences, ReferencePresentation,
    SelectionExtent,
    TrackLoad, TrackLoadKind, TrackRestore, ViewportCommit, history, translate,
};
