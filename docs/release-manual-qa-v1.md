# Moor release manual QA v1

This document is the executable release-team contract between a successful
`release-candidate.yml` run and `release-promote.yml`. “Manual QA” means the
release team supervises and records the complete product-level checks against
the candidate artifacts. It never means rebuilding a candidate locally, and no
item below requires an interactive keypress or an operator-only physical step.

## Governance preconditions

Before the release workflows are dispatched:

1. `main` is protected, force-push and deletion are disabled, and the existing
   hosted checks are required. Zero pull-request approvals are required; the
   check boundary makes `github.ref_protected` true without turning release
   execution into an approval queue.
2. The `release` environment exists. It has no required reviewer; it is the
   permission boundary used by both release workflows.
3. A trusted repository administrator is available live during promotion with
   a clean protected-main checkout and an authenticated local `gh` session.

The candidate-QA, QA, and promotion workflows have only read repository
permissions. They cannot create a ref, release, asset, or issue comment. The
local administrator is the sole release mutator and runs one complete command
printed by the waiting promotion workflow. Actions accepts the resulting
canonical preflight and completion records, then independently verifies the
published immutable release and all six downloaded asset bytes.

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

Dispatch `release-candidate-qa.yml` from protected Moor `main` with the exact
candidate run/attempt and metadata and candidate-record artifact IDs. The exact
reviewed Desk commit is pinned in the protected workflow rather than accepted
as a dispatch input, so changing executable Desk bytes requires the same
reviewed-main transaction as changing the release tooling. The workflow checks
out that pinned Desk SHA, projects the candidate manifest into Desk pin schema
3, exercises `fetch:moor` against the downloaded candidate asset without
rebuilding it, and runs the product suites on `ubuntu-22.04`,
`ubuntu-24.04-arm`, `macos-15-intel`, and `macos-15`.

Each verdict must be `passed` and cite that exact hosted Moor candidate-QA
Actions run or one of its job URLs. An unrelated Actions run, issue, pull
request, commit, or other GitHub page is not run evidence. The complete
checklist, also in exact order, is:

1. `candidate-install` — Desk’s candidate install path consumes the canonical
   manifest and target tuple rather than compiling Moor.
2. `binary-identity` — each installed asset returns the expected basename and
   version from `moor --version`.
3. `v5-dialect` — a CRC-valid HELLO dialect `04` is refused and dialect `05` is
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
13. `restart-adoption` — Desk adopts the real restarted holder through the v5
    observer/authority path.

Each item must be `passed` and cite a hosted Actions run or job URL. One hosted
run may support several items when its job log proves each named behavior.

The all-green workflow emits a one-file
`moor-release-candidate-qa-evidence` artifact containing
`manual-qa-evidence.json`. Record its immutable artifact ID. The release team
posts those exact file bytes, unchanged and without a Markdown fence, as the
evidence comment described next.

## Authenticated evidence comment

