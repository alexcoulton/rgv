use std::path::Path;

use crate::TrackKind;

/// A concrete genomic source format inferred from a source path or URL.
///
/// This is intentionally more specific than [`TrackKind`]: callers use it to
/// choose an I/O backend while `kind` describes the user-facing track family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GenomicSourceFormat {
    Bam,
    Cram,
    Fasta,
    Bed,
    Gff3,
    Gtf,
    GenePred,
    RefFlat,
    NarrowPeak,
    BroadPeak,
    Psl,
    Gwas,
    Vcf,
    Bcf,
    BigWig,
    Tdf,
    BedGraph,
    Wig,
    BedPe,
    Interact,
    Seg,
    MutationMaf,
    BigBed,
    BigGenePred,
    BigNarrowPeak,
}

impl GenomicSourceFormat {
    pub const fn kind(self) -> TrackKind {
        match self {
            Self::Bam | Self::Cram => TrackKind::Alignment,
            Self::Fasta => TrackKind::Reference,
            Self::Bed
            | Self::NarrowPeak
            | Self::BroadPeak
            | Self::Psl
            | Self::BigBed
            | Self::BigNarrowPeak => TrackKind::Feature,
            Self::Gff3 | Self::Gtf | Self::GenePred | Self::RefFlat | Self::BigGenePred => {
                TrackKind::GeneModel
            }
            Self::Gwas => TrackKind::Gwas,
            Self::Vcf | Self::Bcf => TrackKind::Variant,
            Self::BigWig | Self::Tdf | Self::BedGraph | Self::Wig => TrackKind::Quantitative,
            Self::BedPe | Self::Interact => TrackKind::Interaction,
            Self::Seg => TrackKind::CopyNumber,
            Self::MutationMaf => TrackKind::Mutation,
        }
    }

    /// Whether RGV supports this source as gzip-compressed text.
    pub const fn supports_gzip(self) -> bool {
        matches!(
            self,
            Self::Bed
                | Self::Gff3
                | Self::Gtf
                | Self::GenePred
                | Self::RefFlat
                | Self::NarrowPeak
                | Self::BroadPeak
                | Self::Psl
                | Self::Gwas
                | Self::Vcf
                | Self::MutationMaf
        )
    }

    /// Whether the current desktop I/O stack can open this format remotely. Text annotation
    /// formats are downloaded whole; the others are read by range request.
    pub const fn supports_remote(self) -> bool {
        matches!(
            self,
            Self::Bam
                | Self::Cram
                | Self::Fasta
                | Self::Bed
                | Self::Gff3
                | Self::Gtf
                | Self::GenePred
                | Self::RefFlat
                | Self::NarrowPeak
                | Self::BroadPeak
                | Self::Psl
                | Self::Vcf
                | Self::Bcf
                | Self::BigWig
                | Self::Tdf
                | Self::BigBed
                | Self::BigGenePred
                | Self::BigNarrowPeak
        )
    }

    /// Suffixes accepted for a companion index, in preference order.
    ///
    /// An empty slice means that the format is either self-indexing or is read
    /// sequentially by RGV.
    pub const fn companion_index_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::Bam => &[".bai", ".csi"],
            Self::Cram => &[".crai"],
            Self::Fasta => &[".fai"],
            Self::Vcf => &[".tbi", ".csi"],
            Self::Bcf => &[".csi"],
            _ => &[],
        }
    }

    pub const fn requires_companion_index(self) -> bool {
        !self.companion_index_suffixes().is_empty()
    }

    /// Stable human-readable name used when no more specific label is known.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Bam => "BAM alignment",
            Self::Cram => "CRAM alignment",
            Self::Fasta => "FASTA reference",
            Self::Bed => "BED features",
            Self::Gff3 => "GFF3 annotations",
            Self::Gtf => "GTF annotations",
            Self::GenePred => "genePred annotations",
            Self::RefFlat => "refFlat annotations",
            Self::NarrowPeak => "narrowPeak features",
            Self::BroadPeak => "broadPeak features",
            Self::Psl => "PSL alignments",
            Self::Gwas => "GWAS associations",
            Self::Vcf => "VCF variants",
            Self::Bcf => "BCF variants",
            Self::BigWig => "BigWig signal",
            Self::Tdf => "TDF signal",
            Self::BedGraph => "bedGraph signal",
            Self::Wig => "Wiggle signal",
            Self::BedPe => "BEDPE interactions",
            Self::Interact => "Interact interactions",
            Self::Seg => "SEG copy number",
            Self::MutationMaf => "mutation MAF",
            Self::BigBed => "BigBed features",
            Self::BigGenePred => "bigGenePred annotations",
            Self::BigNarrowPeak => "bigNarrowPeak features",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassifiedGenomicSource {
    pub format: GenomicSourceFormat,
    pub compressed: bool,
}

