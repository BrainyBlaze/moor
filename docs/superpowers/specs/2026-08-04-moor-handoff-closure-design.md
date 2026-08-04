# Moor Handoff Closure Design

**Date:** 2026-08-04

**Status:** Approved design for amending controller wire schema 3 and event schema 2 in place before the first conforming implementation.

## 1. Objective

Close the implementation-blocking gaps in the Moor clean-room handoff without exceeding the existing hard ceiling of 5,000 nonblank, non-comment first-party production source lines.

The amended handoff must make all of these surfaces determinate:

1. controller leases, reconnect-safe input, queries, status, and viewer discovery;
2. durable event, log, and exit storage;
3. the frozen CLI and child-launch contract;
4. terminal observation, canonical event serialization, and multi-record transactions;
5. contradictions, obligation closure, compatibility exceptions, conformance vectors, and integrity hashes.

The user has explicitly approved:

- amending wire schema 3 and event schema 2 in place rather than incrementing their labels; and
- expanding OB-24 beyond its current twelve corrections where a newly frozen external or on-disk behavior requires it.

This is a one-time pre-implementation amendment. The artifacts' SHA-256 digests identify the amended handoff. Once the amended handoff is published, later frozen-layout changes again require a version increment.

## 2. Non-negotiable constraints

- The behavioral specification remains authoritative when it conflicts with the wire artifact.
- Only `spec/README.md`, `spec/moor-spec.md`, and `spec/moor-wire-schema.md` are authoritative implementation inputs.
- The holder remains byte-transparent except for explicitly enumerated, bounded viewer/query behavior.
- Same-user OS peer identity remains the authorization decision. Tokens, generations, and incarnations are freshness fences.
- A peer, viewer, storage failure, or malformed byte stream may not terminate the child unless the frozen output-coordinate exhaustion rule requires owned-session termination.
- The complete first-party production implementation, including helpers, remains below 5,000 counted lines. The design budget is 4,900 lines, leaving 100 lines of mandatory headroom.
- The supervisor still performs an atomic coordinated cutover. There is no mixed controller/event dialect.

## 3. Chosen architecture

Use two shared primitives rather than subsystem-specific mechanisms:

1. A single controller connection and lease state machine serves human viewers, probes, supervisors, and non-viewing `push` clients.
2. A single dual-body/dual-commit durable store serves the event stream, capped log, and lifecycle exit record on every platform.

This is preferred over independent POSIX sidecars and bespoke log/exit formats because it closes commit-frontier races once, reduces platform divergence, and makes the line ceiling achievable.

The authoritative documents are amended directly. No third normative errata document is introduced.

## 4. Controller protocol closure

### 4.1 Status fields

Use the three currently reserved high bits in the status descriptor's main flags byte:

- bit 5: at least one fully attached viewer exists;
- bit 6: the requested child is running;
- bit 7: the configured event store is writable.

Bit 4 continues to mean that the requesting controller currently owns the input lease. A probe is never counted as a viewer. An input-only `push` connection is never counted as a viewer.

This lets `list` select the unadorned live rendering versus `[attached]`, and lets a bounded `STATUS_REPLY` expose stream failure without waiting for a heartbeat.

### 4.2 Lease frames

Add these controller frame types after the existing highest assigned value:

- `LEASE_REQUEST`: acquire a free lease or resume a disconnected lease;
- `LEASE_RESULT`: granted, resumed, released, busy, refused, or exhausted;
- `LEASE_RELEASE`: explicitly release the current lease;
- `LEASE_KEEPALIVE`: prove responsiveness while otherwise idle.

A lease request carries:

- operation: fresh acquire or resume;
- role: attached viewer or input-only controller;
- expected epoch (`0` on fresh acquire);
- a 16-byte resume token (all zero on fresh acquire).

A successful result carries the nonzero lease epoch and a fresh 16-byte resume token. The token is connection-local freshness material, not authorization; same-user authentication has already occurred.

### 4.3 Lease transitions

The holder begins with allocated epoch `0` and no owner.

- A fresh grant allocates `previous allocated epoch + 1` and resets the request high-water to zero.
- Release does not increment the epoch. It invalidates the token and leaves the last allocated epoch as history.
- Epoch `FFFFFFFF` may be granted once. After it is released or expires, every fresh request is `RESOURCE_EXHAUSTED`; the value never wraps.
- A graceful handover is release followed by another controller's fresh request. There is no queue and no forced steal.
- A busy fresh request returns `LEASE_BUSY` and changes no state.
- Every valid `INPUT`, `RESIZE`, `QUERY_REPLY`, or `LEASE_KEEPALIVE` from the owner refreshes its 10-second responsiveness deadline.
- A live lease client sends `LEASE_KEEPALIVE` every 3 seconds while otherwise idle.
- Transport loss reserves the lease, token, high-water request, complete cached request, and cached receipt for the remainder of the 10-second deadline.
- A reconnect authenticated as the same user may resume only with exact generation, holder incarnation, epoch, and token. Resume preserves the request high-water and cached receipt.
- Deadline expiry releases the lease without incrementing the epoch.

