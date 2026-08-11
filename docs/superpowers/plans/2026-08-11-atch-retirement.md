# `atch` Compatibility-Name Retirement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `atch` as a built, shipped, tested, and documented compatibility command while preserving generic invoked-basename behavior.

**Architecture:** Retire the named product at manifest, automation, regression, and normative-contract boundaries without changing production Rust. Cargo metadata becomes the executable-target authority, CI packages only `moor`, and the behavioural-spec digest is updated atomically with the amendment.

**Tech Stack:** Rust/Cargo, GitHub Actions YAML, POSIX shell, PowerShell, Markdown, SHA-256.

---

### Task 1: Make `moor` the sole Cargo binary

**Files:**
- Modify: `Cargo.toml:37-44`
- Modify: `tests/cli.rs:357-372`
- Modify: `.github/workflows/quality.yml:65-68`

- [ ] **Step 1: Run the sole-binary assertion and verify the red state**

Run:

```bash
test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "moor") | .targets[] | select(.kind == ["bin"]) | .name')" = moor
```

Expected: exit 1 because the current metadata prints both `atch` and `moor`.

- [ ] **Step 2: Remove the compatibility bin target**

Delete this block from `Cargo.toml`:

```toml
[[bin]]
name = "atch"
path = "src/main.rs"
```

Retain the existing `moor` target unchanged.

- [ ] **Step 3: Preserve generic renamed-copy coverage without naming `atch`**

In `tests/cli.rs`, rename the test to
`invoked_renamed_copy_name_drives_help_and_diagnostics`, create the symlink as
`moor-copy`, and expect `moor-copy <version>\n`. Do not change the underlying
`name::program` or basename-derived root/environment logic.

- [ ] **Step 4: Add the semantic target gate and singular release build**

Replace the final quality step with:

```yaml
      - name: Verify and build the sole release command
        run: |
          test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "moor") | .targets[] | select(.kind == ["bin"]) | .name')" = moor
          cargo build --release --bin moor
```

- [ ] **Step 5: Verify the green state**

Run:

```bash
test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "moor") | .targets[] | select(.kind == ["bin"]) | .name')" = moor
cargo test --test cli invoked_renamed_copy_name_drives_help_and_diagnostics -- --exact
cargo build --release --bin moor
git diff --check
```

Expected: all commands exit 0; Cargo no longer warns that `src/main.rs` belongs
to multiple binary targets.

- [ ] **Step 6: Commit the executable-surface change**

```bash
git add Cargo.toml tests/cli.rs .github/workflows/quality.yml
git commit -m "build: retire atch command target"
```

### Task 2: Package and smoke only `moor`

**Files:**
- Modify: `.github/workflows/native-hosted.yml:59-86,141-169,226-240`
- Modify: `.github/workflows/native-self-hosted.yml:176-205,334-361`

- [ ] **Step 1: Convert hosted POSIX packaging to one command**

Rename step labels from “both release command names” to “the release command”.
Build only `--bin moor`, copy only `target/release/moor`, hash only `moor`, and
remove both `atch` smoke invocations.

- [ ] **Step 2: Convert hosted Windows packaging to one command**

Build only `--bin moor`, copy only `moor.exe`, iterate over only
`@("$out/moor.exe")` when hashing, and remove both `atch.exe` smoke invocations.

- [ ] **Step 3: Convert the Alpine container packaging to one command**

Build, copy, hash, and smoke only `moor`; leave the Alpine identity, tests,
artifact upload, and retention behavior unchanged.

- [ ] **Step 4: Convert every self-hosted platform lane**

Apply the same singular build/package/hash/smoke behavior to POSIX/WSL and
Windows self-hosted jobs. Do not alter runner probes, test commands, evidence
paths, or artifact retention.

- [ ] **Step 5: Verify active automation has no `atch` surface**

Run:

```bash
! rg -n '(^|[^[:alnum:]_])atch([^[:alnum:]_]|$)|ATCH_' Cargo.toml tests .github
git diff --check
```

Expected: both commands exit 0. Retirement design/plan documents are excluded
because they intentionally record what was removed.

- [ ] **Step 6: Commit release-automation cleanup**

```bash
git add .github/workflows/native-hosted.yml .github/workflows/native-self-hosted.yml
git commit -m "ci: package only the moor command"
```

### Task 3: Ratify the single-name behavioural contract

**Files:**
- Modify: `spec/moor-spec.md:7-8,1065-1068`
- Modify: `spec/README.md:35-40`
- Modify: `.github/workflows/quality.yml:53-63`
- Modify: `docs/superpowers/plans/2026-08-04-moor-implementation.md:35`

- [ ] **Step 1: Amend the distribution-name requirement**

Replace the top-level “install both names” paragraph with a requirement that the
distribution installs only `moor`. State that a user-created renamed copy still
derives a separate root and environment keys from its invoked basename and is
not a packaged compatibility entrypoint.

- [ ] **Step 2: Generalize the generation-key examples**

Replace the two `atch`/`ATCH_*` examples in §10.1 with a generic `moor-copy`
example and `<BASENAME>_*` wording. Preserve the actual transformation,
truncation, and identity requirements.

- [ ] **Step 3: Update the active implementation plan**

