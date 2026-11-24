//! Read-stack hover: the tooltip over a read event (or the read itself) and the highlight that
//! picks the hovered event out.
//!
//! **Timing.** The tooltip appears [`TOOLTIP_DELAY`] after the pointer settles on a target, or at
//! once when it moves on from a target whose tooltip was showing (within [`TOOLTIP_WARM`]), as
//! operating-system tooltips do. The highlight fades in over [`FADE_IN`] with an ease-out and
//! out over [`FADE_OUT`]; nothing loops. A repaint is requested only while a fade runs or a
//! tooltip delay is pending ([`ReadHoverAnimator::next_repaint`]), so the app is idle again
//! once the fade completes. With reduced motion the highlight appears and goes without fading.
//!
//! **Highlight.** It is an egui overlay laid out at paint time from the presented alignment
//! frame (the capture the GPU draws and the read text is laid out from, in `submit_ui_frame`),
//! not a shader uniform: an outline needs to be drawn outside the hovered shape, which a
//! fragment shader limited to that shape's quad cannot do, and the overlay re-projects the
//! hovered primitive through the very camera of the drawn frame, so it stays pixel-aligned while
//! the view scrolls, exactly as the text does. The hovered read's body tint is the existing
//! per-instance hover flag (`interaction_color` in `alignment_scene.wgsl`). Neither the tooltip
//! nor the highlight is part of any export.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use rgv_alignment_scene::{
    AlignmentComponent, AlignmentGeometryToken, AlignmentHoverTarget, BisulfiteObservationToken,
    ModificationState, UltimaIndelToken,
};
use rgv_core::PanelId;
use rgv_hts::{
    AlignedBaseClassification, AlignmentRecord, CigarOpKind, ReferenceSlice, SequenceData,
};

/// Delay before a tooltip appears over a newly hovered target.
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(250);
/// After a shown tooltip is left, the next one appears at once within this long.
pub(crate) const TOOLTIP_WARM: Duration = Duration::from_millis(500);
/// Highlight fade-in (ease-out) and fade-out times.
pub(crate) const FADE_IN: Duration = Duration::from_millis(140);
pub(crate) const FADE_OUT: Duration = Duration::from_millis(100);
/// Inserted or clipped bases shown in a tooltip before an ellipsis.
pub(crate) const SEQUENCE_PREVIEW_BASES: usize = 30;
/// Characters of a read name shown in a tooltip before an ellipsis.
pub(crate) const READ_NAME_CHARACTERS: usize = 40;

