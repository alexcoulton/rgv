use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::TrackSource;

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_CACHE_BUDGET_MIB: usize = 512;
pub const MINIMUM_CACHE_BUDGET_MIB: usize = 128;
pub const MAXIMUM_CACHE_BUDGET_MIB: usize = 4_096;
pub const DEFAULT_READ_BROWSING_BUFFER_PERCENT: u16 = 100;
pub const MINIMUM_READ_BROWSING_BUFFER_PERCENT: u16 = 0;
pub const MAXIMUM_READ_BROWSING_BUFFER_PERCENT: u16 = 400;
pub const DEFAULT_AUTOSAVE_INTERVAL_SECONDS: u64 = 120;
pub const MINIMUM_AUTOSAVE_INTERVAL_SECONDS: u64 = 30;
pub const MAXIMUM_AUTOSAVE_INTERVAL_SECONDS: u64 = 3_600;
pub const DEFAULT_AUTOSAVE_RETENTION: usize = 5;
pub const MINIMUM_AUTOSAVE_RETENTION: usize = 1;
pub const MAXIMUM_AUTOSAVE_RETENTION: usize = 20;
pub const DEFAULT_INTERFACE_SCALE_PERCENT: u16 = 100;
pub const MINIMUM_INTERFACE_SCALE_PERCENT: u16 = 75;
pub const MAXIMUM_INTERFACE_SCALE_PERCENT: u16 = 200;
const RECENT_ITEM_LIMIT: usize = 12;

/// How alignment caches treat the user's cache budget.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentMemoryPolicy {
    /// Charge what a build actually retained, keep each cache class inside its own share of the
    /// budget by evicting least-recently-used unpinned entries, and report pressure only when
    /// visible content alone cannot fit.
    #[default]
    Bounded,
    /// Reserve a worst-case bound against one shared budget before every build and refuse the
    /// build rather than exceed it.
    Complete,
}

/// Colour theme of the interface and the genomic canvas.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceTheme {
    #[default]
    Light,
    Dark,
    /// Light, with categories recoloured to stay distinguishable under protanopia and
    /// deuteranopia.
    LightColorBlind,
    /// Dark, with the same colour-blind-safe categories.
    DarkColorBlind,
}

/// Which pipeline draws auxiliary (non-alignment) tracks.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TrackPipeline {
    /// Every track is queried and drawn by the established per-viewport path.
    #[default]
    Legacy,
    /// Experimental: the track kinds the desktop has opted in are read as cached world-space
    /// tiles and drawn by the paged tile renderer. Every other track stays on `Legacy`.
    Tiles,
}

/// User-controlled desktop preferences. Diagnostics are disabled unless the
/// user explicitly opts in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct DesktopSettings {
    pub schema_version: u32,
    pub recent_alignments: Vec<TrackSource>,
    pub recent_sessions: Vec<String>,
    pub recent_loci: Vec<String>,
    pub cache_budget_mib: usize,
    /// Whether alignment caches are bounded by eviction or by strict reserve-before-build.
    pub alignment_memory_policy: AlignmentMemoryPolicy,
    /// Desired decoded-read margin on each side, as a percentage of the viewport span.
    pub read_browsing_buffer_percent: u16,
    pub diagnostics_opt_in: bool,
    /// Periodic recovery snapshots are disabled until the user opts in.
    pub autosave_enabled: bool,
    pub autosave_interval_seconds: u64,
    pub autosave_retention: usize,
    /// UI scaling is stored as an integer percentage for stable validation and persistence.
    pub interface_scale_percent: u16,
    pub high_contrast: bool,
    pub reduced_motion: bool,
    /// Absent in files written before themes existed, which therefore load as Light.
    pub theme: InterfaceTheme,
    /// Whether alignment coverage tiles are persisted in the per-user coverage cache directory.
    /// Off keeps coverage in memory only. Absent in older files, which load as enabled.
    pub coverage_cache_enabled: bool,
    /// Which pipeline draws auxiliary tracks. Absent in older files, which load as `Legacy`.
    pub track_pipeline: TrackPipeline,
    /// Folder holding the built-in gene landmark catalogue (`catalog.json`, or an `igv`
    /// subfolder with one), chosen with **Genomes → Choose landmark folder**. Consulted after
    /// `--landmark-resource-root` and `RGV_LANDMARK_RESOURCE_ROOT`. Absent in older files.
    pub landmark_resource_root: Option<PathBuf>,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            recent_alignments: Vec::new(),
            recent_sessions: Vec::new(),
            recent_loci: Vec::new(),
            cache_budget_mib: DEFAULT_CACHE_BUDGET_MIB,
            alignment_memory_policy: AlignmentMemoryPolicy::default(),
            read_browsing_buffer_percent: DEFAULT_READ_BROWSING_BUFFER_PERCENT,
            diagnostics_opt_in: false,
            autosave_enabled: false,
            autosave_interval_seconds: DEFAULT_AUTOSAVE_INTERVAL_SECONDS,
            autosave_retention: DEFAULT_AUTOSAVE_RETENTION,
            interface_scale_percent: DEFAULT_INTERFACE_SCALE_PERCENT,
            high_contrast: false,
            reduced_motion: false,
            theme: InterfaceTheme::default(),
            coverage_cache_enabled: true,
            track_pipeline: TrackPipeline::Legacy,
            landmark_resource_root: None,
        }
    }
}

