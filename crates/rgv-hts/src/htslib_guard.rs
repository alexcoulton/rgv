//! Checks made in Rust before, or right after, a `rust-htslib` call that does not check what
//! `HTSlib` returned. `rust-htslib` 1.0.1 wraps several possibly-null pointers as if they were
//! valid (`bcf::Reader`'s header, `faidx::Reader`'s index) or dereferences them at once
//! (`tbx::Reader`'s file handle), and its fields are private.

use std::{fmt::Debug, fs::File, io, io::Read as _, path::Path};

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