impl ClassifiedGenomicSource {
    pub const fn kind(self) -> TrackKind {
        self.format.kind()
    }

    pub const fn is_supported(self) -> bool {
        !self.compressed || self.format.supports_gzip()
    }
}

/// Classify a local path or URL without inspecting its contents.
///
/// Query strings and fragments are excluded before extension parsing so signed
/// remote URLs are classified using the object path. Unknown extensions return
/// `None` and can be handled according to the caller's context.
pub fn classify_genomic_source(value: &str) -> Option<ClassifiedGenomicSource> {
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let path = Path::new(path);
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let inner_extension = path
        .file_stem()
        .and_then(|value| Path::new(value).extension())
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    let compressed = extension == "gz";
    let effective = if compressed {
        inner_extension.as_deref().unwrap_or_default()
    } else {
        extension.as_str()
    };
    let format = match (effective, extension.as_str(), inner_extension.as_deref()) {
        ("bam", _, _) => GenomicSourceFormat::Bam,
        ("cram", _, _) => GenomicSourceFormat::Cram,
        ("fa" | "fasta" | "fna" | "2bit", _, _) => GenomicSourceFormat::Fasta,
        ("bed", _, _) => GenomicSourceFormat::Bed,
        ("gff" | "gff3", _, _) => GenomicSourceFormat::Gff3,
        ("gtf", _, _) => GenomicSourceFormat::Gtf,
        ("genepred" | "gp", _, _) => GenomicSourceFormat::GenePred,
        ("refflat", _, _) => GenomicSourceFormat::RefFlat,
        ("narrowpeak", _, _) => GenomicSourceFormat::NarrowPeak,
        ("broadpeak", _, _) => GenomicSourceFormat::BroadPeak,
        ("psl", _, _) => GenomicSourceFormat::Psl,
        ("linear" | "logistic" | "assoc" | "qassoc" | "gwas", _, _) => GenomicSourceFormat::Gwas,
        ("vcf", _, _) => GenomicSourceFormat::Vcf,
        ("bcf", _, _) => GenomicSourceFormat::Bcf,
        ("bw" | "bigwig", _, _) => GenomicSourceFormat::BigWig,
        ("tdf", _, _) => GenomicSourceFormat::Tdf,
        ("bedgraph" | "bdg", _, _) => GenomicSourceFormat::BedGraph,
        ("wig" | "wiggle", _, _) => GenomicSourceFormat::Wig,
        ("bedpe", _, _) => GenomicSourceFormat::BedPe,
        ("interact", _, _) => GenomicSourceFormat::Interact,
        ("seg" | "cbs", _, _) => GenomicSourceFormat::Seg,
        ("maf", _, _) | ("txt", "txt", Some("maf")) => GenomicSourceFormat::MutationMaf,
        ("bb" | "bigbed", _, _) => GenomicSourceFormat::BigBed,
        ("biggenepred", _, _) => GenomicSourceFormat::BigGenePred,
        ("bignarrowpeak", _, _) => GenomicSourceFormat::BigNarrowPeak,
        _ => return None,
    };
    Some(ClassifiedGenomicSource { format, compressed })
}

