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
from pathlib import Path
from typing import Any, Iterable, Iterator, Optional, Sequence


SCHEMA_VERSION = 1
IGV_DATA_ROOT = "https://raw.githubusercontent.com/igvteam/igv-data/main/genomes/json"
DEFAULT_OUTPUT = (
    Path(__file__).resolve().parents[2]
    / "benchmark-data"
    / "large-public-alignments"
    / "landmarks"
    / "igv"
)
BIGBED_MAGIC = 0x8789F2EB


@dataclass(frozen=True)
class BuildSpec:
    identifier: str
    landmark_mode: str
    landmark_url: Optional[str] = None


BUILD_SPECS = {
    "hg38": BuildSpec("hg38", "igv_mane"),
    "hg19": BuildSpec("hg19", "igv_mane"),
    "mm10": BuildSpec("mm10", "representative_refgene"),
    "mm39": BuildSpec("mm39", "representative_refgene"),
    "hs1": BuildSpec(
        "hs1",
        "curated_biggenepred",
        "https://hgdownload.soe.ucsc.edu/gbdb/hs1/ncbiRefSeq/ncbiRefSeqCurated.bb",
    ),
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


def asset(path: Path, root: Path, role: str, source: str, format_name: str) -> dict[str, Any]:
    path.chmod(0o644)
    return {
        "role": role,
        "path": path.relative_to(root).as_posix(),
        "format": format_name,
        "source": source,
        "bytes": path.stat().st_size,
        "sha256": sha256(path),
    }


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
    write_gzip_lines(
        names,
        (
            f"{fields[11]}\t{fields[0]}\t{fields[1]}\t{fields[3]}\t{fields[4]}\n"
            for fields in rows
        ),
    )
    return {
        "genes": len(rows),
        "source_transcripts": source_transcript_count,
        "transcripts": transcript_count,
        "off_reference_transcripts_excluded": excluded_transcript_count,
        "exons": exon_count,
    }


def validate_bigbed(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(4)
    if len(header) != 4 or BIGBED_MAGIC not in {
        struct.unpack("<I", header)[0],
        struct.unpack(">I", header)[0],
    }:
        raise ValueError(f"{path} is not a BigBed file")


def download_trix(url: str, directory: Path, force: bool) -> list[dict[str, Any]]:
    assets = []
    for suffix in ("", "x"):
        source = url + suffix
        destination = directory / ("genes.ix" + suffix)
        download(source, destination, force)
        assets.append(asset(destination, directory, "gene_search", source, "trix"))
    return assets


def supporting_assets(
    configuration: dict[str, Any], directory: Path, force: bool
) -> tuple[list[dict[str, Any]], dict[str, int], list[str]]:
    assets: list[dict[str, Any]] = []
    chrom_sizes_url = configuration.get("chromSizesURL")
    if not isinstance(chrom_sizes_url, str):
        raise ValueError(f"IGV configuration {configuration.get('id')} has no chromosome sizes")
    chrom_sizes_path = directory / "chrom.sizes"
    download(chrom_sizes_url, chrom_sizes_path, force)
    sizes, order = chromosome_sizes(chrom_sizes_path)
    assets.append(asset(chrom_sizes_path, directory, "chromosome_sizes", chrom_sizes_url, "tsv"))

    for key, role, filename, format_name in (
        ("aliasURL", "contig_aliases", "aliases.tsv", "tsv"),
        ("cytobandURL", "cytobands", "cytobands.txt.gz", "ucsc_cytoband"),
        ("cytobandBbURL", "cytobands", "cytobands.bb", "bigBed"),
    ):
        url = configuration.get(key)
        if isinstance(url, str):
            destination = directory / filename
            download(url, destination, force)
            assets.append(asset(destination, directory, role, url, format_name))
    return assets, sizes, order


def build_bundle(spec: BuildSpec, output_root: Path, force: bool) -> dict[str, Any]:
    directory = output_root / spec.identifier
    directory.mkdir(parents=True, exist_ok=True)
    config_url = f"{IGV_DATA_ROOT}/{spec.identifier}.json"
    configuration_path = directory / "igv.json"
    configuration = load_json_url(config_url, configuration_path, force)
    if configuration.get("id") != spec.identifier:
        raise ValueError(f"IGV configuration ID does not match {spec.identifier}")
    track = first_track(configuration)
    assets = [asset(configuration_path, directory, "igv_configuration", config_url, "json")]
    support, sizes, order = supporting_assets(configuration, directory, force)
    assets.extend(support)

    track_url = track["url"]
    track_format = str(track.get("format", "")).lower()
    counts: dict[str, int] = {}
    if track_format == "refgene":
        transcripts = directory / "transcripts.genePred.gz"
        download(track_url, transcripts, force)
        assets.append(
            asset(transcripts, directory, "detailed_transcripts", track_url, "genePredExt")
        )
        genes = directory / "genes.genePred.gz"
        names = directory / "names.tsv.gz"
        counts = curate_refgene(transcripts, genes, names, sizes, order)
        assets.extend(
            [
                asset(genes, directory, "derived_gene_landmarks", track_url, "genePredExt"),
                asset(names, directory, "derived_gene_search", track_url, "rgv_landmark_names_v1"),
            ]
        )
    elif track_format == "biggenepred":
        transcripts = directory / "transcripts.bigGenePred"
        download(track_url, transcripts, force)
        validate_bigbed(transcripts)
        assets.append(
            asset(transcripts, directory, "detailed_transcripts", track_url, "bigGenePred")
        )
    else:
        raise ValueError(f"unsupported IGV gene format {track_format!r} for {spec.identifier}")

    if spec.landmark_mode == "igv_mane":
        landmark_url = configuration.get("maneBbURL")
        if not isinstance(landmark_url, str):
            raise ValueError(f"IGV configuration {spec.identifier} has no MANE BigBed")
        genes = directory / "genes.bigGenePred"
        download(landmark_url, genes, force)
        validate_bigbed(genes)
        assets.append(asset(genes, directory, "gene_landmarks", landmark_url, "bigGenePred"))
        trix_url = configuration.get("maneTrixURL")
        if isinstance(trix_url, str):
            assets.extend(download_trix(trix_url, directory, force))
    elif spec.landmark_mode == "curated_biggenepred":
        if spec.landmark_url is None:
            raise ValueError(f"missing curated landmark URL for {spec.identifier}")
        genes = directory / "genes.bigGenePred"
        download(spec.landmark_url, genes, force)
        validate_bigbed(genes)
        assets.append(asset(genes, directory, "gene_landmarks", spec.landmark_url, "bigGenePred"))
    elif spec.landmark_mode != "representative_refgene":
        raise ValueError(f"unknown landmark mode {spec.landmark_mode}")

    trix_url = track.get("trixURL")
    if isinstance(trix_url, str):
        assets.extend(download_trix(trix_url, directory, force))

    reference_url = configuration.get("fastaURL") or configuration.get("twoBitURL")
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "id": spec.identifier,
        "name": configuration.get("name", spec.identifier),
        "source_configuration": config_url,
        "reference": {
            "source": reference_url,
            "downloaded": False,
            "note": (
                "Reference sequence is deliberately not part of the lightweight landmark bundle."
            ),
        },
        "selection": (
            "IGV-provided MANE/curated BigGenePred"
            if spec.landmark_mode != "representative_refgene"
            else "one transcript per gene: NM, NR, XM, XR; then coding, spliced, and genomic length"
        ),
        "counts": counts,
        "assets": assets,
    }
    manifest_path = directory / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    landmark = next((item for item in assets if item["role"] == "gene_landmarks"), None)
    if landmark is None:
        landmark = next(item for item in assets if item["role"] == "derived_gene_landmarks")
    return {
        "id": spec.identifier,
        "name": manifest["name"],
        "manifest": manifest_path.relative_to(output_root).as_posix(),
        "landmarks": f"{spec.identifier}/{landmark['path']}",
        "landmark_format": landmark["format"],
    }


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    builds = args.builds or list(BUILD_SPECS)
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
    catalog_path.write_text(
        json.dumps({"schema_version": SCHEMA_VERSION, "bundles": catalog}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"Built {len(catalog)} bundles in {args.output_root}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, urllib.error.URLError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
