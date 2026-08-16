# Moor release manifest v1

This document defines the machine-readable identity of a Moor release
candidate and the only permitted path from that candidate to a GitHub Release.
The supported platforms, native execution requirements, and compatibility
lanes are defined by the [release matrix](release-matrix.md). A downstream
consumer downloads Moor binaries; it never builds Moor from source.

The normative metadata files are:

- `moor-release-manifest-v1.json`, described below; and
- `SHA256SUMS`, covering exactly the four binary assets.

Both files are candidate artifacts first. Promotion publishes those same bytes
after full manual QA; it does not regenerate either file.

## Fixed release identity

Every v1 manifest has these fixed values and relationships:

- `schemaVersion` is the JSON number `1`.
- `repository` is exactly the JSON string
  `"https://github.com/BrainyBlaze/moor"`.
- `version` is exactly `v` followed by a stable SemVer core
  (`vMAJOR.MINOR.PATCH`, with no leading zero, prerelease, or build suffix).
- `commit` is exactly 40 lowercase hexadecimal characters and resolves
  to a commit in the repository. When a GitHub API requires an owner/repository
  slug, it is the fixed derivation `BrainyBlaze/moor`, not another manifest
  field.
- All four binaries are built from that exact commit. A target entry cannot
  override it.

The first release uses the proposed `version: "v0.1.0"`. That value is both the
release version and tag name. The tag does not exist until full manual QA
authorizes promotion. Where an asset embeds the crate version, it uses the
`version` value after removing its one leading `v`, so `v0.1.0` embeds `0.1.0`.

## Exact targets and names

`targets` is an object containing exactly these four keys, in this canonical
order. No alias, glibc target, fifth key, or missing key
is valid.

| target key | v0.1.0 release asset | candidate artifact name |
|---|---|---|
| `x86_64-unknown-linux-musl` | `moor-0.1.0-linux-x64` | `moor-candidate-x86_64-unknown-linux-musl` |
| `aarch64-unknown-linux-musl` | `moor-0.1.0-linux-arm64` | `moor-candidate-aarch64-unknown-linux-musl` |
| `x86_64-apple-darwin` | `moor-0.1.0-macos-x64` | `moor-candidate-x86_64-apple-darwin` |
| `aarch64-apple-darwin` | `moor-0.1.0-macos-arm64` | `moor-candidate-aarch64-apple-darwin` |

For a later v1 manifest, only the `0.1.0` component in the release asset name
changes to the manifest's `version` with its leading `v` removed. Candidate
artifact names remain the exact target-qualified strings above. Each candidate
artifact contains exactly one regular file whose basename is the corresponding
release asset name; directory entries, links, and additional files are invalid.

## JSON data model

Every object has an exact key set. Producers do not emit extension keys, and
consumers reject unknown keys, missing keys, duplicate object keys, `null`, and
values of the wrong JSON type.

### Top level

Keys occur in this order:

1. `schemaVersion`: the number `1`.
2. `repository`: the string `"https://github.com/BrainyBlaze/moor"`.
3. `version`: the version/tag described above.
4. `commit`: the exact source commit.
5. `candidate`: the candidate-run object.
6. `coverage`: the coverage object.
7. `targets`: the exact four-key target object.

The candidate-run object has these keys in order:

1. `workflowRunId`: the GitHub Actions run ID as a nonzero decimal string.
2. `workflowRunAttempt`: the positive JSON integer run attempt.
3. `metadataArtifactName`: exactly `"moor-release-candidate-v1"`.

GitHub assigns the metadata artifact ID only after upload, so it cannot be
self-recorded without a circular mutation. The candidate job output records
that assigned ID alongside the run ID and attempt. Full manual QA and promotion
must receive that immutable metadata artifact ID as an explicit input, retrieve
it from the recorded run, and verify its name is
`moor-release-candidate-v1`. The metadata artifact contains exactly
`moor-release-manifest-v1.json` and `SHA256SUMS`.

### Coverage

`docs/release-matrix.md` defines two disjoint sets of `(target, gate, lane)`
pairs: the **required closure**, which no candidate may omit, and the
**deferred set**, which a candidate may omit (it is empty today: every lane
runs on a hosted runner). The coverage object states, in the manifest's own bytes, which of
the two situations produced this candidate, so a narrowed candidate can never
be mistaken for a complete one by reading the document alone.

Like the exit branches in `spec/moor-wire-schema.md`, its exact key set depends
on the branch it declares:

- Full matrix — exactly one key:
  1. `requiredClosure`: the string `"full-matrix"`.
- Narrowed — exactly two keys in this order:
  1. `requiredClosure`: `"hosted-only"` when no deferred pair was verified, or
     `"partial"` when some were and some were not.
  2. `unverified`: a non-empty array of the deferred pairs this candidate did
     not verify, ascending by `(target, gate, lane)`. Each element has exactly
     `target`, `gate`, and `lane` in that order, each a string drawn from the
     matrix.

