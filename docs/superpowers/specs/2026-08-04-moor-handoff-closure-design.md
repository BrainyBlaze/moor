# Moor Handoff Closure Design

**Date:** 2026-08-04

**Status:** User-approved direction; exact review amendments pending independent approval before authoritative spec edits.

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
- The complete first-party production implementation, including helpers, has an absolute maximum of 4,900 counted lines. The normative 5,000-line ceiling is never approached; its remaining 100 lines are unavailable contingency, not a release budget.
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

Append one byte of health flags to the status descriptor: bit 0 log store writable, bit 1 lifecycle store writable, bit 2 terminal observer exact, bit 3 query delegation still allocatable, bits 4..7 zero. Follow it with u32 selected log epoch, u64 selected log commit index, u64 retained log start, and u64 retained log end; all four are zero when logging is disabled. Disabled logging has bit 0 clear; before child exit, a successfully initialized lifecycle store has bit 1 set. Observer exactness is distinct from the existing tracked-mode exactness bit.

Extend `HEARTBEAT` flags without changing its cadence: bit 0 child running, bit 1 event store writable, bit 2 log store writable, bit 3 lifecycle store writable, bit 4 terminal observer exact, and bits 5..7 zero. A change to any flag queues an immediate heartbeat. Thus event, log, lifecycle, and scanner degradation have a bounded controller-visible carrier even when the event stream itself cannot accept a diagnostic.

### 4.2 Lease frames

Assign these controller frame values after the existing `QUERY` value `14`:

| value | frame | direction | exact payload |
|---|---|---|---|
| `15` | `LEASE_REQUEST` | controller to holder | byte 0 operation, byte 1 role, bytes 2..3 zero, u32 expected epoch, 16-byte expected holder incarnation, 16-byte resume token |
| `16` | `LEASE_RESULT` | holder to controller | byte 0 outcome, byte 1 reason, byte 2 role, byte 3 zero, u32 epoch, 16-byte resume token |
| `17` | `LEASE_RELEASE` | controller to holder | u32 epoch, then the exact 16-byte current resume token |
| `18` | `LEASE_KEEPALIVE` | controller to holder | u32 epoch, then the exact 16-byte current resume token |
| `19` | `LOG_CLEAR` | controller to holder | expected 16-byte holder incarnation, then u64 selected log commit index observed in status |
| `1A` | `LOG_CLEAR_RESULT` | holder to controller | byte outcome, byte reason, 2 zero bytes, u32 resulting log epoch, u64 observed/prior index, u64 resulting index, u64 cleared-through child-output coordinate |

Their exact payload lengths are respectively 40, 24, 20, 20, 24, and 32 bytes; `MORE` is forbidden. Any other length or fragmentation is `MALFORMED_FRAME`.

Operation is `00` fresh and `01` resume. Role is `00` viewer and `01` input-only. A fresh request has zero epoch/incarnation/token. Resume requires all three nonzero fields to match the unexpired reservation and the role to match its original role. Result outcomes are `00` granted, `01` resumed, `02` released, and `03` refused. Result reasons are `00` none, `01` busy, `02` bad epoch, `03` bad token, `04` bad role, `05` not held, `06` exhausted, and `07` bad incarnation. A successful outcome has reason zero, a nonzero epoch, and a nonzero token for grant/resume or an all-zero token for release. A refusal has a nonzero reason, reports the current allocated epoch, and carries an all-zero token. No result reveals another controller's token.

`LOG_CLEAR_RESULT` outcomes are `00` cleared, `01` already empty or disabled, and `02` refused. Reasons are `00` none, `01` stale observed index, `02` store unavailable, and `03` store corrupt. Outcomes 00/01 with reason zero are CLI success; refusal is exit 1. These two log frames are connection operations, not lease operations; same-user authentication is sufficient.

A lease request carries:

- operation: fresh acquire or resume;
- role: attached viewer or input-only controller;
- expected epoch (`0` on fresh acquire);
- expected holder incarnation (all zero on fresh acquire);
- a 16-byte resume token (all zero on fresh acquire).

A successful grant or resume carries the nonzero lease epoch and a fresh 16-byte resume token. Resume rotates the token atomically, so only the newest connection can resume again. The token is connection-local freshness material, not authorization; same-user authentication has already occurred.

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

Grant is the only transition that allocates an epoch. Resume preserves it; release and expiry do not. `LEASE_RELEASE` always receives a `LEASE_RESULT`; an exact current tuple releases, while any mismatch returns refused/not-held and changes nothing. A valid keepalive has no response. An invalid keepalive receives connection `ERROR(LEASE_NOT_HELD)` and closes only that connection. Resume-token generation uses the platform cryptographic source, rejects the all-zero value, and failure refuses the grant without consuming an epoch.

Fresh-request decision order is active or reserved lease → refused/busy; otherwise last allocated epoch `FFFFFFFF` → refused/exhausted; otherwise allocate the next epoch → granted. Resume succeeds only against an exact unexpired reservation; a syntactically valid mismatch is refused without revealing which freshness field differed on the CLI surface.

### 4.4 Attach and `push`

`ATTACH` remains the only operation that creates a viewer. Its existing request-lease flag is shorthand for a viewer-role fresh lease request.

`ATTACH` flag bit 0 remains request lease; bit 1 is `NON_VT` and bits 2..7 are zero. A `NON_VT` viewer can own input and geometry but is never selected for query delegation and receives the empty preamble/control behavior in §6.3.

Attach ordering becomes:

1. `TERMINAL_STATE`;
2. `ATTACH_ACK` with the status descriptor;
3. `LEASE_RESULT` when the attach requested a lease;
4. frozen replay baseline;
5. live output.

An observer attach must send preserve geometry. A fresh nonzero geometry is applied only if that attach obtains the lease; a nonzero geometry without a lease request is malformed except on the immediate attach of an already resumed viewer, which already owns the lease and may apply it.

`push` performs `HELLO`, then an input-only `LEASE_REQUEST`. It receives no terminal preamble, attach acknowledgement, replay, or viewer output. If the lease is busy it fails loudly and writes nothing. If granted, it sends input requests sequentially, safely resumes after a lost receipt when necessary, explicitly releases, and exits. It never changes geometry and never counts as an attached viewer.

### 4.5 Connection phases and exact attach/resume order

Each authenticated controller connection is in exactly one phase:

| phase | viewer | owns lease | legal state-changing frames |
|---|---:|---:|---|
| `U` authenticated/unattached | no | no | `ATTACH`; fresh input-only `LEASE_REQUEST`; viewer-role resume `LEASE_REQUEST`; `LOG_CLEAR` |
| `I` input-only | no | yes | `INPUT`, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `R` resumed viewer, attach pending | no | yes | `ATTACH` without its request bit, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `O` attached observer | yes | no | fresh viewer `LEASE_REQUEST`, `LOG_CLEAR` |
| `V` attached lease viewer | yes | yes | `INPUT`, `RESIZE`, `QUERY_REPLY`, `LEASE_KEEPALIVE`, `LEASE_RELEASE`, `LOG_CLEAR` |
| `C` closing | no | no | none |

`STATUS` is legal in `U`, `O`, and `V`; `OUTPUT_ACK` is legal only in `O` and `V`; termination retains its existing authenticated phase rules. Any frame not legal in the current phase is `MALFORMED_FRAME` and changes no state.

A fresh interactive attach is `HELLO`, `ATTACH`, then the holder's terminal-state/ack/result/baseline sequence. The `ATTACH` request bit asks for an atomic fresh viewer-role grant. Busy does not fail the attach: it yields a refused/busy `LEASE_RESULT` and phase `O`. An observer later upgrades with a fresh viewer-role `LEASE_REQUEST`; success changes `O` to `V` without another baseline.

A reconnecting lease viewer performs `HELLO`, a resume `LEASE_REQUEST` in `U`, receives `LEASE_RESULT(resumed)` and a rotated token, then sends `ATTACH` with its request bit clear within the existing 2-second identity deadline. This changes `R` to `V` and produces the ordinary terminal-state/ack/baseline sequence without a second lease result. A viewer-role fresh request is illegal in `U`; a viewer must attach first or use the attach shorthand. An input-only fresh request changes `U` to `I`, and `ATTACH` is illegal in `I`.

The holder accepts an `ATTACH` atomically: it freezes the replay descriptor and changes the connection to `O` or `V` before queuing `TERMINAL_STATE`. That instant is the definition of **fully attached**, so the status descriptor in the following `ATTACH_ACK` counts the new viewer and `list` may render `[attached]`. A send failure detaches it again. Replay is complete when the last frozen `GAP`/`OUTPUT` frame has been queued; live output is ordered after it.

A graceful viewer detach releases a held lease and waits for `LEASE_RESULT(released)` before closing. Transport loss detaches the viewer immediately but reserves an owned lease under §4.3. Resume never makes a viewer until its later `ATTACH`. On any owner disconnect, every outstanding query for that connection is resolved by §4.6 before the transport state is discarded.

### 4.6 Query frames and arbitration

Assign exact query-class values:

- `01`: primary device attributes;
- `02`: secondary device attributes;
- `03`: terminal name/version;
- `04`: private-mode report;
- `05`: cursor-position report.

Change query correlation identifiers to nonzero u64 values. They start at 1 per holder incarnation, never wrap, and are never reused. `QUERY` carries u64 correlation, u32 lease epoch, one-byte class, then length-prefixed exact query bytes. `QUERY_REPLY` carries the same first three fields followed by the length-prefixed reply bytes.

The complete accepted query grammar is below. `CSI7` is bytes `1B 5B`; `CSI8` is byte `9B`. Each row accepts either introducer and nothing else.

| class | exact query tail after CSI |
|---|---|
| `01` primary attributes | `63` or `30 63` |
| `02` secondary attributes | `3E 63` or `3E 30 63` |
| `03` terminal version | `3E 30 71` |
| `04` private mode | `3F <mode> 24 70`, where mode is canonical decimal `0..4294967295` |
| `05` cursor position | `36 6E` |

Accepted viewer replies are likewise closed:

- class `01`: `CSI ? P[;P]* c`, one through 16 canonical decimal parameters, each `0..65535`;
- class `02`: `CSI > P;P;P c`, exactly three canonical decimal parameters, each `0..4294967295`;
- class `03`: `DCS > | T ST`, where the accepted pairs are 7-bit `DCS=1B 50` with `ST=1B 5C` or C1 `DCS=90` with `ST=9C`, and `T` is 1..128 bytes in `20..7E`; mixed introducer/terminator forms are invalid;
- class `04`: `CSI ? <same-mode> ; S $ y`, with the exact query mode bytes echoed and `S` one canonical digit `0..4`;
- class `05`: `CSI R;C R`, with canonical decimal row and column each `1..65535`.

