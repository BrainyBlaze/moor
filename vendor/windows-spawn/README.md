# windows-spawn

[![CI](https://github.com/P4suta/windows-spawn/actions/workflows/ci.yml/badge.svg)](https://github.com/P4suta/windows-spawn/actions/workflows/ci.yml)

Windows process creation with explicit handle transfer, ordered Job attachment,
mitigation policies, ConPTY, and suspended inspection.

## Use and platform

Use `std::process::Command` for portable child processes. Use this crate for
`CreateProcessW` features that require explicit ownership and rollback.

The crate requires Windows 10 version 1809 or later and Rust 1.75 or later.
Non-Windows targets expose no public API.

This project is not affiliated with Microsoft or the `windows-rs` project. The
`windows-` prefix describes the target platform, not the publisher.

## Installation

```console
cargo add windows-spawn
```

## Minimal example

```rust
use windows_spawn::{Command, DropPolicy, SpawnOptions};

let mut command = Command::new(r"C:\Windows\System32\cmd.exe");
command.args(["/D", "/S", "/C"]).raw_arg("echo hello");

let output = command.output_with(
    SpawnOptions::new().drop_policy(DropPolicy::KillTree),
)?;
assert!(output.status.success());
# Ok::<(), std::io::Error>(())
```

## Documentation

- [API and behavioral contracts](https://docs.rs/windows-spawn)
- [Examples](https://github.com/P4suta/windows-spawn/tree/main/examples)
- [Architecture decisions](https://github.com/P4suta/windows-spawn/tree/main/docs/adr)
- [Security policy](https://github.com/P4suta/windows-spawn/security/policy)

## License

Apache-2.0 OR MIT.