The label is determined by which deferred pairs are missing, never by whether
any are: a candidate that verified some deferred lanes and not others is
`"partial"`, because it is no longer hosted-only and the label is the part of
this object a reader trusts at a glance. `"full-matrix"` asserts that every
deferred pair was also verified, so the array would be empty and is therefore
absent: this format never encodes an empty array. A deferred pair that *was* verified is an ordinary verification —
it appears in the target's `provenance.verification` like any other and is
absent from `unverified`. Deferral never weakens a record: a deferred lane's
verification must still cite this exact `commit` and the same `sha256` as every
other lane for that target.

### Target entry

Each target value is an object with these keys in order:

1. `asset`: the literal release filename from the table, adjusted only for the
   manifest `version` without its leading `v`, as described above.
2. `size`: the binary's positive byte length as a JSON integer no greater than
   `9007199254740991`.
3. `sha256`: 64 lowercase hexadecimal characters: SHA-256 of exactly `size`
   binary bytes.
4. `artifactId`: the immutable GitHub Actions artifact ID as a nonzero decimal
   string.
5. `artifactName`: the exact candidate artifact name from the table.
6. `provenance`: the provenance object.

The GitHub artifact identified by `(repository, candidate.workflowRunId,
artifactId)` must belong to `candidate.workflowRunAttempt`, have exactly the
declared `artifactName`, and contain exactly the declared `asset`. An artifact
name alone is never an identity.

### Provenance

The provenance object has exactly two keys in order:

1. `build`: one job reference for the job that produced and uploaded the
   target artifact.
2. `verification`: a nonempty array of verification-job references.

The build job reference has these keys in order:

1. `workflowRunId`
2. `workflowRunAttempt`
3. `jobId`
4. `jobName`

The two IDs are nonzero decimal strings, the attempt is a positive JSON
integer, and the name is the exact nonempty GitHub Actions job name. The build
reference's run and attempt must equal the top-level candidate run and attempt.
The artifact metadata must identify this build job's run as its creator.

Each verification-job reference has these keys in order:

1. `gate`: one of `native-conformance`, `compatibility`, `static-linkage`, or
   `identity`.
2. `lane`: the stable ASCII lane name used by the release workflow.
3. `workflowRunId`
4. `workflowRunAttempt`
5. `jobId`
6. `jobName`

IDs, attempts, and job names use the same representation as the build
reference. Every referenced job must have concluded `success`, must identify
the same `repository` and `commit`, and must have downloaded the target
by its declared candidate artifact ID. The references must cover every gate
and exact-byte lane required for that target by `release-matrix.md`; one green
job may appear once for each gate it proves. Linux entries require explicit
`static-linkage` evidence. Missing or non-native evidence cannot be represented
as a weaker provenance value and blocks the entire release.

Verification references are ordered by the gate order shown above, then by
ASCII `lane`, then numerically by run ID, run attempt, and job ID. Duplicate
`(gate, lane)` pairs are invalid.

## Canonical JSON bytes

The candidate producer emits one deterministic representation:

1. UTF-8 without a byte-order mark.
2. The object and field orders defined above. `targets` uses the four-row order
   in the target table.
3. Two ASCII spaces per indentation level, one space after `:`, and no trailing
   whitespace.
4. The layout is the result of serializing the ordered data with a two-space
   pretty-print indent: each opening `{` or `[` follows its key or array
   position; each member or array element starts on a new line; each closing
   delimiter is on its own line at its parent's indentation; and every
   nonfinal member or element has one trailing comma.