In those four CSI reply rows `CSI` is either `CSI7` or `CSI8`. Canonical decimal is `0` or a nonzero digit followed by digits, with no leading zero. No C0 byte, embedded C1 byte, omitted parameter, private prefix not shown above, or trailing byte is accepted. Holder synthesis mirrors representation: a `CSI7` query receives the existing 7-bit reply; a `CSI8` query receives its C1-CSI equivalent; class `03` maps CSI7 to the 7-bit DCS/ST pair and CSI8 to the C1 pair. The fixed reply bodies remain unchanged and class `05` is never synthesized.

At most 64 correlations may be outstanding. Query admission first determines whether a fully attached, VT-capable viewer owns the lease; otherwise it uses the no-viewer rule without consuming an identifier. If 64 are already outstanding, overload wins before the numeric counter is examined or advanced: disconnect that viewer as a slow control consumer and reserve its lease under the normal disconnect rule. If fewer than 64 are outstanding but no successor identifier exists, report `RESOURCE_EXHAUSTED` to and disconnect that viewer; exhaustion is permanent for the incarnation. In either disconnect case, cancel every still-outstanding correlation in allocation/child-output order and resolve each immediately under the no-live-viewer rule, then resolve the newly recognized query last. Only after those decisions are serialized are its raw query bytes released to viewers. An already accepted viewer reply is never synthesized again.

Transport loss and explicit lease release use the same ordered cancellation rule. A malformed/class-mismatched reply is discarded while its correlation remains pending until reply, cancellation, or the 250 ms deadline. A duplicate or expired reply is discarded.

Correlation `FFFFFFFFFFFFFFFF` may be allocated once. After it is resolved, delegation is permanently exhausted for that holder incarnation: the first later query performs the just-defined exhaustion report/disconnect; subsequent queries are processed immediately under the no-viewer synthesis rule, no `QUERY` frame is emitted, and raw query bytes still reach every viewer. This does not terminate the child or prevent later input leases; status health bit 3 remains clear.

`QUERY_REPLY` echoes the query class in addition to correlation and lease epoch. A class mismatch, malformed class-specific reply, expired correlation, duplicate reply, or superseded lease is discarded.

Cursor-position query grammar is `CSI 6 n`; it is viewer-only. Private-mode replies must echo the queried mode.

The holder delays at most the bytes of a possible supported query, never arbitrary terminal output. Query candidates are capped at 32 bytes and a 50 ms recognition deadline. Once recognized, `QUERY` is sent before those buffered query bytes are forwarded. A candidate that exceeds either bound is released unchanged as ordinary output. This bounded delay is added explicitly to the transparency exceptions. A `NON_VT` lease viewer is treated as no eligible viewer for arbitration: no `QUERY` is sent to it and the holder immediately applies the synthesis-or-silence rule.

## 5. Unified durable store

### 5.1 Carrier

Events, logs, and exit state use a directory containing exactly:

- `body.0`;
- `body.1`;
- `commit.0`;
- `commit.1`.

The event directory may be pre-created empty by the caller or absent for Moor to create exclusively after stale cleanup. Log and lifecycle directories are holder-created companions. POSIX uses exact owner-only modes; Windows uses the already frozen protected DACL and no-reparse rules. Extra entries or pre-existing slot files are never adopted.

The caller's `-T` path names the event directory; the lifecycle launch record retains that exact native path for cleanup. The existing `.events`, `.log`, and `.exit` reserved suffixes remain reserved. Section 6.5 adds `.instrument` for the immutable `-S` staging companion.

### 5.2 Generic commit record

Replace the platform-specific event commit with this exact 92-byte portable record. All integers are unsigned little-endian.

| offset | width | field |
|---:|---:|---|
| 0 | 8 | ASCII magic `MOORCMT1` (`4D 4F 4F 52 43 4D 54 31`) |
| 8 | 1 | format `01` |
| 9 | 1 | self commit slot `00` or `01` |
| 10 | 1 | named body slot `00` or `01` |
| 11 | 1 | kind: `01` event, `02` log, `03` lifecycle |
| 12 | 4 | session wire generation |
| 16 | 4 | logical epoch |
| 20 | 4 | flags/reserved, zero |
| 24 | 8 | strictly increasing nonzero commit index |
| 32 | 8 | committed body prefix length |
| 40 | 8 | logical start coordinate |
| 48 | 8 | logical exclusive end coordinate |
| 56 | 32 | SHA-256 of exactly the committed body prefix |
| 88 | 4 | CRC-32C over bytes 0..87 |

A commit file is valid only when it is exactly 92 bytes, its self slot matches its filename, every enum/range/reserved check passes, its CRC is valid, the named body is at least the committed length, the prefix hash matches, and the kind-specific body rules below pass. Empty commit files are invalid initialization state. Equal valid indexes with different record bytes are corruption; byte-identical equals select that record. Otherwise the greatest independently valid index is authoritative. Readers ignore body bytes after its prefix.

Creation exclusively creates the directory and four empty regular, non-link slots, durably flushes the directory entries, writes `body.0`, flushes it, writes index 1 to `commit.0`, and flushes that file. Event index 1 contains the schema header with epoch 0 and coordinates `[0,0)`. Log index 1 has an empty body, epoch 1, and `[0,0)`. Lifecycle index 1 contains the canonical running record, epoch 1, and `[0,0)`. Publication is forbidden until every enabled initial store is valid.

Writers never mutate bytes inside the selected prefix. Growth writes from the selected prefix length, removes any older uncommitted tail, flushes the body, writes the alternate commit file at offset zero, truncates it to 92 bytes, and flushes it. Replacement writes the inactive body from offset zero, truncates, flushes, then writes and flushes the alternate commit pointing to it. File creation and removal additionally flush the containing directory on POSIX and use the corresponding durable namespace operation on Windows. A failure before a new valid commit is selected leaves the prior commit authoritative. A commit-flush timeout is explicitly ambiguous: either the old or submitted commit may later validate, so no rollback is guessed and the store closes permanently.

