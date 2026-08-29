use std::path::Path;

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
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            recent_alignments: Vec::new(),
            recent_sessions: Vec::new(),
            recent_loci: Vec::new(),
            cache_budget_mib: DEFAULT_CACHE_BUDGET_MIB,
            read_browsing_buffer_percent: DEFAULT_READ_BROWSING_BUFFER_PERCENT,
            diagnostics_opt_in: false,
            autosave_enabled: false,
            autosave_interval_seconds: DEFAULT_AUTOSAVE_INTERVAL_SECONDS,
            autosave_retention: DEFAULT_AUTOSAVE_RETENTION,
            interface_scale_percent: DEFAULT_INTERFACE_SCALE_PERCENT,
            high_contrast: false,
            reduced_motion: false,
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

    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json()?)?;
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
}
