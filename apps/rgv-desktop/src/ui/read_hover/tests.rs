use super::*;
use rgv_alignment_scene::{
    AlignmentBodyColorToken, AlignmentHandle, AlignmentHit, AlignmentLabelKind, AlignmentOperation,
    AlignmentSemanticPrimitive, AlignmentSourceRevision, AlignmentStyleToken, ClipEdgeToken,
    ContractFingerprint, RevisionFingerprint, SbxInsertionToken, SceneRevision, SourceFingerprint,
    SpecialistMode, SpecialistUnavailableReason, UltimaQualitySourceToken,
};
use rgv_core::{ContigId, TrackId};
use rgv_hts::{AlignmentFlags, CigarOp, ModificationCode};

fn handle() -> AlignmentHandle {
    AlignmentHandle::new(
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        ),
        rgv_hts::AlignmentSourcePosition::bam_virtual_offset(7),
    )
}

fn target_with(
    component: AlignmentComponent,
    span: std::ops::Range<u64>,
    geometry: AlignmentGeometryToken,
) -> AlignmentHoverTarget {
    AlignmentHoverTarget {
        hit: AlignmentHit {
            track: TrackId::new(1),
            scene_revision: SceneRevision::new(RevisionFingerprint::new(5, 6)),
            handle: handle(),
            component,
        },
        primitive: AlignmentSemanticPrimitive {
            start_offset: span.start,
            end_offset: span.end,
            row_offset: 0,
            handle: handle(),
            component,
            geometry,
            style: AlignmentStyleToken {
                flags: 0x10,
                mapping_quality: Some(42),
                operation: None,
                body: AlignmentBodyColorToken::Neutral,
            },
            draw_order: 0,
        },
    }
}

fn target(component: AlignmentComponent) -> AlignmentHoverTarget {
    target_with(
        component,
        9_100_813..9_100_814,
        AlignmentGeometryToken::Rectangle,
    )
}

fn hover(component: AlignmentComponent) -> ReadHover {
    ReadHover {
        panel: PanelId::new(1),
        target: target(component),
    }
}

/// `10S 20M 14I 5M 6D 20M 3N 10M` from 9,100,794, with a 14-base insertion after 9,100,813.
fn record() -> AlignmentRecord {
    use CigarOpKind::{
        Deletion as D, Insertion as I, Match as M, ReferenceSkip as N, SoftClip as S,
    };
    let cigar = [
        (S, 10),
        (M, 20),
        (I, 14),
        (M, 5),
        (D, 6),
        (M, 20),
        (N, 3),
        (M, 10),
    ];
    let mut bases = b"ttttttttttAAAAAAAAAAAAAAAAAAAAACGTTGCAACGTTG".to_vec();
    bases.extend(vec![b'A'; 35]);
    let mut qualities = vec![30; bases.len()];
    qualities[25] = 23;
    AlignmentRecord {
        contig: ContigId::new(0),
        start: 9_100_794,
        end: 9_100_794 + 20 + 5 + 6 + 20 + 3 + 10,
        flags: AlignmentFlags::from_bits(0x10),
        mapping_quality: Some(42),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"m64011_190830_220126/123456789/ccs/a-very-long-read-name-indeed".to_vec(),
        cigar: cigar
            .iter()
            .map(|(kind, len)| CigarOp {
                kind: *kind,
                len: *len,
            })
            .collect(),
        sequence: SequenceData::Present {
            bases,
            qualities: Some(qualities),
        },
        auxiliary_fields: Vec::new(),
    }
}

fn texts(lines: &[TooltipLine]) -> Vec<&str> {
    lines.iter().map(|line| line.text.as_str()).collect()
}

fn context(record: Option<&AlignmentRecord>) -> TooltipContext<'_> {
    TooltipContext {
        record,
        reference: None,
        contig: "chr1",
        position: None,
        label: None,
    }
}

