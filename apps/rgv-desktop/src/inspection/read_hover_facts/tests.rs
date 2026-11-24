//! The tooltip's locus counts equal the figures the coverage, pileup and Modifications lanes
//! (and their inspectors) show for the same position.

use super::*;
use crate::ui::read_hover::{LocusFacts, TooltipContext, tooltip_lines};
use rgv_alignment_scene::{
    AlignmentBodyColorToken, AlignmentGeometryToken, AlignmentHandle, AlignmentHit,
    AlignmentHoverTarget, AlignmentSemanticPrimitive, AlignmentSourceRevision, AlignmentStyleToken,
    AllocationLedger, ModificationState, RevisionFingerprint, SourceFingerprint,
};
use rgv_core::{ContigId, TrackId};
use rgv_hts::{AlignmentFlags, AuxField, AuxValue, CigarOp, ModificationCode};

fn record(start: u64, cigar: &[(CigarOpKind, u32)], bases: &[u8], flags: u16) -> AlignmentRecord {
    let reference_length = cigar
        .iter()
        .filter(|(kind, _)| {
            matches!(
                kind,
                CigarOpKind::Match | CigarOpKind::Deletion | CigarOpKind::ReferenceSkip
            )
        })
        .map(|(_, len)| u64::from(*len))
        .sum::<u64>();
    AlignmentRecord {
        contig: ContigId::new(0),
        start,
        end: start + reference_length,
        flags: AlignmentFlags::from_bits(flags),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: format!("read-{start}-{flags}-{}", bases.len()).into_bytes(),
        cigar: cigar
            .iter()
            .map(|(kind, len)| CigarOp {
                kind: *kind,
                len: *len,
            })
            .collect(),
        sequence: SequenceData::Present {
            bases: bases.to_vec(),
            qualities: Some(vec![35; bases.len()]),
        },
        auxiliary_fields: Vec::new(),
    }
}

fn with_call(mut record: AlignmentRecord, probability: u64) -> AlignmentRecord {
    record.auxiliary_fields = vec![
        AuxField {
            tag: *b"MM",
            value: AuxValue::String("C+m,0;".to_owned()),
        },
        AuxField {
            tag: *b"ML",
            value: AuxValue::UnsignedArray(vec![probability]),
        },
    ];
    record
}

fn bases(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn target(
    scene: &AlignmentSceneSnapshot,
    component: AlignmentComponent,
    span: std::ops::Range<u64>,
    geometry: AlignmentGeometryToken,
) -> AlignmentHoverTarget {
    let handle = AlignmentHandle::new(
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        ),
        rgv_hts::AlignmentSourcePosition::bam_virtual_offset(1),
    );
    AlignmentHoverTarget {
        hit: AlignmentHit {
            track: TrackId::new(1),
            scene_revision: scene.reads().revision(),
            handle,
            component,
        },
        primitive: AlignmentSemanticPrimitive {
            start_offset: span.start,
            end_offset: span.end,
            row_offset: 0,
            handle,
            component,
            geometry,
            style: AlignmentStyleToken {
                flags: 0,
                mapping_quality: Some(60),
                operation: None,
                body: AlignmentBodyColorToken::Neutral,
            },
            draw_order: 0,
        },
    }
}

/// A small pileup over an all-`A` reference with a `CpG` at 60..62: insertions after base 19
/// (`TT`, `TA`, `T`), deletions at 30 (two of three bases, one of two), a C at 15 in two reads,
/// soft clips at 50 (two of three reads starting there), and one forward and one reverse
/// cytosine call on that `CpG`.
struct Pileup {
    scene: std::sync::Arc<AlignmentSceneSnapshot>,
    records: Vec<AlignmentRecord>,
    reference: Vec<u8>,
}

fn pileup() -> Pileup {
    use CigarOpKind::{Deletion as D, Insertion as I, Match as M, SoftClip as S};
    let a = |length: usize| vec![b'A'; length];
    let mut mismatched = a(10);
    mismatched[5] = b'C';
    let cpg = b"AAAAACGAAA".to_vec();
    let records = vec![
        record(
            10,
            &[(M, 10), (I, 2), (M, 10), (D, 3), (M, 10)],
            &bases(&[&mismatched, b"TT", &a(20)]),
            0,
        ),
        record(
            10,
            &[(M, 10), (I, 2), (M, 10), (D, 3), (M, 10)],
            &bases(&[&a(10), b"TA", &a(20)]),
            0,
        ),
        record(
            10,
            &[(M, 10), (I, 1), (M, 10), (D, 2), (M, 11)],
            &bases(&[&a(10), b"T", &a(21)]),
            0,
        ),
        record(5, &[(M, 40)], &bases(&[&a(10), b"C", &a(29)]), 0),
        record(50, &[(S, 5), (M, 20)], &a(25), 0),
        record(50, &[(S, 3), (M, 20)], &a(23), 0),
        record(50, &[(M, 20)], &a(20), 0),
        with_call(record(55, &[(M, 10)], &cpg, 0), 230),
        // Reverse: the call is on the read's own C, which is the stored G at 61.
        with_call(record(55, &[(M, 10)], &cpg, 0x10), 200),
    ];
    let mut reference = vec![b'A'; 100];
    reference[60] = b'C';
    reference[61] = b'G';
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 16 << 20, 16 << 20);
    let scene =
        crate::tests::scene_fixture::alignment_scene_snapshot_of(&ledger, &records, &reference);
    Pileup {
        scene,
        records,
        reference,
    }
}

