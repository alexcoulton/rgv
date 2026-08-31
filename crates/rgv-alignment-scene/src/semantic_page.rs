use std::{
    collections::{BTreeSet, HashMap},
    mem::size_of,
    num::NonZeroU32,
    sync::Arc,
};

use rgv_core::{
    AlignmentColorMode, GenomicInterval, ModifiedBaseColorMode, ReadBaseDisplayMode,
    SbxSimplexTailMode,
};
use rgv_hts::{
    AlignedBaseClassification, AuxValue, CigarOpKind, ImplicitModificationMode, PairClassification,
    PairOrientation, ReferenceSlice, SbxInsertionState, SbxTailBounds, SequenceView,
    TemplateLengthBounds, UltimaEventUnavailable, UltimaIndelKind, UltimaQualitySource,
    aligned_position_for_sequencer_position, analyze_ultima_record, bisulfite_strand_is_reverse,
    classify_aligned_base, classify_bisulfite_observation, classify_sbx_insertion,
    decode_modified_bases_parts, decode_smrt_kinetics, pair_evidence_view, sbx_tail_bounds,
    sequencer_base_matches, smrt_kinetics_tag,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentReadLabelMode, AlignmentRowAtlas,
    AlignmentRowConnector, AlignmentSourceRevision, AlignmentStyleContract, AllocationClass,
    AllocationLease, AllocationLedger, CanonicalGenomicTile, CanonicalTileError,
    CapabilityRequirement, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, GeometryLod, LedgerError, PresentationContract, ReservationKind,
    RowAtlasRecordHit, RowAtlasRevision, TransientResourceClass, fingerprint::StableHasher,
};

/// Measurement-selected canonical vertical page identity. The block height is explicit in every
/// key, so later profiling can change it without ambiguously reusing old geometry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalRowBlock {
    coordinate: u32,
    rows: NonZeroU32,
}

impl CanonicalRowBlock {
    #[must_use]
    pub const fn new(coordinate: u32, rows: NonZeroU32) -> Self {
        Self { coordinate, rows }
    }

    #[must_use]
    pub const fn coordinate(self) -> u32 {
        self.coordinate
    }

    #[must_use]
    pub const fn rows(self) -> NonZeroU32 {
        self.rows
    }

    pub fn range(self, total_rows: usize) -> Result<std::ops::Range<u32>, SemanticPageError> {
        let start = self
            .coordinate
            .checked_mul(self.rows.get())
            .ok_or(SemanticPageError::RowCoordinateOverflow)?;
        let total =
            u32::try_from(total_rows).map_err(|_| SemanticPageError::RowCoordinateOverflow)?;
        if total == 0 && self.coordinate == 0 {
            return Ok(0..0);
        }
        if start >= total {
            return Err(SemanticPageError::RowBlockOutsideAtlas { start, total });
        }
        Ok(start..start.saturating_add(self.rows.get()).min(total))
    }
}

/// Content identity for one immutable alignment geometry page. No exact camera coordinate,
/// pixel position, hover, selection, or vertical scroll state participates in this key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentPageKey {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    row_atlas_revision: RowAtlasRevision,
    layout: ContractFingerprint,
    presentation: ContractFingerprint,
    genomic_tile: CanonicalGenomicTile,
    row_block: CanonicalRowBlock,
    family: AlignmentPageFamily,
    lod: GeometryLod,
}

