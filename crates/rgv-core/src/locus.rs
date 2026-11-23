//! Canonical parsing of user-typed loci.
//!
//! Every surface that accepts a locus (the locus box, the command line,
//! automation, IGV session import) should parse it here so the accepted
//! spellings cannot drift between them. Typed coordinates are one-based and
//! inclusive; parsed coordinates are zero-based and half-open, as everywhere
//! else in RGV.

use thiserror::Error;

use crate::{
    ContigAliasError, ContigAliases, ContigId, GenomicInterval, ReferenceDictionary,
    resolve_contig_alias,
};

/// The coordinate part of a typed locus, zero-based and half-open.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocusRange {
    /// A bare contig name such as `chr1`.
    WholeContig,
    /// A single typed position such as `chr1:12,345` (stored zero-based).
    Position(u64),
    /// A typed range such as `chr1:100-200`, stored as `start..end`.
    Interval { start: u64, end: u64 },
}

/// A syntactically parsed locus whose contig has not been resolved.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Locus {
    pub contig: String,
    pub range: LocusRange,
}

/// A locus resolved against one reference dictionary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLocus {
    pub contig: ContigId,
    /// The dictionary's canonical spelling, not the typed one.
    pub contig_name: String,
    pub contig_length: u64,
    pub range: LocusRange,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LocusError {
    #[error("locus is empty")]
    Empty,
    #[error("locus coordinates must be one-based and forward, got {0}")]
    InvalidRange(String),
    #[error("no contig named {0}")]
    UnknownContig(String),
    #[error("contig {0} has zero length")]
    EmptyContig(String),
    #[error("locus starts at {start}, beyond the end of {contig} ({contig_length} bp)")]
    BeyondContig {
        contig: String,
        start: u64,
        contig_length: u64,
    },
    #[error(transparent)]
    Alias(#[from] ContigAliasError),
}

impl Locus {
    /// Parses a locus without a dictionary.
    ///
    /// Accepted forms: `chr1`, `chr1:12345`, `chr1:1,234-5,678`, and the
    /// whitespace- or tab-separated `chr1 100 200` / `chr1 100`. The contig is
    /// split at the *last* colon, and only when what follows is a coordinate
    /// range, so `HLA-A*01:01:01:01:100-200` and `HLA-DRB1*15:01:01:01N` keep
    /// their full names. A colon-containing name that ends in digits (the bare
    /// `HLA-A*01:01:01:01`) is ambiguous without a dictionary; use
    /// [`Locus::resolve`] to disambiguate it.
    pub fn parse(text: &str) -> Result<Self, LocusError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(LocusError::Empty);
        }
        if let Some(locus) = parse_whitespace_separated(text)? {
            return Ok(locus);
        }
        if let Some((contig, range)) = text.rsplit_once(':')
            && !contig.trim().is_empty()
            && looks_like_range(range)
        {
            return Ok(Self {
                contig: contig.trim().to_owned(),
                range: parse_range(range)?,
            });
        }
        Ok(Self {
            contig: text.to_owned(),
            range: LocusRange::WholeContig,
        })
    }

    /// Parses and resolves a locus against `dictionary`.
    ///
    /// The contig is the longest prefix that names a dictionary contig (see
    /// [`resolve_contig_name`]) and is followed by nothing or by a coordinate
    /// range, so a contig whose own name contains colons and digits resolves
    /// to itself rather than to a position on a shorter name.
    pub fn resolve(
        text: &str,
        dictionary: &ReferenceDictionary,
        aliases: &ContigAliases,
    ) -> Result<ResolvedLocus, LocusError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(LocusError::Empty);
        }
        let resolved = |contig: ContigId, range: LocusRange| -> Result<ResolvedLocus, LocusError> {
            let entry = dictionary
                .get(contig)
                .ok_or_else(|| LocusError::UnknownContig(text.to_owned()))?;
            Ok(ResolvedLocus {
                contig,
                contig_name: entry.name().to_owned(),
                contig_length: entry.length(),
                range,
            })
        };
        if let Some(locus) = parse_whitespace_separated(text)? {
            let contig = resolve_contig_name(dictionary, aliases, &locus.contig)?
                .ok_or(LocusError::UnknownContig(locus.contig))?;
            return resolved(contig, locus.range);
        }
        // Longest candidate first: the whole text, then each colon from the right.
        if let Some(contig) = resolve_contig_name(dictionary, aliases, text)? {
            return resolved(contig, LocusRange::WholeContig);
        }
        let mut range_error = None;
        for (colon, _) in text.rmatch_indices(':') {
            let (name, range) = (text[..colon].trim(), &text[colon + 1..]);
            if name.is_empty() || !looks_like_range(range) {
                continue;
            }
            let Some(contig) = resolve_contig_name(dictionary, aliases, name)? else {
                continue;
            };
            match parse_range(range) {
                Ok(range) => return resolved(contig, range),
                Err(error) => range_error = range_error.or(Some(error)),
            }
        }
        Err(range_error.unwrap_or_else(|| {
            LocusError::UnknownContig(
                Self::parse(text).map_or_else(|_| text.to_owned(), |l| l.contig),
            )
        }))
    }
}

