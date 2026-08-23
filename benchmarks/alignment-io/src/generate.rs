use std::{
    cmp::Ordering,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rust_htslib::bam::{
    self, Format, Header, Record, Writer,
    header::HeaderRecord,
    record::{Aux, Cigar, CigarString},
};

const CHR1_LEN: u32 = 3_000_000;
const CHR2_LEN: u32 = 3_000_000;
const VARIANT_CONTIG: &str = "chrVariants";
const VARIANT_CONTIG_LEN: usize = 20_000;
const FASTA_LINE_BASES: usize = 80;
const LONG_MM_TAG: &str = "C+m,500,5;";
const LONG_CIGAR_TEXT: &str = "1000S4000M50I4000M100D2000M500S";
const SUPPLEMENTARY_CIGAR_TEXT: &str = "100S900M";
const SMRT_TRUTH_RECORDS: u32 = 8;
const SBX_INSERTION_READ_START: usize = 5_000;
const SBX_INSERTION_LENGTH: usize = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Short,
    Deep,
    Spliced,
    Long,
    Supplementary,
    PairSv,
    Control,
    LongCigar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Spec {
    tid: i32,
    pos: i64,
    id: u32,
    kind: Kind,
}

impl Ord for Spec {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.tid, self.pos, self.id).cmp(&(other.tid, other.pos, other.id))
    }
}

impl PartialOrd for Spec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn run(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", "chr1")
            .push_tag(b"LN", CHR1_LEN),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", "chr2")
            .push_tag(b"LN", CHR2_LEN),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "synthetic")
            .push_tag(b"SM", "rgv-benchmark"),
    );

    let mut specs = Vec::with_capacity(282_011);

    for id in 0..120_000_u32 {
        specs.push(Spec {
            tid: 0,
            pos: 1_000 + i64::from(id) * 16,
            id,
            kind: Kind::Short,
        });
    }

    for id in 0..150_000_u32 {
        specs.push(Spec {
            tid: 0,
            pos: 900_000 + i64::from(id % 20_000),
            id,
            kind: Kind::Deep,
        });
    }

    for id in 0..10_000_u32 {
        specs.push(Spec {
            tid: 0,
            pos: 2_100_000 + i64::from(id) * 20,
            id,
            kind: Kind::Spliced,
        });
    }

    for id in 0..1_500_u32 {
        specs.push(Spec {
            tid: 1,
            pos: 10_000 + i64::from(id) * 1_200,
            id,
            kind: Kind::Long,
        });
    }

    for id in 0..500_u32 {
        specs.push(Spec {
            tid: 1,
            pos: 500_000 + i64::from(id) * 2_000,
            id,
            kind: Kind::Supplementary,
        });
    }

    for (id, tid, pos) in [
        (0, 0, 2_600_000),
        (1, 0, 2_602_000),
        (2, 0, 2_610_000),
        (3, 0, 2_610_050),
        (4, 0, 2_620_000),
        (5, 0, 2_620_500),
        (6, 0, 2_630_000),
        (7, 0, 2_630_500),
        (8, 0, 2_640_000),
        (9, 1, 2_640_000),
        (10, 0, 2_645_000),
    ] {
        specs.push(Spec {
            tid,
            pos,
            id,
            kind: Kind::PairSv,
        });
    }

    for id in 0..5_u32 {
        specs.push(Spec {
            tid: 0,
            pos: 2_650_000 + i64::from(id) * 500,
            id,
            kind: Kind::Control,
        });
    }

    specs.push(Spec {
        tid: 1,
        pos: 2_500_000,
        id: 0,
        kind: Kind::LongCigar,
    });
    specs.sort_unstable();

    let mut writer = Writer::from_path(path, &header, Format::Bam)
        .with_context(|| format!("failed to create {}", path.display()))?;
    writer.set_threads(2)?;

    for spec in &specs {
        let record = make_record(*spec)?;
        writer.write(&record)?;
    }
    drop(writer);

    let index_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
    bam::index::build(path, Some(&index_path), bam::index::Type::Bai, 2)
        .with_context(|| format!("failed to index {}", path.display()))?;

    let bam_size = fs::metadata(path)?.len();
    let bai_size = fs::metadata(&index_path)?.len();
    println!(
        "generated {} records: {} bytes BAM, {} bytes BAI",
        specs.len(),
        bam_size,
        bai_size
    );
    Ok(())
}

