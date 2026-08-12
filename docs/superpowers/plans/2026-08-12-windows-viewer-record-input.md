# Windows Viewer Record-Input Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shipped Windows viewer preserve ordered text, Unicode, resize, and detach input, then prove the change at merge time on hosted x64/ARM64 and at release time across the full frozen Windows/WSL matrix.

**Architecture:** Keep the existing batched `ReadConsoleInputW` reader as the single input authority. While attached, configure the console for low-level record input by clearing `ENABLE_VIRTUAL_TERMINAL_INPUT`, retaining `ENABLE_WINDOW_INPUT` and `ENABLE_EXTENDED_FLAGS`, and clearing processed/line/echo/quick-edit modes; keep VT output enabled and restore both original modes on drop.

**Tech Stack:** Rust, Win32 Console API, ConPTY, GitHub Actions native Windows runners.

---

### Task 1: Specify low-level record mode in tests

**Files:**
- Modify: `tests/unit/windows_security.rs:59-83`
- Modify: `tests/windows.rs:1020-1450`

- [ ] **Step 1: Make the unit test require VT input to be cleared**

Include `ENABLE_VIRTUAL_TERMINAL_INPUT` in the input passed to `viewer_modes`, then replace the current nonzero assertion with:

```rust
assert_eq!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
assert_eq!(
    input & (ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS),
    ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS
);
```

Retain the existing assertions that processed, line, echo, and quick-edit input are cleared and that processed/VT output remains enabled.

- [ ] **Step 2: Make the shipped-process test assert the same contract**

Remove the `CSI ?9001h` wait because that sequence is specific to VT input mode and is absent on the Server 2022 console host. Change the live-mode assertion to require:

```rust
assert_eq!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
assert_eq!(
    input & (ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS),
    ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS
);
```

Replace the encoded win32-input-mode detach fixture with the user-visible raw detach byte:

```rust
console.write(&[0x1c]).unwrap();
```

Keep the existing real-process assertions for `A -> resize(41,101) -> B`, successful viewer detach, and exact restoration of the original console modes. This legacy Console API child proves record/resize ordering.

- [ ] **Step 3: Split exact-byte translation proof from shipped record-semantics proof**

Add a Windows unit test at Moor's `ConsoleInput::record` / `ConsoleInput::encode` boundary. Feed adjacent key-down, repeat-one input records for `A`, UTF-16 `D83D DE42` (🙂), `00E9` (é), semantic NUL, and `Z`, encode with CP65001, and require the complete vector, including length, to equal:

```rust
b"A\xf0\x9f\x99\x82\xc3\xa9\0Z"
```

This unit test is the exact-byte proof. Do not use ANSI `ReadFile` or `ReadConsoleA` in the real child: the console's ANSI input path obtains one UTF-16 unit through `GetChar` and converts each call separately, so a supplementary scalar split across `D83D DE42` cannot be required to survive that boundary as one UTF-8 sequence.

Add a separate Windows-only helper test in `tests/windows.rs` that runs as the real session child and:

1. selects CP65001;
2. configures its inner ConPTY input for low-level records by clearing `ENABLE_VIRTUAL_TERMINAL_INPUT`, processed, line, echo, and quick-edit input while preserving `ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS`;
3. uses only bounded `WaitForSingleObject` plus `ReadConsoleInputW` calls;
4. accepts exactly the ordered semantic UTF-16 sequence `0041 D83D DE42 00E9 0000 005A`, requiring every accepted record to be key-down with repeat one and rejecting wrong or extra semantic records. At the inner-console compatibility boundary only, normalize either the newer exact `VkKeyScanW(0)` NUL chord or the historical exact raw-zero record used by Windows 10 1809 and Server 2019 conhost.

Add an outer-console sender helper that issues one `WriteConsoleInputW` batch containing those adjacent key-down records. It must assert that every record was written, then create a sender-complete sentinel file whose path is passed by environment. Spawn it in the same outer pseudoconsole after the Moor viewer reports ready, and pump the outer ConPTY output while waiting for the bounded sender exit so sender panic diagnostics are retained.

