#!/usr/bin/env bash

set -euo pipefail

workspace_root="$(git rev-parse --show-toplevel)"
curator_python="$workspace_root/tools/landmarks/.venv/bin/python"
if [[ ! -x "$curator_python" ]]; then
    echo "Landmark curator environment is missing." >&2
    echo "Run: uv sync --project $workspace_root/tools/landmarks" >&2
    exit 1
fi
exec "$curator_python" "$workspace_root/tools/landmarks/build_bundles.py" "$@"