impl DesktopSettings {
    pub fn from_json(json: &str) -> Result<Self, SettingsError> {
        let settings: Self = serde_json::from_str(json)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn to_json(&self) -> Result<String, SettingsError> {
        self.validate()?;
        let mut persisted = self.clone();
        persisted.recent_alignments = persisted
            .recent_alignments
            .iter()
            .map(TrackSource::redacted_for_persistence)
            .collect();
        Ok(format!("{}\n", serde_json::to_string_pretty(&persisted)?))
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Reads settings, falling back to defaults without losing a damaged file.
    ///
    /// A missing file yields defaults. A file that cannot be read as valid
    /// settings (truncated by a crash, hand-edited, or from a newer RGV) is
    /// copied beside itself as `<name>.corrupt` and reported, so the caller
    /// can tell the user their preferences were reset and where the old file
    /// went instead of silently overwriting it on the next save.
    pub fn read_or_recover(path: impl AsRef<Path>) -> (Self, Option<SettingsRecovery>) {
        let path = path.as_ref();
        match Self::read(path) {
            Ok(settings) => (settings, None),
            Err(SettingsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), None)
            }
            Err(error) => {
                let preserved_at = preserve_corrupt_settings(path);
                (
                    Self::default(),
                    Some(SettingsRecovery {
                        error: error.to_string(),
                        preserved_at,
                    }),
                )
            }
        }
    }

    /// Atomically replaces the settings file: the complete JSON is flushed to
    /// a temporary file in the same directory and renamed over the target, so
    /// a crash or a full disk mid-save cannot leave a truncated file. An
    /// existing file that is not valid settings is first preserved as
    /// `<name>.corrupt` rather than being silently replaced.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        let path = path.as_ref();
        let json = self.to_json()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        if matches!(Self::read(path), Err(error) if !error.is_not_found()) {
            preserve_corrupt_settings(path);
        }
        crate::session::atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    pub fn remember_alignment(&mut self, source: &TrackSource) {
        let source = source.redacted_for_persistence();
        self.recent_alignments.retain(|item| item != &source);
        self.recent_alignments.insert(0, source);
        self.recent_alignments.truncate(RECENT_ITEM_LIMIT);
    }

    pub fn remember_session(&mut self, path: impl AsRef<str>) {
        remember_string(&mut self.recent_sessions, path.as_ref());
    }

    pub fn remember_locus(&mut self, locus: impl AsRef<str>) {
        remember_string(&mut self.recent_loci, locus.as_ref());
    }

