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
///
/// Deserialization rebuilds the name index, reassigns every [`ContigId`] from
/// its position (the identifier is defined as the index), and rejects
/// duplicate names, so a decoded dictionary is indistinguishable from one
/// built through [`ReferenceDictionary::from_contigs`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SerializedReferenceDictionary")]
pub struct ReferenceDictionary {
    contigs: Vec<Contig>,
    #[serde(skip)]
    names: HashMap<String, ContigId>,
}

#[derive(Deserialize)]
struct SerializedReferenceDictionary {
    contigs: Vec<Contig>,
}

impl TryFrom<SerializedReferenceDictionary> for ReferenceDictionary {
    type Error = DuplicateContigError;

    fn try_from(serialized: SerializedReferenceDictionary) -> Result<Self, Self::Error> {
        let mut dictionary = Self::default();
        for contig in serialized.contigs {
            dictionary.try_push(contig.name, contig.length)?;
        }
        Ok(dictionary)
    }
}

/// A contig name was supplied more than once to one dictionary.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("reference dictionary already contains contig {name}")]
pub struct DuplicateContigError {
    pub name: String,
}

impl ReferenceDictionary {
    pub fn from_contigs(contigs: impl IntoIterator<Item = (String, u64)>) -> Self {
        let mut dictionary = Self::default();
        for (name, length) in contigs {
            dictionary.push(name, length);
        }
        dictionary
    }

    /// Builds a dictionary, rejecting a repeated contig name.
    pub fn try_from_contigs(
        contigs: impl IntoIterator<Item = (String, u64)>,
    ) -> Result<Self, DuplicateContigError> {
        let mut dictionary = Self::default();
        for (name, length) in contigs {
            dictionary.try_push(name, length)?;
        }
        Ok(dictionary)
    }

    /// Appends a contig. A repeated name keeps its positional entry but never
    /// shadows the first contig of that name: name lookup stays stable and
    /// agrees before and after serialization. Prefer [`Self::try_push`].
    pub fn push(&mut self, name: impl Into<String>, length: u64) -> ContigId {
        let id = ContigId::new(u32::try_from(self.contigs.len()).expect("too many contigs"));
        let name = name.into();
        self.names.entry(name.clone()).or_insert(id);
        self.contigs.push(Contig::new(id, name, length));
        id
    }

    /// Appends a contig, rejecting a name the dictionary already contains.
    pub fn try_push(
        &mut self,
        name: impl Into<String>,
        length: u64,
    ) -> Result<ContigId, DuplicateContigError> {
        let name = name.into();
        if self.names.contains_key(&name) {
            return Err(DuplicateContigError { name });
        }
        Ok(self.push(name, length))
    }

    pub fn get(&self, id: ContigId) -> Option<&Contig> {
        usize::try_from(id.get())
            .ok()
            .and_then(|index| self.contigs.get(index))
    }

    pub fn id_by_name(&self, name: &str) -> Option<ContigId> {
        // Every construction path, including deserialization, maintains the index.
        self.names.get(name).copied()
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
///
/// Deserialization goes through [`GenomicInterval::new`], so an empty or
/// reversed interval is a decode error instead of a later `len()` underflow.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SerializedGenomicInterval")]
pub struct GenomicInterval {
    contig: ContigId,
    start: u64,
    end: u64,
}

#[derive(Deserialize)]
struct SerializedGenomicInterval {
    contig: ContigId,
    start: u64,
    end: u64,
}

impl TryFrom<SerializedGenomicInterval> for GenomicInterval {
    type Error = IntervalError;

    fn try_from(serialized: SerializedGenomicInterval) -> Result<Self, Self::Error> {
        Self::new(serialized.contig, serialized.start, serialized.end)
    }
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
    fn decoded_dictionary_rebuilds_index_and_positional_ids() {
        // A foreign writer disagreed about ids; position is authoritative.
        let json = r#"{"contigs":[{"id":7,"name":"chr1","length":100},{"id":0,"name":"chr2","length":50}]}"#;
        let decoded: ReferenceDictionary = serde_json::from_str(json).unwrap();
        let built =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("chr2".to_owned(), 50)]);
        assert_eq!(decoded, built);
        assert_eq!(decoded.id_by_name("chr2"), Some(ContigId::new(1)));
        assert_eq!(
            decoded.get(ContigId::new(1)).unwrap().id(),
            ContigId::new(1)
        );
        assert_eq!(decoded.id_by_name("missing"), None);
    }

    #[test]
    fn duplicate_contig_names_are_rejected_and_never_shadow() {
        let json = r#"{"contigs":[{"id":0,"name":"chr1","length":100},{"id":1,"name":"chr1","length":50}]}"#;
        assert!(serde_json::from_str::<ReferenceDictionary>(json).is_err());

        let mut dictionary = ReferenceDictionary::default();
        assert_eq!(dictionary.try_push("chr1", 100), Ok(ContigId::new(0)));
        assert_eq!(
            dictionary.try_push("chr1", 50),
            Err(DuplicateContigError {
                name: "chr1".to_owned()
            })
        );
        assert_eq!(dictionary.len(), 1);
        // The legacy infallible path keeps the first mapping.
        dictionary.push("chr1", 50);
        assert_eq!(dictionary.id_by_name("chr1"), Some(ContigId::new(0)));
        assert!(
            ReferenceDictionary::try_from_contigs([("a".to_owned(), 1), ("a".to_owned(), 2)])
                .is_err()
        );
    }

    #[test]
    fn decoded_intervals_are_validated() {
        let valid: GenomicInterval =
            serde_json::from_str(r#"{"contig":0,"start":5000000000,"end":5000000010}"#).unwrap();
        assert_eq!(valid.len(), 10);
        assert_eq!(
            serde_json::from_str::<GenomicInterval>(&serde_json::to_string(&valid).unwrap())
                .unwrap(),
            valid
        );
        assert!(
            serde_json::from_str::<GenomicInterval>(r#"{"contig":0,"start":20,"end":10}"#).is_err()
        );
        assert!(
            serde_json::from_str::<GenomicInterval>(r#"{"contig":0,"start":10,"end":10}"#).is_err()
        );
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