#[test]
fn the_tooltip_appears_after_its_delay_and_at_once_once_warm() {
    let start = Instant::now();
    let mut animator = ReadHoverAnimator::default();
    let first = hover(AlignmentComponent::Deletion { cigar_index: 4 });
    assert!(animator.set(start, Some(first), None, false));
    assert_eq!(animator.tooltip(start), None);
    assert_eq!(animator.tooltip(start + Duration::from_millis(249)), None);
    assert_eq!(animator.tooltip(start + TOOLTIP_DELAY), Some(first));
    // The same target again is no change; the pointer's base is updated in place.
    assert!(!animator.set(start + TOOLTIP_DELAY, Some(first), Some(12), false));
    assert_eq!(animator.position(), Some(12));
    // Shown, then moved to another event: its tooltip shows at once.
    let shown = start + TOOLTIP_DELAY + Duration::from_millis(50);
    let second = hover(AlignmentComponent::ReadSpan);
    assert!(animator.set(shown, Some(second), None, false));
    assert_eq!(animator.tooltip(shown), Some(second));
    // Left for longer than the warm period: the delay applies again.
    let later = shown + Duration::from_millis(10);
    animator.set(later, None, None, false);
    let much_later = later + TOOLTIP_WARM + Duration::from_millis(1);
    animator.set(much_later, Some(first), None, false);
    assert_eq!(animator.tooltip(much_later), None);
    assert_eq!(animator.current(), Some(first));
}

#[test]
fn the_highlight_fades_in_and_out_and_repaints_stop_when_it_settles() {
    let start = Instant::now();
    let mut animator = ReadHoverAnimator::default();
    // Nothing hovered: no frames asked for, ever.
    assert_eq!(animator.next_repaint(start, false), None);
    let first = hover(AlignmentComponent::Deletion { cigar_index: 4 });
    animator.set(start, Some(first), None, false);
    let alpha = |animator: &ReadHoverAnimator, at: Instant| {
        animator
            .highlights(at, false)
            .iter()
            .find(|(hover, _)| *hover == first)
            .map_or(0.0, |(_, alpha)| *alpha)
    };
    assert!(alpha(&animator, start).abs() < 1.0e-6);
    let early = alpha(&animator, start + FADE_IN / 4);
    let middle = alpha(&animator, start + FADE_IN / 2);
    // Ease-out: more than linear progress early on, monotone, complete at the end.
    assert!(
        early > 0.25 && early < middle && middle < 1.0,
        "{early} {middle}"
    );
    assert!((alpha(&animator, start + FADE_IN) - 1.0).abs() < 1.0e-6);
    // Every frame while fading, then one frame when the tooltip delay ends, then none.
    assert_eq!(
        animator.next_repaint(start + FADE_IN / 2, false),
        Some(Duration::ZERO)
    );
    assert_eq!(
        animator.next_repaint(start + FADE_IN, false),
        TOOLTIP_DELAY.checked_sub(FADE_IN)
    );
    assert_eq!(animator.next_repaint(start + TOOLTIP_DELAY, false), None);
    assert_eq!(
        animator.next_repaint(start + Duration::from_secs(60), false),
        None
    );

    // Leaving fades out over FADE_OUT from where it was, then asks for nothing more.
    let left = start + TOOLTIP_DELAY;
    animator.set(left, None, None, false);
    let fading = animator.highlights(left + FADE_OUT / 2, false);
    assert_eq!(fading.len(), 1);
    assert!(fading[0].1 > 0.0 && fading[0].1 < 0.5, "{fading:?}");
    assert_eq!(
        animator.next_repaint(left + FADE_OUT / 2, false),
        Some(Duration::ZERO)
    );
    assert!(animator.highlights(left + FADE_OUT, false).is_empty());
    assert_eq!(animator.next_repaint(left + FADE_OUT, false), None);
    assert_eq!(
        animator.next_repaint(left + Duration::from_secs(60), false),
        None
    );
}

#[test]
fn reduced_motion_shows_the_highlight_without_animating() {
    let start = Instant::now();
    let mut animator = ReadHoverAnimator::default();
    let first = hover(AlignmentComponent::ReadSpan);
    animator.set(start, Some(first), None, true);
    assert_eq!(animator.highlights(start, true), [(first, 1.0)]);
    // Only the tooltip delay asks for a frame; no fade frames.
    assert_eq!(animator.next_repaint(start, true), Some(TOOLTIP_DELAY));
    assert_eq!(animator.next_repaint(start + TOOLTIP_DELAY, true), None);
    animator.set(start + TOOLTIP_DELAY, None, None, true);
    assert!(animator.highlights(start + TOOLTIP_DELAY, true).is_empty());
    assert_eq!(animator.next_repaint(start + TOOLTIP_DELAY, true), None);
}

