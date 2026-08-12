# Windows Contract and Evidence Closure Implementation Plan

> **For agentic workers:** Use `superpowers:test-driven-development` for each behavior change and `superpowers:verification-before-completion` before freezing a candidate.

**Goal:** Close the remaining Windows lifecycle, handle-identity, console-input, and native-evidence gaps with one reviewed commit whose implementation, frozen contract, shipped-process tests, and native results agree.

**Architecture:** Keep ConPTY as the UTF-8 transport and the Windows console as the keyboard/mode translator. The viewer negotiates win32-input-mode on its own terminal, reads ordered `INPUT_RECORD` batches, keeps resize records out of band, and recognizes only the canonical win32 keyboard carrier needed to apply the existing detach-byte state machine. Artifact creation and rollback remain bound to pinned handles. Terminal-close callbacks wake the normal holder path and retain a shorter Windows-owned deadline than Ctrl-C/Break.

**Tech Stack:** Rust, Win32 Console and NT file APIs, ConPTY, KVM-backed native Windows guests, GitHub Actions, SHA-256-pinned Markdown specifications.

---

### Task 1: Ratify the behavior that native Windows can prove

**Files:**

- Modify: `spec/moor-spec.md`
- Modify: `spec/README.md`
- Modify: `.github/workflows/quality.yml`
- Modify: `docs/release-matrix.md`

- [ ] Name `ESC[?9001h` / `ESC[?9001l` as viewer-side input negotiation and restoration controls attempted on every supported Windows host.
- [ ] Limit full input fidelity to natively proven capable hosts while preserving the explicitly listed older-host compatibility subset.
- [ ] Give `CTRL_CLOSE_EVENT`, `CTRL_LOGOFF_EVENT`, and `CTRL_SHUTDOWN_EVENT` the two-second force / approximately five-second durable-retirement schedule imposed by Windows; preserve the five-/ten-second Ctrl-C/Break schedule.
- [ ] Freeze the sole input parser exception: canonical six-field `Vk;Sc;Uc;Kd;Cs;Rc_` keyboard carriers, 7-bit or UTF-8 C1 CSI, bounded to 64 bytes and one 50 ms monotonic candidate lifetime, solely for detach semantics.
- [ ] Define carrier atomicity, key-up/modifier shielding, characterless-key handling, exact NUL discrimination, repeat-count parity, byte-exact malformed fallback, and `-E` bypass.
- [ ] Recompute the Moor-spec SHA-256 and update both authoritative pins atomically; keep the wire digest unchanged.

### Task 2: Replace hollow fixtures with shipped-process regressions

**Files:**

- Modify: `tests/windows.rs`
- Modify: `tests/unit/windows_security.rs`
- Modify: `tests/unit/runtime_io.rs`

- [ ] Inject `A`, application-cursor Up, `Z`, and the default detach record through one real `WriteConsoleInputW` call in the viewer's outer pseudoconsole.
- [ ] Require the VT-native requested child to observe exactly `A ESC O A Z`, then require a 500 ms no-suffix interval so a delayed duplicate cannot pass.
- [ ] Require the real viewer to detach with status 0, emit `ESC[?9001l`, leave the session live, restore the original console modes, and retire only after explicit child release.
- [ ] Keep geometry evidence separate: assert ordered `A -> ResizePseudoConsole(41,101) -> B`, then detach using the raw byte path.
- [ ] Synchronize terminal-close timing at the actual `WM_CLOSE` post and prove graceful and ignoring-child durable retirement through shipped binaries.
- [ ] Exercise publication races, post-rename identity validation, relative event operands, pinned event/stderr/instrument handles, semantic-token freshness, instrumentation rejection, and durable fast prepublication exit.
- [ ] Mutation-test missing detach, missing mode negotiation, and premature terminal-close force so the strengthened tests have demonstrated RED evidence.

### Task 3: Implement the narrow carrier and rollback fixes

**Files:**

- Modify: `src/runtime/io.rs`
- Modify: `src/windows.rs`
- Modify: `tests/unit/runtime_io.rs`
- Modify: `tests/unit/windows_security.rs`

- [ ] Add typed input frames so carrier syntax is never searched as raw detach bytes and ordinary `InputState::Bytes` retains the existing byte-by-byte semantics.
- [ ] Decode all six canonical fields with their frozen ranges; distinguish semantic NUL from navigation/function keys and classify only key-up/modifier-only records as transparent metadata.
- [ ] Treat a carrier as one atomic occurrence, process `Rc=N` in order, forward each completed pair as an equivalent `Rc=1` carrier, and forward a different next key whole before detaching.
- [ ] Batch adjacent carrier frames to avoid one receipt-gated session write per key while preserving byte/resize/error order.
- [ ] Track one monotonic 50 ms deadline from the first candidate byte; slow fragments must not restart it. Replay malformed, incomplete, overlong, resize-interrupted, and error-interrupted candidates byte-exact.
- [ ] Serialize accepted UTF-16 console scalars as UTF-8 regardless of the outer legacy code page; preserve surrogate, NUL, and repeat semantics.
- [ ] On unpublished rollback, keep the exact owned store-directory handle pinned and retry its disposition briefly while already-issued slot deletions become visible under filesystem load.

### Task 4: Freeze and prove one exact candidate

**Files:**

- Review every path above.

- [ ] Format the crate and the two `include!`-based unit-test files; require `git diff --check` to pass.
- [ ] Run host all-target tests and strict Clippy, then cross-Windows all-target strict Clippy with the pinned GNU linker setup.
- [ ] On real Server 2022 x64, run the complete Windows library suite and the complete shipped-process integration suite from the rebuilt candidate bytes. Stress the publication-conflict rollback concurrently.
- [ ] Create a clean candidate commit before recording final hashes. Any later source, test, spec, workflow, or plan change creates a new candidate and restarts exact-SHA gates.
- [ ] Rebuild and hash the Windows binary, library harness, integration harness, and instrumentation DLL from that exact commit; verify guest copies before execution.
- [ ] Require hosted Windows 11 ARM64 native evidence for the same applicable suite before merge approval. Treat Server 2019/Win10 1809 as their frozen below-input-floor subset and never infer an unexecuted lane.
- [ ] Obtain final adversarial review of contract/implementation agreement, handle-bound rollback, default detach, carrier bounds, lifecycle residue, mode restoration, and native logs.
- [ ] Push and merge only the reviewed SHA. Do not create or move a release tag until the separate full manual QA promotion gate succeeds.

Final local checks:

```bash
cargo fmt --all -- --check
rustfmt --edition 2024 --check tests/unit/runtime_io.rs tests/unit/windows_security.rs
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets --all-features -- -D warnings
git diff --check
git status --short
git log -1 --format=%H
```
