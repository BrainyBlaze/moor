# Local-Admin Release Promotion Design

Status: Approved by delegated team consensus on 2026-08-17

## Summary

Moor release promotion will no longer ask a GitHub Actions token to create the
tag, draft, assets, or published release. GitHub Actions becomes a read-only
orchestrator and independent verifier. The repository administrator performs
the release transaction from an authenticated local `gh` session by pasting
one command disclosed by the waiting workflow.

This preserves the exact already-reviewed and already-QAed candidate commit
`aa26a26f5308aa31091d143c6600d5f5dd1c1bf1`. It also avoids storing a personal
token or GitHub App credential in the repository. The published release is
successful only after Actions independently proves the exact tag, deterministic
metadata, immutable state, and all six downloaded asset bytes.

## Release instance this design must recover

- Protected-main release tooling baseline:
  `1bc9734f247c80207c9157f5a0bd497933507349`.
- Candidate commit:
  `aa26a26f5308aa31091d143c6600d5f5dd1c1bf1`.
- Candidate run/attempt: `32003467728/1`.
- Candidate metadata artifact: `9279394300`.
- Candidate record artifact: `9279395137`.
- Candidate QA run/attempt: `32015916481/1`.
- Candidate QA evidence artifact: `9283559446`.
- Candidate QA evidence comment: issue comment `5314360464`.
- Release QA run/attempt: `32016500124/1`.
- Release QA evidence artifact: `9283701943`.
- Release QA API digest:
  `sha256:40eee8ef019c6054a79f1fd48802e114057cf8d1b0bc67cf622fc6f76d49cd8f`.
- Failed promotion run `32016759142/1` stopped on the immutable-settings
  authorization check and made zero release mutations. It must never be rerun.
- Failed promotion run `32022936178/1` passed attestation, then received 403 on
  its first release mutation, tag creation, and made zero release mutations. It
  must never be rerun.

The design is reusable for later releases, but those later invocations must
bind their own candidate and QA records.

## Why the authority boundary changes

The candidate predates the current release workflow and therefore differs from
the default branch under `.github/workflows`. A GitHub Actions integration token
with nominal `contents: write` cannot create a tag or release targeting that
commit. The live failure was executable evidence of that restriction.

The administrator's local OAuth session is a different authority class. Before
implementation, two bounded probes established the required capabilities:

1. On `BrainyBlaze/moor`, the local `levi770` session created a scratch tag at
   the exact candidate, created a draft release, uploaded an asset, and then
   deleted the draft and tag. The repository returned to zero tags and zero
   releases.
2. In a private throwaway repository with immutable releases enabled, the same
   session published a draft and a fresh read returned `immutable: true` with
   the uploaded asset intact.

The target repository probe deliberately did not publish. Publication on Moor
remains the operator-owned irreversible action.

## Constraints

1. No personal access token, OAuth token, or GitHub App credential is stored in
   Actions, repository secrets, artifacts, issue comments, or evidence files.
2. The human runs exactly one local command for the real release transaction.
3. Agents and Actions stop at the accepted gate and never perform the release
   mutation command.
4. Promotion is attempt-1 only. Reruns of a promotion attempt are refused.
5. A failed execution is never restarted after its first release mutation.
   Recovery always uses a fresh attempt-1 dispatch and nonce.
6. The tag is lightweight, points exactly to the candidate, and is never moved
   or deleted.
7. No published release is edited, deleted, or republished.
8. The only permitted deletion is an expected-name asset in `starter` state on
   the exact draft, chosen by the existing asset planner after fresh identity
   fences, with a maximum of two deletions per name per execution.
9. The public release body remains the exact approved deterministic three-line
   body. Run-specific evidence never enters it.
10. An Actions success is based on independent post-publication observation,
    not on the helper's write response or completion receipt alone.

## Non-goals

- Re-cutting or rebuilding the candidate.
- Changing Rust production source or the six release asset bytes.
- Storing a reusable release credential in GitHub.
- Making drafts visible to a read-only Actions token.
- Rolling back an indeterminate public state.
- Performing a Desk installer pin or any later deployment action as part of
  promotion.

## Components and authority

### Read-only workflow

`.github/workflows/release-promote.yml` has only:

