# Moor release matrix

Authoritative definition of which platform binaries Moor publishes, how each is
proven, and how a consumer selects one. The binary asset **manifest** — literal
filenames, schema, `SHA256SUMS`, source commit, and provenance references — is
defined separately in `docs/release-manifest-v1.md`, which cross-links this
file. A downstream installer consumes the manifest; it never builds Moor from
source.

## Supported assets

Exactly four immutable assets per release. Linux is **static musl only** — there
is no glibc asset and no libc dimension, so a consumer selects on
`(os, arch)` alone.

| os | arch | Rust target triple | ABI / libc | asset name |
|---|---|---|---|---|
| linux | x64 | `x86_64-unknown-linux-musl` | fully static musl | `moor-<version>-linux-x64` |
| linux | arm64 | `aarch64-unknown-linux-musl` | fully static musl | `moor-<version>-linux-arm64` |
| macos | x64 | `x86_64-apple-darwin` | system | `moor-<version>-macos-x64` |
| macos | arm64 | `aarch64-apple-darwin` | system | `moor-<version>-macos-arm64` |

`<version>` is the crate version from `Cargo.toml` (`0.1.0` for the first
release). The manifest key is the target triple; the asset name above is the
published filename. Only these four targets are built, verified, or published.

### Why static musl for Linux, not glibc

A downloaded glibc binary fails against the target distribution's glibc version
(`version 'GLIBC_2.XX' not found`), and Node's `process.platform`/`process.arch`
cannot distinguish glibc from musl — so shipping both would force the installer
into fragile libc sniffing (`/etc/os-release`, `ldd --version`). A single fully
static musl binary removes that entire failure class: it has no dynamic
interpreter and no shared-library dependency, so it runs on any Linux
distribution regardless of libc. Moor uses only facilities musl provides
statically (PTY via `openpty`/`posix_openpt`/`grantpt`, `getuid`/`geteuid`,
`/proc` ancestry, positional file I/O); it performs no NSS name resolution or
DNS, so static linkage removes nothing it needs.

## Build-once, verify-many

Each asset is **built once and hashed once**; the identical bytes are then
exercised across every compatibility lane below. A compatibility lane never
rebuilds — it downloads the one published artifact and runs it, so the proof is
about the shipped bytes, not a lane-local rebuild.

| asset | exact-byte compatibility lanes |
|---|---|
| linux-x64 (musl) | Ubuntu (glibc host), Alpine (musl host) |
| linux-arm64 (musl) | Ubuntu 24.04 ARM64, Alpine 3.20 ARM64 (both native ARM64 execution) |
| macos-x64 | macOS 13+ on Intel |
| macos-arm64 | macOS 13+ on Apple silicon |

macOS assets are built with deployment target 13.0 on each arch.

## Publication gates

An asset is published only when all of the following pass on its native lane;
none may be waived, and the release fails closed on any missing or red gate.
"Waived" means accepted without evidence and without saying so — that remains
forbidden. The deferred set defined below is the opposite of a waiver: it is
declared in the candidate's own bytes, and a candidate that omits a deferred
lane says which one it omitted.

1. **§12.8 native conformance** — `create`, `attach`, `detach`, and `input`
   exercised against the shipped binary on the native platform. Compilation and
   smoke alone are insufficient.
2. **Linux static proof** — `file`, `readelf`, and `ldd` on each Linux asset
   demonstrate no dynamic interpreter and no shared-library dependency. A
   dynamically linked Linux artifact is nonconforming.
3. **Identity** — `moor --version` on the shipped asset reports exactly the
   release `<version>`.

### Required closure and the deferred set

The `(target, gate, lane)` pairs above split into two disjoint sets.

The **required closure** is every pair of the matrix — 18 pairs, covering
both macOS lanes, both Alpine musl lanes, and the Ubuntu lanes — and every one
of them runs on a GitHub-hosted runner. A candidate that omits any of these is
refused.

The **deferred set** is empty: no lane needs a self-hosted runner.

The manifest's `coverage` object still names the closure it verified
(`docs/release-manifest-v1.md`): with an empty deferred set every candidate
states `"full-matrix"` and carries no `unverified` list. The other two labels
the producer can emit — `"hosted-only"` when no deferred pair was verified and
`"partial"` when some were — cannot occur while the set is empty, but they stay
documented and exercised because `coverage` is part of the consumer pin
contract, and because a future lane that needs enrolment can be deferred there
without changing the consumer. A deferred lane that runs is held to the same
standard as any other, and one that runs and **fails** still refuses the
candidate.

### Native-provenance per asset

Each asset's manifest entry records its source commit and a reference to the
green native lane that satisfied gate 1. Provenance is labelled honestly:

| target | native-provenance lane | label |
|---|---|---|
| `x86_64-unknown-linux-musl` | Alpine musl + Ubuntu glibc | native |
| `aarch64-unknown-linux-musl` | Ubuntu 24.04 ARM64 + Alpine 3.20 ARM64, native execution | native (required) |
| `x86_64-apple-darwin` | macOS x64 | native |
| `aarch64-apple-darwin` | macOS arm64 | native |

Native execution is mandatory for every asset, aarch64 Linux included: §12.8
requires the exact static asset to execute natively on Ubuntu 24.04 ARM64 and
on Alpine 3.20 ARM64. Cross-compilation may produce the bytes and QEMU may add
diagnostics, but neither substitutes for native execution. The manifest carries
no weakened native-proven flag — an asset without native §12.8 evidence is
blocked, and so is the release.

## Consumer selection

An installer maps the running platform to exactly one asset and fails closed on
any other combination:

| `process.platform` | `process.arch` | asset |
|---|---|---|
| linux | x64 | linux-x64 |
| linux | arm64 | linux-arm64 |
| darwin | x64 | macos-x64 |
| darwin | arm64 | macos-arm64 |

Any other `(platform, arch)` is unsupported: the installer refuses rather than
downloading a nearest match. After download it verifies the asset's SHA-256
against the manifest and confirms `moor --version` before activating the binary.