#[test]
fn a_gesture_hides_everything_at_once_and_cools_the_tooltip() {
    let start = Instant::now();
    let mut animator = ReadHoverAnimator::default();
    let first = hover(AlignmentComponent::ReadSpan);
    animator.set(start, Some(first), None, false);
    let shown = start + TOOLTIP_DELAY;
    assert!(animator.tooltip(shown).is_some());
    assert!(animator.suspend());
    // No fade-out, no tooltip, no frames.
    assert!(animator.highlights(shown, false).is_empty());
    assert_eq!(animator.tooltip(shown), None);
    assert_eq!(animator.next_repaint(shown, false), None);
    assert!(!animator.suspend());
    // After the gesture the delay applies again (not warm).
    animator.set(shown, Some(first), None, false);
    assert_eq!(animator.tooltip(shown), None);
}

/// Every kind of element the read stack can draw (or label) has a tooltip.
#[allow(clippy::too_many_lines)]
fn every_component() -> Vec<AlignmentComponent> {
    // Fails to compile when a kind is added, so this list is kept complete.
    fn exhaustive(component: AlignmentComponent) {
        match component {
            AlignmentComponent::ReadSpan
            | AlignmentComponent::AlignedSegment { .. }
            | AlignmentComponent::Insertion { .. }
            | AlignmentComponent::Deletion { .. }
            | AlignmentComponent::ReferenceSkip { .. }
            | AlignmentComponent::SoftClip { .. }
            | AlignmentComponent::HardClip { .. }
            | AlignmentComponent::Padding { .. }
            | AlignmentComponent::DirectionTerminal
            | AlignmentComponent::PairConnector { .. }
            | AlignmentComponent::Label { .. }
            | AlignmentComponent::ModifiedBase { .. }
            | AlignmentComponent::BisulfiteBase { .. }
            | AlignmentComponent::SmrtKinetics { .. }
            | AlignmentComponent::UltimaIndel { .. }
            | AlignmentComponent::SbxInsertion { .. }
            | AlignmentComponent::SbxTailBase { .. }
            | AlignmentComponent::SpecialistUnavailable { .. }
            | AlignmentComponent::Base { .. }
            | AlignmentComponent::ClipGhostBase { .. } => {}
        }
    }
    let mut components = vec![
        AlignmentComponent::ReadSpan,
        AlignmentComponent::AlignedSegment {
            cigar_index: 1,
            operation: AlignmentOperation::Match,
        },
        AlignmentComponent::Insertion {
            cigar_index: 2,
            read_start: 30,
            length: 14,
        },
        AlignmentComponent::Deletion { cigar_index: 4 },
        AlignmentComponent::ReferenceSkip { cigar_index: 6 },
        AlignmentComponent::SoftClip {
            cigar_index: 0,
            read_start: 0,
            length: 10,
        },
        AlignmentComponent::HardClip {
            cigar_index: 0,
            length: 10,
        },
        AlignmentComponent::Padding {
            cigar_index: 0,
            length: 2,
        },
        AlignmentComponent::DirectionTerminal,
        AlignmentComponent::PairConnector {
            template: 1,
            mate: None,
        },
        AlignmentComponent::ModifiedBase {
            read_index: 3,
            canonical_base: b'C',
            code: ModificationCode::Letter(b'm'),
            probability: Some(235),
            state: ModificationState::Modified,
            color_by_type: true,
        },
        AlignmentComponent::BisulfiteBase {
            read_index: 3,
            observation: BisulfiteObservationToken::Converted,
        },
        AlignmentComponent::SmrtKinetics {
            read_index: 3,
            tag: *b"ip",
            value: 12,
        },
        AlignmentComponent::UltimaIndel {
            cigar_index: 2,
            kind: UltimaIndelToken::Deletion,
            quality_bits: Some(12.5_f32.to_bits()),
            quality_source: Some(UltimaQualitySourceToken::T0),
            unavailable: None,
        },
        AlignmentComponent::SbxInsertion {
            cigar_index: 2,
            state: SbxInsertionToken::PartlyDiscordant,
        },
        AlignmentComponent::SbxTailBase { read_index: 2 },
        AlignmentComponent::SpecialistUnavailable {
            mode: SpecialistMode::ModifiedBases,
            reason: SpecialistUnavailableReason::MissingRecordData,
        },
        AlignmentComponent::Base {
            cigar_index: 1,
            read_index: 25,
            base: b'T',
            quality: Some(23),
            classification: AlignedBaseClassification::Substitution,
        },
        AlignmentComponent::ClipGhostBase {
            cigar_index: 0,
            read_index: 3,
            base: b't',
            obscured: false,
        },
    ];
    for kind in [
        AlignmentLabelKind::ReadName,
        AlignmentLabelKind::AuxiliaryTag(*b"HP"),
        AlignmentLabelKind::InsertionLength,
        AlignmentLabelKind::ShortInsertion,
        AlignmentLabelKind::DeletionLength,
        AlignmentLabelKind::SkipLength,
        AlignmentLabelKind::ClipLength,
    ] {
        components.push(AlignmentComponent::Label {
            kind,
            id: ContractFingerprint::new(1),
        });
    }
    components.iter().copied().for_each(exhaustive);
    components
}

