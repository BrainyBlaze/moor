# Moor release manual QA v1

This document is the executable release-team contract between a successful
`release-candidate.yml` run and `release-promote.yml`. “Manual QA” means the
release team supervises and records the complete product-level checks against
the candidate artifacts. It never means rebuilding a candidate locally, and no
item below requires an interactive keypress or an operator-only physical step.

## Governance preconditions

Before either release workflow is dispatched:

1. `main` is protected, force-push and deletion are disabled, and the existing
   hosted checks are required. Zero pull-request approvals are required; the
   check boundary makes `github.ref_protected` true without turning release
   execution into an approval queue.
2. The `release` environment exists. It has no required reviewer; it is the
   permission boundary used by both release workflows.
3. immutable releases are enabled for the repository.

The QA workflow has only `actions: read`, `contents: read`, and `issues: read`.
It cannot create a ref or release. The promotion workflow is the only workflow
with `contents: write`, and it refuses to run unless protected main, the release
environment, and immutable releases are all active.

## Candidate identity

Record all of these values from one successful attempt-1 candidate run:

- candidate workflow run ID and attempt;
- immutable `moor-release-candidate-v1` artifact ID;
- immutable `moor-release-candidate-record` artifact ID; and
- for each target, its immutable artifact ID, exact artifact name, release
  filename, byte length, and SHA-256 from the canonical manifest.

The v0.1.0 candidate is run `32003467728`, attempt `1`, source commit
`aa26a26f5308aa31091d143c6600d5f5dd1c1bf1`, metadata artifact `9279394300`,
and candidate-record artifact `9279395137`. Those numbers identify artifacts;
names alone do not.

## Team-executed checklist

All checks consume binaries downloaded by their immutable candidate artifact
IDs. A host may recompute the SHA-256 of the bytes it downloaded, but evidence
from a source rebuild or a same-named artifact is invalid.

The four platform verdicts, in this exact order, are:

1. `x86_64-unknown-linux-musl`
2. `aarch64-unknown-linux-musl`
3. `x86_64-apple-darwin`
4. `aarch64-apple-darwin`

Each verdict must be `passed` and cite a hosted Actions run or job URL that
consumed the canonical manifest and the exact candidate artifact. An issue,
pull request, commit, or other GitHub page is not run evidence. The complete
checklist, also in exact order, is:

1. `candidate-install` — Desk’s candidate install path consumes the canonical
   manifest and target tuple rather than compiling Moor.
2. `binary-identity` — each installed asset returns the expected basename and
   version from `moor --version`.
3. `v4-dialect` — a CRC-valid HELLO dialect `03` is refused and dialect `04` is
   accepted, without replacing the tested holder bytes.
4. `session-create` — provision and initial attach succeed against the real
   candidate holder.
5. `provider-identity` — Desk reports the expected Moor source/generation
   identity.
6. `resume-argv` — restart uses the recorded supervised launch arguments.
7. `resume-continuity` — the same committed session resumes without output or
   input discontinuity.
8. `resume-mismatch` — a mismatched or stale identity fails closed.
9. `rebind` — lease loss and exact-generation rebind preserve one authority.
10. `channel-delivery` — terminal output reaches every supported Desk channel
    without a legacy transport fallback.
11. `input-path` — input reaches the current lease holder exactly once.
12. `restart-geometry` — restart/adoption preserves geometry and applies the
    latest commanded resize once.
13. `restart-adoption` — Desk adopts the real restarted holder through the v4
    observer/authority path.

Each item must be `passed` and cite a hosted Actions run or job URL. One hosted
run may support several items when its job log proves each named behavior.

## Authenticated evidence comment

Post one unedited issue comment in `BrainyBlaze/moor` from the repository owner
account. Its entire body is one JSON object: do not wrap it in a Markdown code
fence or add prose. Objects use the exact keys shown below; arrays use the exact
orders above; unknown, missing, duplicate, failed, or reordered entries are
rejected.

```json
{
  "schemaVersion": 1,
  "repository": "https://github.com/BrainyBlaze/moor",
  "version": "v0.1.0",
  "commit": "aa26a26f5308aa31091d143c6600d5f5dd1c1bf1",
  "candidate": {
    "workflowRunId": "32003467728",
    "workflowRunAttempt": 1,
    "metadataArtifactId": "9279394300",
    "candidateRecordArtifactId": "9279395137"
  },
  "platforms": [
    {"target": "x86_64-unknown-linux-musl", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"target": "aarch64-unknown-linux-musl", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"target": "x86_64-apple-darwin", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"target": "aarch64-apple-darwin", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"}
  ],
  "checklist": [
    {"id": "candidate-install", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "binary-identity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "v4-dialect", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "session-create", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "provider-identity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "resume-argv", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "resume-continuity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "resume-mismatch", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "rebind", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "channel-delivery", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "input-path", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "restart-geometry", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"},
    {"id": "restart-adoption", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/desk/actions/runs/RUN_ID"}
  ],
  "confirmation": "APPROVE MOOR v0.1.0 aa26a26f5308aa31091d143c6600d5f5dd1c1bf1 32003467728/1 9279394300 9279395137 full-matrix"
}
```

The QA workflow fetches the comment by numeric ID, requires that its issue URL
matches the supplied Moor issue, its author is both the workflow actor and the
repository `OWNER`, and `created_at == updated_at`. It snapshots the exact body
bytes. Editing the comment invalidates the record; post a new comment instead.

## QA record and promotion

Dispatch `release-qa.yml` from protected `main` with the candidate run/attempt,
the two artifact IDs, and evidence issue/comment IDs. It re-downloads the
metadata, candidate record, and all four binaries by ID, verifies each artifact
name/run/source/shape/size/hash, validates the evidence, and uploads exactly:

- `moor-release-qa-v1.json`; and
- `manual-qa-evidence.txt`.

Record the resulting QA run ID, attempt `1`, and immutable QA artifact ID.

Dispatch `release-promote.yml` with those three values. Promotion re-fetches
the QA artifact and evidence comment, re-downloads the candidate artifacts by
their QA-bound IDs, and verifies everything again before creating a tag. It
then creates or resumes one draft release, uploads only missing final-name
assets, and rejects any unexpected or conflicting asset. Promotion never rebuilds, deletes, or overwrites
a release asset.

The published release contains the four binaries plus `moor-release-manifest-v1.json` and `SHA256SUMS`.
Promotion verifies all six while the release is still a draft, publishes it
once, then downloads all six again and verifies their sizes and hashes. An
exact rerun is idempotent. A
conflict, expired artifact, edited evidence comment, or nonexact tag requires a
new candidate and QA cycle; it is never repaired by substitution.

Publication is followed in the same delivery sweep by Desk’s schema-2 pin PR.
That PR copies the manifest’s full-matrix coverage and four target tuples
byte-for-byte, then proves `fetch-moor` and `install.sh` against the live release.