pub fn run_variants(path: &Path, reference_path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = reference_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut reference = repeating_reference(VARIANT_CONTIG_LEN);
    apply_ultima_truth_reference(&mut reference);
    write_indexed_fasta(reference_path, VARIANT_CONTIG, &reference)?;

    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", VARIANT_CONTIG)
            .push_tag(b"LN", VARIANT_CONTIG_LEN),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "variant-truth")
            .push_tag(b"SM", "rgv-variant-truth")
            .push_tag(b"PL", "ULTIMA")
            .push_tag(b"mc", 3),
    );

    let specs = variant_specs();
    let mut writer = Writer::from_path(path, &header, Format::Bam)
        .with_context(|| format!("failed to create {}", path.display()))?;
    for spec in &specs {
        writer.write(&make_variant_record(spec, &reference)?)?;
    }
    let ultima_records = ultima_truth_records(&reference)?;
    for record in &ultima_records {
        writer.write(record)?;
    }
    drop(writer);

    let index_path = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index_path), bam::index::Type::Bai, 1)
        .with_context(|| format!("failed to index {}", path.display()))?;
    println!(
        "generated {} visual truth reads in {}\nreference: {}\nsmall events: {}:9941-10080\nlarge indels: {}:9401-12600",
        specs.len() + ultima_records.len(),
        path.display(),
        reference_path.display(),
        VARIANT_CONTIG,
        VARIANT_CONTIG
    );
    Ok(())
}

#[derive(Clone, Debug)]
struct VariantSpec {
    name: &'static str,
    position: usize,
    cigar: Vec<Cigar>,
}

fn variant_specs() -> Vec<VariantSpec> {
    vec![
        variant_spec(
            "long_insertion_1000bp",
            9_500,
            &[('=', 500), ('I', 1_000), ('=', 500)],
        ),
        variant_spec(
            "long_deletion_2000bp",
            9_500,
            &[('=', 500), ('D', 2_000), ('=', 500)],
        ),
        variant_spec("reference_control_a", 9_920, &[('=', 200)]),
        variant_spec("reference_control_b", 9_920, &[('=', 200)]),
        variant_spec("snv_single", 9_920, &[('=', 80), ('X', 1), ('=', 119)]),
        variant_spec(
            "snv_cluster_3",
            9_920,
            &[
                ('=', 45),
                ('X', 1),
                ('=', 9),
                ('X', 1),
                ('=', 9),
                ('X', 1),
                ('=', 134),
            ],
        ),
        variant_spec(
            "small_insertion_1bp",
            9_920,
            &[('=', 80), ('I', 1), ('=', 120)],
        ),
        variant_spec(
            "small_insertion_6bp",
            9_920,
            &[('=', 80), ('I', 6), ('=', 120)],
        ),
        variant_spec(
            "small_deletion_1bp",
            9_920,
            &[('=', 80), ('D', 1), ('=', 120)],
        ),
        variant_spec(
            "small_deletion_12bp",
            9_920,
            &[('=', 80), ('D', 12), ('=', 120)],
        ),
        variant_spec(
            "large_insertion_100bp",
            9_920,
            &[('=', 80), ('I', 100), ('=', 120)],
        ),
        variant_spec(
            "large_deletion_500bp",
            9_920,
            &[('=', 80), ('D', 500), ('=', 120)],
        ),
        variant_spec(
            "compound_snv_ins_del",
            9_920,
            &[
                ('=', 50),
                ('I', 3),
                ('=', 30),
                ('D', 10),
                ('=', 20),
                ('X', 1),
                ('=', 99),
            ],
        ),
        variant_spec("mnv_3bp", 10_200, &[('=', 40), ('X', 3), ('=', 57)]),
        variant_spec("ambiguous_bases", 10_220, &[('=', 100)]),
        variant_spec(
            "low_quality_snv_q5",
            10_240,
            &[('=', 49), ('X', 1), ('=', 50)],
        ),
        variant_spec(
            "reverse_strand_snv",
            10_260,
            &[('=', 49), ('X', 1), ('=', 50)],
        ),
        variant_spec(
            "small_insertion_2bp",
            10_300,
            &[('=', 50), ('I', 2), ('=', 50)],
        ),
        variant_spec(
            "small_insertion_10bp",
            10_320,
            &[('=', 50), ('I', 10), ('=', 50)],
        ),
        variant_spec(
            "small_deletion_2bp",
            10_340,
            &[('=', 50), ('D', 2), ('=', 50)],
        ),
        variant_spec(
            "small_deletion_10bp",
            10_360,
            &[('=', 50), ('D', 10), ('=', 50)],
        ),
        variant_spec(
            "large_deletion_100bp",
            10_380,
            &[('=', 50), ('D', 100), ('=', 50)],
        ),
        variant_spec(
            "large_deletion_1000bp",
            10_400,
            &[('=', 50), ('D', 1_000), ('=', 50)],
        ),
        variant_spec(
            "mixed_m_soft_hard_clip",
            10_450,
            &[('S', 5), ('M', 50), ('X', 1), ('M', 49), ('H', 5)],
        ),
        variant_spec(
            "skip_padding_adjacent_events",
            10_500,
            &[
                ('M', 40),
                ('N', 25),
                ('M', 20),
                ('P', 3),
                ('I', 2),
                ('M', 38),
            ],
        ),
    ]
}