impl ResolvedLocus {
    /// The interval to display, clamped to the contig. A single position is
    /// widened by `position_flank` bases on each side.
    pub fn interval(&self, position_flank: u64) -> Result<GenomicInterval, LocusError> {
        if self.contig_length == 0 {
            return Err(LocusError::EmptyContig(self.contig_name.clone()));
        }
        let (start, end) = match self.range {
            LocusRange::WholeContig => (0, self.contig_length),
            LocusRange::Position(position) => (
                position.saturating_sub(position_flank),
                position.saturating_add(1).saturating_add(position_flank),
            ),
            LocusRange::Interval { start, end } => (start, end),
        };
        let anchor = match self.range {
            LocusRange::Position(position) => position,
            _ => start,
        };
        if anchor >= self.contig_length {
            return Err(LocusError::BeyondContig {
                contig: self.contig_name.clone(),
                start: anchor.saturating_add(1),
                contig_length: self.contig_length,
            });
        }
        GenomicInterval::new(self.contig, start, end.min(self.contig_length))
            .map_err(|_| LocusError::InvalidRange(format!("{start}-{end}")))
    }
}

/// Resolves a typed contig name against a dictionary.
///
/// In order: the exact name or an explicit alias; the `chr`-prefix and
/// mitochondrial spellings (`1`/`chr1`, `M`/`MT`/`chrM`/`chrMT`); then the same
/// candidates ASCII case-insensitively, provided exactly one contig matches.
pub fn resolve_contig_name(
    dictionary: &ReferenceDictionary,
    aliases: &ContigAliases,
    name: &str,
) -> Result<Option<ContigId>, ContigAliasError> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(contig) = resolve_contig_alias(dictionary, aliases, name)? {
        return Ok(Some(contig));
    }
    let candidates = contig_spellings(name);
    if let Some(contig) = candidates
        .iter()
        .find_map(|candidate| dictionary.id_by_name(candidate))
    {
        return Ok(Some(contig));
    }
    // Explicit aliases typed in another case.
    let mut alias_targets = aliases
        .iter()
        .filter(|(alias, _)| alias.eq_ignore_ascii_case(name))
        .filter_map(|(_, target)| dictionary.id_by_name(target));
    if let Some(contig) = alias_targets.next()
        && alias_targets.all(|other| other == contig)
    {
        return Ok(Some(contig));
    }
    // Earlier candidates are closer to what was typed and win; within one
    // candidate, two contigs differing only by case are ambiguous.
    for candidate in &candidates {
        let mut matches = dictionary
            .iter()
            .filter(|contig| contig.name().eq_ignore_ascii_case(candidate));
        if let Some(first) = matches.next() {
            return Ok(matches.next().is_none().then(|| first.id()));
        }
    }
    Ok(None)
}

/// Splits a whitespace-separated expression into its loci.
///
/// `chr2:20-40 chr1:1-10` is two loci, but a contig token followed by one or
/// two bare coordinates (`chr1 100 200`, `chr1<TAB>12345`, as pasted from a
/// table) is a single locus and is returned whole for [`Locus::parse`] or
/// [`Locus::resolve`]. That form wins over reading the coordinates as
/// numerically named contigs.
#[must_use]
pub fn split_loci(expression: &str) -> Vec<&str> {
    let tokens = expression.split_whitespace().collect::<Vec<_>>();
    if matches!(tokens.len(), 2 | 3) && tokens[1..].iter().all(|token| is_coordinate(token)) {
        return vec![expression.trim()];
    }
    tokens
}

/// `name` followed by its `chr`-prefix and mitochondrial equivalents.
pub(crate) fn contig_spellings(name: &str) -> Vec<String> {
    let has_prefix = name
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("chr"));
    let base = if has_prefix { &name[3..] } else { name };
    let mut spellings = vec![name.to_owned()];
    let mut push = |spelling: String| {
        if !spelling.is_empty() && !spellings.contains(&spelling) {
            spellings.push(spelling);
        }
    };
    if has_prefix {
        push(base.to_owned());
    } else {
        push(format!("chr{base}"));
    }
    if base.eq_ignore_ascii_case("M") || base.eq_ignore_ascii_case("MT") {
        for mitochondrial in ["chrM", "MT", "chrMT", "M"] {
            push(mitochondrial.to_owned());
        }
    }
    spellings
}