Kind rules are exact:

- **event:** prefix is nonempty canonical schema-v2 JSONL within the existing 256 KiB cap and its two already bounded overage exceptions, starts with exactly one header, ends in LF, has no malformed/unknown record, commit epoch equals header epoch, and commit coordinates equal header `first_retained` and `next_seq`;
- **log:** prefix is arbitrary raw bytes of length exactly `end-start`, length is at most the configured cap, and epoch increments for every body replacement but not growth; empty is valid;
- **lifecycle:** prefix is at most 4 MiB and exactly one canonical JSON object plus LF, epoch is 1, start equals end, and that coordinate is zero in `running` phase or the final child-output end in `exited` phase.

Event sequence/epoch/commit exhaustion uses the existing whole-transaction precedence on every platform. Log commit `FFFFFFFFFFFFFFFF` may be used once for the newest representable suffix or a clear, after which logging becomes permanently unwritable; a replacement that would require epoch `100000000` is refused before mutation and likewise closes logging. Lifecycle has exactly the initialization and exit commits, so any recovered index/epoch state that cannot admit the exit update is corruption rather than a reset. No counter wraps.

This supplies an observable commit frontier on POSIX as well as Windows: a complete newline written before a failed `fsync` remains outside the selected prefix and is not consumed.

### 5.3 Event store

The event body remains canonical JSONL. Its commit logical coordinates are `first_retained` and `next_seq`; the body header remains the semantic source of epoch and session identity. Ordinary append and compaction use the same transaction and alternate-commit algorithm on every platform.

The amendment corrects header `next_seq` to mean the exclusive next unallocated event sequence, matching its name and the commit end coordinate. The first retained event, when present, has `seq == first_retained`; event sequences are dense through `next_seq-1`; an empty body has equality. The prior sentence calling `next_seq` the first record after the header is removed.

`WAKEUP`, durable semantic ACK, and writer durable-cursor advance occur only after the commit record is durable.

### 5.4 Log store

The selected log body prefix contains only the raw retained child-output bytes. The commit record carries the absolute half-open child-output range represented by that prefix.

- Growth below the cap appends and commits.
- Rotation writes the retained suffix into the inactive body and commits its new absolute range.
- `clear` commits an empty body with `start == end == current child-output end`.
- `tail -f` follows commit indexes, not file offsets. If its next coordinate is below the selected start, it emits the exact maximal gap and resumes there.

This preserves the raw log contract while making exact gap reporting and `clear` races determinate.

### 5.5 Lifecycle store

The lifecycle body is one canonical JSON object with a closed key set. Its index-1 `running` record is committed before rendezvous publication. Keys occur exactly in this order: `v`, `type`, `phase`, `session`, `generation`, `wire_generation`, `incarnation`, `start_wall_ms`, `start_mono_ms`, `boot_id`, `path_encoding`, `event_path`, `instrument_path`. Values are respectively `1`, `"lifecycle"`, `"running"`, canonical padded base64 identity, allocated u32 or `null`, wire u32, padded-base64 16 bytes, canonical decimal-string u64, canonical decimal-string u64, padded-base64 16 bytes, `"posix-bytes"` or `"windows-wtf8"`, and padded base64 of each exact native encoding or `null`. That record is the cleanup manifest after holder loss; it is not rendered as `[exited]`.

The index-2 `exited` replacement changes `phase` to `"exited"`, retains every common value, and appends keys `end_wall_ms`, `output_end`, `ended`, then the branch keys below. The two new u64 values are canonical decimal strings. It therefore carries:

- schema version and type;
- canonical session identity;
- allocated generation (`null` when unsupervised) and wire generation;
- holder incarnation;
- wall/monotonic start and boot identity;
- wall-clock end;
- exact platform outcome branch;
- final child-output coordinate.

Outcome branches match event schema 2 exactly: POSIX normal exit is `ended:"exited",code:<u8>`; POSIX signal is `ended:"signalled",signal:<positive platform signal number>` with no `code`; Windows external/normal end is `ended:"exited",code:<u32>`; a holder-caused Windows stop is `ended:"terminated",code:<u32>,method:"graceful"|"forced"`. The foreground shell status for a POSIX signal remains 1 and is not stored as the child code.

All u64 values that can exceed JSON's exact integer range are canonical decimal strings. Identity and opaque byte values use canonical padded base64.

Exit ordering is:

1. replace and commit the lifecycle store with phase `exited`;
2. commit the event-stream `exit` transaction when the event store remains writable;
3. remove the rendezvous object;
4. close the stores and terminate.

At observed child end the holder freezes outcome/final coordinate, lets an already-dispatched log job use only its existing deadline, gives lifecycle exit and event exit at most two seconds each, then performs rendezvous removal. Storage waiting is therefore at most six seconds inside §7.5's ten-second whole-shutdown bound.

If step 1 fails or exceeds its two-second progress deadline, the holder reports storage failure, leaves the rendezvous plus the `running` manifest as stale evidence, and exits by the ten-second shutdown bound; it does not unlink into an undiscoverable state. A crash between later steps produces one of the already specified rendezvous/lifecycle-record cross-product cells. Only an `exited` record without a rendezvous renders `[exited]`.

### 5.6 Cleanup, provisioning, and writer exclusion

Creation performs this order and no other:

1. Resolve and probe the requested session without mutating it. Live or indeterminate refuses.
2. For stale residue, bind the rendezvous and lifecycle manifest to the same canonical identity/generation, then remove only the old log, old event path named by that manifest, lifecycle, and instrumentation stage. Every object is revalidated immediately before removal. A missing or disagreeing manifest makes unowned companions non-removable, never guessed.
3. Provision the requested event target. If absent, create its directory exclusively. If present, accept it only when it is the exact validated empty directory handed off by this caller. Create the four slots exclusively; a leftover slot is refusal.
4. Create log, lifecycle, and optional instrumentation companions; commit all initial records; then publish the rendezvous.

The resolved rendezvous, event, log, lifecycle, and instrumentation objects must be pairwise distinct by canonical path and opened file identity. Any alias is refusal before cleanup/provisioning mutation.

Before publication the creator owns rollback. It records every created file identity and removes only those same identities, in reverse order, after confirming the child/holder did not survive. Once published, the holder owns normal retirement and `rm` owns confirmed-stale cleanup. An uncertain termination never removes anything.

Background launch transfers rollback ownership over its private holder-to-creator result stream using fixed 12-byte records: ASCII magic `MORR`, format byte `01`, state byte (`01` store-adopted, `02` ready, `03` failed), little-endian u16 result code, and little-endian u32 generation. `store-adopted` is sent only after the holder owns every writer lease and captured every object identity; from that record onward the creator never deletes by path. `ready` is sent only after initial commits, child launch gate, and rendezvous publication. EOF/failure before adoption leaves rollback with the creator after confirmed holder death; loss after adoption is resolved by identity probe and otherwise remains indeterminate. Foreground `run` crosses the same states internally.

Every store has a portable exclusive writer lease on `commit.0`: nonblocking `flock(LOCK_EX)` on POSIX and an exclusive `LockFileEx` byte-range lock held through a non-delete-sharing handle on Windows. The holder acquires lifecycle, event, then log leases before publication and holds them until close. Offline `clear`, `rm`, and stale replacement acquire the same applicable order under one two-second total deadline, re-probe liveness after acquisition, and release in reverse order. Failure to acquire or a changed probe is indeterminate and performs no mutation. This serializes competing offline commands as well as holder writes without adding a fifth file.

There is one live writer per store: the holder's store lane. `clear` against a verified-live session must use `LOG_CLEAR`. Admission compares the requested incarnation and observed commit index to current selected state, captures current assigned child-output end `E`, and places an empty replacement barrier after every earlier log job and before every later output job. A mismatch refuses stale status without mutation; success is returned only after the `[E,E)` commit is selected. Loss of `LOG_CLEAR_RESULT` is indeterminate and the CLI never resends automatically. Against confirmed stale residue, `clear` may commit directly while holding the writer lease after the destructive identity fence because no live writer exists. Against indeterminate residue it refuses with exit 1; this is the required correction to the former unsafe liveness-independent write. `tail` remains read-only and liveness-independent. A successor generation never adopts any store body or commit.

### 5.7 Bounded durable I/O

The shared store engine runs one worker lane per enabled store so a stuck log flush cannot block events or lifecycle. The event lane accepts at most 64 queued whole transactions and 512 KiB of serialized queue bytes, enough for the one bounded cap-overage transaction. The log lane accepts at most 64 chunks and 1 MiB. The lifecycle lane accepts one update of at most 4 MiB. Queue admission and serialization occur without waiting for storage.

Initial store commits run in parallel under one two-second launch gate. At runtime, the oldest item in each lane must obtain a selected durable commit within two seconds of admission. Queue overflow on a mandatory holder observation, I/O error, or missed progress deadline atomically closes only that store, clears later queued work, and quarantines its worker; a semantic request that would cross the event queue bound is instead refused before state change with `SEM_RESOURCE_EXHAUSTED`. The main PTY loop never joins or waits for a stuck worker. A worker that returns after quarantine may finish only an already-issued commit flush and must not issue another write. Readers may therefore select either the previous or that one submitted candidate, never a later one.

Closing the event lane clears status/heartbeat event-writable, refuses new semantic ingress before state change, sends no false ACK, and drops later observational events. Closing the log lane clears log-writable, stops copying output to the log while viewer delivery continues, makes a live `clear` fail, and makes `tail -f` drain the last selected prefix then exit 1 with `<program-name>: log store is unavailable` plus LF on standard error. Closing the lifecycle lane clears its health bit; if the child later exits the holder retains the rendezvous as stale evidence. None of these transitions terminates or backpressures a running child.

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

For `-C`, lowercase `k`, `m`, and `g` multiply an otherwise canonical u64 decimal by 1024, 1048576, and 1073741824; overflow is invalid and uppercase suffixes are invalid. Unsuffixed values are bytes. `tail -n 0` emits no existing lines and, with `-f`, follows only bytes committed later.

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

### 6.1 Literal help, version, and option ownership

`<p>` below is the OB-29 rendering of the invoked basename and `<v>` is the build's canonical SemVer 2.0.0 release token (ASCII, no leading `v`, no whitespace). `--version` is exactly `<p> <v>\n`. Help, `-h`, `?`, and no arguments print that version line followed immediately by this exact LF-terminated block to standard output:

```text
Usage:
  <p> <session> [options] [command [argument...]]
  <p> new|start|run [options] <session> [options] [command [argument...]]
  <p> attach [options] <session>
  <p> push <session>
  <p> kill [-f] [-q] <session>
  <p> rm [-q] <session> | <p> rm -a [-q]
  <p> list [-a]
  <p> current
  <p> tail [-f] [-n N] <session>
  <p> clear [<session>]

Attach/create options:
  -e <char>  detach byte (default ^\)
  -E         disable detach
  -r <mode>  child redraw: none, ctrl_l, winch (default none)
  -R <mode>  viewer reset: none, move (default none)
  -z         pass ^Z to the child
  -q         suppress informational messages
  -t         viewer is not VT-compatible

Create-only options:
  -C <size>  log cap (default 1m; 0 disables)
  -2 <path>  redirect child standard error
  -T <path>  event store directory
  -S <path>  launch-time instrumentation object
  -d <path>  child working directory
```

The placeholders are substituted on every line; the angle-bracket spellings shown for operands remain literal. The legacy token table remains normative even though help advertises only modern command names.

Attach/create options are accepted by bare, `-A`, `new`/`n`/`-c`, `start`/`s`/`-n`, `run`/`-N`, and `attach`/`a`/`-a`; create-only options are refused by attach and ignored by no command. `-q` is additionally accepted by `kill` and `rm`. The lifecycle/input/log commands accept only the options displayed on their own help row. Repeated booleans are idempotent; repeated scalar options use the last value; among `-e` and `-E`, the last occurrence wins. Defaults are `-r none`, `-R none`, detach `1C`, suspend `1A`, log cap 1 MiB, and tail count 10.

`-e` accepts either one printable ASCII byte `20..7E` or exactly two bytes of caret notation: `^@` through `^_` map to `00..1F`, and `^?` maps to `7F`. Lowercase after `^` is invalid; a literal caret is the one-byte argument `^`. No locale or Unicode decoding participates.

### 6.2 Exact argument diagnostics

Every argument failure writes exactly two LF lines to standard output and nothing to standard error:

```text
<p>: <message>
Try '<p> --help' for more information.
```

The closed messages are `Invalid mode '<x>'`, `Invalid number of arguments`, `Option '<o>' requires an argument`, `Invalid value '<x>' for option '<o>'`, and `Option '<o>' is not valid for '<command>'`. Tokens and values use OB-29 rendering inside the shown ASCII quotes. Unknown leading-dash tokens use `Invalid mode`; missing/excess operands use `Invalid number of arguments`; the other three cases are literal. All exit 1.

Runtime session-state diagnostics retain the exact templates already frozen in §3/§13 and use standard output plus LF. Sink validation, working-directory failure, instrumentation failure, and child-exec failure use standard error; only child-exec failure uses the already frozen CRLF. `tail` gap/store diagnostics use standard error plus LF. Help/version/informational lines use standard output plus LF. No other stream or line-ending choice remains implicit.

The amendment consolidates the runtime branches into this template matrix; `<name>`/`<path>` use OB-29 rendering and `<error>` is the platform's nonempty single-line error text only on the legacy child-exec row. `<cause>` is exactly one of `missing`, `wrong-type`, `not-directory`, `not-searchable`, `link`, `reparse-point`, `wrong-owner`, `wrong-mode`, `broad-dacl`, `not-empty`, `extra-entry`, `pre-existing-slot`, `outside-root`, `identity-changed`, `io-error`, `wrong-architecture`, or `load-unacknowledged`:

| branch | exact message after `<p>: ` | stream / ending / status |
|---|---|---|
| absent session | `session '<name>' does not exist` | stdout / LF / 1 |
| stale session for live operation | `session '<name>' is not running` | stdout / LF / 1 |
| indeterminate session | `session '<name>' could not be identified` | stdout / LF / 1 |
| create-only against live | `session '<name>' is already running` | stdout / LF / 1 |
| `rm` against live | `session '<name>' is running` | stdout / LF / 1 |
| missing log for `tail` | `no log for session '<name>'` | stdout / LF / 1 |
| no controlling terminal | `no controlling terminal` | stderr / LF / 1 |
| child exited before publication | `child exited before session publication` | stderr / LF / 1 |
| log lane unavailable | `log store is unavailable` | stderr / LF / 1 |
| working directory rejected | `could not enter <path> (<cause>)` | stderr / LF / 1 |
| root rejected | `session root rejected: <path> (<cause>)` | stderr / LF / 1 |
| standard-error sink rejected | `standard-error sink rejected: <path> (<cause>)` | stderr / LF / 1 |
| event target rejected | `event store rejected: <path> (<cause>)` | stderr / LF / 1 |
| instrumentation rejected | `instrumentation rejected: <path> (<cause>)` | stderr / LF / 1 |
| child exec failed | `could not execute <path>: <error>` | stderr / CRLF / 127 |

More specific validation causes are controller/storage error enums and conformance metadata, not localized additions to these CLI lines. Existing exact success/skip/removal/list/current/tail-gap lines remain as already frozen and are copied into the byte-fixture appendix rather than restated inconsistently.

### 6.3 Viewer controls and headless terminal

Detach doubling uses 250 ms measured monotonically. The first configured detach byte is consumed and arms the timer. If the next byte before expiry is the same byte, the arm is cancelled and exactly one detach byte is sent to the child. A different next byte is sent unchanged and then detach completes; expiry or input EOF also completes detach. `-E` bypasses this state machine completely.

Suspend is local viewer byte `1A`; unless `-z` is set it suspends only the viewer process and is not sent on the controller connection. `ctrl_l` is byte `0C`. `move` is bytes `1B 5B 48`, sent after `TERMINAL_STATE` and before replay. A `winch` redraw sends one `RESIZE` even when geometry is unchanged. With `-t`, `TERMINAL_STATE` is empty even when tracking is exact, the ACK may therefore retain tracked-mode exactness, the viewer never becomes a query delegate, and `move` is refused as an invalid option combination; raw child output remains unchanged. This is the second legal empty-preamble branch alongside inexact tracking.

