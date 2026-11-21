//! One mutable alignment source shared by every panel lane which displays it.
//!
//! The coordinator is split by responsibility; every item keeps the path it had when this was
//! one file because each submodule is re-exported here.

mod capacity;
mod contract;
mod error;
mod facade;
mod frame;
mod lane;
mod pressure;
mod pump;
mod source;
mod source_presentation;
mod source_scene_build;
mod telemetry;

#[cfg(test)]
mod tests;

pub(crate) use capacity::*;
pub use contract::*;
pub use error::*;
pub use facade::*;
pub use frame::*;
pub(crate) use lane::*;
pub(crate) use pressure::*;
pub use pump::*;
pub use source::*;
pub use telemetry::*;