impl AlignmentPageKey {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        source_revision: AlignmentSourceRevision,
        evidence_revision: EvidenceRevision,
        row_atlas_revision: RowAtlasRevision,
        layout: ContractFingerprint,
        presentation: ContractFingerprint,
        genomic_tile: CanonicalGenomicTile,
        row_block: CanonicalRowBlock,
        family: AlignmentPageFamily,
        lod: GeometryLod,
    ) -> Self {
        Self {
            source_revision,
            evidence_revision,
            row_atlas_revision,
            layout,
            presentation,
            genomic_tile,
            row_block,
            family,
            lod,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn evidence_revision(self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn row_atlas_revision(self) -> RowAtlasRevision {
        self.row_atlas_revision
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
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentOperation {
    Match,
    SequenceMatch,
    SequenceMismatch,
    Insertion,
    Deletion,
    ReferenceSkip,
    SoftClip,
    HardClip,
    Padding,
}

impl From<CigarOpKind> for AlignmentOperation {
    fn from(value: CigarOpKind) -> Self {
        match value {
            CigarOpKind::Match => Self::Match,
            CigarOpKind::SequenceMatch => Self::SequenceMatch,
            CigarOpKind::SequenceMismatch => Self::SequenceMismatch,
            CigarOpKind::Insertion => Self::Insertion,
            CigarOpKind::Deletion => Self::Deletion,
            CigarOpKind::ReferenceSkip => Self::ReferenceSkip,
            CigarOpKind::SoftClip => Self::SoftClip,
            CigarOpKind::HardClip => Self::HardClip,
            CigarOpKind::Padding => Self::Padding,
        }
    }
}

/// Typed semantic target shared by rendering, hover, click, inspection, and export.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentComponent {
    ReadSpan,
    AlignedSegment {
        cigar_index: u32,
        operation: AlignmentOperation,
    },
    Insertion {
        cigar_index: u32,
        read_start: u32,
        length: u32,
    },
    Deletion {
        cigar_index: u32,
    },
    ReferenceSkip {
        cigar_index: u32,
    },
    SoftClip {
        cigar_index: u32,
        read_start: u32,
        length: u32,
    },
    HardClip {
        cigar_index: u32,
        length: u32,
    },
    Padding {
        cigar_index: u32,
        length: u32,
    },
    DirectionTerminal,
    PairConnector {
        template: u64,
        mate: Option<AlignmentHandle>,
    },
    Label {
        kind: AlignmentLabelKind,
        id: ContractFingerprint,
    },
    ModifiedBase {
        read_index: u32,
        canonical_base: u8,
        code: rgv_hts::ModificationCode,
        probability: Option<u8>,
        state: ModificationState,
    },
    BisulfiteBase {
        read_index: u32,
        observation: BisulfiteObservationToken,
    },
    SmrtKinetics {
        read_index: u32,
        tag: [u8; 2],
        value: u64,
    },
    UltimaIndel {
        cigar_index: u32,
        kind: UltimaIndelToken,
        quality_bits: Option<u32>,
        quality_source: Option<UltimaQualitySourceToken>,
        unavailable: Option<UltimaUnavailableToken>,
    },
    SbxInsertion {
        cigar_index: u32,
        state: SbxInsertionToken,
    },
    SbxTailBase {
        read_index: u32,
    },
    SpecialistUnavailable {
        mode: SpecialistMode,
        reason: SpecialistUnavailableReason,
    },
    Base {
        cigar_index: u32,
        read_index: u32,
        base: u8,
        quality: Option<u8>,
        classification: AlignedBaseClassification,
    },
}

/// Camera-independent shape carried by semantic geometry.
///
/// Direction belongs here rather than being inferred by one renderer. This keeps the CPU
/// reference projector, GPU renderer, picking, and exports on the same geometry contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentGeometryToken {
    Rectangle,
    EventLine,
    PointMarker,
    DirectionalTerminalLeft,
    DirectionalTerminalRight,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModificationState {
    Modified,
    Canonical,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BisulfiteObservationToken {
    Protected,
    Converted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaIndelToken {
    Insertion,
    Deletion,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaQualitySourceToken {
    TpDirection,
    T0,
    MinimumHomopolymer,
    MaximumHomopolymerLimit,
    TpReadMaximumFallback,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaUnavailableToken {
    NotBracketedByAlignedBases,
    AmbiguousHomopolymer,
    NonHomopolymer,
    ReferenceUnavailable,
    T0Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SbxInsertionToken {
    Concordant,
    PartlyDiscordant,
    FullyDiscordant,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpecialistMode {
    ModifiedBases,
    Bisulfite,
    SmrtKinetics,
    Ultima,
    Sbx,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpecialistUnavailableReason {
    MissingRecordData,
    MalformedRecordData,
    MissingReference,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentLabelKind {
    ReadName,
    AuxiliaryTag([u8; 2]),
    InsertionLength,
    DeletionLength,
    SkipLength,
    ClipLength,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSemanticLabel {
    id: ContractFingerprint,
    text: Arc<str>,
}

impl AlignmentSemanticLabel {
    #[must_use]
    pub const fn id(&self) -> ContractFingerprint {
        self.id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentStyleToken {
    pub flags: u16,
    pub mapping_quality: Option<u8>,
    pub operation: Option<AlignmentOperation>,
    pub body: AlignmentBodyColorToken,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentBodyColorToken {
    Neutral,
    Strand { reverse: bool },
    MappingQuality(Option<u8>),
    Category(Option<ContractFingerprint>),
    TemplateLength { absolute: u64, status: u8 },
    PairOrientation(u8),
    PairStatus(u8),
    FirstOfPairStrand(Option<bool>),
    Specialist { mode: u8, reverse: bool },
}

/// Page-local semantic primitive. Coordinates are integer offsets from the genomic tile and row
/// block, never absolute chromosome `f32` values or pane pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSemanticPrimitive {
    pub start_offset: u64,
    pub end_offset: u64,
    pub row_offset: u32,
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub geometry: AlignmentGeometryToken,
    pub style: AlignmentStyleToken,
    pub draw_order: u32,
}

impl AlignmentSemanticPrimitive {
    #[must_use]
    pub const fn contains_offset(self, offset: u64) -> bool {
        self.start_offset <= offset && offset < self.end_offset
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPageHit {
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub draw_order: u32,
}

/// Immutable CPU semantic geometry page. GPU pages and the software projector derive from this
/// object; GPU residency can never alter its evidence or picking identity.
#[derive(Debug)]
pub struct AlignmentSemanticPage {
    key: AlignmentPageKey,
    interval: GenomicInterval,
    row_start: u32,
    row_end: u32,
    primitives: Arc<[AlignmentSemanticPrimitive]>,
    row_offsets: Arc<[u32]>,
    labels: Arc<[AlignmentSemanticLabel]>,
    allocation: AllocationLease,
}

#[derive(Clone, Debug)]
struct PreparedRecordSemantics {
    body: AlignmentBodyColorToken,
    read_label: Option<(AlignmentLabelKind, String)>,
    overlays: Vec<PreparedOverlay>,
    hidden_sbx_tail: Option<SbxTailBounds>,
}

#[derive(Clone, Copy, Debug)]
struct PreparedOverlay {
    start: u64,
    end: u64,
    component: AlignmentComponent,
}

#[derive(Clone, Debug)]
struct PendingLabel {
    id: ContractFingerprint,
    text: String,
}

impl AlignmentSemanticPage {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn build(
        key: AlignmentPageKey,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        contig_length: u64,
        reference: Option<&ReferenceSlice>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SemanticPageError> {
        validate_dependencies(key, evidence, atlas, presentation, reference)?;
        let interval = key.genomic_tile.interval(contig_length)?;
        if !evidence.coverage().covers(interval) {
            return Err(SemanticPageError::UncertifiedTile(interval));
        }
        let rows = key.row_block.range(atlas.row_count())?;
        let atlas_records = atlas.records_for_rows(rows.clone(), interval);
        let selection = evidence.select_handles_with_reservation_kind(
            interval,
            atlas_records.iter().map(|hit| hit.record.handle()),
            reservation_kind,
        )?;
        let atlas_record_count = atlas_records.len();
        let connector_count = if key.family == AlignmentPageFamily::ReadGeometry {
            atlas.connector_count_for_rows(rows.clone(), interval)
        } else {
            0
        };
        let base_scratch_bytes =
            semantic_page_base_scratch_bytes(selection.len(), atlas_record_count, connector_count);
        let _base_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            base_scratch_bytes,
            reservation_kind,
        )?;
        let mut by_handle = HashMap::with_capacity(selection.len());
        for (index, record) in selection.records().iter().enumerate() {
            by_handle.insert(record.handle, index);
        }
        let pair_connectors = if key.family == AlignmentPageFamily::ReadGeometry {
            atlas.connectors_for_rows(rows.clone(), interval)
        } else {
            Vec::new()
        };

        let dynamic_scratch_bytes = semantic_page_dynamic_scratch_bytes(
            &selection,
            &atlas_records,
            &by_handle,
            key,
            presentation,
        )?;
        let _dynamic_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            dynamic_scratch_bytes,
            reservation_kind,
        )?;

        // First pass computes an exact output upper bound without creating retained geometry.
        let mut maximum_primitives = pair_connectors.len();
        let mut maximum_labels = 0_usize;
        let mut maximum_label_bytes = 0_usize;
        let mut prepared =
            HashMap::<AlignmentHandle, PreparedRecordSemantics>::with_capacity(atlas_records.len());
        for hit in &atlas_records {
            let selection_index = *by_handle.get(&hit.record.handle()).ok_or(
                SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
            )?;
            let record = selection
                .record_view(selection_index)
                .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
            let body = if key.family == AlignmentPageFamily::ReadGeometry {
                body_color_token(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.color_mode,
                    atlas.interval(),
                    atlas.layout_contract().pair_length_bounds,
                )?
            } else {
                AlignmentBodyColorToken::Neutral
            };
            let read_label = if key.family == AlignmentPageFamily::Labels
                && matches!(key.lod, GeometryLod::Read | GeometryLod::Base)
            {
                read_label_for_record(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.read_labels,
                )?
            } else {
                None
            };
            let overlays = if key.family == AlignmentPageFamily::SpecialistOverlay
                && matches!(key.lod, GeometryLod::Read | GeometryLod::Base)
            {
                prepare_specialist_overlays(
                    &selection,
                    selection_index,
                    record,
                    presentation.style,
                    reference,
                    interval,
                )?
            } else {
                Vec::new()
            };
            let hidden_sbx_tail = if key.family == AlignmentPageFamily::ReadGeometry
                && matches!(
                    presentation.style.color_mode,
                    AlignmentColorMode::Sbx(options)
                        if options.simplex_tail_mode == SbxSimplexTailMode::Hide
                ) {
                hidden_sbx_tail_bounds(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.color_mode,
                )
            } else {
                None
            };
            if let Some((_, label)) = &read_label {
                maximum_labels = maximum_labels.saturating_add(1);
                maximum_label_bytes = maximum_label_bytes.saturating_add(label.len());
            }
            let event_labels = if key.family == AlignmentPageFamily::Labels {
                record
                    .cigar()
                    .iter()
                    .filter(|operation| {
                        operation.len >= presentation.style.minimum_event_label_size
                            && matches!(
                                operation.kind,
                                CigarOpKind::Insertion
                                    | CigarOpKind::Deletion
                                    | CigarOpKind::ReferenceSkip
                                    | CigarOpKind::SoftClip
                                    | CigarOpKind::HardClip
                            )
                    })
                    .count()
            } else {
                0
            };
            maximum_labels = maximum_labels.saturating_add(event_labels);
            maximum_label_bytes =
                maximum_label_bytes.saturating_add(event_labels.saturating_mul(10));
            maximum_primitives = maximum_primitives
                .saturating_add(overlays.len())
                .saturating_add(event_labels)
                .saturating_add(usize::from(read_label.is_some()))
                .saturating_add(if key.family == AlignmentPageFamily::ReadGeometry {
                    match key.lod {
                        GeometryLod::Overview => 1,
                        GeometryLod::Span | GeometryLod::Read => {
                            record.cigar().len().max(1).saturating_add(1)
                        }
                        GeometryLod::Base => record.cigar().iter().fold(
                            record.cigar().len().max(1).saturating_add(1),
                            |total, operation| {
                                total.saturating_add(match operation.kind {
                                    CigarOpKind::Match
                                    | CigarOpKind::SequenceMatch
                                    | CigarOpKind::SequenceMismatch => operation.len as usize,
                                    _ => 0,
                                })
                            },
                        ),
                    }
                } else {
                    0
                });
            prepared.insert(
                hit.record.handle(),
                PreparedRecordSemantics {
                    body,
                    read_label,
                    overlays,
                    hidden_sbx_tail,
                },
            );
        }
        let row_offset_count = usize::try_from(rows.end.saturating_sub(rows.start))
            .map_err(|_| SemanticPageError::RowCoordinateOverflow)?
            .saturating_add(1);
        let maximum_bytes = semantic_page_bytes(
            maximum_primitives,
            row_offset_count,
            maximum_labels,
            maximum_label_bytes,
        );
        let _output_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::SemanticPage,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut primitives = Vec::with_capacity(maximum_primitives);
        let mut labels = Vec::with_capacity(maximum_labels);
        let tile_start = interval.start();
        for hit in atlas_records {
            let selection_index = *by_handle.get(&hit.record.handle()).ok_or(
                SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
            )?;
            let record = selection
                .record_view(selection_index)
                .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
            let row_offset = hit.record.row().saturating_sub(rows.start);
            let prepared = prepared.remove(&hit.record.handle()).ok_or(
                SemanticPageError::MissingPreparedSemantics(hit.record.handle()),
            )?;
            emit_record_primitives(
                &mut primitives,
                &mut labels,
                hit.record.handle(),
                row_offset,
                record,
                selection.sequence(selection_index),
                prepared.body,
                key.lod,
                interval,
                tile_start,
                reference,
                presentation.style,
                prepared.read_label.as_ref(),
                key.family,
                prepared.hidden_sbx_tail,
            )?;
            let overlay_style = AlignmentStyleToken {
                flags: record.flags().bits(),
                mapping_quality: record.mapping_quality(),
                operation: None,
                body: prepared.body,
            };
            for (draw_order, overlay) in prepared.overlays.into_iter().enumerate() {
                push_span(
                    &mut primitives,
                    hit.record.handle(),
                    row_offset,
                    overlay.start,
                    overlay.end,
                    interval,
                    tile_start,
                    overlay.component,
                    overlay_style,
                    20_000_u32.saturating_add(u32::try_from(draw_order).unwrap_or(u32::MAX)),
                );
            }
        }
        for connector in pair_connectors {
            push_span(
                &mut primitives,
                connector.left(),
                connector.row().saturating_sub(rows.start),
                connector.start(),
                connector.end(),
                interval,
                tile_start,
                AlignmentComponent::PairConnector {
                    template: connector.template(),
                    mate: connector.right(),
                },
                AlignmentStyleToken {
                    flags: 0,
                    mapping_quality: None,
                    operation: None,
                    body: AlignmentBodyColorToken::Neutral,
                },
                0,
            );
        }
        primitives.sort_unstable_by_key(|primitive| {
            (
                primitive.row_offset,
                primitive.start_offset,
                primitive.end_offset,
                primitive.draw_order,
                primitive.handle,
                primitive.component,
            )
        });
        let mut row_offsets = vec![0_u32; row_offset_count];
        let mut cursor = 0_usize;
        for row in 0..row_offset_count.saturating_sub(1) {
            row_offsets[row] =
                u32::try_from(cursor).map_err(|_| SemanticPageError::PrimitiveCountOverflow)?;
            while primitives
                .get(cursor)
                .is_some_and(|primitive| primitive.row_offset as usize == row)
            {
                cursor = cursor.saturating_add(1);
            }
        }
        if let Some(end) = row_offsets.last_mut() {
            *end = u32::try_from(primitives.len())
                .map_err(|_| SemanticPageError::PrimitiveCountOverflow)?;
        }
        primitives.shrink_to_fit();
        row_offsets.shrink_to_fit();
        let labels = finalize_labels(labels)?;
        let retained_label_bytes = labels.iter().map(|label| label.text.len()).sum::<usize>();
        let retained_bytes = semantic_page_bytes(
            primitives.len(),
            row_offsets.len(),
            labels.len(),
            retained_label_bytes,
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            key,
            interval,
            row_start: rows.start,
            row_end: rows.end,
            primitives: primitives.into(),
            row_offsets: row_offsets.into(),
            labels: labels.into(),
            allocation,
        }))
    }

    #[must_use]
    pub const fn key(&self) -> AlignmentPageKey {
        self.key
    }

    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub const fn row_range(&self) -> std::ops::Range<u32> {
        self.row_start..self.row_end
    }

    #[must_use]
    pub fn primitives(&self) -> &[AlignmentSemanticPrimitive] {
        &self.primitives
    }

    #[must_use]
    pub fn labels(&self) -> &[AlignmentSemanticLabel] {
        &self.labels
    }

    #[must_use]
    pub fn label(&self, id: ContractFingerprint) -> Option<&str> {
        self.labels
            .binary_search_by_key(&id, AlignmentSemanticLabel::id)
            .ok()
            .and_then(|index| self.labels.get(index))
            .map(AlignmentSemanticLabel::text)
    }

    #[must_use]
    pub fn label_arc(&self, id: ContractFingerprint) -> Option<Arc<str>> {
        self.labels
            .binary_search_by_key(&id, AlignmentSemanticLabel::id)
            .ok()
            .and_then(|index| self.labels.get(index))
            .map(|label| Arc::clone(&label.text))
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    #[must_use]
    pub fn pick(&self, row: u32, genomic_position: u64) -> Option<SemanticPageHit> {
        self.primitive_candidates(row, genomic_position)
            .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
            .max_by_key(|primitive| (primitive.draw_order, primitive.component))
            .map(|primitive| SemanticPageHit {
                handle: primitive.handle,
                component: primitive.component,
                draw_order: primitive.draw_order,
            })
    }

    /// Returns all typed components at a stable row/genomic position. The CPU picker refines
    /// these candidates against their projected component shapes without GPU readback.
    ///
    /// This iterator borrows the immutable page and performs no allocation. Interactive pointer
    /// motion must not allocate a temporary vector for every page under the cursor.
    pub fn primitive_candidates(
        &self,
        row: u32,
        genomic_position: u64,
    ) -> impl Iterator<Item = AlignmentSemanticPrimitive> + '_ {
        let candidates = if row < self.row_start
            || row >= self.row_end
            || !self.interval.contains(genomic_position)
        {
            &[][..]
        } else {
            usize::try_from(row - self.row_start)
                .ok()
                .and_then(|local_row| {
                    let start = self.row_offsets.get(local_row).copied()?;
                    let end = self.row_offsets.get(local_row.saturating_add(1)).copied()?;
                    self.primitives.get(start as usize..end as usize)
                })
                .unwrap_or_default()
        };
        let offset = genomic_position.saturating_sub(self.interval.start());
        candidates
            .iter()
            .filter(move |primitive| primitive.contains_offset(offset))
            .copied()
    }

    /// Compatibility collector for non-interactive callers and tests that need an owned list.
    #[must_use]
    pub fn primitives_at(
        &self,
        row: u32,
        genomic_position: u64,
    ) -> Vec<AlignmentSemanticPrimitive> {
        self.primitive_candidates(row, genomic_position).collect()
    }
}

fn body_color_token(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    mode: AlignmentColorMode,
    visible_interval: GenomicInterval,
    pair_bounds: TemplateLengthBounds,
) -> Result<AlignmentBodyColorToken, SemanticPageError> {
    let pair = || pair_evidence_view(record, visible_interval, pair_bounds);
    Ok(match mode {
        AlignmentColorMode::None => AlignmentBodyColorToken::Neutral,
        AlignmentColorMode::Strand => AlignmentBodyColorToken::Strand {
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::MappingQuality => {
            AlignmentBodyColorToken::MappingQuality(record.mapping_quality())
        }
        AlignmentColorMode::ReadGroup => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, *b"RG")?)
        }
        AlignmentColorMode::Haplotype => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, *b"HP")?)
        }
        AlignmentColorMode::TemplateLength => AlignmentBodyColorToken::TemplateLength {
            absolute: record.template_length().unsigned_abs(),
            status: classification_rank(pair().classification),
        },
        AlignmentColorMode::PairOrientation => {
            AlignmentBodyColorToken::PairOrientation(orientation_rank(pair().orientation))
        }
        AlignmentColorMode::UnexpectedPair => {
            AlignmentBodyColorToken::PairStatus(classification_rank(pair().classification))
        }
        AlignmentColorMode::MateContig => {
            let category = record.mate_contig().map(|contig| {
                let mut hash = StableHasher::default();
                hash.write(b"mate-contig\0");
                hash.write(&contig.get().to_le_bytes());
                hash.finish()
            });
            AlignmentBodyColorToken::Category(category)
        }
        AlignmentColorMode::FirstOfPairStrand => {
            AlignmentBodyColorToken::FirstOfPairStrand(first_of_pair_reverse(record.flags()))
        }
        AlignmentColorMode::Bisulfite(_) => AlignmentBodyColorToken::Specialist {
            mode: 0,
            reverse: bisulfite_strand_is_reverse(record.flags()),
        },
        AlignmentColorMode::SmrtKinetics(_) => AlignmentBodyColorToken::Specialist {
            mode: 1,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Ultima(_) => AlignmentBodyColorToken::Specialist {
            mode: 2,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Sbx(_) => AlignmentBodyColorToken::Specialist {
            mode: 3,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Tag(tag) => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, tag)?)
        }
    })
}

fn auxiliary_category(
    selection: &crate::EvidenceSelection,
    index: usize,
    tag: [u8; 2],
) -> Result<Option<ContractFingerprint>, SemanticPageError> {
    let Some(field) = selection.auxiliary_field(index, tag)? else {
        return Ok(None);
    };
    let mut hash = StableHasher::default();
    hash.write(b"alignment-category\0");
    hash.write(&tag);
    let supported = match field.value {
        AuxValue::Character(value) => {
            hash.write(&[0, value]);
            true
        }
        AuxValue::Signed(value) => {
            hash.write(&[1]);
            hash.write(&value.to_le_bytes());
            true
        }
        AuxValue::Unsigned(value) => {
            hash.write(&[2]);
            hash.write(&value.to_le_bytes());
            true
        }
        AuxValue::Float(value) => {
            hash.write(&[3]);
            hash.write(&value.to_bits().to_le_bytes());
            true
        }
        AuxValue::Double(value) => {
            hash.write(&[4]);
            hash.write(&value.to_bits().to_le_bytes());
            true
        }
        AuxValue::String(value) => {
            hash.write(&[5]);
            hash.write(value.as_bytes());
            true
        }
        AuxValue::HexByteArray(value) => {
            hash.write(&[6]);
            hash.write(value.as_bytes());
            true
        }
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => false,
    };
    Ok(supported.then(|| hash.finish()))
}

fn first_of_pair_reverse(flags: rgv_hts::AlignmentFlags) -> Option<bool> {
    if flags.is_first_in_template() {
        Some(flags.is_reverse())
    } else if flags.is_last_in_template() {
        Some(flags.is_mate_reverse())
    } else {
        None
    }
}

const fn orientation_rank(value: PairOrientation) -> u8 {
    match value {
        PairOrientation::Inward => 0,
        PairOrientation::Outward => 1,
        PairOrientation::SameStrand => 2,
        PairOrientation::Unknown => 3,
    }
}

const fn classification_rank(value: PairClassification) -> u8 {
    match value {
        PairClassification::Proper => 0,
        PairClassification::Concordant => 1,
        PairClassification::DiscordantTemplateLengthShort => 2,
        PairClassification::DiscordantTemplateLengthLong => 3,
        PairClassification::DiscordantOrientation => 4,
        PairClassification::InterContig => 5,
        PairClassification::MateUnmapped => 6,
        PairClassification::MateMissing => 7,
        PairClassification::ReadUnmapped => 8,
        PairClassification::Unpaired => 9,
    }
}

fn prepare_specialist_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    style: AlignmentStyleContract,
    reference: Option<&ReferenceSlice>,
    tile: GenomicInterval,
) -> Result<Vec<PreparedOverlay>, SemanticPageError> {
    let mut output = Vec::new();
    if style.show_modified_bases {
        prepare_modified_base_overlays(selection, index, record, style, tile, &mut output)?;
    }
    match style.color_mode {
        AlignmentColorMode::Bisulfite(context) => {
            prepare_bisulfite_overlays(
                selection,
                index,
                record,
                reference,
                context,
                tile,
                &mut output,
            );
        }
        AlignmentColorMode::SmrtKinetics(mode) => {
            prepare_smrt_overlays(selection, index, record, mode, tile, &mut output)?;
        }
        AlignmentColorMode::Ultima(options) => {
            prepare_ultima_overlays(
                selection,
                index,
                record,
                reference,
                options,
                tile,
                &mut output,
            )?;
        }
        AlignmentColorMode::Sbx(options) => {
            prepare_sbx_overlays(selection, index, record, options, tile, &mut output);
        }
        _ => {}
    }
    output.sort_unstable_by_key(|overlay| (overlay.start, overlay.end, overlay.component));
    Ok(output)
}

fn prepare_modified_base_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    style: AlignmentStyleContract,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return Ok(());
    };
    let mm_upper = selection.auxiliary_field(index, *b"MM")?;
    let mm_lower = selection.auxiliary_field(index, *b"Mm")?;
    let Some(mm) = mm_upper.as_ref().or(mm_lower.as_ref()) else {
        return Ok(());
    };
    let ml_upper = selection.auxiliary_field(index, *b"ML")?;
    let ml_lower = selection.auxiliary_field(index, *b"Ml")?;
    let ml = ml_upper.as_ref().or(ml_lower.as_ref());
    let calls = match decode_modified_bases_parts(
        bases,
        record.flags(),
        &mm.value,
        ml.map(|field| &field.value),
    ) {
        Ok(calls) => calls,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::ModifiedBases,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return Ok(());
        }
    };
    let mut canonical_groups = Vec::<(u8, rgv_hts::ModificationCode, BTreeSet<usize>)>::new();
    for call in &calls {
        if style.hidden_modification_codes.contains(&Some(call.code))
            || (style.minimum_modification_probability > 0
                && call
                    .probability
                    .is_none_or(|probability| probability < style.minimum_modification_probability))
        {
            continue;
        }
        let Some(read_index) = aligned_position_for_sequencer_position(
            bases.len(),
            record.flags(),
            call.sequencer_position,
        ) else {
            continue;
        };
        let Some(position) =
            reference_position_for_read_position(record.start(), record.cigar(), read_index)
        else {
            continue;
        };
        if tile.contains(position) {
            output.push(PreparedOverlay {
                start: position,
                end: position.saturating_add(1),
                component: AlignmentComponent::ModifiedBase {
                    read_index: u32::try_from(read_index)
                        .map_err(|_| SemanticPageError::ReadCoordinateOverflow)?,
                    canonical_base: call.canonical_base,
                    code: call.code,
                    probability: call.probability,
                    state: ModificationState::Modified,
                },
            });
        }
        if style.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified
            && call.implicit_mode == ImplicitModificationMode::Canonical
        {
            if let Some((_, _, positions)) = canonical_groups
                .iter_mut()
                .find(|(base, code, _)| *base == call.canonical_base && *code == call.code)
            {
                positions.insert(call.sequencer_position);
            } else {
                canonical_groups.push((
                    call.canonical_base,
                    call.code,
                    BTreeSet::from([call.sequencer_position]),
                ));
            }
        }
    }
    if style.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified {
        for (canonical_base, code, called) in canonical_groups {
            for sequencer_position in 0..bases.len() {
                if called.contains(&sequencer_position)
                    || !sequencer_base_matches(
                        bases,
                        record.flags(),
                        sequencer_position,
                        canonical_base,
                    )
                {
                    continue;
                }
                let Some(read_index) = aligned_position_for_sequencer_position(
                    bases.len(),
                    record.flags(),
                    sequencer_position,
                ) else {
                    continue;
                };
                let Some(position) = reference_position_for_read_position(
                    record.start(),
                    record.cigar(),
                    read_index,
                ) else {
                    continue;
                };
                if tile.contains(position) {
                    output.push(PreparedOverlay {
                        start: position,
                        end: position.saturating_add(1),
                        component: AlignmentComponent::ModifiedBase {
                            read_index: u32::try_from(read_index)
                                .map_err(|_| SemanticPageError::ReadCoordinateOverflow)?,
                            canonical_base,
                            code,
                            probability: None,
                            state: ModificationState::Canonical,
                        },
                    });
                }
            }
        }
    }
    Ok(())
}

fn prepare_bisulfite_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    reference: Option<&ReferenceSlice>,
    context: rgv_core::BisulfiteContext,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) {
    let Some(reference) = reference else {
        push_unavailable(
            output,
            record,
            tile,
            SpecialistMode::Bisulfite,
            SpecialistUnavailableReason::MissingReference,
        );
        return;
    };
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return;
    };
    let reverse = bisulfite_strand_is_reverse(record.flags());
    for_each_aligned_base(record, |read_index, position, _| {
        let Some(base) = bases.get(read_index).copied() else {
            return;
        };
        let observation =
            classify_bisulfite_observation(reference, position, reverse, context, base);
        let observation = match observation {
            Some(rgv_hts::BisulfiteObservation::Protected) => {
                Some(BisulfiteObservationToken::Protected)
            }
            Some(rgv_hts::BisulfiteObservation::Converted) => {
                Some(BisulfiteObservationToken::Converted)
            }
            None => None,
        };
        if tile.contains(position)
            && let Some(observation) = observation
            && let Ok(read_index) = u32::try_from(read_index)
        {
            output.push(PreparedOverlay {
                start: position,
                end: position.saturating_add(1),
                component: AlignmentComponent::BisulfiteBase {
                    read_index,
                    observation,
                },
            });
        }
    });
}

fn prepare_smrt_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    mode: rgv_core::SmrtKineticsMode,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return Ok(());
    };
    let tag = smrt_kinetics_tag(mode, record.flags().is_reverse());
    let Some(field) = selection.auxiliary_field(index, tag)? else {
        return Ok(());
    };
    let series = match decode_smrt_kinetics(tag, &field.value, bases.len()) {
        Ok(series) => series,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::SmrtKinetics,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return Ok(());
        }
    };
    for_each_aligned_base(record, |read_index, position, _| {
        if !tile.contains(position) {
            return;
        }
        if let (Some(value), Ok(read_index)) = (
            series.value_at_aligned_read_position(record.flags(), read_index),
            u32::try_from(read_index),
        ) {
            output.push(PreparedOverlay {
                start: position,
                end: position.saturating_add(1),
                component: AlignmentComponent::SmrtKinetics {
                    read_index,
                    tag,
                    value,
                },
            });
        }
    });
    Ok(())
}