fn variant_spec(name: &'static str, position: usize, operations: &[(char, u32)]) -> VariantSpec {
    let cigar = operations
        .iter()
        .map(|(kind, length)| match kind {
            '=' => Cigar::Equal(*length),
            'X' => Cigar::Diff(*length),
            'I' => Cigar::Ins(*length),
            'D' => Cigar::Del(*length),
            'M' => Cigar::Match(*length),
            'S' => Cigar::SoftClip(*length),
            'H' => Cigar::HardClip(*length),
            'N' => Cigar::RefSkip(*length),
            'P' => Cigar::Pad(*length),
            _ => unreachable!("the truth-set CIGAR alphabet is fixed"),
        })
        .collect();
    VariantSpec {
        name,
        position,
        cigar,
    }
}

fn make_variant_record(spec: &VariantSpec, reference: &[u8]) -> Result<Record> {
    let mut sequence = Vec::new();
    let mut reference_position = spec.position;
    let mut inserted_position = 0_usize;
    let mut edit_distance = 0_u32;
    for operation in &spec.cigar {
        match operation {
            Cigar::Match(length) | Cigar::Equal(length) => {
                let length = *length as usize;
                sequence
                    .extend_from_slice(&reference[reference_position..reference_position + length]);
                reference_position += length;
            }
            Cigar::Diff(length) => {
                edit_distance += *length;
                let length = *length as usize;
                sequence.extend(
                    reference[reference_position..reference_position + length]
                        .iter()
                        .copied()
                        .map(mutate_base),
                );
                reference_position += length;
            }
            Cigar::Ins(length) => {
                sequence.extend(
                    (0..*length as usize).map(|offset| b"TGCA"[(inserted_position + offset) % 4]),
                );
                inserted_position += *length as usize;
                edit_distance += *length;
            }
            Cigar::Del(length) => {
                reference_position += *length as usize;
                edit_distance += *length;
            }
            Cigar::SoftClip(length) => {
                sequence.extend(
                    (0..*length as usize).map(|offset| b"ACGT"[(inserted_position + offset) % 4]),
                );
                inserted_position += *length as usize;
            }
            Cigar::RefSkip(length) => reference_position += *length as usize,
            Cigar::HardClip(_) | Cigar::Pad(_) => {}
        }
    }
    ensure!(
        reference_position <= reference.len(),
        "variant record exceeds reference"
    );

    if spec.name == "ambiguous_bases" {
        sequence[48..52].fill(b'N');
    }
    let mut qualities = vec![40_u8; sequence.len()];
    if spec.name == "low_quality_snv_q5" {
        qualities[49] = 5;
    }
    let reverse = spec.name == "reverse_strand_snv";
    if reverse {
        sequence = sequence
            .iter()
            .rev()
            .map(|base| match base.to_ascii_uppercase() {
                b'A' => b'T',
                b'C' => b'G',
                b'G' => b'C',
                b'T' => b'A',
                other => other,
            })
            .collect();
        qualities.reverse();
    }
    let mut record = Record::new();
    record.set(
        spec.name.as_bytes(),
        Some(&CigarString(spec.cigar.clone())),
        &sequence,
        &qualities,
    );
    record.set_tid(0);
    record.set_pos(spec.position as i64);
    record.set_bin(bai_bin(spec.position as u64, reference_position as u64));
    record.set_flags(if reverse { 0x10 } else { 0 });
    record.set_mapq(60);
    record.push_aux(b"RG", Aux::String("variant-truth"))?;
    record.push_aux(b"EV", Aux::String(spec.name))?;
    record.push_aux(b"NM", Aux::I32(edit_distance as i32))?;
    let tp = vec![0_i8; sequence.len()];
    let t0 = "?".repeat(sequence.len());
    record.push_aux(b"tp", Aux::ArrayI8((&tp[..]).into()))?;
    record.push_aux(b"t0", Aux::String(&t0))?;
    Ok(record)
}

fn apply_ultima_truth_reference(reference: &mut [u8]) {
    reference[15_000..15_008].copy_from_slice(b"CGTAAAAA");
    reference[15_100..15_107].copy_from_slice(b"CGTACGT");
}

fn ultima_truth_records(reference: &[u8]) -> Result<Vec<Record>> {
    let insertion_sequence = [
        &reference[14_000..14_003],
        b"TT".as_slice(),
        &reference[14_003..14_006],
    ]
    .concat();
    let mut insertion_tp = vec![0_i8; insertion_sequence.len()];
    insertion_tp[4] = -2;

    let deletion_sequence = [&reference[15_000..15_003], &reference[15_005..15_008]].concat();
    let mut deletion_tp = vec![0_i8; deletion_sequence.len()];
    deletion_tp[4] = 2;

    let single_deletion_sequence =
        [&reference[15_100..15_103], &reference[15_104..15_107]].concat();
    let single_t0 = [20_u8, 20, 20, 35, 20, 20];

    Ok(vec![
        make_ultima_truth_record(
            "ultima_hmer_insertion_q40",
            14_000,
            vec![Cigar::Equal(3), Cigar::Ins(2), Cigar::Equal(3)],
            insertion_sequence,
            vec![30, 30, 30, 30, 40, 30, 30, 30],
            insertion_tp,
            &[30; 8],
        )?,
        make_ultima_truth_record(
            "ultima_hmer_deletion_mc_q0",
            15_000,
            vec![Cigar::Equal(3), Cigar::Del(2), Cigar::Equal(3)],
            deletion_sequence,
            vec![30, 30, 30, 30, 38, 30],
            deletion_tp,
            &[30; 6],
        )?,
        make_ultima_truth_record(
            "ultima_single_deletion_t0_q35",
            15_100,
            vec![Cigar::Equal(3), Cigar::Del(1), Cigar::Equal(3)],
            single_deletion_sequence,
            vec![30; 6],
            vec![0; 6],
            &single_t0,
        )?,
    ])
}