/// Derive a concise source label without exposing URL query parameters.
pub fn default_genomic_source_label(value: &str) -> Option<String> {
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let path = Path::new(path);
    let classified = classify_genomic_source(value)?;
    let mut stem = path.file_name()?.to_string_lossy().into_owned();
    if classified.compressed {
        stem = Path::new(&stem).file_stem()?.to_string_lossy().into_owned();
    }
    Path::new(&stem)
        .file_stem()
        .map(|label| label.to_string_lossy().into_owned())
        .filter(|label| !label.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_advertised_track_family() {
        for (source, format, kind) in [
            ("reads.bam", GenomicSourceFormat::Bam, TrackKind::Alignment),
            (
                "reference.fa",
                GenomicSourceFormat::Fasta,
                TrackKind::Reference,
            ),
            (
                "genes.gtf.gz",
                GenomicSourceFormat::Gtf,
                TrackKind::GeneModel,
            ),
            (
                "cohort.vcf.gz",
                GenomicSourceFormat::Vcf,
                TrackKind::Variant,
            ),
            (
                "signal.bigWig",
                GenomicSourceFormat::BigWig,
                TrackKind::Quantitative,
            ),
            (
                "association.logistic",
                GenomicSourceFormat::Gwas,
                TrackKind::Gwas,
            ),
            (
                "cohort.cbs",
                GenomicSourceFormat::Seg,
                TrackKind::CopyNumber,
            ),
            (
                "mutations.maf.txt",
                GenomicSourceFormat::MutationMaf,
                TrackKind::Mutation,
            ),
            (
                "loops.bedpe",
                GenomicSourceFormat::BedPe,
                TrackKind::Interaction,
            ),
            (
                "genes.bigGenePred",
                GenomicSourceFormat::BigGenePred,
                TrackKind::GeneModel,
            ),
        ] {
            let classified = classify_genomic_source(source).unwrap();
            assert_eq!(classified.format, format, "{source}");
            assert_eq!(classified.kind(), kind, "{source}");
        }
    }

    #[test]
    fn signed_urls_use_the_object_path() {
        let classified =
            classify_genomic_source("https://example.test/data/cohort.vcf.gz?token=secret#ignored")
                .unwrap();
        assert_eq!(classified.format, GenomicSourceFormat::Vcf);
        assert!(classified.compressed);
        assert!(classified.is_supported());
    }

    #[test]
    fn compressed_binary_formats_are_classified_but_not_supported() {
        let classified = classify_genomic_source("cohort.bcf.gz").unwrap();
        assert_eq!(classified.format, GenomicSourceFormat::Bcf);
        assert!(classified.compressed);
        assert!(!classified.is_supported());
    }

    #[test]
    fn unknown_sources_are_explicit() {
        assert_eq!(classify_genomic_source("notes.txt"), None);
        assert_eq!(classify_genomic_source("no-extension"), None);
    }

    #[test]
    fn declares_remote_and_index_policy() {
        assert!(GenomicSourceFormat::Bam.supports_remote());
        assert!(GenomicSourceFormat::Bam.requires_companion_index());
        assert_eq!(
            GenomicSourceFormat::Bam.companion_index_suffixes(),
            &[".bai", ".csi"]
        );
        assert!(GenomicSourceFormat::Bed.supports_remote());
        assert!(!GenomicSourceFormat::BedGraph.supports_remote());
        assert!(!GenomicSourceFormat::BigWig.requires_companion_index());
    }

    #[test]
    fn default_labels_hide_url_credentials_and_extensions() {
        assert_eq!(
            default_genomic_source_label("https://example.test/data/cohort.vcf.gz?token=secret"),
            Some("cohort".to_owned())
        );
        assert_eq!(default_genomic_source_label("notes.txt"), None);
    }
}
