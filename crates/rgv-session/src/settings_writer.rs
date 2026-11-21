//! Desktop settings persistence off the event loop (APP-11).
//!
//! Settings are saved after most navigations. The save reads the existing
//! file, serialises, fsyncs and renames, which must not run on the UI thread.
//! One worker owns the file; a burst of saves collapses into a write of the
//! newest snapshot.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use rgv_core::{DesktopSettings, SettingsRecovery};

/// Tells the user their preferences were reset and where the old file went.
pub fn settings_recovery_notice(path: &Path, recovery: &SettingsRecovery) -> String {
    let fate = recovery.preserved_at.as_ref().map_or_else(
        || "it could not be copied aside and will be replaced on the next save".to_owned(),
        |preserved| format!("the unreadable file was kept as {}", preserved.display()),
    );
    format!(
        "Settings file {} could not be read ({}); preferences were reset to defaults and {fate}",
        path.display(),
        recovery.error
    )
}

/// The newest snapshot waiting in `pending`, starting from `first`, and how
/// many older ones it supersedes. Never blocks.
pub fn newest_pending<T>(first: T, pending: &Receiver<T>) -> (T, usize) {
    let mut newest = first;
    let mut superseded = 0;
    loop {
        match pending.try_recv() {
            Ok(next) => {
                newest = next;
                superseded += 1;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return (newest, superseded),
        }
    }
}

pub struct SettingsWriter {
    path: PathBuf,
    snapshots: Option<Sender<DesktopSettings>>,
    failures: Receiver<String>,
    worker: Option<JoinHandle<()>>,
    /// The last snapshot handed over, so an unchanged save costs nothing.
    last_submitted: Option<DesktopSettings>,
}

impl SettingsWriter {
    pub fn new(path: PathBuf) -> Self {
        Self::with_write(path, |settings, path| {
            settings.write(path).map_err(|error| error.to_string())
        })
    }

    fn with_write(
        path: PathBuf,
        write: impl Fn(&DesktopSettings, &Path) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let (snapshots, pending) = channel::<DesktopSettings>();
        let (report, failures) = channel();
        let worker_path = path.clone();
        let worker = std::thread::Builder::new()
            .name("rgv-settings-write".to_owned())
            .spawn(move || {
                while let Ok(first) = pending.recv() {
                    let (newest, _) = newest_pending(first, &pending);
                    if let Err(error) = write(&newest, &worker_path) {
                        tracing::warn!(
                            path = %worker_path.display(),
                            %error,
                            "could not persist desktop settings"
                        );
                        let _ = report.send(error);
                    }
                }
            })
            .map_err(|error| {
                tracing::warn!(%error, "could not start the settings writer; saving inline");
            })
            .ok();
        Self {
            path,
            snapshots: worker.is_some().then_some(snapshots),
            failures,
            worker,
            last_submitted: None,
        }
    }

    /// Queues `settings` to be written. Returns immediately; an identical
    /// consecutive snapshot is skipped.
    pub fn submit(&mut self, settings: &DesktopSettings) {
        if self.last_submitted.as_ref() == Some(settings) {
            return;
        }
        self.last_submitted = Some(settings.clone());
        match &self.snapshots {
            Some(snapshots) if snapshots.send(settings.clone()).is_ok() => {}
            // No worker (thread creation failed, or it died): keep the old
            // synchronous behaviour rather than lose the save.
            _ => {
                if let Err(error) = settings.write(&self.path) {
                    tracing::warn!(
                        path = %self.path.display(),
                        %error,
                        "could not persist desktop settings"
                    );
                }
            }
        }
    }

    /// The most recent write failure since the last call, if any.
    pub fn take_failure(&mut self) -> Option<String> {
        self.failures.try_iter().last()
    }

    /// Writes everything queued and stops the worker. Called at exit so the
    /// last save is on disk before the process ends.
    pub fn finish(&mut self) {
        self.snapshots = None;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("settings writer panicked during shutdown");
        }
    }
}

impl Drop for SettingsWriter {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_burst_of_snapshots_collapses_to_the_newest() {
        let (sender, pending) = channel();
        for value in 1..=5 {
            sender.send(value).unwrap();
        }
        assert_eq!(newest_pending(0, &pending), (5, 5));
        assert_eq!(newest_pending(9, &pending), (9, 0));
        drop(sender);
        assert_eq!(newest_pending(7, &pending), (7, 0));
    }

    #[test]
    fn rapid_saves_are_coalesced_and_the_last_one_reaches_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let gate = Arc::new(Mutex::new(()));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut writer = SettingsWriter::with_write(path.clone(), {
            let (gate, recorded) = (Arc::clone(&gate), Arc::clone(&recorded));
            move |settings, path| {
                // Hold the first write until the whole burst is queued.
                drop(gate.lock().unwrap());
                recorded.lock().unwrap().push(settings.recent_loci.clone());
                settings.write(path).map_err(|error| error.to_string())
            }
        });

        let held = gate.lock().unwrap();
        let mut settings = DesktopSettings::default();
        for index in 0..40 {
            settings.remember_locus(format!("chr1:{}-{}", index + 1, index + 100));
            writer.submit(&settings);
        }
        // Unchanged settings are not queued again.
        writer.submit(&settings);
        drop(held);
        writer.finish();

        let completed = recorded.lock().unwrap();
        assert!(
            completed.len() <= 2,
            "40 queued saves ran {} writes",
            completed.len()
        );
        assert_eq!(completed.last().unwrap(), &settings.recent_loci);
        assert_eq!(DesktopSettings::read(&path).unwrap(), settings);
        assert_eq!(writer.take_failure(), None);
    }

    #[test]
    fn a_failed_write_is_reported_to_the_event_loop() {
        let mut writer =
            SettingsWriter::with_write(PathBuf::from("unused"), |_, _| Err("disk full".to_owned()));
        writer.submit(&DesktopSettings::default());
        writer.finish();
        assert_eq!(writer.take_failure().as_deref(), Some("disk full"));
        assert_eq!(writer.take_failure(), None);
    }

    #[test]
    fn a_reset_settings_file_is_explained_with_the_preserved_copy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(&path, "{ truncated").unwrap();
        let (settings, recovery) = DesktopSettings::read_or_recover(&path);
        assert_eq!(settings, DesktopSettings::default());
        let notice = settings_recovery_notice(&path, &recovery.unwrap());
        assert!(notice.contains("settings.json.corrupt"), "{notice}");
        assert!(notice.contains("reset to defaults"), "{notice}");

        let lost = SettingsRecovery {
            error: "bad".to_owned(),
            preserved_at: None,
        };
        assert!(settings_recovery_notice(&path, &lost).contains("could not be copied aside"));
    }
}
