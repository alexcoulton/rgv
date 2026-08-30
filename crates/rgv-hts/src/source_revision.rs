use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use rgv_core::ReferenceDictionary;
use sha2::{Digest, Sha256};

use crate::{AlignmentHeaderMetadata, UltimaTagOrientation};

/// Physical alignment encoding whose coordinates are interpreted by a source adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentSourceFormat {
    Bam,
    Cram,
}

/// Opaque identity of one validated alignment source generation.
///
/// `source` identifies the logical object. `revision` identifies the exact file/index snapshot
/// accepted when that object was opened. Callers may provide a previously persisted stamp when a
/// local copy and an HTTP object are known to be the same immutable resource; otherwise adapters
/// derive a conservative access-path-specific stamp automatically.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentSourceStamp {
    source: [u8; 16],
    revision: [u8; 16],
}

/// Metadata guard for the exact local file/index pair held by an open source.
#[derive(Debug)]
pub(crate) struct LocalSourceGuard {
    source_path: PathBuf,
    source_version: LocalFileVersion,
    index_path: PathBuf,
    index_version: LocalFileVersion,
}

impl LocalSourceGuard {
    pub(crate) fn capture(source_path: &Path, index_path: &Path) -> io::Result<Self> {
        Ok(Self {
            source_path: source_path.to_owned(),
            source_version: LocalFileVersion::capture(source_path)?,
            index_path: index_path.to_owned(),
            index_version: LocalFileVersion::capture(index_path)?,
        })
    }

    pub(crate) fn validate(&self) -> io::Result<bool> {
        Ok(
            LocalFileVersion::capture(&self.source_path)? == self.source_version
                && LocalFileVersion::capture(&self.index_path)? == self.index_version,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalFileVersion {
    length: u64,
    modified_seconds: u64,
    modified_nanoseconds: u32,
}

impl LocalFileVersion {
    fn capture(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok());
        Ok(Self {
            length: metadata.len(),
            modified_seconds: modified.map_or(0, |value| value.as_secs()),
            modified_nanoseconds: modified.map_or(0, |value| value.subsec_nanos()),
        })
    }
}

impl AlignmentSourceStamp {
    #[must_use]
    pub const fn new(source: [u8; 16], revision: [u8; 16]) -> Self {
        Self { source, revision }
    }

    #[must_use]
    pub const fn source(self) -> [u8; 16] {
        self.source
    }

    #[must_use]
    pub const fn revision(self) -> [u8; 16] {
        self.revision
    }
}

pub(crate) fn local_source_stamp(
    path: &Path,
    index_path: &Path,
    format: AlignmentSourceFormat,
    dictionary: &ReferenceDictionary,
    header: &AlignmentHeaderMetadata,
) -> io::Result<AlignmentSourceStamp> {
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let canonical_index = fs::canonicalize(index_path).unwrap_or_else(|_| index_path.to_owned());
    let source_metadata = fs::metadata(path)?;
    let index_metadata = fs::metadata(index_path)?;
    let index_digest = digest_file(index_path)?;

    let mut source = FingerprintBuilder::new(b"rgv-alignment-source-v1");
    source.field(canonical_path.to_string_lossy().as_bytes());

    let mut revision = FingerprintBuilder::new(b"rgv-alignment-revision-v1");
    write_semantic_descriptor(
        &mut revision,
        format,
        source_metadata.len(),
        dictionary,
        header,
        &index_digest,
    );
    revision.field(canonical_index.to_string_lossy().as_bytes());
    write_metadata_revision(&mut revision, &source_metadata);
    write_metadata_revision(&mut revision, &index_metadata);
    Ok(AlignmentSourceStamp::new(
        source.finish(),
        revision.finish(),
    ))
}

pub(crate) fn remote_source_stamp(
    url: &str,
    version: &crate::remote::RemoteObjectVersion,
    format: AlignmentSourceFormat,
    dictionary: &ReferenceDictionary,
    header: &AlignmentHeaderMetadata,
    index_digest: &[u8; 32],
) -> AlignmentSourceStamp {
    let mut source = FingerprintBuilder::new(b"rgv-alignment-source-v1");
    source.field(url.as_bytes());

    let mut revision = FingerprintBuilder::new(b"rgv-alignment-revision-v1");
    write_semantic_descriptor(
        &mut revision,
        format,
        version.length,
        dictionary,
        header,
        index_digest,
    );
    revision.field(version.etag.as_deref().unwrap_or_default());
    revision.field(version.last_modified.as_deref().unwrap_or_default());
    AlignmentSourceStamp::new(source.finish(), revision.finish())
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn digest_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut DigestWriter(&mut digest))?;
    Ok(digest.finalize().into())
}

struct DigestWriter<'a>(&'a mut Sha256);

