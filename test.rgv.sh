#!/usr/bin/env bash
# Minimal local pre-push check, mirroring the `lint` and `build-test` jobs in
# .github/workflows/ci.yml. Uses the toolchain pinned in rust-toolchain.toml.
#
# Usage: ./test.rgv.sh [extra cargo test args...]
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked "$@"
