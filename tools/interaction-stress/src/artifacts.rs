use std::{
    fmt::Write as _,
    fs::OpenOptions,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use rgv_automation::{InteractionStressRunManifest, StressArtifactReference, StressRunState};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

pub struct RunArtifacts {
    root: PathBuf,
    manifest: InteractionStressRunManifest,
}

impl RunArtifacts {
    pub fn create(
        parent: impl AsRef<Path>,
        mut manifest: InteractionStressRunManifest,
    ) -> anyhow::Result<Self> {
        let parent = parent.as_ref();
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create artifact root {}", parent.display()))?;
        let root = parent.join(&manifest.run_id);
        std::fs::create_dir(&root)
            .with_context(|| format!("could not create run directory {}", root.display()))?;
        for child in ["automation", "diagnostics", "recording", "reduction"] {
            std::fs::create_dir(root.join(child))?;
        }
        manifest.state = StressRunState::Running;
        let artifacts = Self { root, manifest };
        artifacts.persist_manifest()?;
        Ok(artifacts)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &InteractionStressRunManifest {
        &self.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut InteractionStressRunManifest {
        &mut self.manifest
    }

    pub fn write_json<T: Serialize>(
        &mut self,
        relative: impl AsRef<Path>,
        kind: &str,
        value: &T,
    ) -> anyhow::Result<PathBuf> {
        let relative = validate_relative(relative.as_ref())?;
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write_json(&path, value)?;
        self.register(kind, relative, true)?;
        Ok(path)
    }

    pub fn append_json_line<T: Serialize>(
        &self,
        relative: impl AsRef<Path>,
        value: &T,
        durable: bool,
    ) -> anyhow::Result<()> {
        let relative = validate_relative(relative.as_ref())?;
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.flush()?;
        if durable {
            file.sync_data()?;
        }
        Ok(())
    }

    pub fn register(
        &mut self,
        kind: &str,
        relative: impl AsRef<Path>,
        complete: bool,
    ) -> anyhow::Result<()> {
        let relative = validate_relative(relative.as_ref())?;
        let path = self.root.join(&relative);
        let sha256 = path.is_file().then(|| checksum(&path)).transpose()?;
        self.manifest
            .artifacts
            .retain(|artifact| artifact.path != relative);
        self.manifest.artifacts.push(StressArtifactReference {
            kind: kind.to_owned(),
            path: relative,
            sha256,
            complete,
        });
        self.persist_manifest()
    }

    pub fn finish(
        &mut self,
        state: StressRunState,
        finished_unix_millis: u64,
    ) -> anyhow::Result<()> {
        self.manifest.state = state;
        self.manifest.finished_unix_millis = Some(finished_unix_millis);
        self.persist_manifest()
    }

    pub fn persist_manifest(&self) -> anyhow::Result<()> {
        atomic_write_json(self.root.join("stress-run.json"), &self.manifest)
    }
}

pub fn atomic_write_json(path: impl AsRef<Path>, value: &impl Serialize) -> anyhow::Result<()> {
    let mut json = serde_json::to_string_pretty(value)?;
    json.push('\n');
    atomic_write_text(path, &json)
}

pub fn atomic_write_text(path: impl AsRef<Path>, text: &str) -> anyhow::Result<()> {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let temporary = parent.join(format!(".{name}.{}.partial", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("could not atomically write {}", path.display()))
}

fn validate_relative(path: &Path) -> anyhow::Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("artifact path must contain only ordinary relative components");
    }
    Ok(path.to_owned())
}

fn checksum(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let digest = digest.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use rgv_automation::InteractionStressRunManifest;

    use super::*;

    fn manifest() -> InteractionStressRunManifest {
        InteractionStressRunManifest {
            schema_version: 1,
            run_id: "run-1".to_owned(),
            state: StressRunState::Running,
            scenario_name: "test".to_owned(),
            scenario_schema: rgv_automation::INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
            scenario_schema_version: rgv_automation::INTERACTION_STRESS_SCHEMA_VERSION,
            seed: None,
            requirements: rgv_automation::StressRequirements::default(),
            limits: rgv_automation::StressRunLimits::default(),
            environment: rgv_automation::StressRunEnvironment::default(),
            application_instance_id: "instance".to_owned(),
            process_id: 4,
            started_unix_millis: 1,
            finished_unix_millis: None,
            requested_events: 0,
            posted_events: 0,
            received_events: 0,
            applied_events: 0,
            coalesced_events: 0,
            rejected_events: 0,
            emergency_release_events: 0,
            failure: None,
            steps: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn run_manifest_exists_before_other_artifacts_and_finishes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let mut artifacts = RunArtifacts::create(directory.path(), manifest()).unwrap();
        assert!(artifacts.root().join("stress-run.json").is_file());
        artifacts
            .write_json(
                "scenario.input.json",
                "scenario",
                &serde_json::json!({"a": 1}),
            )
            .unwrap();
        artifacts.finish(StressRunState::Passed, 2).unwrap();
        let value: InteractionStressRunManifest = serde_json::from_str(
            &std::fs::read_to_string(artifacts.root().join("stress-run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value.state, StressRunState::Passed);
        assert_eq!(value.artifacts.len(), 1);
        assert!(value.artifacts[0].sha256.is_some());
    }
}
