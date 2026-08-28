#!/usr/bin/env bash

# Source this file from an RGV Git worktree to give one development agent an
# isolated Cargo target directory while sharing the local sccache and large
# read-only benchmark fixtures.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    echo "source scripts/agent-worktree-env.sh [agent-id]" >&2
    exit 2
fi

rgv_configure_agent_worktree() {
    if (($# > 1)); then
        echo "usage: source scripts/agent-worktree-env.sh [agent-id]" >&2
        return 2
    fi

    local workspace_root
    workspace_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
        echo "agent-worktree-env.sh must be sourced from an RGV worktree" >&2
        return 2
    }

    local agent_id="${1:-${RGV_AGENT_ID:-$(basename "$workspace_root")}}"
    if [[ -z "$agent_id" || ! "$agent_id" =~ ^[A-Za-z0-9._-]+$ ]]; then
        echo "agent-id must contain only letters, digits, dots, underscores, or hyphens" >&2
        return 2
    fi

    if ! command -v sccache >/dev/null 2>&1; then
        echo "sccache is required; install it before configuring an agent worktree" >&2
        return 1
    fi

    local common_git_dir
    common_git_dir="$(git rev-parse --path-format=absolute --git-common-dir)" || return 2
    local canonical_root="${RGV_CANONICAL_ROOT:-$(dirname "$common_git_dir")}"
    local temp_root="${TMPDIR:-/tmp}"
    temp_root="${temp_root%/}"
    local target_base="${RGV_AGENT_TARGET_ROOT:-$temp_root/rgv-agent-targets}"
    target_base="${target_base%/}"
    local agent_target="$target_base/$agent_id"
    local shared_data="${RGV_SHARED_BENCHMARK_DATA:-$canonical_root/benchmark-data/large-public-alignments}"

    mkdir -p "$agent_target"

    export RGV_AGENT_ID="$agent_id"
    export RGV_CANONICAL_ROOT="$canonical_root"
    export RGV_SHARED_BENCHMARK_DATA="$shared_data"
    export CARGO_TARGET_DIR="$agent_target"
    export CARGO_INCREMENTAL=0
    export RUSTC_WRAPPER=sccache
    export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-10G}"

    if [[ "$workspace_root" != "$canonical_root" && -d "$shared_data" ]]; then
        local fixture_link="$workspace_root/benchmark-data/large-public-alignments"
        mkdir -p "$(dirname "$fixture_link")"
        if [[ -L "$fixture_link" ]]; then
            if [[ "$(readlink "$fixture_link")" != "$shared_data" ]]; then
                echo "warning: $fixture_link points somewhere other than $shared_data" >&2
            fi
        elif [[ -e "$fixture_link" ]]; then
            echo "warning: $fixture_link already exists; leaving it unchanged" >&2
        else
            ln -s "$shared_data" "$fixture_link"
        fi
    elif [[ ! -d "$shared_data" ]]; then
        echo "warning: shared benchmark data is unavailable at $shared_data" >&2
    fi

    printf 'RGV agent environment: %s\n' "$agent_id"
    printf '  worktree:    %s\n' "$workspace_root"
    printf '  cargo target: %s\n' "$CARGO_TARGET_DIR"
    printf '  shared data:  %s\n' "$RGV_SHARED_BENCHMARK_DATA"
    printf '  compiler:     sccache (limit %s, rustc incremental off)\n' "$SCCACHE_CACHE_SIZE"
}

rgv_configure_agent_worktree "$@"
rgv_agent_environment_status=$?
unset -f rgv_configure_agent_worktree
return "$rgv_agent_environment_status"
