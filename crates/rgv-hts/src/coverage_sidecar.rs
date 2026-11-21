//! Persistent sidecar cache of coverage-pyramid tiles.
//!
//! Computing whole-chromosome coverage means decoding every record once. The tiles are small,
//! so they are kept in the user's cache directory and a reopened file serves them without
//! touching the alignment data.
//!
//! # Location
//!
//! Never beside the data file and never in the working directory:
//! [`default_coverage_cache_directory`] names a per-user cache directory, and
//! [`CoverageSidecarCache::open`] refuses a relative path.
//!
//! # Layout
//!
//! ```text
//! <root>/<key>/c<contig>-l<level>-t<index>.rgvcov
//! ```
//!
//! `<key>` is the hex of a SHA-256 over the source's [`AlignmentSourceStamp`] (object identity
//! and exact revision), the canonical encoding of the [`CoverageEligibility`] settings, the
//! pyramid parameters and the format version. A changed file, index, filter or geometry
//! therefore selects another directory; stale entries are never read and age out.
//!
//! Each tile file is
//!
//! ```text
//! magic "RGVCOV\0\1" | version u32 | key [32] | contig u32 | level u32 | index u64
//! | contig_length u64 | bin_count u32 | bins (sum u64, min u32, max u32)* | sha256 [32]
//! ```
//!
//! little-endian, the checksum covering every preceding byte. A file is written to a temporary
//! name in the same directory and renamed into place, so a reader sees a whole tile or none.
//! Anything that fails validation (truncation, a flipped bit, a foreign key, inconsistent
//! statistics) is deleted and reported as absent; the caller rebuilds it.
//!
//! # Size bound
//!
//! The cache holds at most its configured number of bytes. Reading a tile marks it used; when
//! the bound is crossed the least recently used tiles are deleted first.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use rgv_model::{
    AlignmentSourceStamp, CoverageBin, CoverageEligibility, CoveragePyramidParameters,
    CoverageTile, CoverageTileId,
};
use sha2::{Digest, Sha256};

/// Default bound of the whole cache: 256 MiB, about sixteen thousand tiles.
pub const COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES: u64 = 256 * 1024 * 1024;

const MAGIC: [u8; 8] = *b"RGVCOV\0\x01";
const FORMAT_VERSION: u32 = 1;
const HEADER_BYTES: usize = 8 + 4 + 32 + 4 + 4 + 8 + 8 + 4;
const CHECKSUM_BYTES: usize = 32;
const TILE_EXTENSION: &str = "rgvcov";
/// Writes between two full recounts of the directory; in between the size is tracked.
const WRITES_PER_RECOUNT: u32 = 256;

/// Identity of one pyramid in the cache.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CoverageSidecarKey {
    digest: [u8; 32],
}

impl CoverageSidecarKey {
    #[must_use]
    pub fn new(
        stamp: AlignmentSourceStamp,
        eligibility: &CoverageEligibility,
        parameters: CoveragePyramidParameters,
    ) -> Self {
        let mut digest = Sha256::new();
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(b"rgv-coverage-sidecar");
        field(&FORMAT_VERSION.to_le_bytes());
        field(&stamp.source());
        field(&stamp.revision());
        field(&eligibility.canonical_bytes());
        field(&parameters.canonical_bytes());
        Self {
            digest: digest.finalize().into(),
        }
    }

    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }

    fn directory_name(self) -> String {
        use std::fmt::Write as _;
        self.digest[..16]
            .iter()
            .fold(String::with_capacity(32), |mut name, byte| {
                let _ = write!(name, "{byte:02x}");
                name
            })
    }
}

/// Counters of one cache handle, for telemetry and tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoverageSidecarStats {
    pub hits: u64,
    pub misses: u64,
    /// Entries that failed validation and were deleted.
    pub rejected: u64,
    pub stored: u64,
    pub evicted: u64,
    /// Bytes on disk as of the last write or recount.
    pub bytes: u64,
}

#[derive(Debug, Default)]
struct State {
    stats: CoverageSidecarStats,
    writes_since_recount: u32,
    counted: bool,
}

/// A size-bounded directory of coverage tiles shared by every source of the process.
#[derive(Debug)]
pub struct CoverageSidecarCache {
    root: PathBuf,
    maximum_bytes: u64,
    state: Mutex<State>,
}

