//! Content identity of read semantic pages.
//!
//! [`AlignmentPageKey`] names a page by the evidence and row-atlas revisions it was first built
//! from. Every evidence publication mints fresh revisions, so a lookup by that key alone can
//! never find a page across an edge acquisition, although most pages are untouched by one.
//!
//! [`AlignmentPageIdentity`] instead names a page by everything its builder reads:
//!
//! * the factorised contracts (source, record contract, view filter, layout, per-family
//!   presentation, level of detail) and the canonical tile and row block;
//! * a [`AlignmentPageContentToken`] digest of the builder's remaining inputs: the clamped row
//!   range, every `(handle, row, start, end)` the row atlas hands the page in canonical order,
//!   the payload sidecar contract each of those records resolves to in the evidence root, every
//!   pair connector crossing the page, and the reference bases the page family can read.
//!
//! A record's packed core and payload bytes are a pure function of its source-stable handle,
//! the record contract and the sidecar contract, so two roots which produce the same identity
//! hand the page builder identical inputs and the builder, being deterministic, produces an
//! identical page. That is the whole soundness argument for reuse, and the seeded property
//! harness checks it against freshly built pages.

use rgv_core::{AlignmentColorMode, GenomicInterval};
use rgv_hts::{AlignmentSourcePosition, ReferenceSlice};

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentPageKey, AlignmentRowAtlas,
    AlignmentRowConnector, AlignmentSourceRevision, AlignmentStyleContract, CanonicalGenomicTile,
    CanonicalRowBlock, ContractFingerprint, EvidenceRoot, GeometryLod, PayloadContract,
    PayloadSidecar, RevisionFingerprint, RowAtlasRecordHit,
};

/// Bases beside a reference-reading window which a specialist classifier may consult. Bisulfite
/// context reads two bases either side of a position; the margin is deliberately wider.
const REFERENCE_CONTEXT_MARGIN: u64 = 8;

/// Digest of every page-builder input that is not already a factorised contract.
///
/// `records` covers row range, record membership, row assignment, record extents, payload
/// availability and pair connectors. `reference` covers the reference bases the page family
/// reads, and is zero for a family which reads none.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentPageContentToken {
    records: RevisionFingerprint,
    reference: RevisionFingerprint,
}

impl AlignmentPageContentToken {
    #[must_use]
    pub const fn records(self) -> RevisionFingerprint {
        self.records
    }

    #[must_use]
    pub const fn reference(self) -> RevisionFingerprint {
        self.reference
    }
}

/// Revision-free content identity of one read semantic page.
///
/// Two pages with equal identities are structurally equal whichever evidence or row-atlas
/// revision each was built from. Unlike [`AlignmentPageKey`] this survives eviction and
/// rebuild, so it is the right key for any derived cache which wants to outlive a page object.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentPageIdentity {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    view_filter: ContractFingerprint,
    layout: ContractFingerprint,
    presentation: ContractFingerprint,
    genomic_tile: CanonicalGenomicTile,
    row_block: CanonicalRowBlock,
    family: AlignmentPageFamily,
    lod: GeometryLod,
    content: AlignmentPageContentToken,
}

impl AlignmentPageIdentity {
    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub const fn view_filter(self) -> ContractFingerprint {
        self.view_filter
    }

    #[must_use]
    pub const fn layout(self) -> ContractFingerprint {
        self.layout
    }

    #[must_use]
    pub const fn presentation(self) -> ContractFingerprint {
        self.presentation
    }

    #[must_use]
    pub const fn genomic_tile(self) -> CanonicalGenomicTile {
        self.genomic_tile
    }

    #[must_use]
    pub const fn row_block(self) -> CanonicalRowBlock {
        self.row_block
    }

    #[must_use]
    pub const fn family(self) -> AlignmentPageFamily {
        self.family
    }

    #[must_use]
    pub const fn lod(self) -> GeometryLod {
        self.lod
    }

    #[must_use]
    pub const fn content(self) -> AlignmentPageContentToken {
        self.content
    }

    /// The same identity with the reference digest cleared. Used to ask whether a page with
    /// these records and rows is resident when the caller holds no reference slice.
    #[must_use]
    pub(crate) const fn without_reference(mut self) -> Self {
        self.content.reference = RevisionFingerprint::new(0, 0);
        self
    }
}

/// Two-lane 128-bit digest. Every step is a bijection of each lane for a fixed input word, so
/// two inputs can only collide through the full 128-bit state.
pub(crate) struct PageDigest {
    first: u64,
    second: u64,
    words: u64,
}

impl PageDigest {
    pub(crate) fn new(domain: &[u8]) -> Self {
        let mut digest = Self {
            first: 0x243f_6a88_85a3_08d3,
            second: 0x1319_8a2e_0370_7344,
            words: 0,
        };
        digest.write_bytes(domain);
        digest
    }