    pub fn clear_recent(&mut self) {
        self.recent_alignments.clear();
        self.recent_sessions.clear();
        self.recent_loci.clear();
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedVersion(self.schema_version));
        }
        if !(MINIMUM_CACHE_BUDGET_MIB..=MAXIMUM_CACHE_BUDGET_MIB).contains(&self.cache_budget_mib) {
            return Err(SettingsError::InvalidCacheBudget(self.cache_budget_mib));
        }
        if !(MINIMUM_READ_BROWSING_BUFFER_PERCENT..=MAXIMUM_READ_BROWSING_BUFFER_PERCENT)
            .contains(&self.read_browsing_buffer_percent)
        {
            return Err(SettingsError::InvalidReadBrowsingBuffer(
                self.read_browsing_buffer_percent,
            ));
        }
        if !(MINIMUM_AUTOSAVE_INTERVAL_SECONDS..=MAXIMUM_AUTOSAVE_INTERVAL_SECONDS)
            .contains(&self.autosave_interval_seconds)
        {
            return Err(SettingsError::InvalidAutosaveInterval(
                self.autosave_interval_seconds,
            ));
        }
        if !(MINIMUM_AUTOSAVE_RETENTION..=MAXIMUM_AUTOSAVE_RETENTION)
            .contains(&self.autosave_retention)
        {
            return Err(SettingsError::InvalidAutosaveRetention(
                self.autosave_retention,
            ));
        }
        if !(MINIMUM_INTERFACE_SCALE_PERCENT..=MAXIMUM_INTERFACE_SCALE_PERCENT)
            .contains(&self.interface_scale_percent)
        {
            return Err(SettingsError::InvalidInterfaceScale(
                self.interface_scale_percent,
            ));
        }
        Ok(())
    }
}

/// Why settings fell back to defaults, and where the unreadable file went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsRecovery {
    /// Why the file was rejected.
    pub error: String,
    /// The preserved copy, or `None` if it could not be written.
    pub preserved_at: Option<PathBuf>,
}

/// Best-effort copy of an unreadable settings file to `<name>.corrupt`.
fn preserve_corrupt_settings(path: &Path) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_owned();
    name.push(".corrupt");
    let preserved = path.with_file_name(name);
    std::fs::copy(path, &preserved).ok().map(|_| preserved)
}

impl SettingsError {
    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
    }
}

