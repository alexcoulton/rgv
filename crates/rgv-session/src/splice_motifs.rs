//! Reference splice motifs of the junctions the Sashimi lanes draw.
//!
//! A junction's strand comes from its canonical splice motif when no read tag names one. The
//! four bases per intron are read on the reference actor (never on the UI thread), once per
//! intron and FASTA authority: this cache remembers every answer and every outstanding request,
//! and forgets them all when another FASTA is opened.

use std::collections::{HashMap, HashSet};

use rgv_alignment_scene::ReferenceRevision;
use rgv_core::ContigId;
use rgv_engine::{
    EngineQueueError, MAX_SPLICE_MOTIF_QUERY_INTRONS, ReferenceEngine, SpliceMotifQuery,
};
use rgv_model::SpliceMotif;

/// Most introns awaiting an answer at once; further misses wait for a later frame.
pub const MAX_PENDING_SPLICE_MOTIFS: usize = 4 * MAX_SPLICE_MOTIF_QUERY_INTRONS;

/// Most motifs remembered for one FASTA authority before the cache starts over.
pub const MAX_CACHED_SPLICE_MOTIFS: usize = 262_144;

#[derive(Clone, Debug, Default)]
pub struct SpliceMotifCache {
    revision: Option<ReferenceRevision>,
    motifs: HashMap<(ContigId, u64, u64), SpliceMotif>,
    pending: HashSet<(ContigId, u64, u64)>,
    /// Advances whenever an answer arrives, so a presentation built from this cache knows it is
    /// stale.
    generation: u64,
}

impl SpliceMotifCache {
    /// Forgets everything learned from another FASTA authority.
    fn bind(&mut self, revision: Option<ReferenceRevision>) {
        if self.revision != revision {
            self.revision = revision;
            self.motifs.clear();
            self.pending.clear();
            self.generation = self.generation.wrapping_add(1);
        }
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The motif of an intron under `revision`, or `Unavailable` when not (yet) known.
    #[must_use]
    pub fn motif(
        &self,
        revision: Option<ReferenceRevision>,
        contig: ContigId,
        start: u64,
        end: u64,
    ) -> SpliceMotif {
        if revision.is_none() || self.revision != revision {
            return SpliceMotif::Unavailable;
        }
        self.motifs
            .get(&(contig, start, end))
            .copied()
            .unwrap_or_default()
    }

    /// Asks the reference actor for every intron of `introns` neither known nor already asked
    /// for. Returns how many were requested and how many are still neither known nor asked for
    /// (the queue was full or the pending bound reached), so the caller can ask again later.
    pub fn request_missing(
        &mut self,
        engine: Option<&ReferenceEngine>,
        revision: Option<ReferenceRevision>,
        contig: ContigId,
        contig_name: &str,
        introns: impl IntoIterator<Item = (u64, u64)>,
    ) -> (usize, usize) {
        self.bind(revision);
        let (Some(engine), Some(revision)) = (engine, revision) else {
            return (0, 0);
        };
        let mut missing = Vec::new();
        let mut deferred = 0_usize;
        for (start, end) in introns {
            let key = (contig, start, end);
            if self.motifs.contains_key(&key) || self.pending.contains(&key) {
                continue;
            }
            if self.pending.len().saturating_add(missing.len()) >= MAX_PENDING_SPLICE_MOTIFS {
                deferred = deferred.saturating_add(1);
            } else {
                missing.push((start, end));
            }
        }
        missing.sort_unstable();
        missing.dedup();
        let mut requested = 0_usize;
        let total = missing.len();
        for batch in missing.chunks(MAX_SPLICE_MOTIF_QUERY_INTRONS) {
            match engine.try_query_splice_motifs(SpliceMotifQuery {
                revision,
                contig,
                contig_name: contig_name.to_owned(),
                introns: batch.to_vec(),
            }) {
                Ok(()) => {
                    self.pending
                        .extend(batch.iter().map(|(start, end)| (contig, *start, *end)));
                    requested = requested.saturating_add(batch.len());
                }
                // A full queue is retried by the next frame that still misses these motifs.
                Err(EngineQueueError::Full | EngineQueueError::Disconnected) => break,
            }
        }
        (requested, deferred.saturating_add(total - requested))
    }

    /// Records an answer from the reference actor. Returns whether it was for the current
    /// authority (and so may change a presentation).
    pub fn accept(
        &mut self,
        current: Option<ReferenceRevision>,
        revision: ReferenceRevision,
        contig: ContigId,
        motifs: impl IntoIterator<Item = ((u64, u64), SpliceMotif)>,
    ) -> bool {
        self.bind(current);
        if current != Some(revision) {
            return false;
        }
        if self.motifs.len() >= MAX_CACHED_SPLICE_MOTIFS {
            self.motifs.clear();
        }
        for ((start, end), motif) in motifs {
            let key = (contig, start, end);
            self.pending.remove(&key);
            self.motifs.insert(key, motif);
        }
        self.generation = self.generation.wrapping_add(1);
        true
    }

    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.motifs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.motifs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_alignment_scene::RevisionFingerprint;

    fn revision(low: u64) -> ReferenceRevision {
        ReferenceRevision::new(RevisionFingerprint::new(1, low))
    }

    #[test]
    fn answers_are_kept_per_authority_and_forgotten_when_it_changes() {
        let mut cache = SpliceMotifCache::default();
        let contig = ContigId::new(2);
        let first = revision(1);
        assert!(cache.accept(
            Some(first),
            first,
            contig,
            [((100, 200), SpliceMotif::CtAc)]
        ));
        assert_eq!(
            cache.motif(Some(first), contig, 100, 200),
            SpliceMotif::CtAc
        );
        let generation = cache.generation();
        // A late answer for an older FASTA is ignored.
        assert!(!cache.accept(
            Some(first),
            revision(9),
            contig,
            [((300, 400), SpliceMotif::GtAg)]
        ));
        assert_eq!(
            cache.motif(Some(first), contig, 300, 400),
            SpliceMotif::Unavailable
        );
        // Another authority starts empty.
        assert_eq!(
            cache.motif(Some(revision(2)), contig, 100, 200),
            SpliceMotif::Unavailable
        );
        assert_eq!(
            cache.request_missing(None, Some(revision(2)), contig, "chr3", [(1, 9)]),
            (0, 0)
        );
        assert!(cache.is_empty());
        assert!(cache.generation() != generation);
    }
}