- `actions: read`
- `contents: read`
- `issues: read`

It supports two explicit `workflow_dispatch` modes:

- `promote`: reconstruct, gate, wait for the helper, then independently verify
  the published release.
- `verify-published`: authenticate a prior accepted completion receipt and
  independently verify the live immutable release without opening a gate.

Both modes refuse any run attempt other than attempt 1. The workflow contains
no release mutation command and no stored administrator credential.

### Local helper

`scripts/release-admin-promote.py` runs from a clean checkout at the exact
protected-main head disclosed by the workflow. It invokes the existing local
`gh` authentication without reading or printing the token. The helper owns all
issue-comment and release mutations:

- preflight comment
- tag creation
- draft creation
- asset upload and the narrowly permitted starter cleanup
- one publish transition
- completion comment

The helper never edits a comment, moves or deletes a tag, deletes a release, or
modifies a published release.

### Existing release logic

`scripts/release-asset-transaction.py plan` and `verify-complete` remain the
only asset decision engine. Candidate, QA-record, and release-manifest tools
remain the provenance and byte-verification authorities. The new helper
orchestrates them rather than duplicating their rules.

## Data flow

1. A fresh attempt-1 `promote` dispatch validates the protected-main head and
   the exact candidate and QA tuple.
2. Actions reconstructs the six release files and creates a canonical promotion
   manifest.
3. Actions uploads a promotion bundle and enters a named preflight wait. The job
   summary prints one complete, pasteable helper command with all values filled.
4. The helper verifies its local authority and the exact source bytes, then
   posts one canonical preflight comment.
5. Actions validates that comment and enters a named completion-wait step.
6. The helper observes that exact live step before every release mutation,
   executes or adopts exact state, publishes once, and performs its own fresh
   post-publication verification.
7. The helper freezes a local transaction evidence manifest and posts one
   completion comment.
8. Actions treats the completion comment as a signal only. It re-fetches all
   anchors and downloads all six published assets before succeeding.

## Canonical promotion manifest

The workflow writes UTF-8 JSON with sorted keys, compact separators, and one
trailing LF. It binds:

- schema version, repository, release version, dispatch mode, nonce;
- promotion run, attempt 1, protected-main head;
- candidate run, attempt, commit, and every immutable candidate artifact ID and
  API digest named by the QA record;
- candidate-QA and release-QA run, attempt, artifact ID, and API digest;
- exact tag, release name, and SHA-256 of the deterministic release body;
- six asset entries sorted by name, each with name, byte length, and SHA-256.

The workflow uploads this manifest and the six files as a named artifact. The
artifact ID and API digest are outside the manifest to avoid a circular digest.
The upload action's bare 64-hex digest is normalized once to the REST form
`sha256:<hex>` and compared to the artifact API record; only that canonical
prefixed value crosses the gate. The gate binds the outer artifact identity and
the manifest SHA-256.

## Source modes

The primary source mode is `run-bundle`. Before helper download code is chosen,
a hosted diagnostic must prove whether a finalized upload-artifact v4 artifact
can be downloaded by numeric ID while its workflow run is still in progress.

If that live contract is unavailable, the helper uses `qa-reconstruction`. It
downloads the immutable candidate artifacts named by the QA record and rebuilds
the same six files. Both source modes must produce the identical canonical
promotion manifest SHA-256. A fallback therefore changes transport, not
provenance or expected bytes.

## Preflight

One helper invocation performs preflight and mutation phases automatically.
Before any release mutation it must:

1. Prove the checkout is clean and its `HEAD` equals the protected-main gate
   head.
2. Resolve the authenticated login and require it to equal the workflow
   dispatcher.
3. Re-read repository permissions and require current administrator access.
4. Read the live OAuth scope header and require `repo` and `workflow` for the
   established classic OAuth session.
5. Compare local UTC time with an authenticated GitHub `Date` header.
6. Read immutable-release settings, validate the documented typed response with
   `enabled == true`, and retain the exact response bytes and SHA-256.
7. Download or reconstruct the six files and require exact manifest equality.
8. Require the matching attempt-1 run to be in the named preflight wait.
9. Post exactly one canonical preflight comment for the nonce.

