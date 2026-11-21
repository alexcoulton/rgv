//! The notification log (APP-7): failures and warnings that outlive the status line.

use rgv_core::TrackId;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NotificationSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub sequence: u64,
    pub severity: NotificationSeverity,
    pub track: Option<TrackId>,
    pub message: String,
    /// How many consecutive times this exact notification was reported.
    pub occurrences: u32,
}

/// Bounded log of failures. Unlike the single status line, a later progress message cannot
/// overwrite an entry: errors stay until the user dismisses them.
#[derive(Debug, Default)]
pub struct NotificationLog {
    entries: VecDeque<Notification>,
    next_sequence: u64,
    /// Sequence below which the user has already seen every entry.
    seen_before: u64,
    /// Last status text classified by [`Self::observe_status`], so a status that persists across
    /// frames is logged once.
    last_observed_status: String,
}

impl NotificationLog {
    pub const CAPACITY: usize = 200;

    pub fn push(
        &mut self,
        severity: NotificationSeverity,
        track: Option<TrackId>,
        message: impl Into<String>,
    ) {
        let message = message.into();
        if message.trim().is_empty() {
            return;
        }
        if let Some(last) = self.entries.back_mut()
            && last.severity == severity
            && last.track == track
            && last.message == message
        {
            last.occurrences = last.occurrences.saturating_add(1);
            last.sequence = self.next_sequence;
            self.next_sequence += 1;
            return;
        }
        self.entries.push_back(Notification {
            sequence: self.next_sequence,
            severity,
            track,
            message,
            occurrences: 1,
        });
        self.next_sequence += 1;
        while self.entries.len() > Self::CAPACITY {
            // Prefer dropping the oldest non-error so sticky errors survive a burst of warnings.
            let victim = self
                .entries
                .iter()
                .position(|entry| entry.severity != NotificationSeverity::Error)
                .unwrap_or(0);
            self.entries.remove(victim);
        }
    }

    /// Logs a failure that the caller also shows on the status line, so that the same text is
    /// not logged a second time when the status line is next observed.
    pub fn push_reported(
        &mut self,
        severity: NotificationSeverity,
        track: Option<TrackId>,
        message: &str,
    ) {
        self.push(severity, track, message);
        message.clone_into(&mut self.last_observed_status);
    }

    /// Routes a status-line value into the log when it reports a failure. Returns whether it was
    /// logged. The status line keeps showing transient progress; this keeps the failures.
    pub fn observe_status(&mut self, status: &str) -> bool {
        if status == self.last_observed_status {
            return false;
        }
        status.clone_into(&mut self.last_observed_status);
        match classify_status(status) {
            Some(severity) => {
                self.push(severity, None, status);
                true
            }
            None => false,
        }
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Notification> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.severity == NotificationSeverity::Error)
            .count()
    }

    pub fn unseen_error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.severity == NotificationSeverity::Error && entry.sequence >= self.seen_before
            })
            .count()
    }

    pub fn mark_seen(&mut self) {
        self.seen_before = self.next_sequence;
    }

    pub fn dismiss(&mut self, sequence: u64) {
        self.entries.retain(|entry| entry.sequence != sequence);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.mark_seen();
    }
}

