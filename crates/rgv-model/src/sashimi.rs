//! Sashimi presentation rules shared by the interactive renderer, picking, labels and exports.
//!
//! Junction evidence is an exact count of spliced reads. How each junction is drawn (its strand,
//! whether it is weak enough to hide, its share of each splice site, the arc's height and stroke)
//! is decided here once, as pure functions, so every consumer draws the same arcs.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use rgv_core::LibraryStrandedness;

use crate::{JunctionEvidence, JunctionStrand};

/// The dinucleotides bounding an intron, classified from the reference's `+` strand bases.
///
/// For an intron occupying `[start, end)`, the donor-side bases are `start..start + 2` and the
/// acceptor-side bases are `end - 2..end`, both read on the reference's `+` strand. The `Ct*`
/// and `GtAt` variants are the reverse complements of the canonical `+` strand motifs, so they
/// name a `-` strand intron.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpliceMotif {
    /// The reference bases are not (yet) known.
    #[default]
    Unavailable,
    /// GT..AG on `+` (major U2).
    GtAg,
    /// GC..AG on `+`.
    GcAg,
    /// AT..AC on `+` (U12).
    AtAc,
    /// CT..AC on `+`: GT-AG on `-`.
    CtAc,
    /// CT..GC on `+`: GC-AG on `-`.
    CtGc,
    /// GT..AT on `+`: AT-AC on `-`.
    GtAt,
    /// Known bases that match no canonical motif on either strand.
    NonCanonical,
}

impl SpliceMotif {
    /// Classifies an intron from its first two and last two reference bases (`+` strand order).
    #[must_use]
    pub fn classify(first: [u8; 2], last: [u8; 2]) -> Self {
        let first = first.map(|base| base.to_ascii_uppercase());
        let last = last.map(|base| base.to_ascii_uppercase());
        match (&first, &last) {
            (b"GT", b"AG") => Self::GtAg,
            (b"GC", b"AG") => Self::GcAg,
            (b"AT", b"AC") => Self::AtAc,
            (b"CT", b"AC") => Self::CtAc,
            (b"CT", b"GC") => Self::CtGc,
            (b"GT", b"AT") => Self::GtAt,
            _ => Self::NonCanonical,
        }
    }

    /// Strand implied by a canonical motif; `Unknown` otherwise.
    #[must_use]
    pub const fn strand(self) -> JunctionStrand {
        match self {
            Self::GtAg | Self::GcAg | Self::AtAc => JunctionStrand::Positive,
            Self::CtAc | Self::CtGc | Self::GtAt => JunctionStrand::Negative,
            Self::Unavailable | Self::NonCanonical => JunctionStrand::Unknown,
        }
    }

    /// The motif read along the transcript (donor first), e.g. `GT-AG` for both `GtAg` and
    /// `CtAc`.
    #[must_use]
    pub const fn transcript_label(self) -> &'static str {
        match self {
            Self::GtAg | Self::CtAc => "GT-AG",
            Self::GcAg | Self::CtGc => "GC-AG",
            Self::AtAc | Self::GtAt => "AT-AC",
            Self::NonCanonical => "non-canonical",
            Self::Unavailable => "unavailable",
        }
    }

    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    const fn code(self) -> u8 {
        match self {
            Self::Unavailable => 0,
            Self::GtAg => 1,
            Self::GcAg => 2,
            Self::AtAc => 3,
            Self::CtAc => 4,
            Self::CtGc => 5,
            Self::GtAt => 6,
            Self::NonCanonical => 7,
        }
    }
}

/// Where a junction's drawn strand came from.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum JunctionStrandSource {
    /// `XS`, `TS` or `ts` tags on the supporting reads.
    Tag,
    /// The canonical splice motif in the reference.
    Motif,
    /// The track's declared library strandedness and the reads' first-of-pair orientation.
    Library,
    #[default]
    Unknown,
}

impl JunctionStrandSource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Motif => "motif",
            Self::Library => "library",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedJunctionStrand {
    pub strand: JunctionStrand,
    pub source: JunctionStrandSource,
    /// Reads tag both strands; the majority was drawn.
    pub conflicting_tags: bool,
}