/// One hovered target, in the panel whose presented frame it was found in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadHover {
    pub(crate) panel: PanelId,
    pub(crate) target: AlignmentHoverTarget,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entered {
    hover: ReadHover,
    since: Instant,
    tooltip_from: Instant,
    /// The base under the pointer; it may change while the target stays the same.
    position: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Leaving {
    hover: ReadHover,
    alpha: f32,
    since: Instant,
}

/// The hover state machine: which target is hovered since when, which one is fading out, and
/// whether tooltips are "warm". Every query takes the time, so it is a pure function of the
/// pointer's history and the clock.
#[derive(Debug, Default)]
pub(crate) struct ReadHoverAnimator {
    current: Option<Entered>,
    leaving: Option<Leaving>,
    warm_until: Option<Instant>,
}

fn progress(since: Instant, now: Instant, duration: Duration) -> f32 {
    (now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// Cubic ease-out: fast at first, settling gently.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

impl ReadHoverAnimator {
    /// The pointer now hovers `next` (or nothing) over the base at `position`. Returns whether
    /// the target changed; moving within one target only updates the position.
    #[allow(clippy::large_types_passed_by_value)]
    pub(crate) fn set(
        &mut self,
        now: Instant,
        next: Option<ReadHover>,
        position: Option<u64>,
        reduced_motion: bool,
    ) -> bool {
        if self.current.map(|entered| entered.hover) == next {
            if let Some(entered) = self.current.as_mut() {
                entered.position = position;
            }
            return false;
        }
        if let Some(previous) = self.current.take() {
            if previous.tooltip_from <= now {
                self.warm_until = Some(now + TOOLTIP_WARM);
            }
            let alpha = Self::entered_alpha(&previous, now, reduced_motion);
            self.leaving = (!reduced_motion && alpha > 0.0).then_some(Leaving {
                hover: previous.hover,
                alpha,
                since: now,
            });
        }
        let warm = self.warm_until.is_some_and(|until| now <= until);
        self.current = next.map(|hover| Entered {
            hover,
            since: now,
            tooltip_from: if warm { now } else { now + TOOLTIP_DELAY },
            position,
        });
        true
    }

    /// A gesture (drag, scroll, pinch, key) began: everything disappears at once, without a
    /// fade, and the next tooltip waits for its delay again. Returns whether anything was shown
    /// or pending.
    pub(crate) fn suspend(&mut self) -> bool {
        let active = self.current.is_some() || self.leaving.is_some();
        *self = Self::default();
        active
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<ReadHover> {
        self.current.map(|entered| entered.hover)
    }

    /// The base under the pointer over the current target.
    pub(crate) fn position(&self) -> Option<u64> {
        self.current.and_then(|entered| entered.position)
    }

    fn entered_alpha(entered: &Entered, now: Instant, reduced_motion: bool) -> f32 {
        if reduced_motion {
            1.0
        } else {
            ease_out(progress(entered.since, now, FADE_IN))
        }
    }

    fn leaving_alpha(leaving: &Leaving, now: Instant, reduced_motion: bool) -> f32 {
        if reduced_motion {
            0.0
        } else {
            leaving.alpha * (1.0 - ease_out(progress(leaving.since, now, FADE_OUT)))
        }
    }

    /// Every highlight to draw at `now` with its opacity: the target fading out (if any), then
    /// the hovered one.
    pub(crate) fn highlights(&self, now: Instant, reduced_motion: bool) -> Vec<(ReadHover, f32)> {
        self.leaving
            .map(|leaving| {
                (
                    leaving.hover,
                    Self::leaving_alpha(&leaving, now, reduced_motion),
                )
            })
            .into_iter()
            .chain(self.current.map(|entered| {
                (
                    entered.hover,
                    Self::entered_alpha(&entered, now, reduced_motion),
                )
            }))
            .filter(|(_, alpha)| *alpha > 0.0)
            .collect()
    }

    /// The target whose tooltip shows at `now`.
    pub(crate) fn tooltip(&self, now: Instant) -> Option<ReadHover> {
        self.current
            .filter(|entered| entered.tooltip_from <= now)
            .map(|entered| entered.hover)
    }

    /// When the next frame is needed: at once while a fade runs, at the end of a pending
    /// tooltip delay, or never (`None`) once everything has settled.
    pub(crate) fn next_repaint(&self, now: Instant, reduced_motion: bool) -> Option<Duration> {
        let fading = !reduced_motion
            && (self
                .current
                .is_some_and(|entered| progress(entered.since, now, FADE_IN) < 1.0)
                || self.leaving.is_some_and(|leaving| {
                    progress(leaving.since, now, FADE_OUT) < 1.0 && leaving.alpha > 0.0
                }));
        if fading {
            return Some(Duration::ZERO);
        }
        self.current
            .filter(|entered| entered.tooltip_from > now)
            .map(|entered| entered.tooltip_from - now)
    }
}

/// One tooltip line; sequences are set in monospace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TooltipLine {
    pub(crate) text: String,
    pub(crate) monospace: bool,
}

impl TooltipLine {
    fn text(text: String) -> Self {
        Self {
            text,
            monospace: false,
        }
    }

    fn sequence(text: String) -> Self {
        Self {
            text,
            monospace: true,
        }
    }
}

/// Allele counts at one base, exactly as the coverage lane and pileup inspector have them (the
/// scene's allele summary: same read filters, base-quality threshold and mate policy).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AlleleFacts {
    /// A, C, G, T, N.
    pub(crate) bases: [u32; 5],
    pub(crate) deletion: u32,
    /// Reads with a base here (the denominator of the coverage lane's allele fractions).
    pub(crate) observations: u32,
}

impl AlleleFacts {
    pub(crate) fn count(&self, base: u8) -> u32 {
        match base.to_ascii_uppercase() {
            b'A' => self.bases[0],
            b'C' => self.bases[1],
            b'G' => self.bases[2],
            b'T' => self.bases[3],
            _ => self.bases[4],
        }
    }
}

/// Insertions on one base boundary, as the coverage inspector reports them: reads inserting
/// there, the reads that could (`rgv_render::insertion_covering_reads`), and the inserted
/// sequences, most common first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct InsertionFacts {
    pub(crate) with: u32,
    pub(crate) covering: u32,
    pub(crate) sequences: Vec<(Vec<u8>, u32)>,
}

/// One deletion's recurrence among the loaded reads spanning it, and the allele summary's
/// deletion count over its first base.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeletionFacts {
    pub(crate) same: u32,
    pub(crate) spanning: u32,
    pub(crate) any_over_first_base: Option<u32>,
}

/// The Modifications lane's tally of the site a call is summarised at (a reverse-strand `CpG`
/// call is pooled on the `CpG`'s C), and the code the hovered cell shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModificationFacts {
    pub(crate) tally: rgv_hts::ModificationSiteTally,
    pub(crate) code: rgv_hts::ModificationCode,
}

/// The Sashimi lane's junction for a splice gap, as its arc's tooltip states it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JunctionFacts {
    pub(crate) depth: u32,
    pub(crate) motif: String,
    pub(crate) strand: String,
    pub(crate) annotation: Option<String>,
}

/// A read's pair, as the read inspector classifies it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PairFacts {
    pub(crate) insert_size: u64,
    pub(crate) mate: String,
    pub(crate) orientation: &'static str,
    pub(crate) classification: &'static str,
}