This makes a lost input receipt replayable after reconnect without permitting an old controller to write after handover.

### 4.4 Attach and `push`

`ATTACH` remains the only operation that creates a viewer. Its existing request-lease flag is shorthand for a viewer-role fresh lease request.

Attach ordering becomes:

1. `TERMINAL_STATE`;
2. `ATTACH_ACK` with the status descriptor;
3. `LEASE_RESULT` when the attach requested a lease;
4. frozen replay baseline;
5. live output.

An observer attach must send preserve geometry. A nonzero geometry is applied only if that attach obtains the lease; a nonzero geometry without a lease request is malformed.

`push` performs `HELLO`, then an input-only `LEASE_REQUEST`. It receives no terminal preamble, attach acknowledgement, replay, or viewer output. If the lease is busy it fails loudly and writes nothing. If granted, it sends input requests sequentially, safely resumes after a lost receipt when necessary, explicitly releases, and exits. It never changes geometry and never counts as an attached viewer.

### 4.5 Query frames and arbitration

Assign exact query-class values:

- `01`: primary device attributes;
- `02`: secondary device attributes;
- `03`: terminal name/version;
- `04`: private-mode report;
- `05`: cursor-position report.

Change query correlation identifiers to nonzero u64 values. They start at 1 per holder incarnation, never wrap, and are never reused. At most 64 correlations may be outstanding. A lease viewer exceeding that bound is disconnected as a slow control consumer; holder synthesis then follows the no-viewer rule.

`QUERY_REPLY` echoes the query class in addition to correlation and lease epoch. The schema freezes both query and accepted reply grammars. A class mismatch, malformed class-specific reply, expired correlation, duplicate reply, or superseded lease is discarded.

Cursor-position query grammar is `CSI 6 n`; it is viewer-only. Private-mode replies must echo the queried mode. Primary, secondary, version, mode, and cursor response grammars accept both their frozen 7-bit form and the corresponding single-byte C1 introducer form where one exists, with bounded canonical numeric parsing.

The holder delays at most the bytes of a possible supported query, never arbitrary terminal output. Query candidates are capped at 32 bytes and a 50 ms recognition deadline. Once recognized, `QUERY` is sent before those buffered query bytes are forwarded. A candidate that exceeds either bound is released unchanged as ordinary output. This bounded delay is added explicitly to the transparency exceptions.

## 5. Unified durable store

### 5.1 Carrier

Events, logs, and exit state use a directory containing exactly:

- `body.0`;
- `body.1`;
- `commit.0`;
- `commit.1`.

The event directory is pre-created empty by the caller. Log and exit directories are holder-created companions. POSIX uses exact owner-only modes; Windows uses the already frozen protected DACL and no-reparse rules. Extra entries, pre-existing slots, or an invalid directory are never adopted.

The existing `.events`, `.log`, and `.exit` reserved suffixes continue to name the three stores, so no new collision grammar is needed.

### 5.2 Generic commit record

Replace the platform-specific event commit with one portable fixed record containing:

- magic and format;
- self commit slot and named body slot;
- store kind: event, log, or exit;
- session wire generation;
- store logical epoch;
- strictly increasing nonzero u64 commit index;
- committed body prefix length;
- logical start and exclusive end coordinates;
- SHA-256 of the exact body prefix;
- CRC-32C of the preceding commit bytes.

Both platforms use little-endian encoding. Equal valid indexes with different record bytes are corruption. The greatest independently valid commit is authoritative.

Writers never mutate bytes inside the currently committed body prefix. Ordinary growth appends after that prefix, flushes the body, writes the alternate commit, and flushes it. Replacement/rotation writes the inactive body from offset zero, truncates, flushes, then commits it through the alternate commit slot. A failed flush leaves the prior commit authoritative. Readers ignore bytes after the selected prefix.

This supplies an observable commit frontier on POSIX as well as Windows: a complete newline written before a failed `fsync` remains outside the selected prefix and is not consumed.

### 5.3 Event store

The event body remains canonical JSONL. Its commit logical coordinates are `first_retained` and `next_seq`; the body header remains the semantic source of epoch and session identity. Ordinary append and compaction use the same transaction and alternate-commit algorithm on every platform.

`WAKEUP`, durable semantic ACK, and writer durable-cursor advance occur only after the commit record is durable.

### 5.4 Log store

The selected log body prefix contains only the raw retained child-output bytes. The commit record carries the absolute half-open child-output range represented by that prefix.

