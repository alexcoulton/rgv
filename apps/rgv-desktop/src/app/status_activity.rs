pub(crate) use rgv_session::status::{BusyStatus, StatusActivity};

use crate::Viewer;

impl Viewer {
    /// Sets the status line and declares that it describes background work in progress.
    pub(crate) fn set_busy_status(&mut self, status: impl Into<String>) {
        self.session.set_busy_status(status);
    }

    /// Activity of the status line as it currently reads.
    pub(crate) fn status_activity(&self) -> StatusActivity {
        self.session.status_activity()
    }
}
