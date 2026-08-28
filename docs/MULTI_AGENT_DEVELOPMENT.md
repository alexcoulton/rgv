# Multi-Agent Development

RGV agents should use independent Git worktrees and persistent, independent
Cargo target directories. A bounded `sccache` supplements those targets;
large public genomics fixtures are shared read-only from the canonical
worktree.

This avoids three problems caused by a single shared Cargo target directory:

- concurrent Cargo processes waiting on the same build lock;
- one worktree replacing another worktree's final executable; and
- cleanup or incompatible compiler settings invalidating every agent at once.

## One-time setup

Install `sccache` and confirm it is available:

```bash
brew install sccache
sccache --version
```

The agent environment bounds the shared cache at 10 GB by default. Override
`SCCACHE_CACHE_SIZE` before sourcing the environment if another limit is
preferred.

Do not count on Rust objects being reusable across different agent target
paths. With `sccache` 0.17.0, the target path remains part of the Rust cache
key even when `SCCACHE_BASEDIRS` is configured. The practical cache hierarchy
is therefore:

1. each agent reuses its own persistent Cargo target;
2. `sccache` can accelerate recompilation with an unchanged path; and
3. registry and Git source downloads remain shared through Cargo home.

The canonical worktree owns the large local fixtures at:

```text
benchmark-data/large-public-alignments/
```

That directory is ignored by Git. Linked worktrees receive a symlink to the
canonical copy when the agent environment is activated.

## Create and activate a worktree

Create a branch and sibling worktree in the usual way:

```bash
git worktree add ../rgv-agent-inspector -b agent/inspector
cd ../rgv-agent-inspector
source scripts/agent-worktree-env.sh inspector
```

The activation prints the resolved configuration and exports:

```text
RGV_AGENT_ID
RGV_CANONICAL_ROOT
RGV_SHARED_BENCHMARK_DATA
CARGO_TARGET_DIR
CARGO_INCREMENTAL=0
RUSTC_WRAPPER=sccache
SCCACHE_CACHE_SIZE=10G
```

By default, the target directory is disposable and scoped to the agent:

```text
$TMPDIR/rgv-agent-targets/<agent-id>/
```

Agents can now run normal Cargo commands concurrently:

```bash
cargo check --all-targets
cargo test -p rgv-desktop
```

Use a unique agent ID for every live worktree. Reusing an ID deliberately
reuses that agent target and reintroduces build locking between those users.

## Shared versus isolated state

Shared:

- Cargo registry and Git source downloads in Cargo home;
- the bounded `sccache` storage (with the cross-target limitation above); and
- the large public BAM, FASTA, index, and annotation fixtures.

Isolated:

- debug and test artifacts;
- final debug executables;
- incremental rustc state, which is disabled in agent environments;
- screenshots, exports, temporary indexes, and test output; and
- each worktree's source changes.

Release builds used by the canonical MCP-controlled application should be
produced only from the canonical integration worktree. Agent worktrees should
not point launch services at their disposable targets.

## Cleanup

After an agent has stopped all Cargo processes, remove its disposable target:

```bash
scripts/cleanup-agent-target.sh inspector
```

This runs `cargo clean` against the exact per-agent target directory. It does
not touch another agent, the canonical release build, the shared compiler
cache, or genomic fixtures.

Inspect shared cache usage with:

```bash
sccache --show-stats
```

Do not run `cargo clean` against a target directory shared by active agents.

## Integration workflow

1. Develop and run focused tests in isolated agent worktrees.
2. Integrate completed changes into the canonical worktree.
3. Run workspace-wide checks from the canonical worktree.
4. Produce the release desktop and MCP binaries from the canonical worktree.
5. Restart the canonical controlled session only after the release build
   succeeds.
