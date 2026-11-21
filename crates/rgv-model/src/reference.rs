use rgv_core::GenomicInterval;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceSlice {
    pub interval: GenomicInterval,
    pub bases: Vec<u8>,
}

impl ReferenceSlice {
    pub fn base_at(&self, genomic_position: u64) -> Option<u8> {
        if !self.interval.contains(genomic_position) {
            return None;
        }
        let offset = usize::try_from(genomic_position - self.interval.start()).ok()?;
        self.bases.get(offset).copied()
    }
}