Post one unedited issue comment in `BrainyBlaze/moor` from the same actor who
dispatches QA. The actor must have live `admin` permission on the Moor
repository at QA time; GitHub's observed author association (`MEMBER`, `OWNER`,
or another recognized association) is recorded verbatim but is not used as a
permission substitute. Its entire body is the producer artifact's JSON object:
do not wrap it in a Markdown code fence or add prose. Objects use the exact
keys shown below; arrays use the exact orders above; unknown, missing,
duplicate, failed, or reordered entries are rejected.

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
  "candidateQa": {
    "workflowRunId": "CANDIDATE_QA_RUN_ID",
    "workflowRunAttempt": 1,
    "deskCommit": "14e727bafe11a41e87a81a068c3ecbd3151fd2c8"
  },
  "platforms": [
    {"target": "x86_64-unknown-linux-musl", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"target": "aarch64-unknown-linux-musl", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"target": "x86_64-apple-darwin", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"target": "aarch64-apple-darwin", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"}
  ],
  "checklist": [
    {"id": "candidate-install", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "binary-identity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "v5-dialect", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "session-create", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "provider-identity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "resume-argv", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "resume-continuity", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "resume-mismatch", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "rebind", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "channel-delivery", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "input-path", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "restart-geometry", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
    {"id": "restart-adoption", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"}
  ],
  "confirmation": "APPROVE MOOR v0.1.0 aa26a26f5308aa31091d143c6600d5f5dd1c1bf1 32003467728/1 9279394300 9279395137 full-matrix"
}
```

The QA workflow fetches the comment by numeric ID, requires that its issue URL
matches the supplied Moor issue, its author is the workflow actor with live
repository `admin` permission, and `created_at == updated_at`. It downloads the
candidate-QA evidence artifact by immutable ID and requires the comment body to
be byte-identical to `manual-qa-evidence.json`. Editing the comment invalidates
the record; post a new comment instead.

## QA record and promotion

Dispatch `release-qa.yml` from protected `main` with the candidate run/attempt,
the two candidate artifact IDs, candidate-QA run/attempt and evidence artifact
ID, and evidence issue/comment IDs. It re-downloads the metadata, candidate
record, candidate-QA evidence, and all four binaries by ID, verifies each
artifact name/run/source/shape/size/hash, validates the evidence, and uploads
exactly:

- `moor-release-qa-v1.json`; and
- `manual-qa-evidence.txt`.

Record the resulting QA run ID, attempt `1`, and immutable QA artifact ID.
The canonical QA record embeds that same run ID and attempt; promotion requires
both embedded values to equal its dispatched QA run inputs before reconstructing
and accepting the record.

Generate a fresh nonce with 32 random bytes encoded as 64 lowercase hex, then
dispatch `release-promote.yml` in `promote` mode with the QA run ID, QA
attempt `1`, QA artifact ID, `promotion_issue_number`, and
`promotion_nonce`. Every promotion execution is attempt `1`: never rerun a failed promotion attempt.
Recovery always uses a new workflow dispatch and a
new nonce.

Promotion re-fetches the QA artifact and evidence comment, re-downloads the
candidate artifacts by their QA-bound IDs, verifies everything again, creates a
canonical `promotion-manifest.json`, and uploads a closed
`moor-release-promotion-v1` bundle containing only that manifest and the six
release files. It authenticates the uploaded artifact's numeric ID and REST API
digest before opening the gate.

The named gate prints one complete, pasteable command:

```bash
python3 scripts/release-admin-promote.py promote \
  --repository BrainyBlaze/moor \
  --promotion-run-id <run-id> --promotion-run-attempt 1 \
  --head-sha <protected-main-sha> \
  --source-artifact-id <artifact-id> \
  --source-artifact-name moor-release-promotion-v1 \
  --source-api-digest sha256:<digest> \
  --issue-number <promotion-issue> \
  --dispatcher <admin-login> \
  --gate-ready-at <utc-second> \
  --nonce <64-lowercase-hex> \
  --transaction-root "$HOME/.local/state/moor/release-<run-id>-1"
```

Paste that command from a clean checkout whose `HEAD` is the displayed
protected-main SHA. Do not split it into manual API calls and do not use the
workflow's `GITHUB_TOKEN`. The helper uses the existing authenticated local
`gh` session without reading or printing its token. Before any release
mutation it proves:

1. local `HEAD` and cleanliness;
2. authenticated login equals the dispatcher;
3. current repository `admin` permission;
4. OAuth scopes include `repo` and `workflow`;
5. local UTC agrees with GitHub's HTTPS `Date` header;
6. `GET /repos/{owner}/{repo}/immutable-releases` with
   `X-GitHub-Api-Version: 2026-03-10` and
   `Accept: application/vnd.github+json` returned the documented exact response bytes
   `{"enabled":true,"enforced_by_owner":false}` and
   `enabled == true`;
7. the closed bundle, canonical manifest, and all six files match the
   workflow-bound artifact ID and digest; and
8. the attempt-1 workflow is live in the named preflight wait.

The helper posts a canonical preflight comment at byte zero, waits until Actions
accepts that exact comment and enters the named completion wait, and rechecks
the live gate before every tag, draft, asset, deletion, and publish mutation.
Actions polls the preflight record for at most 30 minutes and the completion
record for at most 60 minutes. Both records require exact canonical bytes,
`created_at == updated_at`, a fresh nonce, the complete run/QA/source/manifest
tuple, current administrator identity, bounded timestamps, and the exact
immutable-settings response digest.

The release transaction creates or adopts the exact lightweight tag and exact
deterministic draft, then delegates every asset decision to
`release-asset-transaction.py`. Promotion never rebuilds or overwrites a
release asset. The single permitted deletion is an expected-name asset whose
fresh state is `starter` on the exact transaction-bound draft. Immediately
before deletion the helper rechecks release and asset numeric identities; it
replans afterward and permits at most two starter deletions per name. Uploaded
assets, unexpected names, tags, releases, and every asset on a published release
are never deleted.

Only an exact six-asset draft may be published. The draft contains the four binaries plus `moor-release-manifest-v1.json` and `SHA256SUMS`.
Immediately before publication
the helper rechecks the live completion step, accepted preflight, tag, release,
downloaded asset bytes, administrator authority, GitHub time, and immutable
settings. It performs one publish transition, requires a fresh read with
`draft == false` and `immutable == true`, re-downloads all six assets, freezes
the local transaction-evidence manifest, and posts one canonical completion
comment.

The preflight and completion comments are persistent but not immutable. Actions
therefore treats the completion record only as a signal: it independently
re-fetches both exact comments and live administrator permission, resolves the
tag and deterministic release metadata, requires the published immutable state,
downloads all six assets, compares their IDs, sizes, and SHA-256 values to the
canonical manifest and completion record, and re-fetches the same completion
comment once more before success.

If Actions stops after publication, dispatch a fresh attempt-1
`verify-published` run with the original promotion run ID and accepted
preflight/completion comment IDs. That mode is read-only: it authenticates the
prior bundle and records and repeats the complete published proof. If the exact
immutable release exists without an acceptable completion record, use a fresh
`promote` dispatch and nonce; the helper adopts the published release with zero
release mutations and creates a new receipt.

GitHub exposes no conditional publish covering every trusted administrator.
The helper's repeated checks minimize but cannot eliminate the
preflight-to-publication race: another trusted repository administrator can
change state between the final check and publication. A mutable public release
in that interval is postpublication detection, not prepublication fail-closed prevention.
Freeze all writers and investigate; never rerun the failed attempt.
An immutable published release cannot be rolled back or repaired.

Publication is followed in the same delivery sweep by Desk’s pin schema 3 PR.
That PR copies the manifest’s full-matrix coverage and four target tuples
byte-for-byte, then proves `fetch-moor` and `install.sh` against the live release.
