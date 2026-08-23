use std::{hash::Hash, num::NonZeroUsize};

use lru::LruCache;
use rgv_core::{DetailLevel, GenomicInterval, PanelTrackId};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceFingerprint(u64);

impl SourceFingerprint {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AlignmentTileKey {
    pub source: SourceFingerprint,
    pub interval: GenomicInterval,
    pub detail: DetailLevel,
    pub filter_hash: u64,
    pub coverage_bins: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CoverageTileKey {
    pub source: SourceFingerprint,
    pub interval: GenomicInterval,
    pub filter_hash: u64,
    pub bins: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LayoutTileKey {
    pub owner: PanelTrackId,
    pub source: SourceFingerprint,
    pub interval: GenomicInterval,
    pub detail: DetailLevel,
    pub filter_hash: u64,
    pub layout_hash: u64,
    pub style_version: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuTileKey {
    pub layout: LayoutTileKey,
    pub device_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheBudgets {
    pub remote_blocks: usize,
    pub decoded_alignments: usize,
    pub coverage_summaries: usize,
    pub layouts: usize,
    pub gpu_buffers: usize,
}

impl CacheBudgets {
    pub const fn total(self) -> usize {
        self.remote_blocks
            + self.decoded_alignments
            + self.coverage_summaries
            + self.layouts
            + self.gpu_buffers
    }

    /// Scales the default cache hierarchy to an exact total byte budget while
    /// retaining the same relative allocation between tiers.
    pub fn from_total_bytes(total: NonZeroUsize) -> Self {
        let total = total.get();
        let remote_blocks = (total / 8).max(1);
        let decoded_alignments = (total * 5 / 8).max(1);
        let coverage_summaries = (total / 16).max(1);
        let layouts = (total / 8).max(1);
        let allocated = remote_blocks + decoded_alignments + coverage_summaries + layouts;
        let gpu_buffers = total.saturating_sub(allocated).max(1);
        Self {
            remote_blocks,
            decoded_alignments,
            coverage_summaries,
            layouts,
            gpu_buffers,
        }
    }
}

impl Default for CacheBudgets {
    fn default() -> Self {
        const MIB: usize = 1024 * 1024;
        Self {
            remote_blocks: 64 * MIB,
            decoded_alignments: 320 * MIB,
            coverage_summaries: 32 * MIB,
            layouts: 64 * MIB,
            gpu_buffers: 32 * MIB,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub resident_weight: usize,
    pub maximum_weight: usize,
}

struct CacheEntry<V> {
    value: V,
    weight: usize,
}

/// An LRU with an explicit byte-like weight budget rather than an entry-count limit.
pub struct WeightedLruCache<K, V> {
    entries: LruCache<K, CacheEntry<V>>,
    resident_weight: usize,
    maximum_weight: NonZeroUsize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl<K, V> WeightedLruCache<K, V>
where
    K: Eq + Hash,
{
    pub fn new(maximum_weight: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            resident_weight: 0,
            maximum_weight,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub fn insert(
        &mut self,
        key: K,
        value: V,
        weight: usize,
    ) -> Result<Option<V>, CacheInsertError<V>> {
        if weight > self.maximum_weight.get() {
            return Err(CacheInsertError::TooLarge {
                value,
                weight,
                maximum_weight: self.maximum_weight.get(),
            });
        }
        let replaced = self.entries.put(key, CacheEntry { value, weight });
        if let Some(previous) = &replaced {
            self.resident_weight = self.resident_weight.saturating_sub(previous.weight);
        }
        self.resident_weight = self.resident_weight.saturating_add(weight);
        while self.resident_weight > self.maximum_weight.get() {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.resident_weight = self.resident_weight.saturating_sub(evicted.weight);
            self.evictions = self.evictions.saturating_add(1);
        }
        Ok(replaced.map(|entry| entry.value))
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.entries.contains(key) {
            self.hits = self.hits.saturating_add(1);
            self.entries.get(key).map(|entry| &entry.value)
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let removed = self.entries.pop(key)?;
        self.resident_weight = self.resident_weight.saturating_sub(removed.weight);
        Some(removed.value)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.resident_weight = 0;
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            resident_weight: self.resident_weight,
            maximum_weight: self.maximum_weight.get(),
        }
    }
}

#[derive(Debug, Error)]
pub enum CacheInsertError<V> {
    #[error("cache item weight {weight} exceeds cache budget {maximum_weight}")]
    TooLarge {
        value: V,
        weight: usize,
        maximum_weight: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_eviction_respects_recent_access() {
        let mut cache = WeightedLruCache::new(NonZeroUsize::new(10).unwrap());
        cache.insert("a", 1, 4).unwrap();
        cache.insert("b", 2, 4).unwrap();
        assert_eq!(cache.get(&"a"), Some(&1));
        cache.insert("c", 3, 4).unwrap();
        assert_eq!(cache.get(&"b"), None);
        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.get(&"c"), Some(&3));
        assert_eq!(cache.stats().resident_weight, 8);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn oversized_items_are_rejected_without_eviction() {
        let mut cache = WeightedLruCache::new(NonZeroUsize::new(4).unwrap());
        cache.insert("kept", 1, 4).unwrap();
        assert!(cache.insert("too-large", 2, 5).is_err());
        assert_eq!(cache.get(&"kept"), Some(&1));
    }

    #[test]
    fn default_hierarchy_has_a_bounded_512_mib_budget() {
        assert_eq!(CacheBudgets::default().total(), 512 * 1024 * 1024);
    }

    #[test]
    fn hierarchy_scales_to_an_exact_budget() {
        let budget = CacheBudgets::from_total_bytes(NonZeroUsize::new(256 * 1024 * 1024).unwrap());
        assert_eq!(budget.total(), 256 * 1024 * 1024);
        assert_eq!(budget.decoded_alignments, 160 * 1024 * 1024);
    }
}