fn remember_string(items: &mut Vec<String>, item: &str) {
    let item = item.trim().to_owned();
    if item.is_empty() {
        return;
    }
    items.retain(|candidate| candidate != &item);
    items.insert(0, item);
    items.truncate(RECENT_ITEM_LIMIT);
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("settings schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("cache budget {0} MiB is outside the supported range")]
    InvalidCacheBudget(usize),
    #[error("read browsing buffer {0}% is outside the supported range")]
    InvalidReadBrowsingBuffer(u16),
    #[error("autosave interval {0} seconds is outside the supported range")]
    InvalidAutosaveInterval(u64),
    #[error("autosave retention {0} is outside the supported range")]
    InvalidAutosaveRetention(usize),
    #[error("interface scale {0}% is outside the supported range")]
    InvalidInterfaceScale(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_writes_are_atomic_and_leave_no_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("settings.json");
        let mut settings = DesktopSettings::default();
        settings.write(&path).unwrap();
        settings.cache_budget_mib = 1_024;
        settings.remember_locus("chr1:1-100");
        settings.write(&path).unwrap();
        assert_eq!(DesktopSettings::read(&path).unwrap(), settings);
        let names = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [std::ffi::OsString::from("settings.json")]);

        // An invalid document never reaches the disk or disturbs the last good file.
        let mut invalid = settings.clone();
        invalid.cache_budget_mib = 0;
        assert!(invalid.write(&path).is_err());
        assert_eq!(DesktopSettings::read(&path).unwrap(), settings);

        // A failed replacement (the target is a directory) leaves no debris.
        let blocked = directory.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        assert!(settings.write(&blocked).is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn corrupt_settings_are_preserved_instead_of_silently_reset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let corrupt = directory.path().join("settings.json.corrupt");

        assert_eq!(
            DesktopSettings::read_or_recover(&path),
            (DesktopSettings::default(), None)
        );
        assert!(!corrupt.exists());

        // A truncated file, as left by an interrupted non-atomic write.
        let truncated = "{\n  \"schema_version\": 1,\n  \"recent_loci\": [\"chr7:1";
        std::fs::write(&path, truncated).unwrap();
        let (settings, recovery) = DesktopSettings::read_or_recover(&path);
        assert_eq!(settings, DesktopSettings::default());
        let recovery = recovery.unwrap();
        assert_eq!(recovery.preserved_at.as_deref(), Some(corrupt.as_path()));
        assert!(!recovery.error.is_empty());
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), truncated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), truncated);

        // A caller that only falls back to defaults and later saves still
        // keeps the damaged file.
        std::fs::remove_file(&corrupt).unwrap();
        DesktopSettings::default().write(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), truncated);
        assert_eq!(
            DesktopSettings::read_or_recover(&path),
            (DesktopSettings::default(), None)
        );

        // Replacing a healthy file does not manufacture a `.corrupt` copy.
        std::fs::remove_file(&corrupt).unwrap();
        DesktopSettings::default().write(&path).unwrap();
        assert!(!corrupt.exists());
    }

    #[test]
    fn recent_items_are_deduplicated_bounded_and_newest_first() {
        let mut settings = DesktopSettings::default();
        for index in 0..20 {
            settings.remember_locus(format!("chr1:{index}-{}", index + 1));
        }
        settings.remember_locus("chr1:19-20");
        assert_eq!(settings.recent_loci.len(), RECENT_ITEM_LIMIT);
        assert_eq!(settings.recent_loci[0], "chr1:19-20");
    }

    #[test]
    fn persisted_recent_urls_are_sanitized_and_diagnostics_default_off() {
        let mut settings = DesktopSettings::default();
        settings.remember_alignment(&TrackSource::Remote {
            url: "https://user:password@example.org/reads.bam?token=secret".to_owned(),
        });
        let json = settings.to_json().unwrap();
        assert!(!json.contains("password"));
        assert!(!json.contains("token"));
        assert!(
            !DesktopSettings::from_json(&json)
                .unwrap()
                .diagnostics_opt_in
        );
    }

    #[test]
    fn invalid_cache_budget_is_rejected() {
        let mut settings = DesktopSettings {
            cache_budget_mib: 1,
            ..DesktopSettings::default()
        };
        assert!(matches!(
            settings.to_json(),
            Err(SettingsError::InvalidCacheBudget(1))
        ));
        settings.cache_budget_mib = DEFAULT_CACHE_BUDGET_MIB;
        assert!(settings.to_json().is_ok());
    }

    #[test]
    fn autosave_is_opt_in_bounded_and_backward_compatible() {
        let defaults = DesktopSettings::from_json(
            r#"{"schema_version":1,"recent_alignments":[],"recent_sessions":[],"recent_loci":[],"cache_budget_mib":512,"diagnostics_opt_in":false}"#,
        )
        .unwrap();
        assert!(!defaults.autosave_enabled);
        assert_eq!(
            defaults.autosave_interval_seconds,
            DEFAULT_AUTOSAVE_INTERVAL_SECONDS
        );
        assert_eq!(defaults.autosave_retention, DEFAULT_AUTOSAVE_RETENTION);
        assert_eq!(
            defaults.interface_scale_percent,
            DEFAULT_INTERFACE_SCALE_PERCENT
        );
        assert_eq!(
            defaults.read_browsing_buffer_percent,
            DEFAULT_READ_BROWSING_BUFFER_PERCENT
        );
        assert!(!defaults.high_contrast);
        assert!(!defaults.reduced_motion);

        let mut invalid = DesktopSettings {
            autosave_interval_seconds: 1,
            ..DesktopSettings::default()
        };
        assert!(matches!(
            invalid.to_json(),
            Err(SettingsError::InvalidAutosaveInterval(1))
        ));
        invalid.autosave_interval_seconds = DEFAULT_AUTOSAVE_INTERVAL_SECONDS;
        invalid.autosave_retention = MAXIMUM_AUTOSAVE_RETENTION + 1;
        assert!(matches!(
            invalid.to_json(),
            Err(SettingsError::InvalidAutosaveRetention(_))
        ));
    }

    #[test]
    fn coverage_cache_is_enabled_for_older_files_and_round_trips() {
        assert!(DesktopSettings::default().coverage_cache_enabled);
        let mut older: serde_json::Value =
            serde_json::from_str(&DesktopSettings::default().to_json().unwrap()).unwrap();
        older
            .as_object_mut()
            .unwrap()
            .remove("coverage_cache_enabled")
            .unwrap();
        let loaded = DesktopSettings::from_json(&older.to_string()).unwrap();
        assert!(loaded.coverage_cache_enabled);
        let disabled = DesktopSettings {
            coverage_cache_enabled: false,
            ..DesktopSettings::default()
        };
        let reloaded = DesktopSettings::from_json(&disabled.to_json().unwrap()).unwrap();
        assert!(!reloaded.coverage_cache_enabled);
    }

    #[test]
    fn track_pipeline_defaults_to_legacy_for_older_files_and_round_trips() {
        assert_eq!(
            DesktopSettings::default().track_pipeline,
            TrackPipeline::Legacy
        );
        let mut older: serde_json::Value =
            serde_json::from_str(&DesktopSettings::default().to_json().unwrap()).unwrap();
        older
            .as_object_mut()
            .unwrap()
            .remove("track_pipeline")
            .unwrap();
        let loaded = DesktopSettings::from_json(&older.to_string()).unwrap();
        assert_eq!(loaded.track_pipeline, TrackPipeline::Legacy);
        let tiles = DesktopSettings {
            track_pipeline: TrackPipeline::Tiles,
            ..DesktopSettings::default()
        };
        let json = tiles.to_json().unwrap();
        assert!(
            json.contains("\"track_pipeline\": \"tiles\"")
                || json.contains("\"track_pipeline\":\"tiles\"")
        );
        let reloaded = DesktopSettings::from_json(&json).unwrap();
        assert_eq!(reloaded.track_pipeline, TrackPipeline::Tiles);
    }

    #[test]
    fn theme_defaults_to_light_for_older_files_and_round_trips() {
        let mut older: serde_json::Value =
            serde_json::from_str(&DesktopSettings::default().to_json().unwrap()).unwrap();
        older.as_object_mut().unwrap().remove("theme").unwrap();
        let loaded = DesktopSettings::from_json(&older.to_string()).unwrap();
        assert_eq!(loaded.theme, InterfaceTheme::Light);
        for theme in [
            InterfaceTheme::Light,
            InterfaceTheme::Dark,
            InterfaceTheme::LightColorBlind,
            InterfaceTheme::DarkColorBlind,
        ] {
            let settings = DesktopSettings {
                theme,
                ..DesktopSettings::default()
            };
            let json = settings.to_json().unwrap();
            assert_eq!(DesktopSettings::from_json(&json).unwrap(), settings);
        }
        assert!(
            DesktopSettings {
                theme: InterfaceTheme::DarkColorBlind,
                ..DesktopSettings::default()
            }
            .to_json()
            .unwrap()
            .contains("\"theme\": \"dark_color_blind\"")
        );
    }

    #[test]
    fn accessibility_preferences_are_bounded_and_round_trip() {
        let settings = DesktopSettings {
            interface_scale_percent: 150,
            high_contrast: true,
            reduced_motion: true,
            ..DesktopSettings::default()
        };
        assert_eq!(
            DesktopSettings::from_json(&settings.to_json().unwrap()).unwrap(),
            settings
        );

        let invalid = DesktopSettings {
            interface_scale_percent: MAXIMUM_INTERFACE_SCALE_PERCENT + 1,
            ..DesktopSettings::default()
        };
        assert!(matches!(
            invalid.to_json(),
            Err(SettingsError::InvalidInterfaceScale(_))
        ));
    }

    #[test]
    fn read_browsing_buffer_is_bounded_and_backward_compatible() {
        let settings = DesktopSettings {
            read_browsing_buffer_percent: 250,
            ..DesktopSettings::default()
        };
        assert_eq!(
            DesktopSettings::from_json(&settings.to_json().unwrap())
                .unwrap()
                .read_browsing_buffer_percent,
            250
        );

        let invalid = DesktopSettings {
            read_browsing_buffer_percent: MAXIMUM_READ_BROWSING_BUFFER_PERCENT + 1,
            ..DesktopSettings::default()
        };
        assert!(matches!(
            invalid.to_json(),
            Err(SettingsError::InvalidReadBrowsingBuffer(_))
        ));
    }

    #[test]
    fn landmark_folder_setting_round_trips_and_is_absent_in_older_files() {
        let older = DesktopSettings::from_json(r#"{"schema_version":1}"#).unwrap();
        assert_eq!(older.landmark_resource_root, None);

        let settings = DesktopSettings {
            landmark_resource_root: Some(PathBuf::from("/data/landmarks")),
            ..DesktopSettings::default()
        };
        assert_eq!(
            DesktopSettings::from_json(&settings.to_json().unwrap())
                .unwrap()
                .landmark_resource_root,
            Some(PathBuf::from("/data/landmarks"))
        );
    }
}