#[test]
fn substitution_counts_are_the_coverage_lane_alleles() {
    let pileup = pileup();
    let scene = &pileup.scene;
    let substitution = AlignmentComponent::Base {
        cigar_index: 0,
        read_index: 5,
        base: b'C',
        quality: Some(35),
        classification: rgv_hts::AlignedBaseClassification::Substitution,
    };
    let target = target(
        scene,
        substitution,
        15..16,
        AlignmentGeometryToken::Rectangle,
    );
    let facts = scene_locus_facts(scene, &target, Some(&pileup.records[0]));
    let alleles = facts.alleles.expect("the scene counts alleles");
    let bin = scene.pileup_site_at(15).unwrap().evidence.bin;
    assert_eq!(alleles.count(b'C'), bin.alleles.c);
    assert_eq!(alleles.count(b'A'), bin.alleles.a);
    assert_eq!(alleles.observations, bin.alleles.base_observations());
    assert_eq!((alleles.count(b'C'), alleles.observations), (2, 4));
    let reference = rgv_hts::ReferenceSlice {
        interval: scene.prepared_interval(),
        bases: pileup.reference.clone(),
    };
    let lines = tooltip_lines(
        &target,
        TooltipContext {
            record: Some(&pileup.records[0]),
            reference: Some(&reference),
            contig: "chr3",
            position: None,
            label: None,
        },
        &facts,
    );
    assert_eq!(lines[0].text, "Substitution A>C · chr3:16 · base quality 35");
    assert_eq!(
        lines[1].text,
        "This change: 2 of 4 reads here (50%) · ref A 2"
    );
}

#[test]
fn insertion_counts_are_the_coverage_inspector_counts() {
    let pileup = pileup();
    let scene = &pileup.scene;
    let insertion = AlignmentComponent::Insertion {
        cigar_index: 1,
        read_start: 10,
        length: 2,
    };
    let target = target(
        scene,
        insertion,
        20..21,
        AlignmentGeometryToken::PointMarker,
    );
    let facts = scene_locus_facts(scene, &target, Some(&pileup.records[0]));
    let insertion_facts = facts
        .insertion
        .clone()
        .expect("the scene counts insertions");
    // The coverage inspector's figures for the boundary after base 19.
    let bin = scene.pileup_site_at(19).unwrap().evidence.bin;
    assert_eq!(insertion_facts.with, bin.alleles.insertion);
    assert_eq!(
        insertion_facts.covering,
        rgv_render::insertion_covering_reads(&bin)
    );
    assert_eq!(insertion_facts.sequences, inserted_sequences_at(scene, 19));
    assert_eq!(insertion_facts.with, 3);
    assert_eq!(
        insertion_facts.sequences,
        [(b"T".to_vec(), 1), (b"TA".to_vec(), 1), (b"TT".to_vec(), 1)]
    );
    let lines = tooltip_lines(
        &target,
        TooltipContext {
            record: Some(&pileup.records[0]),
            reference: None,
            contig: "chr3",
            position: None,
            label: None,
        },
        &facts,
    );
    assert_eq!(
        lines[0].text,
        "Insertion · 2 bp (+TT) · between chr3:20 and 21"
    );
    assert_eq!(
        lines[1].text,
        format!(
            "Insertion here in 3 of {} reads ({:.0}%) · most common: +T (1), +TA (1), +TT (1)",
            insertion_facts.covering,
            300.0 / f64::from(insertion_facts.covering)
        )
    );
}