The comment begins at byte zero with a fixed ASCII marker. No byte is allowed
before the marker. The remainder is canonical JSON followed by one LF. It binds
the full manifest/source tuple, dispatcher, administrator login, settings
response SHA-256, GitHub server time, check time, and helper commit.

Actions accepts the comment only when:

- its bytes are exact;
- `created_at == updated_at`;
- exactly one comment exists for the nonce;
- its timestamps are within the bounded freshness window;
- the commenter and dispatcher identities match;
- live administrator permission still holds;
- every disclosed run, QA, source, manifest, and issue value matches the gate.

Actions then enters the named 60-minute completion wait. The entire job has a
90-minute timeout. The preflight wait is separately bounded and named.

## Transaction state machine

### S0: Prepared

All preflight checks and the accepted workflow gate are current. No release
mutation has occurred.

### S1: Tagged

- Absent tag: create one lightweight ref at the exact candidate.
- Exact tag: adopt it.
- Wrong or duplicate tag state: refuse and freeze.

The tag is never moved or deleted. An ambiguous create response is resolved by
fresh observation, never by blind retry.

### S2: Drafted

- No release: create the exact deterministic draft.
- Exact draft: adopt it.
- Exact already-published immutable release: skip to S5 without mutation.
- Conflicting draft, multiple release, or mutable published release: refuse and
  freeze.

The helper never deletes a release.

### S3: Assets complete

Before each planner decision, list assets freshly and download every uploaded
asset. The existing planner returns one of:

- all exact: continue;
- one missing expected name: upload that file;
- one expected-name `starter`: after fresh release and asset identity fences,
  delete that exact asset if the per-name execution count is below two;
- any foreign, duplicate, conflicting, or published starter state: refuse.

After every action, re-list and re-plan. Complete state requires all six exact
uploaded assets.

### S4: Published

Immediately before publication, the helper re-resolves:

- live accepted workflow completion step;
- exact tag and candidate SHA;
- exact draft identity and deterministic metadata;
- unedited preflight comment and current administrator identity;
- immutable-release settings and GitHub time;
- all six downloaded asset bytes.

Only then may it patch `draft` from true to false. A fresh release read must
return `draft == false` and `immutable == true`. The tag, metadata, and six
assets are independently re-resolved and re-downloaded.

### S5: Receipted

The helper creates or adopts one exact completion receipt for the nonce. It
never republishes. If the exact immutable release exists without an acceptable
receipt, recovery requires a fresh promote dispatch and a fresh receipt under a
new nonce, with zero release mutation.

## Completion receipt

The completion comment uses a distinct fixed ASCII marker at byte zero,
canonical JSON, and one LF. It binds:

- full promotion, candidate, QA, source, and manifest tuple;
- preflight comment ID and exact body SHA-256;
- tag ref and candidate SHA;
- release ID, URL, tag, name, release-body SHA-256, and `immutable == true`;
- six sorted asset IDs, names, sizes, and downloaded SHA-256 values;
- a fresh authority check with settings-response SHA-256 and GitHub time;
- authority phase `prepublish` for the check immediately before publication, or
  `published-recovery` for a fresh check immediately before a zero-mutation
  receipt on an already immutable exact release;
- local transaction-evidence manifest SHA-256.

Receipt uniqueness is per nonce. A duplicate, edited, or refused receipt is
never repaired. A fresh promote dispatch is the recovery boundary.

## Local evidence

The helper creates a numbered, sanitized transcript for every security-relevant
API exchange through final post-publication proof. Each exchange record includes
method, path, sanitized request body, status, selected response headers, and
exact response body. Every semantic SHA-256 comparison also creates a canonical
check record binding its subject, expected and observed digests, match result,
and the request sequence that produced the observed bytes. Authorization
material is never recorded.

Before posting the completion receipt, the helper freezes
`transaction-evidence-manifest.json`. It canonically lists each transcript's
relative path, byte length, and SHA-256. The receipt binds the SHA-256 of those
manifest bytes.

The completion-comment POST and response are written to a sibling delivery
transcript after the transaction manifest is frozen. This explicit boundary
avoids the impossible requirement that a receipt bind the response caused by
that same receipt. Actions stores its own independent receipt reads and final
verification in workflow evidence.

## Independent Actions verification