/// How common the hovered event is at its locus; each figure is absent when its source is not
/// available (then its line is omitted rather than guessed).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LocusFacts {
    pub(crate) alleles: Option<AlleleFacts>,
    pub(crate) insertion: Option<InsertionFacts>,
    pub(crate) deletion: Option<DeletionFacts>,
    pub(crate) modification: Option<ModificationFacts>,
    /// Loaded reads soft-clipped on the same side at the same boundary.
    pub(crate) clipped_here: Option<u32>,
    pub(crate) junction: Option<JunctionFacts>,
    pub(crate) pair: Option<PairFacts>,
}

/// What a tooltip may draw on besides the hovered target itself.
#[derive(Clone, Copy)]
pub(crate) struct TooltipContext<'a> {
    /// The hovered read's record, when it could be retrieved.
    pub(crate) record: Option<&'a AlignmentRecord>,
    /// The panel's reference slice, when it belongs to the hovered scene.
    pub(crate) reference: Option<&'a ReferenceSlice>,
    pub(crate) contig: &'a str,
    /// The base under the pointer.
    pub(crate) position: Option<u64>,
    /// The text of a hovered label (a chip or an event length).
    pub(crate) label: Option<&'a str>,
}

fn grouped(value: u64) -> String {
    crate::workspace::alignment_management::sashimi_presentation::grouped_position(value)
}

fn base_letter(base: u8) -> char {
    char::from(base.to_ascii_uppercase())
}

fn percent(part: u32, whole: u32) -> String {
    if whole == 0 {
        "–".to_owned()
    } else {
        format!("{:.0}%", f64::from(part) * 100.0 / f64::from(whole))
    }
}

/// The first [`SEQUENCE_PREVIEW_BASES`] of `bases` in upper case, then `…` if there are more.
pub(crate) fn sequence_preview(bases: &[u8]) -> String {
    let mut preview = bases
        .iter()
        .take(SEQUENCE_PREVIEW_BASES)
        .map(|base| base_letter(*base))
        .collect::<String>();
    if bases.len() > SEQUENCE_PREVIEW_BASES {
        preview.push('…');
    }
    preview
}

