use std::path::PathBuf;

use clap::Parser;
use reqwest::Url;
use thiserror::Error;

const DEFAULT_CONTROL_URL: &str = "http://127.0.0.1:60151";

#[derive(Clone, Debug, Parser)]
#[command(
    name = "rgv-mcp",
    about = "Expose a controlled live RGV desktop window to an MCP client"
)]
pub struct RgvMcpArgs {
    /// Loopback URL of the RGV local-control service.
    #[arg(long)]
    pub control_url: Option<String>,
    /// Protected file containing the RGV local-control bearer secret.
    #[arg(long)]
    pub control_secret_file: Option<PathBuf>,
    /// Filesystem root selected for the matching RGV local-control service.
    #[arg(long)]
    pub control_root: Option<PathBuf>,
    /// Root-relative directory owned by this MCP process for temporary captures.
    #[arg(long, default_value = ".rgv-mcp-artifacts")]
    pub artifact_directory: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct RgvMcpConfig {
    pub(crate) control_url: Url,
    pub(crate) control_secret: String,
    pub(crate) control_root: PathBuf,
    pub(crate) artifact_directory: PathBuf,
}

impl RgvMcpArgs {
    pub(crate) fn load(self) -> Result<RgvMcpConfig, ConfigError> {
        let control_url = self
            .control_url
            .or_else(|| std::env::var("RGV_MCP_CONTROL_URL").ok())
            .unwrap_or_else(|| DEFAULT_CONTROL_URL.to_owned());
        let control_url = validated_control_url(&control_url)?;
        let secret_file = self
            .control_secret_file
            .or_else(|| std::env::var_os("RGV_MCP_CONTROL_SECRET_FILE").map(PathBuf::from))
            .ok_or(ConfigError::MissingSecretFile)?;
        let control_secret = read_control_secret(&secret_file)?;
        let control_root = self
            .control_root
            .or_else(|| std::env::var_os("RGV_MCP_CONTROL_ROOT").map(PathBuf::from))
            .unwrap_or(std::env::current_dir().map_err(ConfigError::CurrentDirectory)?);
        let control_root =
            control_root
                .canonicalize()
                .map_err(|source| ConfigError::ControlRoot {
                    path: control_root,
                    source,
                })?;
        validate_relative_path(&self.artifact_directory)
            .map_err(ConfigError::InvalidArtifactDirectory)?;
        Ok(RgvMcpConfig {
            control_url,
            control_secret,
            control_root,
            artifact_directory: self.artifact_directory,
        })
    }
}

pub(crate) fn validated_control_url(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(ConfigError::ControlUrl)?;
    if url.scheme() != "http"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(ConfigError::UnsafeControlUrl);
    }
    let is_loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !is_loopback || url.port().is_none() {
        return Err(ConfigError::UnsafeControlUrl);
    }
    Ok(url)
}

pub(crate) fn validate_relative_path(path: &std::path::Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("path must be a non-empty control-root-relative path".to_owned());
    }
    if path
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("path must contain only ordinary relative components".to_owned());
    }
    Ok(())
}

fn read_control_secret(path: &std::path::Path) -> Result<String, ConfigError> {
    const MINIMUM_BYTES: usize = 32;
    const MAXIMUM_BYTES: usize = 1_024;
    let metadata = std::fs::metadata(path).map_err(|source| ConfigError::SecretFile {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_BYTES as u64 {
        return Err(ConfigError::InvalidSecretFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(ConfigError::InsecureSecretPermissions);
        }
    }
    let secret = std::fs::read_to_string(path).map_err(|source| ConfigError::SecretFile {
        path: path.to_owned(),
        source,
    })?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if !(MINIMUM_BYTES..=MAXIMUM_BYTES).contains(&secret.len())
        || secret.trim() != secret
        || secret.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidSecretFile);
    }
    Ok(secret.to_owned())
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("--control-secret-file or RGV_MCP_CONTROL_SECRET_FILE is required")]
    MissingSecretFile,
    #[error("the control URL is invalid: {0}")]
    ControlUrl(url::ParseError),
    #[error("the control URL must be an HTTP URL with an explicit loopback IP and port")]
    UnsafeControlUrl,
    #[error("could not resolve the current directory: {0}")]
    CurrentDirectory(std::io::Error),
    #[error("could not resolve control root {path}: {source}")]
    ControlRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid artifact directory: {0}")]
    InvalidArtifactDirectory(String),
    #[error("could not read control secret file {path}: {source}")]
    SecretFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the control secret file must be a bounded regular file containing one printable 32..=1024 byte line"
    )]
    InvalidSecretFile,
    #[error("the control secret file must not be accessible by group or others")]
    InsecureSecretPermissions,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_url_requires_an_explicit_loopback_ip_and_port() {
        assert!(validated_control_url("http://127.0.0.1:60151").is_ok());
        assert!(validated_control_url("http://[::1]:60151").is_ok());
        for rejected in [
            "http://localhost:60151",
            "https://127.0.0.1:60151",
            "http://192.168.1.2:60151",
            "http://127.0.0.1",
            "http://user@127.0.0.1:60151",
            "http://127.0.0.1:60151/private",
        ] {
            assert!(
                validated_control_url(rejected).is_err(),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn artifact_paths_cannot_be_absolute_or_traverse() {
        assert!(validate_relative_path(std::path::Path::new("artifacts/view.png")).is_ok());
        assert!(validate_relative_path(std::path::Path::new("../view.png")).is_err());
        assert!(validate_relative_path(std::path::Path::new("/tmp/view.png")).is_err());
    }
}