#[allow(clippy::too_many_arguments)]
fn make_ultima_truth_record(
    name: &str,
    position: usize,
    cigar: Vec<Cigar>,
    sequence: Vec<u8>,
    qualities: Vec<u8>,
    tp: Vec<i8>,
    t0_qualities: &[u8],
) -> Result<Record> {
    ensure!(sequence.len() == qualities.len());
    ensure!(sequence.len() == tp.len());
    ensure!(sequence.len() == t0_qualities.len());
    let reference_end = position as u64 + cigar_reference_len(&cigar);
    let mut record = Record::new();
    record.set(
        name.as_bytes(),
        Some(&CigarString(cigar)),
        &sequence,
        &qualities,
    );
    record.set_tid(0);
    record.set_pos(position as i64);
    record.set_bin(bai_bin(position as u64, reference_end));
    record.set_mapq(60);
    record.push_aux(b"RG", Aux::String("variant-truth"))?;
    record.push_aux(b"EV", Aux::String(name))?;
    record.push_aux(b"tp", Aux::ArrayI8((&tp[..]).into()))?;
    let t0 = t0_qualities
        .iter()
        .map(|quality| char::from(quality.saturating_add(33)))
        .collect::<String>();
    record.push_aux(b"t0", Aux::String(&t0))?;
    Ok(record)
}

fn mutate_base(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'C',
        b'C' => b'G',
        b'G' => b'T',
        _ => b'A',
    }
}

fn cigar_reference_len(cigar: &[Cigar]) -> u64 {
    cigar
        .iter()
        .map(|operation| match operation {
            Cigar::Match(length)
            | Cigar::Equal(length)
            | Cigar::Diff(length)
            | Cigar::Del(length)
            | Cigar::RefSkip(length) => u64::from(*length),
            Cigar::Ins(_) | Cigar::SoftClip(_) | Cigar::HardClip(_) | Cigar::Pad(_) => 0,
        })
        .sum()
}

/// Computes the classic BAI bin for a zero-based, half-open reference interval.
fn bai_bin(start: u64, end: u64) -> u16 {
    let inclusive_end = end.max(start.saturating_add(1)).saturating_sub(1);
    let bin = if start >> 14 == inclusive_end >> 14 {
        4_681 + (start >> 14)
    } else if start >> 17 == inclusive_end >> 17 {
        585 + (start >> 17)
    } else if start >> 20 == inclusive_end >> 20 {
        73 + (start >> 20)
    } else if start >> 23 == inclusive_end >> 23 {
        9 + (start >> 23)
    } else if start >> 26 == inclusive_end >> 26 {
        1 + (start >> 26)
    } else {
        0
    };
    u16::try_from(bin).expect("BAI bins fit in 16 bits")
}

fn repeating_reference(length: usize) -> Vec<u8> {
    (0..length).map(|index| b"ACGT"[index % 4]).collect()
}

fn write_indexed_fasta(path: &Path, contig: &str, sequence: &[u8]) -> Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let header = format!(">{contig}\n");
    writer.write_all(header.as_bytes())?;
    for line in sequence.chunks(FASTA_LINE_BASES) {
        writer.write_all(line)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    let index_path = PathBuf::from(format!("{}.fai", path.display()));
    fs::write(
        &index_path,
        format!(
            "{contig}\t{}\t{}\t{}\t{}\n",
            sequence.len(),
            header.len(),
            FASTA_LINE_BASES,
            FASTA_LINE_BASES + 1
        ),
    )
    .with_context(|| format!("failed to create {}", index_path.display()))?;
    Ok(())
}