Construct the sender's semantic NUL exactly as `console_wide_with_nul` does: call `VkKeyScanW(0)`, use mapping bits 0..7 for `wVirtualKeyCode`, and normalize mapping bits 8/9/10 against shift/control/alt state. The sender sets `bKeyDown=TRUE`, `wRepeatCount=1`, and `uChar.UnicodeChar=0`. This includes the documented `-1` mapping bit-pattern case; do not substitute a raw zero record at the Moor input boundary. Microsoft Terminal commit `8747a39` added the chord reconstruction on 2023-11-27, so the real-child receiver must additionally accept the older conhost's exact raw-zero representation (`VK=0`, scan=0, controls=0) without weakening Moor's exact semantic-NUL unit proof.

After accepting `Z`, the child must wait for the sender-complete sentinel, then continue bounded record reads until a fixed 500 ms quiet deadline. Drain and ignore non-character records, but fail on every additional semantic input record, including duplicates and reordered suffixes. The sentinel proves the sender did not merely pause mid-batch.

The parent must assert sender and viewer success and the real session child's zero exit status. Together, the unit test and shipped-process fixture prove `ReadConsoleInputW -> UTF-16 scalar assembly -> exact CP65001 bytes -> Moor wire -> inner low-level semantic records` without making an invalid exact-byte claim across the lossy ANSI console input boundary.

- [ ] **Step 4: Run local formatting and cross-target compile checks**

Run:

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo check --tests --target x86_64-pc-windows-gnu
git diff --check
```

Expected: all commands exit 0. These prove syntax and cross-target compilation only; they do not replace native execution.

- [ ] **Step 5: Commit and push the RED tests**

```bash
git add tests/unit/windows_security.rs tests/windows.rs
git commit -m "test: require Windows console record input"
git push origin codex/issue21-windows
```

- [ ] **Step 6: Verify the native RED state**

Inspect the exact-SHA `Hosted native evidence` run. Expected before the production change: Windows native tests fail because `viewer_modes` still leaves `ENABLE_VIRTUAL_TERMINAL_INPUT` set; all unrelated lanes remain green. Record the exact failing test names and SHA rather than treating a compile-only result as RED evidence.

### Task 2: Configure the viewer for record input

**Files:**
- Modify: `src/windows.rs:2027-2038`

- [ ] **Step 1: Clear VT input in the mode transformation**

Change the input expression to:

```rust
(input | ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT)
    & !(raw | ENABLE_VIRTUAL_TERMINAL_INPUT)
```

Do not alter the output expression, `ConsoleInput`, `ViewerConsole::drop`, or the record batching/Unicode/resize logic.

- [ ] **Step 2: Run local focused and full host checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo check --tests --target x86_64-pc-windows-gnu
git diff --check
```

Expected: all commands exit 0. The Windows-only behavioral tests remain gated on native execution.

- [ ] **Step 3: Commit and push the minimal production fix**

```bash
git add src/windows.rs
git commit -m "fix: use Windows console record input"
git push origin codex/issue21-windows
```

- [ ] **Step 4: Gate the merge candidate on exact hosted native evidence**

Require both `Native hosted / windows-2022-x64` and `Native hosted / windows-11-arm64` to pass the full real-process suite at the exact production-fix SHA. The suite must include the legacy record/resize child, the inner record-mode semantic child, and the exact-byte translation-boundary unit test. Also require all six POSIX/musl/macOS lanes to remain green.

This is merge-candidate regression evidence, not the full §12.8 release-conformance claim: Server 2022 is not a substitute for required Server 2019 or Windows 10 1809, and the hosted workflow supplies neither WSL1 nor WSL2.

- [ ] **Step 5: Gate on exact quality evidence**

Require format, strict Clippy, all-target tests, release build, and production-source report to pass. A frozen-spec digest failure may be resolved only by the separately reviewed §12.2 amendment and its ratified digest update; never weaken the hash gate here.

### Task 3: Review the branch for merge readiness