- Growth below the cap appends and commits.
- Rotation writes the retained suffix into the inactive body and commits its new absolute range.
- `clear` commits an empty body with `start == end == current child-output end`.
- `tail -f` follows commit indexes, not file offsets. If its next coordinate is below the selected start, it emits the exact maximal gap and resumes there.

This preserves the raw log contract while making exact gap reporting and `clear` races determinate.

### 5.5 Exit store

The exit body is one canonical JSON object with a closed key set carrying:

- schema version and type;
- canonical session identity;
- allocated generation (`null` when unsupervised) and wire generation;
- holder incarnation;
- wall/monotonic start and boot identity;
- wall-clock end;
- exact platform outcome branch;
- final child-output coordinate.

All u64 values that can exceed JSON's exact integer range are canonical decimal strings. Identity and opaque byte values use canonical padded base64.

Exit ordering is:

1. commit the lifecycle exit store;
2. commit the event-stream `exit` transaction when the event store remains writable;
3. remove the rendezvous object;
4. close the stores and terminate.

If step 1 fails, the holder reports storage failure, leaves the rendezvous as stale evidence, and exits; it does not unlink into an undiscoverable state. A crash between later steps produces one of the already specified rendezvous/exit-record cross-product cells. New creation and `rm` remove all three companion stores only after stale identity is established.

## 6. CLI and child-launch closure

The behavioral spec gains a byte-exact CLI appendix covering:

- full usage text and version-line template;
- accepted options per command;
- defaults and repeated/conflicting option precedence;
- bare, modern, and legacy placement rules;
- every argument and runtime diagnostic stream and line ending;
- list and bulk-removal ordering;
- no-argument `clear`, `rm`, and invalid mixed `rm -a <name>` behavior;
- `tail -n 0` and suffix applicability;
- session-name edge grammar.

Parsing decisions:

- modern/legacy options may surround the session operand until the first child-command operand;
- in the bare form the session is consumed first, after which options are recognized until the child command;
- `--` ends option recognition and may introduce a dash-leading session;
- repeated scalar options use the last occurrence; `-e`/`-E` and other mutually exclusive pairs likewise use the last occurrence;
- `rm -a` accepts no name; `rm` without either `-a` or a name is an argument error;
- `clear` without a name targets the innermost valid current session and succeeds silently when no valid current session exists;
- list and bulk-removal entries are ordered by rendered-name bytes ascending;
- `tail -n` accepts canonical unsuffixed u32 decimal, including zero; size options alone accept `k/m/g` suffixes.

Viewer-control decisions:

- default detach byte is `1C`; `-e` accepts one ASCII byte or canonical caret notation; a duplicate within a frozen short escape interval sends one byte to the child;
- default suspend key is `1A`; `-z` passes it through instead;
- `ctrl_l` is exactly byte `0C`;
- `move` is exactly `ESC [ H`, emitted after the terminal-state preamble and before replay;
- `winch` is an explicit exception to the change-only resize rule and emits a resize notification even when geometry is unchanged;
- `-t` marks the viewer non-VT: it receives an empty preamble, never participates in capability-query arbitration, and receives no viewer-generated control bytes, while raw child output remains unchanged.

Child execution decisions:

- POSIX commands containing `/` are executed exactly; commands without `/` use the inherited `PATH` with the platform's normal `execvp` search semantics. `argv[0]` is the command operand as supplied.
- Windows uses the documented executable-search order for a bare executable and a frozen inverse-`CommandLineToArgvW` quoting algorithm for the argument vector.
- `-d` is applied before command lookup so relative paths resolve from the requested child directory.
- Headless POSIX termios flags, control bytes, and speeds are enumerated exactly in the behavioral spec.
- Geometry accepts the full nonzero u16 range. The unreachable product limit is removed. Preserve remains `0 x 0`; mixed zero remains invalid.
- Fast successful-exec/early-exit and create-without-terminal branches receive explicit statuses and publication outcomes.

## 7. Terminal observation and event semantics

### 7.1 Scanner grammar

Freeze these reported forms and no others:

- title: OSC 0 and OSC 2;
- hyperlink: OSC 8, including open and empty-target close;
- readiness: the first recognized capability query from the five query classes;
- termination: BEL or ST, with both 7-bit and C1 OSC/ST forms;
- exact malformed-sequence, cancellation, bound, and resynchronization behavior.

Add an `observer-degraded` event carrying scanner family and reason. One transition is emitted per degradation episode when the event store remains writable. The status descriptor continues to expose tracked-mode exactness independently.

### 7.2 Canonical JSON

Event schema 2 gains a single canonical serializer:

- schema-defined key order per record type;
- no insignificant whitespace;
- UTF-8 output, with non-ASCII characters emitted directly;
- quote, backslash, and control bytes escaped by one frozen rule;
- booleans spelled `true`/`false`;
- existing canonical numeric, decimal-string, and base64 rules retained.