The workflow does not trust the helper's completion assertions. After seeing a
candidate receipt, it independently:

1. Re-fetches the exact preflight and completion comments and requires exact,
   unedited bytes and live administrator identity.
2. Resolves the tag and requires the candidate SHA.
3. Resolves the published release and requires exact deterministic metadata,
   `draft == false`, and `immutable == true`.
4. Lists exactly the six expected assets.
5. Downloads every asset and compares size and SHA-256 to its independently
   reconstructed promotion manifest.
6. Re-fetches the same completion comment again and requires identical bytes
   and timestamps.

Only this sequence can make the workflow green.

## Verification-only recovery

`verify-published` is dispatch-only, attempt-1-only, and read-only. Its inputs
identify the prior completion comment and original promote run. It
authenticates the original preflight, receipt, bundle or reconstruction source,
QA tuple, nonce, and head, then repeats the full independent published proof.

It has no human gate and cannot call a mutation endpoint. It fails with a named
cause if the release is not exactly immutable or any asset differs. It cannot
bless a mutable release.

## Failure classes

- `PREMUTATION_REFUSAL`: authority, checkout, clock, settings, provenance,
  comment, or gate mismatch. Zero release mutation.
- `GATE_TIMEOUT`: bounded preflight or completion wait expires. A fresh attempt-1
  dispatch is required.
- `RESUMABLE_PARTIAL`: an abandoned execution left exact tag, draft, or assets.
  A fresh dispatch/nonce/preflight may adopt the exact state.
- `PUBLISHED_RECEIPT_PENDING`: the exact immutable release exists without an
  acceptable receipt. A fresh promote dispatch performs zero release mutation
  and creates a fresh receipt.
- `VERIFIED_RECOVERY`: `verify-published` proves a prior accepted receipt and
  live immutable state with zero mutation.
- `INDETERMINATE_PUBLIC`: conflicting tag, conflicting or mutable public
  release, duplicate or conflicting comments/assets, or an ambiguous mutation
  not resolved by bounded fresh observation. Freeze and escalate.

There is no rollback and no blind retry. After an ambiguous write, bounded
fresh observation may advance only on exact state. Conflicting state freezes;
unresolved or absent state stops for a fresh dispatch.

## Testing strategy

Implementation is test-driven. A fake `gh` harness records methods, routes,
bodies, responses, and ordering. It covers:

- every preflight authority and provenance refusal with zero mutations;
- both source modes and identical canonical manifest output;
- exact marker-at-byte-zero comment contracts;
- every tag, draft, asset, publish, receipt, and ambiguous-response state;
- crash injection after every mutation boundary;
- fresh-nonce adoption and zero-mutation published recovery;
- evidence inventory and self-reference boundary;
- mutation-free `verify-published` success and named failures.

Static workflow tests enforce exact read-only permissions, attempt-1 modes,
named timeouts, the complete pasteable command, published-only verification,
same-comment final re-fetch, and absence of release mutation commands.

A hosted diagnostic proves or rejects in-progress run-artifact download before
the helper chooses its normal source path. The diagnostic has read-only
permissions and is removed before merge.

Local gates include all Python contract suites, release workflow and asset
planner suites, formatting, Cargo format, warnings-denied lint, and the complete
Rust test matrix. Hosted gates are Quality, CodeQL, Linux, and macOS. Two
independent exact-head reviews are required before merge.

## Rollout

1. Merge the approved, fully green implementation PR to protected main.
2. Dispatch a fresh attempt-1 `promote` run bound to the existing candidate and
   approved QA tuple.
3. Agents verify the gate and relay its exact one-line command. They do not run
   it.
4. The human pastes the command locally and remains the sole release mutator.
5. Actions independently proves the immutable release and six assets.
6. If Actions dies after publication, use fresh attempt-1 read-only
   `verify-published`, or fresh `promote` for a new receipt if the prior receipt
   was unacceptable.
7. Treat any later installer or Desk pin as a separate authorized operation.

## Approval record

The architecture, state machine, schemas/evidence, and testing/operations
sections were reviewed live in the team channel. Two independent reviewers
approved each substantive section. The final delegated-consensus correction is
that there is no additional human approval gate before implementation and no
Windows hosted lane. The human's only remaining action in this release is the
single local promotion command.
