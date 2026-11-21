//! Seeded, dependency-free property harness for the alignment scene.
//!
//! Every case derives random reads (plain, indel-bearing, spliced, long, paired, modified) and a
//! random tile size, acquisition partition and extension order from one seed, drives them
//! through the real [`EvidenceStore`] path, and checks three families of invariants:
//!
//! * an extended evidence root and row atlas describe the same records as a clean build;
//! * per-tile exact summaries add up to a brute-force whole-interval pileup, including at
//!   every tile seam;
//! * no builder ever retains more than it reserved (a build fails, or a debug assertion fires,
//!   when one does).
//!
//! A failure message always names the seed, so a case can be replayed on its own.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
    ModifiedBaseColorMode, OverlappingMatePolicy, ReadBaseDisplayMode, ReadStackingMode,
};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, SequenceData,
    TemplateLengthBounds,
};

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentPageKey, AlignmentReadLabelMode,
    AlignmentRowAtlas, AlignmentSemanticPage, AlignmentSourceRevision, AlignmentStyleContract,
    AlleleSummaryDetail, AllocationClass, AllocationLedger, CanonicalGenomicTile,
    CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement, CompleteCoverage,
    ContractFingerprint, DepthSummaryContract, EvidenceAcquisition, EvidenceChunk, EvidenceRoot,
    EvidenceStore, ExactDepthSummaryTile, ExactSummaryRoot, FilteredRecordProvenance, GeometryLod,
    LayoutContract, PayloadContract, PayloadSidecar, PresentationContract, ReadSampling,
    ReservationKind, RevisionFingerprint, RowAtlasRevision, SourceChunkId, SourceFingerprint,
    SummaryContract,
};

const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(23);
const CASES: u64 = 20;

/// Small xorshift64* generator: deterministic, seedable, and no dependency.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 16
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }

    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }

    fn length(&mut self, low: u32, high: u32) -> u32 {
        u32::try_from(self.between(u64::from(low), u64::from(high))).unwrap()
    }
}

fn source_revision() -> AlignmentSourceRevision {
    AlignmentSourceRevision::new(
        SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
        RevisionFingerprint::new(3, 4),
    )
}

fn payload_contract() -> PayloadContract {
    PayloadContract::new_all_auxiliary(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        None,
    )
}

fn interval(start: u64, end: u64) -> GenomicInterval {
    GenomicInterval::new(ContigId::new(0), start, end).unwrap()
}

const fn is_aligned(kind: CigarOpKind) -> bool {
    matches!(
        kind,
        CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch
    )
}

fn reference_span(cigar: &[CigarOp]) -> u64 {
    cigar
        .iter()
        .filter(|operation| {
            is_aligned(operation.kind)
                || matches!(
                    operation.kind,
                    CigarOpKind::Deletion | CigarOpKind::ReferenceSkip
                )
        })
        .map(|operation| u64::from(operation.len))
        .sum()
}

fn query_length(cigar: &[CigarOp]) -> usize {
    cigar
        .iter()
        .filter(|operation| {
            is_aligned(operation.kind)
                || matches!(
                    operation.kind,
                    CigarOpKind::Insertion | CigarOpKind::SoftClip
                )
        })
        .map(|operation| operation.len as usize)
        .sum()
}

/// Random CIGAR: aligned blocks separated by insertions, deletions or reference skips, with
/// optional clips. It always starts and ends (inside the clips) with an aligned block.
fn random_cigar(rng: &mut XorShift) -> Vec<CigarOp> {
    let (blocks, block_low, block_high, skip_odds) = match rng.below(20) {
        0..=7 => (1, 20, 120, 0),
        8..=12 => (rng.between(2, 5), 8, 60, 0),
        13..=16 => (rng.between(2, 4), 15, 80, 10),
        _ => (rng.between(8, 30), 20, 150, 1),
    };
    let mut cigar = Vec::new();
    let op = |kind, len| CigarOp { kind, len };
    if rng.chance(1, 12) {
        cigar.push(op(CigarOpKind::HardClip, rng.length(1, 9)));
    }
    if rng.chance(1, 6) {
        cigar.push(op(CigarOpKind::SoftClip, rng.length(1, 12)));
    }
    for block in 0..blocks {
        if block > 0 {
            if rng.chance(skip_odds, 10) {
                cigar.push(op(CigarOpKind::ReferenceSkip, rng.length(30, 500)));
            } else if rng.chance(1, 2) {
                cigar.push(op(CigarOpKind::Insertion, rng.length(1, 8)));
            } else {
                cigar.push(op(CigarOpKind::Deletion, rng.length(1, 12)));
            }
        }
        let kind = match rng.below(10) {
            0 => CigarOpKind::SequenceMatch,
            1 => CigarOpKind::SequenceMismatch,
            _ => CigarOpKind::Match,
        };
        cigar.push(op(kind, rng.length(block_low, block_high)));
    }
    if rng.chance(1, 6) {
        cigar.push(op(CigarOpKind::SoftClip, rng.length(1, 12)));
    }
    if rng.chance(1, 12) {
        cigar.push(op(CigarOpKind::HardClip, rng.length(1, 9)));
    }
    cigar
}

