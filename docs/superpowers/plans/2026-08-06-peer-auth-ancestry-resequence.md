# Peer Authentication and Live-Ancestry Resequencing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Authenticate every holder peer with an OS-derived PID, refuse unauthorized or over-capacity peers after only their four-byte dialect preface, and reject live descendant attach attempts before any attach mutation.

**Architecture:** Extend the holder admission record with authenticated PID and a deferred refusal, while preserving the one global sixteen-slot authentication/handshake budget and one sticky overflow response. Platform adapters supply fail-closed identity/PID evidence and bounded live-parent walks; the holder remains the single owner of refusal framing and attach ordering.

**Tech Stack:** Rust 2024, crossbeam/interprocess transports, Linux `/proc`, macOS local-socket/process APIs, Windows named pipes and Toolhelp, existing compiled-wire tests.

---

### Task 1: Characterize holder admission and attach ordering

**Files:**
- Modify: `tests/runtime_holder.rs`
- Modify: `tests/unit/runtime_holder.rs`
- Modify: `tests/windows.rs`

- [x] Add a fake native ancestry probe with counters and a generic runtime fixture.
- [x] Add tests proving wrong-user controller and semantic peers receive exact profile refusals only after the split four-byte preface, then close without consuming a lasting slot.
- [x] Add tests proving sixteen handshakes share one budget, exactly one sticky overflow peer receives the resource-exhausted refusal, existing peers survive, and peer-ID exhaustion never wraps.
- [x] Add a test proving a valid ancestral ATTACH is checked once and refused before descriptor queuing, resize, lease, terminal preamble, or replay; malformed attach must not invoke ancestry and unrelated attach remains unchanged.
- [x] Run the named tests and observe failures caused by the old `Runtime::accept`/`Native` contracts or premature close.

### Task 2: Implement holder-side bounded admission

**Files:**
- Modify: `src/runtime/holder.rs`

- [x] Add a default-false `Native::holder_ancestor(pid)` hook.
- [x] Store authenticated PID and optional deferred refusal on each initial peer.
- [x] Count authentication plus handshaking globally, reserving at most one overflow response; make `u64` peer allocation use checked exhaustion.
- [x] Read at most four preface bytes before selecting the profile. Discard any trailing unauthorized/overflow bytes, encode the exact controller or semantic refusal, then close.
- [x] Invoke live ancestry only after a valid ATTACH decode and before descriptor queuing or any policy/native/output mutation.
- [x] Run the focused holder tests to green and rerun the existing holder suite.

### Task 3: Implement Unix authenticated PID and live ancestry

**Files:**
- Modify: `src/unix.rs` only in `UnixNative`, holder acceptance, and peer identity/ancestry helpers
- Modify: `tests/unix_e2e.rs`

- [x] Add the shipped-binary self-attach test through an executable name containing `)` and observe the attach succeed or otherwise miss the required refusal before implementation.
- [x] Derive same-user and nonzero peer PID from socket credentials; on macOS obtain PID with `LOCAL_PEERPID`.
- [x] Parse Linux `/proc/<pid>/stat` from the last `)` and extract the state/PPID fields; on macOS query `proc_pidinfo`. Bound every live walk to 4096 parents and fail closed to non-ancestral on missing/cyclic data.
- [x] Feed PID and sticky capacity state into holder admission without touching artifact preparation/ownership types.
- [x] Run focused holder and Unix E2E tests to green.

### Task 4: Implement Windows fail-closed authentication and ancestry

**Files:**
- Modify: `src/windows.rs` only in named-pipe authentication, `HolderNative`, and holder acceptance
- Modify: `tests/windows.rs` only for current holder admission call sites

- [x] Make impersonation/token lookup/reversion return an error on any failure; never admit a connection after failed reversion.
- [x] Capture a nonzero named-pipe client PID only for the authenticated owner.
- [x] Add a bounded Toolhelp process snapshot and parent walk with explicit enumeration-error handling.
- [x] Share the sixteen slots across authentication workers and runtime handshakes, with at most one separately tracked sticky overflow worker.
- [x] Run Windows GNU and MSVC compile checks and the portable Windows tests.

### Task 5: Verify, review, and commit

**Files:**
- Verify all modified files

- [x] Run `cargo fmt --all -- --check`.
- [x] Run the focused mutation-proven tests and Linux shipped-binary self-attach test.
- [x] Run `cargo test --all-targets --no-fail-fast`.
- [x] Run strict Clippy with warnings denied.
- [x] Run Linux musl, macOS, Windows GNU, and Windows MSVC checks where installed; report unavailable targets precisely.
- [x] Review the diff specifically for later query precedence, geometry/redraw, resumed deadlines, completion ownership, storage/descriptor linearization, Unix artifact ownership, and Windows bootstrap preservation.
- [x] Request independent code review, fix every critical/important finding, rerun verification, then commit without merge or push.