**Files:**
- Review: `src/windows.rs`
- Review: `tests/windows.rs`
- Review: `tests/unit/windows_security.rs`
- Review: `.github/workflows/native-hosted.yml`
- Review: `.github/workflows/quality.yml`

- [ ] **Step 1: Request adversarial code review at the exact green SHA**

The reviewer must check the mode/API pairing, raw detach behavior, Unicode and resize ordering, original-mode restoration, and that no diagnostic-only handshake dependency remains.

- [ ] **Step 2: Re-run branch cleanliness and evidence checks**

Run:

```bash
git status --short
git diff --check origin/main...HEAD
git log -1 --format=%H
```

Expected: clean worktree, no whitespace errors, and the printed SHA exactly matches both green hosted workflows.

- [ ] **Step 3: Publish the evidence ledger**

Post the exact SHA, both hosted workflow run URLs, Windows test counts, and reviewer disposition to `#desk-moor`. Do not merge until the §12.2 amendment and its hash gate are green. Hosted evidence may establish merge readiness; it does not authorize a release tag.

### Task 4: Execute the deferred full release-conformance workflow

This task runs after the reviewed #21 branch is merged. Unavailable required runners block the release/tag, not the preceding hosted-evidence merge.

**Files:**
- Modify if necessary: `.github/workflows/native-self-hosted.yml`
- Review: `.github/workflows/native-hosted.yml`
- Review: `tests/windows.rs`
- Create: `docs/native-conformance-matrix.md`

- [ ] **Step 1: Commit the static requirement-to-test/lane matrix**

Create `docs/native-conformance-matrix.md` from the normative matrix and scenario list at `spec/moor-spec.md:1490-1502`. Map each required create/attach/detach/input/replay/termination, lease, query, clear, peer, fencing, staging, recovery, crash-prefix, scanner, vector, restart, and Windows-hostile scenario to a shipped-process test and required lane. Do **not** put the candidate SHA, job URLs, run results, or artifact hashes in this tracked file: doing so would change the candidate it describes.

Commit and push this static matrix and any necessary workflow changes before declaring the release-candidate SHA. The resulting commit, after it is clean and reviewed, becomes the candidate whose artifacts are tested.

- [ ] **Step 2: Preserve exact shipped-artifact identity across every lane**

Every dynamic result must name that same release-candidate commit and archive the tested `moor` binary, SHA-256, build log, test log, smoke log, and runner identity. A rebuild on a different commit, compile-only check, queued/skipped/cancelled job, or library-only test is not evidence. Store dynamic SHA/job/artifact results in immutable CI artifacts and publish their URLs externally; never amend the tracked matrix with self-referential results.

- [ ] **Step 3: Run every frozen minimum Windows/WSL lane**

Execute at the exact candidate SHA:

```text
Windows 10 1809 x64       legacy + record-mode semantic child + exact-byte unit proof
Windows Server 2019 x64   legacy + record-mode semantic child + exact-byte unit proof
Windows 11 ARM64          legacy + record-mode semantic child + exact-byte unit proof
WSL1 Ubuntu 22.04 x64     shipped-artifact real-process suite
WSL2 Ubuntu 22.04 x64     shipped-artifact real-process suite
```

Hosted Windows Server 2022 x64 remains useful extra regression evidence but does not replace either required x64 native-Windows lane. Hosted Windows 11 ARM64 may satisfy that required lane only when its archived identity proves the required OS/architecture and the complete applicable scenario set passes.

- [ ] **Step 4: Verify the dynamic results against the static matrix**

For every row in `docs/native-conformance-matrix.md`, require an immutable exact-SHA CI artifact from every stated lane. A missing scenario or lane narrows the support claim and blocks the tag; it may not be waived on paper.

- [ ] **Step 5: Publish artifacts and block release tagging until green**

Post exact run/job URLs and artifact hashes to `#desk-moor`. If unavailable self-hosted hardware prevents a lane, report the release as blocked and provision a real VM/runner (for example a KVM-backed Windows guest) rather than substituting Wine, cross-compilation, or a unit test. Tagging remains blocked until this full matrix and full manual QA across both Moor and Desk are complete.