/// The per-user coverage cache directory, or `None` when the platform names no home.
///
/// `RGV_COVERAGE_CACHE_DIR` overrides it (an absolute path). Otherwise: `~/Library/Caches/RGV/
/// coverage` on macOS, `%LOCALAPPDATA%\RGV\Cache\coverage` on Windows, and
/// `$XDG_CACHE_HOME/rgv/coverage` or `~/.cache/rgv/coverage` elsewhere. It never falls back to
/// the working directory or to the directory of a data file.
#[must_use]
pub fn default_coverage_cache_directory() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(directory) = absolute("RGV_COVERAGE_CACHE_DIR") {
        return Some(directory);
    }
    #[cfg(target_os = "macos")]
    {
        absolute("HOME").map(|home| {
            home.join("Library")
                .join("Caches")
                .join("RGV")
                .join("coverage")
        })
    }
    #[cfg(target_os = "windows")]
    {
        absolute("LOCALAPPDATA").map(|local| local.join("RGV").join("Cache").join("coverage"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        absolute("XDG_CACHE_HOME")
            .map(|cache| cache.join("rgv").join("coverage"))
            .or_else(|| {
                absolute("HOME").map(|home| home.join(".cache").join("rgv").join("coverage"))
            })
    }
}

impl CoverageSidecarCache {
    /// Opens (creating it if needed) the cache rooted at the absolute directory `root`.
    pub fn open(root: impl Into<PathBuf>, maximum_bytes: u64) -> io::Result<Self> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the coverage cache directory must be an absolute path",
            ));
        }
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            maximum_bytes,
            state: Mutex::new(State::default()),
        })
    }

    /// Opens the cache at [`default_coverage_cache_directory`] with the default bound. `None`
    /// when there is no such directory or it cannot be created; coverage then is not cached.
    #[must_use]
    pub fn open_default() -> Option<Self> {
        Self::open(
            default_coverage_cache_directory()?,
            COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES,
        )
        .ok()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn maximum_bytes(&self) -> u64 {
        self.maximum_bytes
    }

    #[must_use]
    pub fn stats(&self) -> CoverageSidecarStats {
        self.state
            .lock()
            .map_or_else(|_| CoverageSidecarStats::default(), |state| state.stats)
    }

    fn tile_path(&self, key: CoverageSidecarKey, id: CoverageTileId) -> PathBuf {
        self.root.join(key.directory_name()).join(format!(
            "c{}-l{}-t{}.{TILE_EXTENSION}",
            id.contig.get(),
            id.level,
            id.index
        ))
    }

    /// Reads one tile. Absent, unreadable, foreign or corrupt entries are `None`; an entry that
    /// exists but fails validation is deleted so it is rebuilt.
    #[must_use]
    pub fn load(
        &self,
        key: CoverageSidecarKey,
        id: CoverageTileId,
        parameters: CoveragePyramidParameters,
        contig_length: u64,
    ) -> Option<CoverageTile> {
        let path = self.tile_path(key, id);
        let Ok(bytes) = fs::read(&path) else {
            self.note(|stats| stats.misses += 1);
            return None;
        };
        let Some(tile) = decode_tile(&bytes, key, id, parameters, contig_length) else {
            let _ = fs::remove_file(&path);
            self.note(|stats| {
                stats.rejected += 1;
                stats.misses += 1;
                stats.bytes = stats.bytes.saturating_sub(bytes.len() as u64);
            });
            return None;
        };
        // Mark the entry used for least-recently-used cleanup. Failure only ages it sooner.
        if let Ok(file) = fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        self.note(|stats| stats.hits += 1);
        Some(tile)
    }

    /// Writes one tile atomically, then enforces the size bound.
    pub fn store(&self, key: CoverageSidecarKey, tile: &CoverageTile) -> io::Result<()> {
        let path = self.tile_path(key, tile.id());
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("coverage tile path has no parent"))?;
        fs::create_dir_all(directory)?;
        let bytes = encode_tile(key, tile);
        let replaced = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let mut temporary = tempfile::Builder::new()
            .prefix(".tile-")
            .suffix(".tmp")
            .tempfile_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_data()?;
        temporary.persist(&path).map_err(|error| error.error)?;

        let recount = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("coverage cache state is poisoned"))?;
            state.stats.stored += 1;
            state.stats.bytes = state
                .stats
                .bytes
                .saturating_sub(replaced)
                .saturating_add(bytes.len() as u64);
            state.writes_since_recount = state.writes_since_recount.saturating_add(1);
            !state.counted
                || state.writes_since_recount >= WRITES_PER_RECOUNT
                || state.stats.bytes > self.maximum_bytes
        };
        if recount {
            self.enforce_limit()?;
        }
        Ok(())
    }

    /// Recounts the directory and deletes least recently used tiles until it fits the bound.
    /// Leftover temporary files of an interrupted write are removed as well.
    pub fn enforce_limit(&self) -> io::Result<CoverageSidecarStats> {
        let mut entries = Vec::<(SystemTime, u64, PathBuf)>::new();
        let mut total = 0_u64;
        for key_directory in fs::read_dir(&self.root)? {
            let key_directory = key_directory?;
            // Only this cache's own pyramid directories are counted and evicted from; a link
            // or a stranger's directory under the root is not entered.
            if !is_key_directory(&key_directory) {
                continue;
            }
            let key_directory = key_directory.path();
            let Ok(files) = fs::read_dir(&key_directory) else {
                continue;
            };
            for file in files {
                let Ok(file) = file else { continue };
                let path = file.path();
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if path.extension().and_then(|extension| extension.to_str()) != Some(TILE_EXTENSION)
                {
                    // A temporary file an interrupted writer left behind. One still being
                    // written is younger than this.
                    let stale = modified
                        .elapsed()
                        .is_ok_and(|age| age > std::time::Duration::from_secs(3_600));
                    if stale {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                total = total.saturating_add(metadata.len());
                entries.push((modified, metadata.len(), path));
            }
        }
        let mut evicted = 0_u64;
        if total > self.maximum_bytes {
            entries.sort();
            for (_, length, path) in entries {
                if total <= self.maximum_bytes {
                    break;
                }
                if fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(length);
                    evicted += 1;
                    if let Some(directory) = path.parent() {
                        // Succeeds only once the pyramid's last tile is gone.
                        let _ = fs::remove_dir(directory);
                    }
                }
            }
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("coverage cache state is poisoned"))?;
        state.stats.bytes = total;
        state.stats.evicted = state.stats.evicted.saturating_add(evicted);
        state.writes_since_recount = 0;
        state.counted = true;
        Ok(state.stats)
    }

    /// Deletes every cached tile and every emptied pyramid directory under the cache root,
    /// returning the bytes freed. The cache stays usable; tiles are rebuilt on demand.
    ///
    /// Only what this cache writes is touched: regular files named `*.rgvcov` (and the
    /// temporary files of its own interrupted writes) directly inside a directory named like
    /// a pyramid key (32 hexadecimal digits) directly under the root. Nothing is followed:
    /// a symbolic link under the root, whatever it points to, is neither entered nor deleted,
    /// and neither is any other file or directory someone put there. A file that cannot be
    /// removed is left and not counted.
    pub fn clear(&self) -> io::Result<u64> {
        let mut freed = 0_u64;
        for entry in fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            let directory = entry.path();
            if !is_key_directory(&entry) {
                continue;
            }
            let Ok(files) = fs::read_dir(&directory) else {
                continue;
            };
            for file in files {
                let Ok(file) = file else { continue };
                let path = file.path();
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                let name = file.file_name();
                let name = name.to_string_lossy();
                let is_tile = path.extension().and_then(|extension| extension.to_str())
                    == Some(TILE_EXTENSION);
                let is_temporary = name.starts_with(".tile-") && name.ends_with(".tmp");
                if metadata.is_file() && (is_tile || is_temporary) && fs::remove_file(&path).is_ok()
                {
                    freed = freed.saturating_add(metadata.len());
                }
            }
            // Succeeds only when nothing else is left inside.
            let _ = fs::remove_dir(&directory);
        }
        // Recount rather than assume: something may have been left, or written meanwhile.
        self.enforce_limit()?;
        Ok(freed)
    }

    fn note(&self, update: impl FnOnce(&mut CoverageSidecarStats)) {
        if let Ok(mut state) = self.state.lock() {
            update(&mut state.stats);
        }
    }
}

