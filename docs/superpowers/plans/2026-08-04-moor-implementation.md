# Moor implementation plan

**Goal:** Build the Moor program from the authoritative specification, closing all five approved areas, with at most 4,900 nonblank/non-comment production lines (hard failure at 4,901; normative ceiling 5,000).

**Architecture:** One Rust binary with shared policy modules and small `cfg(unix)` / `cfg(windows)` OS backends. One framed controller codec/state machine and one dual-body/dual-commit store implementation are reused by every command. Tests and fixtures do not count as production code.

## Task 1 — foundation, exact CLI, and LOC gate (budget 650)

Files: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/name.rs`, `tests/cli.rs`, `scripts/count-production-loc.sh`.

Write failing CLI/diagnostic/name tests first. Implement the frozen modern/legacy/bare grammar, option ownership/defaults, help/version bytes, strict numerics, name validation/rendering, and a counter that rejects 4,901 production lines. Verify targeted tests, full tests, and LOC.

## Task 2 — wire codec and controller state machines (budget 850; cumulative 1,500)

Files: `src/wire.rs`, `src/session.rs`, `tests/wire.rs`, `tests/lease.rs`.

Write failing vector/state tests first. Implement controller and semantic-producer framing/CRC, identity/status descriptors, generation fencing, leases with reconnect tokens and replay cache, input receipts, source epochs/durable semantic ACKs/application correlation/degradation, query recognition/arbitration, status/viewer bits, heartbeat, termination, and log-clear request/result. Verify vectors and transition tables.

## Task 3 — portable committed stores and event observation (budget 1,000; cumulative 2,500)

Files: `src/store.rs`, `src/events.rs`, `src/terminal.rs`, `tests/store.rs`, `tests/events.rs`, `tests/terminal.rs`.

Write crash-prefix/canonical-JSON/scanner tests first. Implement the approved in-place replacement of wire §13 with the shared 92-byte commit format, event/log/lifecycle adapters, recovery/exhaustion, canonical schema-v2 JSON, multi-record transactions, log gaps/clear barriers, and bounded OSC/query scanners. Replace the superseded 76-byte Windows-only carrier in the authoritative schema and verify old compatibility plus new vectors at every commit boundary.

## Task 4 — Unix holder and end-to-end commands (budget 1,450; cumulative 3,950)

Files: `src/unix.rs`, `src/holder.rs`, `src/client.rs`, `tests/unix_e2e.rs`.

Write failing shipped-binary tests first. Implement protected roots, durable supervised generation allocation, the private launch discriminator and adoption/freshness gate, atomic Unix rendezvous, peer credentials, PTY child setup, environment/exec gate, holder loop, attach/detach/replay/input/resize, start/new/run/push/list/current/tail/clear/kill/rm, signals and bounded shutdown. Exercise the real binary through sessions and supervisor-style reconnects.

## Task 5 — Windows backend, instrumentation hooks, and final conformance (budget 850; cumulative 4,800)

Files: `src/windows.rs`, `src/instrument.rs`, `tests/conformance.rs`, CI configuration if present.

Write shipped-binary/layout tests first. Implement the named-pipe/marker/ConPTY/job/bootstrap interfaces behind the shared policy, immutable instrumentation staging/ACK validation, and platform outcome encoding. Run the mandatory native Windows x64 and arm64 lanes (cross-compilation alone is not completion), plus Linux and macOS lanes. Package and test the sole `moor` executable while retaining an arbitrary renamed-copy CLI regression for basename-derived behavior. Recompute all vectors/hashes, run the full suite, run production LOC enforcement, and perform independent spec and quality reviews.

No task may consume the 100-line normative safety margin. If an area exceeds budget, simplify shared code before continuing.