const fn complement(base: u8) -> u8 {
    match base {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' => b'A',
        other => other,
    }
}

/// Valid `MM`/`ML`/`MN` tags for `bases`, in the original sequencer orientation.
fn modification_tags(rng: &mut XorShift, bases: &[u8], reverse: bool) -> Vec<AuxField> {
    let oriented = if reverse {
        bases.iter().rev().copied().map(complement).collect()
    } else {
        bases.to_vec()
    };
    let mut mm = String::new();
    let mut ml = Vec::new();
    for (canonical, header, codes) in [(b'C', "C+m", 1), (b'C', "C+mh?", 2), (b'A', "A+a.", 1)] {
        if rng.chance(1, 2) {
            continue;
        }
        let mut deltas = String::new();
        let mut skipped = 0_u32;
        for base in &oriented {
            if *base != canonical {
                continue;
            }
            if rng.chance(1, 3) {
                write!(deltas, ",{skipped}").unwrap();
                skipped = 0;
                for _ in 0..codes {
                    ml.push(rng.below(256));
                }
            } else {
                skipped += 1;
            }
        }
        if !deltas.is_empty() {
            mm.push_str(header);
            mm.push_str(&deltas);
            mm.push(';');
        }
    }
    if mm.is_empty() {
        return Vec::new();
    }
    vec![
        AuxField {
            tag: *b"MM",
            value: AuxValue::String(mm),
        },
        AuxField {
            tag: *b"ML",
            value: AuxValue::UnsignedArray(ml),
        },
        AuxField {
            tag: *b"MN",
            value: AuxValue::Unsigned(bases.len() as u64),
        },
    ]
}

fn random_record(
    rng: &mut XorShift,
    name: &str,
    contig_length: u64,
    near: Option<u64>,
) -> AlignmentRecord {
    let cigar = loop {
        let cigar = random_cigar(rng);
        if reference_span(&cigar) < contig_length {
            break cigar;
        }
    };
    let span = reference_span(&cigar);
    let latest = contig_length - span;
    let start = match near {
        Some(near) => (near.saturating_sub(40) + rng.below(400)).min(latest),
        None => rng.below(latest + 1),
    };
    let reverse = rng.chance(1, 2);
    let bases = (0..query_length(&cigar))
        .map(|_| b"ACGT"[rng.below(4) as usize])
        .collect::<Vec<_>>();
    let qualities = bases
        .iter()
        .map(|_| u8::try_from(rng.between(2, 40)).unwrap())
        .collect();
    let mut auxiliary_fields = Vec::new();
    if cigar
        .iter()
        .any(|operation| operation.kind == CigarOpKind::ReferenceSkip)
        && rng.chance(7, 10)
    {
        auxiliary_fields.push(AuxField {
            tag: *b"XS",
            value: AuxValue::Character(if rng.chance(1, 2) { b'+' } else { b'-' }),
        });
    }
    if rng.chance(1, 3) {
        auxiliary_fields.extend(modification_tags(rng, &bases, reverse));
        if rng.chance(1, 2) {
            auxiliary_fields.push(AuxField {
                tag: *b"HP",
                value: AuxValue::Unsigned(rng.between(1, 2)),
            });
        }
    }
    AlignmentRecord {
        contig: ContigId::new(0),
        start,
        end: start + span,
        flags: AlignmentFlags::from_bits(if reverse { 0x10 } else { 0 }),
        mapping_quality: Some(u8::try_from(rng.between(0, 60)).unwrap()),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: name.as_bytes().to_vec(),
        cigar,
        sequence: SequenceData::Present {
            bases,
            qualities: Some(qualities),
        },
        auxiliary_fields,
    }
}