5. All strings are printable ASCII and contain neither `"` nor `\`, so no
   JSON escape has an alternative spelling.
6. Integers use unsigned base-10 notation with no leading zero, sign, decimal
   point, or exponent.
7. Exactly one LF (`0a`) terminates the final `}`.

Consumers validate the semantic schema even if their JSON parser does not
preserve member order. Producers and any tool comparing candidate metadata to
published metadata compare the canonical bytes, not a reserialized
approximation.

### Desk pin projection

After validating a QA-approved manifest, Desk commits a mechanical projection
with exactly the top-level keys `schemaVersion`, `repository`, `version`,
`commit`, `coverage`, and `targets`. It retains all four exact target keys; each
target contains exactly `asset`, `size`, and `sha256`. Candidate, artifact, and
provenance fields are intentionally excluded from the consumer pin, but they
must have been validated before the projection was made.

`schemaVersion` is the one key the projection **sets** rather than copies: the
manifest states `1`, its own schema, and the pin states `2`, the consumer
schema described below. The two documents version independently, so copying
that number would claim the pin is something it is not. Every other projected
value — `repository`, `version`, `commit`, `coverage`, and each target's
`asset`, `size`, and `sha256` — is copied verbatim, without renaming or
normalization.

`coverage` is copied **verbatim**, with the same three branches defined above.
It is the one manifest field whose absence would mislead: the consumer installs
from the pin, not from the manifest, so a pin without coverage makes a narrowed
candidate byte-identical to a complete one and the consumer would embed
unverified coverage believing it verified. Copying rather than translating also
means the pin and the manifest can be compared literally, so the two documents
cannot drift.

Because the pin carries an exact six-key top level, the projection is a
whitelist: it copies the six keys it names. An exclusion list would leak the
next field added here into the pin, and the consumer rejects unknown keys, so
the leak would surface as a fail-closed refusal at install time rather than at
build time.

The consumer's pin schema version is `2`. A pin at version `1` predates
`coverage` and is refused with a diagnostic that says so, rather than being
read as fully covered — the whole point of carrying the field is defeated if a
document that lacks it is treated as if it claimed completeness. A narrowed
closure that names no lane is refused for the same reason: an assertion that
cannot be checked is worse than none.

Installing a narrowed candidate is then an explicit operator decision rather
than a default. The consumer refuses a pin whose closure is not `"full-matrix"`
unless the operator opts in, and the refusal names each unverified lane, so the
decision is made against the facts instead of the label.

Desk may exercise the pinned candidate bytes through an explicit candidate base
URL during integration and manual QA. Its production default is enabled only
after promotion and resolves an asset as
`${repository}/releases/download/${version}/${asset}`. It never treats a
candidate override as the production release location.

## `SHA256SUMS`

`SHA256SUMS` is UTF-8/ASCII with no byte-order mark. It contains exactly four
lines in the target-table order. Each line is:

```text
<64 lowercase hexadecimal SHA-256><two ASCII spaces><literal asset filename><LF>
```

There is no leading path, `*` marker, carriage return, comment, blank line, or
entry for the metadata files. Every digest and filename must equal the
corresponding manifest fields. The final line also ends in LF.

For v0.1.0 the filename column is, literally:

```text
moor-0.1.0-linux-x64
moor-0.1.0-linux-arm64
moor-0.1.0-macos-x64
moor-0.1.0-macos-arm64
```

## Candidate construction and QA

A manually dispatched candidate workflow accepts an exact `commit` and
proposed `version`. It checks out that commit and performs this sequence:

1. Build each of the four targets exactly once. The output of that build becomes
   the candidate binary; no later lane compiles a substitute.
2. Establish the authoritative `size` and `sha256` once at the build boundary,
   upload the single-file target artifact, and capture its immutable artifact
   ID and name. Later digest calculations are verification of that identity,
   not a new candidate hash.
3. Every compatibility, native-conformance, static-linkage, and identity job
   downloads the artifact by the captured run/attempt/artifact ID, verifies its
   sole filename, size, and digest before execution, and records the job
   reference.
4. Only after all automatic gates are green, create the canonical manifest and
   `SHA256SUMS`, upload the two-file metadata artifact, and record its immutable
   artifact ID in the workflow output and manual-QA record.

Full manual QA downloads the metadata artifact and all four binary artifacts by
those exact IDs. The QA record identifies the repository, source commit,
candidate run and attempt, metadata artifact ID, four binary artifact IDs,
sizes, and hashes. Testing a locally rebuilt binary, a same-named artifact, or
bytes copied from another run does not satisfy the gate.

## Immutable promotion

Promotion is allowed only after the operator records that full manual QA passed
for the exact candidate identity above. Only then may the tag named by
`manifest.version` be created. The promotion workflow performs no compilation,
linking, packaging, or source-based reconstruction.

Promotion must:

1. Resolve the tag named by `manifest.version` to a commit and require exact
   equality with `manifest.commit`.
2. Require the repository, candidate run ID and attempt, and metadata artifact
   ID/name to equal the approved manual-QA record.
3. Download each target artifact by the manifest's immutable artifact ID and
   require GitHub metadata to match repository, candidate run/attempt, and
   artifact name.
4. Require each artifact to contain only its declared regular file, then verify
   the filename, byte length, and SHA-256 before upload.
5. Require the candidate manifest and `SHA256SUMS` to be byte-for-byte equal to
   the files approved by manual QA.
6. Upload the four verified raw binaries plus those two unchanged metadata files
   to the GitHub Release for the exact `manifest.version` tag.
7. Download the published release assets again and verify their byte length and
   SHA-256 before reporting promotion success.

An expired or missing candidate artifact, an artifact ID/name/run mismatch, a
changed byte, a non-green or missing provenance job, a tag/source mismatch, an
already-populated conflicting release asset, or an unverifiable manual-QA
record fails closed. The remedy is a new candidate run and a new full QA cycle;
promotion never rebuilds or silently substitutes bytes.
