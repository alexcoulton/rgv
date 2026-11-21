use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use rgv_automation::AUTOMATION_SCHEMA_VERSION;
use rgv_control_client::{ControlClient, ControlStatus};
use serde::{Deserialize, Serialize};
use url::Url;

const ATTACH_SCHEMA_VERSION: u32 = 1;
const MINIMUM_SECRET_BYTES: usize = 32;
const MAXIMUM_SECRET_BYTES: usize = 1_024;
const REQUIRED_CAPABILITIES: [&str; 3] =
    ["workspace_state", "interaction_recording", "stress_health"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachConfiguration {
    pub schema_version: u32,
    pub control_url: String,
    pub secret_file: PathBuf,
    pub control_root: PathBuf,
    #[serde(default)]
    pub expected_application_instance_id: Option<String>,
    #[serde(default = "default_process_name")]
    pub expected_process_name: String,
    /// Operator-declared fixture identity used to gate scenarios whose data cannot be inferred
    /// from redacted application state alone.
    #[serde(default)]
    pub fixture: Option<String>,
}

pub struct AttachedRgv {
    pub client: ControlClient,
    pub status: ControlStatus,
    pub control_root: PathBuf,
    pub expected_process_name: String,
    pub fixture: Option<String>,
}

impl AttachConfiguration {
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("could not read attach configuration {}", path.display()))?;
        let mut configuration: Self = serde_json::from_str(&json)
            .with_context(|| format!("could not parse attach configuration {}", path.display()))?;
        if configuration.schema_version != ATTACH_SCHEMA_VERSION {
            bail!(
                "unsupported attach schema version {}; expected {ATTACH_SCHEMA_VERSION}",
                configuration.schema_version
            );
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if configuration.secret_file.is_relative() {
            configuration.secret_file = parent.join(&configuration.secret_file);
        }
        if configuration.control_root.is_relative() {
            configuration.control_root = parent.join(&configuration.control_root);
        }
        validate_loopback_url(&configuration.control_url)?;
        if configuration.expected_process_name.trim().is_empty() {
            bail!("expected_process_name must not be empty");
        }
        if configuration
            .expected_application_instance_id
            .as_ref()
            .is_some_and(|identity| identity.trim().is_empty())
        {
            bail!("expected_application_instance_id must not be empty when supplied");
        }
        if configuration
            .fixture
            .as_ref()
            .is_some_and(|fixture| fixture.trim().is_empty())
        {
            bail!("fixture must not be empty when supplied");
        }
        Ok(configuration)
    }

    pub async fn attach(&self) -> anyhow::Result<AttachedRgv> {
        let control_root = self.control_root.canonicalize().with_context(|| {
            format!(
                "could not resolve control root {}",
                self.control_root.display()
            )
        })?;
        let secret = read_secret(&self.secret_file)?;
        let url = validate_loopback_url(&self.control_url)?;
        let client = ControlClient::new(url, secret).context("could not create control client")?;
        let status = client
            .status()
            .await
            .context("could not query the selected RGV instance")?;
        validate_control_status(&status)?;
        if let Some(expected) = &self.expected_application_instance_id
            && expected != &status.application_instance_id
        {
            bail!(
                "RGV instance identity changed: expected {expected}, observed {}",
                status.application_instance_id
            );
        }
        let health = client
            .health()
            .await
            .context("could not query authenticated stress health")?;
        if health.application_instance_id != status.application_instance_id
            || health.process_id != status.process_id
        {
            bail!("status and health identify different RGV processes");
        }
        Ok(AttachedRgv {
            client,
            status,
            control_root,
            expected_process_name: self.expected_process_name.clone(),
            fixture: self.fixture.clone(),
        })
    }
}

fn validate_control_status(status: &ControlStatus) -> anyhow::Result<()> {
    if status.service != "rgv-local-control" || status.authentication != "bearer" {
        bail!("the endpoint is not an authenticated RGV local-control service");
    }
    if status.schema_version != AUTOMATION_SCHEMA_VERSION
        || status.control_schema_version != Some(AUTOMATION_SCHEMA_VERSION)
    {
        bail!(
            "RGV control schema is incompatible: expected {}, observed status={} control={:?}",
            AUTOMATION_SCHEMA_VERSION,
            status.schema_version,
            status.control_schema_version
        );
    }
    if status.process_id == 0
        || status.window_count != 1
        || status.active_window_ordinal != 1
        || status.application_instance_id.is_empty()
    {
        bail!("RGV did not report one unambiguous live application window");
    }
    if status.surface_width_pixels == 0
        || status.surface_height_pixels == 0
        || !status.scale_factor.is_finite()
        || status.scale_factor <= 0.0
    {
        bail!("RGV reported invalid surface geometry or scale");
    }
    let missing = REQUIRED_CAPABILITIES
        .iter()
        .copied()
        .filter(|required| {
            !status
                .capabilities
                .iter()
                .any(|available| available == required)
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "RGV does not advertise required interaction-stress capabilities: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

fn default_process_name() -> String {
    "rgv-desktop".to_owned()
}

fn validate_loopback_url(value: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).context("control_url is not a valid URL")?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http"
        || !loopback
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        bail!("control_url must be an HTTP loopback URL with an explicit port");
    }
    Ok(url)
}

fn read_secret(path: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("could not inspect control secret {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_SECRET_BYTES as u64 {
        bail!("control secret must be a bounded regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("control secret must not be accessible by group or others");
        }
    }
    let secret = std::fs::read_to_string(path)
        .with_context(|| format!("could not read control secret {}", path.display()))?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if !(MINIMUM_SECRET_BYTES..=MAXIMUM_SECRET_BYTES).contains(&secret.len())
        || secret.trim() != secret
        || secret.chars().any(char::is_control)
    {
        bail!("control secret must be one printable 32..=1024-byte line");
    }
    Ok(secret.to_owned())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn relative_paths_are_resolved_from_the_attach_document() {
        let directory = tempfile::tempdir().unwrap();
        let secret = directory.path().join("secret");
        std::fs::write(&secret, "0123456789abcdef0123456789abcdef").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let path = directory.path().join("control.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"control_url":"http://127.0.0.1:60151","secret_file":"secret","control_root":"."}"#,
        )
        .unwrap();
        let configuration = AttachConfiguration::read(path).unwrap();
        assert_eq!(configuration.secret_file, secret);
        assert_eq!(configuration.control_root, directory.path());
    }

    #[test]
    fn non_loopback_control_is_rejected() {
        assert!(validate_loopback_url("http://example.com:60151").is_err());
        assert!(validate_loopback_url("https://127.0.0.1:60151").is_err());
        assert!(validate_loopback_url("http://127.0.0.1").is_err());
    }

    #[test]
    fn control_status_requires_exact_schema_identity_and_stress_capabilities() {
        let mut status = ControlStatus {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            control_schema_version: Some(AUTOMATION_SCHEMA_VERSION),
            application_version: "test".to_owned(),
            application_instance_id: "instance-1".to_owned(),
            process_id: 42,
            window_count: 1,
            active_window_ordinal: 1,
            renderer_backend: "metal".to_owned(),
            surface_width_pixels: 1280,
            surface_height_pixels: 800,
            scale_factor: 1.0,
            service: "rgv-local-control".to_owned(),
            authentication: "bearer".to_owned(),
            capabilities: REQUIRED_CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        };
        assert!(validate_control_status(&status).is_ok());
        status.capabilities.retain(|value| value != "stress_health");
        assert!(
            validate_control_status(&status)
                .unwrap_err()
                .to_string()
                .contains("stress_health")
        );
    }
}
