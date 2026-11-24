//! Instance culling on long-read-like scenes: the drawn ranges keep every instance that can
//! reach the clip, and their size follows the visible area rather than the pages' contents.

use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentCamera, AlignmentPageFamily, AlignmentPresentationScene, AllocationLedger,
    CpuClipRect, GeometryLod,
};
use rgv_core::{ContigId, GenomicInterval, Viewport};
use rgv_model::{AlignmentFlags, AlignmentRecord, CigarOp, CigarOpKind, SequenceData};

use super::{
    AlignmentComponent, PackedAlignmentPage, alignment_page_has_gpu_geometry, culled_draw_ranges,
    project_alignment_gpu_instance, tests::test_scene_from_records,
};

/// Pages as the desktop builds them: 32 kb tiles of 256 rows.
pub(crate) const LONG_READ_TILE_BASES: u64 = 32_768;
pub(crate) const LONG_READ_ROW_BLOCK_ROWS: u32 = 256;
pub(crate) const LONG_READ_CONTIG: u64 = 100_000;

/// Small xorshift generator, so the fixture is deterministic without a dependency.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 16
    }

    pub(crate) fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }
}

/// ONT-like records: about 30 kb of reference each, aligned in blocks of 15-45 bases separated
/// by one- to six-base insertions and deletions (about a thousand CIGAR operations per read),
/// soft clips at both ends, and a read sequence that matches the all-`A` test reference except
/// for one base in twenty.
pub(crate) fn long_read_records(seed: u64, count: usize) -> Vec<AlignmentRecord> {
    let mut rng = Rng::new(seed);
    (0..count)
        .map(|index| {
            let target_span = rng.between(24_000, 32_000);
            let start = rng.between(0, LONG_READ_CONTIG - target_span - 1);
            let op = |kind, len| CigarOp { kind, len };
            let mut cigar = vec![op(CigarOpKind::SoftClip, 40)];
            let (mut reference, mut query) = (0_u64, 40_usize);
            while reference < target_span {
                if reference > 0 {
                    let len = rng.between(1, 6) as u32;
                    if rng.next().is_multiple_of(2) {
                        cigar.push(op(CigarOpKind::Insertion, len));
                        query += len as usize;
                    } else {
                        cigar.push(op(CigarOpKind::Deletion, len));
                        reference += u64::from(len);
                    }
                }
                let len = rng.between(15, 45) as u32;
                cigar.push(op(CigarOpKind::Match, len));
                reference += u64::from(len);
                query += len as usize;
            }
            cigar.push(op(CigarOpKind::SoftClip, 40));
            query += 40;
            let bases = (0..query)
                .map(|_| {
                    if rng.next().is_multiple_of(20) {
                        b"CGT"[(rng.next() % 3) as usize]
                    } else {
                        b'A'
                    }
                })
                .collect::<Vec<_>>();
            AlignmentRecord {
                contig: ContigId::new(0),
                start,
                end: start + reference,
                flags: AlignmentFlags::from_bits(if index % 2 == 0 { 0 } else { 0x10 }),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("long-{index}").into_bytes(),
                cigar,
                sequence: SequenceData::Present {
                    qualities: Some(vec![30; bases.len()]),
                    bases,
                },
                auxiliary_fields: Vec::new(),
            }
        })
        .collect()
}

pub(crate) fn long_read_scene(
    ledger: &AllocationLedger,
    records: &[AlignmentRecord],
) -> Arc<AlignmentPresentationScene> {
    test_scene_from_records(
        ledger,
        GenomicInterval::new(ContigId::new(0), 0, LONG_READ_CONTIG).unwrap(),
        LONG_READ_CONTIG,
        NonZeroU64::new(LONG_READ_TILE_BASES).unwrap(),
        records,
        NonZeroU32::new(LONG_READ_ROW_BLOCK_ROWS).unwrap(),
        None,
        GeometryLod::Base,
    )
}

/// A desktop-sized lane: `bases` across 1,500 points, 800 points of 14-point rows.
pub(crate) fn long_read_camera(
    start: u64,
    bases: u64,
    pan_points: f64,
    row_scroll: f64,
) -> AlignmentCamera {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), start, start + bases).unwrap(),
        1_500.0,
    )
    .unwrap()
    .pan_pixels(pan_points, LONG_READ_CONTIG)
    .unwrap();
    AlignmentCamera::from_viewport(
        viewport,
        row_scroll,
        14.0,
        11.0,
        CpuClipRect {
            x: 12.3,
            y: 40.6,
            width: 1_500.0,
            height: 800.0,
        },
    )
    .unwrap()
}

/// `(drawn, unculled)` instances of the camera's visible GPU pages.
pub(crate) fn culled_instance_counts(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
) -> (usize, usize) {
    scene
        .visible_pages(camera.interval(), camera.visible_rows())
        .into_iter()
        .filter(|page| alignment_page_has_gpu_geometry(page))
        .fold((0, 0), |(drawn, all), page| {
            let instances = u32::try_from(page.drawable_primitive_count()).unwrap();
            let culled = culled_draw_ranges(page, camera, instances, true)
                .iter()
                .map(|(_, range)| (range.end - range.start) as usize)
                .sum::<usize>();
            (drawn + culled, all + instances as usize)
        })
}