fn prepare_ultima_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    reference: Option<&ReferenceSlice>,
    options: rgv_core::UltimaDisplayOptions,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let materialized = selection.materialize_retained_record(index)?;
    let analysis = match analyze_ultima_record(
        &materialized,
        reference,
        options.quality_mode,
        options.maximum_homopolymer_length_override,
    ) {
        Ok(analysis) => analysis,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::Ultima,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return Ok(());
        }
    };
    for event in analysis.events {
        let end = match event.kind {
            UltimaIndelKind::Insertion => event.reference_start.saturating_add(1),
            UltimaIndelKind::Deletion => event
                .reference_start
                .saturating_add(u64::try_from(event.length).unwrap_or(u64::MAX)),
        };
        if event.reference_start >= tile.end() || end <= tile.start() {
            continue;
        }
        output.push(PreparedOverlay {
            start: event.reference_start,
            end,
            component: AlignmentComponent::UltimaIndel {
                cigar_index: u32::try_from(event.operation_index)
                    .map_err(|_| SemanticPageError::CigarIndexOverflow)?,
                kind: match event.kind {
                    UltimaIndelKind::Insertion => UltimaIndelToken::Insertion,
                    UltimaIndelKind::Deletion => UltimaIndelToken::Deletion,
                },
                quality_bits: event.quality.map(f32::to_bits),
                quality_source: event.quality_source.map(map_ultima_quality_source),
                unavailable: event.unavailable.map(map_ultima_unavailable),
            },
        });
    }
    Ok(())
}

