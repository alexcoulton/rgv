use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, PanelId, TrackId, TrackKind,
    TrackSource,
};

pub const AUTOMATION_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_AUTOMATION_COMMANDS: usize = 10_000;
pub const MAXIMUM_AUTOMATION_STRING_BYTES: usize = 16_384;
pub const MAXIMUM_AUTOMATION_OPTION_BYTES: usize = 64 * 1024;
pub const MAXIMUM_AUTOMATION_LOCI: usize = 16;
pub const MAXIMUM_AUTOMATION_TRACK_SELECTION: usize = 1_024;
pub const MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS: u64 = 100;
pub const MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS: u64 = 600_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationBatch {
    pub schema_version: u32,
    pub commands: Vec<AutomationCommand>,
}

impl AutomationBatch {
    pub fn from_json(json: &str) -> Result<Self, AutomationError> {
        let batch: Self = serde_json::from_str(json)?;
        batch.validate()?;
        Ok(batch)
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, AutomationError> {
        let path = path.as_ref();
        let mut batch = Self::from_json(&std::fs::read_to_string(path)?)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        batch.resolve_paths(std::path::absolute(parent)?);
        Ok(batch)
    }

    pub fn to_json(&self) -> Result<String, AutomationError> {
        self.validate()?;
        let mut redacted = self.clone();
        for command in &mut redacted.commands {
            command.redact_sources();
        }
        Ok(format!("{}\n", serde_json::to_string_pretty(&redacted)?))
    }

    /// Resolves local inputs and outputs against the directory containing a
    /// batch file. A local command service can call the same method with its
    /// explicitly selected working root.
    pub fn resolve_paths(&mut self, root: impl AsRef<Path>) {
        let root = root.as_ref();
        for command in &mut self.commands {
            command.resolve_paths(root);
        }
    }

    /// Resolves local paths against an explicitly selected control root and
    /// rejects lexical attempts to access paths outside it.
    pub fn resolve_paths_within(&mut self, root: impl AsRef<Path>) -> Result<(), AutomationError> {
        let root = normalize_automation_path(&std::path::absolute(root)?);
        for command in &mut self.commands {
            command.resolve_paths_within(&root)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), AutomationError> {
        if self.schema_version != AUTOMATION_SCHEMA_VERSION {
            return Err(AutomationError::UnsupportedVersion(self.schema_version));
        }
        if self.commands.is_empty() || self.commands.len() > MAXIMUM_AUTOMATION_COMMANDS {
            return Err(AutomationError::InvalidCommandCount(self.commands.len()));
        }
        for (index, command) in self.commands.iter().enumerate() {
            command
                .validate()
                .map_err(|reason| AutomationError::InvalidCommand { index, reason })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AutomationCommand {
    NewWorkspace,
    SetGenome {
        source: TrackSource,
        #[serde(default)]
        name: Option<String>,
    },
    LoadTrack {
        source: TrackSource,
        #[serde(default)]
        kind: Option<TrackKind>,
        #[serde(default)]
        name: Option<String>,
    },
    RemoveTrack {
        track_id: TrackId,
    },
    Goto {
        loci: Vec<String>,
    },
    AddPanel {
        locus: String,
    },
    ClosePanel {
        panel_id: PanelId,
    },
    SetOption {
        scope: AutomationOptionScope,
        key: String,
        value: serde_json::Value,
    },
    Group {
        track_ids: Vec<TrackId>,
        mode: AlignmentGroupMode,
        #[serde(default)]
        tag: Option<String>,
    },
    Sort {
        track_ids: Vec<TrackId>,
        mode: AlignmentSortMode,
        #[serde(default)]
        direction: AlignmentOrderDirection,
        #[serde(default)]
        tag: Option<String>,
    },
    Filter {
        track_ids: Vec<TrackId>,
        expression: String,
    },
    Select {
        selection: AutomationSelection,
    },
    WaitUntilSettled {
        #[serde(default = "default_settle_timeout_millis")]
        timeout_millis: u64,
    },
    Snapshot {
        path: String,
        #[serde(default)]
        scope: AutomationSnapshotScope,
        #[serde(default)]
        format: AutomationSnapshotFormat,
    },
    Export {
        path: String,
        kind: AutomationExportKind,
    },
    Save {
        path: String,
    },
    Exit,
}

impl AutomationCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::NewWorkspace => "new_workspace",
            Self::SetGenome { .. } => "set_genome",
            Self::LoadTrack { .. } => "load_track",
            Self::RemoveTrack { .. } => "remove_track",
            Self::Goto { .. } => "goto",
            Self::AddPanel { .. } => "add_panel",
            Self::ClosePanel { .. } => "close_panel",
            Self::SetOption { .. } => "set_option",
            Self::Group { .. } => "group",
            Self::Sort { .. } => "sort",
            Self::Filter { .. } => "filter",
            Self::Select { .. } => "select",
            Self::WaitUntilSettled { .. } => "wait_until_settled",
            Self::Snapshot { .. } => "snapshot",
            Self::Export { .. } => "export",
            Self::Save { .. } => "save",
            Self::Exit => "exit",
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::NewWorkspace | Self::Exit => Ok(()),
            Self::SetGenome { source, name } | Self::LoadTrack { source, name, .. } => {
                validate_source(source)?;
                validate_optional_string(name.as_deref(), "name")
            }
            Self::RemoveTrack { track_id } => validate_track_id(*track_id),
            Self::Goto { loci } => {
                if loci.is_empty() || loci.len() > MAXIMUM_AUTOMATION_LOCI {
                    return Err(format!(
                        "loci must contain 1..={MAXIMUM_AUTOMATION_LOCI} entries"
                    ));
                }
                for locus in loci {
                    validate_string(locus, "locus")?;
                }
                Ok(())
            }
            Self::AddPanel { locus } => validate_string(locus, "locus"),
            Self::ClosePanel { panel_id } => {
                if panel_id.get() == 0 {
                    Err("panel_id must be positive".to_owned())
                } else {
                    Ok(())
                }
            }
            Self::SetOption { scope, key, value } => {
                scope.validate()?;
                validate_string(key, "option key")?;
                let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
                if bytes.len() > MAXIMUM_AUTOMATION_OPTION_BYTES {
                    return Err(format!(
                        "option value exceeds {MAXIMUM_AUTOMATION_OPTION_BYTES} bytes"
                    ));
                }
                Ok(())
            }
            Self::Group { track_ids, tag, .. } | Self::Sort { track_ids, tag, .. } => {
                validate_track_selection(track_ids)?;
                validate_optional_sam_tag(tag.as_deref())
            }
            Self::Filter {
                track_ids,
                expression,
            } => {
                validate_track_selection(track_ids)?;
                validate_string(expression, "filter expression")
            }
            Self::Select { selection } => selection.validate(),
            Self::WaitUntilSettled { timeout_millis } => {
                if (MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS
                    ..=MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS)
                    .contains(timeout_millis)
                {
                    Ok(())
                } else {
                    Err(format!(
                        "timeout_millis must be between {MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS} and {MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS}"
                    ))
                }
            }
            Self::Snapshot { path, .. } | Self::Export { path, .. } | Self::Save { path } => {
                validate_string(path, "output path")
            }
        }
    }

    fn redact_sources(&mut self) {
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                *source = source.redacted_for_persistence();
            }
            _ => {}
        }
    }

