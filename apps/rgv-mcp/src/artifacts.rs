use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use thiserror::Error;

use crate::config::validate_relative_path;

const MAXIMUM_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ArtifactStore {
    root: Arc<PathBuf>,
    temporary_directory: Arc<PathBuf>,
    counter: Arc<AtomicU64>,
}

impl ArtifactStore {
    pub(crate) fn new(root: &Path, relative_directory: &Path) -> Result<Self, ArtifactError> {
        validate_relative_path(relative_directory).map_err(ArtifactError::InvalidPath)?;
        let temporary_directory = ensure_directory_without_symlinks(root, relative_directory)?;
        let root = root.canonicalize()?;
        let temporary_directory = temporary_directory.canonicalize()?;
        if !temporary_directory.starts_with(&root) {
            return Err(ArtifactError::OutsideRoot);
        }
        Ok(Self {
            root: Arc::new(root),
            temporary_directory: Arc::new(temporary_directory),
            counter: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Reserves a unique temporary artifact path. The file the viewer writes there is removed
    /// when the returned guard is dropped, so every exit path of a tool discards it.
    pub(crate) fn temporary(&self, extension: &str) -> TemporaryArtifact {
        let identity = self.counter.fetch_add(1, Ordering::Relaxed);
        TemporaryArtifact {
            path: self.temporary_directory.join(format!(
                "rgv-mcp-{}-{identity}.{extension}",
                std::process::id()
            )),
            store: self.clone(),
        }
    }

    fn read_temporary_capture(&self, path: &Path) -> Result<Vec<u8>, ArtifactError> {
        self.read_confined(path, MAXIMUM_CAPTURE_BYTES)
    }

    fn discard_temporary(&self, path: &Path) {
        if path.parent() == Some(self.temporary_directory.as_ref()) {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(crate) fn durable_path(&self, relative: &Path) -> Result<PathBuf, ArtifactError> {
        validate_relative_path(relative).map_err(ArtifactError::InvalidPath)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new("."));
        let directory = ensure_directory_without_symlinks(&self.root, parent)?;
        let name = relative
            .file_name()
            .ok_or_else(|| ArtifactError::InvalidPath("missing file name".to_owned()))?;
        let candidate = directory.join(name);
        if candidate.exists()
            && std::fs::symlink_metadata(&candidate)?
                .file_type()
                .is_symlink()
        {
            return Err(ArtifactError::Symlink);
        }
        Ok(candidate)
    }

    pub(crate) fn read_durable_text(
        &self,
        path: &Path,
        maximum_bytes: usize,
    ) -> Result<Option<String>, ArtifactError> {
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > maximum_bytes as u64 {
            return Ok(None);
        }
        let bytes = self.read_confined(path, maximum_bytes)?;
        Ok(String::from_utf8(bytes).ok())
    }

    pub(crate) fn relative_display(&self, path: &Path) -> Result<String, ArtifactError> {
        let path = path.canonicalize()?;
        let relative = path
            .strip_prefix(self.root.as_ref())
            .map_err(|_| ArtifactError::OutsideRoot)?;
        Ok(relative.display().to_string())
    }

    fn read_confined(&self, path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, ArtifactError> {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ArtifactError::NotRegularFile);
        }
        if metadata.len() > maximum_bytes as u64 {
            return Err(ArtifactError::TooLarge);
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(self.root.as_ref()) {
            return Err(ArtifactError::OutsideRoot);
        }
        let bytes = std::fs::read(canonical)?;
        if bytes.len() > maximum_bytes {
            return Err(ArtifactError::TooLarge);
        }
        Ok(bytes)
    }
}

/// A temporary artifact owned by one tool call. Dropping the guard removes the file, whether the
/// call succeeded, failed validation, or returned early.
pub(crate) struct TemporaryArtifact {
    path: PathBuf,
    store: ArtifactStore,
}

impl TemporaryArtifact {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the bounded, root-confined capture. The file itself is removed on drop.
    pub(crate) fn read_capture(&self) -> Result<Vec<u8>, ArtifactError> {
        self.store.read_temporary_capture(&self.path)
    }
}

impl Drop for TemporaryArtifact {
    fn drop(&mut self) {
        self.store.discard_temporary(&self.path);
    }
}

fn ensure_directory_without_symlinks(
    root: &Path,
    relative: &Path,
) -> Result<PathBuf, ArtifactError> {
    let mut directory = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            if component == std::path::Component::CurDir {
                continue;
            }
            return Err(ArtifactError::InvalidPath(
                "directory contains a non-relative component".to_owned(),
            ));
        };
        directory.push(component);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ArtifactError::Symlink);
            }
            Ok(metadata) if !metadata.is_dir() => return Err(ArtifactError::NotDirectory),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&directory)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(directory)
}

