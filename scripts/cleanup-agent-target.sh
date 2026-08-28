#!/usr/bin/env bash

set -euo pipefail

if (($# > 1)); then
    echo "usage: scripts/cleanup-agent-target.sh [agent-id]" >&2
    exit 2
fi

workspace_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "cleanup-agent-target.sh must run from an RGV worktree" >&2
    exit 2
}
agent_id="${1:-${RGV_AGENT_ID:-$(basename "$workspace_root")}}"
if [[ -z "$agent_id" || ! "$agent_id" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "agent-id must contain only letters, digits, dots, underscores, or hyphens" >&2
    exit 2
fi

temp_root="${TMPDIR:-/tmp}"
temp_root="${temp_root%/}"
target_base="${RGV_AGENT_TARGET_ROOT:-$temp_root/rgv-agent-targets}"
target_base="${target_base%/}"
agent_target="$target_base/$agent_id"

case "$agent_target" in
    "$target_base"/*) ;;
    *)
        echo "refusing to clean a target outside $target_base" >&2
        exit 2
        ;;
esac

if [[ ! -d "$agent_target" ]]; then
    echo "No target directory exists for $agent_id at $agent_target"
    exit 0
fi

echo "Cleaning the disposable Cargo target for $agent_id: $agent_target"
cargo clean --target-dir "$agent_target"
