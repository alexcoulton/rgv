#!/usr/bin/env python3
"""Download and curate the small gene-context assets used by IGV genome presets."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import shutil
import struct
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass
from itertools import chain
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Iterator, Optional, Sequence

try:
    import pyBigWig  # type: ignore[import-not-found]
except ImportError:  # The error is made actionable only when BigBed decoding is needed.
    pyBigWig = None


SCHEMA_VERSION = 1
SEARCH_SCHEMA_VERSION = "rgv_gene_names_v1"
IGV_DATA_ROOT = "https://raw.githubusercontent.com/igvteam/igv-data/main/genomes/json"
DEFAULT_OUTPUT = (
    Path(__file__).resolve().parents[2]
    / "benchmark-data"
    / "large-public-alignments"
    / "landmarks"
    / "igv"
)
BIGBED_MAGIC = 0x8789F2EB
REQUIRED_RUNTIME_ROLES = {
    "representative_genes",
    "detailed_transcripts",
    "gene_search",
    "cytobands",
    "contig_aliases",
    "chromosome_sizes",
    "notice",
}


@dataclass(frozen=True)
class BuildSpec:
    identifier: str
    landmark_mode: str
    landmark_url: Optional[str] = None
    bundle_version: str = "1.0-rgv1"
    assembly_stable_id: str = ""
    assembly_display_name: str = ""
    compatible_ids: tuple[str, ...] = ()
    provider: str = "NCBI RefSeq"
    release: str = "IGV curated snapshot"
    compatible_dictionary_sha256: tuple[str, ...] = ()


BUILD_SPECS = {
    "hg38": BuildSpec(
        "hg38",
        "igv_mane",
        assembly_stable_id="GRCh38.no_alt_analysis_set",
        assembly_display_name="GRCh38",
        compatible_ids=("GRCh38", "GRCh38.p14", "hg38"),
        provider="MANE (NCBI RefSeq and Ensembl/GENCODE)",
        release="MANE 1.0",
        # GENCODE v29 UCSC-named GRCh38 plus the ERCC and phiX sequences used
        # by the qualified ENCODE RNA-seq fixture. This is exact dictionary
        # evidence, not a primary-length heuristic.
        compatible_dictionary_sha256=(
            "36bda445209ba362bb534c9acf5f453f1adeacd3707586a346257f8f69a53fae",
        ),
    ),
    "hg19": BuildSpec(
        "hg19",
        "igv_mane",
        assembly_stable_id="GRCh37.p13",
        assembly_display_name="GRCh37 / hg19",
        compatible_ids=("GRCh37", "hg19"),
        release="RefSeq Select snapshot used by IGV hg19",
        # 1000 Genomes b37 plus hs37d5, with Ensembl-style primary names and
        # the 16,569-base mitochondrial sequence. Its nuclear coordinates are
        # exactly GRCh37; source-contig aliases are bound separately at runtime.
        compatible_dictionary_sha256=(
            "d74373ee19d2e0f8378f17ea869b31f242189d381b2e04be48f6fbd76fb7028b",
        ),
    ),
    "mm10": BuildSpec(
        "mm10",
        "representative_refgene",
        assembly_stable_id="GRCm38.p6",
        assembly_display_name="GRCm38 / mm10",
        compatible_ids=("GRCm38", "mm10"),
    ),
    "mm39": BuildSpec(
        "mm39",
        "representative_refgene",
        assembly_stable_id="GRCm39",
        assembly_display_name="GRCm39 / mm39",
        compatible_ids=("GRCm39", "mm39"),
    ),
    "hs1": BuildSpec(
        "hs1",
        "curated_biggenepred",
        "https://hgdownload.soe.ucsc.edu/gbdb/hs1/ncbiRefSeq/ncbiRefSeqCurated.bb",
        assembly_stable_id="T2T-CHM13v2.0",
        assembly_display_name="T2T CHM13 v2.0 / hs1",
        compatible_ids=("CHM13v2.0", "hs1"),
        release="NCBI RefSeq Curated snapshot used by UCSC hs1",
    ),
}


PRIMARY_CONTIGS = {
    "hg38": {*(f"chr{value}" for value in range(1, 23)), "chrX", "chrY", "chrM"},
    "hg19": {*(f"chr{value}" for value in range(1, 23)), "chrX", "chrY", "chrM"},
    "mm10": {*(f"chr{value}" for value in range(1, 20)), "chrX", "chrY", "chrM"},
    "mm39": {*(f"chr{value}" for value in range(1, 20)), "chrX", "chrY", "chrM"},
    "hs1": {*(f"chr{value}" for value in range(1, 23)), "chrX", "chrY", "chrM"},
}


SEQUENCE_MD5 = {
    "hg38": {
        "chr6": "5691468a67c7e7a7b5f2a3a683792c29",
        "chr20": "b18e6c531b0bd70e949a7fc20859cb01",
        "chrM": "c68f52674c9fb33aef52dcf399755519",
    }
}


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build local RGV landmark bundles from the resources used by IGV."
    )
    parser.add_argument(
        "--build",
        action="append",
        choices=sorted(BUILD_SPECS),
        dest="builds",
        help="Build only this genome (repeatable); defaults to all curated genomes.",
    )
    parser.add_argument("--output-root", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--force", action="store_true", help="Download existing assets again.")
    parser.add_argument(
        "--validate-existing",
        action="store_true",
        help="Validate installed bundles and print their gene/cytoband qualification without downloading.",
    )
    return parser.parse_args(argv)


def request(url: str) -> urllib.request.Request:
    return urllib.request.Request(url, headers={"User-Agent": "RGV-landmark-curator/1"})


def download(url: str, destination: Path, force: bool = False) -> None:
    if destination.is_file() and not force:
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        prefix=f".{destination.name}.", suffix=".part", dir=destination.parent, delete=False
    ) as temporary:
        temporary_path = Path(temporary.name)
        try:
            with urllib.request.urlopen(request(url), timeout=120) as response:
                shutil.copyfileobj(response, temporary, length=1024 * 1024)
            temporary.flush()
            os.fsync(temporary.fileno())
            os.replace(temporary_path, destination)
        except BaseException:
            temporary_path.unlink(missing_ok=True)
            raise


def load_json_url(url: str, destination: Path, force: bool) -> dict[str, Any]:
    download(url, destination, force)
    with destination.open(encoding="utf-8") as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError(f"{url} did not contain a JSON object")
    return value


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def asset(
    path: Path,
    root: Path,
    role: str,
    source: str,
    format_name: str,
    *,
    project: str,
    version: str,
    derivation: str,
    citation: str,
    license_name: str,
    license_url: str,
    required_notice: str = "",
) -> dict[str, Any]:
    path.chmod(0o644)
    return {
        "role": role,
        "path": path.relative_to(root).as_posix(),
        "format": format_name,
        "bytes": path.stat().st_size,
        "sha256": sha256(path),
        "source_url": source,
        "upstream_project": project,
        "upstream_version": version,
        "derivation": derivation,
        "citation": citation,
        "license": license_name,
        "license_url": license_url,
        "required_notice": required_notice,
    }


def validate_runtime_bundle(bundle: dict[str, Any]) -> None:
    assembly = bundle.get("assembly")
    if not isinstance(assembly, dict):
        raise ValueError("bundle assembly identity is not an object")
    dictionary_hashes = [assembly.get("dictionary_sha256"), *assembly.get("compatible_dictionary_sha256", [])]
    if any(
        not isinstance(digest, str)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest.lower())
        for digest in dictionary_hashes
    ) or len(set(dictionary_hashes)) != len(dictionary_hashes):
        raise ValueError("bundle assembly has invalid or duplicate dictionary SHA-256 claims")
    assets = bundle.get("assets")
    if not isinstance(assets, list):
        raise ValueError("bundle assets are not a list")
    roles: set[str] = set()
    for entry in assets:
        if not isinstance(entry, dict):
            raise ValueError("bundle asset is not an object")
        role = entry.get("role")
        path_value = entry.get("path")
        if not isinstance(role, str) or role in roles:
            raise ValueError(f"duplicate or invalid bundle asset role {role!r}")
        roles.add(role)
        if not isinstance(path_value, str):
            raise ValueError(f"bundle asset {role} has no relative path")
        path = PurePosixPath(path_value)
        if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
            raise ValueError(f"bundle asset {role} has unsafe path {path_value!r}")
        if not isinstance(entry.get("bytes"), int) or entry["bytes"] <= 0:
            raise ValueError(f"bundle asset {role} has no byte size")
        digest = entry.get("sha256")
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest.lower())
        ):
            raise ValueError(f"bundle asset {role} has invalid SHA-256")
        for key in (
            "source_url",
            "upstream_project",
            "upstream_version",
            "derivation",
            "citation",
            "license",
            "license_url",
        ):
            if not isinstance(entry.get(key), str) or not entry[key].strip():
                raise ValueError(f"bundle asset {role} has incomplete {key}")
    missing = REQUIRED_RUNTIME_ROLES.difference(roles)
    if missing:
        raise ValueError(f"bundle is missing runtime roles: {sorted(missing)}")


def immutable_asset_fingerprints(bundle: dict[str, Any]) -> dict[str, tuple[str, str, int]]:
    return {
        entry["role"]: (entry["path"], entry["sha256"], entry["bytes"])
        for entry in bundle.get("assets", [])
        if isinstance(entry, dict)
        and isinstance(entry.get("role"), str)
        and isinstance(entry.get("path"), str)
        and isinstance(entry.get("sha256"), str)
        and isinstance(entry.get("bytes"), int)
    }


def igv_asset(
    path: Path,
    root: Path,
    role: str,
    source: str,
    format_name: str,
    *,
    version: str,
    derivation: str,
) -> dict[str, Any]:
    return asset(
        path,
        root,
        role,
        source,
        format_name,
        project="IGV data repository and named upstream provider",
        version=version,
        derivation=derivation,
        citation="Robinson et al., Integrative Genomics Viewer; source-specific citations in NOTICE.md",
        license_name="Source-specific; see NOTICE.md",
        license_url="https://github.com/igvteam/igv-data/blob/main/LICENSE",
        required_notice="Retain bundle NOTICE.md and source URLs when redistributing.",
    )


def first_track(configuration: dict[str, Any]) -> dict[str, Any]:
    tracks = configuration.get("tracks")
    if not isinstance(tracks, list) or not tracks or not isinstance(tracks[0], dict):
        raise ValueError(f"IGV configuration {configuration.get('id')} has no default gene track")
    track = tracks[0]
    if not isinstance(track.get("url"), str):
        raise ValueError(f"IGV configuration {configuration.get('id')} has no gene-track URL")
    return track


def normalized_refgene_fields(line: str) -> list[str]:
    fields = line.rstrip("\r\n").split("\t")
    if len(fields) == 16:
        int(fields[0])
        fields = fields[1:]
    if len(fields) != 15:
        raise ValueError(f"RefSeq genePred row has {len(fields)} fields instead of 15 or 16")
    return fields


def comma_integers(value: str) -> list[int]:
    return [int(item) for item in value.rstrip(",").split(",") if item]


def validate_refgene(fields: Sequence[str], chrom_sizes: dict[str, int]) -> tuple[int, int]:
    transcript, chrom, strand = fields[:3]
    if not transcript or strand not in {"+", "-"}:
        raise ValueError(f"invalid transcript identity or strand: {transcript!r}")
    start, end, cds_start, cds_end, exon_count = map(int, fields[3:8])
    if chrom not in chrom_sizes:
        raise ValueError(f"genePred contig {chrom!r} is absent from chromosome sizes")
    if not (0 <= start < end <= chrom_sizes[chrom]):
        raise ValueError(f"genePred interval {chrom}:{start}-{end} is outside the assembly")
    if not (start <= cds_start <= cds_end <= end):
        raise ValueError(f"genePred CDS is outside transcript {transcript}")
    exon_starts = comma_integers(fields[8])
    exon_ends = comma_integers(fields[9])
    frames = comma_integers(fields[14])
    if not (exon_count == len(exon_starts) == len(exon_ends) == len(frames)):
        raise ValueError(f"genePred exon arrays disagree for {transcript}")
    previous_end = start
    for exon_start, exon_end in zip(exon_starts, exon_ends):
        if exon_start < previous_end or exon_start >= exon_end or exon_end > end:
            raise ValueError(f"invalid exon geometry for {transcript}")
        previous_end = exon_end
    spliced_length = sum(end_ - start_ for start_, end_ in zip(exon_starts, exon_ends))
    return exon_count, spliced_length


def chromosome_sizes(path: Path) -> tuple[dict[str, int], list[str]]:
    sizes: dict[str, int] = {}
    order: list[str] = []
    with path.open(encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            fields = line.rstrip("\r\n").split("\t")
            if len(fields) != 2 or not fields[0]:
                raise ValueError(f"invalid chromosome-size row {line_number} in {path}")
            size = int(fields[1])
            if size <= 0 or fields[0] in sizes:
                raise ValueError(f"invalid chromosome size for {fields[0]!r}")
            sizes[fields[0]] = size
            order.append(fields[0])
    if not sizes:
        raise ValueError(f"empty chromosome-size file {path}")
    return sizes, order


def dictionary_sha256(sizes: dict[str, int], order: Sequence[str]) -> str:
    digest = hashlib.sha256()
    for chrom in order:
        digest.update(f"{chrom}\t{sizes[chrom]}\n".encode("utf-8"))
    return digest.hexdigest()


def normalize_aliases(source: Path, destination: Path, sizes: dict[str, int]) -> int:
    aliases: dict[str, str] = {}
    with source.open(encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            if not line.strip() or line.startswith("#"):
                continue
            fields = [value for value in line.rstrip("\r\n").split("\t") if value]
            canonical = next((value for value in fields if value in sizes), None)
            if canonical is None:
                # Upstream alias tables may include an alternative mitochondrial
                # assembly or retired scaffold absent from this bundle. It has
                # no valid target and therefore contributes no normalized alias.
                continue
            for alias_name in fields:
                if alias_name == canonical:
                    continue
                if alias_name in sizes and alias_name != canonical:
                    raise ValueError(
                        f"alias {alias_name!r} collides with canonical contig {canonical!r}"
                    )
                previous = aliases.get(alias_name)
                if previous is not None and previous != canonical:
                    raise ValueError(
                        f"alias {alias_name!r} targets both {previous!r} and {canonical!r}"
                    )
                aliases[alias_name] = canonical
    rows = (f"{alias_name}\t{aliases[alias_name]}\n" for alias_name in sorted(aliases))
    write_text_lines(destination, rows)
    return len(aliases)


def valid_cytoband_stain(value: str) -> bool:
    return value in {
        "gneg",
        "gpos25",
        "gpos33",
        "gpos50",
        "gpos66",
        "gpos75",
        "gpos100",
        "acen",
        "gvar",
        "stalk",
    }


def validate_cytoband_row(
    chrom: str, start: int, end: int, name: str, stain: str, sizes: dict[str, int]
) -> Optional[str]:
    if chrom not in sizes:
        return None
    if not (0 <= start < end <= sizes[chrom]):
        raise ValueError(f"cytoband interval {chrom}:{start}-{end} is outside the assembly")
    if not valid_cytoband_stain(stain):
        raise ValueError(f"unsupported cytoband stain {stain!r}")
    return f"{chrom}\t{start}\t{end}\t{name}\t{stain}\n"


def normalize_text_cytobands(source: Path, destination: Path, sizes: dict[str, int]) -> int:
    rows: list[str] = []
    opener = gzip.open if source.suffix == ".gz" else open
    with opener(source, "rt", encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            fields = line.rstrip("\r\n").split("\t")
            if len(fields) < 5:
                raise ValueError(f"cytoband row {line_number} has fewer than five columns")
            row = validate_cytoband_row(
                fields[0], int(fields[1]), int(fields[2]), fields[3], fields[4], sizes
            )
            if row is not None:
                rows.append(row)
    rows.sort(key=lambda row: (row.split("\t", 1)[0], int(row.split("\t")[1])))
    write_text_lines(destination, rows)
    return len(rows)


def require_pybigwig() -> Any:
    if pyBigWig is None:
        raise ValueError(
            "BigBed normalization requires pyBigWig; run "
            "`uv venv tools/landmarks/.venv && "
            "uv pip install --python tools/landmarks/.venv/bin/python pyBigWig numpy`"
        )
    return pyBigWig


def normalize_bigbed_cytobands(source: Path, destination: Path, sizes: dict[str, int]) -> int:
    module = require_pybigwig()
    bigbed = module.open(str(source))
    rows: list[str] = []
    try:
        for chrom, length in bigbed.chroms().items():
            for start, end, rest in bigbed.entries(chrom, 0, length) or []:
                fields = rest.split("\t")
                if len(fields) < 2:
                    raise ValueError(f"BigBed cytoband at {chrom}:{start} has no name/stain")
                row = validate_cytoband_row(
                    chrom, int(start), int(end), fields[0], fields[1], sizes
                )
                if row is not None:
                    rows.append(row)
    finally:
        bigbed.close()
    rows.sort(key=lambda row: (row.split("\t", 1)[0], int(row.split("\t")[1])))
    write_text_lines(destination, rows)
    return len(rows)


def write_text_lines(path: Path, lines: Iterable[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        prefix=f".{path.name}.", mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as stream:
        temporary = Path(stream.name)
        try:
            stream.writelines(lines)
            stream.flush()
            os.fsync(stream.fileno())
            os.replace(temporary, path)
            path.chmod(0o644)
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise


def refgene_rows(path: Path) -> Iterator[list[str]]:
    with gzip.open(path, "rt", encoding="utf-8", newline="") as stream:
        for line in stream:
            if line.strip() and not line.startswith("#"):
                yield normalized_refgene_fields(line)


def accession_priority(accession: str) -> int:
    for priority, prefix in enumerate(("NM_", "NR_", "XM_", "XR_")):
        if accession.startswith(prefix):
            return priority
    return 4


def representative_rank(fields: Sequence[str]) -> tuple[int, int, int, int, str]:
    start, end, cds_start, cds_end = map(int, fields[3:7])
    exon_starts = comma_integers(fields[8])
    exon_ends = comma_integers(fields[9])
    spliced_length = sum(end_ - start_ for start_, end_ in zip(exon_starts, exon_ends))
    return (
        accession_priority(fields[0]),
        -(cds_end - cds_start),
        -spliced_length,
        -(end - start),
        fields[0],
    )


def write_gzip_lines(path: Path, lines: Iterable[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix=f".{path.name}.", dir=path.parent, delete=False) as raw:
        temporary = Path(raw.name)
        try:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                for line in lines:
                    compressed.write(line.encode("utf-8"))
            raw.flush()
            os.fsync(raw.fileno())
            os.replace(temporary, path)
            path.chmod(0o644)
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise


def validate_curated_gene_assets(representatives: Path, names: Path) -> int:
    """Verify the runtime gene model and symbol-search assets describe the same records."""
    representative_by_transcript: dict[str, tuple[str, str, int, int, str]] = {}
    for fields in refgene_rows(representatives):
        transcript = fields[0]
        if transcript in representative_by_transcript:
            raise ValueError(f"duplicate representative transcript {transcript!r}")
        symbol = fields[11]
        if not symbol:
            raise ValueError(f"representative transcript {transcript!r} has no gene symbol")
        representative_by_transcript[transcript] = (
            symbol,
            fields[1],
            int(fields[3]),
            int(fields[4]),
            fields[2],
        )

    search_by_transcript: dict[str, tuple[str, str, int, int, str]] = {}
    with gzip.open(names, "rt", encoding="utf-8", newline="") as stream:
        schema = stream.readline().rstrip("\r\n")
        if schema != SEARCH_SCHEMA_VERSION:
            raise ValueError(f"unsupported gene-search schema {schema!r}")
        for line_number, line in enumerate(stream, 2):
            fields = line.rstrip("\r\n").split("\t")
            if len(fields) != 9:
                raise ValueError(
                    f"gene-search row {line_number} has {len(fields)} fields instead of 9"
                )
            normalized, symbol, gene_id, transcript, chrom, start, end, strand, _ = fields
            if not symbol or not gene_id or not transcript:
                raise ValueError(f"gene-search row {line_number} has incomplete identity")
            if normalized != symbol.casefold():
                raise ValueError(
                    f"gene-search row {line_number} has a normalized symbol mismatch"
                )
            if strand not in {"+", "-"}:
                raise ValueError(f"gene-search row {line_number} has invalid strand {strand!r}")
            if transcript in search_by_transcript:
                raise ValueError(f"duplicate gene-search transcript {transcript!r}")
            search_by_transcript[transcript] = (
                symbol,
                chrom,
                int(start),
                int(end),
                strand,
            )

    if representative_by_transcript.keys() != search_by_transcript.keys():
        missing_search = sorted(representative_by_transcript.keys() - search_by_transcript.keys())
        missing_model = sorted(search_by_transcript.keys() - representative_by_transcript.keys())
        raise ValueError(
            "representative/search transcript sets disagree: "
            f"missing search rows {missing_search[:3]}, missing models {missing_model[:3]}"
        )
    for transcript, representative in representative_by_transcript.items():
        search = search_by_transcript[transcript]
        if representative != search:
            raise ValueError(
                f"representative/search metadata disagree for {transcript!r}: "
                f"{representative!r} != {search!r}"
            )
    if not representative_by_transcript:
        raise ValueError("curated gene assets contain no representative records")
    return len(representative_by_transcript)


def validate_existing_bundle(output_root: Path, identifier: str) -> dict[str, Any]:
    manifest_path = output_root / identifier / "manifest.json"
    with manifest_path.open(encoding="utf-8") as stream:
        manifest = json.load(stream)
    if manifest.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{identifier} has an unsupported manifest schema")
    bundle = manifest.get("bundle")
    if not isinstance(bundle, dict) or bundle.get("id") != identifier:
        raise ValueError(f"{identifier} manifest has the wrong bundle identity")
    validate_runtime_bundle(bundle)

    canonical_root = output_root.resolve()
    assets_by_role: dict[str, Path] = {}
    for entry in bundle["assets"]:
        path = (output_root / entry["path"]).resolve()
        if canonical_root != path and canonical_root not in path.parents:
            raise ValueError(f"bundle asset {entry['role']} escapes the output root")
        if not path.is_file():
            raise ValueError(f"bundle asset {entry['role']} is missing at {path}")
        if path.stat().st_size != entry["bytes"]:
            raise ValueError(f"bundle asset {entry['role']} has the wrong byte size")
        if sha256(path) != entry["sha256"]:
            raise ValueError(f"bundle asset {entry['role']} has the wrong SHA-256")
        assets_by_role[entry["role"]] = path

    genes = validate_curated_gene_assets(
        assets_by_role["representative_genes"], assets_by_role["gene_search"]
    )
    assembly = bundle["assembly"]
    sizes = {contig["name"]: contig["length"] for contig in assembly["contigs"]}
    primary = {contig["name"] for contig in assembly["contigs"] if contig["primary"]}
    cytoband_contigs: set[str] = set()
    acen_contigs: set[str] = set()
    cytoband_count = 0
    with assets_by_role["cytobands"].open(encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            fields = line.rstrip("\r\n").split("\t")
            if len(fields) != 5:
                raise ValueError(f"cytoband row {line_number} has {len(fields)} fields instead of 5")
            row = validate_cytoband_row(
                fields[0], int(fields[1]), int(fields[2]), fields[3], fields[4], sizes
            )
            if row is None:
                raise ValueError(f"cytoband row {line_number} targets an unknown contig")
            cytoband_count += 1
            cytoband_contigs.add(fields[0])
            if fields[4] == "acen":
                acen_contigs.add(fields[0])
    return {
        "bundle": identifier,
        "genes": genes,
        "cytobands": cytoband_count,
        "primary_without_cytobands": sorted(primary - cytoband_contigs),
        "primary_without_acen": sorted(primary - acen_contigs),
    }


def curate_refgene(
    source: Path,
    representatives: Path,
    names: Path,
    sizes: dict[str, int],
    chromosome_order: Sequence[str],
) -> dict[str, int]:
    selected: dict[str, list[str]] = {}
    source_transcript_count = 0
    transcript_count = 0
    excluded_transcript_count = 0
    exon_count = 0
    for fields in refgene_rows(source):
        source_transcript_count += 1
        if fields[1] not in sizes:
            excluded_transcript_count += 1
            continue
        exons, _ = validate_refgene(fields, sizes)
        transcript_count += 1
        exon_count += exons
        gene = fields[11] or fields[0]
        previous = selected.get(gene)
        if previous is None or representative_rank(fields) < representative_rank(previous):
            selected[gene] = fields

    order = {chrom: index for index, chrom in enumerate(chromosome_order)}
    rows = sorted(
        selected.values(),
        key=lambda fields: (
            order.get(fields[1], len(order)),
            int(fields[3]),
            int(fields[4]),
            fields[0],
        ),
    )
    write_gzip_lines(representatives, ("\t".join(fields) + "\n" for fields in rows))
    search_rows = [
        (fields[11].casefold(), fields[11], fields[11], fields[0], fields[1], fields[3], fields[4], fields[2], "")
        for fields in rows
    ]
    search_rows.sort(key=lambda row: (row[0], order.get(row[4], len(order)), int(row[5]), int(row[6]), row[2]))
    write_gzip_lines(
        names,
        chain(
            [SEARCH_SCHEMA_VERSION + "\n"],
            ("\t".join(row) + "\n" for row in search_rows),
        ),
    )
    return {
        "genes": len(rows),
        "source_transcripts": source_transcript_count,
        "transcripts": transcript_count,
        "off_reference_transcripts_excluded": excluded_transcript_count,
        "exons": exon_count,
    }


def biggenepred_gene_identity(fields: Sequence[str]) -> tuple[str, str]:
    transcript = fields[0]
    stable_gene = fields[14] if len(fields) > 14 else ""
    symbol = (
        (fields[15] if len(fields) > 15 else "")
        or stable_gene
        or (fields[9] if len(fields) > 9 else "")
        or transcript
    )
    transcript_like = stable_gene.startswith(("NM_", "NR_", "XM_", "XR_", "ENST"))
    gene_id = symbol if not stable_gene or transcript_like else stable_gene
    return gene_id, symbol


def biggenepred_rank(record: dict[str, Any]) -> tuple[int, int, int, int, int, str]:
    fields = record["fields"]
    mane_status = fields[21] if len(fields) > 21 else ""
    mane_priority = 0 if mane_status == "MANE Select" else 1 if mane_status else 2
    cds_start, cds_end = int(fields[3]), int(fields[4])
    block_sizes = comma_integers(fields[7])
    return (
        mane_priority,
        accession_priority(fields[0]),
        -(cds_end - cds_start),
        -sum(block_sizes),
        -(record["end"] - record["start"]),
        fields[0],
    )


def biggenepred_to_refgene(record: dict[str, Any], symbol: str) -> list[str]:
    chrom = record["chrom"]
    start = int(record["start"])
    end = int(record["end"])
    fields = record["fields"]
    if len(fields) < 17:
        raise ValueError(f"BigGenePred {chrom}:{start}-{end} has fewer than 20 total fields")
    block_count = int(fields[6])
    block_sizes = comma_integers(fields[7])
    relative_starts = comma_integers(fields[8])
    if block_count != len(block_sizes) or block_count != len(relative_starts):
        raise ValueError(f"BigGenePred exon arrays disagree for {fields[0]}")
    exon_starts = [start + value for value in relative_starts]
    exon_ends = [block_start + size for block_start, size in zip(exon_starts, block_sizes)]
    normalized = [
        fields[0],
        chrom,
        fields[2],
        str(start),
        str(end),
        fields[3],
        fields[4],
        str(block_count),
        ",".join(map(str, exon_starts)) + ",",
        ",".join(map(str, exon_ends)) + ",",
        fields[1],
        symbol,
        fields[10],
        fields[11],
        fields[12] if fields[12].endswith(",") else fields[12] + ",",
    ]
    return normalized


def curate_biggenepred(
    source: Path,
    representatives: Path,
    names: Path,
    sizes: dict[str, int],
    chromosome_order: Sequence[str],
) -> dict[str, int]:
    module = require_pybigwig()
    bigbed = module.open(str(source))
    selected: dict[str, dict[str, Any]] = {}
    source_transcript_count = 0
    excluded_transcript_count = 0
    try:
        for chrom, length in bigbed.chroms().items():
            for start, end, rest in bigbed.entries(chrom, 0, length) or []:
                source_transcript_count += 1
                if chrom not in sizes:
                    excluded_transcript_count += 1
                    continue
                fields = rest.split("\t")
                gene_id, symbol = biggenepred_gene_identity(fields)
                record = {
                    "chrom": chrom,
                    "start": int(start),
                    "end": int(end),
                    "fields": fields,
                    "gene_id": gene_id,
                    "symbol": symbol,
                }
                previous = selected.get(gene_id)
                if previous is None or biggenepred_rank(record) < biggenepred_rank(previous):
                    selected[gene_id] = record
    finally:
        bigbed.close()

    order = {chrom: index for index, chrom in enumerate(chromosome_order)}
    records = sorted(
        selected.values(),
        key=lambda record: (
            order.get(record["chrom"], len(order)),
            record["start"],
            record["end"],
            record["fields"][0],
        ),
    )
    normalized = [biggenepred_to_refgene(record, record["symbol"]) for record in records]
    for fields in normalized:
        validate_refgene(fields, sizes)
    write_gzip_lines(representatives, ("\t".join(fields) + "\n" for fields in normalized))

    search_rows = [
        (
            record["symbol"].casefold(),
            record["symbol"],
            record["gene_id"],
            record["fields"][0],
            record["chrom"],
            str(record["start"]),
            str(record["end"]),
            record["fields"][2],
            "",
        )
        for record in records
    ]
    search_rows.sort(
        key=lambda row: (
            row[0],
            order.get(row[4], len(order)),
            int(row[5]),
            int(row[6]),
            row[2],
        )
    )
    write_gzip_lines(
        names,
        chain(
            [SEARCH_SCHEMA_VERSION + "\n"],
            ("\t".join(row) + "\n" for row in search_rows),
        ),
    )
    return {
        "genes": len(records),
        "source_transcripts": source_transcript_count,
        "transcripts": source_transcript_count - excluded_transcript_count,
        "off_reference_transcripts_excluded": excluded_transcript_count,
        "exons": sum(int(fields[7]) for fields in normalized),
    }


def validate_bigbed(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(4)
    if len(header) != 4 or BIGBED_MAGIC not in {
        struct.unpack("<I", header)[0],
        struct.unpack(">I", header)[0],
    }:
        raise ValueError(f"{path} is not a BigBed file")


def supporting_assets(
    spec: BuildSpec,
    configuration: dict[str, Any],
    directory: Path,
    output_root: Path,
    force: bool,
) -> tuple[list[dict[str, Any]], dict[str, int], list[str]]:
    assets: list[dict[str, Any]] = []
    chrom_sizes_url = configuration.get("chromSizesURL")
    if not isinstance(chrom_sizes_url, str):
        raise ValueError(f"IGV configuration {configuration.get('id')} has no chromosome sizes")
    chrom_sizes_path = directory / "chrom.sizes"
    download(chrom_sizes_url, chrom_sizes_path, force)
    sizes, order = chromosome_sizes(chrom_sizes_path)
    assets.append(
        igv_asset(
            chrom_sizes_path,
            output_root,
            "chromosome_sizes",
            chrom_sizes_url,
            "chrom_sizes_tsv",
            version=spec.release,
            derivation="Copied and validated as an ordered contig dictionary.",
        )
    )

    alias_url = configuration.get("aliasURL")
    if not isinstance(alias_url, str):
        raise ValueError(f"IGV configuration {spec.identifier} has no contig aliases")
    raw_aliases = directory / "aliases.tsv"
    download(alias_url, raw_aliases, force)
    normalized_aliases = directory / "aliases.normalized.tsv"
    normalize_aliases(raw_aliases, normalized_aliases, sizes)
    assets.append(
        igv_asset(
            normalized_aliases,
            output_root,
            "contig_aliases",
            alias_url,
            "alias_tsv",
            version=spec.release,
            derivation="Normalized to one alias<TAB>canonical pair per row; conflicts rejected.",
        )
    )

    cytoband_url = configuration.get("cytobandURL")
    cytoband_bigbed_url = configuration.get("cytobandBbURL")
    normalized_cytobands = directory / "cytobands.txt"
    if isinstance(cytoband_url, str):
        raw_cytobands = directory / "cytobands.txt.gz"
        download(cytoband_url, raw_cytobands, force)
        normalize_text_cytobands(raw_cytobands, normalized_cytobands, sizes)
        source_url = cytoband_url
        derivation = "Decompressed and coordinate/stain validated as UCSC cytoband text."
    elif isinstance(cytoband_bigbed_url, str):
        raw_cytobands = directory / "cytobands.bb"
        download(cytoband_bigbed_url, raw_cytobands, force)
        validate_bigbed(raw_cytobands)
        normalize_bigbed_cytobands(raw_cytobands, normalized_cytobands, sizes)
        source_url = cytoband_bigbed_url
        derivation = "Decoded from BigBed and coordinate/stain validated as UCSC cytoband text."
    else:
        raise ValueError(f"IGV configuration {spec.identifier} has no cytobands")
    assets.append(
        igv_asset(
            normalized_cytobands,
            output_root,
            "cytobands",
            source_url,
            "ucsc_cytoband",
            version=spec.release,
            derivation=derivation,
        )
    )
    return assets, sizes, order


def notice_text(spec: BuildSpec, configuration_url: str) -> str:
    return f"""# {spec.identifier} landmark bundle notice

