use std::{fs, time::Duration};

use rgv_core::ContigId;
use rgv_model::{AlignmentFilter, CoverageBin};

use super::*;

const CONTIG_LENGTH: u64 = 248_956_422;
const PARAMETERS: CoveragePyramidParameters = CoveragePyramidParameters::DEFAULT;

fn stamp(revision: u8) -> AlignmentSourceStamp {
    AlignmentSourceStamp::new([7; 16], [revision; 16])
}

fn key(revision: u8) -> CoverageSidecarKey {
    CoverageSidecarKey::new(stamp(revision), &CoverageEligibility::default(), PARAMETERS)
}

fn tile(level: u8, index: u64, seed: u32) -> CoverageTile {
    let id = CoverageTileId::new(ContigId::new(0), level, index);
    let count = PARAMETERS.tile_bin_count(id, CONTIG_LENGTH).unwrap();
    let bases = PARAMETERS.bin_bases(level).unwrap();
    let mut tile_bins = (0..count)
        .map(|bin| {
            let low = (bin as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) % 50;
            CoverageBin {
                depth_sum: u64::from(low + 1) * bases,
                minimum_depth: low,
                maximum_depth: low + 9,
            }
        })
        .collect::<Vec<_>>();
    // The contig's last bin may be partial; keep it consistent.
    if let Some(last) = tile_bins.last_mut() {
        *last = CoverageBin {
            depth_sum: 0,
            minimum_depth: 0,
            maximum_depth: 0,
        };
    }
    CoverageTile::from_bins(id, PARAMETERS, CONTIG_LENGTH, tile_bins).unwrap()
}

fn tile_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for directory in fs::read_dir(root).unwrap() {
        for file in fs::read_dir(directory.unwrap().path()).unwrap() {
            files.push(file.unwrap().path());
        }
    }
    files.sort();
    files
}

#[test]
fn tiles_round_trip_through_a_reopened_cache() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("coverage");
    let tiles = [
        tile(0, 0, 1),
        tile(0, 15_195, 2),
        tile(4, 3, 3),
        tile(7, 0, 4),
    ];
    {
        let cache = CoverageSidecarCache::open(&root, 1 << 30).unwrap();
        for tile in &tiles {
            cache.store(key(1), tile).unwrap();
        }
        assert_eq!(cache.stats().stored, 4);
    }
    // A new handle, as after restarting the application: nothing is decoded again.
    let cache = CoverageSidecarCache::open(&root, 1 << 30).unwrap();
    for tile in &tiles {
        assert_eq!(
            cache
                .load(key(1), tile.id(), PARAMETERS, CONTIG_LENGTH)
                .as_ref(),
            Some(tile)
        );
    }
    assert_eq!(cache.stats().hits, 4);
    // The contig's last level-0 tile is short; it round-trips with its own bin count.
    assert!(tiles[1].bins().len() < PARAMETERS.tile_bins() as usize);
    // The root tile holds the whole chromosome.
    assert_eq!(tiles[3].span(), (0, CONTIG_LENGTH));
    // An absent tile is a miss, not an error.
    let absent = CoverageTileId::new(ContigId::new(0), 0, 9);
    assert!(
        cache
            .load(key(1), absent, PARAMETERS, CONTIG_LENGTH)
            .is_none()
    );
    assert_eq!(cache.stats().misses, 1);
    // Every file lives under the cache root in a per-pyramid directory.
    let files = tile_files(&root);
    assert_eq!(files.len(), 4);
    assert!(
        files
            .iter()
            .all(|file| file.starts_with(&root) && file.extension().unwrap() == TILE_EXTENSION)
    );
}