/// Resolves the single strand a junction is drawn on: strand tags, then the canonical splice
/// motif, then the declared library strandedness, otherwise unknown. Alignment orientation alone
/// is never used: the two mates of one fragment align in opposite directions.
#[must_use]
pub fn resolve_junction_strand(
    evidence: &JunctionEvidence,
    motif: SpliceMotif,
    library: LibraryStrandedness,
) -> ResolvedJunctionStrand {
    let (positive, negative) = (
        evidence.tagged_positive_depth,
        evidence.tagged_negative_depth,
    );
    let conflicting_tags = positive > 0 && negative > 0;
    if positive != negative {
        return ResolvedJunctionStrand {
            strand: if positive > negative {
                JunctionStrand::Positive
            } else {
                JunctionStrand::Negative
            },
            source: JunctionStrandSource::Tag,
            conflicting_tags,
        };
    }
    let motif_strand = motif.strand();
    if motif_strand != JunctionStrand::Unknown {
        return ResolvedJunctionStrand {
            strand: motif_strand,
            source: JunctionStrandSource::Motif,
            conflicting_tags,
        };
    }
    let (forward, reverse) = (
        evidence.first_of_pair_forward_depth,
        evidence.first_of_pair_reverse_depth,
    );
    let library_strand = match library {
        LibraryStrandedness::Unstranded => None,
        _ if forward == reverse => None,
        LibraryStrandedness::FirstOfPairSense => Some(forward > reverse),
        LibraryStrandedness::FirstOfPairAntisense => Some(reverse > forward),
    };
    if let Some(positive) = library_strand {
        return ResolvedJunctionStrand {
            strand: if positive {
                JunctionStrand::Positive
            } else {
                JunctionStrand::Negative
            },
            source: JunctionStrandSource::Library,
            conflicting_tags,
        };
    }
    ResolvedJunctionStrand {
        strand: JunctionStrand::Unknown,
        source: JunctionStrandSource::Unknown,
        conflicting_tags,
    }
}

/// Which end of the intron a splice site is.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpliceSiteSide {
    /// The intron's first base (`junction.start`, zero-based).
    Start,
    /// The intron's exclusive end (`junction.end`, zero-based).
    End,
}

/// One splice site: an intron boundary shared by every junction with that start (or end).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SpliceSite {
    pub position: u64,
    pub side: SpliceSiteSide,
}

impl SpliceSite {
    #[must_use]
    pub const fn start_of(start: u64) -> Self {
        Self {
            position: start,
            side: SpliceSiteSide::Start,
        }
    }

    #[must_use]
    pub const fn end_of(end: u64) -> Self {
        Self {
            position: end,
            side: SpliceSiteSide::End,
        }
    }

    #[must_use]
    pub const fn matches(self, start: u64, end: u64) -> bool {
        match self.side {
            SpliceSiteSide::Start => start == self.position,
            SpliceSiteSide::End => end == self.position,
        }
    }

    /// One-based coordinate of the intron base at this boundary: the first intron base for a
    /// start site, the last for an end site.
    #[must_use]
    pub const fn one_based_position(self) -> u64 {
        match self.side {
            SpliceSiteSide::Start => self.position.saturating_add(1),
            SpliceSiteSide::End => self.position,
        }
    }
}

/// Track and panel state that decides which junctions a Sashimi lane draws and how.
#[derive(Clone, Debug, PartialEq)]
pub struct SashimiPresentationOptions {
    /// Absolute weak threshold: fewer reads than this hides a junction.
    pub minimum_depth: u32,
    /// Relative weak threshold in percent of the strongest junction sharing either site.
    pub relative_threshold_percent: f32,
    /// Ignore both weak thresholds.
    pub show_all: bool,
    pub library: LibraryStrandedness,
    /// Show only junctions using this splice site.
    pub site_filter: Option<SpliceSite>,
    /// Show only junctions with exactly these `(start, end)` intron extents.
    pub intron_filter: Option<Arc<BTreeSet<(u64, u64)>>>,
}

impl Default for SashimiPresentationOptions {
    fn default() -> Self {
        Self {
            minimum_depth: 2,
            relative_threshold_percent: 1.0,
            show_all: false,
            library: LibraryStrandedness::Unstranded,
            site_filter: None,
            intron_filter: None,
        }
    }
}

