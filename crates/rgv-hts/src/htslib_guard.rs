//! Checks made in Rust before, or right after, a `rust-htslib` call that does not check what
//! `HTSlib` returned. `rust-htslib` 1.0.1 wraps several possibly-null pointers as if they were
//! valid (`bcf::Reader`'s header, `faidx::Reader`'s index) or dereferences them at once
//! (`tbx::Reader`'s file handle), and its fields are private.

use std::{
    fmt::Debug,
    fs::File,
    io,
    io::Read as _,
    path::{Path, PathBuf},
};

/// Whether `value`, a `rust-htslib` wrapper whose derived `Debug` prints its single raw
/// pointer as `inner: 0x…`, wraps a null pointer.
///
/// A rendering that is not recognised counts as null, so a change in that output makes the
/// caller refuse the file instead of dereferencing null (and fails the tests that open valid
/// files).
pub(crate) fn wraps_null_pointer(value: &impl Debug) -> bool {
    let rendered = format!("{value:?}");
    let address = rendered
        .split_once("inner: 0x")
        .map(|(_, rest)| {
            rest.split(|character: char| !character.is_ascii_hexdigit())
                .next()
                .unwrap_or_default()
        })
        .and_then(|digits| u128::from_str_radix(digits, 16).ok());
    !matches!(address, Some(address) if address != 0)
}

/// Fails unless `path` is a regular file whose first byte can be read. `HTSlib` returns a null
/// handle for anything else (a directory, a file without read permission), which
/// `tbx::Reader` dereferences.
pub(crate) fn ensure_readable_file(path: &Path) -> io::Result<()> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    File::open(path)?.read(&mut [0_u8; 1]).map(|_| ())
}

/// Runs a `rust-htslib` call that panics on input it does not expect (`tbx::Reader` unwraps
/// the UTF-8 conversion of every comment line, sequence name and data line, and panics on an
/// iterator status it does not know) and turns such a panic into an error message.
///
/// Only for calls that leave the wrapped `HTSlib` state consistent when they panic: the
/// panics contained here are raised in Rust, after the C call has returned.
pub(crate) fn contain_panic<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned())
    })
}

/// Largest BAM header text or CSI auxiliary block skipped to reach a reference count.
const MAX_SKIPPED_HEADER_BYTES: u64 = 1 << 30;

fn read_i32(reader: &mut impl io::Read) -> io::Result<i32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

/// Skips a length-prefixed block. `None` when the block is longer than is ever legitimate.
fn skip_block(reader: &mut impl io::Read) -> io::Result<Option<()>> {
    let Ok(length) = u64::try_from(read_i32(reader)?) else {
        return Ok(None);
    };
    if length > MAX_SKIPPED_HEADER_BYTES {
        return Ok(None);
    }
    let skipped = io::copy(&mut reader.take(length), &mut io::sink())?;
    if skipped == length {
        Ok(Some(()))
    } else {
        Err(io::ErrorKind::UnexpectedEof.into())
    }
}

/// The number of reference sequences the header of the BAM at `path` declares. `None` when
/// the file is not a BAM this check understands; `HTSlib` then judges it on its own.
fn bam_header_reference_count(path: &Path) -> io::Result<Option<i32>> {
    let mut reader = io::BufReader::new(flate2::read::MultiGzDecoder::new(File::open(path)?));
    let mut magic = [0_u8; 4];
    if reader.read_exact(&mut magic).is_err() || &magic != b"BAM\x01" {
        return Ok(None);
    }
    if skip_block(&mut reader)?.is_none() {
        return Ok(None);
    }
    read_i32(&mut reader).map(Some)
}

/// The number of reference sequences the BAI or CSI index at `path` claims to describe.
/// `None` when the file is neither; `HTSlib` then rejects it on its own.
fn alignment_index_reference_count(path: &Path) -> io::Result<Option<i32>> {
    let mut file = io::BufReader::new(File::open(path)?);
    let mut magic = [0_u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return Ok(None);
    }
    if &magic == b"BAI\x01" {
        return read_i32(&mut file).map(Some);
    }
    if magic[..2] != [0x1f, 0x8b] {
        return Ok(None);
    }
    // A CSI is BGZF-compressed: magic, min_shift, depth, auxiliary block, reference count.
    let mut reader = io::BufReader::new(flate2::read::MultiGzDecoder::new(File::open(path)?));
    if reader.read_exact(&mut magic).is_err() || &magic != b"CSI\x01" {
        return Ok(None);
    }
    read_i32(&mut reader)?;
    read_i32(&mut reader)?;
    if skip_block(&mut reader)?.is_none() {
        return Ok(None);
    }
    read_i32(&mut reader).map(Some)
}

/// Where `HTSlib` looks for the index of the BAM at `path`, in its order: a CSI before a BAI,
/// `<path>.<extension>` before the path with its last extension replaced.
fn bam_index_candidates(path: &Path) -> Vec<PathBuf> {
    ["csi", "bai"]
        .into_iter()
        .flat_map(|extension| {
            let mut appended = path.as_os_str().to_owned();
            appended.push(".");
            appended.push(extension);
            [PathBuf::from(appended), path.with_extension(extension)]
        })
        .collect()
}

/// Fails when the index `HTSlib` would load for the BAM at `path` does not describe exactly
/// the reference sequences the BAM header declares.
///
/// `HTSlib` trusts the reference count at the head of an index: a `.bai` of a few bytes that
/// claims 2^31 references keeps it busy for tens of seconds before it notices that the file
/// has ended. An index is always written for all the references of its BAM, so any other
/// count means the index belongs to a different file (or to nobody) and is refused here,
/// before `HTSlib` sees it. A file this check cannot interpret is left to `HTSlib`.
pub(crate) fn ensure_bam_index_matches_header(
    path: &Path,
    index_override: Option<&Path>,
) -> io::Result<()> {
    let Some(header_references) = bam_header_reference_count(path)? else {
        return Ok(());
    };
    let index = match index_override {
        Some(index) => Some(index.to_owned()),
        None => bam_index_candidates(path)
            .into_iter()
            .find(|candidate| candidate.is_file()),
    };
    let Some(index) = index else {
        return Ok(());
    };
    let Some(index_references) = alignment_index_reference_count(&index)? else {
        return Ok(());
    };
    if index_references == header_references {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} describes {index_references} reference sequences but {} declares \
             {header_references}; the index does not belong to this alignment file",
            index.display(),
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Wrapper {
        #[allow(dead_code)]
        inner: *const u8,
    }

    #[derive(Debug)]
    struct Renamed {
        #[allow(dead_code)]
        handle: *const u8,
    }

    #[test]
    fn null_and_unrecognised_renderings_count_as_null() {
        let byte = 7_u8;
        assert!(!wraps_null_pointer(&Wrapper {
            inner: &raw const byte
        }));
        assert!(wraps_null_pointer(&Wrapper {
            inner: std::ptr::null()
        }));
        assert!(wraps_null_pointer(&Renamed {
            handle: &raw const byte
        }));
        assert!(wraps_null_pointer(&"inner: 0x"));
    }

    #[test]
    fn directories_and_missing_files_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        assert!(ensure_readable_file(directory.path()).is_err());
        assert!(ensure_readable_file(&directory.path().join("missing")).is_err());
        let file = directory.path().join("empty");
        std::fs::write(&file, b"").unwrap();
        assert!(ensure_readable_file(&file).is_ok());
    }
}