Header `ts` is the original stream-creation time and does not change during compaction. A snapshot preserves the timestamp of the transition whose knowledge it restates. A triggering transition uses its observation time.

Byte-exact vectors cover every record type, every branch, string escaping, maximum fields, and compaction snapshots.

### 7.3 Multi-record transactions and exhaustion

Admission operates on a transaction containing zero or more snapshots followed by one or more ordered transitions. The writer preflights the complete serialized transaction before changing in-memory state or durable state.

Required multi-transition transactions include:

- stateful-source replacement: old `disconnected/superseded`, then new `connected`;
- source loss with all newly due missing-receipt records;
- session ending: stateful sources sorted by raw source id with `disconnected/session-ending`, then the `exit` event.

Sequence, epoch, and commit exhaustion rules apply to the whole transaction. No prefix is accepted as though the operation completed. The final `stream-exhausted` transaction names the limiting axis and leaves state consistent with exactly the transitions durably committed.

Output record partitioning is explicitly unconstrained within the existing 1..65536-byte invariant. Conformance checks byte coordinates, ordering, retention bounds, and gap honesty rather than OS-read-dependent record boundaries.

Old semantic-epoch dedupe entries are discarded when their producer is superseded and no pending correlation names that epoch. Pending correlations retain only the exact tuples they still need. This makes the 512-entry bounds real rather than cumulative across all past epochs.

## 8. Compatibility and obligation register

OB-24 expands to enumerate every newly visible correction, at minimum:

- complete controller lease/query/status behavior under the coordinated supervisor cutover;
- POSIX event sink changing from a file to the portable committed directory;
- log and exit companions changing to committed directories;
- exact help/option/diagnostic behavior where the prior handoff supplied no fixture;
- completed terminal-control behavior;
- the new observer-degradation event;
- geometry values above 1000;
- corrected signalled-exit event branch.

The migration remains atomic for supervisor-owned protocol and event surfaces. Existing legacy log/exit residue is inventoried and drained before enabling the new reader. No new holder adopts an old companion layout.

The register is edited so that OB-9, OB-14, OB-23, OB-25, and OB-42 name concrete formats, carriers, and deadlines. OB-37 remains the sole downstream provider-runtime gate.

The contradictory clauses identified by review are removed or explicitly superseded, including query-delay transparency, unchanged `winch`, signalled exit, stream status, viewer discovery, synthetic-reply exceptions, and the pre-implementation in-place amendment rule.

## 9. Production line budget

The implementation plan must enforce this budget before feature work begins:

| production area | maximum counted lines |
|---|---:|
| CLI, controller, and viewer | 700 |
| holder event loop and state machines | 900 |
| controller and semantic codecs | 600 |
| terminal/query scanners | 450 |
| shared durable store, event, log, and exit adapters | 500 |
| POSIX backend | 600 |
| Windows backend, bootstrap, and insertion | 950 |
| instrumentation shims and build-time runtime source | 200 |
| **aggregate design budget** | **4,900** |
| mandatory headroom below normative ceiling | **100** |

Rules:

- One serializer, one frame codec, one deadline scheduler, and one committed-store implementation are shared.
- Platform modules implement only OS primitives; they do not duplicate policy state machines.
- Table-driven constants and generated test fixtures are preferred, but generated production source still counts at its generated total.
- Unmodified third-party dependencies may supply cryptography, JSON primitives, platform bindings, and Windows insertion machinery, subject to the existing vendoring rule.
- CI reports the per-file and aggregate count on every change and fails at 4,901, preserving the final 99 lines for release-only corrections while guaranteeing the normative 5,000 ceiling is never crossed.
- Every planned task states its budget delta. A task that exceeds its allocation must simplify or remove code before proceeding; borrowing requires an updated aggregate ledger and may never consume the 100-line normative headroom.

## 10. Verification strategy

The amendment is complete only when all of the following hold:

- both Markdown artifacts have no unresolved TODO, choice, or unnamed carrier;
- every behavioral field has an exact wire/storage representation or is explicitly unconstrained;
- all frame types, enum values, error mappings, deadlines, precedence rules, and state transitions are closed;
- byte-exact vectors validate for old and new frames, commit records, exit records, canonical JSON, CLI help/version, diagnostics, and terminal controls;
- vector lengths, CRC-32C values, SHA-256 values, and declared payload lengths are independently recomputed;
- every cross-reference resolves to the owning normative section;
- OB-1 through OB-42 are audited substantively, not merely present by number;
- only OB-37 remains open, with the same downstream owner;
- README hashes are updated last and independently rechecked;
- the implementation plan contains a 4,900-line budget ledger and an automated hard failure before 5,000 production lines.

No implementation, existing tests, workaround notes, decision briefs, or prior holder history are consulted during this amendment.