#[derive(Debug, Error)]
pub(crate) enum ArtifactError {
    #[error("invalid artifact path: {0}")]
    InvalidPath(String),
    #[error("artifact path traverses a symbolic link")]
    Symlink,
    #[error("artifact directory component is not a directory")]
    NotDirectory,
    #[error("artifact is not a regular file")]
    NotRegularFile,
    #[error("artifact lies outside the configured control root")]
    OutsideRoot,
    #[error("artifact exceeds its byte limit")]
    TooLarge,
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_artifacts_are_confined_and_removed_after_reading() {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), Path::new("artifacts")).unwrap();
        let temporary = store.temporary("png");
        let path = temporary.path().to_owned();
        std::fs::write(&path, b"png").unwrap();
        assert_eq!(temporary.read_capture().unwrap(), b"png");
        assert!(path.exists(), "the guard, not the read, owns removal");
        drop(temporary);
        assert!(!path.exists());
    }

    #[test]
    fn temporary_artifact_is_removed_on_an_early_return_without_a_read() {
        fn fails_before_reading(store: &ArtifactStore) -> Result<PathBuf, PathBuf> {
            let temporary = store.temporary("png");
            std::fs::write(temporary.path(), b"png").unwrap();
            // Models a validation failure between the viewer write and the adapter read.
            Err(temporary.path().to_owned())
        }

        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), Path::new("artifacts")).unwrap();
        let path = fails_before_reading(&store).unwrap_err();
        assert!(!path.exists());
    }

    #[test]
    fn temporary_artifact_guard_tolerates_a_file_that_was_never_written() {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), Path::new("artifacts")).unwrap();
        let temporary = store.temporary("png");
        let path = temporary.path().to_owned();
        assert!(temporary.read_capture().is_err());
        drop(temporary);
        assert!(!path.exists());
        assert_ne!(store.temporary("png").path(), path);
    }

    #[test]
    fn oversized_and_non_regular_captures_are_rejected_and_discarded() {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), Path::new("artifacts")).unwrap();
        let temporary = store.temporary("png");
        std::fs::create_dir(temporary.path()).unwrap();
        assert!(matches!(
            temporary.read_capture(),
            Err(ArtifactError::NotRegularFile)
        ));
        let directory = temporary.path().to_owned();
        drop(temporary);
        // Only regular files are discarded; a directory is left for the operator to inspect.
        assert!(directory.is_dir());
        assert!(matches!(
            store.read_confined(&directory, 1),
            Err(ArtifactError::NotRegularFile)
        ));
        let file = root.path().join("artifacts/large.txt");
        std::fs::write(&file, b"four").unwrap();
        assert!(matches!(
            store.read_confined(&file, 3),
            Err(ArtifactError::TooLarge)
        ));
        assert_eq!(store.read_durable_text(&file, 3).unwrap(), None);
    }

    #[test]
    fn durable_paths_reject_traversal_and_absolute_arguments() {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), Path::new("artifacts")).unwrap();
        assert!(matches!(
            store.durable_path(Path::new("../escape.tsv")),
            Err(ArtifactError::InvalidPath(_))
        ));
        assert!(matches!(
            store.durable_path(Path::new("/etc/passwd")),
            Err(ArtifactError::InvalidPath(_))
        ));
        let accepted = store.durable_path(Path::new("exports/a.tsv")).unwrap();
        std::fs::write(&accepted, b"x").unwrap();
        assert_eq!(
            store.relative_display(&accepted).unwrap(),
            Path::new("exports").join("a.tsv").display().to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_directory_rejects_a_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("artifacts")).unwrap();
        assert!(matches!(
            ArtifactStore::new(root.path(), Path::new("artifacts")),
            Err(ArtifactError::Symlink)
        ));
    }
}
