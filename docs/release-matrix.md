# Moor release matrix

Authoritative definition of which platform binaries Moor publishes, how each is
proven, and how a consumer selects one. The binary asset **manifest** — literal
filenames, schema, `SHA256SUMS`, source commit, and provenance references — is
defined separately in `docs/release-manifest-v1.md`, which cross-links this
file. A downstream installer consumes the manifest; it never builds Moor from
source.

## Supported assets

Exactly six immutable assets per release. Linux is **static musl only** — there
is no glibc asset and no libc dimension, so a consumer selects on
`(os, arch)` alone.

| os | arch | Rust target triple | ABI / libc | asset name |
|---|---|---|---|---|
| linux | x64 | `x86_64-unknown-linux-musl` | fully static musl | `moor-<version>-linux-x64` |
| linux | arm64 | `aarch64-unknown-linux-musl` | fully static musl | `moor-<version>-linux-arm64` |
| macos | x64 | `x86_64-apple-darwin` | system | `moor-<version>-macos-x64` |
| macos | arm64 | `aarch64-apple-darwin` | system | `moor-<version>-macos-arm64` |
| windows | x64 | `x86_64-pc-windows-msvc` | MSVC, static CRT | `moor-<version>-windows-x64.exe` |
| windows | arm64 | `aarch64-pc-windows-msvc` | MSVC, static CRT | `moor-<version>-windows-arm64.exe` |

`<version>` is the crate version from `Cargo.toml` (`0.1.0` for the first
release). The manifest key is the target triple; the asset name above is the
published filename. `x86_64-pc-windows-gnu` remains **compile-evidence only**
and is never published — MSVC is the distributed Windows ABI.

The MSVC assets statically link the VC++ runtime and therefore do not require a
separately installed Visual C++ Redistributable. Native packaging records the PE
dependency table and rejects any `VCRUNTIME*.dll` or `MSVCP*.dll` import.

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
| linux-x64 (musl) | Ubuntu (glibc host), Alpine (musl host), WSL1, WSL2 |
| linux-arm64 (musl) | Ubuntu 24.04 ARM64, Alpine 3.20 ARM64 (both native ARM64 execution) |
| macos-x64 | macOS 13+ on Intel |
| macos-arm64 | macOS 13+ on Apple silicon |
| windows-x64 | Windows Server 2022 (input-fidelity floor, win32 input carrier); Windows 10 1809 and Windows Server 2019 as below-input-floor lanes (§12.8 — input-carrier cases expected-absent, everything else exercised) |
| windows-arm64 | Windows 11 ARM64 (input-fidelity floor) |

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
4. **Windows static-CRT proof** — the PE dependency table for each Windows asset
   is archived and contains no `VCRUNTIME*.dll` or `MSVCP*.dll` import.

### Required closure and the deferred set

The `(target, gate, lane)` pairs above split into two disjoint sets.

The **required closure** is every pair reachable on a GitHub-hosted runner —
26 pairs, covering both Windows lanes at the input-fidelity floor
(`windows-2022-x64`, `windows-11-arm64`), both macOS lanes, both Alpine musl
lanes, and the Ubuntu lanes. A candidate that omits any of these is refused.

The **deferred set** is the six pairs that require a self-hosted runner:

| target | gate | lane |
|---|---|---|
| `x86_64-unknown-linux-musl` | compatibility | `wsl1-ubuntu-22.04-x64` |
| `x86_64-unknown-linux-musl` | compatibility | `wsl2-ubuntu-22.04-x64` |
| `x86_64-pc-windows-msvc` | compatibility | `windows-10-1809-x64` |
| `x86_64-pc-windows-msvc` | compatibility | `windows-server-2019-x64` |
| `x86_64-pc-windows-msvc` | native-conformance | `windows-10-1809-x64` |
| `x86_64-pc-windows-msvc` | native-conformance | `windows-server-2019-x64` |

No runner is enrolled for these lanes, so requiring them made every candidate
unbuildable. For `v0.1.0` the operator therefore narrowed the mandatory closure
to the required set, with the deferred set restored once the runners exist.
Three properties keep that from becoming a silent waiver:

1. The deferred pairs remain **permitted**. A record from one of these lanes is
   accepted the moment its runner is enrolled, with no change to the matrix or
   the producer — restoring the full matrix is enrolment, not a code edit.
2. The candidate **names what it lacks**: its `coverage` object lists each
   deferred pair it did not verify and labels the closure by which pairs are
   missing, never by whether any are (`docs/release-manifest-v1.md`) —
   `"hosted-only"` if and only if all six are missing, `"partial"` if and only
   if one to five are missing, and `"full-matrix"` if and only if none are.
3. A deferred lane that runs is held to the same standard as any other: its
   record must cite the exact candidate commit and the exact asset digest, and
   a deferred lane that runs and **fails** still refuses the candidate. Only
   absence is tolerated, never failure.

These lanes carry the §12.8 below-input-floor evidence (Windows 10 1809 and
Server 2019) and the WSL1/WSL2 compatibility evidence. Until they are restored,
`v0.1.0` is not evidenced on those environments, and the candidate says so.

### Native-provenance per asset

Each asset's manifest entry records its source commit and a reference to the
green native lane that satisfied gate 1. Provenance is labelled honestly:

| target | native-provenance lane | label |
|---|---|---|
| `x86_64-unknown-linux-musl` | Alpine musl + Ubuntu glibc | native |
| `aarch64-unknown-linux-musl` | Ubuntu 24.04 ARM64 + Alpine 3.20 ARM64, native execution | native (required) |
| `x86_64-apple-darwin` | macOS x64 | native |
| `aarch64-apple-darwin` | macOS arm64 | native |
| `x86_64-pc-windows-msvc` | Windows Server 2022 (input-fidelity floor) + 1809/Server 2019 below-floor lanes | native |
| `aarch64-pc-windows-msvc` | Windows 11 ARM64 | native |

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
| win32 | x64 | windows-x64.exe |
| win32 | arm64 | windows-arm64.exe |

Any other `(platform, arch)` is unsupported: the installer refuses rather than
downloading a nearest match. After download it verifies the asset's SHA-256
against the manifest and confirms `moor --version` before activating the binary.
