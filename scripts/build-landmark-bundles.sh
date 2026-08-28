#!/usr/bin/env bash

set -euo pipefail

workspace_root="$(git rev-parse --show-toplevel)"
exec python3 "$workspace_root/tools/landmarks/build_bundles.py" "$@"