fn random_records(rng: &mut XorShift, contig_length: u64) -> Vec<AlignmentRecord> {
    let mut records = Vec::new();
    for template in 0..rng.between(20, 80) {
        let name = format!("template-{template:03}");
        let mut first = random_record(rng, &name, contig_length, None);
        if rng.chance(2, 5) {
            let mut last = random_record(rng, &name, contig_length, Some(first.start));
            let reverse = |record: &AlignmentRecord| u16::from(record.flags.is_reverse());
            first.flags =
                AlignmentFlags::from_bits(0x1 | 0x40 | reverse(&first) << 4 | reverse(&last) << 5);
            last.flags =
                AlignmentFlags::from_bits(0x1 | 0x80 | reverse(&last) << 4 | reverse(&first) << 5);
            first.mate_contig = Some(ContigId::new(0));
            first.mate_start = Some(last.start);
            last.mate_contig = Some(ContigId::new(0));
            last.mate_start = Some(first.start);
            records.push(first);
            records.push(last);
        } else {
            records.push(first);
        }
    }
    records
}

/// One generated case: reads, each in its own physical source chunk, plus the tile size and
/// the acquisition pieces (cut on tile boundaries) in the order they will be admitted.
struct World {
    seed: u64,
    ledger: AllocationLedger,
    contig_length: u64,
    tile_bases: NonZeroU64,
    records: Vec<AlignmentRecord>,
    chunks: Vec<Arc<EvidenceChunk>>,
    /// `(admitted piece, target after admitting it, whether it extended leftwards)`.
    steps: Vec<(GenomicInterval, GenomicInterval, bool)>,
}

fn handle(index: usize) -> AlignmentHandle {
    AlignmentHandle::new(source_revision(), source_position(index))
}

fn source_position(index: usize) -> AlignmentSourcePosition {
    AlignmentSourcePosition::bam_virtual_offset(((index as u64 + 1) << 16) | 1)
}

fn chunk(ledger: &AllocationLedger, record: &AlignmentRecord, index: usize) -> Arc<EvidenceChunk> {
    let mut decoded = AlignmentBatch::default();
    decoded
        .push_with_source_position(record, source_position(index))
        .unwrap();
    let payload = Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
    let core = CanonicalRecordChunk::new(
        source_revision(),
        SourceChunkId::BamBlock {
            compressed_offset: index as u64 + 1,
        },
        Arc::new(decoded.copy_core().unwrap()),
        RECORD_CONTRACT,
    )
    .unwrap();
    let register = |class, bytes| {
        ledger
            .reserve(class, bytes, ReservationKind::Normal)
            .unwrap()
            .register(bytes, 1)
            .unwrap()
    };
    let payload_allocation = register(AllocationClass::PayloadSidecar, payload.estimated_bytes());
    let payload = Arc::new(
        PayloadSidecar::new(&core, payload_contract(), payload, payload_allocation).unwrap(),
    );
    let footprint = EvidenceChunk::footprint(&core);
    let core_allocation = register(AllocationClass::CoreChunk, footprint.core_bytes);
    let overlap_allocation = register(AllocationClass::OverlapIndex, footprint.overlap_index_bytes);
    Arc::new(
        EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
            .unwrap(),
    )
}