/// The text anchors the label projector examined before pages carried a spatial directory:
/// every label, base cell and clip-ghost base of every visible text page.
fn unindexed_label_anchors(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
) -> usize {
    scene
        .visible_pages(camera.interval(), camera.visible_rows())
        .into_iter()
        .filter(|page| {
            matches!(
                page.key().family(),
                AlignmentPageFamily::Labels | AlignmentPageFamily::ReadGeometry
            )
        })
        .map(|page| {
            page.label_anchor_count()
                + page
                    .primitives()
                    .iter()
                    .filter(|primitive| {
                        matches!(
                            primitive.component,
                            AlignmentComponent::Base { .. }
                                | AlignmentComponent::ClipGhostBase { .. }
                        )
                    })
                    .count()
        })
        .sum()
}

/// Every instance a culled draw leaves out projects to nothing inside the clip under the GPU
/// projection mirror (`project_alignment_gpu_instance`, the headless twin of the shader), and
/// the drawn ranges are ascending, disjoint and inside the page.
#[test]
fn culled_draws_omit_only_instances_that_cannot_reach_the_clip() {
    let ledger = AllocationLedger::new_with_transient_budget(8 << 30, 64 << 20, 2 << 30);
    let scene = long_read_scene(&ledger, &long_read_records(7, 90));
    let packed = scene
        .pages()
        .iter()
        .filter(|page| alignment_page_has_gpu_geometry(page))
        .map(|page| {
            (
                page.key(),
                PackedAlignmentPage::from_semantic(page).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut rng = Rng::new(11);
    let mut cameras = 0;
    let mut omitted = 0_usize;
    for bases in [9, 27, 120, 600, 4_000, 20_000, 40_000] {
        for _ in 0..4 {
            let start = rng.between(0, LONG_READ_CONTIG - bases - 1);
            let pan = (rng.next() % 1_000) as f64 / 7.0;
            let scroll = (rng.next() % 6_000) as f64 / 100.0;
            let camera = long_read_camera(start, bases, pan, scroll);
            for page in scene
                .visible_pages(camera.interval(), camera.visible_rows())
                .into_iter()
                .filter(|page| alignment_page_has_gpu_geometry(page))
            {
                let instances = &packed[&page.key()].instances;
                let count = u32::try_from(instances.len()).unwrap();
                let ranges = culled_draw_ranges(page, camera, count, true);
                for pair in ranges.windows(2) {
                    assert!(pair[0].1.end < pair[1].1.start, "{ranges:?}");
                }
                let mut drawn = vec![false; instances.len()];
                for (_, range) in &ranges {
                    assert!(range.end <= count);
                    drawn[range.start as usize..range.end as usize].fill(true);
                }
                for (index, instance) in instances.iter().enumerate() {
                    if drawn[index] {
                        continue;
                    }
                    omitted += 1;
                    assert_eq!(
                        project_alignment_gpu_instance(
                            *instance,
                            page.interval().start(),
                            page.row_range().start,
                            camera,
                        ),
                        None,
                        "{bases} bp camera {camera:?}: culled instance {index} of {:?} is visible",
                        page.key()
                    );
                }
            }
            cameras += 1;
        }
    }
    assert!(
        cameras > 0 && omitted > 0,
        "culling never left anything out"
    );
}

/// Culling makes a frame's instances (and the label projector's anchors) follow the visible
/// area on long reads, where a page holds hundreds of thousands of pieces.
#[test]
fn long_read_frames_draw_and_examine_only_the_visible_area() {
    let ledger = AllocationLedger::new_with_transient_budget(8 << 30, 64 << 20, 2 << 30);
    // About 75x deep: more rows than the lane shows, as on a real long-read locus.
    let scene = long_read_scene(&ledger, &long_read_records(3, 260));
    let mut report = Vec::new();
    for (bases, label) in [(27, "27 bp"), (4_000, "4 kb")] {
        let camera = long_read_camera(48_000, bases, 0.0, 0.0);
        let (drawn, unculled) = culled_instance_counts(&scene, camera);
        let labels =
            rgv_alignment_scene::CpuAlignmentProjector::project_labels(&scene, 1, camera).unwrap();
        let before = unindexed_label_anchors(&scene, camera);
        report.push(format!(
            "{label}: instances drawn {drawn} of {unculled}; label anchors examined {} \
             (previously {before}); labels {}",
            labels.anchors_examined,
            labels.labels.len()
        ));
        eprintln!("{}", report.last().unwrap());
        assert!(
            unculled > 100_000,
            "{label}: the fixture is not long-read sized: {unculled}"
        );
        assert!(
            drawn * 10 < unculled,
            "{label}: drew {drawn} of {unculled} instances"
        );
        assert!(
            labels.anchors_examined * 10 < before,
            "{label}: examined {} of {before} anchors",
            labels.anchors_examined
        );
    }
    assert_eq!(report.len(), 2);
}