/// `chr1 100 200`, `chr1\t100\t200` and `chr1 100`: a contig token followed by
/// one or two coordinates. Anything else is not this form.
fn parse_whitespace_separated(text: &str) -> Result<Option<Locus>, LocusError> {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let range = match tokens.as_slice() {
        [_, position] if is_coordinate(position) => (*position).to_owned(),
        [_, start, end] if is_coordinate(start) && is_coordinate(end) => format!("{start}-{end}"),
        _ => return Ok(None),
    };
    Ok(Some(Locus {
        contig: tokens[0].to_owned(),
        range: parse_range(&range)?,
    }))
}

pub(crate) fn is_coordinate(token: &str) -> bool {
    token.bytes().any(|byte| byte.is_ascii_digit())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b',')
}

/// Whether `text` is shaped like `position` or `start-end` (not whether its
/// values are valid), which decides where the contig name ends.
fn looks_like_range(text: &str) -> bool {
    let text = text.trim();
    match text.split_once('-') {
        Some((start, end)) => is_coordinate(start.trim()) && is_coordinate(end.trim()),
        None => is_coordinate(text),
    }
}

fn parse_range(text: &str) -> Result<LocusRange, LocusError> {
    let invalid = || LocusError::InvalidRange(text.trim().to_owned());
    let coordinate = |value: &str| {
        value
            .trim()
            .replace(',', "")
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(invalid)
    };
    match text.split_once('-') {
        Some((start, end)) => {
            let (start, end) = (coordinate(start)?, coordinate(end)?);
            if end < start {
                return Err(invalid());
            }
            Ok(LocusRange::Interval {
                start: start - 1,
                end,
            })
        }
        None => Ok(LocusRange::Position(coordinate(text)? - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_split_into_loci_but_keep_tabular_coordinates_together() {
        assert_eq!(
            split_loci("chr2:20-40  chr1:1-10\nchrX"),
            vec!["chr2:20-40", "chr1:1-10", "chrX"]
        );
        assert_eq!(split_loci(" chr1\t12345 "), vec!["chr1\t12345"]);
        assert_eq!(split_loci("chr1 100 200"), vec!["chr1 100 200"]);
        assert_eq!(split_loci("chr1 1,000 2,000"), vec!["chr1 1,000 2,000"]);
        // Four tokens, or a non-coordinate second token, is a list of loci.
        assert_eq!(split_loci("chr1 100 200 300").len(), 4);
        assert_eq!(split_loci("chr1 chr2"), vec!["chr1", "chr2"]);
        assert!(split_loci("  ").is_empty());
    }

    fn dictionary() -> ReferenceDictionary {
        ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 248_956_422),
            ("chrM".to_owned(), 16_569),
            ("HLA-A*01:01:01:01".to_owned(), 3_503),
            ("HLA-A*01:01".to_owned(), 9_000),
            ("giant".to_owned(), 40_000_000_000),
            ("7".to_owned(), 1_000),
        ])
    }

    fn resolve(text: &str) -> Result<ResolvedLocus, LocusError> {
        Locus::resolve(text, &dictionary(), &ContigAliases::default())
    }

    #[test]
    fn parses_every_typed_form_without_a_dictionary() {
        let interval = |start, end| LocusRange::Interval { start, end };
        for (text, contig, range) in [
            ("chr1", "chr1", LocusRange::WholeContig),
            ("  chr1:12345 ", "chr1", LocusRange::Position(12_344)),
            ("chr1:1,234-5,678", "chr1", interval(1_233, 5_678)),
            ("chr1: 100 - 200", "chr1", interval(99, 200)),
            ("chr1 100 200", "chr1", interval(99, 200)),
            ("chr1\t1,000\t2,000", "chr1", interval(999, 2_000)),
            ("chr1 100", "chr1", LocusRange::Position(99)),
            ("chr1:5-5", "chr1", interval(4, 5)),
            (
                "HLA-A*01:01:01:01:100-200",
                "HLA-A*01:01:01:01",
                interval(99, 200),
            ),
            (
                "HLA-DRB1*15:01:01:01N",
                "HLA-DRB1*15:01:01:01N",
                LocusRange::WholeContig,
            ),
            ("All", "All", LocusRange::WholeContig),
        ] {
            assert_eq!(
                Locus::parse(text),
                Ok(Locus {
                    contig: contig.to_owned(),
                    range
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_zero_and_reversed_coordinates() {
        assert_eq!(Locus::parse("  "), Err(LocusError::Empty));
        for text in ["chr1:0-10", "chr1:200-100", "chr1:0", "chr1 200 100"] {
            assert!(
                matches!(Locus::parse(text), Err(LocusError::InvalidRange(_))),
                "{text:?}"
            );
        }
        assert!(matches!(
            Locus::parse("chr1:99999999999999999999999-5"),
            Err(LocusError::InvalidRange(_))
        ));
    }

    #[test]
    fn resolves_the_longest_dictionary_name_prefix() {
        // The bare name wins over "position 1 of HLA-A*01:01:01".
        let bare = resolve("HLA-A*01:01:01:01").unwrap();
        assert_eq!(bare.contig_name, "HLA-A*01:01:01:01");
        assert_eq!(bare.range, LocusRange::WholeContig);

        let ranged = resolve("HLA-A*01:01:01:01:100-200").unwrap();
        assert_eq!(ranged.contig_name, "HLA-A*01:01:01:01");
        assert_eq!(
            ranged.range,
            LocusRange::Interval {
                start: 99,
                end: 200
            }
        );

        // Only the shorter name exists for this spelling, so the tail is a position.
        let shorter = resolve("HLA-A*01:01:7").unwrap();
        assert_eq!(shorter.contig_name, "HLA-A*01:01");
        assert_eq!(shorter.range, LocusRange::Position(6));

        assert_eq!(
            resolve("chrUn:5"),
            Err(LocusError::UnknownContig("chrUn".to_owned()))
        );
        assert!(matches!(
            resolve("chr1:9-3"),
            Err(LocusError::InvalidRange(_))
        ));
    }

    #[test]
    fn resolves_case_prefix_and_mitochondrial_aliases() {
        for (typed, canonical) in [
            ("1", "chr1"),
            ("CHR1", "chr1"),
            ("Chr1:5-9", "chr1"),
            ("MT", "chrM"),
            ("mt", "chrM"),
            ("chrMT", "chrM"),
            ("M", "chrM"),
            ("chr7", "7"),
            ("CHR7 5 9", "7"),
            ("GIANT", "giant"),
            ("hla-a*01:01:01:01", "HLA-A*01:01:01:01"),
        ] {
            assert_eq!(resolve(typed).unwrap().contig_name, canonical, "{typed:?}");
        }

        let aliases = ContigAliases::from([("NC_000001.11".to_owned(), "chr1".to_owned())]);
        let resolved = Locus::resolve("nc_000001.11:10-20", &dictionary(), &aliases).unwrap();
        assert_eq!(resolved.contig_name, "chr1");

        // Two contigs that differ only by case are never guessed between.
        let ambiguous = ReferenceDictionary::from_contigs([
            ("Scaffold".to_owned(), 10),
            ("SCAFFOLD".to_owned(), 10),
        ]);
        assert_eq!(
            resolve_contig_name(&ambiguous, &ContigAliases::default(), "scaffold"),
            Ok(None)
        );
        assert_eq!(
            resolve_contig_name(&ambiguous, &ContigAliases::default(), "SCAFFOLD"),
            Ok(Some(ContigId::new(1)))
        );
    }

    #[test]
    fn intervals_are_clamped_and_exact_above_four_gibibases() {
        let whole = resolve("chrM").unwrap().interval(20).unwrap();
        assert_eq!((whole.start(), whole.end()), (0, 16_569));

        let position = resolve("chr1:10").unwrap().interval(20).unwrap();
        assert_eq!((position.start(), position.end()), (0, 30));

        let clamped = resolve("chrM:16,000-99,999").unwrap().interval(20).unwrap();
        assert_eq!((clamped.start(), clamped.end()), (15_999, 16_569));
        assert!(matches!(
            resolve("chrM:20,000-30,000").unwrap().interval(20),
            Err(LocusError::BeyondContig { .. })
        ));

        // Coordinates beyond u32 and beyond 2^32 survive untouched.
        let large = resolve("giant:4,294,967,297-34,359,738,368")
            .unwrap()
            .interval(0)
            .unwrap();
        assert_eq!(large.start(), (1_u64 << 32));
        assert_eq!(large.end(), 1_u64 << 35);
        assert_eq!(large.len(), (1_u64 << 35) - (1_u64 << 32));
        let point = resolve("giant\t39999999999").unwrap().interval(5).unwrap();
        assert_eq!(
            (point.start(), point.end()),
            (39_999_999_993, 40_000_000_000)
        );
    }
}