impl World {
    fn new(seed: u64) -> Self {
        let mut rng = XorShift::new(seed);
        let tile_bases = [50_u64, 64, 100, 128, 250][rng.below(5) as usize];
        let contig_length = rng.between(1_200, 3_000);
        let records = random_records(&mut rng, contig_length);
        let ledger = AllocationLedger::new(1 << 30, 0);
        let chunks = records
            .iter()
            .enumerate()
            .map(|(index, record)| chunk(&ledger, record, index))
            .collect();

        // Cut the contig into acquisition pieces on tile boundaries, then admit them outwards
        // from a random piece so the target grows left and right in a random order.
        let tiles = contig_length.div_ceil(tile_bases);
        let mut cuts = vec![0, contig_length];
        for _ in 0..rng.between(1, 4) {
            cuts.push((rng.between(1, tiles - 1) * tile_bases).min(contig_length));
        }
        cuts.sort_unstable();
        cuts.dedup();
        let pieces = cuts
            .windows(2)
            .map(|pair| interval(pair[0], pair[1]))
            .collect::<Vec<_>>();
        let first = rng.below(pieces.len() as u64) as usize;
        let (mut low, mut high) = (first, first);
        let mut steps = vec![(pieces[first], pieces[first], false)];
        while low > 0 || high + 1 < pieces.len() {
            let left = high + 1 == pieces.len() || (low > 0 && rng.chance(1, 2));
            if left {
                low -= 1;
            } else {
                high += 1;
            }
            let piece = pieces[if left { low } else { high }];
            steps.push((
                piece,
                interval(pieces[low].start(), pieces[high].end()),
                left,
            ));
        }
        Self {
            seed,
            ledger,
            contig_length,
            tile_bases: NonZeroU64::new(tile_bases).unwrap(),
            records,
            chunks,
            steps,
        }
    }

    fn overlapping(&self, target: GenomicInterval) -> impl Iterator<Item = usize> + '_ {
        self.records
            .iter()
            .enumerate()
            .filter(move |(_, record)| record.start < target.end() && record.end > target.start())
            .map(|(index, _)| index)
    }

    /// A complete acquisition certificate for `target`: every read overlapping it.
    fn acquisition(&self, target: GenomicInterval) -> Arc<EvidenceAcquisition> {
        let chunks = self
            .overlapping(target)
            .map(|index| Arc::clone(&self.chunks[index]))
            .collect::<Vec<_>>();
        let records = chunks.len() as u64;
        Arc::new(
            EvidenceAcquisition::new(
                CompleteCoverage::new(
                    source_revision(),
                    RECORD_CONTRACT,
                    target,
                    FilteredRecordProvenance {
                        records_seen: records,
                        eligible_records: records,
                        ..FilteredRecordProvenance::default()
                    },
                )
                .unwrap(),
                chunks,
            )
            .unwrap(),
        )
    }

    fn store(&self) -> EvidenceStore {
        EvidenceStore::new(source_revision(), RECORD_CONTRACT, self.ledger.clone())
    }

    /// Evidence built in one piece from a single certificate over `target`.
    fn clean_evidence(&self, target: GenomicInterval) -> Arc<EvidenceRoot> {
        let mut store = self.store();
        store.admit(self.acquisition(target)).unwrap();
        Arc::clone(
            store
                .prepare(target, payload_contract(), ReservationKind::Normal)
                .unwrap()
                .root(),
        )
    }
}

fn layout(stacking: ReadStackingMode, sampling: ReadSampling) -> LayoutContract {
    LayoutContract {
        stacking,
        grouping: AlignmentGroupMode::None,
        group_order: AlignmentOrderDirection::Natural,
        sorting: AlignmentSortMode::Start,
        sort_order: AlignmentOrderDirection::Natural,
        sort_anchor: None,
        view_as_pairs: false,
        pair_length_bounds: TemplateLengthBounds::default(),
        promoted_template: None,
        sampling,
    }
}

fn summary_contract(policy: OverlappingMatePolicy) -> SummaryContract {
    SummaryContract {
        overlapping_mate_policy: policy,
        minimum_base_quality: 0,
        minimum_junction_flank: 0,
        allele_detail: AlleleSummaryDetail::Counts,
        collect_modifications: true,
    }
}

