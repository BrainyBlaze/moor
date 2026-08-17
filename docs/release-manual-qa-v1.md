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
3. A trusted repository administrator is available live during promotion to
   read the immutable-release setting and post the run-bound attestation below.

The candidate-QA and QA workflows have only read permissions. They cannot
create a ref or release. The promotion workflow is the only workflow with
`contents: write`. It refuses to mutate unless protected main and the release
environment are active and a fresh run-bound administrator attestation proves
the repository returned an enabled immutable-release setting.

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
    {"id": "v4-dialect", "verdict": "passed", "evidence": "https://github.com/BrainyBlaze/moor/actions/runs/CANDIDATE_QA_RUN_ID"},
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
dispatch `release-promote.yml` with the QA run ID, QA attempt `1`, QA artifact
ID, the Moor issue number that will hold the attestation, and that fresh nonce.
Every promotion execution is attempt `1`: never rerun a failed promotion attempt.
Recovery always uses a new workflow dispatch and a new nonce.

Promotion first re-fetches the QA artifact and evidence comment, re-downloads
the candidate artifacts by their QA-bound IDs, and verifies everything again.
After the last read-only artifact check, it publishes the exact repository,
promotion run/attempt, protected-main SHA, QA tuple, nonce, and `gateReadyAt`
UTC second in the live log and step summary. Only then may the administrator
perform the settings read:

1. Save the exact response bytes from GitHub's
   [`GET /repos/{owner}/{repo}/immutable-releases`](https://docs.github.com/en/rest/repos/repos?apiVersion=2026-03-10#check-if-immutable-releases-are-enabled-for-a-repository)
   endpoint without reserialization, then record the current UTC second as
   `checkedAt`.
2. From the same protected-main checkout, run
   `scripts/release-admin-attestation.py create` with the displayed run,
   attempt, head SHA, QA tuple, fresh nonce, `gateReadyAt`, `checkedAt`, and
   exact response file. The helper emits strict canonical JSON containing the
   response base64 and SHA-256; it accepts only the documented two-field JSON
   object with `enabled == true` and boolean `enforced_by_owner`.
3. Post those bytes unchanged as one issue comment from the actor who
   dispatched promotion. Do not add a Markdown fence or prose.

Use an administrator-authenticated local `gh` session, never the workflow's
`GITHUB_TOKEN`. After copying the displayed values into the variables below,
compare the local UTC clock with GitHub's HTTPS `Date` header and synchronize it
if the absolute offset exceeds two seconds. The exact operator sequence is:

```bash
set -euo pipefail
ATTESTATION_TMP="$(mktemp -d)"
gh api -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  repos/BrainyBlaze/moor/immutable-releases \
  > "$ATTESTATION_TMP/immutable-release-settings.json"
ATTESTATION_CHECKED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
python3 scripts/release-admin-attestation.py create \
  --repository BrainyBlaze/moor \
  --head-sha "$ATTESTATION_HEAD_SHA" \
  --qa-run-id "$ATTESTATION_QA_RUN_ID" --qa-run-attempt 1 \
  --qa-artifact-id "$ATTESTATION_QA_ARTIFACT_ID" \
  --run-id "$ATTESTATION_PROMOTION_RUN_ID" --run-attempt 1 \
  --nonce "$ATTESTATION_NONCE" \
  --gate-ready-at "$ATTESTATION_GATE_READY_AT" \
  --checked-at "$ATTESTATION_CHECKED_AT" \
  --response "$ATTESTATION_TMP/immutable-release-settings.json" \
  > "$ATTESTATION_TMP/attestation-comment.json"
gh issue comment "$ATTESTATION_ISSUE_NUMBER" --repo BrainyBlaze/moor \
  --body-file "$ATTESTATION_TMP/attestation-comment.json"
```

The current direct-repository response is the exact 42-byte body
`{"enabled":true,"enforced_by_owner":false}` with no trailing LF. An
organization-enforced repository instead reports `enforced_by_owner` as
`true`; both boolean values are valid while `enabled` must remain `true`.

The workflow polls for at most ten minutes. Before the first tag or release
mutation it requires exactly one matching comment, strict key order and
canonical JSON, a canonical base64 round trip, the exact response digest and
documented two-field object, `created_at == updated_at`, both the settings-read and
comment times at or after `gateReadyAt` (with five seconds of clock-skew
tolerance), the exact documented response shape with `enabled == true`, and a
non-future comment no more than fifteen minutes old. The
settings read must also be non-future and no more than fifteen minutes old. The
comment author must equal the dispatcher and the collaborator-permission API
must still report live repository `admin` permission. Missing, duplicate,
conflicting, edited, stale, future, mismatched, or timed-out evidence fails
before any mutation.

The accepted issue comment is the run-bound audit record: it binds repository,
protected-main SHA, QA run/attempt/artifact, promotion run/attempt, fresh nonce,
gate time, exact response bytes, and UTC check time. Its ID, URL, body digest,
and response digest are recorded in the run summary. An issue comment is
persistent but not immutable: a trusted repository administrator can later
edit or delete it. Promotion therefore re-fetches the same comment after the
published assets have been independently downloaded and verified, and requires
the original ID, author, timestamps, canonical body hash, and bindings before
final success. Later administrator tampering remains inside the explicitly
trusted-administrator boundary; the comment is not described as immutable
release evidence.

After accepting the attestation, promotion creates or resumes one
transaction-bound draft release, uploads only missing final-name assets, and
rejects any unexpected or conflicting asset. The release body remains
deterministic across fresh recovery dispatches. Promotion never rebuilds or overwrites
a release asset.

The single permitted deletion is an incomplete GitHub upload record whose
fresh API state is `starter`. It is planner-directed only for an expected name
on the exact transaction-bound draft. Immediately before deletion, promotion
re-reads the release and requires the same numeric release ID, tag, source
commit, name, body, and `draft == true`; it then re-reads the same numeric asset
ID and requires the expected name and `state == "starter"`. State is the
predicate: a starter may report a nonzero declared size even though its upload
body was interrupted. Uploaded assets, unexpected names, and every asset on a
published release are never deleted. After deletion, promotion discards the
inventory and replans from a fresh list. At most two starter records per asset
name may be deleted in one run; a third fails closed.

The published release contains the four binaries plus `moor-release-manifest-v1.json` and `SHA256SUMS`.
Promotion verifies all six while the release is still a draft, publishes it
once, then downloads all six again and verifies their sizes and hashes. A
fresh attempt-1 dispatch may adopt only the same exact transaction-bound draft;
it never resumes by rerunning a failed workflow attempt. A conflict, expired
artifact, edited evidence comment, or nonexact tag requires a new candidate and
QA cycle; it is never repaired by substitution.

GitHub exposes no conditional “publish only if the tag, release, settings, and
asset set still equal these values” operation. The workflow freshly validates
the tag, release, and all six assets immediately before publication, requires
the publish response and a fresh release read to report `immutable == true`,
re-resolves the tag, and independently downloads and verifies all six published
assets before one final immutable-release read. Its explicit concurrency group
prevents two promotion runs, but trusted repository administrators remain able
to mutate repository state in the API interval.

The attestation deliberately leaves a trusted-administrator
preflight-to-publication race. An administrator can disable immutable releases
after the attested read; in that case a mutable public release can become
visible before the workflow detects the mismatch. This is postpublication
detection, not prepublication fail-closed prevention. Recovery is: freeze all
release writers, confirm the failed run and exact repository state, delete the
mutable release, prove the public release and assets are absent and record the
exact tag state, restore and freshly attest the enabled setting, then use a NEW
attempt-1 dispatch with a new nonce. Never rerun the failed attempt. A release
that GitHub has already made immutable cannot be rolled back or repaired.

Publication is followed in the same delivery sweep by Desk’s pin schema 3 PR.
That PR copies the manifest’s full-matrix coverage and four target tuples
byte-for-byte, then proves `fetch-moor` and `install.sh` against the live release.