Change Task 5’s packaging sentence to require packaging/testing the sole `moor`
executable while retaining the arbitrary renamed-copy CLI regression.

- [ ] **Step 4: Compute the amended behavioural-spec digest**

Run:

```bash
sha256sum spec/moor-spec.md
```

Record the exact 64-hex result. Do not change `spec/moor-wire-schema.md` or its
digest.

- [ ] **Step 5: Update both integrity anchors atomically**

Replace the old `moor-spec.md` digest in `spec/README.md` and in both occurrences
inside `.github/workflows/quality.yml`. Leave the wire-schema digest unchanged.

- [ ] **Step 6: Verify document integrity and active-surface removal**

Run from the repository root:

```bash
(cd spec && sha256sum moor-spec.md moor-wire-schema.md)
```

Compare both outputs byte-for-byte with the table in `spec/README.md` and the
quality workflow. Then, from the repository root, run the active-surface gate
and the complete classified inventory:

```bash
! rg -n '(^|[^[:alnum:]_])atch([^[:alnum:]_]|$)|ATCH_' Cargo.toml tests .github spec/moor-spec.md docs/superpowers/plans/2026-08-04-moor-implementation.md
rg -n --hidden -g '!target/**' -g '!.git' -g '!.git/**' '(^|[^[:alnum:]_])atch([^[:alnum:]_]|$)|ATCH_' .
git diff --check
```

Expected: the active-surface gate and whitespace check exit 0. The complete
inventory contains only the approved retirement design and implementation plan,
where `atch` names the historical surface being removed; classify every match
explicitly rather than relying on a zero-match assertion.

- [ ] **Step 7: Commit the ratified amendment**

```bash
git add spec/moor-spec.md spec/README.md .github/workflows/quality.yml docs/superpowers/plans/2026-08-04-moor-implementation.md
git commit -m "docs: ratify the sole moor command name"
```

### Task 4: Verify, integrate, and close issue #25

**Files:**
- Verify only: all files changed above

- [ ] **Step 1: Run focused acceptance checks**

```bash
test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "moor") | .targets[] | select(.kind == ["bin"]) | .name')" = moor
cargo test --test cli invoked_renamed_copy_name_drives_help_and_diagnostics -- --exact
cargo build --release --bin moor
scripts/count-production-loc.sh
```

Expected: all exit 0 and production LOC is no greater than the 11,500-line
pre-change baseline. The separate normative 4,900-line issue remains open.

- [ ] **Step 2: Run local quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
git diff --check
```

Expected: all commands exit 0. If the pre-existing parallel Unix E2E stale-
classification flake recurs, preserve its output, run the complete suite once
with `-- --test-threads=1` for deterministic comparison, and report both results;
do not describe the normal-concurrency suite as passing.

- [ ] **Step 3: Review the complete branch diff**

```bash
git diff --stat main...HEAD
git diff --check main...HEAD
git log --oneline main..HEAD
```

Expected: only the approved design/plan, Cargo target, atch-specific regression,
current contract/integrity anchors, and release workflows changed; no production
Rust source changed.

- [ ] **Step 4: Rebase onto current `origin/main` and re-run affected gates**

Run:

```bash
git fetch origin
git rebase origin/main
```

Resolve only genuine non-overlapping changes, then repeat Tasks 3.6, 4.1, and
4.2. Claude’s issue #17 branch may modify `tests/cli.rs`; preserve both its
launch regressions and this plan’s generic renamed-copy regression. Expected:
the branch is based directly on current `origin/main` and every repeated gate
has the same result as before the rebase.

- [ ] **Step 5: Fast-forward `main`, push, and observe exact-SHA CI**

From the isolated worktree, record the candidate SHA:

```bash
git rev-parse HEAD
```

Then fast-forward and push through the primary checkout:

```bash
git -C /home/dev/projects/desk/moor fetch origin
git -C /home/dev/projects/desk/moor merge --ff-only origin/main
git -C /home/dev/projects/desk/moor merge --ff-only codex/issue25-atch-retirement
git -C /home/dev/projects/desk/moor push origin main
git -C /home/dev/projects/desk/moor rev-parse HEAD origin/main
```

Expected: the remote update is first incorporated without rewriting it, the
feature branch then fast-forwards cleanly, and the final two hashes equal the
recorded candidate SHA. Poll until GitHub registers both exact-SHA workflows,
then wait for each:

```bash
candidate=$(git rev-parse HEAD)
for attempt in $(seq 1 30); do
  runs=$(gh run list --commit "$candidate" --limit 10 --json databaseId,workflowName,status,conclusion,headSha,url)
  test "$(printf '%s' "$runs" | jq '[.[] | select(.workflowName == "Hosted quality" or .workflowName == "Hosted native evidence")] | length')" -ge 2 && break
  sleep 2
done
printf '%s\n' "$runs"
gh run watch <quality-run-id> --exit-status
gh run watch <native-run-id> --exit-status
```

Expected: both `Hosted quality` and `Hosted native evidence` complete with
`success` at the exact candidate SHA.

- [ ] **Step 6: Close issue #25 with evidence**

Comment with the exact SHA, sole-bin Cargo metadata result, active-surface
inventory, unchanged production LOC, specification digest, and hosted workflow
URLs. Close only after both exact-SHA workflows succeed.