/// `handle -> (row, start, end)` for every record holding a row, checking on the way that no
/// two records in one row overlap.
fn atlas_rows(
    atlas: &AlignmentRowAtlas,
    target: GenomicInterval,
    context: &str,
) -> BTreeMap<AlignmentHandle, (u32, u64, u64)> {
    let rows = u32::try_from(atlas.row_count()).unwrap();
    let mut by_row = BTreeMap::<u32, Vec<(u64, u64)>>::new();
    let mut records = BTreeMap::new();
    for hit in atlas.records_for_rows(0..rows, target) {
        let record = hit.record;
        by_row
            .entry(record.row())
            .or_default()
            .push((record.start(), record.end()));
        assert!(
            records
                .insert(
                    record.handle(),
                    (record.row(), record.start(), record.end())
                )
                .is_none(),
            "{context}: a record holds two rows"
        );
    }
    for (row, mut spans) in by_row {
        spans.sort_unstable();
        for pair in spans.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0,
                "{context}: row {row} overlaps {pair:?}"
            );
        }
    }
    assert_eq!(records.len(), atlas.record_count(), "{context}");
    records
}

/// Brute-force per-base aligned depth, A/C/G/T counts and junction support over the contig.
struct Pileup {
    depth: Vec<u32>,
    bases: Vec<[u32; 4]>,
    junctions: BTreeMap<(u64, u64), u32>,
}

fn pileup(world: &World) -> Pileup {
    let length = usize::try_from(world.contig_length).unwrap();
    let mut pileup = Pileup {
        depth: vec![0; length],
        bases: vec![[0; 4]; length],
        junctions: BTreeMap::new(),
    };
    for record in &world.records {
        let SequenceData::Present { bases, .. } = &record.sequence else {
            unreachable!("generated reads carry a sequence");
        };
        let mut reference = usize::try_from(record.start).unwrap();
        let mut read = 0_usize;
        for operation in &record.cigar {
            let len = operation.len as usize;
            match operation.kind {
                kind if is_aligned(kind) => {
                    for offset in 0..len {
                        pileup.depth[reference + offset] += 1;
                        let base = b"ACGT"
                            .iter()
                            .position(|base| *base == bases[read + offset])
                            .unwrap();
                        pileup.bases[reference + offset][base] += 1;
                    }
                    reference += len;
                    read += len;
                }
                CigarOpKind::Insertion | CigarOpKind::SoftClip => read += len,
                CigarOpKind::Deletion => reference += len,
                CigarOpKind::ReferenceSkip => {
                    *pileup
                        .junctions
                        .entry((reference as u64, (reference + len) as u64))
                        .or_default() += 1;
                    reference += len;
                }
                _ => {}
            }
        }
    }
    pileup
}

fn tile_depths(tiles: &[Arc<ExactDepthSummaryTile>], prepared: GenomicInterval) -> Vec<u32> {
    let mut depth = vec![0_u32; usize::try_from(prepared.len()).unwrap()];
    let mut covered = 0_u64;
    for tile in tiles {
        let tile_interval = tile.depth().interval();
        covered += tile_interval.len();
        for run in tile.depth().runs() {
            // A run never leaves its own tile: seams are where runs must meet exactly.
            assert!(tile_interval.start() <= run.start && run.end <= tile_interval.end());
            for position in run.start..run.end {
                depth[usize::try_from(position - prepared.start()).unwrap()] += run.depth;
            }
        }
    }
    assert_eq!(covered, prepared.len(), "tiles partition the interval");
    depth
}

fn assert_summaries_match_pileup(
    world: &World,
    expected: &Pileup,
    root: &ExactSummaryRoot,
    context: &str,
) {
    let prepared = root.prepared_interval();
    let offset = usize::try_from(prepared.start()).unwrap();
    let depth = tile_depths(root.depth_tiles(), prepared);
    for (index, depth) in depth.iter().enumerate() {
        let position = offset + index;
        assert_eq!(
            *depth, expected.depth[position],
            "{context}: depth at {position}"
        );
        let alleles = root
            .allele_site(position as u64)
            .unwrap()
            .map_or([0; 4], |site| {
                [
                    site.alleles.a,
                    site.alleles.c,
                    site.alleles.g,
                    site.alleles.t,
                ]
            });
        assert_eq!(
            alleles, expected.bases[position],
            "{context}: alleles at {position}"
        );
    }
    // Every junction reaching into the prepared interval, including one whose start lies left
    // of it: all its supporting reads span the interval's first base and are resident.
    let junctions = root
        .junctions(prepared)
        .unwrap()
        .into_iter()
        .map(|junction| ((junction.start, junction.end), junction.depth))
        .collect::<Vec<_>>();
    let expected_junctions = expected
        .junctions
        .iter()
        .filter(|((start, end), _)| *start < prepared.end() && *end > prepared.start())
        .map(|(span, depth)| (*span, *depth))
        .collect::<Vec<_>>();
    assert_eq!(junctions, expected_junctions, "{context}: junctions");
    assert!(prepared.end() <= world.contig_length);
}

