use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable index into a [`ReferenceDictionary`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ContigId(u32);

impl ContigId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Contig {
    id: ContigId,
    name: String,
    length: u64,
}

impl Contig {
    pub fn new(id: ContigId, name: impl Into<String>, length: u64) -> Self {
        Self {
            id,
            name: name.into(),
            length,
        }
    }

    pub const fn id(&self) -> ContigId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// Ordered contig metadata from an alignment or reference header.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReferenceDictionary {
    contigs: Vec<Contig>,
    #[serde(skip)]
    names: HashMap<String, ContigId>,
}

impl ReferenceDictionary {
    pub fn from_contigs(contigs: impl IntoIterator<Item = (String, u64)>) -> Self {
        let mut dictionary = Self::default();
        for (name, length) in contigs {
            dictionary.push(name, length);
        }
        dictionary
    }

    pub fn push(&mut self, name: impl Into<String>, length: u64) -> ContigId {
        let id = ContigId::new(u32::try_from(self.contigs.len()).expect("too many contigs"));
        let name = name.into();
        self.names.insert(name.clone(), id);
        self.contigs.push(Contig::new(id, name, length));
        id
    }

    pub fn get(&self, id: ContigId) -> Option<&Contig> {
        usize::try_from(id.get())
            .ok()
            .and_then(|index| self.contigs.get(index))
    }

    pub fn id_by_name(&self, name: &str) -> Option<ContigId> {
        self.names.get(name).copied().or_else(|| {
            // Rebuild-on-read makes a serde round trip valid without serializing the index.
            self.contigs
                .iter()
                .find(|contig| contig.name() == name)
                .map(Contig::id)
        })
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Contig> {
        self.contigs.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.contigs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.contigs.len()
    }
}

pub type ContigAliases = BTreeMap<String, String>;

/// Resolves an exact canonical contig name or an explicitly configured alias.
/// Canonical names always remain meaningful and conflicting aliases are rejected.
pub fn resolve_contig_alias(
    dictionary: &ReferenceDictionary,
    aliases: &ContigAliases,
    requested: &str,
) -> Result<Option<ContigId>, ContigAliasError> {
    let canonical = dictionary.id_by_name(requested);
    let Some(target) = aliases.get(requested) else {
        return Ok(canonical);
    };
    let target_id =
        dictionary
            .id_by_name(target)
            .ok_or_else(|| ContigAliasError::UnknownTarget {
                alias: requested.to_owned(),
                target: target.clone(),
            })?;
    if let Some(canonical_id) = canonical
        && canonical_id != target_id
    {
        return Err(ContigAliasError::CanonicalConflict {
            alias: requested.to_owned(),
            target: target.clone(),
        });
    }
    Ok(Some(target_id))
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContigAliasError {
    #[error("contig alias {alias} targets unknown canonical contig {target}")]
    UnknownTarget { alias: String, target: String },
    #[error("contig alias {alias} conflicts with that canonical contig and targets {target}")]
    CanonicalConflict { alias: String, target: String },
}

/// A validated zero-based, half-open genomic interval.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct GenomicInterval {
    contig: ContigId,
    start: u64,
    end: u64,
}

impl GenomicInterval {
    pub fn new(contig: ContigId, start: u64, end: u64) -> Result<Self, IntervalError> {
        if end <= start {
            return Err(IntervalError::EmptyOrReversed { start, end });
        }
        Ok(Self { contig, start, end })
    }

    pub fn bounded(
        contig: ContigId,
        start: u64,
        end: u64,
        contig_length: u64,
    ) -> Result<Self, IntervalError> {
        if end > contig_length {
            return Err(IntervalError::BeyondContig { end, contig_length });
        }
        Self::new(contig, start, end)
    }

    pub const fn contig(self) -> ContigId {
        self.contig
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Validated genomic intervals are always non-empty.
    pub const fn is_empty(self) -> bool {
        false
    }

    pub const fn contains(self, position: u64) -> bool {
        position >= self.start && position < self.end
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.contig.0 == other.contig.0 && self.start < other.end && other.start < self.end
    }

    pub const fn contains_interval(self, other: Self) -> bool {
        self.contig.0 == other.contig.0 && self.start <= other.start && self.end >= other.end
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        if !self.overlaps(other) {
            return None;
        }
        Some(Self {
            contig: self.contig,
            start: self.start.max(other.start),
            end: self.end.min(other.end),
        })
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IntervalError {
    #[error("genomic interval must be non-empty and forward: {start}..{end}")]
    EmptyOrReversed { start: u64, end: u64 },
    #[error("interval end {end} exceeds contig length {contig_length}")]
    BeyondContig { end: u64, contig_length: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_are_half_open() {
        let interval = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        assert!(!interval.contains(9));
        assert!(interval.contains(10));
        assert!(interval.contains(19));
        assert!(!interval.contains(20));
    }

    #[test]
    fn intersection_requires_matching_contigs() {
        let left = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let overlapping = GenomicInterval::new(ContigId::new(0), 15, 30).unwrap();
        let other_contig = GenomicInterval::new(ContigId::new(1), 15, 30).unwrap();

        assert_eq!(left.intersection(overlapping).unwrap().len(), 5);
        assert_eq!(left.intersection(other_contig), None);
    }

    #[test]
    fn dictionary_survives_serialization() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 100)]);
        let json = serde_json::to_string(&dictionary).unwrap();
        let decoded: ReferenceDictionary = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id_by_name("chr1"), Some(ContigId::new(0)));
    }

    #[test]
    fn explicit_aliases_resolve_and_conflicts_are_rejected() {
        let dictionary =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("1".to_owned(), 100)]);
        let aliases = ContigAliases::from([
            ("one".to_owned(), "chr1".to_owned()),
            ("missing".to_owned(), "chrMissing".to_owned()),
            ("1".to_owned(), "chr1".to_owned()),
        ]);

        assert_eq!(
            resolve_contig_alias(&dictionary, &aliases, "one").unwrap(),
            Some(ContigId::new(0))
        );
        assert!(matches!(
            resolve_contig_alias(&dictionary, &aliases, "missing"),
            Err(ContigAliasError::UnknownTarget { .. })
        ));
        assert!(matches!(
            resolve_contig_alias(&dictionary, &aliases, "1"),
            Err(ContigAliasError::CanonicalConflict { .. })
        ));
    }

    #[test]
    fn interval_intersection_obeys_overlap_properties() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let left_start = state % 10_000;
            let left_end = left_start + 1 + (state.rotate_left(11) % 1_000);
            let right_start = state.rotate_left(23) % 10_000;
            let right_end = right_start + 1 + (state.rotate_left(37) % 1_000);
            let left = GenomicInterval::new(ContigId::new(0), left_start, left_end).unwrap();
            let right = GenomicInterval::new(ContigId::new(0), right_start, right_end).unwrap();

            assert_eq!(left.overlaps(right), right.overlaps(left));
            assert_eq!(left.intersection(right), right.intersection(left));
            assert_eq!(left.intersection(right).is_some(), left.overlaps(right));
            if let Some(intersection) = left.intersection(right) {
                assert!(left.contains_interval(intersection));
                assert!(right.contains_interval(intersection));
            }
        }
    }
}