/// Classifies free-text status lines written by the many legacy `data_status` call sites.
/// Failures that are constructed as typed errors should call [`NotificationLog::push`] directly.
pub fn classify_status(status: &str) -> Option<NotificationSeverity> {
    const ERROR_MARKERS: &[&str] = &[
        "could not",
        "cannot ",
        "can't ",
        "failed",
        "failure",
        "error",
        "invalid",
        "unsupported",
        "not supported",
        "not found",
        "no such file",
        "permission denied",
        "timed out",
        "is unavailable",
        "is not available",
        "unexpectedly",
        "panicked",
        "malformed",
        "mismatch",
        "denied",
        "rejected",
        "missing",
        "too narrow",
        "exceeds",
        "must be",
        "does not",
        "is not a",
        "unknown",
    ];
    const WARNING_MARKERS: &[&str] = &["warning", "unresolved", "skipped", "truncated", "retry"];
    let trimmed = status.trim();
    if trimmed.is_empty() || trimmed.ends_with('…') {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if ERROR_MARKERS.iter().any(|marker| lower.contains(marker)) {
        Some(NotificationSeverity::Error)
    } else if WARNING_MARKERS.iter().any(|marker| lower.contains(marker)) {
        Some(NotificationSeverity::Warning)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_survive_later_progress_messages() {
        let mut log = NotificationLog::default();
        assert!(log.observe_status("Could not open /data/a.bam: permission denied"));
        assert!(!log.observe_status("Opening /data/b.bam…"));
        assert!(!log.observe_status("Loaded 12 reads"));
        assert_eq!(log.error_count(), 1);
        assert_eq!(log.unseen_error_count(), 1);
        log.mark_seen();
        assert_eq!(log.unseen_error_count(), 0);
        assert_eq!(log.error_count(), 1, "seen errors stay until dismissed");
        let sequence = log.entries().next().unwrap().sequence;
        log.dismiss(sequence);
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn a_status_that_persists_across_frames_is_logged_once_and_repeats_are_counted() {
        let mut log = NotificationLog::default();
        assert!(log.observe_status("Track query failed: truncated block"));
        assert!(!log.observe_status("Track query failed: truncated block"));
        assert_eq!(log.len(), 1);
        log.observe_status("Loading…");
        assert!(log.observe_status("Track query failed: truncated block"));
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries().next().unwrap().occurrences, 2);
        log.push(
            NotificationSeverity::Error,
            Some(TrackId::new(7)),
            "index is missing",
        );
        assert_eq!(log.entries().last().unwrap().track, Some(TrackId::new(7)));
        assert_eq!(log.unseen_error_count(), 2);
    }

    #[test]
    fn an_explicitly_reported_failure_is_not_logged_again_from_the_status_line() {
        let mut log = NotificationLog::default();
        log.push_reported(NotificationSeverity::Error, None, "Could not save project");
        assert!(!log.observe_status("Could not save project"));
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries().next().unwrap().occurrences, 1);
    }

    #[test]
    fn status_classification_separates_failures_from_progress() {
        for failure in [
            "Could not save project /x.json: disk full",
            "Project-save worker failed while writing /x.json",
            "remote session cytoband files are not yet supported: unsupported",
            "the window is too narrow to display 9 readable panels",
            "invalid locus chr1:b-a",
        ] {
            assert_eq!(
                classify_status(failure),
                Some(NotificationSeverity::Error),
                "{failure}"
            );
        }
        for progress in [
            "",
            "Opening /data/a.bam…",
            "Saved project to /x.json",
            "Recovery autosave updated at /x.json",
            "Could not open yet; still loading…",
            "TP53 · genes · gene",
        ] {
            assert_eq!(classify_status(progress), None, "{progress}");
        }
        assert_eq!(
            classify_status("Opened project with 2 unresolved sources"),
            Some(NotificationSeverity::Warning)
        );
    }

    #[test]
    fn the_log_is_bounded_and_keeps_errors_in_preference_to_warnings() {
        let mut log = NotificationLog::default();
        log.push(NotificationSeverity::Error, None, "first failure");
        for index in 0..NotificationLog::CAPACITY * 2 {
            log.push(
                NotificationSeverity::Warning,
                None,
                format!("warning {index}"),
            );
        }
        assert_eq!(log.len(), NotificationLog::CAPACITY);
        assert_eq!(log.entries().next().unwrap().message, "first failure");
        log.clear();
        assert_eq!(log.len(), 0);
        log.push(NotificationSeverity::Error, None, "   ");
        assert_eq!(log.len(), 0, "blank messages are not logged");
    }
}