fn make_record(spec: Spec) -> Result<Record> {
    let mut record = Record::new();
    let qname = if spec.kind == Kind::Supplementary {
        format!("Long-{:06}", spec.id)
    } else if spec.kind == Kind::Short {
        format!("Short-{:06}", spec.id / 2)
    } else if spec.kind == Kind::PairSv {
        format!("PairSv-{:06}", spec.id / 2)
    } else {
        format!("{:?}-{:06}", spec.kind, spec.id)
    };

    let (cigar, read_len, mut flags, mut mapq) = match spec.kind {
        Kind::Short => (
            vec![Cigar::Match(150)],
            150,
            if spec.id.is_multiple_of(2) {
                0x63
            } else {
                0x93
            },
            60,
        ),
        Kind::Deep => (
            vec![Cigar::Match(150)],
            150,
            if spec.id.is_multiple_of(3) { 16 } else { 0 },
            45,
        ),
        Kind::Spliced => (
            vec![Cigar::Match(75), Cigar::RefSkip(1_000), Cigar::Match(75)],
            150,
            if spec.id.is_multiple_of(2) { 0 } else { 16 },
            50,
        ),
        Kind::Long => (
            vec![
                Cigar::SoftClip(1_000),
                Cigar::Match(4_000),
                Cigar::Ins(50),
                Cigar::Match(4_000),
                Cigar::Del(100),
                Cigar::Match(2_000),
                Cigar::SoftClip(500),
            ],
            11_550,
            if spec.id.is_multiple_of(2) { 0 } else { 16 },
            40,
        ),
        Kind::Supplementary => (
            vec![Cigar::SoftClip(100), Cigar::Match(900)],
            1_000,
            0x800,
            35,
        ),
        Kind::PairSv => (vec![Cigar::Match(150)], 150, pair_sv_fields(spec).0, 55),
        Kind::Control => (vec![Cigar::Match(150)], 150, 0, 45),
        Kind::LongCigar => {
            let mut cigar = Vec::with_capacity(70_000);
            for i in 0..70_000 {
                cigar.push(if i % 2 == 0 {
                    Cigar::Match(1)
                } else {
                    Cigar::Ins(1)
                });
            }
            (cigar, 70_000, 0, 20)
        }
    };

    let sequence = deterministic_sequence(read_len, spec.id ^ ((spec.tid as u32) << 24));
    let mut qualities = synthetic_qualities(spec, read_len);
    if matches!(spec.kind, Kind::Deep | Kind::Control) {
        match spec.id {
            0 => flags |= 0x400,
            1 => flags |= 0x200,
            2 => mapq = 0,
            3 => qualities.fill(u8::MAX),
            4 => flags |= 0x100,
            _ => {}
        }
    }
    let reference_end = u64::try_from(spec.pos)? + cigar_reference_len(&cigar);
    record.set(
        qname.as_bytes(),
        Some(&CigarString(cigar)),
        &sequence,
        &qualities,
    );
    record.set_tid(spec.tid);
    record.set_pos(spec.pos);
    record.set_bin(bai_bin(u64::try_from(spec.pos)?, reference_end));
    record.set_flags(flags);
    record.set_mapq(mapq);
    if spec.kind == Kind::Short {
        let first = spec.id.is_multiple_of(2);
        record.set_mtid(spec.tid);
        record.set_mpos(if first {
            spec.pos.saturating_add(16)
        } else {
            spec.pos.saturating_sub(16)
        });
        record.set_insert_size(if first { 166 } else { -166 });
    } else if spec.kind == Kind::PairSv {
        let (_, mate_tid, mate_position, template_length) = pair_sv_fields(spec);
        record.set_mtid(mate_tid);
        record.set_mpos(mate_position);
        record.set_insert_size(template_length);
    }
    record.push_aux(b"RG", Aux::String("synthetic"))?;
    record.push_aux(b"NM", Aux::I32((spec.id % 17) as i32))?;

    match spec.kind {
        Kind::Spliced => match spec.id % 4 {
            0 => record.push_aux(b"XS", Aux::Char(b'+'))?,
            1 => record.push_aux(b"XS", Aux::Char(b'-'))?,
            2 => {}
            _ => record.push_aux(b"XS", Aux::String("?"))?,
        },
        Kind::Long => {
            match spec.id {
                1_490 => {}
                1_491 => {
                    record.push_aux(b"MM", Aux::String("not-valid"))?;
                    record.push_aux(b"ML", Aux::ArrayU8((&[255_u8][..]).into()))?;
                }
                1_492 => {
                    record.push_aux(b"MM", Aux::String(LONG_MM_TAG))?;
                    record.push_aux(b"ML", Aux::ArrayU8((&[255_u8][..]).into()))?;
                }
                1_493 => {
                    record.push_aux(b"MM", Aux::String("C+m,500;C-h,510;"))?;
                    record.push_aux(b"ML", Aux::ArrayU8((&[255_u8, 64][..]).into()))?;
                }
                1_494 => {
                    record.push_aux(b"MM", Aux::I32(42))?;
                }
                _ => {
                    record.push_aux(b"MM", Aux::String(LONG_MM_TAG))?;
                    record.push_aux(b"ML", Aux::ArrayU8((&[255_u8, 180][..]).into()))?;
                }
            }
            record.push_aux(b"HP", Aux::I32((spec.id % 2 + 1) as i32))?;
            if spec.id < SMRT_TRUTH_RECORDS {
                add_smrt_truth_tags(&mut record, read_len, spec.id)?;
            }
            if spec.id < 500 {
                let supplementary_position = 500_000_u64 + u64::from(spec.id) * 2_000;
                let sa = format!(
                    "chr2,{},+,{SUPPLEMENTARY_CIGAR_TEXT},35,{};",
                    supplementary_position + 1,
                    spec.id % 17
                );
                record.push_aux(b"SA", Aux::String(&sa))?;
            }
        }
        Kind::Supplementary => {
            let primary_position = 10_000_u64 + u64::from(spec.id) * 1_200;
            let primary_strand = if spec.id.is_multiple_of(2) { '+' } else { '-' };
            let sa = format!(
                "chr2,{},{primary_strand},{LONG_CIGAR_TEXT},40,{};",
                primary_position + 1,
                spec.id % 17
            );
            record.push_aux(b"SA", Aux::String(&sa))?;
        }
        _ => {}
    }

    ensure!(
        record.seq_len() == read_len,
        "record sequence length mismatch"
    );
    Ok(record)
}