Redraw occurs only if this connection owns the lease after attach; a busy observer never prompts the child. The holder queues the non-interleaved terminal-state/ACK/lease-result prefix and complete frozen replay, then performs the selected `ctrl_l` write or unchanged-size `winch`; output caused by that prompt is live output behind the baseline. The viewer applies `move` locally after `TERMINAL_STATE` and before consuming the following replay frames.

For a headless POSIX creation every mode word starts zero and every control-character slot starts `_POSIX_VDISABLE`. Moor then sets `c_iflag=ICRNL|IXON`, `c_oflag=OPOST|ONLCR`, `c_cflag=CS8|CREAD`, and `c_lflag=ISIG|ICANON|IEXTEN|ECHO|ECHOE|ECHOK|ECHOCTL|ECHOKE`. Input and output speeds are `B38400`. Control bytes are `VINTR=03`, `VQUIT=1C`, `VERASE=7F`, `VKILL=15`, `VEOF=04`, `VSTART=11`, `VSTOP=13`, `VSUSP=1A`, `VREPRINT=12`, `VDISCARD=0F`, `VWERASE=17`, `VLNEXT=16`, `VMIN=01`, and `VTIME=00`; `VEOL` and `VEOL2` remain disabled. Linux also leaves `VSWTC` disabled. macOS sets `VDSUSP=19` and `VSTATUS=14`. A listed symbolic flag or control unavailable on its named supported platform is a build failure, not a silently different default.

### 6.4 Child-start publication boundary

POSIX exec success is the close-on-exec launch pipe reaching EOF; Windows success is the bootstrap's authenticated process-created result. Failure before that point is child-start status 127 and publishes nothing. `attach`, the bare form, `-A`, `new`/`n`, and `-c` validate that the caller has the required controlling terminal before starting anything; without one they fail with status 1, write `<program-name>: no controlling terminal` plus LF to standard error, and leave no child or residue. Headless `start`/`s`/`-n`, `run`, and `-N` use §6.3's default terminal and 80x24 geometry.

If successful exec is followed by observed child exit before atomic rendezvous publication, Moor commits the lifecycle `exited` record and any enabled final event/log state without ever publishing the rendezvous. `run`/`-N` returns the child's normal status (or 1 for a POSIX signal). Every background or attaching creator returns 1, writes `<program-name>: child exited before session publication` plus LF to standard error, and emits no created/started message. The residue is discoverable only as `[exited]` under `list -a`. Exit observed after publication follows ordinary session semantics: background creation has succeeded and returns 0, while `run` returns the child outcome. Publication is the sole boundary, so scheduler timing cannot select another rule.

### 6.5 Instrumentation identity binding

`-S` never gives a later component the caller's path. The creator opens and validates that object once, then copies its exact bytes from the validated handle into an exclusively created staging file in the enforced owner-only per-user root. Its basename is `<H>.instrument`, where `H` is 64 lowercase hexadecimal digits encoding SHA-256 of the wide-length-prefixed canonical session identity, little-endian wire generation, and holder incarnation in that order. This remains protected even for a path-form rendezvous whose parent safety belongs to the caller. The creator flushes the bytes, sets POSIX mode `0500` or the Windows protected read/execute DACL, closes the write handle, reopens the stage without following links, and verifies its recorded file identity and SHA-256. Only this immutable stage path is placed in the loader variable or passed to Windows insertion; the caller path is never reopened.

The stage remains for the session lifetime so POSIX descendants inheriting the preload variable can load the same bytes. The holder keeps a validated read handle and revalidates stage identity after the existing in-module load ACK. Rollback/`rm` removes it only under the same confirmed identity fence as the other companions. `.instrument` joins the reserved-suffix grammar and OB-24 list. This path substitution is explicit compatibility fallout; it closes the former check/use contradiction without relying on platform-specific loader-by-handle behavior.

## 7. Terminal observation and event semantics

### 7.1 Scanner grammar

Freeze these reported forms and no others:

- title: `OSC 0 ; <text> TERM` and `OSC 2 ; <text> TERM`;
- hyperlink: `OSC 8 ; <params> ; <uri> TERM`, including nonempty-target open and empty-target close;
- readiness: the first recognized complete capability query from §4.6's five query classes;
- introducers: OSC is `1B 5D` or `9D`; terminator is BEL `07`, ST `1B 5C`, or C1 ST `9C`;
- title text is arbitrary bytes; hyperlink params are 0..1024 bytes excluding `07`, `1B`, `9C`, and `;`; URI is arbitrary bytes;
- exact malformed-sequence, cancellation, bound, and resynchronization behavior below.

The title/link scanner retains at most 65,536 bytes from introducer through the newest byte. CAN `18` or SUB `1A` cancels an incomplete control string. An OSC with a missing selector/semicolon, a forbidden params byte, or another control introducer before termination is abandoned. On cancellation, malformed input, or byte 65,537, the scanner returns to ground state without changing observed title/link state and reprocesses the first abandoning byte when it can itself begin ESC, OSC, or CSI; otherwise it scans forward to the next such introducer. Observation never changes the forwarded raw bytes.

Add a transition-only, never-snapshotted `observer-degraded` event carrying `scanner` (`"osc"` or `"query"`) and `reason` (`"cancelled"`, `"malformed"`, `"limit"`, or `"deadline"`). One transition is emitted for the first abandonment in each episode when the event store remains writable. An episode begins on abandonment and ends when that scanner next reaches ground state and consumes an ordinary byte or recognizes a complete valid sequence. During the episode observer-exact is false in status/heartbeat; tracked-mode exactness remains independent. If events are disabled/unwritable, those health bits are the required report carrier.