    fn resolve_paths(&mut self, root: &Path) {
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                if let TrackSource::Local { path } = source {
                    let source_path = Path::new(path);
                    if source_path.is_relative() {
                        *path = normalize_automation_path(&root.join(source_path))
                            .display()
                            .to_string();
                    }
                }
            }
            Self::Snapshot { path, .. } | Self::Export { path, .. } | Self::Save { path } => {
                let output = PathBuf::from(&*path);
                if output.is_relative() {
                    *path = normalize_automation_path(&root.join(output))
                        .display()
                        .to_string();
                }
            }
            _ => {}
        }
    }

    fn resolve_paths_within(&mut self, root: &Path) -> Result<(), AutomationError> {
        let resolve = |value: &mut String| -> Result<(), AutomationError> {
            let path = Path::new(value);
            let candidate = if path.is_relative() {
                root.join(path)
            } else {
                path.to_owned()
            };
            let resolved = normalize_automation_path(&candidate);
            if !resolved.starts_with(root) {
                return Err(AutomationError::PathOutsideControlRoot(
                    path.display().to_string(),
                ));
            }
            *value = resolved.display().to_string();
            Ok(())
        };
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                if let TrackSource::Local { path } = source {
                    resolve(path)?;
                }
            }
            Self::Snapshot { path, .. } | Self::Export { path, .. } | Self::Save { path } => {
                resolve(path)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn normalize_automation_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AutomationOptionScope {
    Workspace,
    Track { track_id: TrackId },
    Panel { panel_id: PanelId },
}

impl AutomationOptionScope {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Workspace => Ok(()),
            Self::Track { track_id } => validate_track_id(*track_id),
            Self::Panel { panel_id } if panel_id.get() > 0 => Ok(()),
            Self::Panel { .. } => Err("panel_id must be positive".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum AutomationSelection {
    Track {
        track_id: TrackId,
    },
    Panel {
        panel_id: PanelId,
    },
    Position {
        panel_id: PanelId,
        track_id: TrackId,
        position: u64,
    },
}

impl AutomationSelection {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Track { track_id } => validate_track_id(*track_id),
            Self::Panel { panel_id } if panel_id.get() > 0 => Ok(()),
            Self::Panel { .. } => Err("panel_id must be positive".to_owned()),
            Self::Position {
                panel_id, track_id, ..
            } => {
                if panel_id.get() == 0 {
                    return Err("panel_id must be positive".to_owned());
                }
                validate_track_id(*track_id)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotScope {
    #[default]
    VisibleWorkspace,
    ActivePanel,
    FullActivePanel,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotFormat {
    #[default]
    Png,
    Svg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationExportKind {
    Alignments,
    Pileup,
    SelectedEvidence,
    Modifications,
    Junctions,
    Variants,
    Quantitative,
    Regions,
    TrackNames,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationErrorCode {
    InvalidBatch,
    UnsupportedVersion,
    InvalidCommand,
    UnsupportedCommand,
    ResourceNotFound,
    InvalidState,
    Timeout,
    Io,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationCommandStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationCommandResult {
    pub command_index: usize,
    #[serde(default)]
    pub command: String,
    pub status: AutomationCommandStatus,
    #[serde(default)]
    pub error_code: Option<AutomationErrorCode>,
    pub message: String,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default)]
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationEnvironmentTelemetry {
    #[serde(default)]
    pub application_version: String,
    #[serde(default)]
    pub operating_system: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub gpu_adapter: String,
    #[serde(default)]
    pub gpu_backend: String,
    #[serde(default)]
    pub surface_width_pixels: u32,
    #[serde(default)]
    pub surface_height_pixels: u32,
    #[serde(default)]
    pub scale_factor: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationFrameTelemetry {
    #[serde(default)]
    pub presented_frames: u64,
    #[serde(default)]
    pub samples_ms: Vec<f64>,
    #[serde(default)]
    pub samples_dropped: u64,
    #[serde(default)]
    pub median_ms: Option<f64>,
    #[serde(default)]
    pub p95_ms: Option<f64>,
    #[serde(default)]
    pub maximum_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationGpuTelemetry {
    #[serde(default)]
    pub prepared_frames: u64,
    #[serde(default)]
    pub uploaded_frames: u64,
    #[serde(default)]
    pub total_uploaded_bytes: u64,
    #[serde(default)]
    pub peak_uploaded_bytes: usize,
    #[serde(default)]
    pub peak_instances: usize,
    #[serde(default)]
    pub buffer_reallocations: u64,
    #[serde(default)]
    pub truncated_frames: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunTelemetry {
    #[serde(default)]
    pub run_elapsed_ms: f64,
    #[serde(default)]
    pub startup_to_first_frame_ms: Option<f64>,
    #[serde(default)]
    pub startup_to_first_settled_ms: Option<f64>,
    #[serde(default)]
    pub environment: AutomationEnvironmentTelemetry,
    #[serde(default)]
    pub frames: AutomationFrameTelemetry,
    #[serde(default)]
    pub gpu: AutomationGpuTelemetry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunReport {
    pub schema_version: u32,
    pub succeeded: bool,
    pub results: Vec<AutomationCommandResult>,
    #[serde(default)]
    pub telemetry: AutomationRunTelemetry,
}

impl AutomationRunReport {
    pub fn to_json(&self) -> Result<String, AutomationError> {
        Ok(format!("{}\n", serde_json::to_string_pretty(self)?))
    }
}

fn default_settle_timeout_millis() -> u64 {
    30_000
}

fn validate_source(source: &TrackSource) -> Result<(), String> {
    match source {
        TrackSource::Local { path } => validate_string(path, "local source path"),
        TrackSource::Remote { url } => validate_string(url, "remote source URL"),
    }
}

fn validate_track_selection(track_ids: &[TrackId]) -> Result<(), String> {
    if track_ids.is_empty() || track_ids.len() > MAXIMUM_AUTOMATION_TRACK_SELECTION {
        return Err(format!(
            "track_ids must contain 1..={MAXIMUM_AUTOMATION_TRACK_SELECTION} entries"
        ));
    }
    for track_id in track_ids {
        validate_track_id(*track_id)?;
    }
    Ok(())
}

fn validate_track_id(track_id: TrackId) -> Result<(), String> {
    if track_id.get() == 0 {
        Err("track_id must be positive".to_owned())
    } else {
        Ok(())
    }
}

fn validate_optional_sam_tag(tag: Option<&str>) -> Result<(), String> {
    let Some(tag) = tag else {
        return Ok(());
    };
    let bytes = tag.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric() {
        Ok(())
    } else {
        Err("tag must be a valid two-character SAM tag".to_owned())
    }
}

fn validate_optional_string(value: Option<&str>, label: &str) -> Result<(), String> {
    value.map_or(Ok(()), |value| validate_string(value, label))
}

fn validate_string(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAXIMUM_AUTOMATION_STRING_BYTES {
        Err(format!(
            "{label} must be non-empty and no more than {MAXIMUM_AUTOMATION_STRING_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("automation schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("automation batch contains {0} commands; expected 1..={MAXIMUM_AUTOMATION_COMMANDS}")]
    InvalidCommandCount(usize),
    #[error("automation command {index} is invalid: {reason}")]
    InvalidCommand { index: usize, reason: String },
    #[error("automation path {0} is outside the selected local-control root")]
    PathOutsideControlRoot(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_batch_round_trip_covers_the_public_command_vocabulary() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::NewWorkspace,
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local {
                        path: "data/reads.bam".to_owned(),
                    },
                    kind: Some(TrackKind::Alignment),
                    name: Some("Tumour".to_owned()),
                },
                AutomationCommand::Goto {
                    loci: vec!["chr1:101-200".to_owned(), "chr2:301-400".to_owned()],
                },
                AutomationCommand::WaitUntilSettled {
                    timeout_millis: 30_000,
                },
                AutomationCommand::Snapshot {
                    path: "output/view.png".to_owned(),
                    scope: AutomationSnapshotScope::VisibleWorkspace,
                    format: AutomationSnapshotFormat::Png,
                },
                AutomationCommand::Save {
                    path: "output/project.json".to_owned(),
                },
                AutomationCommand::Exit,
            ],
        };

        let json = batch.to_json().unwrap();
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);
        assert_eq!(
            batch
                .commands
                .iter()
                .map(AutomationCommand::name)
                .collect::<Vec<_>>(),
            [
                "new_workspace",
                "load_track",
                "goto",
                "wait_until_settled",
                "snapshot",
                "save",
                "exit",
            ]
        );
    }

    #[test]
    fn version_one_reports_without_telemetry_receive_safe_defaults() {
        let report: AutomationRunReport = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "succeeded": true,
                "results": [{
                    "command_index": 0,
                    "status": "succeeded",
                    "message": "done",
                    "data": null
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(report.telemetry, AutomationRunTelemetry::default());
        assert_eq!(report.results[0].command, "");
        assert!(report.results[0].elapsed_ms.abs() < f64::EPSILON);
    }

    #[test]
    fn future_versions_invalid_ids_and_unbounded_timeouts_are_rejected() {
        let future = r#"{"schema_version":99,"commands":[{"command":"exit"}]}"#;
        assert!(matches!(
            AutomationBatch::from_json(future),
            Err(AutomationError::UnsupportedVersion(99))
        ));

        let invalid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::WaitUntilSettled {
                timeout_millis: MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS + 1,
            }],
        };
        assert!(matches!(
            invalid.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));
    }

    #[test]
    fn persisted_batch_sources_never_retain_remote_credentials() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::LoadTrack {
                source: TrackSource::Remote {
                    url: "https://user:secret@example.org/reads.bam?token=private".to_owned(),
                },
                kind: Some(TrackKind::Alignment),
                name: None,
            }],
        };
        let json = batch.to_json().unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("token"));
        assert!(json.contains("https://example.org/reads.bam"));
    }

    #[test]
    fn reading_a_batch_roots_every_local_input_and_output_beside_the_batch() {
        let directory = tempfile::tempdir().unwrap();
        let batch_path = directory.path().join("review.json");
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::SetGenome {
                    source: TrackSource::Local {
                        path: "data/reference.fa".to_owned(),
                    },
                    name: None,
                },
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local {
                        path: "data/reads.bam".to_owned(),
                    },
                    kind: Some(TrackKind::Alignment),
                    name: None,
                },
                AutomationCommand::Snapshot {
                    path: "../results/view.svg".to_owned(),
                    scope: AutomationSnapshotScope::VisibleWorkspace,
                    format: AutomationSnapshotFormat::Svg,
                },
                AutomationCommand::Save {
                    path: "results/project.json".to_owned(),
                },
            ],
        };
        std::fs::write(&batch_path, batch.to_json().unwrap()).unwrap();

        let loaded = AutomationBatch::read(&batch_path).unwrap();
        let root = std::path::absolute(directory.path()).unwrap();
        assert!(matches!(
            &loaded.commands[0],
            AutomationCommand::SetGenome {
                source: TrackSource::Local { path },
                ..
            } if Path::new(path) == root.join("data/reference.fa")
        ));
        assert!(matches!(
            &loaded.commands[1],
            AutomationCommand::LoadTrack {
                source: TrackSource::Local { path },
                ..
            } if Path::new(path) == root.join("data/reads.bam")
        ));
        assert!(matches!(
            &loaded.commands[2],
            AutomationCommand::Snapshot { path, .. }
                if Path::new(path) == root.parent().unwrap().join("results/view.svg")
        ));
        assert!(matches!(
            &loaded.commands[3],
            AutomationCommand::Save { path }
                if Path::new(path) == root.join("results/project.json")
        ));
    }

    #[test]
    fn local_control_paths_cannot_escape_the_selected_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("allowed");
        std::fs::create_dir(&root).unwrap();
        let mut accepted = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Snapshot {
                path: "results/view.svg".to_owned(),
                scope: AutomationSnapshotScope::VisibleWorkspace,
                format: AutomationSnapshotFormat::Svg,
            }],
        };
        accepted.resolve_paths_within(&root).unwrap();
        assert!(matches!(
            &accepted.commands[0],
            AutomationCommand::Snapshot { path, .. }
                if Path::new(path) == std::path::absolute(&root).unwrap().join("results/view.svg")
        ));

        let mut traversal = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Save {
                path: "../outside.json".to_owned(),
            }],
        };
        assert!(matches!(
            traversal.resolve_paths_within(&root),
            Err(AutomationError::PathOutsideControlRoot(_))
        ));

        let mut absolute = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::LoadTrack {
                source: TrackSource::Local {
                    path: directory.path().join("private.bam").display().to_string(),
                },
                kind: Some(TrackKind::Alignment),
                name: None,
            }],
        };
        assert!(matches!(
            absolute.resolve_paths_within(&root),
            Err(AutomationError::PathOutsideControlRoot(_))
        ));
    }

    #[test]
    fn checked_in_desktop_smoke_batch_matches_the_public_schema() {
        let batch =
            AutomationBatch::from_json(include_str!("../../../validation/automation-smoke.json"))
                .unwrap();
        assert_eq!(batch.commands.len(), 19);
        assert!(matches!(
            batch.commands.last(),
            Some(AutomationCommand::Exit)
        ));
    }
}
