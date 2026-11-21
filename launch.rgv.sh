#!/usr/bin/env bash
# Developer launch helper. Paths are derived from this script's location, so it
# works from any checkout and any working directory.
#
# Usage: ./launch.rgv.sh [bam|reference|trace|trace-b37] [extra rgv-desktop args...]
#
# The benchmark inputs below live under benchmark-data/large-public-alignments,
# which is not tracked in git; override them with the environment variables
# if your copies are elsewhere.
#   RGV_BAM        alignment file for the `bam` mode
#   RGV_REFERENCE  reference FASTA
#   RGV_TRACE_DIR  output directory for the trace modes (default: system temp)
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

data_dir="benchmark-data/large-public-alignments"
bam="${RGV_BAM:-$data_dir/ENCFF686MTA.bam}"
reference="${RGV_REFERENCE:-$data_dir/reference/GRCh38_V29_UCSC_names_ERCC_phiX.fa}"
trace_dir="${RGV_TRACE_DIR:-${TMPDIR:-/tmp}}"

mode="${1:-bam}"
if [ "$#" -gt 0 ]; then
    shift
fi

case "$mode" in
    bam)
        exec cargo run --release -p rgv-desktop -- \
            --bam "$bam" \
            --reference "$reference" \
            --locus chr7:5526400-5530600 \
            --max-alignments 10000000 "$@"
        ;;
    reference)
        exec cargo run --release -p rgv-desktop -- \
            --reference "$reference" "$@"
        ;;
    trace)
        exec cargo run --release -p rgv-desktop -- \
            --reference "$reference" \
            --start-diagnostic-trace \
            --diagnostic-trace-dir "$trace_dir" "$@"
        ;;
    trace-b37)
        exec cargo run --release -p rgv-desktop -- \
            --reference "${RGV_REFERENCE:-$data_dir/reference/human_g1k_v37_decoy.fasta}" \
            --start-diagnostic-trace \
            --diagnostic-trace-dir "$trace_dir" "$@"
        ;;
    *)
        echo "usage: $0 [bam|reference|trace|trace-b37] [extra rgv-desktop args...]" >&2
        exit 2
        ;;
esac
