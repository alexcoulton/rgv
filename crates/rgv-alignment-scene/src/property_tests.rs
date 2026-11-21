//! Seeded, dependency-free property harness for the alignment scene.
//!
//! Every case derives random reads (plain, indel-bearing, spliced, long, paired, modified) and a
//! random tile size, acquisition partition and extension order from one seed, drives them
//! through the real [`EvidenceStore`] path, and checks three families of invariants:
//!
//! * an extended evidence root and row atlas describe the same records as a clean build;
//! * per-tile exact summaries add up to a brute-force whole-interval pileup, including at
//!   every tile seam, both counting overlapping mates twice and collapsing them onto what the
//!   earlier mate actually contributed (its CIGAR and base-quality eligibility);
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
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId,
    GenomicInterval, ModifiedBaseColorMode, OverlappingMatePolicy, ReadBaseDisplayMode,
    ReadStackingMode,
};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, ReferenceSlice,
    SequenceData, TemplateLengthBounds,
};

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentPageKey, AlignmentPresentationStore,
    AlignmentReadLabelMode, AlignmentRowAtlas, AlignmentSemanticPage, AlignmentSourceRevision,
    AlignmentStyleContract, AlleleSummaryContract, AlleleSummaryDetail, AllocationClass,
    AllocationLedger, CanonicalGenomicTile, CanonicalRecordChunk, CanonicalRowBlock,
    CapabilityRequirement, CompleteCoverage, ContractFingerprint, DepthSummaryContract,
    EvidenceAcquisition, EvidenceChunk, EvidenceRoot, EvidenceStore, ExactAlleleSummaryTile,
    ExactDepthSummaryTile, ExactSummaryRoot, FilteredRecordProvenance, GeometryLod, LayoutContract,
    PayloadContract, PayloadSidecar, PresentationContract, ReadSampling, ReferenceRevision,
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
    drive_steps(world, layout, policy, &mut check);
    let snapshot = world.ledger.snapshot().unwrap();
    assert_eq!(snapshot.reserved, 0, "seed {}", world.seed);
    assert_eq!(snapshot.transient, 0, "seed {}", world.seed);
}

/// What a driver hands each step: context, target, whether the target ever grew leftwards, and
/// the published evidence, summaries and row atlas.
type StepCheck<'a> = dyn FnMut(
        &str,
        GenomicInterval,
        bool,
        &Arc<EvidenceRoot>,
        &Arc<ExactSummaryRoot>,
        &Arc<AlignmentRowAtlas>,
    ) + 'a;

