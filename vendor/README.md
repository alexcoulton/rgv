# Vendored crates

Patched copies of two crates.io packages, wired in through `[patch.crates-io]` in the workspace
`Cargo.toml`. Both exist so that HTSlib builds for Windows with MinGW
(`x86_64-pc-windows-gnu`); on macOS and Linux they build exactly as the published crates do.

| Crate | Version | Source |
|---|---|---|
| `hts-sys` (bundles HTSlib 1.19.1) | 2.2.1 | crates.io, MIT |
| `rust-htslib` | 1.0.1 | crates.io, MIT |

The published crate contents are copied unchanged except for the changes listed here, which are
all marked `RGV:` in the source. `hts-sys` omits HTSlib's `test/`, `samples/` and
`htscodecs/tests/` directories, which the build never reads.

## `hts-sys`

`build.rs`:

- Reads the HTSlib version from the `VERSION=` line of `htslib/version.sh` instead of running
  the script, which Windows cannot execute. A crate copy has no `.git`, so the script printed
  exactly that line anyway.
- For Windows targets, sets what HTSlib's own `configure` sets for MinGW: `_XOPEN_SOURCE=600`,
  `_FILE_OFFSET_BITS=64` (files over 2 GB) and `__USE_MINGW_ANSI_STDIO=1`. The bindgen run gets
  the same defines, so Rust and C agree on the size of `off_t`.
- Leaves out `HAVE_DRAND48` on Windows, where HTSlib then uses its own implementation.
- Adds `windows-compat/regex.h` to the Windows include path: a stub whose `regcomp` always
  fails. HTSlib uses regular expressions only for the `=~` operator of its filter expressions,
  which RGV does not use (upstream links libsystre/libtre instead).

## `rust-htslib`

- `src/bam/mod.rs`: `cram_seek` takes HTSlib's `off_t` rather than `libc::off_t`, which is 32-bit
  on Windows.

## Updating

Copy the new published versions over these directories, re-apply the `RGV:` changes, and check
the Windows build (`.github/actions/windows-mingw`, used by CI and the release workflow).
