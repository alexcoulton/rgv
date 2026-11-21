use crate::Viewer;
use anyhow::Result;

impl Viewer {
    pub(crate) fn ensure_engine_retirement_capacity(&mut self, additional: usize) -> Result<()> {
        self.session.ensure_engine_retirement_capacity(additional)
    }

    pub(crate) fn retire_reference_engine(&mut self) {
        self.session.retire_reference_engine();
    }

    pub(crate) fn retire_track_engine(&mut self) {
        // Routed tracks read through this engine's readers; the next poll routes them again
        // through the engine that replaces it.
        self.track_tiles.reset();
        self.session.retire_track_engine();
    }

    pub(crate) fn poll_engine_retirement_tasks(&mut self) {
        self.session.poll_engine_retirement_tasks();
    }

    pub(crate) fn finish_engine_retirement_tasks(&mut self) {
        // Final shutdown, unlike interactive replacement, owns a blocking join.
        self.retire_reference_engine();
        self.retire_track_engine();
        self.session.join_engine_retirement_tasks();
    }
}