#[test]
fn deletion_counts_match_exact_spans_among_spanning_reads() {
    let pileup = pileup();
    let scene = &pileup.scene;
    let target = target(
        scene,
        AlignmentComponent::Deletion { cigar_index: 3 },
        30..33,
        AlignmentGeometryToken::EventLine,
    );
    let facts = scene_locus_facts(scene, &target, Some(&pileup.records[0]));
    let deletion = facts.deletion.expect("loaded reads span the deletion");
    // Three-base deletions at 30 in two reads; the two-base one and the plain read span it.
    assert_eq!((deletion.same, deletion.spanning), (2, 4));
    // "Any deletion here" is the allele summary's deletion count over 30.
    assert_eq!(
        deletion.any_over_first_base,
        Some(
            scene
                .pileup_site_at(30)
                .unwrap()
                .evidence
                .bin
                .alleles
                .deletion
        )
    );
    assert_eq!(deletion.any_over_first_base, Some(3));
    let lines = tooltip_lines(
        &target,
        TooltipContext {
            record: Some(&pileup.records[0]),
            reference: None,
            contig: "chr3",
            position: None,
            label: None,
        },
        &facts,
    );
    assert_eq!(lines[0].text, "Deletion · 3 bp · chr3:31–33");
    assert_eq!(
        lines[1].text,
        "Same deletion in 2 of 4 spanning reads (50%) · any deletion here: 3"
    );
}

#[test]
fn soft_clips_are_counted_at_the_same_boundary_and_side() {
    let pileup = pileup();
    let scene = &pileup.scene;
    let clip = AlignmentComponent::SoftClip {
        cigar_index: 0,
        read_start: 0,
        length: 5,
    };
    let target = target(
        scene,
        clip,
        50..51,
        AlignmentGeometryToken::ClipEdge(rgv_alignment_scene::ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: false,
        }),
    );
    let facts = scene_locus_facts(scene, &target, Some(&pileup.records[4]));
    assert_eq!(facts.clipped_here, Some(2));
    let lines = tooltip_lines(
        &target,
        TooltipContext {
            record: Some(&pileup.records[4]),
            reference: None,
            contig: "chr3",
            position: None,
            label: None,
        },
        &facts,
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Soft clip · 5 bp at the left end",
            "AAAAA",
            "Reads soft-clipped at this left boundary: 2"
        ]
    );
}

/// The forward read's call sits on the `CpG`'s C (60); the reverse read's on its G (61), which
/// the modification summary pools onto the C. Both tooltips state that pooled site's tally,
/// exactly the Modifications lane's.
#[test]
fn modification_counts_pool_reverse_strand_cpg_calls_on_the_c() {
    let pileup = pileup();
    let scene = &pileup.scene;
    let reference = rgv_hts::ReferenceSlice {
        interval: scene.prepared_interval(),
        bases: pileup.reference.clone(),
    };
    let lane_tally = crate::inspection::inspector_controller::modification_lane_site(
        "chr20",
        scene.summaries().modification_summary_at(60).unwrap().0,
        scene.summaries().modification_summary_at(60).unwrap().1,
        60,
        1,
        Some(&reference),
    )
    .unwrap()
    .2;
    assert_eq!(lane_tally.called(), 2, "{lane_tally:?}");
    for (record, position, read_index) in [(&pileup.records[7], 60, 5), (&pileup.records[8], 61, 6)]
    {
        assert_eq!(
            modification_summary_site(Some(record), position, read_index, b'C'),
            60
        );
        let call = AlignmentComponent::ModifiedBase {
            read_index,
            canonical_base: b'C',
            code: ModificationCode::Letter(b'm'),
            probability: Some(230),
            state: ModificationState::Modified,
            color_by_type: true,
        };
        let target = target(
            scene,
            call,
            position..position + 1,
            AlignmentGeometryToken::Rectangle,
        );
        let facts = scene_locus_facts(scene, &target, Some(record));
        let modification = facts.modification.clone().expect("the site was called");
        assert_eq!(modification.tally, lane_tally);
        let lines = tooltip_lines(
            &target,
            TooltipContext {
                record: Some(record),
                reference: Some(&reference),
                contig: "chr20",
                position: None,
                label: None,
            },
            &facts,
        );
        assert_eq!(
            lines[0].text,
            format!("5mC · probability 0.90 · chr20:{} (CpG)", position + 1)
        );
        let m = lane_tally.code(ModificationCode::Letter(b'm')).unwrap();
        assert_eq!(
            lines[1].text,
            format!(
                "Methylated in {} of 2 called reads ({:.0}%) · unmodified {}",
                m.modified,
                f64::from(m.modified) * 50.0,
                lane_tally.unmodified()
            )
        );
    }
    // A forward read's G is not moved: only reverse-strand `CpG` calls are pooled.
    assert_eq!(
        modification_summary_site(Some(&pileup.records[7]), 61, 6, b'C'),
        61
    );
}

#[test]
fn facts_are_absent_rather_than_guessed() {
    let pileup = pileup();
    let scene = &pileup.scene;
    // No record: the deletion's span cannot be told, so no deletion figures.
    let target = target(
        scene,
        AlignmentComponent::Deletion { cigar_index: 3 },
        30..33,
        AlignmentGeometryToken::EventLine,
    );
    assert_eq!(
        scene_locus_facts(scene, &target, None),
        LocusFacts::default()
    );
}