#[test]
fn a_different_stamp_filter_or_geometry_never_sees_another_pyramids_tiles() {
    let directory = tempfile::tempdir().unwrap();
    let cache = CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap();
    let stored = tile(2, 5, 9);
    cache.store(key(1), &stored).unwrap();
    let load = |key| cache.load(key, stored.id(), PARAMETERS, CONTIG_LENGTH);
    assert_eq!(load(key(1)), Some(stored.clone()));

    // The file or its index changed: another revision.
    assert_ne!(key(1), key(2));
    assert!(load(key(2)).is_none());
    // Another object entirely.
    let other_source = CoverageSidecarKey::new(
        AlignmentSourceStamp::new([8; 16], [1; 16]),
        &CoverageEligibility::default(),
        PARAMETERS,
    );
    assert!(load(other_source).is_none());
    // Each filter setting is its own pyramid.
    let mut keys = vec![key(1), other_source, key(2)];
    for eligibility in [
        CoverageEligibility {
            include_secondary: true,
            ..CoverageEligibility::default()
        },
        CoverageEligibility {
            include_supplementary: false,
            ..CoverageEligibility::default()
        },
        CoverageEligibility {
            overlapping_mate_policy: rgv_core::OverlappingMatePolicy::CountBoth,
            ..CoverageEligibility::default()
        },
        CoverageEligibility {
            filter: AlignmentFilter {
                excluded_flags: 0x400,
                ..AlignmentFilter::default()
            },
            ..CoverageEligibility::default()
        },
        CoverageEligibility {
            filter: AlignmentFilter {
                minimum_mapping_quality: Some(20),
                ..AlignmentFilter::default()
            },
            ..CoverageEligibility::default()
        },
    ] {
        let filtered = CoverageSidecarKey::new(stamp(1), &eligibility, PARAMETERS);
        assert!(load(filtered).is_none(), "{eligibility:?}");
        keys.push(filtered);
    }
    // Other pyramid geometry.
    let geometry = CoveragePyramidParameters::new(32, 4, 1_024).unwrap();
    keys.push(CoverageSidecarKey::new(
        stamp(1),
        &CoverageEligibility::default(),
        geometry,
    ));
    let distinct = keys
        .iter()
        .map(|key| key.digest())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(distinct.len(), keys.len());

    // A file planted under the right name but written for another key is rejected by the key
    // recorded inside it, and removed.
    let planted = cache.tile_path(key(2), stored.id());
    fs::create_dir_all(planted.parent().unwrap()).unwrap();
    fs::copy(cache.tile_path(key(1), stored.id()), &planted).unwrap();
    assert!(load(key(2)).is_none());
    assert!(!planted.exists());
    assert_eq!(cache.stats().rejected, 1);
    // A tile recorded for another contig length does not load either.
    assert!(
        cache
            .load(key(1), stored.id(), PARAMETERS, CONTIG_LENGTH - 1)
            .is_none()
    );
}

#[test]
fn corrupt_entries_are_ignored_removed_and_rebuilt() {
    let directory = tempfile::tempdir().unwrap();
    let cache = CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap();
    let stored = tile(1, 2, 5);
    let path = cache.tile_path(key(1), stored.id());
    let pristine = encode_tile(key(1), &stored);
    let load = || cache.load(key(1), stored.id(), PARAMETERS, CONTIG_LENGTH);

    let mut damaged = Vec::new();
    // Truncations, including an empty file and a missing checksum.
    for length in [
        0,
        7,
        HEADER_BYTES - 1,
        HEADER_BYTES,
        pristine.len() - 33,
        pristine.len() - 1,
    ] {
        damaged.push(pristine[..length].to_vec());
    }
    // One flipped bit in the magic, the version, the key, the id, a bin and the checksum.
    for offset in [0, 9, 20, 46, 52, HEADER_BYTES + 100, pristine.len() - 1] {
        let mut bytes = pristine.clone();
        bytes[offset] ^= 0x10;
        damaged.push(bytes);
    }
    // Trailing garbage, and garbage alone.
    damaged.push([pristine.clone(), vec![0; 16]].concat());
    damaged.push(vec![0xa5; 4_096]);
    // A checksum-valid file whose statistics are impossible (minimum above maximum).
    let mut inconsistent = pristine[..pristine.len() - CHECKSUM_BYTES].to_vec();
    inconsistent[HEADER_BYTES + 8..HEADER_BYTES + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    let checksum: [u8; 32] = Sha256::digest(&inconsistent).into();
    inconsistent.extend_from_slice(&checksum);
    damaged.push(inconsistent);

    let cases = damaged.len() as u64;
    for bytes in damaged {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            load().is_none(),
            "{} corrupt bytes were accepted",
            bytes.len()
        );
        assert!(!path.exists(), "the corrupt entry was left in place");
        // Rebuilt and stored again, it loads.
        cache.store(key(1), &stored).unwrap();
        assert_eq!(load(), Some(stored.clone()));
    }
    assert_eq!(cache.stats().rejected, cases);
}