### 7.2 Canonical JSON

Event schema 2 gains a single canonical serializer:

- schema-defined key order per record type;
- no insignificant whitespace;
- UTF-8 output, with non-ASCII characters emitted directly;
- quote and backslash escaped as `\"` and `\\`; backspace, tab, LF, form feed, and carriage return as `\b`, `\t`, `\n`, `\f`, and `\r`; every other U+0000..U+001F scalar as `\u00XX` with uppercase hexadecimal; slash and U+2028/U+2029 are not escaped;
- booleans spelled `true`/`false`;
- existing canonical numeric, decimal-string, and base64 rules retained.

Only Unicode scalar-value strings reach this serializer. The title/link normalization rule replaces malformed UTF-8 and NUL before serialization; native identities remain padded base64/WTF-8 carriers rather than JSON text. Object member order is the exact order listed for each closed key set, arrays preserve semantic order, and duplicate members are impossible. These rules apply equally to lifecycle JSON and delivery-control JSON.

The header order remains `v,type,ts,session,generation,epoch,next_seq,first_retained`. Every event begins `type,ts,epoch,seq,kind`, followed by its additional fields in the left-to-right order of the event-schema table; `observer-degraded` appends `scanner,reason`. Branch-only exit keys follow `ended` in the branch order stated in §5.5. No map iteration order is normative input to serialization.

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

Rejectable producer/input operations serialize and preflight before changing state; a bound/axis failure refuses the operation. Holder-observed facts that cannot be rejected update live state and enqueue their already-preflighted transaction without waiting for disk. Stateful replacement and session ending change their related in-memory facts atomically in the same listed order as the transaction. If the durable result becomes ambiguous, that candidate is the last possible event state and the stream closes; no later transaction can contradict either selected frontier.

Sequence exhaustion rejects the triggering operation and uses the one reserved sequence for only `stream-exhausted{axis:"seq"}`. Epoch exhaustion commits the already-admissible triggering transition followed by the diagnostic in the maximum epoch without compaction. Commit exhaustion, now portable, commits snapshots, the admissible trigger, and the diagnostic at index `FFFFFFFFFFFFFFFF`. Precedence remains sequence, epoch, commit. An unavoidable observation that reaches the sequence case is reflected only in live status/lifecycle state, because claiming it in an event without a sequence would be dishonest.

### 7.4 Consumer gap and dead-letter records

OB-14 uses canonical JSONL delivery-control schema 1, separate from event schema 2 and controller `GAP`. If a consumer expects sequence `F` and the selected header has `first_retained=R>F`, it emits this record before snapshots or retained events:

```json
{"v":1,"type":"gap","session":"<session>","generation":null,"epoch":0,"first_seq":0,"last_seq":0}
```

The shown values are typed placeholders: `session` exactly copies the header string; `generation` copies its u32 or `null`; `epoch` copies its u32; `first_seq=F`; and `last_seq=R-1`. Keys remain in the shown order, numeric spellings follow schema v2, and LF terminates the record. Multiple unseen compactions coalesce into one maximal inclusive range. It has no Moor event sequence and does not advance the Moor cursor.

OB-25 keys failure count by exact `(session,generation,epoch,seq,SHA-256(record-bytes))`. Success clears it. Failures one and two durably retain the count and do not advance the source cursor. On the third failure, one downstream transaction atomically stores this record and advances past the source record:

```json
{"v":1,"type":"dead-letter","session":"<session>","generation":null,"epoch":0,"seq":0,"attempts":3,"record":"<base64>","reason":"consumer-transaction-failed"}
```

Typed placeholders copy the header/event values; `record` is canonical padded base64 of the exact LF-terminated source record. If that atomic transaction fails, the cursor does not advance. Dead letters are durable queryable supervisor state, are never automatically replayed/deleted, and remain until explicit administrative disposition or retirement of the durable logical session lineage.

### 7.5 Whole shutdown deadline

OB-42 has one ten-second monotonic deadline starting when the first accepted termination notification sets the handler/callback flag. The normal wake path abandons peer waits and never waits for a store worker. Graceful child termination begins immediately, escalates at five seconds, and at ten seconds the holder closes remaining handles and exits, leaving rendezvous evidence whenever lifecycle durability or child termination is uncertain. A second notification escalates immediately but never resets either deadline. No peer response, flush, callback, diagnostic, or thread join may extend the ten seconds.

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
- universal event-directory/commit layout, optional creator provisioning, and live-clear RPC;
- bounded storage-worker failure/timeout behavior and expanded health bits;
- exact option ownership, help/diagnostic bytes, detach timing, `NON_VT`, and headless termios;
- prepublication child-exit outcome;
- immutable `.instrument` staging and the resulting loader path;
- delivery-control gap/dead-letter schema and the ten-second shutdown bound;
- `clear` refusing an indeterminate possible live writer.

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
- CI reports the per-file and aggregate count on every change and fails at 4,901. There is no exception, release reserve, override, or borrowing above 4,900; any correction at the cap first removes or simplifies existing production code.
- Every planned task states its budget delta. A task that exceeds its allocation must simplify or remove code before proceeding; borrowing requires an updated aggregate ledger whose absolute total remains at or below 4,900.

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
- the implementation plan contains a 4,900-line budget ledger and an automated hard failure at 4,901 production lines.

No implementation, existing tests, workaround notes, decision briefs, or prior holder history are consulted during this amendment.