/// [`drive`] without the closing ledger check, for callers which still hold pages or scenes.
fn drive_steps(
    world: &World,
    layout: LayoutContract,
    policy: OverlappingMatePolicy,
    check: &mut StepCheck<'_>,
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

/// What one read says about one reference position.
#[derive(Clone, Copy)]
enum ReadSite {
    /// An aligned base and its quality.
    Base(u8, u8),
    Deleted,
}

/// A read flattened to per-position statements plus the reference positions that follow each
/// of its insertions. Reference skips say nothing about the positions they jump.
struct FlatRead {
    sites: HashMap<u64, ReadSite>,
    insertions: Vec<u64>,
}

fn flatten(record: &AlignmentRecord) -> FlatRead {
    let SequenceData::Present {
        bases,
        qualities: Some(qualities),
    } = &record.sequence
    else {
        unreachable!("generated reads carry a sequence and qualities");
    };
    let mut flat = FlatRead {
        sites: HashMap::new(),
        insertions: Vec::new(),
    };
    let mut reference = record.start;
    let mut read = 0_usize;
    for operation in &record.cigar {
        let len = u64::from(operation.len);
        match operation.kind {
            kind if is_aligned(kind) => {
                for offset in 0..operation.len as usize {
                    flat.sites.insert(
                        reference + offset as u64,
                        ReadSite::Base(bases[read + offset], qualities[read + offset]),
                    );
                }
                reference += len;
                read += operation.len as usize;
            }
            CigarOpKind::Deletion => {
                for position in reference..reference + len {
                    flat.sites.insert(position, ReadSite::Deleted);
                }
                reference += len;
            }
            CigarOpKind::ReferenceSkip => reference += len,
            CigarOpKind::Insertion => {
                flat.insertions.push(reference);
                read += operation.len as usize;
            }
            CigarOpKind::SoftClip => read += operation.len as usize,
            _ => {}
        }
    }
    flat
}

/// Per-base `[A, C, G, T, deletion, insertion]` and aligned depth with overlapping mates
/// counted once.
struct CountOncePileup {
    depth: Vec<u32>,
    alleles: Vec<[u32; 6]>,
}

/// Brute-force count-once pileup, position by position, with no interval arithmetic.
///
/// The earlier mate (lower start; on a tie, the one stored first) is counted in full. The later
/// mate then contributes, at each position:
/// * depth, unless the earlier mate has an aligned base there;
/// * its base (if it passes `minimum_base_quality`), unless the earlier mate has a passing base
///   or a deletion there;
/// * a deletion or an insertion, unless the earlier mate has a base or a deletion there.
fn count_once_pileup(world: &World, minimum_base_quality: u8) -> CountOncePileup {
    let length = usize::try_from(world.contig_length).unwrap();
    let mut pileup = CountOncePileup {
        depth: vec![0; length],
        alleles: vec![[0; 6]; length],
    };
    let flat = world.records.iter().map(flatten).collect::<Vec<_>>();
    let mut templates = HashMap::<&[u8], Vec<usize>>::new();
    for (index, record) in world.records.iter().enumerate() {
        templates.entry(&record.read_name).or_default().push(index);
    }
    let base_slot = |base: u8| b"ACGT".iter().position(|known| *known == base).unwrap();
    for (index, read) in flat.iter().enumerate() {
        let record = &world.records[index];
        let earlier = templates[record.read_name.as_slice()]
            .iter()
            .copied()
            .find(|other| *other != index)
            .filter(|other| {
                (world.records[*other].start, *other) < (record.start, index)
                    && record.flags.is_paired()
            })
            .map(|other| &flat[other]);
        let earlier_site = |position: u64| earlier.and_then(|mate| mate.sites.get(&position));
        for (position, site) in &read.sites {
            let slot = usize::try_from(*position).unwrap();
            match (site, earlier_site(*position)) {
                (ReadSite::Base(base, quality), mate) => {
                    if !matches!(mate, Some(ReadSite::Base(..))) {
                        pileup.depth[slot] += 1;
                    }
                    let mate_observed = match mate {
                        Some(ReadSite::Base(_, mate_quality)) => {
                            *mate_quality >= minimum_base_quality
                        }
                        Some(ReadSite::Deleted) => true,
                        None => false,
                    };
                    if *quality >= minimum_base_quality && !mate_observed {
                        pileup.alleles[slot][base_slot(*base)] += 1;
                    }
                }
                (ReadSite::Deleted, None) => pileup.alleles[slot][4] += 1,
                (ReadSite::Deleted, Some(_)) => {}
            }
        }
        for position in &read.insertions {
            if earlier_site(*position).is_none() {
                // An insertion belongs to the reference base on its left.
                pileup.alleles[usize::try_from(*position - 1).unwrap()][5] += 1;
            }
        }
    }
    pileup
}

#[test]
#[allow(clippy::too_many_lines)]
fn count_once_summaries_match_a_brute_force_mate_aware_pileup() {
    const MINIMUM_BASE_QUALITY: u8 = 20;
    let mut overlapping_pairs_with_indels = 0_usize;
    let mut positions_the_legacy_mask_got_wrong = 0_usize;
    for seed in 0..CASES {
        let world = World::new(seed);
        let whole = interval(0, world.contig_length);
        let evidence = world.clean_evidence(whole);
        let expected = count_once_pileup(&world, MINIMUM_BASE_QUALITY);

        // Prove the generator exercises the cases this test exists for: an overlapping pair
        // whose earlier mate has a deletion, skip or low-quality base inside the overlap.
        let mut templates = HashMap::<&[u8], Vec<&AlignmentRecord>>::new();
        for record in &world.records {
            templates.entry(&record.read_name).or_default().push(record);
        }
        for mates in templates.values().filter(|mates| mates.len() == 2) {
            let (earlier, later) = if mates[0].start <= mates[1].start {
                (mates[0], mates[1])
            } else {
                (mates[1], mates[0])
            };
            if later.start >= earlier.end {
                continue;
            }
            let earlier_flat = flatten(earlier);
            let later_flat = flatten(later);
            let mut differs = 0_usize;
            for position in later.start..earlier.end.min(later.end) {
                let later_base =
                    matches!(later_flat.sites.get(&position), Some(ReadSite::Base(..)));
                let earlier_counted =
                    matches!(
                        earlier_flat.sites.get(&position),
                        Some(ReadSite::Base(_, quality)) if *quality >= MINIMUM_BASE_QUALITY
                    ) || matches!(earlier_flat.sites.get(&position), Some(ReadSite::Deleted));
                differs += usize::from(later_base && !earlier_counted);
            }
            positions_the_legacy_mask_got_wrong += differs;
            overlapping_pairs_with_indels +=
                usize::from(earlier.cigar.iter().chain(&later.cigar).any(|operation| {
                    matches!(
                        operation.kind,
                        CigarOpKind::Deletion | CigarOpKind::Insertion | CigarOpKind::ReferenceSkip
                    )
                }));
        }

        for tile_bases in [world.contig_length, world.tile_bases.get(), 37] {
            let context = format!("seed {seed}, {tile_bases}-base tiles");
            let tiles = CanonicalGenomicTile::covering(
                whole,
                NonZeroU64::new(tile_bases).unwrap(),
                world.contig_length,
            )
            .unwrap();
            let depth_tiles = tiles
                .iter()
                .map(|tile| {
                    ExactDepthSummaryTile::build(
                        &evidence,
                        *tile,
                        world.contig_length,
                        DepthSummaryContract::new(OverlappingMatePolicy::CountOnce),
                        &world.ledger,
                        ReservationKind::Normal,
                        5,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                tile_depths(&depth_tiles, whole),
                expected.depth,
                "{context}"
            );
            for tile in tiles {
                let alleles = ExactAlleleSummaryTile::build(
                    &evidence,
                    tile,
                    world.contig_length,
                    AlleleSummaryContract::new(
                        OverlappingMatePolicy::CountOnce,
                        MINIMUM_BASE_QUALITY,
                        AlleleSummaryDetail::Counts,
                    ),
                    &world.ledger,
                    ReservationKind::Normal,
                    6,
                )
                .unwrap();
                let tile_interval = tile.interval(world.contig_length).unwrap();
                for position in tile_interval.start()..tile_interval.end() {
                    let site = alleles.site_at(position).unwrap().alleles;
                    assert_eq!(
                        [
                            site.a,
                            site.c,
                            site.g,
                            site.t,
                            site.deletion,
                            site.insertion
                        ],
                        expected.alleles[usize::try_from(position).unwrap()],
                        "{context}: alleles at {position}"
                    );
                }
            }
        }
    }
    assert!(
        overlapping_pairs_with_indels >= 20,
        "only {overlapping_pairs_with_indels} overlapping pairs carried an indel or skip"
    );
    assert!(
        positions_the_legacy_mask_got_wrong >= 100,
        "only {positions_the_legacy_mask_got_wrong} positions distinguish the mate-aware mask"
    );
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

// ---------------------------------------------------------------------------------------------
// Read-page reuse across extension (SCENE-3)
// ---------------------------------------------------------------------------------------------

fn reference_bases(world: &World) -> Vec<u8> {
    let mut rng = XorShift::new(world.seed ^ 0x5eed_ba5e);
    (0..world.contig_length)
        .map(|_| b"ACGT"[rng.below(4) as usize])
        .collect()
}

fn reference_slice(bases: &[u8], covered: GenomicInterval) -> ReferenceSlice {
    ReferenceSlice {
        interval: covered,
        bases: bases[covered.start() as usize..covered.end() as usize].to_vec(),
    }
}

/// A random presentation: level of detail, colour mode (including every pair mode, whose body
/// token is derived per record), labels, modified bases, and whether a reference is named.
fn random_presentation(rng: &mut XorShift) -> PresentationContract {
    let lod = [
        GeometryLod::Overview,
        GeometryLod::Span,
        GeometryLod::Read,
        GeometryLod::Base,
    ][rng.below(4) as usize];
    let color_mode = [
        AlignmentColorMode::Strand,
        AlignmentColorMode::MappingQuality,
        AlignmentColorMode::Haplotype,
        AlignmentColorMode::TemplateLength,
        AlignmentColorMode::PairOrientation,
        AlignmentColorMode::UnexpectedPair,
        AlignmentColorMode::FirstOfPairStrand,
        AlignmentColorMode::Bisulfite(rgv_core::BisulfiteContext::Cg),
        AlignmentColorMode::Bisulfite(rgv_core::BisulfiteContext::Chh),
    ][rng.below(9) as usize];
    let reads_reference =
        lod == GeometryLod::Base || matches!(color_mode, AlignmentColorMode::Bisulfite(_));
    PresentationContract {
        geometry_lod: lod,
        style: AlignmentStyleContract {
            color_mode,
            base_display: if rng.chance(1, 2) {
                ReadBaseDisplayMode::All
            } else {
                ReadBaseDisplayMode::Mismatches
            },
            read_labels: if rng.chance(1, 2) {
                AlignmentReadLabelMode::ReadName
            } else {
                AlignmentReadLabelMode::Hidden
            },
            show_modified_bases: rng.chance(1, 2),
            ..AlignmentStyleContract::default()
        },
        reference: (reads_reference && rng.chance(4, 5))
            .then(|| ReferenceRevision::new(RevisionFingerprint::new(70, 71))),
        annotation: None,
    }
}

/// Everything that can make two builds of one page differ, derived from the generated world
/// rather than from the content token: the clamped row range, which generated record sits in
/// which row, the connectors crossing the page, and the reference bases the family reads.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PageManifest {
    rows: std::ops::Range<u32>,
    records: Vec<(usize, u32)>,
    connectors: Vec<(u64, u32, u64, u64)>,
    reference: Option<Vec<(u64, u8)>>,
}

fn page_manifest(
    world: &World,
    atlas: &AlignmentRowAtlas,
    presentation: PresentationContract,
    page: &AlignmentSemanticPage,
    reference: Option<&ReferenceSlice>,
) -> PageManifest {
    let key = page.key();
    let tile = page.interval();
    let rows = key.row_block().range(atlas.row_count()).unwrap();
    let mut records = world
        .overlapping(tile)
        .filter_map(|index| {
            let row = atlas.record(handle(index))?.record.row();
            rows.contains(&row).then_some((index, row))
        })
        .collect::<Vec<_>>();
    records.sort_unstable();
    let connectors = if key.family() == AlignmentPageFamily::ReadGeometry {
        atlas
            .connectors_for_rows(rows.clone(), tile)
            .into_iter()
            .map(|connector| {
                (
                    connector.template(),
                    connector.row(),
                    connector.start(),
                    connector.end(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let bisulfite = key.family() == AlignmentPageFamily::SpecialistOverlay
        && matches!(key.lod(), GeometryLod::Read | GeometryLod::Base)
        && matches!(
            presentation.style.color_mode,
            AlignmentColorMode::Bisulfite(_)
        );
    let base_cells =
        key.family() == AlignmentPageFamily::ReadGeometry && key.lod() == GeometryLod::Base;
    let reference = (bisulfite || base_cells).then(|| {
        let margin = if bisulfite { 8 } else { 0 };
        (tile.start().saturating_sub(margin)..tile.end() + margin)
            .filter_map(|position| Some((position, reference?.base_at(position)?)))
            .collect()
    });
    PageManifest {
        rows,
        records,
        connectors,
        reference,
    }
}

fn assert_pages_structurally_equal(
    resolved: &AlignmentSemanticPage,
    fresh: &AlignmentSemanticPage,
    context: &str,
) {
    let key = resolved.key();
    assert_eq!(resolved.identity(), fresh.identity(), "{context}: {key:?}");
    assert_eq!(resolved.interval(), fresh.interval(), "{context}: {key:?}");
    assert_eq!(
        resolved.row_range(),
        fresh.row_range(),
        "{context}: {key:?}"
    );
    assert_eq!(
        resolved.primitives(),
        fresh.primitives(),
        "{context}: primitives of {key:?}"
    );
    assert_eq!(
        resolved.labels(),
        fresh.labels(),
        "{context}: labels of {key:?}"
    );
    assert_eq!(
        resolved.label_anchors().collect::<Vec<_>>(),
        fresh.label_anchors().collect::<Vec<_>>(),
        "{context}: label anchors of {key:?}"
    );
    assert_eq!(
        resolved.drawable_primitive_count(),
        fresh.drawable_primitive_count(),
        "{context}: {key:?}"
    );
    // Picking walks the page's private row directory; probe it at every primitive.
    for primitive in resolved.primitives() {
        let row = resolved.row_range().start + primitive.row_offset;
        let position = resolved.interval().start() + primitive.start_offset;
        assert_eq!(
            resolved.pick(row, position),
            fresh.pick(row, position),
            "{context}: pick in {key:?}"
        );
    }
}

type Spatial = (CanonicalGenomicTile, CanonicalRowBlock, AlignmentPageFamily);

#[derive(Default)]
struct ReuseTally {
    pages: u64,
    reused_across_revisions: u64,
    changed_and_rebuilt: u64,
    unchanged_and_shared: u64,
    cached_before_prepare: u64,
}

/// Drives one world through one presentation store and checks, for every page of every
/// prepared scene, that it equals a page built from scratch for the same roots, and that a
/// page whose inputs changed since the previous step was not carried over.
#[allow(clippy::too_many_lines)]
fn check_page_reuse(
    world: &World,
    rng: &mut XorShift,
    layout: LayoutContract,
    window_tiles: Option<u64>,
    tally: &mut ReuseTally,
) -> crate::AlignmentReadPageReuseStats {
    let bases = reference_bases(world);
    let presentation = random_presentation(rng);
    let row_block_rows = NonZeroU32::new(u32::try_from(rng.between(3, 24)).unwrap()).unwrap();
    let mut pages = AlignmentPresentationStore::new(world.ledger.clone());
    let mut previous = HashMap::<Spatial, (PageManifest, Arc<AlignmentSemanticPage>)>::new();
    drive_steps(
        world,
        layout,
        OverlappingMatePolicy::CountBoth,
        &mut |context, target, _, evidence, summaries, atlas| {
            // A panning camera prepares a sliding window; otherwise the whole target.
            let prepared = window_tiles.map_or(target, |tiles| {
                let width = tiles * world.tile_bases.get();
                interval(
                    target.end().saturating_sub(width).max(target.start()),
                    target.end(),
                )
            });
            // Cameras are not tile aligned. Trimming the prepared interval leaves its edge
            // tiles overhanging the reference slice, which then fills in as the camera moves.
            let prepared = {
                let mut trim = || {
                    if rng.chance(1, 2) {
                        rng.below(prepared.len().min(world.tile_bases.get()) / 2)
                    } else {
                        0
                    }
                };
                let (left, right) = (trim(), trim());
                interval(prepared.start() + left, prepared.end() - right)
            };
            // The slice covers every summary tile of the target plus the allele lane's margin.
            let slice = presentation.reference.map(|_| {
                reference_slice(
                    &bases,
                    interval(
                        target.start().saturating_sub(4),
                        (target.end() + 4).min(world.contig_length),
                    ),
                )
            });
            // Vertical warm-up may run against a narrower slice than the foreground gets. Pages
            // it leaves behind saw fewer reference bases and must not answer the request below.
            if let Some(full) = &slice
                && atlas.row_count() > 0
                && rng.chance(1, 2)
            {
                let trim = rng.between(1, prepared.len().min(world.tile_bases.get()) / 2 + 1);
                let narrow = interval(
                    (full.interval.start() + trim).min(full.interval.end() - 1),
                    full.interval
                        .end()
                        .saturating_sub(trim)
                        .max(full.interval.start() + trim + 1)
                        .min(full.interval.end()),
                );
                let blocks = (atlas.row_count() as u64).div_ceil(u64::from(row_block_rows.get()));
                pages
                    .warm_read_row_block(
                        evidence,
                        atlas,
                        presentation,
                        prepared,
                        CanonicalRowBlock::new(
                            u32::try_from(rng.below(blocks)).unwrap(),
                            row_block_rows,
                        ),
                        world.contig_length,
                        world.tile_bases,
                        Some(&reference_slice(&bases, narrow)),
                        ReservationKind::Normal,
                    )
                    .unwrap_or_else(|error| panic!("{context}: warm: {error}"));
            }
            let reference = slice.as_ref();
            let rows = 0..u32::try_from(atlas.row_count()).unwrap();
            let block_count = u32::try_from(
                u64::from(rows.end)
                    .div_ceil(u64::from(row_block_rows.get()))
                    .max(1),
            )
            .unwrap();
            let blocks = (0..block_count)
                .map(|coordinate| CanonicalRowBlock::new(coordinate, row_block_rows))
                .collect::<Vec<_>>();
            let fully_cached = |pages: &AlignmentPresentationStore| {
                blocks.iter().all(|block| {
                    pages
                        .read_row_block_is_fully_cached(
                            evidence,
                            atlas,
                            presentation,
                            prepared,
                            *block,
                            world.contig_length,
                            world.tile_bases,
                        )
                        .unwrap()
                })
            };
            // Asked before these roots were ever prepared: true only if earlier pages still
            // describe them, in which case preparing must build nothing.
            let cached_before = presentation.reference.is_none() && fully_cached(&pages);
            let builds_before = pages.read_page_reuse_stats().builds;
            let candidate = pages
                .prepare(
                    evidence,
                    summaries,
                    atlas,
                    presentation,
                    prepared,
                    rows,
                    world.contig_length,
                    world.tile_bases,
                    row_block_rows,
                    reference,
                    ReservationKind::Normal,
                )
                .unwrap_or_else(|error| panic!("{context}: prepare: {error}"));
            for page in candidate.snapshot().reads().pages() {
                let key = page.key();
                let fresh = AlignmentSemanticPage::build(
                    AlignmentPageKey::new(
                        evidence.source_revision(),
                        evidence.revision(),
                        atlas.revision(),
                        atlas.layout_contract().fingerprint(),
                        presentation.page_fingerprint(key.family()),
                        key.genomic_tile(),
                        key.row_block(),
                        key.family(),
                        presentation.geometry_lod,
                    ),
                    evidence,
                    atlas,
                    presentation,
                    world.contig_length,
                    reference,
                    &world.ledger,
                    ReservationKind::Normal,
                    7,
                )
                .unwrap_or_else(|error| panic!("{context}: fresh {key:?}: {error}"));
                assert_pages_structurally_equal(page, &fresh, context);
                tally.pages += 1;
                let reused = key.evidence_revision() != evidence.revision()
                    || key.row_atlas_revision() != atlas.revision();
                tally.reused_across_revisions += u64::from(reused);

                let manifest = page_manifest(world, atlas, presentation, page, reference);
                let spatial = (key.genomic_tile(), key.row_block(), key.family());
                if let Some((before, incumbent)) = previous.get(&spatial) {
                    if *before != manifest {
                        assert!(
                            !Arc::ptr_eq(incumbent, page) && incumbent.key() != key,
                            "{context}: {key:?} changed ({before:?} -> {manifest:?}) but was reused"
                        );
                        tally.changed_and_rebuilt += 1;
                    } else if Arc::ptr_eq(incumbent, page) {
                        tally.unchanged_and_shared += 1;
                    }
                }
                previous.insert(spatial, (manifest, Arc::clone(page)));
            }
            if cached_before {
                assert_eq!(
                    pages.read_page_reuse_stats().builds,
                    builds_before,
                    "{context}: a fully cached scene built pages"
                );
                tally.cached_before_prepare += 1;
            }
            assert!(
                fully_cached(&pages),
                "{context}: prepared pages not resident"
            );
            // Asking again for the same roots builds nothing.
            let builds = pages.read_page_reuse_stats().builds;
            let again = pages
                .prepare(
                    evidence,
                    summaries,
                    atlas,
                    presentation,
                    prepared,
                    0..u32::try_from(atlas.row_count()).unwrap(),
                    world.contig_length,
                    world.tile_bases,
                    row_block_rows,
                    reference,
                    ReservationKind::Normal,
                )
                .unwrap();
            assert_eq!(pages.read_page_reuse_stats().builds, builds, "{context}");
            for (first, second) in candidate
                .snapshot()
                .reads()
                .pages()
                .iter()
                .zip(again.snapshot().reads().pages())
            {
                assert!(Arc::ptr_eq(first, second), "{context}");
            }
            drop(again);
            if rng.chance(1, 4) {
                // Unstaged pages are legal eviction victims; the directory must follow.
                drop(candidate);
                pages.remove_owner(crate::AlignmentPresentationOwner::default());
                pages
                    .evict_inactive_batch(0, rng.between(1, 12) as usize)
                    .unwrap();
            } else {
                pages.stage(candidate);
            }
        },
    );
    pages.read_page_reuse_stats()
}

fn pairs_layout() -> LayoutContract {
    LayoutContract {
        view_as_pairs: true,
        ..layout(ReadStackingMode::Compact, ReadSampling::Complete)
    }
}

#[test]
fn pages_reused_across_extension_equal_fresh_builds_and_changed_pages_rebuild() {
    let mut tally = ReuseTally::default();
    for seed in 0..CASES {
        let world = World::new(seed);
        let mut rng = XorShift::new(seed ^ 0x9a6e);
        for layout in [
            layout(ReadStackingMode::Compact, ReadSampling::Complete),
            layout(ReadStackingMode::Stable, ReadSampling::Complete),
            pairs_layout(),
        ] {
            for _ in 0..2 {
                check_page_reuse(&world, &mut rng, layout, None, &mut tally);
            }
        }
        let snapshot = world.ledger.snapshot().unwrap();
        assert_eq!(snapshot.reserved, 0, "seed {seed}");
        assert_eq!(snapshot.transient, 0, "seed {seed}");
    }
    eprintln!(
        "page reuse: {} pages checked, {} reused across revisions, {} changed and rebuilt, {} unchanged and shared",
        tally.pages,
        tally.reused_across_revisions,
        tally.changed_and_rebuilt,
        tally.unchanged_and_shared
    );
    // The properties above are vacuous unless both outcomes actually occur.
    assert!(tally.reused_across_revisions > 0, "no page was ever reused");
    assert!(
        tally.changed_and_rebuilt > 0,
        "no changed page was observed"
    );
    assert!(
        tally.unchanged_and_shared > 0,
        "no unchanged page was shared"
    );
    assert!(tally.pages > tally.reused_across_revisions);
    assert!(
        tally.cached_before_prepare > 0,
        "no extension was ever answered wholly from cache"
    );
}

/// Reads tiled along a long contig, admitted one tile at a time left to right: the workload of
/// a camera panning into uncached territory.
fn pan_world(seed: u64) -> World {
    let mut world = World::new(seed);
    let tile = world.tile_bases.get();
    let tiles = world.contig_length / tile;
    world.steps = (0..tiles)
        .map(|index| {
            let end = if index + 1 == tiles {
                world.contig_length
            } else {
                (index + 1) * tile
            };
            (interval(index * tile, end), interval(0, end), false)
        })
        .collect();
    world
}

#[test]
fn panning_by_extension_reuses_most_pages() {
    let (mut requests, mut reuses, mut builds) = (0_u64, 0_u64, 0_u64);
    for seed in 0..CASES / 2 {
        let world = pan_world(seed);
        let mut rng = XorShift::new(seed ^ 0x9a17);
        for stacking in [ReadStackingMode::Compact, ReadStackingMode::Stable] {
            let mut tally = ReuseTally::default();
            let stats = check_page_reuse(
                &world,
                &mut rng,
                layout(stacking, ReadSampling::Complete),
                Some(6),
                &mut tally,
            );
            // `check_page_reuse` asks for every scene twice; the repeat is all same-revision.
            requests += stats.cross_revision_reuses + stats.builds;
            reuses += stats.cross_revision_reuses;
            builds += stats.builds;
        }
    }
    eprintln!(
        "pan workload: {reuses} of {requests} first-time page requests reused, {builds} built"
    );
    assert!(reuses > 0, "panning never reused a page");
    // Before content identity every one of these requests built a page. Now a pan step
    // rebuilds only the pages its new reads (or a grown last row block) actually touch.
    assert!(
        reuses * 2 > requests,
        "panning reused only {reuses} of {requests} first-time page requests ({builds} builds)"
    );
}

// ---------------------------------------------------------------------------------------------
// Incremental row atlas (SCENE-4 / SCENE-7)
// ---------------------------------------------------------------------------------------------

/// A random sorted and/or grouped, unpaired layout: every sort key and a spread of group keys,
/// either direction of each, with and without a position anchor and a promoted template.
fn random_ordered_layout(rng: &mut XorShift, world: &World) -> LayoutContract {
    let sorting = [
        AlignmentSortMode::Start,
        AlignmentSortMode::Strand,
        AlignmentSortMode::FirstOfPairStrand,
        AlignmentSortMode::MappingQuality,
        AlignmentSortMode::ReadName,
        AlignmentSortMode::Haplotype,
        AlignmentSortMode::AlignedLength,
        AlignmentSortMode::LeftClip,
        AlignmentSortMode::RightClip,
        AlignmentSortMode::TemplateLength,
        AlignmentSortMode::PairOrientation,
        AlignmentSortMode::MateContig,
        AlignmentSortMode::PairStatus,
        AlignmentSortMode::ReadOrder,
        AlignmentSortMode::Base,
        AlignmentSortMode::Tag(*b"XS"),
    ][rng.below(16) as usize];
    let grouping = [
        AlignmentGroupMode::None,
        AlignmentGroupMode::None,
        AlignmentGroupMode::None,
        AlignmentGroupMode::Strand,
        AlignmentGroupMode::Haplotype,
        AlignmentGroupMode::ReadOrder,
        AlignmentGroupMode::FirstOfPairStrand,
        AlignmentGroupMode::LeftClip,
        AlignmentGroupMode::PairOrientation,
        AlignmentGroupMode::Base,
        AlignmentGroupMode::Insertion,
        AlignmentGroupMode::Tag(*b"XS"),
    ][rng.below(12) as usize];
    let direction = |rng: &mut XorShift| {
        if rng.chance(1, 2) {
            AlignmentOrderDirection::Natural
        } else {
            AlignmentOrderDirection::Reverse
        }
    };
    let needs_anchor = sorting == AlignmentSortMode::Base
        || matches!(
            grouping,
            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
        );
    LayoutContract {
        stacking: if rng.chance(1, 2) {
            ReadStackingMode::Compact
        } else {
            ReadStackingMode::Stable
        },
        grouping,
        group_order: direction(rng),
        sorting,
        sort_order: direction(rng),
        sort_anchor: (needs_anchor || rng.chance(1, 5)).then(|| rng.below(world.contig_length)),
        view_as_pairs: false,
        pair_length_bounds: TemplateLengthBounds::default(),
        promoted_template: rng.chance(1, 4).then(|| {
            let record = &world.records[rng.below(world.records.len() as u64) as usize];
            rgv_hts::template_id(&record.read_name).get()
        }),
        sampling: ReadSampling::Complete,
    }
}

/// Camera-activity answers of an atlas at `probe`, both from a fresh cursor and from a cursor
/// walked there from `via`.
fn activity_answers(
    atlas: &AlignmentRowAtlas,
    via: GenomicInterval,
    probe: GenomicInterval,
) -> [(
    usize,
    usize,
    usize,
    usize,
    Vec<crate::AlignmentRowActivityBin>,
); 2] {
    let bins = std::num::NonZeroUsize::new(7).unwrap();
    let describe = |profile: crate::AlignmentRowActivityProfile| {
        (
            profile.active_row_end(),
            profile.occupied_rows(),
            profile.visible_record_count(),
            profile.visible_connector_count(),
            profile.bins().to_vec(),
        )
    };
    let fresh = describe(atlas.activity_profile(probe, bins).unwrap());
    let mut cursor = atlas.activity_cursor(via).unwrap();
    assert!(cursor.update(probe));
    [fresh, describe(cursor.profile(bins))]
}

#[derive(Default)]
struct AtlasTally {
    ordered_extensions_in_place: u64,
    ordered_extensions_reshuffled: u64,
    rebuilt: u64,
    compactions: u64,
}

/// Drives one layout and checks every published atlas against a clean build, the stability of
/// rows inside one row-coordinate space, and the work each extension reports.
#[allow(clippy::too_many_lines)]
fn check_atlas_extension(
    world: &World,
    rng: &mut XorShift,
    layout: LayoutContract,
    tally: &mut AtlasTally,
) {
    let default_order = layout.grouping == AlignmentGroupMode::None
        && layout.sorting == AlignmentSortMode::Start
        && layout.sort_order == AlignmentOrderDirection::Natural
        && layout.group_order == AlignmentOrderDirection::Natural
        && layout.sort_anchor.is_none()
        && layout.promoted_template.is_none()
        && !layout.view_as_pairs;
    let mut step = 0_usize;
    let mut previous = None::<(
        crate::RowCoordinateSpaceRevision,
        BTreeMap<AlignmentHandle, (u32, u64, u64)>,
    )>;
    drive(
        world,
        layout,
        OverlappingMatePolicy::CountBoth,
        |context, target, extended_left, evidence, _, atlas| {
            let context = &format!("{context} {layout:?}");
            let piece = world.steps[step].0;
            step += 1;
            let rows = atlas_rows(atlas, target, context);
            let work = atlas.construction_work();

            // Rows never move inside one row-coordinate space, and a root which reports no
            // reassignment stays in its incumbent's space.
            if let Some((space, incumbent_rows)) = &previous {
                if *space == atlas.row_coordinate_space() {
                    assert!(!work.rows_reassigned, "{context}");
                    for (handle, assignment) in incumbent_rows {
                        assert_eq!(rows.get(handle), Some(assignment), "{context}: {handle:?}");
                    }
                    if !default_order {
                        tally.ordered_extensions_in_place += 1;
                    }
                } else {
                    assert!(work.rows_reassigned, "{context}");
                    // A reshuffle rewrites the incumbent layers into one; a rebuild from
                    // evidence compacts nothing.
                    if work.layers_compacted > 0 {
                        assert!(!default_order, "{context}");
                        tally.ordered_extensions_reshuffled += 1;
                    } else {
                        tally.rebuilt += 1;
                    }
                }
                // An extension that reassigned nothing visited only the strip it admitted, one
                // priority read per row when ordered, and whatever compaction merged.
                if !work.rows_reassigned {
                    let strip = world.overlapping(piece).count() as u64;
                    let allowance = strip * 4 + atlas.row_count() as u64;
                    if work.layers_compacted == 0 {
                        assert!(
                            work.records_visited <= allowance,
                            "{context}: visited {} for a strip of {strip}",
                            work.records_visited
                        );
                    }
                    tally.compactions += u64::from(work.layers_compacted);
                }
            }
            previous = Some((atlas.row_coordinate_space(), rows.clone()));

            // Equal to a clean build, except where stable stacking froze rows on a leftward
            // extension of the default order (the one documented divergence).
            let clean = world.clean_evidence(target);
            let clean_atlas = AlignmentRowAtlas::build(
                RowAtlasRevision::new(RevisionFingerprint::new(90, 92)),
                &clean,
                target,
                layout,
                &world.ledger,
                ReservationKind::Normal,
                9,
            )
            .unwrap();
            assert_eq!(atlas.record_count(), evidence.record_count(), "{context}");
            let frozen =
                default_order && layout.stacking == ReadStackingMode::Stable && extended_left;
            if frozen {
                return;
            }
            assert_eq!(
                rows,
                atlas_rows(&clean_atlas, target, context),
                "{context}: rows"
            );
            assert_eq!(atlas.row_count(), clean_atlas.row_count(), "{context}");
            assert_eq!(atlas.groups(), clean_atlas.groups(), "{context}: groups");
            for row in 0..u32::try_from(atlas.row_count()).unwrap() {
                assert_eq!(
                    atlas.row_extent(row),
                    clean_atlas.row_extent(row),
                    "{context}: row {row}"
                );
            }
            assert_eq!(
                atlas.assignment_fingerprint(),
                clean_atlas.assignment_fingerprint(),
                "{context}: fingerprint"
            );
            for _ in 0..3 {
                let mut random_interval = || {
                    let start = rng.between(target.start(), target.end() - 1);
                    interval(start, rng.between(start + 1, target.end()))
                };
                let (via, probe) = (random_interval(), random_interval());
                let extended = activity_answers(atlas, via, probe);
                let expected = activity_answers(&clean_atlas, via, probe);
                assert_eq!(extended[0], expected[0], "{context}: activity at {probe:?}");
                assert_eq!(
                    extended[1], extended[0],
                    "{context}: walked cursor at {probe:?}"
                );
                assert_eq!(
                    expected[1], expected[0],
                    "{context}: clean cursor at {probe:?}"
                );
                // And against the generated reads themselves.
                let visible = world
                    .overlapping(probe)
                    .map(|index| rows[&handle(index)].0)
                    .collect::<Vec<_>>();
                let mut occupied = visible.clone();
                occupied.sort_unstable();
                occupied.dedup();
                assert_eq!(
                    extended[0].2,
                    visible.len(),
                    "{context}: visible at {probe:?}"
                );
                assert_eq!(
                    extended[0].1,
                    occupied.len(),
                    "{context}: occupied at {probe:?}"
                );
                assert_eq!(
                    extended[0].0,
                    occupied.last().map_or(0, |row| *row as usize + 1),
                    "{context}: active row end at {probe:?}"
                );
            }
        },
    );
}

#[test]
fn extended_atlas_equals_a_clean_build_in_every_order_and_keeps_rows_within_a_space() {
    let mut tally = AtlasTally::default();
    for seed in 0..CASES {
        let world = World::new(seed);
        let mut rng = XorShift::new(seed ^ 0x000a_71a5);
        for stacking in [ReadStackingMode::Compact, ReadStackingMode::Stable] {
            check_atlas_extension(
                &world,
                &mut rng,
                layout(stacking, ReadSampling::Complete),
                &mut tally,
            );
        }
        for _ in 0..6 {
            let layout = random_ordered_layout(&mut rng, &world);
            check_atlas_extension(&world, &mut rng, layout, &mut tally);
        }
        // Rightward pans exercise chained in-place extension and compaction.
        let pan = pan_world(seed);
        for _ in 0..2 {
            let layout = random_ordered_layout(&mut rng, &pan);
            check_atlas_extension(&pan, &mut rng, layout, &mut tally);
        }
        check_atlas_extension(
            &pan,
            &mut rng,
            layout(ReadStackingMode::Compact, ReadSampling::Complete),
            &mut tally,
        );
    }
    eprintln!(
        "ordered extensions: {} in place, {} reshuffled; rebuilds {}; compactions {}",
        tally.ordered_extensions_in_place,
        tally.ordered_extensions_reshuffled,
        tally.rebuilt,
        tally.compactions
    );
    assert!(tally.ordered_extensions_in_place > 0);
    assert!(tally.ordered_extensions_reshuffled > 0);
    assert!(tally.compactions > 0);
}

/// Complexity regression, counted in record slots visited rather than wall time. A camera pans
/// rightwards over thousands of short reads, one tile per extension.
#[test]
#[allow(clippy::too_many_lines)]
fn row_atlas_extension_work_is_proportional_to_the_reads_it_admits() {
    let tile = 100_u64;
    let read_count = 3_000_usize;
    let contig_length = read_count as u64 * 10 + 100;
    let records = (0..read_count)
        .map(|index| AlignmentRecord {
            contig: ContigId::new(0),
            start: index as u64 * 10,
            end: index as u64 * 10 + 30,
            flags: AlignmentFlags::from_bits(if index % 3 == 0 { 0x10 } else { 0 }),
            mapping_quality: Some((index % 61) as u8),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: format!("read-{index:05}").into_bytes(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 30,
            }],
            sequence: SequenceData::Present {
                bases: vec![b'A'; 30],
                qualities: Some(vec![30; 30]),
            },
            auxiliary_fields: Vec::new(),
        })
        .collect::<Vec<_>>();
    let ledger = AllocationLedger::new(1 << 30, 0);
    let chunks = records
        .iter()
        .enumerate()
        .map(|(index, record)| chunk(&ledger, record, index))
        .collect();
    let tiles = contig_length / tile;
    let world = World {
        seed: 0,
        ledger,
        contig_length,
        tile_bases: NonZeroU64::new(tile).unwrap(),
        records,
        chunks,
        steps: (0..tiles)
            .map(|index| {
                let end = (index + 1) * tile;
                (interval(index * tile, end), interval(0, end), false)
            })
            .collect(),
    };

    for (label, layout, ordered) in [
        (
            "default",
            layout(ReadStackingMode::Compact, ReadSampling::Complete),
            false,
        ),
        (
            // Names rise with position, so every new row ranks after every incumbent row.
            "sorted by read name",
            LayoutContract {
                grouping: AlignmentGroupMode::None,
                sorting: AlignmentSortMode::ReadName,
                ..layout(ReadStackingMode::Compact, ReadSampling::Complete)
            },
            true,
        ),
    ] {
        let mut store = world.store();
        let (mut visited, mut strips, mut full_rebuilds) = (0_u64, 0_u64, 0_u64);
        let mut last = None;
        for (step, (piece, target, _)) in world.steps.iter().copied().enumerate() {
            store.admit(world.acquisition(piece)).unwrap();
            let evidence = store
                .prepare(target, payload_contract(), ReservationKind::Normal)
                .unwrap();
            let rows = store
                .prepare_row_atlas(evidence.root(), target, layout, ReservationKind::Normal)
                .unwrap();
            let atlas = Arc::clone(rows.root());
            let work = atlas.construction_work();
            let strip = world.overlapping(piece).count() as u64;
            if step > 0 {
                assert!(
                    !work.rows_reassigned,
                    "{label}: step {step} reassigned rows"
                );
                visited += work.records_visited;
                strips += strip;
                full_rebuilds += atlas.record_count() as u64;
                if work.layers_compacted == 0 {
                    let allowance = strip * 3 + if ordered { atlas.row_count() as u64 } else { 0 };
                    assert!(
                        work.records_visited <= allowance,
                        "{label}: step {step} visited {} for a strip of {strip}",
                        work.records_visited
                    );
                }
            }
            // Layers stay logarithmic, and superseded atlases leave the cache.
            assert!(
                atlas.layer_count() <= 4 + 12,
                "{label}: {} layers",
                atlas.layer_count()
            );
            assert_eq!(store.row_atlas_count(), 1, "{label}: step {step}");
            last = Some((evidence, atlas));
        }
        let (evidence, atlas) = last.unwrap();
        assert_eq!(atlas.record_count(), read_count);
        let log = u64::from(usize::BITS - read_count.leading_zeros());
        let rows = if ordered {
            atlas.row_count() as u64 * tiles
        } else {
            0
        };
        let bound = strips * 3 + read_count as u64 * (log + 2) + rows;
        eprintln!(
            "{label}: visited {visited} slots over {tiles} extensions (bound {bound}); \
             rebuilding every extension visits at least {full_rebuilds}"
        );
        assert!(visited <= bound, "{label}: visited {visited} > {bound}");
        // The former implementation visited every incumbent record at least once per extension.
        assert!(
            visited * 5 < full_rebuilds,
            "{label}: {visited} vs {full_rebuilds}"
        );

        let clean = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(90, 93)),
            evidence.root(),
            atlas.interval(),
            layout,
            &world.ledger,
            ReservationKind::Normal,
            9,
        )
        .unwrap();
        assert_eq!(
            atlas.assignment_fingerprint(),
            clean.assignment_fingerprint(),
            "{label}"
        );
        assert_eq!(
            atlas_rows(&atlas, atlas.interval(), label),
            atlas_rows(&clean, atlas.interval(), label),
            "{label}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// One selection per summary tile, entered at the tile (SCENE-10)
// ---------------------------------------------------------------------------------------------

/// Every family of every tile the store prepared from one shared selection equals the tile its
/// builder produces alone from its own selection, and the allele and junction tiles equal the
/// former builders that walked each read's whole CIGAR.
#[test]
#[allow(clippy::too_many_lines)]
fn summary_families_from_one_selection_equal_independent_whole_cigar_builds() {
    let mut compared = 0_u64;
    for seed in 0..CASES {
        let world = World::new(seed);
        for policy in [
            OverlappingMatePolicy::CountBoth,
            OverlappingMatePolicy::CountOnce,
        ] {
            let contract = summary_contract(policy);
            drive(
                &world,
                layout(ReadStackingMode::Compact, ReadSampling::Complete),
                policy,
                |context, _, _, evidence, summaries, _| {
                    let ledger = &world.ledger;
                    let kind = ReservationKind::Normal;
                    let length = world.contig_length;
                    for tile in summaries.depth_tiles() {
                        let alone = crate::ExactDepthSummaryTile::build(
                            evidence,
                            tile.tile(),
                            length,
                            contract.depth(),
                            ledger,
                            kind,
                            3,
                        )
                        .unwrap();
                        assert_eq!(
                            (
                                tile.depth(),
                                tile.eligible_records(),
                                tile.collapsed_mate_bases()
                            ),
                            (
                                alone.depth(),
                                alone.eligible_records(),
                                alone.collapsed_mate_bases()
                            ),
                            "{context}: depth {:?}",
                            tile.tile()
                        );
                        compared += 1;
                    }
                    for tile in summaries.allele_tiles() {
                        let describe = |tile: &ExactAlleleSummaryTile| {
                            (
                                tile.sites().to_vec(),
                                tile.eligible_records(),
                                tile.collapsed_mate_bases(),
                            )
                        };
                        let build = |whole: bool| {
                            let builder = if whole {
                                ExactAlleleSummaryTile::build_walking_whole_cigars
                            } else {
                                ExactAlleleSummaryTile::build
                            };
                            builder(
                                evidence,
                                tile.tile(),
                                length,
                                contract.alleles(),
                                ledger,
                                kind,
                                3,
                            )
                            .unwrap()
                        };
                        let expected = describe(&build(true));
                        assert_eq!(
                            describe(tile),
                            expected,
                            "{context}: alleles {:?}",
                            tile.tile()
                        );
                        assert_eq!(
                            describe(&build(false)),
                            expected,
                            "{context}: {:?}",
                            tile.tile()
                        );
                        compared += 1;
                    }
                    for tile in summaries.junction_tiles() {
                        let describe = |tile: &crate::ExactJunctionSummaryTile| {
                            (tile.summary().clone(), tile.entering_junctions().to_vec())
                        };
                        let build = |whole: bool| {
                            let builder = if whole {
                                crate::ExactJunctionSummaryTile::build_walking_whole_cigars
                            } else {
                                crate::ExactJunctionSummaryTile::build
                            };
                            builder(
                                evidence,
                                tile.tile(),
                                length,
                                contract.junctions(),
                                None,
                                ledger,
                                kind,
                                3,
                            )
                            .unwrap()
                        };
                        let expected = describe(&build(true));
                        assert_eq!(
                            describe(tile),
                            expected,
                            "{context}: junctions {:?}",
                            tile.tile()
                        );
                        assert_eq!(
                            describe(&build(false)),
                            expected,
                            "{context}: {:?}",
                            tile.tile()
                        );
                        compared += 1;
                    }
                    for tile in summaries.pair_tiles() {
                        let alone = crate::ExactPairSummaryTile::build(
                            evidence,
                            tile.tile(),
                            length,
                            contract.pairs(),
                            ledger,
                            kind,
                            3,
                        )
                        .unwrap();
                        assert_eq!(
                            (tile.candidates(), tile.summary(), tile.maximum_record_end()),
                            (
                                alone.candidates(),
                                alone.summary(),
                                alone.maximum_record_end()
                            ),
                            "{context}: pairs {:?}",
                            tile.tile()
                        );
                        compared += 1;
                    }
                    for tile in summaries.modification_tiles() {
                        let alone = crate::ExactModificationSummaryTile::build(
                            evidence,
                            tile.tile(),
                            length,
                            contract.modifications().unwrap(),
                            ledger,
                            kind,
                            3,
                        )
                        .unwrap();
                        assert_eq!(
                            tile.summary(),
                            alone.summary(),
                            "{context}: modifications {:?}",
                            tile.tile()
                        );
                        compared += 1;
                    }
                },
            );
        }
    }
    assert!(compared > 1_000, "only {compared} tiles compared");
}

/// The store selects a tile's records once for all of its summary families, and not at all for
/// a tile whose families are every one cached.
#[test]
fn exact_summaries_select_each_tile_once_for_all_families() {
    for seed in 0..CASES {
        let world = World::new(seed);
        let target = interval(0, world.contig_length);
        let tiles = world.contig_length.div_ceil(world.tile_bases.get());
        let mut store = world.store();
        store.admit(world.acquisition(target)).unwrap();
        let evidence = store
            .prepare(target, payload_contract(), ReservationKind::Normal)
            .unwrap();
        let prepare = |store: &mut EvidenceStore| {
            store
                .prepare_exact_summaries(
                    evidence.root(),
                    target,
                    world.tile_bases,
                    world.contig_length,
                    summary_contract(OverlappingMatePolicy::CountOnce),
                    None,
                    ReservationKind::Normal,
                )
                .unwrap()
        };
        let first = prepare(&mut store);
        // Depth, alleles, junctions, pairs and modifications all read records: five families.
        assert!(!first.root().modification_tiles().is_empty(), "seed {seed}");
        assert!(!first.root().allele_tiles().is_empty(), "seed {seed}");
        assert_eq!(store.exact_summary_selection_count(), tiles, "seed {seed}");
        let again = prepare(&mut store);
        assert_eq!(
            store.exact_summary_selection_count(),
            tiles,
            "seed {seed}: cached"
        );
        assert_eq!(
            again.root().depth_tiles().len(),
            first.root().depth_tiles().len()
        );
        assert_eq!(world.ledger.snapshot().unwrap().transient, 0, "seed {seed}");
    }
}