/// How one junction is drawn. Totals count every junction of the lane's prepared evidence,
/// including hidden ones, so splice-site shares (PSI) never depend on what is drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JunctionPresentation {
    pub strand: JunctionStrand,
    pub source: JunctionStrandSource,
    pub conflicting_tags: bool,
    pub motif: SpliceMotif,
    /// Below a weak-junction threshold and not shown.
    pub weak: bool,
    /// Excluded by a splice-site or transcript filter.
    pub filtered: bool,
    /// Reads of every junction starting where this one starts.
    pub start_site_total: u32,
    /// Reads of every junction ending where this one ends.
    pub end_site_total: u32,
}

impl JunctionPresentation {
    /// A junction drawn from its own evidence alone: tag strand, never hidden.
    #[must_use]
    pub fn from_evidence(evidence: &JunctionEvidence) -> Self {
        let resolved = resolve_junction_strand(
            evidence,
            SpliceMotif::Unavailable,
            LibraryStrandedness::Unstranded,
        );
        Self {
            strand: resolved.strand,
            source: resolved.source,
            conflicting_tags: resolved.conflicting_tags,
            motif: SpliceMotif::Unavailable,
            weak: false,
            filtered: false,
            start_site_total: evidence.depth,
            end_site_total: evidence.depth,
        }
    }

    #[must_use]
    pub const fn hidden(&self) -> bool {
        self.weak || self.filtered
    }

    /// Share of the reads leaving the start site that use this junction.
    #[must_use]
    pub fn start_psi(&self, depth: u32) -> Option<f64> {
        share(depth, self.start_site_total)
    }

    #[must_use]
    pub fn end_psi(&self, depth: u32) -> Option<f64> {
        share(depth, self.end_site_total)
    }

    /// `(donor, acceptor)` PSI for a stranded junction; `(start, end)` for an unknown one.
    #[must_use]
    pub fn donor_acceptor_psi(&self, depth: u32) -> (Option<f64>, Option<f64>) {
        match self.strand {
            JunctionStrand::Negative => (self.end_psi(depth), self.start_psi(depth)),
            JunctionStrand::Positive | JunctionStrand::Unknown => {
                (self.start_psi(depth), self.end_psi(depth))
            }
        }
    }

    fn hash_into(&self, hash: &mut Fnv) {
        hash.write(&[
            strand_code(self.strand),
            self.source as u8,
            u8::from(self.conflicting_tags),
            self.motif.code(),
            u8::from(self.weak),
            u8::from(self.filtered),
        ]);
        hash.write(&self.start_site_total.to_le_bytes());
        hash.write(&self.end_site_total.to_le_bytes());
    }
}

fn share(depth: u32, total: u32) -> Option<f64> {
    (total > 0).then(|| (f64::from(depth) / f64::from(total)).clamp(0.0, 1.0))
}

const fn strand_code(strand: JunctionStrand) -> u8 {
    match strand {
        JunctionStrand::Positive => 0,
        JunctionStrand::Negative => 1,
        JunctionStrand::Unknown => 2,
    }
}

/// The resolved presentation of every junction in one Sashimi lane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SashimiPresentationTable {
    entries: HashMap<(u64, u64), JunctionPresentation>,
    fingerprint: u64,
    weak_hidden: usize,
    filtered_out: usize,
    site_filter: Option<SpliceSite>,
    intron_filter_active: bool,
}