#[test]
fn every_kind_of_element_has_a_tooltip() {
    let record = record();
    for component in every_component() {
        assert!(!hover_kind_name(component).is_empty());
        for record in [None, Some(&record)] {
            let lines = tooltip_lines(
                &target(component),
                TooltipContext {
                    label: Some("+14"),
                    ..context(record)
                },
                &LocusFacts::default(),
            );
            assert!(
                !lines.is_empty() && lines.iter().all(|line| !line.text.trim().is_empty()),
                "{component:?}: {lines:?}"
            );
            assert!(lines.len() <= 4, "{component:?}: {lines:?}");
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn tooltips_name_each_event_its_size_and_locus() {
    let record = record();
    let tooltip = |component: AlignmentComponent,
                   span: std::ops::Range<u64>,
                   geometry: AlignmentGeometryToken| {
        tooltip_lines(
            &target_with(component, span, geometry),
            context(Some(&record)),
            &LocusFacts::default(),
        )
    };
    // Insertion: its length, the bases either side, and its first 30 bases then an ellipsis
    // (all 14 here).
    let insertion = tooltip(
        AlignmentComponent::Insertion {
            cigar_index: 2,
            read_start: 30,
            length: 14,
        },
        9_100_814..9_100_815,
        AlignmentGeometryToken::PointMarker,
    );
    assert_eq!(
        texts(&insertion),
        [
            "Insertion · 14 bp · between chr1:9,100,814 and 9,100,815",
            "ACGTTGCAACGTTG"
        ]
    );
    assert!(insertion[1].monospace);
    assert_eq!(
        sequence_preview(&[b'a'; 31]),
        format!("{}…", "A".repeat(30))
    );
    assert_eq!(
        texts(&tooltip(
            AlignmentComponent::Deletion { cigar_index: 4 },
            9_100_819..9_100_825,
            AlignmentGeometryToken::EventLine,
        )),
        ["Deletion · 6 bp · chr1:9,100,820–9,100,825"]
    );
    assert_eq!(
        texts(&tooltip(
            AlignmentComponent::ReferenceSkip { cigar_index: 6 },
            9_100_845..9_100_848,
            AlignmentGeometryToken::EventLine,
        )),
        ["Splice gap · intron 3 bp · chr1:9,100,846–9,100,848"]
    );
    let soft = tooltip(
        AlignmentComponent::SoftClip {
            cigar_index: 0,
            read_start: 0,
            length: 10,
        },
        9_100_794..9_100_795,
        AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: false,
        }),
    );
    assert_eq!(
        texts(&soft),
        ["Soft clip · 10 bp at the left end", "TTTTTTTTTT"]
    );
    let ghost = tooltip(
        AlignmentComponent::ClipGhostBase {
            cigar_index: 0,
            read_index: 3,
            base: b't',
            obscured: false,
        },
        9_100_787..9_100_788,
        AlignmentGeometryToken::ClipGhost,
    );
    assert_eq!(
        ghost[0].text,
        "Soft-clipped base T · base 4 of the 10-base clip · chr1:9,100,788"
    );
    // Substitution with its reference base and base quality.
    let reference = ReferenceSlice {
        interval: rgv_core::GenomicInterval::new(ContigId::new(0), 9_100_800, 9_100_830).unwrap(),
        bases: vec![b'C'; 30],
    };
    let substitution = tooltip_lines(
        &target_with(
            AlignmentComponent::Base {
                cigar_index: 1,
                read_index: 25,
                base: b'T',
                quality: Some(23),
                classification: AlignedBaseClassification::Substitution,
            },
            9_100_809..9_100_810,
            AlignmentGeometryToken::Rectangle,
        ),
        TooltipContext {
            reference: Some(&reference),
            ..context(Some(&record))
        },
        &LocusFacts::default(),
    );
    assert_eq!(
        texts(&substitution),
        ["Substitution C>T · chr1:9,100,810 · base quality 23"]
    );
    // A modification call on a CpG.
    let mut cpg = reference.clone();
    cpg.bases[10] = b'C';
    cpg.bases[11] = b'G';
    let call = tooltip_lines(
        &target_with(
            AlignmentComponent::ModifiedBase {
                read_index: 3,
                canonical_base: b'C',
                code: ModificationCode::Letter(b'm'),
                probability: Some(235),
                state: ModificationState::Modified,
                color_by_type: true,
            },
            9_100_810..9_100_811,
            AlignmentGeometryToken::Rectangle,
        ),
        TooltipContext {
            reference: Some(&cpg),
            ..context(Some(&record))
        },
        &LocusFacts::default(),
    );
    assert_eq!(
        texts(&call),
        ["5mC · probability 0.92 · chr1:9,100,811 (CpG)"]
    );
    // The read itself: name (shortened), strand, MAPQ, length, the base under the pointer.
    let read = tooltip_lines(
        &target(AlignmentComponent::ReadSpan),
        TooltipContext {
            position: Some(9_100_816),
            reference: Some(&reference),
            ..context(Some(&record))
        },
        &LocusFacts::default(),
    );
    assert_eq!(
        texts(&read),
        [
            "m64011_190830_220126/123456789/ccs/a-ver…",
            "Reverse strand · MAPQ 42 · 79 bp",
            "Base 9,100,817 · ref C · read A · base quality 30",
            "Click for details"
        ]
    );
    // In a deletion the read has no base.
    let deleted = tooltip_lines(
        &target(AlignmentComponent::ReadSpan),
        TooltipContext {
            position: Some(9_100_819),
            ..context(Some(&record))
        },
        &LocusFacts::default(),
    );
    assert_eq!(deleted[2].text, "Base 9,100,820 · not aligned here");
    // Without its record the read still has a tooltip, from its drawn style.
    let bare = tooltip_lines(
        &target(AlignmentComponent::ReadSpan),
        context(None),
        &LocusFacts::default(),
    );
    assert_eq!(
        texts(&bare),
        ["Read", "Reverse strand · MAPQ 42", "Click for details"]
    );
}

#[test]
fn junction_pair_and_label_tooltips_state_what_they_mean() {
    let record = record();
    let facts = LocusFacts {
        junction: Some(JunctionFacts {
            depth: 12_300,
            motif: "GT-AG".to_owned(),
            strand: "− (motif)".to_owned(),
            annotation: Some("annotated".to_owned()),
        }),
        pair: Some(PairFacts {
            insert_size: 312,
            mate: "chr1:9,100,950".to_owned(),
            orientation: "FR",
            classification: "Proper pair",
        }),
        ..LocusFacts::default()
    };
    let splice = tooltip_lines(
        &target_with(
            AlignmentComponent::ReferenceSkip { cigar_index: 6 },
            9_100_845..9_100_848,
            AlignmentGeometryToken::EventLine,
        ),
        context(Some(&record)),
        &facts,
    );
    assert_eq!(
        splice[1].text,
        "Junction: 12,300 reads · GT-AG · − (motif) strand · annotated"
    );
    let pair = tooltip_lines(
        &target(AlignmentComponent::PairConnector {
            template: 1,
            mate: None,
        }),
        context(Some(&record)),
        &facts,
    );
    assert_eq!(
        pair[0].text,
        "Pair · insert size 312 · mate at chr1:9,100,950 · FR · Proper pair"
    );
    let label = |kind: AlignmentLabelKind, text: &str| {
        tooltip_lines(
            &target(AlignmentComponent::Label {
                kind,
                id: ContractFingerprint::new(1),
            }),
            TooltipContext {
                label: Some(text),
                ..context(None)
            },
            &LocusFacts::default(),
        )[0]
        .text
        .clone()
    };
    assert_eq!(
        label(AlignmentLabelKind::ClipLength, "10H"),
        "Clip length · hard clip of 10 bp (bases not stored)"
    );
    assert_eq!(
        label(AlignmentLabelKind::ClipLength, "7"),
        "Clip length · soft clip of 7 bp (faint bases beside the cut edge)"
    );
    assert_eq!(
        label(AlignmentLabelKind::InsertionLength, "+14"),
        "Insertion length chip · 14 inserted bases · hover the I-beam for the sequence"
    );
    assert_eq!(
        label(AlignmentLabelKind::DeletionLength, "6"),
        "Deletion length · 6 bp"
    );
}

#[test]
fn the_tooltip_cache_builds_each_target_once() {
    let mut cache = ReadHoverTooltipCache::default();
    let deletion = target(AlignmentComponent::Deletion { cigar_index: 4 });
    let read = target(AlignmentComponent::ReadSpan);
    let mut fetches = 0;
    for _ in 0..3 {
        let lines = cache.lines(&deletion, Some(5), |cache| {
            cache.record(&deletion, || {
                fetches += 1;
                Some(record())
            });
            vec![TooltipLine::text("x".to_owned())]
        });
        assert_eq!(lines.len(), 1);
    }
    // Over an event the pointer's base does not matter.
    cache.lines(&deletion, Some(6), |_| Vec::new());
    assert_eq!(cache.builds, 1);
    // Over the read itself it does; the read's record is not fetched again.
    cache.lines(&read, Some(5), |cache| {
        cache.record(&read, || {
            fetches += 1;
            None
        });
        Vec::new()
    });
    cache.lines(&read, Some(6), |_| Vec::new());
    assert_eq!(cache.builds, 3);
    assert_eq!(fetches, 1);
}

#[test]
fn highlights_ring_the_shape_outside_it_and_grow_out_as_they_fade_in() {
    // A 10 x 11 box; `outline_at(gap)` is that box grown by `gap` on every side.
    let outline_at = |gap: f64| {
        vec![
            [10.0 - gap, 20.0 - gap],
            [20.0 + gap, 20.0 - gap],
            [20.0 + gap, 31.0 + gap],
            [10.0 - gap, 31.0 + gap],
        ]
    };
    let clip = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
    let color = egui::Color32::from_rgb(71, 133, 240);
    let paths = |alpha: f32| {
        highlight_shapes(outline_at, alpha, clip, color)
            .into_iter()
            .map(|shape| {
                assert_eq!(shape.clip_rect, clip);
                let egui::Shape::Path(path) = shape.shape else {
                    panic!("a closed outline");
                };
                assert!(path.closed);
                let egui::epaint::ColorMode::Solid(drawn) = path.stroke.color else {
                    panic!("solid");
                };
                (path.points[0], path.stroke.width, drawn)
            })
            .collect::<Vec<_>>()
    };
    let settled = paths(1.0);
    assert_eq!(settled.len(), 3, "two halo layers and the ring");
    let (ring_corner, ring_width, ring_color) = settled[2];
    // The ring sits the settled gap clear of the shape, in the hover colour.
    let gap = HIGHLIGHT_RING_GAP as f32;
    assert_eq!(ring_corner, egui::pos2(10.0 - gap, 20.0 - gap));
    assert_eq!(ring_color, color.gamma_multiply(0.95));
    // No layer's stroke reaches into the shape, and the halo fades outward.
    for (corner, width, _) in &settled {
        assert!(10.0 - corner.x - width / 2.0 >= 0.0, "{corner:?} {width}");
    }
    assert!(settled[0].2.a() < settled[1].2.a() && settled[1].2.a() < ring_color.a());
    assert!((ring_width - 1.5).abs() < f32::EPSILON);
    // Half faded in, every layer is fainter and the ring is still closer to the shape.
    let entering = paths(0.5);
    assert!(entering[2].0.x > ring_corner.x);
    assert_eq!(entering[2].2, color.gamma_multiply(0.95 * 0.5));
}

#[test]
fn a_tooltip_card_splits_the_first_line_and_names_the_copy_shortcut() {
    let event = [
        TooltipLine::text("Substitution C>T · chr3:52,694,330 · base quality 37".to_owned()),
        TooltipLine::text("T in 12 of 40 reads (30%)".to_owned()),
    ];
    let card = TooltipCard::new(&event).expect("a card");
    assert_eq!(card.heading.text, "Substitution C>T");
    assert_eq!(
        card.subtitle.as_deref(),
        Some("chr3:52,694,330 · base quality 37")
    );
    assert_eq!(texts(&card.body), ["T in 12 of 40 reads (30%)"]);
    assert_eq!(card.hint, format!("{COPY_SHORTCUT} to copy"));
    // A read: its monospace name is the whole heading and the click hint becomes the footer.
    let read = [
        TooltipLine::sequence("A00201R:287:HLF75DSXY:2:1302:3667:20306".to_owned()),
        TooltipLine::text("Forward strand · MAPQ 60 · 101 bp".to_owned()),
        TooltipLine::text("Click for details".to_owned()),
    ];
    let card = TooltipCard::new(&read).expect("a card");
    assert!(card.heading.monospace);
    assert_eq!(card.subtitle, None);
    assert_eq!(texts(&card.body), ["Forward strand · MAPQ 60 · 101 bp"]);
    assert_eq!(
        card.hint,
        format!("Click for details · {COPY_SHORTCUT} to copy")
    );
    assert_eq!(
        tooltip_copy_text(&read),
        "A00201R:287:HLF75DSXY:2:1302:3667:20306\nForward strand · MAPQ 60 · 101 bp"
    );
    assert_eq!(TooltipCard::new(&[]), None);
}

#[test]
fn modifier_keys_and_copy_keep_the_hover_but_navigation_keys_hide_it() {
    use winit::keyboard::{Key, NamedKey};
    let character = |text: &str| Key::Character(text.into());
    for modifier in [
        NamedKey::Super,
        NamedKey::Shift,
        NamedKey::Control,
        NamedKey::Alt,
    ] {
        assert!(!key_moves_view(&Key::Named(modifier), false));
        assert!(!key_moves_view(&Key::Named(modifier), true));
    }
    // Copying the tooltip, or a screenshot chord such as Cmd+Shift+4.
    assert!(!key_moves_view(&character("c"), true));
    assert!(!key_moves_view(&character("4"), true));
    // Feature jumps, undo and plain navigation keys move the view.
    for jump in ["f", "b", "z"] {
        assert!(key_moves_view(&character(jump), true));
    }
    assert!(key_moves_view(&character("+"), false));
    assert!(key_moves_view(&Key::Named(NamedKey::ArrowLeft), false));
}

#[test]
fn a_pointer_is_over_the_content_area_only_inside_it() {
    let (origin, size) = ((100.0, 50.0), (800.0, 600.0));
    assert!(content_contains(origin, size, (100.0, 50.0)));
    assert!(content_contains(origin, size, (500.0, 400.0)));
    assert!(!content_contains(origin, size, (900.0, 400.0)));
    assert!(!content_contains(origin, size, (500.0, 49.0)));
    assert_eq!(
        palette_color32([0.28, 0.52, 0.94, 1.0]),
        egui::Color32::from_rgb(71, 133, 240)
    );
}

#[test]
fn the_tooltip_sits_beside_the_pointer_and_stays_on_screen() {
    let screen = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
    let size = egui::vec2(200.0, 60.0);
    let rect = tooltip_rect(egui::pos2(100.0, 100.0), size, screen);
    assert_eq!(rect.min, egui::pos2(114.0, 116.0));
    // Near the right and bottom edges it flips to the other side of the pointer.
    let rect = tooltip_rect(egui::pos2(750.0, 580.0), size, screen);
    assert_eq!(rect.min, egui::pos2(540.0, 510.0));
    assert!(screen.contains_rect(rect));
    // Wider than the screen allows: kept inside.
    let rect = tooltip_rect(egui::pos2(5.0, 5.0), egui::vec2(900.0, 60.0), screen);
    assert!(rect.left() >= 0.0);
}