fn prepare_sbx_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    options: rgv_core::SbxDisplayOptions,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) {
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = selection.sequence(index)
    else {
        return;
    };
    let tails = match sbx_tail_bounds(
        record.flags(),
        bases,
        qualities,
        options.simplex_tail_quality_maximum,
    ) {
        Ok(tails) => tails,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::Sbx,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return;
        }
    };
    if options.simplex_tail_mode != SbxSimplexTailMode::Off {
        for_each_aligned_base(record, |read_index, position, _| {
            if tails.is_tail(read_index)
                && tile.contains(position)
                && let Ok(read_index) = u32::try_from(read_index)
            {
                output.push(PreparedOverlay {
                    start: position,
                    end: position.saturating_add(1),
                    component: AlignmentComponent::SbxTailBase { read_index },
                });
            }
        });
    }
    let mut reference_position = record.start();
    let mut read_position = 0_usize;
    for (cigar_index, operation) in record.cigar().iter().enumerate() {
        match operation.kind {
            CigarOpKind::Insertion => {
                if tile.contains(reference_position) {
                    let state = classify_sbx_insertion(
                        record.flags(),
                        bases,
                        qualities,
                        read_position,
                        operation.len as usize,
                        options.discordant_quality_maximum,
                    )
                    .map_or(
                        SbxInsertionToken::Unavailable,
                        |state| match state {
                            SbxInsertionState::Concordant => SbxInsertionToken::Concordant,
                            SbxInsertionState::PartlyDiscordant => {
                                SbxInsertionToken::PartlyDiscordant
                            }
                            SbxInsertionState::FullyDiscordant => {
                                SbxInsertionToken::FullyDiscordant
                            }
                        },
                    );
                    if let Ok(cigar_index) = u32::try_from(cigar_index) {
                        output.push(PreparedOverlay {
                            start: reference_position.min(record.end().saturating_sub(1)),
                            end: reference_position
                                .min(record.end().saturating_sub(1))
                                .saturating_add(1),
                            component: AlignmentComponent::SbxInsertion { cigar_index, state },
                        });
                    }
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
}

fn hidden_sbx_tail_bounds(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    color_mode: AlignmentColorMode,
) -> Option<SbxTailBounds> {
    let AlignmentColorMode::Sbx(options) = color_mode else {
        return None;
    };
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = selection.sequence(index)
    else {
        return None;
    };
    sbx_tail_bounds(
        record.flags(),
        bases,
        qualities,
        options.simplex_tail_quality_maximum,
    )
    .ok()
}

fn push_unavailable(
    output: &mut Vec<PreparedOverlay>,
    record: rgv_hts::AlignmentRecordView<'_>,
    tile: GenomicInterval,
    mode: SpecialistMode,
    reason: SpecialistUnavailableReason,
) {
    let start = record.start().max(tile.start());
    let end = record.end().min(tile.end());
    if start < end {
        output.push(PreparedOverlay {
            start,
            end,
            component: AlignmentComponent::SpecialistUnavailable { mode, reason },
        });
    }
}

fn for_each_aligned_base(
    record: rgv_hts::AlignmentRecordView<'_>,
    mut visit: impl FnMut(usize, u64, usize),
) {
    let mut reference_position = record.start();
    let mut read_position = 0_usize;
    for (cigar_index, operation) in record.cigar().iter().enumerate() {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                for offset in 0..operation.len as usize {
                    visit(
                        read_position.saturating_add(offset),
                        reference_position.saturating_add(offset as u64),
                        cigar_index,
                    );
                }
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
}

fn reference_position_for_read_position(
    alignment_start: u64,
    cigar: &[rgv_hts::CigarOp],
    target: usize,
) -> Option<u64> {
    let mut reference_position = alignment_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let read_end = read_position.saturating_add(operation.len as usize);
                if (read_position..read_end).contains(&target) {
                    return Some(
                        reference_position.saturating_add((target - read_position) as u64),
                    );
                }
                read_position = read_end;
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    None
}

const fn map_ultima_quality_source(value: UltimaQualitySource) -> UltimaQualitySourceToken {
    match value {
        UltimaQualitySource::TpDirection => UltimaQualitySourceToken::TpDirection,
        UltimaQualitySource::TpReadMaximumFallback => {
            UltimaQualitySourceToken::TpReadMaximumFallback
        }
        UltimaQualitySource::MinimumHomopolymer => UltimaQualitySourceToken::MinimumHomopolymer,
        UltimaQualitySource::T0 => UltimaQualitySourceToken::T0,
        UltimaQualitySource::MaximumHomopolymerLimit => {
            UltimaQualitySourceToken::MaximumHomopolymerLimit
        }
    }
}

const fn map_ultima_unavailable(value: UltimaEventUnavailable) -> UltimaUnavailableToken {
    match value {
        UltimaEventUnavailable::NotBracketedByAlignedBases => {
            UltimaUnavailableToken::NotBracketedByAlignedBases
        }
        UltimaEventUnavailable::ReferenceUnavailable => {
            UltimaUnavailableToken::ReferenceUnavailable
        }
        UltimaEventUnavailable::AmbiguousHomopolymer => {
            UltimaUnavailableToken::AmbiguousHomopolymer
        }
        UltimaEventUnavailable::NonHomopolymer => UltimaUnavailableToken::NonHomopolymer,
        UltimaEventUnavailable::T0Unavailable => UltimaUnavailableToken::T0Unavailable,
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn emit_record_primitives(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    record: rgv_hts::AlignmentRecordView<'_>,
    sequence: Option<SequenceView<'_>>,
    body: AlignmentBodyColorToken,
    lod: GeometryLod,
    tile: GenomicInterval,
    tile_start: u64,
    reference: Option<&ReferenceSlice>,
    style_contract: AlignmentStyleContract,
    read_label: Option<&(AlignmentLabelKind, String)>,
    family: AlignmentPageFamily,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Result<(), SemanticPageError> {
    let emit_geometry = family == AlignmentPageFamily::ReadGeometry;
    let emit_labels = family == AlignmentPageFamily::Labels;
    let style = AlignmentStyleToken {
        flags: record.flags().bits(),
        mapping_quality: record.mapping_quality(),
        operation: None,
        body,
    };
    let mut draw_order = 0_u32;
    if lod == GeometryLod::Overview {
        if emit_geometry
            && let Some((visible_start, visible_end)) =
                visible_reference_bounds(record, hidden_sbx_tail)
        {
            push_span(
                output,
                handle,
                row_offset,
                visible_start,
                visible_end,
                tile,
                tile_start,
                AlignmentComponent::ReadSpan,
                style,
                draw_order,
            );
        }
        return Ok(());
    }

    if emit_labels && let Some((kind, text)) = read_label {
        push_label_span(
            output,
            labels,
            handle,
            row_offset,
            record.start(),
            record.end(),
            tile,
            tile_start,
            *kind,
            text,
            style,
            10_000,
        )?;
    }

    let mut reference_position = record.start();
    let mut read_position = 0_u32;
    let cigar = record.cigar();
    if emit_geometry && cigar.is_empty() && hidden_sbx_tail.is_none() {
        push_span(
            output,
            handle,
            row_offset,
            record.start(),
            record.end(),
            tile,
            tile_start,
            AlignmentComponent::ReadSpan,
            style,
            draw_order,
        );
    }
    for (index, operation) in cigar.iter().copied().enumerate() {
        let cigar_index =
            u32::try_from(index).map_err(|_| SemanticPageError::CigarIndexOverflow)?;
        let operation_style = AlignmentStyleToken {
            operation: Some(operation.kind.into()),
            ..style
        };
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry
                    && let Some((visible_read_start, visible_read_end)) =
                        visible_read_range(read_position, operation.len, hidden_sbx_tail)
                {
                    let visible_start = reference_position
                        .saturating_add(u64::from(visible_read_start - read_position));
                    let visible_end = reference_position
                        .saturating_add(u64::from(visible_read_end - read_position));
                    push_span(
                        output,
                        handle,
                        row_offset,
                        visible_start,
                        visible_end,
                        tile,
                        tile_start,
                        AlignmentComponent::AlignedSegment {
                            cigar_index,
                            operation: operation.kind.into(),
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_geometry && lod == GeometryLod::Base {
                    emit_bases(
                        output,
                        handle,
                        row_offset,
                        cigar_index,
                        operation.kind,
                        reference_position,
                        read_position,
                        operation.len,
                        sequence,
                        tile,
                        tile_start,
                        reference,
                        style_contract.base_display,
                        hidden_sbx_tail,
                        operation_style,
                        &mut draw_order,
                    )?;
                }
                reference_position = end;
                read_position = read_position.saturating_add(operation.len);
            }
            CigarOpKind::Deletion => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry && operation.len >= style_contract.minimum_indel_size {
                    push_span(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentComponent::Deletion { cigar_index },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_span(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentLabelKind::DeletionLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry {
                    push_span(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentComponent::ReferenceSkip { cigar_index },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_span(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentLabelKind::SkipLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::Insertion => {
                if emit_geometry
                    && operation.len >= style_contract.minimum_indel_size
                    && visible_read_range(read_position, operation.len, hidden_sbx_tail).is_some()
                {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::Insertion {
                            cigar_index,
                            read_start: read_position,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_point(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentLabelKind::InsertionLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                read_position = read_position.saturating_add(operation.len);
            }
            CigarOpKind::SoftClip => {
                if emit_geometry
                    && visible_read_range(read_position, operation.len, hidden_sbx_tail).is_some()
                {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::SoftClip {
                            cigar_index,
                            read_start: read_position,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_point(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentLabelKind::ClipLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                read_position = read_position.saturating_add(operation.len);
            }
            CigarOpKind::HardClip => {
                if emit_geometry {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::HardClip {
                            cigar_index,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_point(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentLabelKind::ClipLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
            }
            CigarOpKind::Padding => {
                if emit_geometry {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::Padding {
                            cigar_index,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
            }
        }
    }
    if emit_geometry
        && style_contract.show_direction_terminals
        && let Some((visible_start, visible_end)) =
            visible_reference_bounds(record, hidden_sbx_tail)
    {
        let terminal = if record.flags().is_reverse() {
            visible_start
        } else {
            visible_end.saturating_sub(1)
        };
        push_point(
            output,
            handle,
            row_offset,
            terminal,
            record.start(),
            record.end(),
            tile,
            tile_start,
            AlignmentComponent::DirectionTerminal,
            style,
            draw_order,
        );
        mark_directional_read_geometry(output, handle, row_offset, record, tile, tile_start);
    }
    Ok(())
}

fn mark_directional_read_geometry(
    output: &mut [AlignmentSemanticPrimitive],
    handle: AlignmentHandle,
    row_offset: u32,
    record: rgv_hts::AlignmentRecordView<'_>,
    tile: GenomicInterval,
    tile_start: u64,
) {
    let geometry = if record.flags().is_reverse() {
        AlignmentGeometryToken::DirectionalTerminalLeft
    } else {
        AlignmentGeometryToken::DirectionalTerminalRight
    };
    let mut mark_block = |start: u64, end: u64| {
        if start >= end {
            return;
        }
        let terminal = if record.flags().is_reverse() {
            start
        } else {
            end.saturating_sub(1)
        };
        if !tile.contains(terminal) {
            return;
        }
        let offset = terminal.saturating_sub(tile_start);
        for primitive in output.iter_mut().filter(|primitive| {
            primitive.handle == handle
                && primitive.row_offset == row_offset
                && primitive.contains_offset(offset)
                && matches!(
                    primitive.component,
                    AlignmentComponent::ReadSpan
                        | AlignmentComponent::AlignedSegment { .. }
                        | AlignmentComponent::Base { .. }
                        | AlignmentComponent::DirectionTerminal
                )
        }) {
            primitive.geometry = geometry;
        }
    };

    if record.cigar().is_empty() {
        mark_block(record.start(), record.end());
        return;
    }
    let mut reference_position = record.start();
    let mut block_start = None;
    let mut block_end = None;
    for operation in record.cigar().iter().copied() {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                block_start.get_or_insert(reference_position);
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                block_end = Some(reference_position);
            }
            CigarOpKind::Deletion => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::ReferenceSkip => {
                if let (Some(start), Some(end)) = (block_start.take(), block_end.take()) {
                    mark_block(start, end);
                }
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    if let (Some(start), Some(end)) = (block_start, block_end) {
        mark_block(start, end);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_bases(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    cigar_index: u32,
    cigar_kind: CigarOpKind,
    reference_start: u64,
    read_start: u32,
    length: u32,
    sequence: Option<SequenceView<'_>>,
    tile: GenomicInterval,
    tile_start: u64,
    reference: Option<&ReferenceSlice>,
    base_display: ReadBaseDisplayMode,
    hidden_sbx_tail: Option<SbxTailBounds>,
    style: AlignmentStyleToken,
    draw_order: &mut u32,
) -> Result<(), SemanticPageError> {
    let Some(sequence) = sequence else {
        return Err(SemanticPageError::SequencePayloadUnavailable);
    };
    let SequenceView::Present { bases, qualities } = sequence else {
        return match sequence {
            SequenceView::Missing => Ok(()),
            SequenceView::NotLoaded => Err(SemanticPageError::SequencePayloadUnavailable),
            SequenceView::Present { .. } => unreachable!(),
        };
    };
    for offset in 0..length {
        let position = reference_start.saturating_add(u64::from(offset));
        if !tile.contains(position) {
            continue;
        }
        let read_index = read_start
            .checked_add(offset)
            .ok_or(SemanticPageError::ReadCoordinateOverflow)?;
        let index =
            usize::try_from(read_index).map_err(|_| SemanticPageError::ReadCoordinateOverflow)?;
        if hidden_sbx_tail.is_some_and(|tail| tail.is_tail(index)) {
            continue;
        }
        let base = *bases
            .get(index)
            .ok_or(SemanticPageError::InvalidSequenceGeometry)?;
        let quality = qualities.and_then(|qualities| qualities.get(index).copied());
        let classification = classify_aligned_base(
            cigar_kind,
            Some(base),
            reference.and_then(|reference| reference.base_at(position)),
        );
        let visible = match base_display {
            ReadBaseDisplayMode::All => true,
            ReadBaseDisplayMode::Hidden => false,
            ReadBaseDisplayMode::Mismatches => !matches!(
                classification,
                AlignedBaseClassification::Match | AlignedBaseClassification::Unknown
            ),
        };
        if !visible {
            continue;
        }
        output.push(AlignmentSemanticPrimitive {
            start_offset: position - tile_start,
            end_offset: position - tile_start + 1,
            row_offset,
            handle,
            component: AlignmentComponent::Base {
                cigar_index,
                read_index,
                base,
                quality,
                classification,
            },
            geometry: AlignmentGeometryToken::Rectangle,
            style,
            draw_order: *draw_order,
        });
        *draw_order = draw_order.saturating_add(1);
    }
    Ok(())
}

fn visible_read_range(
    start: u32,
    length: u32,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Option<(u32, u32)> {
    let end = start.checked_add(length)?;
    let Some(tail) = hidden_sbx_tail else {
        return (start < end).then_some((start, end));
    };
    let retained_start = u32::try_from(tail.retained_start).unwrap_or(u32::MAX);
    let retained_end = u32::try_from(tail.retained_end).unwrap_or(u32::MAX);
    let visible_start = start.max(retained_start);
    let visible_end = end.min(retained_end);
    (visible_start < visible_end).then_some((visible_start, visible_end))
}

fn visible_reference_bounds(
    record: rgv_hts::AlignmentRecordView<'_>,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Option<(u64, u64)> {
    let Some(tail) = hidden_sbx_tail else {
        return (record.start() < record.end()).then_some((record.start(), record.end()));
    };
    let mut minimum = None::<u64>;
    let mut maximum = None::<u64>;
    for_each_aligned_base(record, |read_index, position, _| {
        if tail.is_tail(read_index) {
            return;
        }
        minimum = Some(minimum.map_or(position, |value| value.min(position)));
        maximum = Some(maximum.map_or(position, |value| value.max(position)));
    });
    minimum.zip(maximum.map(|position| position.saturating_add(1)))
}

fn read_label_for_record(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_hts::AlignmentRecordView<'_>,
    mode: AlignmentReadLabelMode,
) -> Result<Option<(AlignmentLabelKind, String)>, SemanticPageError> {
    Ok(match mode {
        AlignmentReadLabelMode::Hidden => None,
        AlignmentReadLabelMode::ReadName => (!record.read_name().is_empty()).then(|| {
            (
                AlignmentLabelKind::ReadName,
                String::from_utf8_lossy(record.read_name()).into_owned(),
            )
        }),
        AlignmentReadLabelMode::Tag(tag) => selection
            .auxiliary_field(index, tag)?
            .and_then(|field| auxiliary_value_label(&field.value))
            .map(|label| (AlignmentLabelKind::AuxiliaryTag(tag), label)),
    })
}

fn auxiliary_value_label(value: &AuxValue) -> Option<String> {
    Some(match value {
        AuxValue::Character(value) => char::from(*value).to_string(),
        AuxValue::Signed(value) => value.to_string(),
        AuxValue::Unsigned(value) => value.to_string(),
        AuxValue::Float(value) => value.to_string(),
        AuxValue::Double(value) => value.to_string(),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => value.clone(),
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => {
            return None;
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn push_label_span(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    start: u64,
    end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    kind: AlignmentLabelKind,
    text: &str,
    style: AlignmentStyleToken,
    draw_order: u32,
) -> Result<(), SemanticPageError> {
    let semantic_start = start;
    let semantic_end = end;
    let start = start.max(tile.start());
    let end = end.min(tile.end());
    if start >= end || text.is_empty() {
        return Ok(());
    }
    let id = label_id(kind, text, semantic_start, semantic_end);
    labels.push(PendingLabel {
        id,
        text: text.to_owned(),
    });
    output.push(AlignmentSemanticPrimitive {
        start_offset: start - tile_start,
        end_offset: end - tile_start,
        row_offset,
        handle,
        component: AlignmentComponent::Label { kind, id },
        geometry: AlignmentGeometryToken::Rectangle,
        style,
        draw_order,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_label_point(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    kind: AlignmentLabelKind,
    text: &str,
    style: AlignmentStyleToken,
    draw_order: u32,
) -> Result<(), SemanticPageError> {
    let position = position.clamp(record_start, record_end.saturating_sub(1));
    push_label_span(
        output,
        labels,
        handle,
        row_offset,
        position,
        position.saturating_add(1),
        tile,
        tile_start,
        kind,
        text,
        style,
        draw_order,
    )
}

fn label_id(
    kind: AlignmentLabelKind,
    text: &str,
    semantic_start: u64,
    semantic_end: u64,
) -> ContractFingerprint {
    let mut hash = StableHasher::default();
    hash.write(b"alignment-semantic-label-v2\0");
    match kind {
        AlignmentLabelKind::ReadName => hash.write(&[0]),
        AlignmentLabelKind::AuxiliaryTag(tag) => {
            hash.write(&[1]);
            hash.write(&tag);
        }
        AlignmentLabelKind::InsertionLength => hash.write(&[2]),
        AlignmentLabelKind::DeletionLength => hash.write(&[3]),
        AlignmentLabelKind::SkipLength => hash.write(&[4]),
        AlignmentLabelKind::ClipLength => hash.write(&[5]),
    }
    hash.write(text.as_bytes());
    hash.write(&semantic_start.to_le_bytes());
    hash.write(&semantic_end.to_le_bytes());
    hash.finish()
}

fn finalize_labels(
    mut pending: Vec<PendingLabel>,
) -> Result<Vec<AlignmentSemanticLabel>, SemanticPageError> {
    pending.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.text.cmp(&right.text))
    });
    let mut labels = Vec::<AlignmentSemanticLabel>::with_capacity(pending.len());
    for label in pending {
        if let Some(previous) = labels.last()
            && previous.id == label.id
        {
            if previous.text.as_ref() != label.text {
                return Err(SemanticPageError::LabelFingerprintCollision(label.id));
            }
            continue;
        }
        labels.push(AlignmentSemanticLabel {
            id: label.id,
            text: Arc::from(label.text),
        });
    }
    labels.shrink_to_fit();
    Ok(labels)
}

#[allow(clippy::too_many_arguments)]
fn push_span(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    start: u64,
    end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    component: AlignmentComponent,
    style: AlignmentStyleToken,
    draw_order: u32,
) {
    let start = start.max(tile.start());
    let end = end.min(tile.end());
    if start >= end {
        return;
    }
    output.push(AlignmentSemanticPrimitive {
        start_offset: start - tile_start,
        end_offset: end - tile_start,
        row_offset,
        handle,
        component,
        geometry: geometry_for(component, style),
        style,
        draw_order,
    });
}

#[allow(clippy::too_many_arguments)]
fn push_point(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    component: AlignmentComponent,
    style: AlignmentStyleToken,
    draw_order: u32,
) {
    let position = position.clamp(record_start, record_end.saturating_sub(1));
    if !tile.contains(position) {
        return;
    }
    output.push(AlignmentSemanticPrimitive {
        start_offset: position - tile_start,
        end_offset: position - tile_start + 1,
        row_offset,
        handle,
        component,
        geometry: geometry_for(component, style),
        style,
        draw_order,
    });
}

fn geometry_for(
    component: AlignmentComponent,
    style: AlignmentStyleToken,
) -> AlignmentGeometryToken {
    match component {
        AlignmentComponent::Deletion { .. }
        | AlignmentComponent::ReferenceSkip { .. }
        | AlignmentComponent::PairConnector { .. } => AlignmentGeometryToken::EventLine,
        AlignmentComponent::Insertion { .. }
        | AlignmentComponent::SoftClip { .. }
        | AlignmentComponent::HardClip { .. }
        | AlignmentComponent::Padding { .. }
        | AlignmentComponent::UltimaIndel { .. }
        | AlignmentComponent::SbxInsertion { .. } => AlignmentGeometryToken::PointMarker,
        AlignmentComponent::DirectionTerminal => {
            if rgv_hts::AlignmentFlags::from_bits(style.flags).is_reverse() {
                AlignmentGeometryToken::DirectionalTerminalLeft
            } else {
                AlignmentGeometryToken::DirectionalTerminalRight
            }
        }
        AlignmentComponent::Label { .. }
        | AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::ModifiedBase { .. }
        | AlignmentComponent::BisulfiteBase { .. }
        | AlignmentComponent::SmrtKinetics { .. }
        | AlignmentComponent::SbxTailBase { .. }
        | AlignmentComponent::SpecialistUnavailable { .. }
        | AlignmentComponent::Base { .. } => AlignmentGeometryToken::Rectangle,
    }
}

fn validate_dependencies(
    key: AlignmentPageKey,
    evidence: &EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    presentation: PresentationContract,
    reference: Option<&ReferenceSlice>,
) -> Result<(), SemanticPageError> {
    if key.source_revision != evidence.source_revision()
        || key.source_revision != atlas.source_revision()
    {
        return Err(SemanticPageError::SourceRevisionMismatch);
    }
    if key.evidence_revision != evidence.revision() {
        return Err(SemanticPageError::EvidenceRevisionMismatch);
    }
    if atlas.record_contract() != evidence.record_contract() {
        return Err(SemanticPageError::RecordContractMismatch);
    }
    if key.row_atlas_revision != atlas.revision() {
        return Err(SemanticPageError::RowAtlasRevisionMismatch);
    }
    if key.layout != atlas.layout_contract().fingerprint() {
        return Err(SemanticPageError::LayoutContractMismatch);
    }
    if key.presentation != presentation.page_fingerprint(key.family)
        || key.lod != presentation.geometry_lod
    {
        return Err(SemanticPageError::PresentationContractMismatch);
    }
    let page_uses_reference = (key.family == AlignmentPageFamily::ReadGeometry
        && key.lod == GeometryLod::Base)
        || (key.family == AlignmentPageFamily::SpecialistOverlay
            && matches!(
                presentation.style.color_mode,
                AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
            ));
    if page_uses_reference && presentation.reference.is_some() && reference.is_none() {
        return Err(SemanticPageError::ReferenceUnavailable);
    }
    validate_style_capabilities(evidence, presentation, key.family)?;
    Ok(())
}

fn validate_style_capabilities(
    evidence: &EvidenceRoot,
    presentation: PresentationContract,
    family: AlignmentPageFamily,
) -> Result<(), SemanticPageError> {
    let style = presentation.style;
    let hides_sbx_tail = matches!(
        style.color_mode,
        AlignmentColorMode::Sbx(options)
            if options.simplex_tail_mode == SbxSimplexTailMode::Hide
    );
    let sequence_required = (family == AlignmentPageFamily::ReadGeometry
        && (presentation.geometry_lod == GeometryLod::Base || hides_sbx_tail))
        || (family == AlignmentPageFamily::SpecialistOverlay
            && (style.show_modified_bases
                || matches!(
                    style.color_mode,
                    AlignmentColorMode::Bisulfite(_)
                        | AlignmentColorMode::SmrtKinetics(_)
                        | AlignmentColorMode::Ultima(_)
                        | AlignmentColorMode::Sbx(_)
                )));
    if sequence_required && evidence.payload().sequence() != CapabilityRequirement::Required {
        return Err(SemanticPageError::StyleSequencePayloadRequired);
    }
    let quality_required = (family == AlignmentPageFamily::ReadGeometry
        && ((presentation.geometry_lod == GeometryLod::Base && style.shade_bases_by_quality)
            || hides_sbx_tail))
        || (family == AlignmentPageFamily::SpecialistOverlay
            && matches!(
                style.color_mode,
                AlignmentColorMode::Ultima(_) | AlignmentColorMode::Sbx(_)
            ));
    if quality_required && evidence.payload().qualities() != CapabilityRequirement::Required {
        return Err(SemanticPageError::StyleQualityPayloadRequired);
    }
    // Reference-backed specialist modes deliberately remain buildable without a FASTA slice.
    // Their page emits `SpecialistUnavailable::MissingReference` while independent modified-base
    // overlays and the core read geometry remain truthful. A later immutable scene upgrades the
    // same family when the requested reference revision becomes available.

    let mut tags = Vec::<[u8; 2]>::new();
    if family == AlignmentPageFamily::ReadGeometry {
        match style.color_mode {
            AlignmentColorMode::ReadGroup => tags.push(*b"RG"),
            AlignmentColorMode::Haplotype => tags.push(*b"HP"),
            AlignmentColorMode::Tag(tag) => tags.push(tag),
            _ => {}
        }
    }
    if family == AlignmentPageFamily::SpecialistOverlay {
        match style.color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => {
                tags.push(smrt_kinetics_tag(mode, false));
                tags.push(smrt_kinetics_tag(mode, true));
            }
            AlignmentColorMode::Ultima(_) => tags.extend([*b"tp", *b"t0"]),
            _ => {}
        }
    }
    if family == AlignmentPageFamily::Labels
        && let AlignmentReadLabelMode::Tag(tag) = style.read_labels
    {
        tags.push(tag);
    }
    if family == AlignmentPageFamily::SpecialistOverlay && style.show_modified_bases {
        tags.extend([*b"MM", *b"Mm", *b"ML", *b"Ml"]);
    }
    tags.sort_unstable();
    tags.dedup();
    if !evidence.payload().all_auxiliary_tags() {
        for tag in tags {
            if evidence
                .payload()
                .auxiliary_tags()
                .binary_search(&tag)
                .is_err()
            {
                return Err(SemanticPageError::StyleAuxiliaryPayloadRequired(tag));
            }
        }
    }
    Ok(())
}

fn semantic_page_base_scratch_bytes(
    selected_records: usize,
    atlas_records: usize,
    connectors: usize,
) -> usize {
    hash_table_scratch_bytes(selected_records, size_of::<(AlignmentHandle, usize)>())
        .saturating_add(atlas_records.saturating_mul(size_of::<RowAtlasRecordHit>()))
        .saturating_add(connectors.saturating_mul(size_of::<AlignmentRowConnector>()))
}

fn semantic_page_dynamic_scratch_bytes(
    selection: &crate::EvidenceSelection,
    atlas_records: &[RowAtlasRecordHit],
    by_handle: &HashMap<AlignmentHandle, usize>,
    key: AlignmentPageKey,
    presentation: PresentationContract,
) -> Result<usize, SemanticPageError> {
    let mut bytes = hash_table_scratch_bytes(
        atlas_records.len(),
        size_of::<(AlignmentHandle, PreparedRecordSemantics)>(),
    );
    let mut overlay_slots = 0_usize;
    let mut pending_label_slots = 0_usize;
    let mut pending_label_bytes = 0_usize;
    let detailed = matches!(key.lod, GeometryLod::Read | GeometryLod::Base);

    for hit in atlas_records {
        let index = *by_handle.get(&hit.record.handle()).ok_or(
            SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
        )?;
        let record = selection
            .record_view(index)
            .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
        let sequence_len = match selection.sequence(index) {
            Some(SequenceView::Present { bases, .. }) => bases.len(),
            Some(SequenceView::Missing | SequenceView::NotLoaded) | None => 0,
        };
        let aligned_bases = record.cigar().iter().fold(0_usize, |total, operation| {
            total.saturating_add(
                if matches!(
                    operation.kind,
                    CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch
                ) {
                    operation.len as usize
                } else {
                    0
                },
            )
        });
        let auxiliary_bytes = selection.auxiliary_encoded_len(index).unwrap_or_default();
        // Decoded tag values, kinetics arrays, and modification calls are temporary owned values.
        bytes = bytes.saturating_add(auxiliary_bytes.saturating_mul(8));

        if key.family == AlignmentPageFamily::Labels && detailed {
            let read_label_bytes = match presentation.style.read_labels {
                AlignmentReadLabelMode::Hidden => 0,
                // Invalid UTF-8 replacement characters may expand one input byte to three.
                AlignmentReadLabelMode::ReadName => record.read_name().len().saturating_mul(3),
                AlignmentReadLabelMode::Tag(tag) => selection
                    .auxiliary_value_payload_len(index, tag)?
                    .unwrap_or_default()
                    .saturating_mul(3)
                    .saturating_add(32),
            };
            if read_label_bytes > 0 {
                pending_label_slots = pending_label_slots.saturating_add(1);
                pending_label_bytes = pending_label_bytes.saturating_add(read_label_bytes);
            }
            let events = record
                .cigar()
                .iter()
                .filter(|operation| {
                    operation.len >= presentation.style.minimum_event_label_size
                        && matches!(
                            operation.kind,
                            CigarOpKind::Insertion
                                | CigarOpKind::Deletion
                                | CigarOpKind::ReferenceSkip
                                | CigarOpKind::SoftClip
                                | CigarOpKind::HardClip
                        )
                })
                .count();
            pending_label_slots = pending_label_slots.saturating_add(events);
            pending_label_bytes = pending_label_bytes.saturating_add(events.saturating_mul(10));
        }

        if key.family == AlignmentPageFamily::SpecialistOverlay && detailed {
            if presentation.style.show_modified_bases {
                let mm_bytes = selection
                    .auxiliary_value_payload_len(index, *b"MM")?
                    .unwrap_or_default()
                    .max(
                        selection
                            .auxiliary_value_payload_len(index, *b"Mm")?
                            .unwrap_or_default(),
                    );
                // Each explicit call consumes MM text, while canonical/unmodified display can
                // emit at most one base per sequence position and modification group.
                overlay_slots = overlay_slots
                    .saturating_add(mm_bytes)
                    .saturating_add(sequence_len.saturating_mul(mm_bytes));
            }
            let specialist_slots = match presentation.style.color_mode {
                AlignmentColorMode::Bisulfite(_)
                | AlignmentColorMode::SmrtKinetics(_)
                | AlignmentColorMode::Sbx(_) => aligned_bases
                    .saturating_add(record.cigar().len())
                    .saturating_add(1),
                AlignmentColorMode::Ultima(_) => aligned_bases
                    .saturating_add(record.cigar().len().saturating_mul(2))
                    .saturating_add(1),
                _ => 0,
            };
            overlay_slots = overlay_slots.saturating_add(specialist_slots);
        }
    }

    bytes = bytes
        .saturating_add(
            overlay_slots
                .saturating_mul(2)
                .saturating_mul(size_of::<PreparedOverlay>()),
        )
        // Modified-base decoding temporarily retains calls and canonical-position sets as well
        // as the final prepared overlays.
        .saturating_add(overlay_slots.saturating_mul(64))
        .saturating_add(
            pending_label_slots
                .saturating_mul(2)
                .saturating_mul(size_of::<PendingLabel>()),
        )
        .saturating_add(pending_label_bytes);
    Ok(bytes)
}

fn hash_table_scratch_bytes(entries: usize, entry_bytes: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    entries
        .saturating_mul(2)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .saturating_mul(entry_bytes.saturating_add(1))
}

const fn semantic_page_bytes(
    primitives: usize,
    row_offsets: usize,
    labels: usize,
    label_bytes: usize,
) -> usize {
    size_of::<AlignmentSemanticPage>()
        .saturating_add(primitives.saturating_mul(size_of::<AlignmentSemanticPrimitive>()))
        .saturating_add(row_offsets.saturating_mul(size_of::<u32>()))
        .saturating_add(labels.saturating_mul(size_of::<AlignmentSemanticLabel>()))
        .saturating_add(label_bytes)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SemanticPageError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    EvidenceSelection(#[from] EvidenceSelectionError),
    #[error("semantic page source revision does not match evidence and rows")]
    SourceRevisionMismatch,
    #[error("semantic page evidence revision is stale or incompatible")]
    EvidenceRevisionMismatch,
    #[error("semantic page row atlas uses a different physical-record contract")]
    RecordContractMismatch,
    #[error("semantic page row-atlas revision is stale or incompatible")]
    RowAtlasRevisionMismatch,
    #[error("semantic page layout contract does not match its row atlas")]
    LayoutContractMismatch,
    #[error("semantic page presentation contract does not match its key")]
    PresentationContractMismatch,
    #[error("semantic page tile {0:?} is not completely certified")]
    UncertifiedTile(GenomicInterval),
    #[error("semantic page row coordinate overflowed")]
    RowCoordinateOverflow,
    #[error("row block beginning at {start} is outside an atlas with {total} rows")]
    RowBlockOutsideAtlas { start: u32, total: u32 },
    #[error("row atlas referenced handle {0:?} absent from tile evidence")]
    MissingEvidenceHandle(AlignmentHandle),
    #[error("semantic page evidence contains an invalid record reference")]
    InvalidEvidenceRecord,
    #[error("semantic page primitive count cannot be represented")]
    PrimitiveCountOverflow,
    #[error("CIGAR operation index cannot be represented")]
    CigarIndexOverflow,
    #[error("read coordinate cannot be represented")]
    ReadCoordinateOverflow,
    #[error("base-detail geometry requires sequence payload")]
    SequencePayloadUnavailable,
    #[error("sequence payload does not satisfy CIGAR geometry")]
    InvalidSequenceGeometry,
    #[error("the presentation contract requires a reference slice")]
    ReferenceUnavailable,
    #[error("alignment style requires retained sequence payload")]
    StyleSequencePayloadRequired,
    #[error("alignment style requires retained base-quality payload")]
    StyleQualityPayloadRequired,
    #[error("alignment style requires a loaded interpretation reference")]
    StyleReferenceRequired,
    #[error("alignment style requires retained auxiliary tag {0:?}")]
    StyleAuxiliaryPayloadRequired([u8; 2]),
    #[error("two distinct semantic labels produced fingerprint {0:?}")]
    LabelFingerprintCollision(ContractFingerprint),
    #[error("semantic-page preparation for handle {0:?} was lost")]
    MissingPreparedSemantics(AlignmentHandle),
    #[error(transparent)]
    PackedRecord(#[from] rgv_hts::PackedRecordError),
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, num::NonZeroU64, sync::Arc};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, BisulfiteContext, ContigId,
        ReadStackingMode, SbxDisplayOptions, SmrtKineticsMode, UltimaDisplayOptions,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, SequenceData, TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AllocationClass, CapabilityRequirement, CompleteCoverage, EvidenceAcquisition,
        EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance, PayloadContract,
        PayloadSidecar, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(17);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn layout_contract() -> crate::LayoutContract {
        crate::LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        }
    }

    fn record(
        name: &[u8],
        start: u64,
        end: u64,
        cigar: &[(CigarOpKind, u32)],
        sequence: &[u8],
    ) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: cigar
                .iter()
                .map(|(kind, len)| CigarOp {
                    kind: *kind,
                    len: *len,
                })
                .collect(),
            sequence: if sequence.is_empty() {
                SequenceData::Missing
            } else {
                let mut qualities = vec![35; sequence.len()];
                if name == b"events" {
                    qualities[..3].fill(5);
                    *qualities.last_mut().unwrap() = 5;
                }
                SequenceData::Present {
                    bases: sequence.to_vec(),
                    qualities: Some(qualities),
                }
            },
            auxiliary_fields: (name == b"events")
                .then(|| {
                    vec![
                        AuxField {
                            tag: *b"RG",
                            value: AuxValue::String("group-a".to_owned()),
                        },
                        AuxField {
                            tag: *b"HP",
                            value: AuxValue::Unsigned(1),
                        },
                        AuxField {
                            tag: *b"ZZ",
                            value: AuxValue::String("label-value".to_owned()),
                        },
                        AuxField {
                            tag: *b"MM",
                            value: AuxValue::String("A+m,2;".to_owned()),
                        },
                        AuxField {
                            tag: *b"ML",
                            value: AuxValue::UnsignedArray(vec![220]),
                        },
                        AuxField {
                            tag: *b"ip",
                            value: AuxValue::UnsignedArray((1..=11).collect()),
                        },
                        AuxField {
                            tag: *b"tp",
                            value: AuxValue::SignedArray(vec![10; 11]),
                        },
                        AuxField {
                            tag: *b"t0",
                            value: AuxValue::String("IIIIIIIIIII".to_owned()),
                        },
                    ]
                })
                .unwrap_or_default(),
        }
    }

    fn evidence_and_atlas(
        ledger: &AllocationLedger,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let records = vec![
            record(
                b"events",
                20,
                30,
                &[
                    (CigarOpKind::SoftClip, 2),
                    (CigarOpKind::Match, 5),
                    (CigarOpKind::Insertion, 2),
                    (CigarOpKind::Deletion, 3),
                    (CigarOpKind::SequenceMismatch, 2),
                    (CigarOpKind::Padding, 1),
                    (CigarOpKind::HardClip, 1),
                ],
                b"AAACCCCCCCC",
            ),
            record(
                b"spliced",
                90,
                210,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::ReferenceSkip, 100),
                    (CigarOpKind::Match, 10),
                ],
                b"AAAAAAAAAAAAAAAAAAAA",
            ),
            record(b"cigarless", 250, 260, &[], b""),
        ];
        evidence_and_atlas_for_records(ledger, &records, layout_contract())
    }

    fn evidence_and_atlas_for_records(
        ledger: &AllocationLedger,
        records: &[AlignmentRecord],
        layout: crate::LayoutContract,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let mut decoded = AlignmentBatch::default();
        for (ordinal, record) in records.iter().enumerate() {
            decoded
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (7 << 16) | u64::try_from(ordinal + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = crate::CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let mut reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let payload_allocation = reservation.register(payload_bytes, 1).unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let mut reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core_allocation = reservation.register(footprint.core_bytes, 1).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage_interval = GenomicInterval::new(ContigId::new(0), 0, 300).unwrap();
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            coverage_interval,
            FilteredRecordProvenance {
                records_seen: records.len() as u64,
                eligible_records: records.len() as u64,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(5, 6)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(2).unwrap();
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(7, 8)),
            &evidence,
            coverage_interval,
            layout,
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        (evidence, atlas)
    }

    fn paired_evidence_and_atlas(
        ledger: &AllocationLedger,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let aligned = [(CigarOpKind::Match, 10)];
        let mut first = record(b"pair", 50, 60, &aligned, b"AAAAAAAAAA");
        first.flags = AlignmentFlags::from_bits(0x63);
        first.mate_contig = Some(ContigId::new(0));
        first.mate_start = Some(150);
        first.template_length = 110;
        let mut last = record(b"pair", 150, 160, &aligned, b"CCCCCCCCCC");
        last.flags = AlignmentFlags::from_bits(0x93);
        last.mate_contig = Some(ContigId::new(0));
        last.mate_start = Some(50);
        last.template_length = -110;
        let primary = record(b"split", 20, 30, &aligned, b"GGGGGGGGGG");
        let mut supplementary = record(b"split", 110, 120, &aligned, b"TTTTTTTTTT");
        supplementary.flags = AlignmentFlags::from_bits(0x800);
        let mut secondary = record(b"split", 180, 190, &aligned, b"ACGTACGTAC");
        secondary.flags = AlignmentFlags::from_bits(0x100);
        let layout = crate::LayoutContract {
            view_as_pairs: true,
            ..layout_contract()
        };
        evidence_and_atlas_for_records(
            ledger,
            &[first, last, primary, supplementary, secondary],
            layout,
        )
    }

    fn page(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        tile_coordinate: u64,
        lod: GeometryLod,
        reference: Option<&ReferenceSlice>,
    ) -> Arc<AlignmentSemanticPage> {
        page_with_style(
            ledger,
            evidence,
            atlas,
            tile_coordinate,
            lod,
            reference,
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::ReadGeometry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn page_with_style(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        tile_coordinate: u64,
        lod: GeometryLod,
        reference: Option<&ReferenceSlice>,
        style: crate::AlignmentStyleContract,
        family: AlignmentPageFamily,
    ) -> Arc<AlignmentSemanticPage> {
        let presentation = PresentationContract {
            geometry_lod: lod,
            style,
            reference: reference
                .map(|_| crate::ReferenceRevision::new(RevisionFingerprint::new(10, 11))),
            annotation: None,
        };
        let key = AlignmentPageKey::new(
            evidence.source_revision(),
            evidence.revision(),
            atlas.revision(),
            atlas.layout_contract().fingerprint(),
            presentation.page_fingerprint(family),
            CanonicalGenomicTile::new(
                ContigId::new(0),
                tile_coordinate,
                NonZeroU64::new(100).unwrap(),
            ),
            CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap()),
            family,
            lod,
        );
        AlignmentSemanticPage::build(
            key,
            evidence,
            atlas,
            presentation,
            300,
            reference,
            ledger,
            ReservationKind::Normal,
            4 + tile_coordinate,
        )
        .unwrap()
    }

    fn pages_for_presentation(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
    ) -> Vec<Arc<AlignmentSemanticPage>> {
        (0..3)
            .flat_map(|tile| {
                presentation
                    .required_page_families()
                    .into_iter()
                    .map(move |family| {
                        page_with_style(
                            ledger,
                            evidence,
                            atlas,
                            tile,
                            presentation.geometry_lod,
                            None,
                            presentation.style,
                            family,
                        )
                    })
            })
            .collect()
    }

    #[test]
    fn specialist_modes_and_read_labels_are_scene_native() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let mut reference_bases = vec![b'A'; 300];
        // The first aligned C in `events` is at reference position 21. Make it a CG site.
        reference_bases[21] = b'C';
        reference_bases[22] = b'G';
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: reference_bases,
        };

        let default_page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::SpecialistOverlay,
        );
        assert!(default_page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            AlignmentComponent::ModifiedBase {
                probability: Some(220),
                state: ModificationState::Modified,
                ..
            }
        )));

        let styles = [
            (
                AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
                SpecialistMode::Bisulfite,
            ),
            (
                AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
                SpecialistMode::SmrtKinetics,
            ),
            (
                AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
                SpecialistMode::Ultima,
            ),
            (
                AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
                SpecialistMode::Sbx,
            ),
        ];
        for (color_mode, expected_mode) in styles {
            let style = crate::AlignmentStyleContract {
                color_mode,
                ..crate::AlignmentStyleContract::default()
            };
            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Base,
                Some(&reference),
                style,
                AlignmentPageFamily::SpecialistOverlay,
            );
            assert!(page.primitives().iter().any(|primitive| {
                match (expected_mode, primitive.component) {
                    (SpecialistMode::Bisulfite, AlignmentComponent::BisulfiteBase { .. })
                    | (SpecialistMode::SmrtKinetics, AlignmentComponent::SmrtKinetics { .. })
                    | (SpecialistMode::Ultima, AlignmentComponent::UltimaIndel { .. })
                    | (SpecialistMode::Sbx, AlignmentComponent::SbxInsertion { .. }) => true,
                    (_, AlignmentComponent::SpecialistUnavailable { mode, .. }) => {
                        mode == expected_mode
                    }
                    _ => false,
                }
            }));
        }

        let style = crate::AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::ReadName,
            ..crate::AlignmentStyleContract::default()
        };
        let page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Read,
            None,
            style,
            AlignmentPageFamily::Labels,
        );
        assert!(page.labels().iter().any(|label| label.text() == "events"));
        assert!(page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            AlignmentComponent::Label {
                kind: AlignmentLabelKind::ReadName,
                ..
            }
        )));

        let hidden_tail_style = crate::AlignmentStyleContract {
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: SbxSimplexTailMode::Hide,
                ..SbxDisplayOptions::default()
            }),
            ..crate::AlignmentStyleContract::default()
        };
        let hidden_tail = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
            hidden_tail_style,
            AlignmentPageFamily::ReadGeometry,
        );
        assert!(hidden_tail.primitives_at(0, 20).is_empty());
        assert!(
            hidden_tail
                .primitives_at(0, 21)
                .iter()
                .any(|primitive| matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                ))
        );
        assert!(
            hidden_tail
                .primitives_at(0, 29)
                .iter()
                .all(|primitive| !matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                        | AlignmentComponent::Base { .. }
                        | AlignmentComponent::DirectionTerminal
                ))
        );
    }

    #[test]
    fn long_and_spliced_geometry_splits_at_tiles_without_identity_or_length_loss() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let pages = (0..3)
            .map(|tile| page(&ledger, &evidence, &atlas, tile, GeometryLod::Read, None))
            .collect::<Vec<_>>();
        let aligned_bases = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                )
            })
            .map(|primitive| primitive.end_offset - primitive.start_offset)
            .sum::<u64>();
        let skipped_bases = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::ReferenceSkip { .. }
                )
            })
            .map(|primitive| primitive.end_offset - primitive.start_offset)
            .sum::<u64>();
        let spliced_handles = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::ReferenceSkip { .. }
                )
            })
            .map(|primitive| primitive.handle)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(aligned_bases, 27);
        assert_eq!(skipped_bases, 100);
        assert_eq!(spliced_handles.len(), 1);
        assert!(matches!(
            pages[1].pick(0, 150).unwrap().component,
            AlignmentComponent::ReferenceSkip { .. }
        ));
    }

    #[test]
    fn every_read_geometry_and_event_label_kind_is_scene_native() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let geometry = (0..3)
            .map(|tile| {
                page(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Base,
                    Some(&reference),
                )
            })
            .collect::<Vec<_>>();
        let components = geometry
            .iter()
            .flat_map(|page| {
                page.primitives()
                    .iter()
                    .map(|primitive| primitive.component)
            })
            .collect::<Vec<_>>();
        for present in [
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::AlignedSegment { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Base { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Insertion { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Deletion { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::ReferenceSkip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::SoftClip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::HardClip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Padding { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::DirectionTerminal)),
        ] {
            assert!(present);
        }

        let label_style = crate::AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::Hidden,
            minimum_event_label_size: 1,
            ..crate::AlignmentStyleContract::default()
        };
        let labels = (0..3)
            .flat_map(|tile| {
                page_with_style(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Read,
                    None,
                    label_style,
                    AlignmentPageFamily::Labels,
                )
                .primitives()
                .iter()
                .filter_map(|primitive| match primitive.component {
                    AlignmentComponent::Label { kind, .. } => Some(kind),
                    _ => None,
                })
                .collect::<Vec<_>>()
            })
            .collect::<std::collections::HashSet<_>>();
        for kind in [
            AlignmentLabelKind::InsertionLength,
            AlignmentLabelKind::DeletionLength,
            AlignmentLabelKind::SkipLength,
            AlignmentLabelKind::ClipLength,
        ] {
            assert!(labels.contains(&kind), "missing semantic label {kind:?}");
        }
    }

    #[test]
    fn span_lod_preserves_cigar_landmarks_and_both_read_directions() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let cigar = [
            (CigarOpKind::Match, 5),
            (CigarOpKind::Insertion, 2),
            (CigarOpKind::Match, 5),
        ];
        let forward = record(b"forward", 20, 30, &cigar, b"AAAAAAAAAAAA");
        let mut reverse = record(b"reverse", 40, 50, &cigar, b"CCCCCCCCCCCC");
        reverse.flags = AlignmentFlags::from_bits(0x10);
        let (evidence, atlas) =
            evidence_and_atlas_for_records(&ledger, &[forward, reverse], layout_contract());
        let page = page(&ledger, &evidence, &atlas, 0, GeometryLod::Span, None);

        assert!(
            page.primitives().iter().any(|primitive| matches!(
                primitive.component,
                AlignmentComponent::Insertion { .. }
            ))
        );
        assert!(page.primitives().iter().any(|primitive| {
            primitive.geometry == AlignmentGeometryToken::DirectionalTerminalRight
        }));
        assert!(page.primitives().iter().any(|primitive| {
            primitive.geometry == AlignmentGeometryToken::DirectionalTerminalLeft
        }));
        assert!(
            page.primitives().iter().all(|primitive| {
                !matches!(primitive.component, AlignmentComponent::Base { .. })
            })
        );
    }

    #[test]
    fn every_standard_read_colour_mode_produces_a_typed_body_token() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let modes = [
            AlignmentColorMode::None,
            AlignmentColorMode::Strand,
            AlignmentColorMode::MappingQuality,
            AlignmentColorMode::ReadGroup,
            AlignmentColorMode::Haplotype,
            AlignmentColorMode::TemplateLength,
            AlignmentColorMode::PairOrientation,
            AlignmentColorMode::UnexpectedPair,
            AlignmentColorMode::MateContig,
            AlignmentColorMode::FirstOfPairStrand,
            AlignmentColorMode::Tag(*b"ZZ"),
        ];
        for mode in modes {
            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Read,
                None,
                crate::AlignmentStyleContract {
                    color_mode: mode,
                    ..crate::AlignmentStyleContract::default()
                },
                AlignmentPageFamily::ReadGeometry,
            );
            let token = page
                .primitives()
                .iter()
                .find(|primitive| {
                    primitive.start_offset == 20
                        && matches!(
                            primitive.component,
                            AlignmentComponent::AlignedSegment { .. }
                        )
                })
                .unwrap()
                .style
                .body;
            let valid = match (mode, token) {
                (AlignmentColorMode::None, AlignmentBodyColorToken::Neutral)
                | (AlignmentColorMode::Strand, AlignmentBodyColorToken::Strand { .. })
                | (
                    AlignmentColorMode::MappingQuality,
                    AlignmentBodyColorToken::MappingQuality(_),
                )
                | (
                    AlignmentColorMode::ReadGroup
                    | AlignmentColorMode::Haplotype
                    | AlignmentColorMode::MateContig
                    | AlignmentColorMode::Tag(_),
                    AlignmentBodyColorToken::Category(_),
                )
                | (
                    AlignmentColorMode::TemplateLength,
                    AlignmentBodyColorToken::TemplateLength { .. },
                )
                | (
                    AlignmentColorMode::PairOrientation,
                    AlignmentBodyColorToken::PairOrientation(_),
                )
                | (AlignmentColorMode::UnexpectedPair, AlignmentBodyColorToken::PairStatus(_))
                | (
                    AlignmentColorMode::FirstOfPairStrand,
                    AlignmentBodyColorToken::FirstOfPairStrand(_),
                ) => true,
                _ => false,
            };
            assert!(valid, "wrong body token {token:?} for {mode:?}");
        }
    }

    #[test]
    fn base_page_and_picker_share_the_same_typed_component_identity() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let page = page(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
        );
        let substitutions = page
            .primitives()
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::Base {
                        classification: AlignedBaseClassification::Substitution
                            | AlignedBaseClassification::DeclaredSubstitution,
                        ..
                    }
                )
            })
            .count();

        assert!(substitutions >= 1);
        let hit = page.pick(0, 29).unwrap();
        assert!(matches!(
            hit.component,
            AlignmentComponent::DirectionTerminal
                | AlignmentComponent::HardClip { .. }
                | AlignmentComponent::Base { .. }
        ));
        let snapshot = page.allocation().snapshot().unwrap();
        assert_eq!(snapshot.class, AllocationClass::SemanticPage);
        assert_eq!(
            snapshot.charged_capacity,
            semantic_page_bytes(
                page.primitives().len(),
                page.row_offsets.len(),
                page.labels().len(),
                page.labels().iter().map(|label| label.text().len()).sum(),
            )
        );
    }

    #[test]
    fn semantic_page_build_scratch_is_separately_bounded_and_released() {
        let ledger = AllocationLedger::new_with_transient_budget(8 << 20, 0, 2 << 20);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Read,
            None,
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::SpecialistOverlay,
        );
        assert!(!page.primitives().is_empty());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_high_water > 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::EvidenceSelection && usage.high_water > 0
        }));
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::SemanticPageBuilder && usage.high_water > 0
        }));
    }

    #[test]
    fn empty_row_atlas_still_produces_an_explicit_empty_page() {
        assert_eq!(
            CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap())
                .range(0)
                .unwrap(),
            0..0
        );
    }

    #[test]
    fn presentation_scene_publishes_only_with_a_complete_mandatory_page_set() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(12, 13)),
            Arc::clone(&evidence),
            Arc::clone(&atlas),
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages.clone(),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();

        assert!(scene.covers(
            GenomicInterval::new(ContigId::new(0), 50, 250).unwrap(),
            0..1
        ));
        assert_eq!(scene.pages().len(), 9);
        assert_eq!(
            scene.metadata_allocation().snapshot().unwrap().class,
            AllocationClass::PresentationScene
        );
        assert!(ledger.snapshot().unwrap().pinned > 0);

        let error = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(14, 15)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages
                .iter()
                .filter(|page| {
                    !(page.key().family() == AlignmentPageFamily::ReadGeometry
                        && page.key().genomic_tile().coordinate() == 1)
                })
                .cloned()
                .collect(),
            &ledger,
            ReservationKind::Normal,
            21,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            crate::AlignmentPresentationSceneError::MissingMandatoryBlock {
                first_missing_base: 100,
                ..
            }
        ));
    }

    #[test]
    fn cpu_projection_and_picking_are_camera_only_views_of_one_scene_snapshot() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(20, 21)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            30,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 50, 250).unwrap(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 100.0,
            },
        )
        .unwrap();
        let before = ledger.snapshot().unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 7, camera).unwrap();
        let before_pick = ledger.snapshot().unwrap();
        let hit = crate::CpuAlignmentProjector::pick(
            &scene,
            rgv_core::TrackId::new(42),
            &frame,
            500.0,
            5.0,
        )
        .unwrap()
        .unwrap();
        let after_pick = ledger.snapshot().unwrap();
        let after = ledger.snapshot().unwrap();

        assert_eq!(frame.scene_revision, scene.revision());
        assert_eq!(frame.camera_sequence, 7);
        assert_eq!(frame.coverage.covered_interval, Some(camera.interval()));
        assert!(matches!(
            hit.component,
            AlignmentComponent::ReferenceSkip { .. }
        ));
        assert!(frame.primitives.iter().any(|primitive| {
            primitive.handle == hit.handle && primitive.component == hit.component
        }));
        let interaction = crate::AlignmentInteractionState::new(
            Some(hit.handle),
            Some(hit.handle),
            scene.supplementary_chain_handles(hit.handle),
        );
        let interactive =
            crate::CpuAlignmentProjector::project_with_interaction(&scene, 9, camera, &interaction)
                .unwrap();
        assert!(interactive.primitives.iter().any(|primitive| {
            primitive.handle == hit.handle
                && primitive.interaction.hovered
                && primitive.interaction.selected
                && primitive.interaction.selected_chain
        }));
        assert!(scene.contains_handle(hit.handle));
        assert!(
            scene
                .edge_handle(crate::AlignmentTraversalDirection::Next)
                .is_some()
        );
        assert!(
            scene
                .edge_handle(crate::AlignmentTraversalDirection::Previous)
                .is_some()
        );
        let hit_record = scene.inspect_retained(hit.handle).unwrap().unwrap();
        assert!(
            scene
                .handles_for_read_name(&hit_record.read_name)
                .contains(&hit.handle)
        );
        assert!(
            scene
                .adjacent_handle(hit.handle, crate::AlignmentTraversalDirection::Next)
                .is_some()
        );
        assert!(scene.mate_handles(hit.handle).is_empty());
        assert_eq!(before_pick, after_pick, "pointer picking must not allocate");
        assert_eq!(before.charged, after.charged);
        assert_eq!(before.records.len(), after.records.len());
        let exported = scene
            .export_full(GenomicInterval::new(ContigId::new(0), 0, 300).unwrap())
            .unwrap();
        assert_eq!(exported.len(), 3);
        let mut streamed = Vec::new();
        scene
            .for_each_full(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |record| {
                    streamed.push(record);
                    true
                },
            )
            .unwrap();
        assert_eq!(streamed, exported);
        let mut stopped_after_one = 0;
        scene
            .for_each_full(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |_| {
                    stopped_after_one += 1;
                    false
                },
            )
            .unwrap();
        assert_eq!(stopped_after_one, 1);
        let mut identities = Vec::new();
        scene
            .for_each_identity(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |handle, read_name, flags| {
                    identities.push((handle, read_name.to_vec(), flags));
                },
            )
            .unwrap();
        assert_eq!(identities.len(), exported.len());
        assert_eq!(
            scene.modification_codes(10, 10).unwrap(),
            vec![rgv_hts::ModificationCode::Letter(b'm')]
        );
        assert_eq!(
            scene.inspect_retained(hit.handle).unwrap().unwrap(),
            exported
                .iter()
                .find(|record| record.handle == hit.handle)
                .unwrap()
                .record
        );
        assert_eq!(
            exported
                .iter()
                .map(|record| record.handle)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );

        let mut stale_frame = frame.clone();
        stale_frame.scene_revision = crate::SceneRevision::new(RevisionFingerprint::new(99, 100));
        assert!(matches!(
            crate::CpuAlignmentProjector::pick(
                &scene,
                rgv_core::TrackId::new(42),
                &stale_frame,
                500.0,
                5.0,
            ),
            Err(crate::CpuInteractionError::StaleSceneFrame { .. })
        ));

        let edge_camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 250, 350).unwrap(),
            0.0,
            10.0,
            8.0,
            camera.clip(),
        )
        .unwrap();
        let edge = crate::CpuAlignmentProjector::project(&scene, 8, edge_camera).unwrap();
        assert_eq!(
            edge.coverage.covered_interval,
            Some(GenomicInterval::new(ContigId::new(0), 250, 300).unwrap())
        );
        assert!(!edge.primitives.is_empty());
    }

    #[test]
    fn pair_chain_picking_inspection_and_export_share_one_stable_handle_authority() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let (evidence, atlas) = paired_evidence_and_atlas(&ledger);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 300).unwrap();
        let selection = evidence.select(interval).unwrap();
        let mut pair_first = None;
        let mut pair_last = None;
        let mut primary = None;
        let mut supplementary = None;
        let mut secondary = None;
        for (index, reference) in selection.records().iter().enumerate() {
            let record = selection.record_view(index).unwrap();
            match (
                record.read_name(),
                record.flags().is_first_in_template(),
                record.flags().is_last_in_template(),
                record.flags().is_supplementary(),
                record.flags().is_secondary(),
            ) {
                (b"pair", true, false, false, false) => pair_first = Some(reference.handle),
                (b"pair", false, true, false, false) => pair_last = Some(reference.handle),
                (b"split", false, false, false, false) => primary = Some(reference.handle),
                (b"split", false, false, true, false) => supplementary = Some(reference.handle),
                (b"split", false, false, false, true) => secondary = Some(reference.handle),
                _ => {}
            }
        }
        let pair_first = pair_first.unwrap();
        let pair_last = pair_last.unwrap();
        let primary = primary.unwrap();
        let supplementary = supplementary.unwrap();
        let secondary = secondary.unwrap();
        drop(selection);

        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let connectors = pages
            .iter()
            .flat_map(|page| {
                page.primitives().iter().filter_map(move |primitive| {
                    let AlignmentComponent::PairConnector { mate, .. } = primitive.component else {
                        return None;
                    };
                    (primitive.handle == pair_first && mate == Some(pair_last)).then_some((
                        page.interval().start() + primitive.start_offset,
                        page.interval().start() + primitive.end_offset,
                    ))
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(connectors, vec![(60, 100), (100, 150)]);

        let row_count = u32::try_from(atlas.row_count()).unwrap();
        let pair_row = atlas.record(pair_first).unwrap().record.row();
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(40, 41)),
            Arc::clone(&evidence),
            atlas,
            presentation,
            interval,
            0..row_count,
            pages,
            &ledger,
            ReservationKind::Normal,
            50,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            interval,
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: f64::from(row_count.max(1)) * 10.0,
            },
        )
        .unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
        let hit = crate::CpuAlignmentProjector::pick(
            &scene,
            rgv_core::TrackId::new(9),
            &frame,
            105.0 / 300.0 * 900.0,
            f64::from(pair_row) * 10.0 + 5.0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.handle, pair_first);
        assert!(matches!(
            hit.component,
            AlignmentComponent::PairConnector {
                mate: Some(handle),
                ..
            } if handle == pair_last
        ));
        assert_eq!(scene.mate_handles(pair_first), vec![pair_last]);
        assert_eq!(scene.mate_handles(pair_last), vec![pair_first]);

        let chain = scene.supplementary_chain_handles(primary);
        assert_eq!(chain.len(), 2);
        assert!(chain.contains(&primary));
        assert!(chain.contains(&supplementary));
        assert!(!chain.contains(&secondary));

        let exported = scene.export_full(interval).unwrap();
        assert_eq!(exported.len(), 5);
        assert_eq!(
            exported
                .iter()
                .map(|record| record.handle)
                .collect::<Vec<_>>(),
            evidence
                .select(interval)
                .unwrap()
                .records()
                .iter()
                .map(|record| record.handle)
                .collect::<Vec<_>>()
        );
        for record in exported {
            assert_eq!(
                scene.inspect_retained(record.handle).unwrap().unwrap(),
                record.record
            );
        }
    }

    #[test]
    fn cpu_labels_reconcile_tile_fragments_and_screen_collisions_without_rekeying_pages() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::ReadName,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let mut page_keys = pages.iter().map(|page| page.key()).collect::<Vec<_>>();
        page_keys.sort_unstable();
        let spliced_fragments = pages
            .iter()
            .flat_map(|page| {
                page.primitives().iter().filter_map(move |primitive| {
                    let AlignmentComponent::Label {
                        kind: AlignmentLabelKind::ReadName,
                        id,
                    } = primitive.component
                    else {
                        return None;
                    };
                    (page.label(id) == Some("spliced")).then_some(id)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(spliced_fragments.len(), 3);
        assert!(
            spliced_fragments
                .iter()
                .all(|id| *id == spliced_fragments[0])
        );

        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(30, 31)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap();
        let clip = crate::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 900.0,
            height: 20.0,
        };
        let wide_camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0.0,
            10.0,
            8.0,
            clip,
        )
        .unwrap();
        let wide = crate::CpuAlignmentProjector::project(&scene, 1, wide_camera).unwrap();
        assert_eq!(
            wide.primitives
                .iter()
                .filter(|primitive| {
                    primitive.shape == crate::CpuPrimitiveShape::TextLabel
                        && primitive.text.as_deref() == Some("spliced")
                })
                .count(),
            1
        );

        let narrow_camera = crate::AlignmentCamera::new(
            wide_camera.interval(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                width: 50.0,
                ..clip
            },
        )
        .unwrap();
        let narrow = crate::CpuAlignmentProjector::project(&scene, 2, narrow_camera).unwrap();
        assert!(narrow.primitives.iter().all(|primitive| {
            primitive.shape != crate::CpuPrimitiveShape::TextLabel
                || primitive.text.as_deref() != Some("spliced")
        }));
        let mut projected_page_keys = scene
            .pages()
            .iter()
            .map(|page| page.key())
            .collect::<Vec<_>>();
        projected_page_keys.sort_unstable();
        assert_eq!(projected_page_keys, page_keys);
    }

    #[test]
    fn cpu_projection_reserves_before_work_and_remains_accounted_while_a_frame_is_live() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(50, 51)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            60,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 20.0,
            },
        )
        .unwrap();
        let charged = ledger.snapshot().unwrap().charged;
        ledger.set_transient_budget(0).unwrap();
        assert!(matches!(
            crate::CpuAlignmentProjector::project(&scene, 1, camera).unwrap_err(),
            crate::CpuProjectionError::Ledger(LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::CpuProjection,
                ..
            })
        ));
        let refused = ledger.snapshot().unwrap();
        assert_eq!(refused.charged, charged);
        assert_eq!(refused.transient, 0);

        ledger.set_transient_budget(4 << 20).unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 2, camera).unwrap();
        let live = ledger.snapshot().unwrap();
        assert!(live.transient > 0);
        assert!(live.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::CpuProjection && usage.current > 0
        }));
        let cloned = frame.clone();
        assert!(Arc::ptr_eq(&frame.primitives, &cloned.primitives));
        drop(frame);
        assert!(ledger.snapshot().unwrap().transient > 0);
        drop(cloned);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
    }
}