impl SashimiPresentationTable {
    /// Resolves every junction. `junctions` may repeat a junction (its first copy is used);
    /// `motif` returns the reference motif of an intron, or `Unavailable`.
    #[must_use]
    pub fn build<'a>(
        junctions: impl IntoIterator<Item = &'a JunctionEvidence>,
        motif: impl Fn(u64, u64) -> SpliceMotif,
        options: &SashimiPresentationOptions,
    ) -> Self {
        let mut unique = HashMap::<(u64, u64), JunctionEvidence>::new();
        for junction in junctions {
            unique
                .entry((junction.start, junction.end))
                .or_insert(*junction);
        }
        let mut start_sites = HashMap::<u64, (u32, u32)>::new();
        let mut end_sites = HashMap::<u64, (u32, u32)>::new();
        for junction in unique.values() {
            for (sites, position) in [
                (&mut start_sites, junction.start),
                (&mut end_sites, junction.end),
            ] {
                let (maximum, total) = sites.entry(position).or_default();
                *maximum = (*maximum).max(junction.depth);
                *total = total.saturating_add(junction.depth);
            }
        }
        let relative = if options.relative_threshold_percent.is_finite() {
            f64::from(options.relative_threshold_percent.clamp(0.0, 100.0)) / 100.0
        } else {
            0.0
        };
        let mut entries = HashMap::with_capacity(unique.len());
        let (mut weak_hidden, mut filtered_out) = (0_usize, 0_usize);
        for (key, junction) in &unique {
            let (start_maximum, start_total) = start_sites
                .get(&junction.start)
                .copied()
                .unwrap_or_default();
            let (end_maximum, end_total) =
                end_sites.get(&junction.end).copied().unwrap_or_default();
            let strongest = start_maximum.max(end_maximum);
            let weak = !options.show_all
                && (junction.depth < options.minimum_depth
                    || f64::from(junction.depth) < relative * f64::from(strongest));
            let filtered = options
                .site_filter
                .is_some_and(|site| !site.matches(junction.start, junction.end))
                || options
                    .intron_filter
                    .as_ref()
                    .is_some_and(|introns| !introns.contains(key));
            if filtered {
                filtered_out = filtered_out.saturating_add(1);
            } else if weak {
                weak_hidden = weak_hidden.saturating_add(1);
            }
            let site_motif = motif(junction.start, junction.end);
            let resolved = resolve_junction_strand(junction, site_motif, options.library);
            entries.insert(
                *key,
                JunctionPresentation {
                    strand: resolved.strand,
                    source: resolved.source,
                    conflicting_tags: resolved.conflicting_tags,
                    motif: site_motif,
                    weak,
                    filtered,
                    start_site_total: start_total,
                    end_site_total: end_total,
                },
            );
        }
        let mut table = Self {
            entries,
            fingerprint: 0,
            weak_hidden,
            filtered_out,
            site_filter: options.site_filter,
            intron_filter_active: options.intron_filter.is_some(),
        };
        let mut keys = table.entries.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        table.fingerprint = table.fingerprint_for(keys);
        table
    }

    #[must_use]
    pub fn get(&self, start: u64, end: u64) -> Option<&JunctionPresentation> {
        self.entries.get(&(start, end))
    }

    /// The junction's presentation, or one from its own evidence when the table predates it.
    #[must_use]
    pub fn resolve(&self, evidence: &JunctionEvidence) -> JunctionPresentation {
        self.get(evidence.start, evidence.end)
            .copied()
            .unwrap_or_else(|| JunctionPresentation::from_evidence(evidence))
    }

    #[must_use]
    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Stable fingerprint of the presentations of `junctions` only, in the order given.
    #[must_use]
    pub fn fingerprint_for(&self, junctions: impl IntoIterator<Item = (u64, u64)>) -> u64 {
        let mut hash = Fnv::default();
        hash.write(b"sashimi-presentation-v1");
        for (start, end) in junctions {
            hash.write(&start.to_le_bytes());
            hash.write(&end.to_le_bytes());
            match self.entries.get(&(start, end)) {
                Some(presentation) => {
                    hash.write(&[1]);
                    presentation.hash_into(&mut hash);
                }
                None => hash.write(&[0]),
            }
        }
        // Never zero, so a key carrying a table is distinct from a key without one.
        hash.finish().max(1)
    }

    /// Junctions hidden as weak (not counting those a filter excludes).
    #[must_use]
    pub const fn weak_hidden(&self) -> usize {
        self.weak_hidden
    }

    #[must_use]
    pub const fn filtered_out(&self) -> usize {
        self.filtered_out
    }

    #[must_use]
    pub const fn site_filter(&self) -> Option<SpliceSite> {
        self.site_filter
    }

    #[must_use]
    pub const fn intron_filter_active(&self) -> bool {
        self.intron_filter_active
    }

    /// Every junction's `(start, end)` and presentation, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = ((u64, u64), &JunctionPresentation)> {
        self.entries.iter().map(|(key, value)| (*key, value))
    }

    /// The drawn junction end nearest `position` (a fractional genomic coordinate), within
    /// `tolerance` bases. A start site lies at the junction's `start` boundary, an end site at
    /// its `end` boundary.
    #[must_use]
    pub fn nearest_drawn_splice_site(&self, position: f64, tolerance: f64) -> Option<SpliceSite> {
        if !position.is_finite() || !tolerance.is_finite() {
            return None;
        }
        self.entries
            .iter()
            .filter(|(_, presentation)| !presentation.hidden())
            .flat_map(|((start, end), _)| [SpliceSite::start_of(*start), SpliceSite::end_of(*end)])
            .map(|site| (site, (site.position as f64 - position).abs()))
            .filter(|(_, distance)| *distance <= tolerance)
            .min_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)))
            .map(|(site, _)| site)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Copy)]
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

