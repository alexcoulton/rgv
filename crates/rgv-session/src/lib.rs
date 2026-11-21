//! The headless session: document state, panels and cameras, the action model and its undo
//! history, with no window, GPU device or widget toolkit in the dependency graph.
//!
//! The desktop application is a shell around this crate: it owns the window, the GPU and the
//! widget toolkit, phrases input as [`action::Action`]s, and presents what the session holds.
//! `tests/dependency_boundary.rs` fails if this crate can reach `winit`, `egui`, `wgpu` or
//! `rfd`. See `docs/architecture/0009-headless-session.md`.

pub mod action;
pub mod alignment;
pub mod document_state;
pub mod gesture;
pub mod navigation;
pub mod navigation_transaction;
pub mod notifications;
pub mod panels;
pub mod reference_dispatch;
pub mod selection;
pub mod session;
pub mod settings_writer;
pub mod sources;
pub mod status;
pub mod track_diagnostics;
pub mod track_events;
pub mod track_ids;
pub mod track_load;
pub mod workspace_lists;

pub use session::Session;