#[test]
fn a_write_is_atomic_and_leaves_no_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let cache = CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap();
    let first = tile(3, 1, 1);
    let second = tile(3, 1, 2);
    assert_ne!(first, second);
    cache.store(key(1), &first).unwrap();
    let path = cache.tile_path(key(1), first.id());
    let pyramid = path.parent().unwrap().to_owned();

    // A writer that died mid-write left a partial temporary file beside the tile. Readers
    // never open it, and the entry in place is still the whole earlier tile.
    let partial = encode_tile(key(1), &second);
    let abandoned = pyramid.join(".tile-abandoned.tmp");
    fs::write(&abandoned, &partial[..partial.len() / 2]).unwrap();
    assert_eq!(
        cache.load(key(1), first.id(), PARAMETERS, CONTIG_LENGTH),
        Some(first.clone())
    );

    // Replacing the tile swaps it in whole: at no point is a mixture visible, and the only
    // files are the tile and the abandoned temporary.
    cache.store(key(1), &second).unwrap();
    assert_eq!(fs::read(&path).unwrap(), partial);
    let mut names = fs::read_dir(&pyramid)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, [".tile-abandoned.tmp", "c0-l3-t1.rgvcov"]);

    // Cleanup removes an abandoned temporary once it is old, never a tile.
    let old = SystemTime::now() - Duration::from_secs(7_200);
    fs::File::options()
        .write(true)
        .open(&abandoned)
        .unwrap()
        .set_modified(old)
        .unwrap();
    cache.enforce_limit().unwrap();
    assert!(!abandoned.exists());
    assert_eq!(
        cache.load(key(1), second.id(), PARAMETERS, CONTIG_LENGTH),
        Some(second)
    );

    // A cache whose directory vanished refuses the write without a partial file.
    let gone = CoverageSidecarCache::open(directory.path().join("gone"), 1 << 30).unwrap();
    fs::remove_dir_all(gone.root()).unwrap();
    fs::write(gone.root(), b"not a directory").unwrap();
    assert!(gone.store(key(1), &first).is_err());
    assert!(
        gone.load(key(1), first.id(), PARAMETERS, CONTIG_LENGTH)
            .is_none()
    );
}

#[test]
fn the_cache_stays_within_its_size_bound_and_evicts_least_recently_used_tiles() {
    let directory = tempfile::tempdir().unwrap();
    let tile_bytes = encode_tile(key(1), &tile(0, 0, 0)).len() as u64;
    // Room for five full tiles.
    let bound = tile_bytes * 5 + tile_bytes / 2;
    let cache = CoverageSidecarCache::open(directory.path(), bound).unwrap();
    let on_disk = || {
        tile_files(directory.path())
            .iter()
            .map(|file| fs::metadata(file).unwrap().len())
            .sum::<u64>()
    };
    let age = |cache: &CoverageSidecarCache, key, index: u64, seconds: u64| {
        let path = cache.tile_path(key, CoverageTileId::new(ContigId::new(0), 0, index));
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    };
    // Five tiles of an older pyramid, written long ago in index order.
    for index in 0..5 {
        cache.store(key(1), &tile(0, index, 1)).unwrap();
        age(&cache, key(1), index, 10_000 - index * 100);
    }
    assert_eq!(on_disk(), tile_bytes * 5);
    // Tile 0 is the oldest write but was read just now.
    assert!(
        cache
            .load(key(1), tile(0, 0, 1).id(), PARAMETERS, CONTIG_LENGTH)
            .is_some()
    );

    // Three tiles of a new pyramid push the cache over its bound after every write.
    for index in 0..3 {
        cache.store(key(2), &tile(0, index, 2)).unwrap();
        assert!(on_disk() <= bound, "{} > {bound}", on_disk());
    }
    assert_eq!(cache.stats().bytes, on_disk());
    assert_eq!(cache.stats().evicted, 3);
    let present = |key, index| {
        cache
            .tile_path(key, CoverageTileId::new(ContigId::new(0), 0, index))
            .exists()
    };
    // Least recently used first: tiles 1, 2 and 3 of the old pyramid went; the one read
    // recently and the newest writes stayed.
    assert!(present(key(1), 0));
    assert!(!present(key(1), 1) && !present(key(1), 2) && !present(key(1), 3));
    assert!(present(key(1), 4));
    assert!((0..3).all(|index| present(key(2), index)));

    // However many tiles are written, the bound holds; emptied pyramid directories go too.
    for index in 3..40 {
        cache.store(key(2), &tile(0, index, 2)).unwrap();
        assert!(on_disk() <= bound);
    }
    assert_eq!(tile_files(directory.path()).len(), 5);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);

    // A bound smaller than one tile keeps nothing rather than exceeding itself.
    let tiny = CoverageSidecarCache::open(directory.path().join("tiny"), 100).unwrap();
    tiny.store(key(1), &tile(0, 0, 1)).unwrap();
    assert_eq!(tile_files(tiny.root()).len(), 0);
}