/// Thinnest Sashimi stroke, in points (a single read).
pub const SASHIMI_MINIMUM_STROKE: f64 = 1.0;
/// Thickest Sashimi stroke, in points (the scale maximum).
pub const SASHIMI_MAXIMUM_STROKE: f64 = 6.0;
/// Lowest arc, in points.
pub const SASHIMI_MINIMUM_ARC_HEIGHT: f64 = 4.0;
/// Space kept between an arc's apex and its band edge for its label.
pub const SASHIMI_LABEL_CLEARANCE: f64 = 12.0;
/// Space kept between an arc's apex and its band edge without labels.
pub const SASHIMI_UNLABELLED_CLEARANCE: f64 = 2.0;
/// Opacity of the other arcs while one is hovered.
pub const SASHIMI_HOVER_FADE_ALPHA: f32 = 0.25;

/// Room an arc may rise (or fall) from its band's baseline.
#[must_use]
pub fn sashimi_available_height(band_height: f64, labels: bool) -> f64 {
    let clearance = if labels {
        SASHIMI_LABEL_CLEARANCE
    } else {
        SASHIMI_UNLABELLED_CLEARANCE
    };
    (band_height * 0.5 - clearance).max(3.0)
}

/// Arc height from its on-screen span: `available × (1 − e^(−span / (2.5 × available)))`.
///
/// Strictly increasing in the span, so a shorter intron always rises less than a longer one
/// that contains it, and approaching `available` smoothly for long introns. Never lower than
/// [`SASHIMI_MINIMUM_ARC_HEIGHT`] or than the stroke plus one point.
#[must_use]
pub fn sashimi_arc_height(span_points: f64, available: f64, stroke: f64) -> f64 {
    let available = available.max(1.0);
    let span = if span_points.is_finite() {
        span_points.max(0.0)
    } else {
        0.0
    };
    let grown = available * (1.0 - (-span / (2.5 * available)).exp());
    grown
        .max(SASHIMI_MINIMUM_ARC_HEIGHT.min(available))
        .max(stroke + 1.0)
}

/// Stroke width from read support: `1 + 5 × √(value / maximum)`, from 1 to 6 points.
#[must_use]
pub fn sashimi_stroke_width(value: f64, scale_maximum: f64) -> f64 {
    let fraction = if scale_maximum.is_finite() && scale_maximum > 0.0 && value.is_finite() {
        (value / scale_maximum).clamp(0.0, 1.0)
    } else {
        0.0
    };
    SASHIMI_MINIMUM_STROKE + (SASHIMI_MAXIMUM_STROKE - SASHIMI_MINIMUM_STROKE) * fraction.sqrt()
}

/// A PSI share as a whole percent, or `<1%` for a share that would round to zero.
#[must_use]
pub fn format_psi(share: f64) -> String {
    let percent = share * 100.0;
    if percent > 0.0 && percent < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{percent:.0}%")
    }
}

/// The PSI label of a junction: `donor/acceptor`, e.g. `92%/88%`.
#[must_use]
pub fn psi_label(presentation: &JunctionPresentation, depth: u32) -> String {
    let (donor, acceptor) = presentation.donor_acceptor_psi(depth);
    let part = |share: Option<f64>| share.map_or_else(|| "–".to_owned(), format_psi);
    format!("{}/{}", part(donor), part(acceptor))
}

