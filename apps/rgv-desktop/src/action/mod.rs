//! The action model lives in `rgv-session` (see `rgv_session::action`). This module re-exports
//! it at its old paths and keeps `apply`, the half that drives engines, layout and the window.

pub(crate) mod apply;
#[cfg(test)]
mod tests;

pub(crate) use rgv_session::action::{
    Action, ActionOutcome, Effect, TrackRestore, ViewportCommit, history, translate,
};
