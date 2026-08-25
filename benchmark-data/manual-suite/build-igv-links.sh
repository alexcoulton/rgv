#!/usr/bin/env bash
set -euo pipefail

suite_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$suite_root/../.." && pwd)"
projects_dir="$suite_root/projects"
output_dir="$suite_root/igv"

command -v jq >/dev/null 2>&1 || {
    echo "jq is required to resolve resources from the manual fixture sessions" >&2
    exit 1
}

mkdir -p "$output_dir"

relative_path() {
    python3 - "$1" "$2" <<'PY'
import os
import sys

print(os.path.relpath(sys.argv[1], sys.argv[2]))
PY
}

link_resource() {
    local source_path="$1"
    local destination_dir="$2"
    local link_name="$3"
    local link_path="$destination_dir/$link_name"
    local relative_target
    relative_target="$(relative_path "$source_path" "$destination_dir")"

    if [[ -e "$link_path" || -L "$link_path" ]]; then
        if [[ -L "$link_path" && "$(readlink "$link_path")" == "$relative_target" ]]; then
            return
        fi
        echo "refusing to replace conflicting IGV resource: $link_path" >&2
        exit 1
    fi
    ln -s "$relative_target" "$link_path"
}

while IFS= read -r project; do
    fixture="$(basename "$project" .rgv.json)"
    fixture_dir="$output_dir/$fixture"
    project_dir="$(dirname "$project")"
    mkdir -p "$fixture_dir"
    link_resource "$project" "$fixture_dir" "session.rgv.json"

    while IFS= read -r configured_path; do
        [[ -n "$configured_path" ]] || continue
        source_path="$(python3 - "$project_dir" "$configured_path" <<'PY'
import os
import sys

print(os.path.abspath(os.path.join(sys.argv[1], sys.argv[2])))
PY
)"
        link_resource "$source_path" "$fixture_dir" "$(basename "$source_path")"

        for suffix in .bai .csi .crai .tbi .fai .gzi; do
            sidecar="$source_path$suffix"
            if [[ -e "$sidecar" ]]; then
                link_resource "$sidecar" "$fixture_dir" "$(basename "$sidecar")"
            fi
        done
    done < <(jq -r '.. | objects | .Local?.path? // empty' "$project" | sort -u)
done < <(find "$projects_dir" -maxdepth 1 -name '*.rgv.json' -print | sort)

link_resource "$output_dir" "$repository_root" "igv-manual-fixtures"

echo "IGV fixture links are ready in $output_dir"