/// Greedy label placement that keeps the higher count on collision: candidates are taken in
/// descending `priority`, and one whose box (`[left, top, right, bottom]`, padded by `gap`)
/// overlaps an already kept box is dropped. Returns the kept indices in input order.
#[must_use]
pub fn place_labels_by_priority(boxes: &[([f64; 4], u64)], gap: f64) -> Vec<usize> {
    let mut order = (0..boxes.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| (std::cmp::Reverse(boxes[*index].1), *index));
    let mut kept = Vec::<usize>::new();
    for index in order {
        let (bounds, _) = boxes[index];
        if kept.iter().any(|other| {
            let other = boxes[*other].0;
            bounds[0] < other[2] + gap
                && bounds[2] > other[0] - gap
                && bounds[1] < other[3] + gap
                && bounds[3] > other[1] - gap
        }) {
            continue;
        }
        kept.push(index);
    }
    kept.sort_unstable();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn junction(start: u64, end: u64, depth: u32) -> JunctionEvidence {
        JunctionEvidence {
            start,
            end,
            depth,
            unknown_depth: depth,
            ..JunctionEvidence::default()
        }
    }

    #[test]
    fn every_canonical_motif_names_its_strand_on_both_strands() {
        let cases = [
            (
                *b"GT",
                *b"AG",
                SpliceMotif::GtAg,
                JunctionStrand::Positive,
                "GT-AG",
            ),
            (
                *b"gc",
                *b"ag",
                SpliceMotif::GcAg,
                JunctionStrand::Positive,
                "GC-AG",
            ),
            (
                *b"AT",
                *b"AC",
                SpliceMotif::AtAc,
                JunctionStrand::Positive,
                "AT-AC",
            ),
            (
                *b"CT",
                *b"AC",
                SpliceMotif::CtAc,
                JunctionStrand::Negative,
                "GT-AG",
            ),
            (
                *b"CT",
                *b"GC",
                SpliceMotif::CtGc,
                JunctionStrand::Negative,
                "GC-AG",
            ),
            (
                *b"GT",
                *b"AT",
                SpliceMotif::GtAt,
                JunctionStrand::Negative,
                "AT-AC",
            ),
        ];
        for (first, last, motif, strand, label) in cases {
            let classified = SpliceMotif::classify(first, last);
            assert_eq!(classified, motif);
            assert_eq!(classified.strand(), strand);
            assert_eq!(classified.transcript_label(), label);
        }
        for (first, last) in [(*b"AA", *b"AG"), (*b"GT", *b"GG"), (*b"NN", *b"NN")] {
            let motif = SpliceMotif::classify(first, last);
            assert_eq!(motif, SpliceMotif::NonCanonical);
            assert_eq!(motif.strand(), JunctionStrand::Unknown);
        }
        assert_eq!(SpliceMotif::Unavailable.strand(), JunctionStrand::Unknown);
    }

    #[test]
    fn tag_beats_motif_beats_library_and_otherwise_unknown() {
        let tagged = JunctionEvidence {
            depth: 5,
            tagged_negative_depth: 3,
            tagged_positive_depth: 1,
            first_of_pair_forward_depth: 5,
            ..JunctionEvidence::default()
        };
        let resolved = resolve_junction_strand(
            &tagged,
            SpliceMotif::GtAg,
            LibraryStrandedness::FirstOfPairSense,
        );
        assert_eq!(resolved.strand, JunctionStrand::Negative);
        assert_eq!(resolved.source, JunctionStrandSource::Tag);
        assert!(resolved.conflicting_tags);

        let untagged = JunctionEvidence {
            depth: 5,
            first_of_pair_forward_depth: 5,
            ..JunctionEvidence::default()
        };
        let motif = resolve_junction_strand(
            &untagged,
            SpliceMotif::CtAc,
            LibraryStrandedness::FirstOfPairSense,
        );
        assert_eq!(
            (motif.strand, motif.source),
            (JunctionStrand::Negative, JunctionStrandSource::Motif)
        );

        for (library, strand) in [
            (
                LibraryStrandedness::FirstOfPairSense,
                JunctionStrand::Positive,
            ),
            (
                LibraryStrandedness::FirstOfPairAntisense,
                JunctionStrand::Negative,
            ),
        ] {
            let resolved = resolve_junction_strand(&untagged, SpliceMotif::NonCanonical, library);
            assert_eq!(
                (resolved.strand, resolved.source),
                (strand, JunctionStrandSource::Library)
            );
        }
        let unknown = resolve_junction_strand(
            &untagged,
            SpliceMotif::NonCanonical,
            LibraryStrandedness::Unstranded,
        );
        assert_eq!(
            (unknown.strand, unknown.source),
            (JunctionStrand::Unknown, JunctionStrandSource::Unknown)
        );
        // Untagged reads in both orientations (both mates of fragments) never decide a strand
        // without a motif or library: this is the alignment-strand fallback that was removed.
        let fallback_only = JunctionEvidence {
            depth: 20,
            unknown_depth: 20,
            fallback_positive_depth: 11,
            fallback_negative_depth: 9,
            first_of_pair_forward_depth: 20,
            ..JunctionEvidence::default()
        };
        let resolved = resolve_junction_strand(
            &fallback_only,
            SpliceMotif::Unavailable,
            LibraryStrandedness::Unstranded,
        );
        assert_eq!(resolved.strand, JunctionStrand::Unknown);
    }

    #[test]
    fn weak_junctions_are_hidden_by_absolute_and_shared_site_relative_thresholds() {
        let junctions = [
            junction(100, 200, 1_000),
            // Shares the start with the strong junction and has < 1% of it.
            junction(100, 300, 9),
            // Shares the start but has exactly 1%.
            junction(100, 400, 10),
            // Shares no site: only the absolute threshold applies.
            junction(500, 600, 3),
            // Below the absolute threshold.
            junction(700, 800, 1),
        ];
        let table = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions::default(),
        );
        let weak = |start, end| table.get(start, end).unwrap().weak;
        assert!(!weak(100, 200));
        assert!(weak(100, 300));
        assert!(!weak(100, 400));
        assert!(!weak(500, 600));
        assert!(weak(700, 800));
        assert_eq!(table.weak_hidden(), 2);
        // Hidden junctions still count toward their splice site.
        assert_eq!(table.get(100, 200).unwrap().start_site_total, 1_019);

        let relaxed = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions {
                minimum_depth: 1,
                relative_threshold_percent: 0.5,
                ..SashimiPresentationOptions::default()
            },
        );
        assert_eq!(relaxed.weak_hidden(), 0);
        let all = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions {
                show_all: true,
                ..SashimiPresentationOptions::default()
            },
        );
        assert_eq!(all.weak_hidden(), 0);
        assert!(all.get(700, 800).is_some_and(|entry| !entry.hidden()));
        assert_ne!(all.fingerprint(), table.fingerprint());
    }

    #[test]
    fn psi_is_the_share_of_each_splice_site_on_the_transcript_strand() {
        let junctions = [
            junction(100, 200, 90),
            junction(100, 300, 10),
            junction(150, 300, 30),
        ];
        let table = SashimiPresentationTable::build(
            &junctions,
            |start, _| {
                if start == 100 {
                    SpliceMotif::GtAg
                } else {
                    SpliceMotif::CtAc
                }
            },
            &SashimiPresentationOptions {
                minimum_depth: 1,
                relative_threshold_percent: 0.0,
                ..SashimiPresentationOptions::default()
            },
        );
        let skipping = table.get(100, 300).unwrap();
        assert_eq!(skipping.strand, JunctionStrand::Positive);
        // Donor (start, + strand): 10 of 100; acceptor (end): 10 of 40.
        let (donor, acceptor) = skipping.donor_acceptor_psi(10);
        assert!((donor.unwrap() - 0.10).abs() < 1e-12);
        assert!((acceptor.unwrap() - 0.25).abs() < 1e-12);
        assert_eq!(psi_label(skipping, 10), "10%/25%");
        // A minus-strand junction's donor is its end.
        let minus = table.get(150, 300).unwrap();
        assert_eq!(minus.strand, JunctionStrand::Negative);
        let (donor, acceptor) = minus.donor_acceptor_psi(30);
        assert!((donor.unwrap() - 0.75).abs() < 1e-12);
        assert!((acceptor.unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(format_psi(0.001), "<1%");
    }

    #[test]
    fn site_and_intron_filters_keep_only_matching_junctions() {
        let junctions = [
            junction(100, 200, 50),
            junction(100, 300, 40),
            junction(150, 300, 30),
        ];
        let options = SashimiPresentationOptions {
            site_filter: Some(SpliceSite::end_of(300)),
            ..SashimiPresentationOptions::default()
        };
        let table =
            SashimiPresentationTable::build(&junctions, |_, _| SpliceMotif::Unavailable, &options);
        assert!(table.get(100, 200).unwrap().filtered);
        assert!(!table.get(100, 300).unwrap().hidden());
        assert!(!table.get(150, 300).unwrap().hidden());
        assert_eq!(table.filtered_out(), 1);
        assert_eq!(table.site_filter(), Some(SpliceSite::end_of(300)));
        assert_eq!(SpliceSite::end_of(300).one_based_position(), 300);
        assert_eq!(SpliceSite::start_of(100).one_based_position(), 101);

        let cleared = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions::default(),
        );
        assert_eq!(cleared.filtered_out(), 0);
        assert_ne!(cleared.fingerprint(), table.fingerprint());

        let introns = Arc::new(BTreeSet::from([(100, 200)]));
        let transcript = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions {
                intron_filter: Some(introns),
                ..SashimiPresentationOptions::default()
            },
        );
        assert!(!transcript.get(100, 200).unwrap().hidden());
        assert_eq!(transcript.filtered_out(), 2);
    }

    #[test]
    fn the_nearest_drawn_splice_site_is_found_within_tolerance() {
        let junctions = [junction(100, 200, 50), junction(150, 300, 1)];
        let table = SashimiPresentationTable::build(
            &junctions,
            |_, _| SpliceMotif::Unavailable,
            &SashimiPresentationOptions::default(),
        );
        assert_eq!(
            table.nearest_drawn_splice_site(199.2, 2.0),
            Some(SpliceSite::end_of(200))
        );
        assert_eq!(
            table.nearest_drawn_splice_site(101.0, 2.0),
            Some(SpliceSite::start_of(100))
        );
        // The weak junction (one read) is hidden, so its sites cannot be picked.
        assert_eq!(table.nearest_drawn_splice_site(150.0, 2.0), None);
        assert_eq!(table.nearest_drawn_splice_site(250.0, 2.0), None);
    }

    #[test]
    fn arc_height_grows_with_span_and_stroke_with_count() {
        let available = 33.0;
        let heights =
            [5.0, 20.0, 80.0, 300.0, 2_000.0].map(|span| sashimi_arc_height(span, available, 1.0));
        for pair in heights.windows(2) {
            assert!(pair[0] < pair[1], "{heights:?}");
        }
        assert!(heights[4] <= available);
        assert!(heights[0] >= SASHIMI_MINIMUM_ARC_HEIGHT);
        assert!((sashimi_stroke_width(0.0, 100.0) - 1.0).abs() < 1e-12);
        assert!((sashimi_stroke_width(100.0, 100.0) - 6.0).abs() < 1e-12);
        assert!((sashimi_stroke_width(500.0, 100.0) - 6.0).abs() < 1e-12);
        assert!(sashimi_stroke_width(25.0, 100.0) > sashimi_stroke_width(4.0, 100.0));
        // A thick stroke still leaves a visible curve.
        assert!(sashimi_arc_height(1.0, available, 6.0) >= 7.0);
    }

    #[test]
    fn label_collision_keeps_the_higher_count() {
        let boxes = [
            ([0.0, 0.0, 20.0, 10.0], 5),
            ([10.0, 0.0, 30.0, 10.0], 900),
            ([100.0, 0.0, 120.0, 10.0], 1),
        ];
        assert_eq!(place_labels_by_priority(&boxes, 2.0), vec![1, 2]);
    }
}