    pub(crate) fn write_u64(&mut self, value: u64) {
        self.first = (self.first ^ value).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.first ^= self.first >> 32;
        self.second = (self.second.rotate_left(27) ^ value).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
        self.second ^= self.second >> 29;
        self.words = self.words.wrapping_add(1);
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0_u8; 8];
            word.copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
        let remainder = chunks.remainder();
        if !remainder.is_empty() {
            let mut word = [0_u8; 8];
            word[..remainder.len()].copy_from_slice(remainder);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    fn write_fingerprint(&mut self, value: RevisionFingerprint) {
        self.write_u64(value.high());
        self.write_u64(value.low());
    }

    fn write_handle(&mut self, handle: AlignmentHandle) {
        let source = handle.source_revision();
        self.write_fingerprint(source.source().fingerprint());
        self.write_fingerprint(source.revision());
        match handle.source_position() {
            AlignmentSourcePosition::BamVirtualOffset(offset) => {
                self.write_u64(0);
                self.write_u64(offset);
            }
            AlignmentSourcePosition::CramRecord {
                container_offset,
                slice_landmark,
                record_index,
            } => {
                self.write_u64(1);
                self.write_u64(container_offset);
                self.write_u64(slice_landmark);
                self.write_u64(u64::from(record_index));
            }
        }
    }

    fn write_payload_contract(&mut self, contract: &PayloadContract) {
        self.write_u64(
            (contract.sequence() as u64)
                | ((contract.qualities() as u64) << 8)
                | (u64::from(contract.all_auxiliary_tags()) << 16),
        );
        self.write_u64(contract.auxiliary_tags().len() as u64);
        for tag in contract.auxiliary_tags() {
            self.write_u64(u64::from(u16::from_le_bytes(*tag)));
        }
        match contract.decode_reference() {
            Some(reference) => {
                self.write_u64(1);
                self.write_fingerprint(reference.fingerprint());
            }
            None => self.write_u64(0),
        }
    }

    pub(crate) fn finish(self) -> RevisionFingerprint {
        let mix = |mut value: u64| {
            value ^= value >> 30;
            value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
            value ^= value >> 27;
            value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
            value ^ (value >> 31)
        };
        RevisionFingerprint::new(
            mix(self.first ^ self.words.rotate_left(17)),
            mix(self.second ^ self.words),
        )
    }
}

/// Whether a page of this family reads the reference slice at all. This is the exact gate the
/// builder applies: base cells in read geometry, and the bisulfite and Ultima overlays.
pub(crate) fn page_reads_reference(
    family: AlignmentPageFamily,
    lod: GeometryLod,
    style: AlignmentStyleContract,
) -> bool {
    match family {
        AlignmentPageFamily::ReadGeometry => lod == GeometryLod::Base,
        AlignmentPageFamily::SpecialistOverlay => {
            matches!(lod, GeometryLod::Read | GeometryLod::Base)
                && matches!(
                    style.color_mode,
                    AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
                )
        }
        AlignmentPageFamily::Labels => false,
    }
}

/// Reference positions a page of this family can read, before clamping to the slice.
///
/// * Base cells read only positions inside the tile.
/// * A bisulfite observation is kept only inside the tile and reads at most two bases beside it.
/// * Ultima analysis walks each whole read, so its window is the union of the page's reads.
fn reference_window(
    family: AlignmentPageFamily,
    style: AlignmentStyleContract,
    tile: GenomicInterval,
    records: &[RowAtlasRecordHit],
) -> (u64, u64) {
    let (mut start, mut end) = (tile.start(), tile.end());
    if family == AlignmentPageFamily::SpecialistOverlay {
        if matches!(style.color_mode, AlignmentColorMode::Ultima(_)) {
            for hit in records {
                start = start.min(hit.record.start());
                end = end.max(hit.record.end());
            }
        }
        start = start.saturating_sub(REFERENCE_CONTEXT_MARGIN);
        end = end.saturating_add(REFERENCE_CONTEXT_MARGIN);
    }
    (start, end)
}

fn reference_digest(
    family: AlignmentPageFamily,
    lod: GeometryLod,
    style: AlignmentStyleContract,
    tile: GenomicInterval,
    records: &[RowAtlasRecordHit],
    reference: Option<&ReferenceSlice>,
) -> RevisionFingerprint {
    if !page_reads_reference(family, lod, style) {
        return RevisionFingerprint::new(0, 0);
    }
    let mut digest = PageDigest::new(b"alignment-page-reference-v1");
    let Some(reference) = reference else {
        digest.write_u64(0);
        return digest.finish();
    };
    digest.write_u64(1);
    if reference.interval.contig() != tile.contig() {
        // Every `base_at` inside the window misses; the page sees no reference base at all.
        digest.write_u64(0);
        return digest.finish();
    }
    // `ReferenceSlice::base_at` answers only inside both the declared interval and the stored
    // bases, so that intersection is exactly what the builder can observe.
    let slice_start = reference.interval.start();
    let slice_end = reference
        .interval
        .end()
        .min(slice_start.saturating_add(reference.bases.len() as u64));
    let (window_start, window_end) = reference_window(family, style, tile, records);
    let start = window_start.max(slice_start);
    let end = window_end.min(slice_end);
    if start >= end {
        digest.write_u64(0);
        return digest.finish();
    }
    digest.write_u64(1);
    digest.write_u64(start);
    digest.write_u64(end);
    let offset = usize::try_from(start - slice_start).unwrap_or(usize::MAX);
    let length = usize::try_from(end - start).unwrap_or(usize::MAX);
    match reference.bases.get(offset..offset.saturating_add(length)) {
        Some(bases) => digest.write_bytes(bases),
        None => digest.write_u64(u64::MAX),
    }
    digest.finish()
}

/// A handle the row atlas assigned to this page which the evidence root cannot resolve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MissingPageHandle(pub(crate) AlignmentHandle);

/// Computes the identity of the page `key` would name, from exactly the inputs the builder is
/// about to read. `rows`, `records` and `connectors` must be what the builder uses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page_identity(
    key: AlignmentPageKey,
    evidence: &EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    style: AlignmentStyleContract,
    contig_length: u64,
    tile: GenomicInterval,
    rows: std::ops::Range<u32>,
    records: &[RowAtlasRecordHit],
    connectors: &[AlignmentRowConnector],
    reference: Option<&ReferenceSlice>,
) -> Result<AlignmentPageIdentity, MissingPageHandle> {
    let mut digest = PageDigest::new(b"alignment-page-records-v1");
    digest.write_u64(contig_length);
    digest.write_u64(tile.start());
    digest.write_u64(tile.end());
    digest.write_u64(u64::from(rows.start));
    digest.write_u64(u64::from(rows.end));
    digest.write_u64(records.len() as u64);
    // Consecutive records usually resolve to one sidecar; hash its contract once and reuse it.
    let mut last_sidecar: Option<(*const PayloadSidecar, RevisionFingerprint)> = None;
    for hit in records {
        let record = hit.record;
        let sidecar = evidence
            .page_record_payload(record.handle())
            .map_err(|_| MissingPageHandle(record.handle()))?;
        digest.write_handle(record.handle());
        digest.write_u64(u64::from(record.row()));
        digest.write_u64(record.start());
        digest.write_u64(record.end());
        match sidecar {
            None => digest.write_u64(0),
            Some(sidecar) => {
                let pointer = std::ptr::from_ref::<PayloadSidecar>(sidecar);
                let contract = match last_sidecar {
                    Some((seen, contract)) if seen == pointer => contract,
                    _ => {
                        let mut contract = PageDigest::new(b"payload");
                        contract.write_payload_contract(sidecar.contract());
                        let contract = contract.finish();
                        last_sidecar = Some((pointer, contract));
                        contract
                    }
                };
                digest.write_u64(1);
                digest.write_fingerprint(contract);
            }
        }
    }
    digest.write_u64(connectors.len() as u64);
    for connector in connectors {
        digest.write_u64(connector.template());
        digest.write_handle(connector.left());
        match connector.right() {
            Some(right) => {
                digest.write_u64(1);
                digest.write_handle(right);
            }
            None => digest.write_u64(0),
        }
        digest.write_u64(u64::from(connector.row()));
        digest.write_u64(connector.start());
        digest.write_u64(connector.end());
    }
    Ok(AlignmentPageIdentity {
        source_revision: key.source_revision(),
        record_contract: evidence.record_contract(),
        view_filter: atlas.view_filter(),
        layout: key.layout(),
        presentation: key.presentation(),
        genomic_tile: key.genomic_tile(),
        row_block: key.row_block(),
        family: key.family(),
        lod: key.lod(),
        content: AlignmentPageContentToken {
            records: digest.finish(),
            reference: reference_digest(key.family(), key.lod(), style, tile, records, reference),
        },
    })
}

/// Digest of one whole reference slice. The presentation store computes it once per request and
/// uses it to remember, without rehashing page inputs, that a request it has already resolved
/// for the same immutable roots and the same slice still resolves to the same page.
pub(crate) fn reference_slice_stamp(reference: Option<&ReferenceSlice>) -> RevisionFingerprint {
    let mut digest = PageDigest::new(b"alignment-page-reference-slice-v1");
    match reference {
        None => digest.write_u64(0),
        Some(reference) => {
            digest.write_u64(1);
            digest.write_u64(u64::from(reference.interval.contig().get()));
            digest.write_u64(reference.interval.start());
            digest.write_u64(reference.interval.end());
            digest.write_bytes(&reference.bases);
        }
    }
    digest.finish()
}