fn add_smrt_truth_tags(record: &mut Record, read_len: usize, seed: u32) -> Result<()> {
    let values = (0..read_len)
        .map(|index| {
            const LEVELS: [u16; 8] = [0, 50, 100, 350, 600, 1_500, 6_000, 7_500];
            LEVELS[(index + seed as usize) % LEVELS.len()]
        })
        .collect::<Vec<_>>();
    for tag in [b"ip", b"pw", b"fi", b"ri", b"fp", b"rp"] {
        record.push_aux(tag, Aux::ArrayU16((&values[..]).into()))?;
    }
    Ok(())
}

fn synthetic_qualities(spec: Spec, read_len: usize) -> Vec<u8> {
    if spec.kind != Kind::Long {
        return vec![30_u8; read_len];
    }
    let mut qualities = vec![39_u8; read_len];
    qualities[..25.min(read_len)].fill(20);
    qualities[read_len.saturating_sub(30)..].fill(20);
    let insertion_end = SBX_INSERTION_READ_START
        .saturating_add(SBX_INSERTION_LENGTH)
        .min(read_len);
    match spec.id {
        0 => qualities[SBX_INSERTION_READ_START.min(read_len)..insertion_end].fill(5),
        2 if SBX_INSERTION_READ_START < read_len => {
            qualities[SBX_INSERTION_READ_START] = 5;
        }
        _ => {}
    }
    qualities
}

fn pair_sv_fields(spec: Spec) -> (u16, i32, i64, i64) {
    match spec.id {
        0 => (0x61, 0, 2_602_000, 2_150),
        1 => (0x91, 0, 2_600_000, -2_150),
        2 => (0x61, 0, 2_610_050, 50),
        3 => (0x91, 0, 2_610_000, -50),
        4 => (0x41, 0, 2_620_500, 650),
        5 => (0x81, 0, 2_620_000, -650),
        6 => (0x51, 0, 2_630_500, 650),
        7 => (0xa1, 0, 2_630_000, -650),
        8 => (0x41, 1, 2_640_000, 0),
        9 => (0x81, 0, 2_640_000, 0),
        10 => (0x49, -1, -1, 0),
        _ => unreachable!("pair-SV fixture id is bounded"),
    }
}