#[test]
fn the_cache_directory_is_absolute_and_never_the_working_directory() {
    assert!(CoverageSidecarCache::open("relative/coverage", 1 << 20).is_err());
    assert!(CoverageSidecarCache::open("", 1 << 20).is_err());
    assert!(CoverageSidecarCache::open(".", 1 << 20).is_err());
    if let Some(directory) = default_coverage_cache_directory() {
        assert!(directory.is_absolute());
        let working = std::env::current_dir().unwrap();
        assert!(
            !directory.starts_with(&working)
                || std::env::var_os("RGV_COVERAGE_CACHE_DIR").is_some()
        );
        assert!(directory.ends_with("coverage"));
    }
}

#[test]
fn clear_removes_the_caches_own_files_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = directory.path().join("coverage");
    let cache = CoverageSidecarCache::open(&root, 1 << 30).unwrap();
    for revision in 1..=3 {
        for index in 0..4 {
            cache.store(key(revision), &tile(0, index, 1)).unwrap();
        }
    }
    let tiles = tile_files(&root);
    assert_eq!(tiles.len(), 12);
    let tile_bytes = tiles
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum::<u64>();
    let key_directory = tiles[0].parent().unwrap().to_owned();
    // A leftover of an interrupted write: the cache's own, removed with the tiles.
    fs::write(key_directory.join(".tile-abc.tmp"), [0_u8; 10]).unwrap();

    // Things that are not the cache's: a stray file beside the tiles, a file and a directory
    // at the root, a tile-named file in a directory that is not a key directory, and links
    // out of the root (to a directory named like a key, and to a tile-named file).
    fs::write(key_directory.join("notes.txt"), b"mine").unwrap();
    fs::write(root.join("README"), b"mine").unwrap();
    fs::create_dir(root.join("other")).unwrap();
    fs::write(root.join("other").join("c0-l0-t0.rgvcov"), b"mine").unwrap();
    let precious = outside.path().join("precious.rgvcov");
    fs::write(&precious, b"do not delete").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path(),
            root.join("0123456789abcdef0123456789abcdef"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&precious, key_directory.join("c9-l0-t9.rgvcov")).unwrap();
    }

    let freed = cache.clear().unwrap();
    assert_eq!(freed, tile_bytes + 10);
    assert_eq!(cache.stats().bytes, 0);
    assert_eq!(fs::read(&precious).unwrap(), b"do not delete");
    assert_eq!(fs::read(key_directory.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(fs::read(root.join("README")).unwrap(), b"mine");
    assert!(root.join("other").join("c0-l0-t0.rgvcov").exists());
    #[cfg(unix)]
    assert!(fs::symlink_metadata(key_directory.join("c9-l0-t9.rgvcov")).is_ok());
    // The two emptied pyramid directories are gone; the one with a stranger's file stays.
    let remaining = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|entry| fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()))
        .count();
    assert_eq!(remaining, 2);

    // The cache still works, and clearing an empty cache frees nothing.
    assert!(
        cache
            .load(
                key(1),
                tile(0, 0, 1).id(),
                CoveragePyramidParameters::DEFAULT,
                1
            )
            .is_none()
    );
    cache.store(key(1), &tile(0, 0, 1)).unwrap();
    assert!(cache.clear().unwrap() > 0);
    assert_eq!(cache.clear().unwrap(), 0);
}