Bundle: `{spec.identifier}-{spec.bundle_version}`

This local RGV curation bundle was derived from the genome preset metadata and source URLs used by
the Integrative Genomics Viewer (IGV). IGV configuration metadata is distributed under the IGV
project's MIT licence. The normalized output remains subject to the terms and attribution
requirements of each named upstream data provider.

- IGV data/configuration: {configuration_url}
- IGV licence: https://github.com/igvteam/igv-data/blob/main/LICENSE
- NCBI data policies and disclaimer: https://www.ncbi.nlm.nih.gov/home/about/policies/
- NCBI RefSeq: https://www.ncbi.nlm.nih.gov/refseq/
- UCSC Genome Browser credits and conditions: https://genome.ucsc.edu/goldenPath/credits.html
- MANE project: https://www.ncbi.nlm.nih.gov/refseq/MANE/
- GENCODE project: https://www.gencodegenes.org/pages/data_format.html

The reference sequence itself is not included. Restricted clinical tracks and liftOver chains are
not included. This notice records provenance for local qualification; distribution still requires
the project's explicit legal/release review.
"""


def write_notice(spec: BuildSpec, directory: Path, configuration_url: str) -> Path:
    path = directory / "NOTICE.md"
    write_text_lines(path, [notice_text(spec, configuration_url)])
    return path


def bundle_contigs(
    spec: BuildSpec, sizes: dict[str, int], order: Sequence[str]
) -> list[dict[str, Any]]:
    primary = PRIMARY_CONTIGS[spec.identifier]
    missing = primary.difference(sizes)
    if missing:
        raise ValueError(f"{spec.identifier} is missing primary contigs: {sorted(missing)}")
    md5 = SEQUENCE_MD5.get(spec.identifier, {})
    return [
        {
            "name": chrom,
            "length": sizes[chrom],
            "md5": md5.get(chrom),
            "primary": chrom in primary,
        }
        for chrom in order
    ]


def build_bundle(spec: BuildSpec, output_root: Path, force: bool) -> dict[str, Any]:
    directory = output_root / spec.identifier
    directory.mkdir(parents=True, exist_ok=True)
    manifest_path = directory / "manifest.json"
    previous_bundle: Optional[dict[str, Any]] = None
    if manifest_path.is_file():
        with manifest_path.open(encoding="utf-8") as stream:
            previous_manifest = json.load(stream)
        candidate = previous_manifest.get("bundle")
        if isinstance(candidate, dict):
            previous_bundle = candidate
    config_url = f"{IGV_DATA_ROOT}/{spec.identifier}.json"
    configuration_path = directory / "igv.json"
    configuration = load_json_url(config_url, configuration_path, force)
    if configuration.get("id") != spec.identifier:
        raise ValueError(f"IGV configuration ID does not match {spec.identifier}")
    track = first_track(configuration)
    assets, sizes, order = supporting_assets(spec, configuration, directory, output_root, force)

    track_url = track["url"]
    track_format = str(track.get("format", "")).lower()
    counts: dict[str, int] = {}
    if track_format == "refgene":
        transcripts = directory / "transcripts.genePred.gz"
        download(track_url, transcripts, force)
        assets.append(
            igv_asset(
                transcripts,
                output_root,
                "detailed_transcripts",
                track_url,
                "gene_pred_ext_gzip",
                version=spec.release,
                derivation="Copied as the optional all-transcript source; coordinates validated during curation.",
            )
        )
    elif track_format == "biggenepred":
        transcripts = directory / "transcripts.bigGenePred"
        download(track_url, transcripts, force)
        validate_bigbed(transcripts)
        assets.append(
            igv_asset(
                transcripts,
                output_root,
                "detailed_transcripts",
                track_url,
                "big_gene_pred",
                version=spec.release,
                derivation="Copied as the indexed optional all-transcript source; BigBed signature validated.",
            )
        )
    else:
        raise ValueError(f"unsupported IGV gene format {track_format!r} for {spec.identifier}")

    if spec.landmark_mode == "igv_mane":
        landmark_url = configuration.get("maneBbURL")
        if not isinstance(landmark_url, str):
            raise ValueError(f"IGV configuration {spec.identifier} has no MANE BigBed")
        landmark_source = directory / "genes.bigGenePred"
        download(landmark_url, landmark_source, force)
        validate_bigbed(landmark_source)
    elif spec.landmark_mode == "curated_biggenepred":
        if spec.landmark_url is None:
            raise ValueError(f"missing curated landmark URL for {spec.identifier}")
        landmark_url = spec.landmark_url
        landmark_source = directory / "genes.bigGenePred"
        download(landmark_url, landmark_source, force)
        validate_bigbed(landmark_source)
    elif spec.landmark_mode == "representative_refgene":
        landmark_url = track_url
        landmark_source = directory / "transcripts.genePred.gz"
    else:
        raise ValueError(f"unknown landmark mode {spec.landmark_mode}")

    genes = directory / "genes.representative.genePred.gz"
    names = directory / "names.rgv.tsv.gz"
    if spec.landmark_mode == "representative_refgene":
        counts = curate_refgene(landmark_source, genes, names, sizes, order)
    else:
        counts = curate_biggenepred(landmark_source, genes, names, sizes, order)
    validated_gene_count = validate_curated_gene_assets(genes, names)
    if validated_gene_count != counts["genes"]:
        raise ValueError(
            "curated gene count disagrees with representative/search validation: "
            f"{counts['genes']} != {validated_gene_count}"
        )
    assets.extend(
        [
            igv_asset(
                genes,
                output_root,
                "representative_genes",
                landmark_url,
                "gene_pred_ext_gzip",
                version=spec.release,
                derivation=(
                    "Selected exactly one model per stable gene: MANE Select first, then NM/NR/XM/XR, "
                    "coding length, spliced length, genomic span, and stable accession."
                ),
            ),
            igv_asset(
                names,
                output_root,
                "gene_search",
                landmark_url,
                "rgv_gene_names_v1_gzip",
                version=spec.release,
                derivation="Derived deterministically from the representative records.",
            ),
        ]
    )

    notice = write_notice(spec, directory, config_url)
    assets.append(
        asset(
            notice,
            output_root,
            "notice",
            config_url,
            "markdown",
            project="RGV landmark curation with IGV/NCBI/UCSC/MANE/GENCODE sources",
            version=spec.bundle_version,
            derivation="Generated bundle-level attribution and release-gate notice.",
            citation="See NOTICE.md",
            license_name="Multiple source-specific terms; see NOTICE.md",
            license_url="https://github.com/igvteam/igv-data/blob/main/LICENSE",
            required_notice="NOTICE.md must accompany redistribution.",
        )
    )

    reference_url = configuration.get("fastaURL") or configuration.get("twoBitURL")
    selection_policy = (
        "one representative transcript per gene: MANE Select first, then NM, NR, XM, XR; "
        "then coding length, spliced length, genomic span, and stable accession"
    )
    bundle = {
        "id": spec.identifier,
        "version": spec.bundle_version,
        "display_name": configuration.get("name", spec.identifier),
        "provider": spec.provider,
        "release": spec.release,
        "selection_policy": selection_policy,
        "assembly": {
            "stable_id": spec.assembly_stable_id,
            "display_name": spec.assembly_display_name,
            "compatible_ids": list(spec.compatible_ids),
            "primary_contig_count": len(PRIMARY_CONTIGS[spec.identifier]),
            "dictionary_sha256": dictionary_sha256(sizes, order),
            "compatible_dictionary_sha256": list(spec.compatible_dictionary_sha256),
            "contigs": bundle_contigs(spec, sizes, order),
        },
        "assets": sorted(assets, key=lambda item: (item["role"], item["path"])),
    }
    validate_runtime_bundle(bundle)
    if (
        previous_bundle is not None
        and previous_bundle.get("id") == bundle["id"]
        and previous_bundle.get("version") == bundle["version"]
        and immutable_asset_fingerprints(previous_bundle)
        != immutable_asset_fingerprints(bundle)
    ):
        raise ValueError(
            f"immutable landmark release {bundle['id']} {bundle['version']} changed; "
            "bump bundle_version before rebuilding"
        )
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "bundle": bundle,
        "source_configuration": config_url,
        "reference": {
            "source_url": reference_url,
            "downloaded": False,
            "note": "Reference sequence is deliberately not part of the lightweight landmark bundle.",
        },
        "counts": counts,
        "distribution_status": "local_qualification_only_pending_release_review",
    }
    write_text_lines(manifest_path, [json.dumps(manifest, indent=2, sort_keys=True) + "\n"])
    return bundle


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    builds = args.builds or list(BUILD_SPECS)
    if args.validate_existing:
        qualifications = [
            validate_existing_bundle(args.output_root, identifier) for identifier in builds
        ]
        print(json.dumps(qualifications, indent=2))
        return 0
    args.output_root.mkdir(parents=True, exist_ok=True)
    catalog_by_id: dict[str, dict[str, Any]] = {}
    catalog_path = args.output_root / "catalog.json"
    if catalog_path.is_file():
        with catalog_path.open(encoding="utf-8") as stream:
            previous_catalog = json.load(stream)
        for entry in previous_catalog.get("bundles", []):
            if isinstance(entry, dict) and isinstance(entry.get("id"), str):
                catalog_by_id[entry["id"]] = entry
    for identifier in builds:
        print(f"Building {identifier}…", flush=True)
        catalog_by_id[identifier] = build_bundle(
            BUILD_SPECS[identifier], args.output_root, args.force
        )
    catalog = [
        catalog_by_id[identifier]
        for identifier in BUILD_SPECS
        if identifier in catalog_by_id
    ]
    write_text_lines(
        catalog_path,
        [json.dumps({"schema_version": SCHEMA_VERSION, "bundles": catalog}, indent=2) + "\n"],
    )
    print(f"Built {len(catalog)} bundles in {args.output_root}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, urllib.error.URLError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
