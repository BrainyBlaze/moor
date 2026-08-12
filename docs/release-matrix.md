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
| windows | x64 | `x86_64-pc-windows-msvc` | MSVC | `moor-<version>-windows-x64.exe` |
| windows | arm64 | `aarch64-pc-windows-msvc` | MSVC | `moor-<version>-windows-arm64.exe` |

`<version>` is the crate version from `Cargo.toml` (`0.1.0` for the first
release). The manifest key is the target triple; the asset name above is the
published filename. `x86_64-pc-windows-gnu` remains **compile-evidence only**
and is never published — MSVC is the distributed Windows ABI.

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
| linux-arm64 (musl) | ARM64 Linux |
| macos-x64 | macOS 13+ on Intel |
| macos-arm64 | macOS 13+ on Apple silicon |
| windows-x64 | Windows 10 1809, Windows Server 2019 (Server 2022 additional) |
| windows-arm64 | Windows 11 ARM64 |

macOS assets are built with deployment target 13.0 on each arch.

## Publication gates

An asset is published only when all of the following pass on its native lane;
none may be waived, and the release fails closed on any missing or red gate.

1. **§12.8 native conformance** — `create`, `attach`, `detach`, and `input`
   exercised against the shipped binary on the native platform. Compilation and
   smoke alone are insufficient.
2. **Linux static proof** — `file`, `readelf`, and `ldd` on each Linux asset
   demonstrate no dynamic interpreter and no shared-library dependency. A
   dynamically linked Linux artifact is nonconforming.
3. **Identity** — `moor --version` on the shipped asset reports exactly the
   release `<version>`.

### Native-provenance per asset

Each asset's manifest entry records its source commit and a reference to the
green native lane that satisfied gate 1. Provenance is labelled honestly:

| target | native-provenance lane | label |
|---|---|---|
| `x86_64-unknown-linux-musl` | Alpine musl + Ubuntu glibc | native |
| `aarch64-unknown-linux-musl` | ARM64 Linux where a native runner exists; otherwise emulated | native **or** `cross/emulated` — never silently conflated |
| `x86_64-apple-darwin` | macOS x64 | native |
| `aarch64-apple-darwin` | macOS arm64 | native |
| `x86_64-pc-windows-msvc` | Windows 10 1809 + Server 2019 | native |
| `aarch64-pc-windows-msvc` | Windows 11 ARM64 | native |

If an ARM64 Linux native runner is unavailable, that asset's provenance is
marked `cross/emulated` in the manifest so a fail-closed installer can
distinguish a natively proven asset from a cross-built one; it is never
presented as native.

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