impl io::Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_semantic_descriptor(
    revision: &mut FingerprintBuilder,
    format: AlignmentSourceFormat,
    source_length: u64,
    dictionary: &ReferenceDictionary,
    header: &AlignmentHeaderMetadata,
    index_digest: &[u8; 32],
) {
    revision.field(&[match format {
        AlignmentSourceFormat::Bam => 1,
        AlignmentSourceFormat::Cram => 2,
    }]);
    revision.field(&source_length.to_le_bytes());
    revision.field(index_digest);
    for contig in dictionary.iter() {
        revision.field(contig.name().as_bytes());
        revision.field(&contig.length().to_le_bytes());
    }
    revision.field(&header.read_groups_omitted.to_le_bytes());
    for read_group in &header.read_groups {
        revision.field(&read_group.id);
        revision.field(
            read_group
                .platform
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        revision.field(
            &read_group
                .maximum_homopolymer_length
                .unwrap_or_default()
                .to_le_bytes(),
        );
        revision.field(&[match read_group.ultima_tag_orientation {
            None => 0,
            Some(UltimaTagOrientation::Reference) => 1,
            Some(UltimaTagOrientation::Synthesis) => 2,
        }]);
    }
}

fn write_metadata_revision(builder: &mut FingerprintBuilder, metadata: &fs::Metadata) {
    builder.field(&metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok());
    builder.field(&modified.map_or(0, |value| value.as_secs()).to_le_bytes());
    builder.field(
        &modified
            .map_or(0, |value| value.subsec_nanos())
            .to_le_bytes(),
    );
}

struct FingerprintBuilder(Sha256);

impl FingerprintBuilder {
    fn new(domain: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update((domain.len() as u64).to_le_bytes());
        digest.update(domain);
        Self(digest)
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    fn finish(self) -> [u8; 16] {
        let digest = self.0.finalize();
        let mut result = [0; 16];
        result.copy_from_slice(&digest[..16]);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_length_delimited() {
        let mut left = FingerprintBuilder::new(b"test");
        left.field(b"a");
        left.field(b"bc");
        let mut right = FingerprintBuilder::new(b"test");
        right.field(b"ab");
        right.field(b"c");
        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn local_index_change_creates_a_new_revision_without_rekeying_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("reads.bam");
        let index_path = directory.path().join("reads.bam.bai");
        fs::write(&source_path, b"source-bytes").unwrap();
        fs::write(&index_path, b"index-one").unwrap();
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 100)]);
        let first = local_source_stamp(
            &source_path,
            &index_path,
            AlignmentSourceFormat::Bam,
            &dictionary,
            &AlignmentHeaderMetadata::default(),
        )
        .unwrap();

        fs::write(&index_path, b"index-two").unwrap();
        let second = local_source_stamp(
            &source_path,
            &index_path,
            AlignmentSourceFormat::Bam,
            &dictionary,
            &AlignmentHeaderMetadata::default(),
        )
        .unwrap();

        assert_eq!(first.source(), second.source());
        assert_ne!(first.revision(), second.revision());
    }
}
