//! Typed background-activity state for the status line.
//!
//! The toolbar spinner used to be keyed on the status text (`starts_with("Loading")`,
//! `contains("queue busy")`). Writers now declare activity explicitly, so rewording a status can
//! no longer start or stop the spinner.

/// Whether the current status line describes background work still in progress.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StatusActivity {
    #[default]
    Idle,
    Busy,
}

/// Remembers which status text was declared busy.
///
/// The status line is a single last-writer-wins string with many writers. Binding the
/// declaration to the exact text it was made for means any later, unrelated status replaces it
/// without every writer having to clear a flag.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BusyStatus(Option<String>);

impl BusyStatus {
    pub fn declare(&mut self, status: &str) {
        self.0 = Some(status.to_owned());
    }

    pub fn activity_for(&self, current_status: &str) -> StatusActivity {
        if self.0.as_deref() == Some(current_status) {
            StatusActivity::Busy
        } else {
            StatusActivity::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_applies_only_to_the_status_it_was_declared_for() {
        let mut busy = BusyStatus::default();
        assert_eq!(
            busy.activity_for("Loading genome hg38…"),
            StatusActivity::Idle
        );
        busy.declare("Loading genome hg38…");
        assert_eq!(
            busy.activity_for("Loading genome hg38…"),
            StatusActivity::Busy
        );
        // Any other writer replacing the status line ends the activity, including one whose
        // wording happens to start with "Loading".
        assert_eq!(
            busy.activity_for("Loaded genome hg38"),
            StatusActivity::Idle
        );
        assert_eq!(
            busy.activity_for("Loading something else"),
            StatusActivity::Idle
        );
        assert_eq!(busy.activity_for(""), StatusActivity::Idle);
    }
}