/// Builds semantic pages for random tiles, row blocks, families, levels of detail and styles.
/// Each build reserves from a tile-clipped count and fails (or trips the page builder's debug
/// assertion) if it then emits more than it reserved.
fn assert_pages_fit_their_reservations(
    world: &World,
    rng: &mut XorShift,
    evidence: &Arc<EvidenceRoot>,
    atlas: &Arc<AlignmentRowAtlas>,
    target: GenomicInterval,
    context: &str,
) {
    let tile_bases = world.tile_bases.get();
    let first_tile = target.start() / tile_bases;
    let tile_count = target.end().div_ceil(tile_bases) - first_tile;
    let rows = NonZeroU32::new(u32::try_from(rng.between(3, 40)).unwrap()).unwrap();
    let blocks = (atlas.row_count() as u64).div_ceil(u64::from(rows.get()));
    if blocks == 0 {
        return;
    }
    for _ in 0..40 {
        let lod = [
            GeometryLod::Overview,
            GeometryLod::Span,
            GeometryLod::Read,
            GeometryLod::Base,
        ][rng.below(4) as usize];
        let family = [
            AlignmentPageFamily::ReadGeometry,
            AlignmentPageFamily::Labels,
            AlignmentPageFamily::SpecialistOverlay,
        ][rng.below(3) as usize];
        let presentation = PresentationContract {
            geometry_lod: lod,
            style: AlignmentStyleContract {
                base_display: if rng.chance(1, 2) {
                    ReadBaseDisplayMode::All
                } else {
                    ReadBaseDisplayMode::Mismatches
                },
                read_labels: AlignmentReadLabelMode::ReadName,
                minimum_event_label_size: u32::try_from(rng.between(1, 4)).unwrap(),
                modified_base_color_mode: if rng.chance(1, 2) {
                    ModifiedBaseColorMode::ModifiedVsUnmodified
                } else {
                    ModifiedBaseColorMode::ByType
                },
                ..AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let tile = CanonicalGenomicTile::new(
            ContigId::new(0),
            first_tile + rng.below(tile_count),
            world.tile_bases,
        );
        let block = CanonicalRowBlock::new(u32::try_from(rng.below(blocks)).unwrap(), rows);
        let key = AlignmentPageKey::new(
            evidence.source_revision(),
            evidence.revision(),
            atlas.revision(),
            atlas.layout_contract().fingerprint(),
            presentation.page_fingerprint(family),
            tile,
            block,
            family,
            lod,
        );
        let page = AlignmentSemanticPage::build(
            key,
            evidence,
            atlas,
            presentation,
            world.contig_length,
            None,
            &world.ledger,
            ReservationKind::Normal,
            7,
        );
        assert!(
            page.is_ok(),
            "{context}: {family:?} {lod:?} page for {tile:?} {block:?} failed: {:?}",
            page.err()
        );
    }
}

/// Admits the case's pieces one at a time through the store, publishing after each, and hands
/// every published authority to `check`.
fn drive(
    world: &World,
    layout: LayoutContract,
    policy: OverlappingMatePolicy,
    mut check: impl FnMut(
        &str,
        GenomicInterval,
        bool,
        &Arc<EvidenceRoot>,
        &Arc<ExactSummaryRoot>,
        &Arc<AlignmentRowAtlas>,
    ),
) {
    let mut store = world.store();
    let mut extended_left = false;
    for (step, (piece, target, left)) in world.steps.iter().copied().enumerate() {
        let context = format!(
            "seed {} step {step} target {}..{} ({:?})",
            world.seed,
            target.start(),
            target.end(),
            layout.stacking
        );
        extended_left |= left;
        store.admit(world.acquisition(piece)).unwrap();
        let evidence = store
            .prepare(target, payload_contract(), ReservationKind::Normal)
            .unwrap_or_else(|error| panic!("{context}: evidence: {error}"));
        let summaries = store
            .prepare_exact_summaries(
                evidence.root(),
                target,
                world.tile_bases,
                world.contig_length,
                summary_contract(policy),
                None,
                ReservationKind::Normal,
            )
            .unwrap_or_else(|error| panic!("{context}: summaries: {error}"));
        let rows = store
            .prepare_row_atlas(evidence.root(), target, layout, ReservationKind::Normal)
            .unwrap_or_else(|error| panic!("{context}: row atlas: {error}"));
        let (evidence, summaries, rows) = store
            .publish_complete_authority(evidence, summaries, rows)
            .unwrap();
        check(
            &context,
            target,
            extended_left,
            &evidence,
            &summaries,
            &rows,
        );
        // Pressure between steps must never cost the published locus its cache entries.
        store.evict_inactive_until(0).unwrap();
    }
    drop(store);
    let snapshot = world.ledger.snapshot().unwrap();
    assert_eq!(snapshot.reserved, 0, "seed {}", world.seed);
    assert_eq!(snapshot.transient, 0, "seed {}", world.seed);
}

#[test]
fn extended_evidence_and_rows_equal_a_clean_build() {
    for seed in 0..CASES {
        let world = World::new(seed);
        let mut rng = XorShift::new(seed ^ 0xa11);
        for stacking in [ReadStackingMode::Compact, ReadStackingMode::Stable] {
            drive(
                &world,
                layout(stacking, ReadSampling::Complete),
                OverlappingMatePolicy::CountBoth,
                |context, target, extended_left, evidence, _, atlas| {
                    let clean = world.clean_evidence(target);
                    assert_eq!(evidence.record_count(), clean.record_count(), "{context}");
                    assert_eq!(
                        evidence.record_count(),
                        world.overlapping(target).count(),
                        "{context}"
                    );
                    let mut probes = vec![target];
                    for _ in 0..4 {
                        let start = rng.between(target.start(), target.end() - 1);
                        probes.push(interval(start, rng.between(start + 1, target.end())));
                    }
                    for probe in probes {
                        let handles = |root: &Arc<EvidenceRoot>| {
                            root.select(probe)
                                .unwrap()
                                .records()
                                .iter()
                                .map(|record| record.handle)
                                .collect::<Vec<_>>()
                        };
                        let mut expected = world
                            .overlapping(probe)
                            .map(|index| {
                                let record = &world.records[index];
                                (record.start, record.end, handle(index))
                            })
                            .collect::<Vec<_>>();
                        expected.sort_unstable();
                        let expected = expected
                            .into_iter()
                            .map(|(_, _, handle)| handle)
                            .collect::<Vec<_>>();
                        assert_eq!(handles(evidence), expected, "{context}: {probe:?}");
                        assert_eq!(handles(&clean), expected, "{context}: clean {probe:?}");
                    }

                    let clean_atlas = AlignmentRowAtlas::build(
                        RowAtlasRevision::new(RevisionFingerprint::new(90, 91)),
                        &clean,
                        target,
                        atlas.layout_contract(),
                        &world.ledger,
                        ReservationKind::Normal,
                        9,
                    )
                    .unwrap();
                    let extended_rows = atlas_rows(atlas, target, context);
                    let clean_rows = atlas_rows(&clean_atlas, target, context);
                    assert_eq!(extended_rows.len(), evidence.record_count(), "{context}");
                    // Same records with the same extents always. A stable atlas freezes
                    // incumbent rows when it grows leftwards, so only then may rows differ.
                    let extents = |rows: &BTreeMap<AlignmentHandle, (u32, u64, u64)>| {
                        rows.iter()
                            .map(|(handle, (_, start, end))| (*handle, *start, *end))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(extents(&extended_rows), extents(&clean_rows), "{context}");
                    if stacking == ReadStackingMode::Compact || !extended_left {
                        assert_eq!(extended_rows, clean_rows, "{context}: row assignment");
                    }
                },
            );
        }
    }
}

#[test]
fn per_tile_summaries_sum_to_a_whole_interval_pileup_at_every_seam() {
    for seed in 0..CASES {
        let world = World::new(seed);
        let expected = pileup(&world);
        drive(
            &world,
            layout(ReadStackingMode::Compact, ReadSampling::Complete),
            OverlappingMatePolicy::CountBoth,
            |context, _, _, _, summaries, _| {
                assert_summaries_match_pileup(&world, &expected, summaries, context);
            },
        );
    }
}

#[test]
fn count_once_depth_does_not_depend_on_where_tile_seams_fall() {
    for seed in 0..CASES {
        let world = World::new(seed);
        let whole = interval(0, world.contig_length);
        let evidence = world.clean_evidence(whole);
        let depth = |tile_bases: u64| {
            let tile_bases = NonZeroU64::new(tile_bases).unwrap();
            let tiles = CanonicalGenomicTile::covering(whole, tile_bases, world.contig_length)
                .unwrap()
                .into_iter()
                .map(|tile| {
                    ExactDepthSummaryTile::build(
                        &evidence,
                        tile,
                        world.contig_length,
                        DepthSummaryContract::new(OverlappingMatePolicy::CountOnce),
                        &world.ledger,
                        ReservationKind::Normal,
                        5,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            tile_depths(&tiles, whole)
        };
        let unbroken = depth(world.contig_length);
        for tile_bases in [world.tile_bases.get(), 37, 1] {
            assert_eq!(
                depth(tile_bases),
                unbroken,
                "seed {seed}: count-once depth with {tile_bases}-base tiles"
            );
        }
    }
}

#[test]
fn every_tile_and_page_fits_the_reservation_taken_for_it() {
    for seed in 0..CASES {
        let world = World::new(seed);
        let mut rng = XorShift::new(seed ^ 0xc0de);
        // Summary tiles (depth, alleles, junctions, pairs, filters, modifications) are built
        // inside `drive`; each registers its retained bytes against its own reservation.
        drive(
            &world,
            layout(ReadStackingMode::Stable, ReadSampling::Complete),
            OverlappingMatePolicy::CountOnce,
            |context, target, _, evidence, summaries, atlas| {
                assert!(!summaries.modification_tiles().is_empty(), "{context}");
                assert_pages_fit_their_reservations(
                    &world, &mut rng, evidence, atlas, target, context,
                );
            },
        );
    }
}

#[test]
fn downsampled_extension_never_shows_half_a_pair() {
    let mut pairs_seen = 0_usize;
    let mut declined = 0_usize;
    for seed in 0..CASES {
        let world = World::new(seed);
        let mut templates = HashMap::<&[u8], Vec<usize>>::new();
        for (index, record) in world.records.iter().enumerate() {
            templates
                .entry(record.read_name.as_slice())
                .or_default()
                .push(index);
        }
        let sampling = ReadSampling::Downsampled {
            window_bases: NonZeroU32::new(100).unwrap(),
            maximum_reads: NonZeroU32::new(2).unwrap(),
        };
        for stacking in [ReadStackingMode::Compact, ReadStackingMode::Stable] {
            drive(
                &world,
                layout(stacking, sampling),
                OverlappingMatePolicy::CountBoth,
                |context, target, _, _, _, atlas| {
                    let rows = atlas_rows(atlas, target, context);
                    // Sampling never changes the eligible population it reports.
                    let eligible = world.overlapping(target).count();
                    assert!(rows.len() <= eligible, "{context}");
                    if let Some((shown, considered)) = atlas.downsampled_read_counts() {
                        assert_eq!((shown, considered), (rows.len(), eligible), "{context}");
                    } else {
                        assert_eq!(rows.len(), eligible, "{context}");
                    }
                    for mates in templates.values().filter(|mates| mates.len() == 2) {
                        let inside = |index: usize| {
                            let record = &world.records[index];
                            record.start < target.end() && record.end > target.start()
                        };
                        if !inside(mates[0]) || !inside(mates[1]) {
                            continue;
                        }
                        pairs_seen += 1;
                        let shown = |index: usize| rows.contains_key(&handle(index));
                        declined += usize::from(!shown(mates[0]));
                        assert_eq!(
                            shown(mates[0]),
                            shown(mates[1]),
                            "{context}: template {:?} shows one mate without the other",
                            String::from_utf8_lossy(&world.records[mates[0]].read_name)
                        );
                    }
                },
            );
        }
    }
    // The property is vacuous unless sampling both kept and declined whole pairs.
    assert!(declined > 0 && declined < pairs_seen);
}