fn deterministic_sequence(len: usize, seed: u32) -> Vec<u8> {
    let mut state = u64::from(seed) + 0x9e37_79b9_7f4a_7c15;
    let mut sequence = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        sequence.push(b"ACGT"[(state & 3) as usize]);
    }
    sequence
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rgv_core::{ContigId, GenomicInterval, UltimaQualityMode};
    use rgv_hts::{
        AlignmentFlags, AlignmentOpenOptions, AlignmentRecord, AlignmentSource, AuxField, AuxValue,
        CigarOp, CigarOpKind, FastaReference, LocalAlignmentSource, QueryDetail, QueryOptions,
        SbxInsertionState, SequenceData, UltimaQualitySource, analyze_ultima_record,
        classify_sbx_insertion, decode_modified_bases, sbx_tail_bounds,
    };
    use rust_htslib::bam::Read as _;

    use super::*;

    #[test]
    fn long_read_modification_calls_land_in_aligned_sequence() {
        for id in 0..64 {
            let sequence = deterministic_sequence(11_550, id ^ (1_u32 << 24));
            let record = AlignmentRecord {
                contig: ContigId::new(1),
                start: 10_000,
                end: 20_100,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(40),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("Long-{id}").into_bytes(),
                cigar: vec![
                    CigarOp {
                        kind: CigarOpKind::SoftClip,
                        len: 1_000,
                    },
                    CigarOp {
                        kind: CigarOpKind::Match,
                        len: 4_000,
                    },
                    CigarOp {
                        kind: CigarOpKind::Insertion,
                        len: 50,
                    },
                    CigarOp {
                        kind: CigarOpKind::Match,
                        len: 4_000,
                    },
                    CigarOp {
                        kind: CigarOpKind::Deletion,
                        len: 100,
                    },
                    CigarOp {
                        kind: CigarOpKind::Match,
                        len: 2_000,
                    },
                    CigarOp {
                        kind: CigarOpKind::SoftClip,
                        len: 500,
                    },
                ],
                sequence: SequenceData::Present {
                    bases: sequence,
                    qualities: None,
                },
                auxiliary_fields: vec![
                    AuxField {
                        tag: *b"MM",
                        value: AuxValue::String(LONG_MM_TAG.to_owned()),
                    },
                    AuxField {
                        tag: *b"ML",
                        value: AuxValue::UnsignedArray(vec![255, 180]),
                    },
                ],
            };
            let calls = decode_modified_bases(&record).unwrap();
            assert_eq!(calls.len(), 2);
            assert!(
                calls
                    .iter()
                    .all(|call| (1_000..5_000).contains(&call.read_position))
            );
        }
    }

    #[test]
    fn long_read_fixture_contains_bounded_smrt_truth_arrays() {
        let record = make_record(Spec {
            tid: 1,
            pos: 10_000,
            id: 0,
            kind: Kind::Long,
        })
        .unwrap();
        for tag in [b"ip", b"pw", b"fi", b"ri", b"fp", b"rp"] {
            let Aux::ArrayU16(values) = record.aux(tag).unwrap() else {
                panic!(
                    "{}{} was not a u16 kinetic array",
                    tag[0] as char, tag[1] as char
                );
            };
            assert_eq!(values.len(), record.seq_len());
            assert_eq!(values.iter().next(), Some(0));
            assert!(values.iter().any(|value| value == 7_500));
        }
        let without_kinetics = make_record(Spec {
            tid: 1,
            pos: 20_000,
            id: SMRT_TRUTH_RECORDS,
            kind: Kind::Long,
        })
        .unwrap();
        assert!(without_kinetics.aux(b"ip").is_err());
    }

    #[test]
    fn long_read_fixture_contains_exact_sbx_quality_truth() {
        for (id, expected) in [
            (0, SbxInsertionState::FullyDiscordant),
            (2, SbxInsertionState::PartlyDiscordant),
            (4, SbxInsertionState::Concordant),
        ] {
            let record = make_record(Spec {
                tid: 1,
                pos: 10_000 + i64::from(id) * 1_200,
                id,
                kind: Kind::Long,
            })
            .unwrap();
            let bases = record.seq().as_bytes();
            let flags = AlignmentFlags::from_bits(record.flags());
            let tails = sbx_tail_bounds(flags, &bases, record.qual(), 30).unwrap();
            assert_eq!((tails.left_tail_bases, tails.right_tail_bases), (25, 30));
            assert_eq!(
                classify_sbx_insertion(
                    flags,
                    &bases,
                    record.qual(),
                    SBX_INSERTION_READ_START,
                    SBX_INSERTION_LENGTH,
                    10,
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn short_read_fixture_records_form_bidirectional_proper_pairs() {
        let left = make_record(Spec {
            tid: 0,
            pos: 1_000,
            id: 42,
            kind: Kind::Short,
        })
        .unwrap();
        let right = make_record(Spec {
            tid: 0,
            pos: 1_016,
            id: 43,
            kind: Kind::Short,
        })
        .unwrap();
        assert_eq!(left.qname(), right.qname());
        assert_eq!(left.mpos(), right.pos());
        assert_eq!(right.mpos(), left.pos());
        assert_eq!(left.insert_size(), 166);
        assert_eq!(right.insert_size(), -166);
        assert_eq!(left.flags(), 0x63);
        assert_eq!(right.flags(), 0x93);
    }

    #[test]
    fn primary_and_supplementary_fixture_records_form_a_bidirectional_chain() {
        let primary = make_record(Spec {
            tid: 1,
            pos: 10_000 + 42 * 1_200,
            id: 42,
            kind: Kind::Long,
        })
        .unwrap();
        let supplementary = make_record(Spec {
            tid: 1,
            pos: 500_000 + 42 * 2_000,
            id: 42,
            kind: Kind::Supplementary,
        })
        .unwrap();
        assert_eq!(primary.qname(), supplementary.qname());
        let Aux::String(primary_sa) = primary.aux(b"SA").unwrap() else {
            panic!("primary record should have an SA string");
        };
        let Aux::String(supplementary_sa) = supplementary.aux(b"SA").unwrap() else {
            panic!("supplementary record should have an SA string");
        };
        assert_eq!(primary_sa, "chr2,584001,+,100S900M,35,8;");
        assert_eq!(
            supplementary_sa,
            "chr2,60401,+,1000S4000M50I4000M100D2000M500S,40,8;"
        );
    }

    #[test]
    fn variant_truth_catalog_covers_small_and_large_events() {
        let specs = variant_specs();
        let maximum_insertion = specs
            .iter()
            .flat_map(|spec| &spec.cigar)
            .filter_map(|operation| match operation {
                Cigar::Ins(length) => Some(*length),
                _ => None,
            })
            .max();
        let maximum_deletion = specs
            .iter()
            .flat_map(|spec| &spec.cigar)
            .filter_map(|operation| match operation {
                Cigar::Del(length) => Some(*length),
                _ => None,
            })
            .max();
        assert_eq!(maximum_insertion, Some(1_000));
        assert_eq!(maximum_deletion, Some(2_000));
        assert!(specs.iter().any(|spec| spec.name == "small_insertion_1bp"));
        assert!(specs.iter().any(|spec| spec.name == "small_deletion_1bp"));
        assert!(specs.iter().any(|spec| {
            spec.cigar
                .iter()
                .any(|operation| matches!(operation, Cigar::Diff(_)))
        }));
    }

    #[test]
    fn variant_truth_sequences_obey_their_cigars() {
        let reference = repeating_reference(VARIANT_CONTIG_LEN);
        for spec in variant_specs() {
            let record = make_variant_record(&spec, &reference).unwrap();
            let expected_query_bases: usize = spec
                .cigar
                .iter()
                .map(|operation| match operation {
                    Cigar::Match(length)
                    | Cigar::Equal(length)
                    | Cigar::Diff(length)
                    | Cigar::Ins(length)
                    | Cigar::SoftClip(length) => *length as usize,
                    _ => 0,
                })
                .sum();
            assert_eq!(record.seq_len(), expected_query_bases, "{}", spec.name);
            assert_eq!(record.qname(), spec.name.as_bytes());
        }
    }

    #[test]
    fn ultima_truth_catalog_contains_direction_t0_and_read_group_metadata() {
        let mut reference = repeating_reference(VARIANT_CONTIG_LEN);
        apply_ultima_truth_reference(&mut reference);
        let records = ultima_truth_records(&reference).unwrap();
        assert_eq!(records.len(), 3);

        let Aux::ArrayI8(insertion_tp) = records[0].aux(b"tp").unwrap() else {
            panic!("insertion tp should be a signed-byte array");
        };
        assert_eq!(
            insertion_tp.iter().collect::<Vec<_>>(),
            vec![0, 0, 0, 0, -2, 0, 0, 0]
        );

        let Aux::String(single_t0) = records[2].aux(b"t0").unwrap() else {
            panic!("single deletion t0 should be a string");
        };
        assert_eq!(single_t0.as_bytes()[3] - 33, 35);
        assert!(
            records
                .iter()
                .all(|record| matches!(record.aux(b"RG"), Ok(Aux::String("variant-truth"))))
        );
    }

    #[test]
    fn generated_ultima_truth_survives_the_indexed_production_query() {
        let directory = tempfile::tempdir().unwrap();
        let bam_path = directory.path().join("variants.bam");
        let reference_path = directory.path().join("variants.fa");
        run_variants(&bam_path, &reference_path).unwrap();
        let mut source = LocalAlignmentSource::open(
            &bam_path,
            &AlignmentOpenOptions {
                reference_path: Some(reference_path.clone()),
                ..AlignmentOpenOptions::default()
            },
        )
        .unwrap();
        let interval = GenomicInterval::new(ContigId::new(0), 13_990, 15_120).unwrap();
        let mut query = source
            .query(
                interval,
                QueryOptions {
                    detail: QueryDetail::Full,
                    max_records: NonZeroUsize::new(10).unwrap(),
                    include_reference: true,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        query.reference = Some(
            FastaReference::open(reference_path)
                .unwrap()
                .fetch_slice(interval)
                .unwrap(),
        );
        assert_eq!(query.batch.len(), 3);
        let analyses = (0..query.batch.len())
            .map(|index| {
                let record = query.materialize_record(index).unwrap();
                analyze_ultima_record(
                    &record,
                    query.reference.as_ref(),
                    UltimaQualityMode::Directional,
                    query
                        .read_group(index)
                        .and_then(|group| group.maximum_homopolymer_length),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            analyses
                .iter()
                .map(|analysis| analysis.summary.quality_events)
                .sum::<usize>(),
            3,
            "{analyses:#?}"
        );
        assert_eq!(
            analyses[0].events[0].quality_source,
            Some(UltimaQualitySource::TpDirection)
        );
        assert_eq!(
            analyses[1].events[0].quality_source,
            Some(UltimaQualitySource::MaximumHomopolymerLimit)
        );
        assert_eq!(
            analyses[2].events[0].quality_source,
            Some(UltimaQualitySource::T0)
        );
    }

    #[test]
    fn variant_index_finds_reads_when_query_starts_inside_them() {
        let directory = tempfile::tempdir().unwrap();
        let bam_path = directory.path().join("variants.bam");
        let reference_path = directory.path().join("variants.fa");
        run_variants(&bam_path, &reference_path).unwrap();

        let mut sequential = bam::Reader::from_path(&bam_path).unwrap();
        let metadata = sequential
            .records()
            .map(|record| {
                let record = record.unwrap();
                (
                    String::from_utf8_lossy(record.qname()).into_owned(),
                    record.pos(),
                    record.cigar().end_pos(),
                    record.bin(),
                    record.flags(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(metadata.len(), 28, "{metadata:?}");

        let mut reader = bam::IndexedReader::from_path(&bam_path).unwrap();
        reader.fetch((VARIANT_CONTIG, 10_008, 10_017)).unwrap();
        assert_eq!(reader.records().count(), 13, "{metadata:?}");
    }

    #[test]
    fn bai_bin_accounts_for_the_complete_reference_span() {
        assert_eq!(bai_bin(9_920, 10_120), 4_681);
        assert_eq!(bai_bin(16_000, 17_000), 585);
    }
}
