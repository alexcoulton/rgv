//! End to end with no window and no GPU device: a session over `benchmark-data/mixed.bam` and
//! small BED and VCF fixtures settles through the headless presenter, presents reads, coverage
//! and feature names, and the `rgv-snapshot` binary writes a PNG and an SVG of it.
//!
//! `benchmark-data/mixed.bam` is a local benchmark file; like the other tests that read it, these
//! skip when it is absent.

use rgv_core::{GenomicInterval, PanelId};
use rgv_engine::TrackSnapshotData;
use rgv_snapshot::{SnapshotRequest, open_host, render_display_list};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const LOCUS: &str = "chr1:900001-920000";
const WIDTH: u32 = 1200;
const HEIGHT: u32 = 800;

fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn mixed_bam() -> Option<PathBuf> {
    let path = manifest_path("../../benchmark-data/mixed.bam");
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} is absent", path.display());
        None
    }
}

fn request(bam: PathBuf) -> SnapshotRequest {
    SnapshotRequest {
        session_file: None,
        reference: None,
        tracks: vec![
            bam,
            manifest_path("tests/fixtures/features.bed"),
            manifest_path("tests/fixtures/variants.vcf"),
        ],
        locus: Some(LOCUS.to_owned()),
        width: WIDTH,
        height: HEIGHT,
    }
}

#[test]
fn a_headless_session_settles_and_presents_reads_coverage_and_features() {
    let Some(bam) = mixed_bam() else {
        return;
    };
    let mut host = open_host(&request(bam)).unwrap();
    host.settle(Duration::from_secs(120)).unwrap();

    let presentation = host.presentation(PanelId::new(1)).unwrap();
    assert_eq!(presentation.contig_name, "chr1");
    let interval = presentation.viewport.interval();
    assert_eq!((interval.start(), interval.end()), (900_000, 920_000));
    assert!(!presentation.pending.any(), "{:?}", presentation.pending);

    // Reads: the alignment lane committed a complete, current scene with records in it.
    assert_eq!(presentation.alignment.len(), 1);
    let lane = &presentation.alignment[0];
    assert!(lane.settled, "{:?}", lane.phase);
    assert!(lane.visible_complete && lane.scene_contracts_current);
    assert!(!lane.reads_refused && lane.unavailable.is_none());
    let scene = lane.scene.as_ref().expect("a committed scene");
    let records = scene.reads().evidence().record_count();
    assert!(records > 1_000, "{records} records");
    assert_eq!(host.presented_frames(PanelId::new(1)).len(), 1);

    // Coverage at known positions, from the scene's exact depth summaries.
    for position in [905_000, 915_000] {
        let site = GenomicInterval::new(interval.contig(), position, position + 1).unwrap();
        let depth = scene.summaries().maximum_depth(site).unwrap();
        assert!(depth > 0, "no coverage at {position}");
        assert!(
            usize::try_from(depth).unwrap() <= records,
            "depth {depth} at {position} exceeds the {records} records"
        );
    }

    // Feature names from the BED and VCF lanes.
    let names = |track: &str| -> Vec<String> {
        let lane = presentation
            .tracks
            .iter()
            .find(|lane| lane.name == track)
            .unwrap_or_else(|| panic!("no {track} lane"));
        match lane.snapshot.as_ref().map(|snapshot| &snapshot.data) {
            Some(TrackSnapshotData::Features(features)) => features
                .iter()
                .filter_map(|feature| feature.name.clone())
                .collect(),
            Some(TrackSnapshotData::Variants(query)) => query
                .records
                .iter()
                .filter_map(|record| record.id.clone())
                .collect(),
            other => panic!("unexpected {track} answer {other:?}"),
        }
    };
    assert_eq!(
        names("features.bed"),
        ["SNAPSHOT_FEATURE", "SECOND_FEATURE"]
    );
    assert_eq!(names("variants.vcf"), ["SNAPSHOT_SNV", "SECOND_SNV"]);

    // The drawn view: read rectangles fill the reads pane, labels name the features.
    let display = render_display_list(&host).unwrap();
    let layout = presentation.lanes.as_ref().unwrap();
    let reads = layout.reads.pane_rect;
    let read_rectangles = display
        .rectangles
        .iter()
        .filter(|rectangle| {
            rectangle.y >= reads.top
                && rectangle.y < reads.bottom
                && rectangle.x >= layout.canvas.left
        })
        .count();
    assert!(read_rectangles > 100, "{read_rectangles} read rectangles");
    for label in ["SNAPSHOT_FEATURE", "SECOND_FEATURE", "SNAPSHOT_SNV", LOCUS] {
        assert!(
            display.texts.iter().any(|text| text.text == label),
            "missing label {label}"
        );
    }
}

#[test]
fn the_binary_writes_a_png_and_an_svg_without_a_window() {
    let Some(bam) = mixed_bam() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    for name in ["view.png", "view.svg"] {
        let out = directory.path().join(name);
        let output = Command::new(env!("CARGO_BIN_EXE_rgv-snapshot"))
            .arg("--track")
            .arg(&bam)
            .arg("--track")
            .arg(manifest_path("tests/fixtures/features.bed"))
            .arg("--track")
            .arg(manifest_path("tests/fixtures/variants.vcf"))
            .args(["--locus", LOCUS, "--width", "1200", "--height", "800"])
            .args(["--timeout", "120", "--out"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            out.metadata().unwrap().len() > 10_000,
            "{name} is too small"
        );
    }

    let image = image::open(directory.path().join("view.png"))
        .unwrap()
        .to_rgba8();
    assert_eq!(image.dimensions(), (WIDTH, HEIGHT));
    let drawn = image
        .pixels()
        .filter(|pixel| pixel.0[..3] != [255, 255, 255])
        .count();
    let total = (WIDTH * HEIGHT) as usize;
    assert!(drawn * 10 > total, "only {drawn} of {total} pixels drawn");
    // The coverage lane is filled under 905,000 (the ruler is 24 points, the lane header 22,
    // the coverage sublane 60): x = gutter + (905,000 - 900,000) / bases per point.
    let bases_per_point = 20_000.0 / f64::from(WIDTH - 160);
    let x = 160 + (5_000.0 / bases_per_point) as u32;
    assert_ne!(image.get_pixel(x, 100).0[..3], [255, 255, 255]);

    let svg = std::fs::read_to_string(directory.path().join("view.svg")).unwrap();
    assert!(svg.starts_with("<?xml") && svg.trim_end().ends_with("</svg>"));
    assert!(svg.matches("<rect").count() > 100);
    assert!(svg.contains(">SNAPSHOT_FEATURE<") && svg.contains(">SNAPSHOT_SNV<"));
}

#[test]
fn an_unsettled_view_fails_with_what_it_was_waiting_for() {
    let Some(bam) = mixed_bam() else {
        return;
    };
    let mut host = open_host(&request(bam)).unwrap();
    let error = host.settle(Duration::ZERO).unwrap_err().to_string();
    assert!(error.contains("did not settle"), "{error}");
    assert!(error.contains("panel 1"), "{error}");
}

#[test]
fn a_view_needs_a_reference_or_alignments() {
    let request = SnapshotRequest {
        session_file: None,
        reference: None,
        tracks: vec![manifest_path("tests/fixtures/features.bed")],
        locus: Some(LOCUS.to_owned()),
        width: WIDTH,
        height: HEIGHT,
    };
    let error = open_host(&request).err().expect("refused").to_string();
    assert!(error.contains("nothing to draw against"), "{error}");
}
