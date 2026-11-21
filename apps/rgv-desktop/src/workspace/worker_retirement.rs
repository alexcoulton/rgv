use crate::Viewer;
use anyhow::Result;

const MAX_RETIRING_CONTEXT_ENGINES: usize = 8;

impl Viewer {
    pub(crate) fn ensure_engine_retirement_capacity(&mut self, additional: usize) -> Result<()> {
        self.poll_engine_retirement_tasks();
        anyhow::ensure!(
            self.engine_retirement_tasks
                .len()
                .saturating_add(additional)
                <= MAX_RETIRING_CONTEXT_ENGINES,
            "Previous sources are still closing; retry opening after they finish"
        );
        Ok(())
    }

    pub(crate) fn retire_reference_engine(&mut self) {
        if let Some(engine) = self.reference_engine.take()
            && let Some(worker) = engine.retire()
        {
            self.engine_retirement_tasks.push(worker);
        }
    }

    pub(crate) fn retire_track_engine(&mut self) {
        // Every track load ends with its engine; a later load of the same id reports afresh.
        self.shell.skipped_lines.reset();
        if let Some(engine) = self.track_engine.take()
            && let Some(worker) = engine.retire()
        {
            self.engine_retirement_tasks.push(worker);
        }
    }

    pub(crate) fn poll_engine_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.engine_retirement_tasks.len() {
            if !self.engine_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            if self.engine_retirement_tasks.remove(index).join().is_err() {
                tracing::warn!("context engine panicked during retirement");
            }
        }
    }

    pub(crate) fn finish_engine_retirement_tasks(&mut self) {
        // Final shutdown, unlike interactive replacement, owns a blocking join.
        self.retire_reference_engine();
        self.retire_track_engine();
        for worker in self.engine_retirement_tasks.drain(..) {
            if worker.join().is_err() {
                tracing::warn!("context engine panicked at shutdown");
            }
        }
    }
}