/// Reference position (0-based), read position and length of CIGAR operation `index`.
pub(crate) fn operation_origin(
    record: &AlignmentRecord,
    index: usize,
) -> Option<(u64, usize, u32)> {
    let operation = record.cigar.get(index)?;
    let mut reference = record.start;
    let mut read = 0_usize;
    for preceding in &record.cigar[..index] {
        match preceding.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference = reference.saturating_add(u64::from(preceding.len));
                read = read.saturating_add(preceding.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference = reference.saturating_add(u64::from(preceding.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read = read.saturating_add(preceding.len as usize);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    Some((reference, read, operation.len))
}

/// The read position aligned to reference `position`, if the read has a base there.
pub(crate) fn aligned_read_position(record: &AlignmentRecord, position: u64) -> Option<usize> {
    let mut reference = record.start;
    let mut read = 0_usize;
    for operation in &record.cigar {
        let length = u64::from(operation.len);
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if (reference..reference + length).contains(&position) {
                    return Some(read + (position - reference) as usize);
                }
                reference += length;
                read += operation.len as usize;
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => reference += length,
            CigarOpKind::Insertion | CigarOpKind::SoftClip => read += operation.len as usize,
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
        if reference > position {
            break;
        }
    }
    None
}

/// `length` read bases from `start`, when the record carries its sequence.
fn read_bases(record: Option<&AlignmentRecord>, start: usize, length: usize) -> Option<&[u8]> {
    match &record?.sequence {
        SequenceData::Present { bases, .. } => bases.get(start..start.checked_add(length)?),
        SequenceData::NotLoaded | SequenceData::Missing => None,
    }
}

/// ` (CpG)` when the reference shows a `CpG` at a cytosine (or its partner guanine) at
/// `position`.
fn cpg_context(reference: Option<&ReferenceSlice>, position: u64, canonical: u8) -> &'static str {
    let Some(reference) = reference else {
        return "";
    };
    let upper = |position: Option<u64>| {
        position
            .and_then(|position| reference.base_at(position))
            .map(|base| base.to_ascii_uppercase())
    };
    let here = upper(Some(position));
    let cpg = match canonical.to_ascii_uppercase() {
        b'C' | b'G' => {
            (here == Some(b'C') && upper(position.checked_add(1)) == Some(b'G'))
                || (here == Some(b'G') && upper(position.checked_sub(1)) == Some(b'C'))
        }
        _ => false,
    };
    if cpg { " (CpG)" } else { "" }
}

/// The reader-facing name of every kind of read-stack element. Exhaustive, so a new kind cannot
/// be added without a name; [`tooltip_lines`] falls back to it (with the position) for any kind
/// it has no richer tooltip for.
pub(crate) const fn hover_kind_name(component: AlignmentComponent) -> &'static str {
    use rgv_alignment_scene::AlignmentLabelKind as Label;
    match component {
        AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::DirectionTerminal => "Read",
        AlignmentComponent::Insertion { .. } => "Insertion",
        AlignmentComponent::Deletion { .. } => "Deletion",
        AlignmentComponent::ReferenceSkip { .. } => "Splice gap",
        AlignmentComponent::SoftClip { .. } => "Soft clip",
        AlignmentComponent::HardClip { .. } => "Hard clip",
        AlignmentComponent::Padding { .. } => "Padding",
        AlignmentComponent::PairConnector { .. } => "Pair",
        AlignmentComponent::Label { kind, .. } => match kind {
            Label::ReadName => "Read name label",
            Label::AuxiliaryTag(_) => "Tag label",
            Label::InsertionLength | Label::ShortInsertion => "Insertion length chip",
            Label::DeletionLength => "Deletion length label",
            Label::SkipLength => "Splice gap length label",
            Label::ClipLength => "Clip length label",
        },
        AlignmentComponent::ModifiedBase { .. } => "Modification call",
        AlignmentComponent::BisulfiteBase { .. } => "Bisulfite call",
        AlignmentComponent::SmrtKinetics { .. } => "SMRT kinetics",
        AlignmentComponent::UltimaIndel { .. } => "Ultima indel",
        AlignmentComponent::SbxInsertion { .. } => "SBX insertion",
        AlignmentComponent::SbxTailBase { .. } => "SBX simplex tail base",
        AlignmentComponent::SpecialistUnavailable { .. } => "Specialist data unavailable",
        AlignmentComponent::Base { .. } => "Base",
        AlignmentComponent::ClipGhostBase { .. } => "Soft-clipped base",
    }
}

/// The tooltip of a hovered target: a first line naming the event, its size and locus; for
/// inserted or clipped bases their sequence; and how common the event is at its locus, from
/// `facts`. For the read itself: its name, strand, mapping quality and length, and the base
/// under the pointer. Kinds without a richer tooltip get [`hover_kind_name`] and the position.
#[allow(clippy::too_many_lines)]
pub(crate) fn tooltip_lines(
    target: &AlignmentHoverTarget,
    context: TooltipContext<'_>,
    facts: &LocusFacts,
) -> Vec<TooltipLine> {
    let TooltipContext {
        record,
        reference,
        contig,
        position,
        label,
    } = context;
    let span = target.genomic_span();
    let locus = |position: u64| format!("{contig}:{}", grouped(position.saturating_add(1)));
    let origin =
        |cigar_index: u32| record.and_then(|record| operation_origin(record, cigar_index as usize));
    let component = target.hit.component;
    let fallback = || {
        vec![TooltipLine::text(format!(
            "{} · {}",
            hover_kind_name(component),
            locus(span.start)
        ))]
    };
    match component {
        AlignmentComponent::Insertion {
            cigar_index,
            read_start,
            length,
        } => {
            // The insertion lies between the reference bases either side of its boundary.
            let boundary = origin(cigar_index).map_or(
                if target.primitive.geometry == AlignmentGeometryToken::PointMarkerAtEnd {
                    span.end
                } else {
                    span.start
                },
                |(reference, _, _)| reference,
            );
            let bases = read_bases(record, read_start as usize, length as usize);
            let short = bases
                .filter(|bases| bases.len() <= 10)
                .map(|bases| format!(" (+{})", sequence_preview(bases)))
                .unwrap_or_default();
            let mut lines = vec![TooltipLine::text(format!(
                "Insertion · {} bp{short} · between {contig}:{} and {}",
                grouped(u64::from(length)),
                grouped(boundary),
                grouped(boundary.saturating_add(1)),
            ))];
            if let Some(bases) = bases.filter(|bases| bases.len() > 10) {
                lines.push(TooltipLine::sequence(sequence_preview(bases)));
            }
            if let Some(insertion) = &facts.insertion {
                let mut text = format!(
                    "Insertion here in {} of {} reads ({})",
                    insertion.with,
                    insertion.covering,
                    percent(insertion.with, insertion.covering)
                );
                let common = insertion
                    .sequences
                    .iter()
                    .take(3)
                    .map(|(sequence, count)| {
                        let mut shown = sequence_preview(&sequence[..sequence.len().min(8)]);
                        if sequence.len() > 8 {
                            shown.push('…');
                        }
                        format!("+{shown} ({count})")
                    })
                    .collect::<Vec<_>>();
                if !common.is_empty() {
                    let _ = write!(text, " · most common: {}", common.join(", "));
                }
                lines.push(TooltipLine::text(text));
            }
            lines
        }
        AlignmentComponent::Deletion { cigar_index } => {
            let (start, length) = origin(cigar_index).map_or(
                (span.start, span.end.saturating_sub(span.start)),
                |(reference, _, length)| (reference, u64::from(length)),
            );
            let mut lines = vec![TooltipLine::text(format!(
                "Deletion · {} bp · {contig}:{}–{}",
                grouped(length),
                grouped(start.saturating_add(1)),
                grouped(start.saturating_add(length)),
            ))];
            if let Some(deletion) = facts.deletion {
                let mut text = format!(
                    "Same deletion in {} of {} spanning reads ({})",
                    deletion.same,
                    deletion.spanning,
                    percent(deletion.same, deletion.spanning)
                );
                if let Some(any) = deletion.any_over_first_base {
                    let _ = write!(text, " · any deletion here: {any}");
                }
                lines.push(TooltipLine::text(text));
            }
            lines
        }
        AlignmentComponent::ReferenceSkip { cigar_index } => {
            let (start, length) = origin(cigar_index).map_or(
                (span.start, span.end.saturating_sub(span.start)),
                |(reference, _, length)| (reference, u64::from(length)),
            );
            let mut lines = vec![TooltipLine::text(format!(
                "Splice gap · intron {} bp · {contig}:{}–{}",
                grouped(length),
                grouped(start.saturating_add(1)),
                grouped(start.saturating_add(length)),
            ))];
            if let Some(junction) = &facts.junction {
                let mut text = format!(
                    "Junction: {} {} · {} · {} strand",
                    grouped(u64::from(junction.depth)),
                    if junction.depth == 1 { "read" } else { "reads" },
                    junction.motif,
                    junction.strand
                );
                if let Some(annotation) = &junction.annotation {
                    let _ = write!(text, " · {annotation}");
                }
                lines.push(TooltipLine::text(text));
            }
            lines
        }
        AlignmentComponent::Base {
            base,
            quality,
            classification,
            ..
        } => {
            let reference_base = reference.and_then(|reference| reference.base_at(span.start));
            let shown_reference = reference_base.map_or('?', base_letter);
            let mut text = match classification {
                AlignedBaseClassification::Substitution
                | AlignedBaseClassification::DeclaredSubstitution => format!(
                    "Substitution {shown_reference}>{} · {}",
                    base_letter(base),
                    locus(span.start)
                ),
                AlignedBaseClassification::EncodingConflict => format!(
                    "CIGAR/sequence conflict · ref {shown_reference}, read {} · {}",
                    base_letter(base),
                    locus(span.start)
                ),
                AlignedBaseClassification::Match | AlignedBaseClassification::Unknown => {
                    format!("Base {} · {}", base_letter(base), locus(span.start))
                }
            };
            if let Some(quality) = quality {
                let _ = write!(text, " · base quality {quality}");
            }
            let mut lines = vec![TooltipLine::text(text)];
            if let Some(alleles) = facts.alleles {
                let observed = alleles.count(base);
                let mut text = if matches!(
                    classification,
                    AlignedBaseClassification::Match | AlignedBaseClassification::Unknown
                ) {
                    format!(
                        "{} {observed} of {} reads here ({})",
                        base_letter(base),
                        alleles.observations,
                        percent(observed, alleles.observations)
                    )
                } else {
                    format!(
                        "This change: {observed} of {} reads here ({})",
                        alleles.observations,
                        percent(observed, alleles.observations)
                    )
                };
                if let Some(reference_base) = reference_base
                    && !reference_base.eq_ignore_ascii_case(&base)
                {
                    let _ = write!(
                        text,
                        " · ref {} {}",
                        base_letter(reference_base),
                        alleles.count(reference_base)
                    );
                }
                for other in [b'A', b'C', b'G', b'T', b'N'] {
                    if other.eq_ignore_ascii_case(&base)
                        || reference_base
                            .is_some_and(|reference| other.eq_ignore_ascii_case(&reference))
                        || alleles.count(other) == 0
                    {
                        continue;
                    }
                    let _ = write!(text, " · {} {}", char::from(other), alleles.count(other));
                }
                if alleles.deletion > 0 {
                    let _ = write!(text, " · deleted {}", alleles.deletion);
                }
                lines.push(TooltipLine::text(text));
            }
            lines
        }
        AlignmentComponent::ModifiedBase {
            canonical_base,
            code,
            probability,
            state,
            ..
        } => {
            let name = rgv_render::modification_style::modification_code_name(code);
            let context = cpg_context(reference, span.start, canonical_base);
            let probability = probability.map(|value| f64::from(value) / 255.0);
            let mut lines = vec![TooltipLine::text(match (state, probability) {
                (ModificationState::Modified, Some(probability)) => format!(
                    "{name} · probability {probability:.2} · {}{context}",
                    locus(span.start)
                ),
                (ModificationState::Modified, None) => {
                    format!("{name} · {}{context}", locus(span.start))
                }
                (ModificationState::Canonical, Some(probability)) => format!(
                    "Unmodified {} · {name} probability {probability:.2} · {}{context}",
                    base_letter(canonical_base),
                    locus(span.start)
                ),
                (ModificationState::Canonical, None) => format!(
                    "Unmodified {} (implicit) · {}{context}",
                    base_letter(canonical_base),
                    locus(span.start)
                ),
            })];
            if let Some(modification) = &facts.modification {
                lines.extend(modification_count_line(modification).map(TooltipLine::text));
            }
            lines
        }
        AlignmentComponent::SoftClip {
            read_start, length, ..
        } => soft_clip_lines(record, target, read_start as usize, length, facts),
        AlignmentComponent::ClipGhostBase {
            cigar_index,
            read_index,
            base,
            ..
        } => {
            let mut lines = Vec::new();
            if let Some((_, read_start, length)) = origin(cigar_index) {
                let index = (read_index as usize).saturating_sub(read_start) + 1;
                lines.push(TooltipLine::text(format!(
                    "Soft-clipped base {} · base {index} of the {length}-base clip · {}",
                    base_letter(base),
                    locus(span.start)
                )));
                lines.extend(soft_clip_lines(record, target, read_start, length, facts));
            } else {
                lines.push(TooltipLine::text(format!(
                    "Soft-clipped base {} · {}",
                    base_letter(base),
                    locus(span.start)
                )));
            }
            lines
        }
        AlignmentComponent::HardClip { length, .. } => vec![TooltipLine::text(format!(
            "Hard clip · {} bp at the {} end (bases not stored in this record)",
            grouped(u64::from(length)),
            clip_side(target)
        ))],
        AlignmentComponent::Padding { length, .. } => vec![TooltipLine::text(format!(
            "Padding · {} bp · {}",
            grouped(u64::from(length)),
            locus(span.start)
        ))],
        AlignmentComponent::BisulfiteBase { observation, .. } => {
            vec![TooltipLine::text(format!(
                "Bisulfite · {} · {}",
                match observation {
                    BisulfiteObservationToken::Protected => "C kept (protected: methylated)",
                    BisulfiteObservationToken::Converted => "C read as T (converted: unmethylated)",
                },
                locus(span.start)
            ))]
        }
        AlignmentComponent::SmrtKinetics { tag, value, .. } => vec![TooltipLine::text(format!(
            "SMRT kinetics · {}{} {value} · {}",
            char::from(tag[0]),
            char::from(tag[1]),
            locus(span.start)
        ))],
        AlignmentComponent::UltimaIndel {
            kind, quality_bits, ..
        } => {
            let mut text = format!(
                "Ultima homopolymer {} · {}",
                match kind {
                    UltimaIndelToken::Insertion => "insertion",
                    UltimaIndelToken::Deletion => "deletion",
                },
                locus(span.start)
            );
            if let Some(bits) = quality_bits {
                let _ = write!(text, " · quality {:.1}", f32::from_bits(bits));
            }
            vec![TooltipLine::text(text)]
        }
        AlignmentComponent::SbxInsertion { state, .. } => vec![TooltipLine::text(format!(
            "SBX insertion · {} · {}",
            match state {
                rgv_alignment_scene::SbxInsertionToken::Concordant => "concordant",
                rgv_alignment_scene::SbxInsertionToken::PartlyDiscordant => "partly discordant",
                rgv_alignment_scene::SbxInsertionToken::FullyDiscordant => "fully discordant",
                rgv_alignment_scene::SbxInsertionToken::Unavailable => "evidence unavailable",
            },
            locus(span.start)
        ))],
        AlignmentComponent::Label { kind, .. } => label_lines(kind, label),
        AlignmentComponent::PairConnector { .. } => {
            let mut lines = vec![TooltipLine::text(match &facts.pair {
                Some(pair) => format!(
                    "Pair · insert size {} · mate at {} · {} · {}",
                    grouped(pair.insert_size),
                    pair.mate,
                    pair.orientation,
                    pair.classification
                ),
                None => "Pair connector · joins this read to its mate".to_owned(),
            })];
            lines.extend(read_lines(target, record, None, None));
            lines
        }
        AlignmentComponent::SpecialistUnavailable { mode, reason } => {
            let mut lines = vec![TooltipLine::text(format!(
                "{mode:?} data unavailable on this read · {}",
                match reason {
                    rgv_alignment_scene::SpecialistUnavailableReason::MissingRecordData => {
                        "the record lacks the tags it needs"
                    }
                    rgv_alignment_scene::SpecialistUnavailableReason::MalformedRecordData => {
                        "its tags are malformed"
                    }
                    rgv_alignment_scene::SpecialistUnavailableReason::MissingReference => {
                        "no reference sequence"
                    }
                }
            ))];
            lines.extend(read_lines(target, record, None, None));
            lines
        }
        AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::DirectionTerminal => read_lines(target, record, position, reference),
        // No richer tooltip: the kind's name and position.
        AlignmentComponent::SbxTailBase { .. } => fallback(),
    }
}

/// The Modifications lane's figures for a hovered call's site: the code's share of the called
/// reads, the other codes, and the unmodified reads.
pub(crate) fn modification_count_line(facts: &ModificationFacts) -> Option<String> {
    let called = facts.tally.called();
    if called == 0 {
        return None;
    }
    let shown = facts.tally.code(facts.code);
    let name = rgv_render::modification_style::modification_code_name(facts.code);
    let lead = if name == "5mC" {
        "Methylated".to_owned()
    } else {
        name
    };
    let modified = shown.map_or(0, |entry| entry.modified);
    let mut text = format!(
        "{lead} in {modified} of {called} called reads ({})",
        percent(modified, called)
    );
    for entry in &facts.tally.codes {
        let Some(code) = entry.code else {
            continue;
        };
        if code == facts.code || entry.called == 0 {
            continue;
        }
        let _ = write!(
            text,
            " · {} {}",
            rgv_render::modification_style::modification_code_name(code),
            entry.modified
        );
    }
    let _ = write!(text, " · unmodified {}", facts.tally.unmodified());
    Some(text)
}

/// `left` or `right`: the read end a clip mark is drawn on.
fn clip_side(target: &AlignmentHoverTarget) -> &'static str {
    match target.primitive.geometry {
        AlignmentGeometryToken::ClipEdge(edge) if edge.at_end => "right",
        // A left cut edge; a ghost base's side is taken from its record when there is one.
        _ => "left",
    }
}

fn soft_clip_lines(
    record: Option<&AlignmentRecord>,
    target: &AlignmentHoverTarget,
    read_start: usize,
    length: u32,
    facts: &LocusFacts,
) -> Vec<TooltipLine> {
    let side = record.map_or_else(
        || clip_side(target),
        |record| {
            if read_start == 0
                && record
                    .cigar
                    .iter()
                    .find(|op| op.kind != CigarOpKind::HardClip)
                    .is_some_and(|op| op.kind == CigarOpKind::SoftClip)
            {
                "left"
            } else {
                "right"
            }
        },
    );
    let mut lines = vec![TooltipLine::text(format!(
        "Soft clip · {} bp at the {side} end",
        grouped(u64::from(length))
    ))];
    if let Some(bases) = read_bases(record, read_start, length as usize) {
        lines.push(TooltipLine::sequence(sequence_preview(bases)));
    }
    if let Some(count) = facts.clipped_here {
        lines.push(TooltipLine::text(format!(
            "Reads soft-clipped at this {side} boundary: {count}"
        )));
    }
    lines
}

/// What a hovered label means: insertion chips and event lengths are read from their text.
fn label_lines(
    kind: rgv_alignment_scene::AlignmentLabelKind,
    text: Option<&str>,
) -> Vec<TooltipLine> {
    use rgv_alignment_scene::AlignmentLabelKind as Label;
    let text = text.unwrap_or_default();
    let digits = text
        .trim_start_matches('+')
        .trim_end_matches(|character: char| !character.is_ascii_digit());
    let length = digits.parse::<u64>().ok().map(grouped);
    let line = match (kind, length) {
        (Label::InsertionLength | Label::ShortInsertion, Some(length)) => format!(
            "Insertion length chip · {length} inserted bases · hover the I-beam for the sequence"
        ),
        (Label::DeletionLength, Some(length)) => format!("Deletion length · {length} bp"),
        (Label::SkipLength, Some(length)) => format!("Splice gap length · intron {length} bp"),
        (Label::ClipLength, Some(length)) if text.ends_with('H') => {
            format!("Clip length · hard clip of {length} bp (bases not stored)")
        }
        (Label::ClipLength, Some(length)) => {
            format!("Clip length · soft clip of {length} bp (faint bases beside the cut edge)")
        }
        (Label::ReadName, _) => format!("Read name · {text}"),
        (Label::AuxiliaryTag(tag), _) => {
            format!("Tag {}{} · {text}", char::from(tag[0]), char::from(tag[1]))
        }
        (kind, None) => format!(
            "{} · {text}",
            hover_kind_name(AlignmentComponent::Label {
                kind,
                id: rgv_alignment_scene::ContractFingerprint::new(0),
            })
        ),
    };
    vec![TooltipLine::text(line)]
}

/// The read itself: its name (shortened); strand, mapping quality and length; the base under
/// the pointer (when known) with the reference base and its quality; and how to see more.
fn read_lines(
    target: &AlignmentHoverTarget,
    record: Option<&AlignmentRecord>,
    position: Option<u64>,
    reference: Option<&ReferenceSlice>,
) -> Vec<TooltipLine> {
    let style = target.primitive.style;
    let (name, reverse, mapping_quality) = record.map_or_else(
        || {
            (
                "Read".to_owned(),
                style.flags & 0x10 != 0,
                style.mapping_quality,
            )
        },
        |record| {
            let name = String::from_utf8_lossy(&record.read_name);
            let mut shown = name.chars().take(READ_NAME_CHARACTERS).collect::<String>();
            if name.chars().count() > READ_NAME_CHARACTERS {
                shown.push('…');
            }
            (shown, record.flags.is_reverse(), record.mapping_quality)
        },
    );
    let mut details = format!(
        "{} strand · MAPQ {}",
        if reverse { "Reverse" } else { "Forward" },
        mapping_quality
            .filter(|quality| *quality != 255)
            .map_or_else(|| "unavailable".to_owned(), |quality| quality.to_string())
    );
    if let Some(record) = record {
        match &record.sequence {
            SequenceData::Present { bases, .. } => {
                let _ = write!(details, " · {} bp", grouped(bases.len() as u64));
            }
            SequenceData::NotLoaded | SequenceData::Missing => {
                let _ = write!(
                    details,
                    " · {} bp aligned",
                    grouped(record.end.saturating_sub(record.start))
                );
            }
        }
        if record.flags.is_supplementary() {
            details.push_str(" · supplementary");
        } else if record.flags.is_secondary() {
            details.push_str(" · secondary");
        }
    }
    let mut lines = vec![TooltipLine::sequence(name), TooltipLine::text(details)];
    if let (Some(position), Some(record)) = (position, record) {
        let contig_free = grouped(position.saturating_add(1));
        let mut text = format!("Base {contig_free}");
        if let Some(reference) = reference.and_then(|reference| reference.base_at(position)) {
            let _ = write!(text, " · ref {}", base_letter(reference));
        }
        if let Some(index) = aligned_read_position(record, position) {
            if let SequenceData::Present { bases, qualities } = &record.sequence {
                if let Some(base) = bases.get(index) {
                    let _ = write!(text, " · read {}", base_letter(*base));
                }
                if let Some(quality) = qualities.as_deref().and_then(|values| values.get(index)) {
                    let _ = write!(text, " · base quality {quality}");
                }
            }
        } else {
            text.push_str(" · not aligned here");
        }
        lines.push(TooltipLine::text(text));
    }
    lines.push(TooltipLine::text("Click for details".to_owned()));
    lines
}

/// A hovered read and its record (when it could be retrieved).
type CachedRecord = (
    rgv_alignment_scene::AlignmentHit,
    Option<std::sync::Arc<AlignmentRecord>>,
);

/// What a tooltip depends on: its target, and over the read itself the base under the pointer.
type TooltipKey = (AlignmentHoverTarget, Option<u64>);

/// The lines of the hovered target's tooltip, kept while the target (and, over the read
/// itself, the base under the pointer) stays the same, and the hovered read's record, kept
/// while the read stays the same: a long read is materialised once, not on every move between
/// its events, and locus counts are gathered once per target, never per frame.
#[derive(Debug, Default)]
pub(crate) struct ReadHoverTooltipCache {
    record: Option<CachedRecord>,
    lines: Option<(TooltipKey, Vec<TooltipLine>)>,
    /// How many times lines were built; diagnostics and tests.
    pub(crate) builds: u64,
}

impl ReadHoverTooltipCache {
    /// The hovered read's record, fetched through `fetch` only when the read changed.
    pub(crate) fn record(
        &mut self,
        target: &AlignmentHoverTarget,
        fetch: impl FnOnce() -> Option<AlignmentRecord>,
    ) -> Option<std::sync::Arc<AlignmentRecord>> {
        let same_read = self.record.as_ref().is_some_and(|(hit, _)| {
            hit.track == target.hit.track
                && hit.scene_revision == target.hit.scene_revision
                && hit.handle == target.hit.handle
        });
        if !same_read {
            self.record = Some((target.hit, fetch().map(std::sync::Arc::new)));
        }
        self.record.as_ref().and_then(|(_, record)| record.clone())
    }

    /// The tooltip of `target` with the pointer over `position`, built by `build` on first use.
    pub(crate) fn lines(
        &mut self,
        target: &AlignmentHoverTarget,
        position: Option<u64>,
        build: impl FnOnce(&mut Self) -> Vec<TooltipLine>,
    ) -> Vec<TooltipLine> {
        // Over an event the pointer's base does not change the tooltip.
        let position = (!target.is_event()).then_some(position).flatten();
        let key = (*target, position);
        if self.lines.as_ref().is_none_or(|(cached, _)| *cached != key) {
            let lines = build(self);
            self.builds += 1;
            self.lines = Some((key, lines));
        }
        self.lines
            .as_ref()
            .map_or_else(Vec::new, |(_, lines)| lines.clone())
    }
}

/// The highlight around one hovered shape: a soft glow and a crisp ring along `outline` (pane
/// points), both at `alpha` of their full strength, clipped to `clip`.
pub(crate) fn highlight_shapes(
    outline: &[[f64; 2]],
    alpha: f32,
    clip: egui::Rect,
    glow: egui::Color32,
    ring: egui::Color32,
) -> Vec<egui::epaint::ClippedShape> {
    let points = outline
        .iter()
        .map(|[x, y]| egui::pos2(*x as f32, *y as f32))
        .collect::<Vec<_>>();
    [(4.0, glow), (1.5, ring)]
        .into_iter()
        .map(|(width, color)| egui::epaint::ClippedShape {
            clip_rect: clip,
            shape: egui::Shape::closed_line(
                points.clone(),
                egui::Stroke::new(width, color.gamma_multiply(alpha.clamp(0.0, 1.0))),
            ),
        })
        .collect()
}

/// Where a tooltip of `size` goes: below and right of the pointer, as egui places tooltips,
/// flipped to the other side of the pointer along any axis it would leave `screen` by, and
/// finally kept inside the screen.
pub(crate) fn tooltip_rect(
    pointer: egui::Pos2,
    size: egui::Vec2,
    screen: egui::Rect,
) -> egui::Rect {
    let mut origin = pointer + egui::vec2(14.0, 16.0);
    if origin.x + size.x > screen.right() {
        origin.x = pointer.x - size.x - 10.0;
    }
    if origin.y + size.y > screen.bottom() {
        origin.y = pointer.y - size.y - 10.0;
    }
    origin.x = origin
        .x
        .clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    origin.y = origin
        .y
        .clamp(screen.top(), (screen.bottom() - size.y).max(screen.top()));
    egui::Rect::from_min_size(origin, size)
}

#[cfg(test)]
mod tests;
