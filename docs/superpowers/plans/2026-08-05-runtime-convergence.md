# Runtime convergence implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve the frozen holder and viewer behavior while reducing `src/runtime/holder.rs` plus `src/runtime/io.rs` to at most 1,200 default-rustfmt effective lines.

**Architecture:** Keep `session`, `wire`, `terminal`, and `storage` as the policy owners. Turn the runtime into one event scheduler: decoded peer requests, PTY bytes, write completions, storage completions, and deadlines each enter one dispatch path and directly call those policy owners. Keep the viewer as one select loop whose small state object handles a network event or local command without auxiliary task abstractions or duplicate reconnect paths.

**Tech Stack:** Rust 2024, `crossbeam-channel`, existing typed wire/session APIs, characterization tests.

---

### Task 1: Lock the behavior baseline

**Files:**
- Test: `tests/runtime_holder.rs`
- Test: `tests/runtime_io.rs`

- [ ] Run `cargo test --test runtime_holder --test runtime_io --test lease --no-fail-fast` and require all tests to pass.
- [ ] Record default-rustfmt effective counts for `src/runtime/holder.rs` and `src/runtime/io.rs`.

### Task 2: Collapse the holder scheduler and dispatch

**Files:**
- Modify: `src/runtime/holder.rs`
- Test: `tests/runtime_holder.rs`

- [ ] Replace wrapper chains around send/refuse/write/storage/disconnect with one direct event dispatcher.
- [ ] Store request data once: borrow decoded bytes through admission and allocate only data that outlives dispatch.
- [ ] Merge deadline scans for peers, leases, notices, queries, heartbeat, storage, and termination into one scheduler pass.
- [ ] Run `cargo test --test runtime_holder --test lease --no-fail-fast` after each independent deletion.

### Task 3: Collapse the viewer loop and duplex pump

**Files:**
- Modify: `src/runtime/io.rs`
- Test: `tests/runtime_io.rs`

- [ ] Merge network/command/timeout handling into one select-driven transition loop.
- [ ] Use one pending-input record and one queue, removing mirrored `advance`, `lost`, `resumed`, and release bookkeeping.
- [ ] Keep `Duplex` as a bounded two-thread adapter, but unify constructor and accounting branches.
- [ ] Run `cargo test --test runtime_io --no-fail-fast` after each independent deletion.

### Task 4: Verify the constrained result

**Files:**
- Modify only if a proven regression exists: `src/runtime/holder.rs`, `src/runtime/io.rs`

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo test --test runtime_holder --test runtime_io --test lease --test wire --no-fail-fast`.
- [ ] Run `cargo check --all-targets` and `cargo check --target x86_64-pc-windows-gnu --all-targets`.
- [ ] Count nonblank, non-`//` lines after default rustfmt and require the two runtime files to total at most 1,200.