/// Whether `entry` of the cache root is a pyramid directory this cache made: a real directory
/// (a symbolic link to one is not) named by 32 hexadecimal digits.
fn is_key_directory(entry: &fs::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir())
}

fn encode_tile(key: CoverageSidecarKey, tile: &CoverageTile) -> Vec<u8> {
    let id = tile.id();
    let mut bytes = Vec::with_capacity(
        HEADER_BYTES + tile.bins().len() * CoverageBin::ENCODED_BYTES + CHECKSUM_BYTES,
    );
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&key.digest);
    bytes.extend_from_slice(&id.contig.get().to_le_bytes());
    bytes.extend_from_slice(&u32::from(id.level).to_le_bytes());
    bytes.extend_from_slice(&id.index.to_le_bytes());
    bytes.extend_from_slice(&tile.contig_length().to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(tile.bins().len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for bin in tile.bins() {
        bytes.extend_from_slice(&bin.depth_sum.to_le_bytes());
        bytes.extend_from_slice(&bin.minimum_depth.to_le_bytes());
        bytes.extend_from_slice(&bin.maximum_depth.to_le_bytes());
    }
    let checksum: [u8; CHECKSUM_BYTES] = Sha256::digest(&bytes).into();
    bytes.extend_from_slice(&checksum);
    bytes
}

fn decode_tile(
    bytes: &[u8],
    key: CoverageSidecarKey,
    id: CoverageTileId,
    parameters: CoveragePyramidParameters,
    contig_length: u64,
) -> Option<CoverageTile> {
    let body_end = bytes.len().checked_sub(CHECKSUM_BYTES)?;
    if body_end < HEADER_BYTES {
        return None;
    }
    let (body, checksum) = bytes.split_at(body_end);
    let expected: [u8; CHECKSUM_BYTES] = Sha256::digest(body).into();
    if checksum != expected {
        return None;
    }
    let mut cursor = Cursor(body);
    if cursor.take::<8>()? != MAGIC
        || u32::from_le_bytes(cursor.take()?) != FORMAT_VERSION
        || cursor.take::<32>()? != key.digest
        || u32::from_le_bytes(cursor.take()?) != id.contig.get()
        || u32::from_le_bytes(cursor.take()?) != u32::from(id.level)
        || u64::from_le_bytes(cursor.take()?) != id.index
        || u64::from_le_bytes(cursor.take()?) != contig_length
    {
        return None;
    }
    let count = usize::try_from(u32::from_le_bytes(cursor.take()?)).ok()?;
    if cursor.0.len() != count.checked_mul(CoverageBin::ENCODED_BYTES)? {
        return None;
    }
    let mut bins = Vec::with_capacity(count);
    for _ in 0..count {
        bins.push(CoverageBin {
            depth_sum: u64::from_le_bytes(cursor.take()?),
            minimum_depth: u32::from_le_bytes(cursor.take()?),
            maximum_depth: u32::from_le_bytes(cursor.take()?),
        });
    }
    CoverageTile::from_bins(id, parameters, contig_length, bins).ok()
}

/// Fuzz entry: decodes `bytes` as a tile file, first as given (so framing and checksum
/// rejection are exercised) and then with the identity its own header names and a recomputed
/// checksum, so mutations reach the header and bin validation instead of stopping at the
/// checksum. A tile that decodes must encode back to the same bytes.
pub(crate) fn fuzz_decode_tile(bytes: &[u8]) -> bool {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let fixed_key = CoverageSidecarKey { digest: [7; 32] };
    let fixed_id = CoverageTileId::new(rgv_core::ContigId::new(0), 0, 0);
    let _ = decode_tile(bytes, fixed_key, fixed_id, parameters, 1 << 20);

    let mut cursor = Cursor(bytes);
    let header = (|| {
        cursor.take::<8>()?;
        cursor.take::<4>()?;
        let digest = cursor.take::<32>()?;
        let contig = u32::from_le_bytes(cursor.take()?);
        let level = u8::try_from(u32::from_le_bytes(cursor.take()?)).ok()?;
        let index = u64::from_le_bytes(cursor.take()?);
        let contig_length = u64::from_le_bytes(cursor.take()?);
        Some((digest, contig, level, index, contig_length))
    })();
    let Some((digest, contig, level, index, contig_length)) = header else {
        return false;
    };
    let key = CoverageSidecarKey { digest };
    let id = CoverageTileId::new(rgv_core::ContigId::new(contig), level, index);
    let mut sealed = bytes.to_vec();
    let checksum: [u8; CHECKSUM_BYTES] = Sha256::digest(&sealed).into();
    sealed.extend_from_slice(&checksum);
    let Some(tile) = decode_tile(&sealed, key, id, parameters, contig_length) else {
        return false;
    };
    assert_eq!(encode_tile(key, &tile), sealed, "a decoded tile re-encodes");
    true
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.0.split_first_chunk::<N>()?;
        self.0 = tail;
        Some(*head)
    }
}

#[cfg(test)]
mod tests;
