# Wire schema and conformance vectors — version 3

**Companion artefact to [moor-spec.md](./moor-spec.md).** §10.2 of that document fixes what this schema must satisfy; this file fixes the shapes. Where the two disagree the specification wins and this file is a defect.

**Version:** `wire-schema-3`. Controller frames use version byte `03`; semantic-producer frames use their separately versioned `MOOS` header (§14). Integer layouts are portable, while native paths are raw bytes on POSIX and canonical WTF-8 on Windows. This revision also freezes the Windows marker (§12), event-commit record (§13), and private supervised/instrumentation launch records (§15). Referenced by the specification as *the accompanying vectors* (§0.2). A change to any frozen value here is a version increment, never an edit in place.

**Integer encoding:** all multi-byte integers are unsigned, **little-endian**, of the stated width. There is no variable-length encoding anywhere.

---

## 1. Frame header

Every frame begins with a fixed 24-byte header.

| offset | width | field | value / notes |
|---|---|---|---|
| 0 | 4 | magic | the four bytes `4D 4F 4F 52` in file order, in that order — the program's name in ASCII. It is a **byte sequence, not an integer**, so it is unaffected by byte order |
| 4 | 1 | version | `03` for this schema |
| 5 | 1 | type | §2 |
| 6 | 1 | flags | bit 0 = `MORE` (continued, §10.2.2); bits 1–7 reserved, must be `0` |
| 7 | 1 | reserved | must be `00` |
| 8 | 4 | generation | §10.1; normally nonzero. `00000000` is legal only on the first controller `HELLO` as §3.1's discovery sentinel; an unsupervised holder sends `00000001`; supervised allocation starts at `00000002` |
| 12 | 4 | sequence | per-direction, strictly increasing by one |
| 16 | 4 | payload length | bytes following the header |
| 20 | 4 | header checksum | **CRC-32C** (Castagnoli, polynomial `0x1EDC6F41`, initial value `0xFFFFFFFF`, reflected input and output, final XOR `0xFFFFFFFF`) computed over bytes 0–19 |

**The frame sequence is per connection and is not the event stream's sequence.** They are different counters with different lifetimes and must not be confused:

| | frame sequence (this field) | event `seq` (§8.4.1 of the specification) |
|---|---|---|
| scope | one direction of one connection | the durable event stream of one session |
| starts at | `1` on every new connection | `0` on a new stream |
| resets | on every reconnect | never |
| exhaustion | `FFFFFFFF` is reserved; reaching it closes the connection with `RESOURCE_EXHAUSTED`, and the controller reconnects with a fresh counter | the reserved maximum carries the exhaustion record itself (OB-28) |

Neither wraps. Wrapping to zero is forbidden in both. Zero is not a generation; the initial-`HELLO` discovery sentinel is a control value and is never echoed as one.

**Bounds.** A single frame's payload is at most **1 MiB** (`00100000`). A reassembled message is at most **16 MiB** (`01000000`). A frame declaring more than the frame bound is `OVERSIZED_FRAME`; a run exceeding the message bound is `OVERSIZED_MESSAGE`. Neither is an allocation.

Per holder, at most 16 sockets await the initial preface/hello, 64 authenticated controller connections exist, and 64 authenticated semantic connections exist. Excess peers are refused without evicting an existing peer or affecting the child. In-progress reassembly storage is additionally capped at 64 MiB aggregate across the holder; the fragment that would cross it is refused with `RESOURCE_EXHAUSTED` and only that connection closes. Declared lengths never allocate the bound eagerly. §14.2 separately caps retained source identities at 64.

**Reserved bits and fields must be zero.** A non-zero reserved value is `MALFORMED_FRAME`. Ignoring it is what turns a version mismatch into silent data loss.

### 1.1 Length-prefixed fields

Wherever a field is described as *length-prefixed*, it is:

| width | field |
|---|---|
| 2 | byte count, unsigned, little-endian, at most `1000` (4096) |
| n | the bytes themselves |

**The bytes are raw and opaque unless the field explicitly says text or native path.** No terminator is present. POSIX native paths are raw bytes. Windows native paths are canonical WTF-8 converted losslessly from UTF-16, including unpaired surrogates; a decoder MUST reject a non-canonical or non-round-tripping form. A length of zero is legal only where the field says so; it is distinct from absence.

#### 1.1.1 Wide identity and native-path prefixes

Every field explicitly described as *wide-length-prefixed* uses a 4-byte unsigned little-endian byte count followed by that many bytes. The count is at most **1 MiB** (`00100000`); the enclosing frame and reassembled-message bounds still apply independently. Only canonical session identities and native path fields use this form. This is deliberately separate from the compact 2-byte prefix: a valid Windows native path can require substantially more than 4096 canonical WTF-8 bytes, so applying §1.1's limit to a working directory or event-stream path would make an otherwise valid session impossible to describe. An over-limit value is refused before publication rather than truncated.

### 1.2 Tagged canonical session identity

Every field named *canonical session identity* is one wide-length-prefixed byte string (§1.1.1) whose first byte is a kind tag:

| tag | exact following bytes |
|---|---|
| `01` | Linux/macOS absolute socket-path bytes after lexical `.`/`..` resolution, without following symbolic links |
| `02` | Windows marker identity: 8-byte little-endian volume serial number, then the exact 16 bytes of `FILE_ID_INFO.FileId`, queried from the same-directory staged marker and required to match after publication |

Tag `02` therefore has total content length 25. An unknown tag or a wrong fixed length is `IDENTITY_MISMATCH`, never a path to normalise.

## 2. Frame types and their payloads

| value | name | direction | payload |
|---|---|---|---|
| `01` | `HELLO` | controller → holder | §3.1 |
| `02` | `HELLO_ACK` | holder → controller | §3.2 |
| `03` | `ATTACH` | controller → holder | geometry (§4), then 1 byte flags: bit 0 = request the input lease |
| `04` | `ATTACH_ACK` | holder → controller | the status descriptor, §5 |
| `05` | `TERMINAL_STATE` | holder → controller | the preamble, §6 |
| `06` | `OUTPUT` | holder → controller | 8 bytes record sequence, 8 bytes byte offset, then the raw bytes to end of payload |
| `07` | `OUTPUT_ACK` | controller → holder | 8 bytes: highest record sequence consumed |
| `08` | `GAP` | holder → controller | 8 bytes first lost sequence, 8 bytes last lost sequence |
| `09` | `INPUT` | controller → holder | §7.1 |
| `0A` | `INPUT_RECEIPT` | holder → controller | §7 |
| `0B` | `RESIZE` | controller → holder | 4 bytes lease epoch, then geometry (§4) |
| `0C` | `QUERY_REPLY` | controller → holder | 4 bytes correlation id, 4 bytes lease epoch, then length-prefixed reply bytes |
| `14` | `QUERY` | holder → controller | 4 bytes correlation id, 4 bytes lease epoch, 1 byte class (§8), then length-prefixed query bytes |
| `0D` | `STATUS` | controller → holder | empty |
| `0E` | `STATUS_REPLY` | holder → controller | the status descriptor, §5 |
| `0F` | `TERMINATE` | controller → holder | §9 |
| `10` | `TERMINATE_RESULT` | holder → controller | outcome, containment result, termination method, then length-prefixed diagnostic (§9.2) |
| `11` | `WAKEUP` | holder → controller | empty — the event stream advanced (OB-30) |
| `12` | `HEARTBEAT` | holder → controller | §10 |
| `13` | `ERROR` | either | 2 bytes code (§11), then a nonempty length-prefixed diagnostic |

An unknown type closes the connection with `UNKNOWN_TYPE`. It is never skipped.

Every unassigned payload flag bit and enum value is reserved. A controller frame carrying one is `MALFORMED_FRAME`; it is never ignored as a forward-compatible extension. In particular, status-descriptor flags 5–7 and termination containment bits 4–7 are zero.

## 3. The identity exchange

### 3.1 `HELLO`

| width | field |
|---|---|
| 4 | magic (repeated, so a stray connection is rejected before parsing) |
| 1 | schema version the controller speaks |
| 2 | reserved flags, zero; hello is side-effect free and only `ATTACH` may request the input lease |
| var | canonical session identity the controller believes it is reaching (OB-17), wide-length-prefixed |

The header generation is either the exact expected nonzero generation or zero to discover the authenticated holder's current generation. Zero is accepted only on this first frame. Nonzero hello flags are `MALFORMED_FRAME`; lease arbitration begins only with `ATTACH`. A supervisor adopting a launch MUST use the nonzero generation it allocated; discovery cannot satisfy the adoption gate. A human attach or liveness probe without allocator state may discover, then uses the nonzero generation returned by `HELLO_ACK` on every later frame.

### 3.2 `HELLO_ACK`

| width | field |
|---|---|
| 1 | schema version the holder speaks |
| 4 | generation (§10.1 allocation, or `1` unsupervised) |
| 16 | **holder incarnation** — an opaque value generated once per holder process, never reused |
| var | canonical session identity as the holder resolved it, wide-length-prefixed |

A mismatch between the two identities is `IDENTITY_MISMATCH` and the connection closes. A nonzero header generation that differs from the holder is `GENERATION_MISMATCH`; a discovery `HELLO_ACK` carries the holder's actual nonzero generation in both header and payload. This is what stops a controller adopting a successor session that happens to occupy the same path without preventing a human who lacks the supervisor's allocator state from attaching safely.

---

## 4. Geometry

| width | field |
|---|---|
| 2 | columns |
| 2 | rows |

**Zero in either dimension means *preserve*** (OB-19). Both zero preserves both. One zero and the other not is `HALF_SPECIFIED_GEOMETRY`. Valid non-zero ranges: columns and rows 1–1000, and the product at most 2,000,000.

---

## 5. The status descriptor

Carried by `ATTACH_ACK` and `STATUS_REPLY` (OB-39).

| width | field | obligation |
|---|---|---|
| var | canonical session identity, wide-length-prefixed | OB-17 |
| 4 | generation | §10.1 |
| 16 | holder incarnation | — |
| 1 | event storage layout: `00` disabled, `01` POSIX single file, `02` Windows dual body/commit | OB-39 |
| var | event-stream identity, wide-length-prefixed native path; empty only when layout is `00` | OB-39 |
| 1 | active event body slot: `00`/`01` for layout `02`, otherwise `FF` | §8.4.2 |
| 8 | active event commit index; zero outside layout `02` | §8.4.2 |
| 8 | active committed body length; zero outside layout `02` | §8.4.2 |
| 32 | active committed body SHA-256; all zero outside layout `02` | §8.4.2 |
| 8 | start, wall clock, milliseconds since the Unix epoch | OB-31 |
| 8 | start, monotonic milliseconds since the holder's boot identity | OB-31 |
| 16 | **boot identity** — an opaque value stable for the lifetime of the operating-system boot | OB-31 |
| var | child working directory, wide-length-prefixed native path | OB-32 |
| 4 | child process identifier | OB-35 |
| 4 | **child containment-set token** — process-group identifier on Linux/macOS; holder-minted nonzero token unique within the holder incarnation on Windows, never a job-object id | OB-35 |
| 16 | **child birth token** — an opaque value derived at child creation, not reused when a process identifier is | OB-35 |
| 8 | retained history, first output record sequence present; zero iff empty | — |
| 8 | retained history, last output record sequence present, inclusive; zero iff empty | — |
| 8 | retained history, first byte offset still present | — |
| 8 | retained history, exclusive end byte offset | — |
| 1 | flags: bit 0 = retained raw output is complete from record 1/byte 0; bit 1 = the tracked terminal-mode state is exact; bits 2–3 zero (wire v3 has no screen checkpoint or buffer-exactness claim); **bit 4 = the input lease is granted to this controller**; bits 5–7 zero | §6.7 |
| 4 | **lease epoch** — the current epoch, whether or not this controller holds it | §6.1 |
| 1 | semantic flags: bit 0 = at least one stateful source is exact; bit 1 = at least one stateful source has degraded or disconnected evidence; bit 2 = at least one source can prepare application-receipt input; bits 3–7 zero | §10.3 |
| 2 | holder-wide pending application-receipt correlation count (0–512) | §10.3.4 |

**Both clock fields are present, always.** The wall clock is for display; age is computed from the monotonic value, and only when the boot identity matches the consumer's own — otherwise age is reported as unknown rather than wrong (OB-31). This is the resolution of "monotonic basis *or* boot identity": it is both, and the boot identity is what makes the monotonic value comparable.

Linux/WSL carries the 16 parsed UUID bytes from `/proc/sys/kernel/random/boot_id`; macOS carries little-endian `kern.boottime` seconds in bytes 0–7, microseconds in bytes 8–11 and ASCII `MAC1` in bytes 12–15; Windows carries documented WMI `LastBootUpTime` converted to UTC FILETIME ticks in little-endian bytes 0–7 with bytes 8–15 zero. Sixteen zero bytes mean unavailable and never compare equal. The matching monotonic clocks and failure rules are frozen in specification §12.6. The active event fields are copied from the same validated commit record a reader would select, not from uncommitted writer state.

---

## 6. The terminal-state preamble

`TERMINAL_STATE` carries a length-prefixed run of raw bytes that a viewer writes into its own emulator. It is sent **exactly once per attaching connection, before `ATTACH_ACK`**, and **carries the connection's own generation** like every other frame — the header rule of §1 admits no exception. Its connection-locality is expressed by the fact that it carries **no record sequence and no byte offset**, so it cannot advance any output cursor and is never logged (§10.2.6). A zero-length run is legal exactly when tracked-mode exactness is false; the following ACK must then clear its mode-exact bit. A probe that never sends `ATTACH` receives no preamble.

**The tracked mode set is exactly twelve, and their canonical bytes are frozen.** All twelve are emitted on every attach, in the order below — see the note after the table for why never only the deviations.

| # | mode | canonical restoration bytes |
|---|---|---|
| 1 | alternate screen | set `1B 5B 3F 31 30 34 39 68`; reset `1B 5B 3F 31 30 34 39 6C` |
| 2 | character set G0 | line drawing `1B 28 30`; ASCII `1B 28 42` |
| 3 | character set G1 | line drawing `1B 29 30`; ASCII `1B 29 42` |
| 4 | auto-wrap | set `1B 5B 3F 37 68`; reset `1B 5B 3F 37 6C` |
| 5 | scroll region | non-default: `1B 5B` *top* `3B` *bottom* `72`, where *top* and *bottom* are decimal ASCII digits without leading zeros — rows 1–24 are `1B 5B 31 3B 32 34 72`; default: `1B 5B 72` |
| 6 | origin mode | set `1B 5B 3F 36 68`; reset `1B 5B 3F 36 6C` |
| 7 | application cursor keys | set `1B 5B 3F 31 68`; reset `1B 5B 3F 31 6C` |
| 8 | bracketed paste | set `1B 5B 3F 32 30 30 34 68`; reset `1B 5B 3F 32 30 30 34 6C` |
| 9 | mouse reporting | first reset all three, in order: `1B 5B 3F 31 30 30 30 6C`, `1B 5B 3F 31 30 30 32 6C`, `1B 5B 3F 31 30 30 33 6C`; then set each tracked enabled bit in the same order with final byte `68`. Off emits only the three resets |
| 10 | mouse encoding | first reset both, in order: `1B 5B 3F 31 30 30 35 6C`, `1B 5B 3F 31 30 30 36 6C`; then set each tracked enabled bit in the same order with final byte `68`. Default emits only the two resets |
| 11 | focus reporting | set `1B 5B 3F 31 30 30 34 68`; reset `1B 5B 3F 31 30 30 34 6C` |
| 12 | cursor visibility | set `1B 5B 3F 32 35 68`; reset `1B 5B 3F 32 35 6C` |

**The update grammar is frozen as well as the restoration bytes.** State starts at the reset/default value for every group. The incremental scanner recognises:

- `CSI ? Pm h` and `CSI ? Pm l`, with one or more semicolon-separated decimal parameters, updating every listed parameter that belongs to the table (`1`, `6`, `7`, `25`, `1000`, `1002`, `1003`, `1004`, `1005`, `1006`, `1049`, `2004`). Leading zeros are accepted because terminals interpret the numeric value; an empty parameter has no tracked effect;
- `ESC ( 0`/`ESC ( B` and `ESC ) 0`/`ESC ) B` for G0/G1;
- `CSI top ; bottom r` for the scroll region, where an omitted/empty top defaults to 1 and an omitted/empty bottom defaults to the current row count, plus `CSI r` for the full-screen default. Leading zeros are accepted; a zero parameter has its terminal default meaning; and
- `ESC c` (RIS), which returns every tracked group to its default and restores tracked-mode exactness.

A private-mode list may update several tracked bits in one sequence; treating only its first parameter is nonconforming. An invalid, over-range or abandoned sequence changes no stored value. Because its intended effect is unknowable, it clears the status descriptor's tracked-mode-exact bit; that bit becomes set again only after RIS. A syntactically valid sequence outside this closed grammar normally passes through without affecting the tracked set, but a sequence that changes one of these same state families to an unrepresentable value — for example another G0/G1 designation or an alternate-buffer control other than tracked `1049` — also clears exactness. This bounded state is solely for the attach preamble and never changes the child bytes delivered to a viewer.

**No cursor position is carried, saved or live.** An earlier version of this schema restored a saved cursor, which would require the holder to track where the cursor is — exactly what §9.1 of the specification forbids and what makes the cursor-position query viewer-only (§8). A viewer that arrives mid-session does not learn where the cursor was, and that is correct: the child's next output puts it wherever the child wants it.

**When tracked-mode state is exact, all twelve groups are emitted in this order — never only the deviations.** Emitting only what differs from a default requires knowing the viewer's default, and the holder does not: viewers differ, and a wrong assumption leaves a mode set on one viewer and clear on another from the same session. Mouse groups deliberately begin by clearing every constituent bit, so an arbitrary combination left in the viewer cannot survive merely because the child selected a different member. The complete canonical restoration block removes the guess entirely. When exactness is false, none of these controls is guessed: the one required frame carries a zero-length run.

**Order is frozen because it is not commutative.** The target screen buffer is selected first because switching buffers can reset buffer-local state; the scroll region and origin mode are restored only afterwards. The prior order restored the scroll region and then switched buffers, immediately erasing the state it claimed to restore.

Nothing outside this table is tracked. A mode not listed is the child's business and passes through unremarked (§9.1).

### 6.1 Output record coordinates

An `OUTPUT` message is nonempty. Its record sequence starts at `1` for the holder incarnation and increases by exactly one per complete reassembled output message. Its byte offset is the zero-based absolute offset of the payload's first byte in that incarnation's child-output stream. The next output offset is exactly `offset + payload_length`; gaps and overlaps are malformed at the producer and detectable by the consumer. The status descriptor's retained byte range is half-open `[first, end)`, while its record range is inclusive. Empty history is encoded only as record range `0,0` and equal byte endpoints. Nonempty history has `1 <= first_record <= last_record` and `first_byte < end_byte`. V2's two bytes occupy offsets 4096 and 4097 with exclusive end 4098. A `GAP` names a nonempty **inclusive** range of lost record sequences (`first <= last`, neither zero); it never pretends to name byte offsets that are no longer known.

Both coordinates are unsigned u64 and never wrap. Record sequence `FFFFFFFFFFFFFFFF` may name the final output record. An output is admitted only when its exclusive end is representable as u64. If another child byte would require a successor after the maximum sequence or an end above `FFFFFFFFFFFFFFFF`, the holder sends `RESOURCE_EXHAUSTED` to every authenticated controller, marks the output path failed, and terminates its owned session through §12.4 of the specification rather than silently dropping or misnumbering output. `OUTPUT_ACK` value zero means no output record consumed; any acknowledgement above the highest record sent is `BAD_SEQUENCE`.

Each `OUTPUT` payload carries 1–65536 child bytes. The holder retains complete newest records up to 4 MiB of payload per holder incarnation. After `TERMINAL_STATE` and `ATTACH_ACK`, every attach receives a frozen baseline: `GAP{1, first_retained-1}` when `first_retained > 1`, then every record from the ACK's inclusive first/last range, then later live records. Empty history emits neither. The connection serialises output that arrives during replay after that baseline. A controller with an existing cursor discards duplicate record sequences; a new controller applies all of them. Bit 0 in the ACK is set exactly when the empty history is still at byte offset zero or the nonempty retained range begins at record 1/byte 0. This is raw replay only: there is no checkpoint frame in wire v3.

## 7. `INPUT` and `INPUT_RECEIPT` — the transport receipt

### 7.1 `INPUT`

| width | field |
|---|---|
| 4 | **lease epoch** — the epoch of the input lease this controller holds (§6.1 of the specification) |
| 8 | **request id** — monotonic per lease epoch, chosen by the controller |
| 1 | flags: bit 0 = `APPLICATION_RECEIPT_REQUIRED`; bits 1–7 zero |
| 16? | application request id, present iff bit 0 is set; all-zero is forbidden |
| var? | semantic source id, length-prefixed, present iff bit 0 is set; ASCII grammar from §14.2 |
| n | the bytes to write |

**The frame sequence is not the request id**, and an earlier version of this schema said it was. Three things break that: a fragmented input spans several frame sequences, so there is no single one to name; a retry would have to reuse a frame sequence, violating strict increase; and a reconnect resets the frame counter, so the same value would refer to a different request. The request id is separate, carried explicitly, and survives both.

A frame whose lease epoch is not the current one is refused with `LEASE_NOT_HELD` and **nothing is written**. When bit 0 is set, the holder requires an active stateful source advertising `APPLICATION_RECEIPT` and `INPUT_NOTICE`, sends §14.6's notice, and receives its prepared ACK within 2 seconds before writing any terminal byte. That ACK is valid only while the same producer instance and source epoch remain current; replacement before the write is `APPLICATION_SOURCE_UNAVAILABLE` and nothing is written. The application id, source id, flags and terminal bytes are all part of the exact replay identity. Reusing an application id while a pending/retained correlation or the cached input-replay entry binds it to a different tuple or digest is `APPLICATION_ID_CONFLICT`. Once every such bounded binding has resolved or expired, the id may accompany a later never-reused lease/request tuple; the holder does not maintain an unbounded generation-long id set, and stale receipts still fail the complete tuple check.

### 7.2 `INPUT_RECEIPT`

| width | field |
|---|---|
| 4 | lease epoch, echoed |
| 8 | request id, echoed |
| 4 | generation as matched |
| 16 | holder incarnation as matched |
| 8 | byte count written to the pseudo-terminal |
| 1 | status: `00` written, `01` refused |
| 2 | result code: zero when written; on refusal, the applicable controller error code from §11 |

**A successful receipt is sent only when the complete write has finished.** There is no success meaning *queued but not yet written*. Status `01` reports a known refusal or an incomplete write; its byte count is the number actually completed, zero for a pre-write refusal, and its result code is nonzero. A partial or failed terminal write uses `INPUT_WRITE_FAILED`. While the outcome is still pending no receipt is sent. Every result frame, written or refused, is cached against the complete request; an identical retry returns that cached payload in a frame carrying the next holder-to-controller frame sequence and never re-evaluates or writes the request. A failed prefix is therefore never duplicated silently, and a lost pre-write refusal cannot turn into a later write under the same identity. Input-specific refusals use this result frame rather than a separate `ERROR`; a connection-level error that prevents the request identity from being parsed has no receipt and does not advance request state.

### 7.3 Replay semantics, frozen

- **One input is in flight at a time per lease epoch.** A controller sends the next request only after the previous one is acknowledged or the lease is lost. This is what makes the rules below expressible at all.
- **A new lease epoch sets the high-water mark to `0`; the first request id of an epoch is `1`.** Zero is never a request id, so the initial state is unambiguous.
- The holder retains the high-water request id **and the exact complete request payload and receipt payload for that one request** — flags, application id, source id and terminal bytes, not a history. Written, pre-write-refused and partial-write-refused receipts are retained identically. Transport-header fields, including frame sequence and fragmentation, are not part of replay identity.
- A request id **equal** to the high-water mark is the replay case: the holder compares that complete payload. **Identical metadata and bytes** → it writes nothing and returns the cached receipt payload in a newly sequenced frame. **Any difference under the same id** → `BAD_SEQUENCE`, and **nothing is written**; a controller reusing an id for different content has lost track, and writing would deliver input the receipt does not describe.
- A request id **below** the high-water mark is refused with `BAD_SEQUENCE` and **nothing is written**. An earlier version of this schema returned the cached receipt for anything at or below the mark, which answers an older request with a newer request's receipt — a wrong answer presented as a correct one.
- A request id **more than one above** the high-water mark is `BAD_SEQUENCE`: the controller has skipped, and writing would deliver input out of order.
- Exactly one value can advance the mark: **high-water plus one**. Once its written or refused `INPUT_RECEIPT` outcome is determined, that request and outcome become the new high-water entry. A framing or connection-level failure that cannot produce a receipt leaves the mark unchanged.

Request ids are unsigned 64-bit values and never wrap. `FFFFFFFFFFFFFFFF` may be the final new request in a lease epoch and remains exactly replayable; after it becomes the high-water mark, any non-replay request is `RESOURCE_EXHAUSTED` until a newly granted lease supplies a fresh epoch. Lease epochs follow §6.1 of the specification and likewise never wrap.

**This is what makes a retry safe.** Without it, the only recovery from an unanswered input is to risk writing it twice — which for an agent prompt means submitting it twice.

**What the receipt proves:** the frame was accepted, the lease epoch, generation and incarnation matched, and exactly the stated number of bytes completed their write to the pseudo-terminal.

**What it does not prove, stated here because the field layout is where an implementer looks:** that the program running under the terminal read those bytes, parsed them, or acted on them. No field carries that, because no holder can observe it. That evidence is **OB-37**, owned downstream.

## 8. Capability arbitration

The holder detects a query **incrementally**, across arbitrary read boundaries (§10.2.7), never by matching within a single read. Detection runs a state machine over the child's output: an escape introducer, then a parameter run, then a final byte. A byte run that resembles a query while inside another sequence is not one.

**The frozen query classes, their exact grammar, and the holder's exact reply.** Nothing else is answered.

| class | query bytes | holder's reply |
|---|---|---|
| primary device attributes | `1B 5B 63` or `1B 5B 30 63` | `1B 5B 3F 36 32 3B 34 63` |
| secondary device attributes | `1B 5B 3E 63` or `1B 5B 3E 30 63` | `1B 5B 3E 31 3B 34 37 3B 30 63` |
| terminal name and version | `1B 5B 3E 30 71` | `1B 50 3E 7C 6B 69 74 74 79 28 30 2E 34 37 2E 30 29 1B 5C` |
| mode query | `1B 5B 3F` *mode* `24 70`, where *mode* is decimal ASCII digits without leading zeros — querying bracketed paste is `1B 5B 3F 32 30 30 34 24 70` | `1B 5B 3F` *mode* `3B` *state* `24 79`, the same *mode* digits echoed, *state* one byte: `31` set, `32` reset, `30` when the mode is outside §6 — so the reply for bracketed paste set is `1B 5B 3F 32 30 30 34 3B 31 24 79` |

**No reply contains a trailing NUL.** The current implementation appends one because it measures its buffer rather than its string; that byte is not part of any reply and a child that reads it sees stray input.

**The holder answers only when it supplied the identity itself.** §4.4.2 of the specification preserves an inherited terminal identity and injects one only when none was inherited. So there are two cases, and conflating them is a real contradiction an earlier version of this schema contained:

- **The holder injected the identity** (nothing was inherited). It knows what it claimed, the replies above match it exactly, and it answers.
- **The identity was inherited** from some other terminal. The holder does not know that terminal's device attributes, cannot obtain them, and **must not fabricate them**. It does not answer these three classes at all; the query passes through, and an attached viewer answers or nothing does.

This is the same rule as the cursor-position query, for the same reason: a synthetic answer is only honest when the holder is the thing being asked about. A child told by its environment that it is talking to one terminal and answered with another's attributes behaves erratically in ways that are extremely hard to diagnose.

**Cursor position is viewer-only.** A cursor-position report can only be answered by something that knows where the cursor is, and §9.1 of the specification forbids the holder from knowing that. When a viewer holds the lease the query passes through and the viewer answers. **When no viewer is attached the query is not answered at all** — a headless session gets silence, which is the honest outcome. Synthesising a position would require the screen model this document exists to keep out.

**A tracked-mode query is synthetic only while tracked-mode exactness is true.** If §6's exactness bit is clear, the lease holder may still answer, but after its 250 ms opportunity a silent viewer is followed by holder silence. The `state` byte `30` is reserved for a mode outside §6 and MUST NOT be used to disguise unknown tracked state. After RIS restores exactness, synthetic set/reset answers may resume.

**The OB-20 opt-out disables every holder-generated reply in this section.** It does not suppress a lease-holding viewer's reply and it does not alter terminal-identity injection.

**Arbitration.** When a viewer holds the input lease, the holder waits **250 ms** for it to answer. If it does, the holder stays silent. If it does not, the holder answers only when the holder injected the terminal identity, OB-20 is not set, the query is one of the synthetic classes above, and a tracked-mode query has exact tracked state. With an inherited identity, an opt-out, or inexact tracked state for a mode query it remains silent, and it never answers cursor position. An observer without the lease never answers. A `QUERY_REPLY` whose correlation id matches no outstanding query, or whose lease epoch is superseded, is discarded and never reaches the child.

**A partial write of a reply is completed**, not abandoned.

## 9. Termination

### 9.1 `TERMINATE`

| width | field |
|---|---|
| var | expected canonical session identity, wide-length-prefixed |
| 4 | expected generation |
| 16 | expected holder incarnation |
| 1 | flags: bit 0 = force immediately, skipping the grace period |

Any mismatch is `REFUSED_IDENTITY` and **nothing is done**.

### 9.2 `TERMINATE_RESULT`

| width | field |
|---|---|
| 1 | outcome (below) |
| 1 | **containment result** (§12.4 of the specification): bit 0 = the foreground process group was signalled — Linux and macOS only; bit 1 = the child's containment set was ended, meaning its process group on Linux and macOS or its job object on Windows; bit 2 = escalation to the platform's unconditional mechanism occurred; bit 3 = **at least one known thing the session started outlived it** — platform-neutral; clear means no survivor was observed, not proof that an unobservable detached descendant does not exist |
| 1 | termination method: `00` none/not applicable, `01` graceful, `02` forced. A Windows holder-caused exit event uses the same known method; reserved values are malformed |
| var | length-prefixed diagnostic: empty exactly for `TERMINATED`/`ALREADY_GONE`, nonempty for every other outcome |

| value | outcome | meaning |
|---|---|---|
| `00` | `TERMINATED` | the child ended and the addressable socket or marker is unlinked |
| `01` | `ALREADY_GONE` | there was nothing to terminate; the addressable socket or marker is absent |
| `02` | `REFUSED_IDENTITY` | an expected value did not match; **nothing was done** |
| `03` | `INDETERMINATE` | the outcome could not be established within the deadline; **nothing may be assumed** |
| `04` | `FAILED` | termination was attempted and did not complete |

Bit 3 is what makes §12.4 of the specification actionable: a supervisor learns that a survivor is *expected* rather than discovering it later by scanning the process table.

### 9.3 Deadlines

Every deadline the specification promises as "stated", stated:

| operation | deadline | on expiry |
|---|---|---|
| graceful termination, before escalating | 5 s | escalate to the platform's unconditional mechanism (§12.4 of the specification) |
| whole termination operation | 10 s | report `INDETERMINATE` |
| identity exchange and adoption (§3, §5) | 2 s | `DEADLINE_EXCEEDED`, the connection closes, the session stays `indeterminate` |
| capability arbitration, waiting for the viewer | 250 ms | the holder answers, subject to §8 |
| launch-time instrumentation load acknowledgement (OB-22) | 2 s | the launch fails; no session is left behind |
| lease release after its holder stops responding | 10 s | the lease is released and its epoch increments |
| incomplete reassembly run | 5 s | `REASSEMBLY_TIMEOUT` |
| heartbeat absence before verified-live evidence expires | 15 s | invalidate that evidence and begin a fresh bounded identity probe; `indeterminate` until the probe resolves |

## 10. `HEARTBEAT` and liveness

| width | field |
|---|---|
| 8 | holder monotonic milliseconds |
| 1 | flags: bit 0 = the child is running; bit 1 = the event stream is writable |

Sent every **5 seconds** while a controller is attached, and immediately on any change to those flags.

**This is the liveness surface, and `WAKEUP` is not** (OB-30). A `WAKEUP` says the event stream advanced; silence on it is the normal state of a quiet session and means nothing. Absence of `HEARTBEAT` past **15 seconds** invalidates the connection's verified-live evidence and triggers a fresh bounded identity probe. Until the probe positively establishes listener absence or completes an authenticated exchange, the session is `indeterminate`; heartbeat loss alone never proves the holder is gone. Conflating `WAKEUP` silence with heartbeat behavior is how a healthy idle session gets reported as lost.

---

## 11. Error codes

A closed set. Every refusal names one.

| value | name |
|---|---|
| 1 | `UNKNOWN_VERSION` |
| 2 | `UNKNOWN_TYPE` |
| 3 | `OVERSIZED_FRAME` |
| 4 | `OVERSIZED_MESSAGE` |
| 5 | `MALFORMED_FRAME` |
| 6 | `BAD_SEQUENCE` |
| 7 | `REASSEMBLY_ABORTED` |
| 8 | `REASSEMBLY_TIMEOUT` |
| 9 | `GENERATION_MISMATCH` |
| 10 | `IDENTITY_MISMATCH` |
| 11 | `UNAUTHORISED_PEER` |
| 12 | `DEADLINE_EXCEEDED` |
| 13 | `RESOURCE_EXHAUSTED` |
| 14 | `HALF_SPECIFIED_GEOMETRY` |
| 15 | `LEASE_NOT_HELD` |
| 16 | `PREAMBLE_OUT_OF_ORDER` |
| 17 | `APPLICATION_SOURCE_UNAVAILABLE` |
| 18 | `APPLICATION_ID_CONFLICT` |
| 19 | `APPLICATION_NOTICE_TIMEOUT` |
| 20 | `INPUT_WRITE_FAILED` |

**Reassembly deadline:** an incomplete run is abandoned after **5 seconds**.

---

## 12. Windows rendezvous marker

The addressable Windows path contains exactly one marker record. Its file length is exactly **84 bytes**: 38 fixed bytes plus the 46-byte pipe name.

| offset | width | field |
|---|---|---|
| 0 | 8 | magic bytes `4D 4F 4F 52 4D 52 4B 33` (`MOORMRK3`) |
| 8 | 1 | marker format `01` |
| 9 | 1 | flags, zero |
| 10 | 2 | reserved, zero |
| 12 | 4 | session generation; unsupervised uses `1` |
| 16 | 16 | holder incarnation |
| 32 | 2 | named-pipe byte length, exactly 46 (`2E 00`) |
| 34 | 46 | ASCII `\\.\pipe\moor-` followed by exactly 32 lowercase hexadecimal digits encoding the holder's 16 fresh random bytes |
| 80 | 4 | CRC-32C over bytes 0–79 |

The marker is parsed only after its regular-file, non-reparse, owner and protected-DACL checks. Wrong total length, pipe length, prefix, case or hexadecimal grammar is malformed; the name is already ASCII and requires no permissive path normalisation. Its pipe name is not a session identity. The tagged marker `FILE_ID_INFO` identity from §1.2 is queried on the staged marker so the initial event header can be committed, then required to match the final marker after atomic publication before any connection is admitted. Replacing the marker necessarily changes that identity.

## 13. Windows event commit record

Each `commit.0`/`commit.1` file is empty (invalid) or contains exactly this 76-byte record. Multi-byte integers are little-endian.

| offset | width | field |
|---|---|---|
| 0 | 8 | magic bytes `4D 4F 4F 52 45 56 43 32` (`MOOREVC2`) |
| 8 | 1 | commit format `01` |
| 9 | 1 | commit slot containing this record, `00` or `01` |
| 10 | 1 | named body slot, `00` or `01` |
| 11 | 1 | flags, zero |
| 12 | 4 | session generation; unsupervised uses `1` |
| 16 | 4 | event JSON epoch named by the body header |
| 20 | 4 | reserved, zero |
| 24 | 8 | commit index; 0 invalid, strictly increasing, `FFFFFFFFFFFFFFFF` reserved for final commit exhaustion |
| 32 | 8 | committed body prefix length; nonzero and at most 256 KiB except OB-12's one-compaction overage and OB-28's single terminal-transaction overage |
| 40 | 32 | SHA-256 of exactly the committed body prefix |
| 72 | 4 | CRC-32C over bytes 0–71 |

A record is valid only when its slot byte matches the filename, its generation matches the holder, every reserved value is zero, the CRC matches, the body contains at least the complete schema-v2 header and final newline within the named prefix, and the prefix SHA-256 matches. Recovery validates both slots independently and chooses the greater valid commit index; an equal index with different valid record bytes is corruption. Bytes beyond body length are never part of the commit.

## 14. Semantic producer wire — version 1

### 14.1 Header and bounds

Semantic frames use the same 24-byte shape as §1 with these substitutions:

| offset | width | field | value / notes |
|---|---|---|---|
| 0 | 4 | magic | `4D 4F 4F 53` (`MOOS`) |
| 4 | 1 | version | `01` |
| 5 | 1 | type | §14.3 |
| 6 | 1 | flags | bit 0 `MORE`; others zero |
| 7 | 1 | reserved | zero |
| 8 | 4 | source epoch | zero on `SEMANTIC_HELLO` and on a `SEMANTIC_ERROR` sent before assignment; holder-assigned nonzero value on every other frame, including `HELLO_ACK` |
| 12 | 4 | frame sequence | per direction, starts 1, increments by one, `FFFFFFFF` reserved and closes the connection |
| 16 | 4 | payload length | at most 64 KiB (`00010000`) |
| 20 | 4 | header CRC-32C | over bytes 0–19 |

Reassembly follows §1/§10.2.2 with a 1 MiB total-message bound and 5-second deadline. Semantic JSON itself is at most 32 KiB. Peer identity is checked before the magic is parsed. On Windows, §12.2 of the specification permits the fixed four-byte pre-authentication accumulation required to establish the impersonation context; it accepts arbitrary short reads, consumes no fifth byte, and leaves the four bytes uninterpreted until the SID check and reversion succeed.

Source epochs are unsigned 32-bit counters allocated independently per source id, starting at `1`, and are never reused during a holder incarnation. `FFFFFFFF` may be the last assigned epoch; a later connection for that source is refused with a zero-epoch `SEM_RESOURCE_EXHAUSTED` error rather than wrapping. A rejected hello may receive a zero-epoch `SEMANTIC_ERROR`; no accepted source uses zero.

### 14.2 Source identifiers and modes

A source id is 1–128 ASCII bytes, each one of `A-Z a-z 0-9 . _ -`, carried length-prefixed. It is compared byte-for-byte. At most 64 distinct source ids are admitted during one holder incarnation, including disconnected ids whose mode and epoch allocation remain retained; a 65th receives zero-epoch `SEM_RESOURCE_EXHAUSTED`, while reconnecting an existing id consumes no new slot. Producer instance, event id, application request id and semantic token are each exactly 16 opaque bytes.

Modes: `00` edge, `01` stateful. Capabilities: bit 0 `ASSERTION`, bit 1 `APPLICATION_RECEIPT`, bit 2 `INPUT_NOTICE`; bits 3–7 zero. A source used for receipt-required `INPUT` must be stateful and advertise bits 1 and 2.

### 14.3 Frame types

| value | name | direction | payload |
|---|---|---|---|
| `01` | `SEMANTIC_HELLO` | producer → holder | §14.4 |
| `02` | `SEMANTIC_HELLO_ACK` | holder → producer | §14.4 |
| `03` | `SEMANTIC_ASSERTION` | producer → holder | §14.5 |
| `04` | `APPLICATION_RECEIPT` | producer → holder | §14.5 |
| `05` | `INPUT_NOTICE` | holder → producer | §14.6 |
| `06` | `INPUT_NOTICE_ACK` | producer → holder | §14.6 |
| `07` | `SEMANTIC_ACK` | holder → producer | §14.7 |
| `08` | `SEMANTIC_HEARTBEAT` | producer → holder | 8-byte producer monotonic milliseconds |
| `09` | `SEMANTIC_ERROR` | either | 2-byte code (§14.8), then a nonempty length-prefixed diagnostic |
| `0A` | `INPUT_NOTICE_CANCEL` | holder → producer | application request id, lease epoch, request id, then a nonempty length-prefixed diagnostic |

Unknown types close with `SEM_UNKNOWN_TYPE`; direction violations are malformed.

Every unassigned semantic payload flag bit or enum value is `SEM_MALFORMED`. It is never ignored as a future extension under semantic wire version 1.

### 14.4 Hello

`SEMANTIC_HELLO` uses source epoch zero and payload:

| width | field |
|---|---|
| 16 | holder-fresh token decoded from `DESK_SESSION_SEMANTIC_TOKEN` |
| 16 | producer instance |
| 4 | producer-carried generation; exact current generation, or zero for an unsupervised session |
| 1 | mode |
| 1 | capabilities |
| var | source id, length-prefixed |

`SEMANTIC_HELLO_ACK` uses the newly assigned source epoch in its header and payload:

| width | field |
|---|---|
| 16 | holder incarnation |
| 1 | flags: bit 0 `SNAPSHOT_REQUIRED`; others zero |
| 4 | maximum semantic JSON bytes: 32768 |
| 4 | heartbeat interval ms: 5000 |
| 4 | missing-receipt diagnostic deadline ms: 60000 |
| 4 | correlation retention ms: 600000 |

An edge producer receives `SNAPSHOT_REQUIRED=0`; it emits no `semantic-source` lifecycle event when it connects or disconnects and may send only transition assertions. A newly current stateful producer receives 1 and is not exact until a snapshot is durably accepted. With event storage disabled, `MOOS` is unavailable and hello receives zero-epoch `SEM_CAPABILITY_ABSENT`. If the event stream becomes unwritable, accepted semantic connections receive `SEM_RESOURCE_EXHAUSTED` and close; no semantic ACK may claim durability after that failure.

### 14.5 Producer events

`SEMANTIC_ASSERTION` payload:

| width | field |
|---|---|
| 16 | event id |
| 8 | source sequence, starts 1 and increments across producer events in this source epoch |
| 1 | assertion kind: `00` transition, `01` complete snapshot; `01` is legal only in stateful mode and an edge producer using it is refused with `SEM_INVALID_PAYLOAD` |
| n | exact UTF-8 JSON object bytes to end of payload |

`APPLICATION_RECEIPT` payload:

| width | field |
|---|---|
| 16 | event id |
| 8 | source sequence |
| 16 | application request id |
| 4 | lease epoch |
| 8 | controller request id |
| 1 | status: `00` accepted, `01` refused |
| var | provider session id, length-prefixed raw bytes, 0–4096; zero is legal and means the producer supplied no such identifier |
| var | provider turn/request id, length-prefixed raw bytes, 0–4096; zero is legal and means the producer supplied no such identifier |

Source sequence advances only for these two event frames, not heartbeat/notice ACK. The holder retains the last 512 accepted `(source sequence, event id, SHA-256 of the exact complete reassembled payload, durable event position)` tuples for the source epoch; transport headers, frame sequence and fragmentation are excluded from that digest. A new event is accepted only at high-water plus one. The retained-tuple lookup occurs before application-correlation lookup. Any retained tuple retried with identical payload bytes is therefore a duplicate even if its application correlation has already resolved, and receives a newly sequenced `SEMANTIC_ACK` with duplicate status and its original durable position; the same event id or sequence with different bytes is conflict. A new receipt event naming a resolved or expired application/lease/request tuple is `SEM_UNKNOWN_APPLICATION_REQUEST`, even if the application id has since been reused with a later tuple. A sequence below high-water that is no longer retained, or more than one above it, is bad sequence. `FFFFFFFFFFFFFFFF` may be the final accepted source sequence; a further new event in that epoch is `SEM_RESOURCE_EXHAUSTED`, never a wrap. Inability to retain the next tuple is also `SEM_RESOURCE_EXHAUSTED` before acknowledgement.

### 14.6 Input correlation

`INPUT_NOTICE` is sent before terminal bytes and contains:

| width | field |
|---|---|
| 16 | application request id |
| 4 | lease epoch |
| 8 | controller request id |
| 8 | terminal byte count |
| 32 | SHA-256 of the exact terminal bytes |

`INPUT_NOTICE_ACK` echoes the first three fields and adds one byte: `00` prepared, `01` refused. It must arrive within 2 seconds. Only `prepared` permits the PTY write. A subsequent write failure sends `INPUT_NOTICE_CANCEL`; absence of that best-effort cancellation never licenses a producer to fabricate acceptance.

### 14.7 Durable semantic acknowledgement

`SEMANTIC_ACK` payload:

| width | field |
|---|---|
| 16 | event id echoed |
| 8 | source sequence echoed |
| 1 | status: `00` accepted and durable, `01` duplicate (original durable position follows), `02` refused |
| 2 | result code: zero for accepted/duplicate; on refusal, the applicable semantic error code from §14.8 |
| 4 | event JSON epoch; the exact durable value for accepted/duplicate, including zero; zero placeholder for refused |
| 8 | event JSON sequence; the exact durable value for accepted/duplicate, including zero; zero placeholder for refused |
| var | diagnostic, length-prefixed; empty on accepted/duplicate and nonempty on refused |

Accepted/duplicate is sent only after the corresponding event crossed §8.4.2's platform storage commit. Epoch zero and sequence zero are both valid durable coordinates, including together for the first event in a new stream; status, not a zero sentinel, distinguishes them from a refusal. A duplicate returns the original position in a new ACK frame with the next holder-to-producer frame sequence. Refused appends nothing, carries a nonzero result code and zero placeholders for both coordinates, and is used when event id/source sequence were parseable; a hello or connection-level refusal uses `SEMANTIC_ERROR`.

### 14.8 Semantic error codes

| value | name |
|---|---|
| 1 | `SEM_UNKNOWN_VERSION` |
| 2 | `SEM_UNKNOWN_TYPE` |
| 3 | `SEM_MALFORMED` |
| 4 | `SEM_UNAUTHORISED_PEER` |
| 5 | `SEM_STALE_TOKEN` |
| 6 | `SEM_SOURCE_CONFLICT` |
| 7 | `SEM_BAD_SOURCE_SEQUENCE` |
| 8 | `SEM_EVENT_CONFLICT` |
| 9 | `SEM_UNKNOWN_APPLICATION_REQUEST` |
| 10 | `SEM_CAPABILITY_ABSENT` |
| 11 | `SEM_SNAPSHOT_REQUIRED` |
| 12 | `SEM_RESOURCE_EXHAUSTED` |
| 13 | `SEM_DEADLINE_EXCEEDED` |
| 14 | `SEM_INVALID_PAYLOAD` |
| 15 | `SEM_APPLICATION_NOT_WRITTEN` |

`SEM_SOURCE_CONFLICT` means that a source id whose edge/stateful mode was already fixed for this holder incarnation attempted to change mode. A same-mode stateful replacement is not a conflict; it receives the next source epoch and must snapshot again.

## 15. Private launch records

These records are carried on distinct inherited byte streams. They are not controller or semantic frames and have no 24-byte header or CRC. Each channel requires its exact fixed-length record followed immediately by EOF within the stated 2-second deadline; extra bytes or a duplicate inherited writer are failure, not an extension.

### 15.1 Supervised-launch discriminator

The launcher writes exactly one 32-byte record to the private one-way channel selected by `DESK_MOOR_LAUNCH_CHANNEL`, closes its write end, and the holder requires EOF after byte 32. This record is a freshness discriminator, not authorisation; same-user trust remains §11.1 of the specification.

| offset | width | field |
|---|---|---|
| 0 | 8 | magic bytes `4D 4F 4F 52 4C 43 48 33` (`MOORLCH3`) |
| 8 | 1 | launch-record format `01` |
| 9 | 1 | flags, zero |
| 10 | 2 | reserved, zero |
| 12 | 4 | supervised session generation in `2`–`4294967295`, exactly equal to both inherited generation variables |
| 16 | 16 | fresh opaque nonce from the operating-system cryptographic random source |

The whole read including EOF has a 2-second deadline. A selector that is present but malformed, a handle outside the explicit inheritance list, a non-byte-stream handle, a short or long record, timeout, bad magic/format/reserved field, zero or mismatched generation is a failed supervised launch and never falls back to unsupervised. The selector and read handle are stripped/closed before the requested child is created.

### 15.2 Instrumentation-load acknowledgement

With `-S`, the holder creates a different one-way byte stream and inherits only its write end into the requested initial child. `DESK_MOOR_INSTRUMENT_CHANNEL` selects that end using canonical unsigned decimal descriptor text on POSIX or 1–16 lowercase hexadecimal digits without `0x` for a nonzero 64-bit Windows handle. `DESK_MOOR_INSTRUMENT_NONCE` is exactly 32 lowercase hexadecimal digits encoding the holder's fresh 16-byte challenge. The module initializer consumes and removes both variables before any application instruction, writes exactly this 36-byte record, and closes the write end.

| offset | width | field |
|---|---|---|
| 0 | 8 | magic bytes `4D 4F 4F 52 49 4E 53 33` (`MOORINS3`) |
| 8 | 1 | acknowledgement format `01` |
| 9 | 1 | flags, zero |
| 10 | 2 | reserved, zero |
| 12 | 4 | session wire generation: `1` unsupervised or the exact supervised generation |
| 16 | 4 | nonzero operating-system PID of the requested initial child |
| 20 | 16 | exact nonce decoded from `DESK_MOOR_INSTRUMENT_NONCE` |

The holder requires the expected generation, requested-child PID and nonce, followed by EOF, within 2 seconds. A selector/nonce with any other grammar, a handle outside the requested child's explicit inheritance set, a non-byte-stream handle, a short or long record, missing EOF, inherited duplicate writer, bad magic/format/reserved field, zero/wrong PID, wrong generation/nonce, or timeout fails the unpublished launch. On POSIX the writer is the module's load constructor. On Windows the injected DLL exports and runs `MoorInstrumentationInitV1` inside the still-suspended requested process as specified in §4.7; its unsigned return value must also be zero. This acknowledgement is separate from §15.1 and cannot establish supervision.

## 16. Serialized vectors

Byte-exact records an implementation must produce and accept. Every checksum is a real CRC-32C over that record's declared domain. **V7–V10 plus V18 are one logical replay sequence** and must be evaluated in protocol order.

**V1** — side-effect-free `HELLO`, exact generation 7, sequence 1; hello flags are reserved zero

```
4D 4F 4F 52 03 01 00 00 07 00 00 00 01 00 00 00
21 00 00 00 26 04 0D F1 4D 4F 4F 52 03 00 00 16
00 00 00 01 2F 74 6D 70 2F 2E 6D 6F 6F 72 2D 31
30 30 30 2F 62 75 69 6C 64
```

**V2** — `OUTPUT`, record sequence 42, byte offset 4096, payload `hi`

```
4D 4F 4F 52 03 06 00 00 07 00 00 00 09 00 00 00
12 00 00 00 85 A9 11 DC 2A 00 00 00 00 00 00 00
00 10 00 00 00 00 00 00 68 69
```

**V3** — `ATTACH` with geometry 0×0 — *preserve both* (OB-19) — requesting the lease

```
4D 4F 4F 52 03 03 00 00 07 00 00 00 02 00 00 00
05 00 00 00 35 5C 53 49 00 00 00 00 01
```

**V4** — `RESIZE`, lease epoch 3, geometry 80×24 — payload is 8 bytes: epoch then geometry

```
4D 4F 4F 52 03 0B 00 00 07 00 00 00 0B 00 00 00
08 00 00 00 7E AE 34 20 03 00 00 00 50 00 18 00
```

**V5** — `RESIZE` with 80×0 — half-specified, MUST be refused with `HALF_SPECIFIED_GEOMETRY`

```
4D 4F 4F 52 03 0B 00 00 07 00 00 00 0C 00 00 00
08 00 00 00 7A AB 6D DA 03 00 00 00 50 00 00 00
```

**V6** — any frame with generation 0 — MUST be refused; shown as `OUTPUT_ACK`

```
4D 4F 4F 52 03 07 00 00 00 00 00 00 03 00 00 00
08 00 00 00 DA 77 CE 5D 01 00 00 00 00 00 00 00
```

**V7** — a `MORE` run of two `INPUT` frames. The first carries lease epoch 3, request id 1 and flags `00`, then `AAAA`; the continuation carries `BB` and no metadata. They reassemble to one request whose data is `AAAABB`

```
4D 4F 4F 52 03 09 01 00 07 00 00 00 14 00 00 00
11 00 00 00 33 71 5F 45 03 00 00 00 01 00 00 00
00 00 00 00 00 41 41 41 41 4D 4F 4F 52 03 09 00
00 07 00 00 00 15 00 00 00 02 00 00 00 56 61 22
D3 42 42
```

**V8** — the `INPUT_RECEIPT` answering V7: lease epoch 3, request id 1, 6 bytes written, status `00`, result code zero. This is the receipt payload the holder caches

```
4D 4F 4F 52 03 0A 00 00 07 00 00 00 0A 00 00 00
2B 00 00 00 EA B3 81 BB 03 00 00 00 01 00 00 00
00 00 00 00 07 00 00 00 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 06 00 00 00 00 00 00 00
00 00 00
```

**V9** — **a replay of V7** — same lease epoch 3, same request id 1, identical bytes. The holder MUST write nothing and return the cached V8 receipt payload in a newly sequenced frame (V18)

```
4D 4F 4F 52 03 09 00 00 07 00 00 00 16 00 00 00
13 00 00 00 BA FD 47 3C 03 00 00 00 01 00 00 00
00 00 00 00 00 41 41 41 41 42 42
```

**V10** — **same request id, different bytes** — lease epoch 3, request id 1, payload `DIFFERENT`. MUST be refused with `BAD_SEQUENCE` and **nothing written**

```
4D 4F 4F 52 03 09 00 00 07 00 00 00 17 00 00 00
16 00 00 00 D6 1B 1C D3 03 00 00 00 01 00 00 00
00 00 00 00 00 44 49 46 46 45 52 45 4E 54
```

**V11** — `ERROR` carrying `GENERATION_MISMATCH` (9)

```
4D 4F 4F 52 03 13 00 00 07 00 00 00 0D 00 00 00
1E 00 00 00 3F E0 B5 E8 09 00 1A 00 67 65 6E 65
72 61 74 69 6F 6E 20 33 20 69 73 20 73 75 70 65
72 73 65 64 65 64
```

**V12** — Windows marker, generation 7, holder incarnation `00..0F`, local pipe `\\.\pipe\moor-000102030405060708090a0b0c0d0e0f`

```
4D 4F 4F 52 4D 52 4B 33 01 00 00 00 07 00 00 00
00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F
2E 00 5C 5C 2E 5C 70 69 70 65 5C 6D 6F 6F 72 2D
30 30 30 31 30 32 30 33 30 34 30 35 30 36 30 37
30 38 30 39 30 61 30 62 30 63 30 64 30 65 30 66
B1 25 D5 68
```

**V13** — Windows initial event commit. The canonical session identity is tag `02`, volume-serial bytes `00..07`, then file-id bytes `08..17`. The body is exactly the following 137 UTF-8 bytes, including its final LF:

```jsonl
{"v":2,"type":"header","ts":0,"session":"AgABAgMEBQYHCAkKCwwNDg8QERITFBUWFw==","generation":7,"epoch":0,"next_seq":0,"first_retained":0}
```

Its SHA-256 is `2c71e92870774150f5dbeec34f19052d82874f6eb4ac4bc9f8d4bf7ad743edfb`; commit slot 0/body slot 0/index 1 is:

```
4D 4F 4F 52 45 56 43 32 01 00 00 00 07 00 00 00
00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
89 00 00 00 00 00 00 00 2C 71 E9 28 70 77 41 50
F5 DB EE C3 4F 19 05 2D 82 87 4F 6E B4 AC 4B C9
F8 D4 BF 7A D7 43 ED FB F1 59 A6 D0
```

**V14** — semantic `HELLO`, generation 7, stateful source `claude`, all three capabilities

```
4D 4F 4F 53 01 01 00 00 00 00 00 00 01 00 00 00
2E 00 00 00 C8 42 0C 36 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 10 11 12 13 14 15 16 17
18 19 1A 1B 1C 1D 1E 1F 07 00 00 00 01 07 06 00
63 6C 61 75 64 65
```

**V15** — semantic `APPLICATION_RECEIPT`, source epoch 5/source sequence 2, accepted, provider ids `sess`/`turn`

```
4D 4F 4F 53 01 04 00 00 05 00 00 00 03 00 00 00
41 00 00 00 C2 D3 5C 3F 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
10 11 12 13 14 15 16 17 18 19 1A 1B 1C 1D 1E 1F
03 00 00 00 01 00 00 00 00 00 00 00 00 04 00 73
65 73 73 04 00 74 75 72 6E
```

**V16** — controller `INPUT` requiring a receipt from source `claude`, application id `20..2F`, data `hello`

```
4D 4F 4F 52 03 09 00 00 07 00 00 00 1E 00 00 00
2A 00 00 00 88 95 3C 6B 03 00 00 00 02 00 00 00
00 00 00 00 01 20 21 22 23 24 25 26 27 28 29 2A
2B 2C 2D 2E 2F 06 00 63 6C 61 75 64 65 68 65 6C
6C 6F
```

**V17** — matching semantic `INPUT_NOTICE`; the final 32 bytes are SHA-256(`hello`)

```
4D 4F 4F 53 01 05 00 00 05 00 00 00 04 00 00 00
44 00 00 00 8C B0 EC 04 20 21 22 23 24 25 26 27
28 29 2A 2B 2C 2D 2E 2F 03 00 00 00 02 00 00 00
00 00 00 00 05 00 00 00 00 00 00 00 2C F2 4D BA
5F B0 A3 0E 26 E8 3B 2A C5 B9 E2 9E 1B 16 1E 5C
1F A7 42 5E 73 04 33 62 93 8B 98 24
```

**V18** — the replay response to V9. Its payload is byte-identical to V8, while its holder-to-controller frame sequence advances from 10 to 11

```
4D 4F 4F 52 03 0A 00 00 07 00 00 00 0B 00 00 00
2B 00 00 00 CD CE BD F2 03 00 00 00 01 00 00 00
00 00 00 00 07 00 00 00 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 06 00 00 00 00 00 00 00
00 00 00
```

**V19** — the accepted durable `SEMANTIC_ACK` for V15, with status `00`, result code zero, at event epoch 2/sequence 9

```
4D 4F 4F 53 01 07 00 00 05 00 00 00 05 00 00 00
29 00 00 00 68 84 6D AE 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
00 00 00 02 00 00 00 09 00 00 00 00 00 00 00 00
00
```

**V20** — the duplicate response after V15 is retried with the next producer frame sequence. Status is `01`, result code stays zero, and the ACK frame sequence advances from V19 while the original durable position remains epoch 2/sequence 9

```
4D 4F 4F 53 01 07 00 00 05 00 00 00 06 00 00 00
29 00 00 00 01 03 29 75 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
01 00 00 02 00 00 00 09 00 00 00 00 00 00 00 00
00
```

**V21** — supervised-launch discriminator, generation 7, nonce `00..0F`; exactly 32 bytes followed by EOF

```
4D 4F 4F 52 4C 43 48 33 01 00 00 00 07 00 00 00
00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F
```

**V22** — instrumentation-load acknowledgement, generation 7, requested-child PID `0x00001234`, nonce `10..1F`; exactly 36 bytes followed by EOF

```
4D 4F 4F 52 49 4E 53 33 01 00 00 00 07 00 00 00
34 12 00 00 10 11 12 13 14 15 16 17 18 19 1A 1B
1C 1D 1E 1F
```

---

## 17. Required conformance coverage

Beyond the serialized frames above, each case below is a vector the shipped binary must satisfy. This list is the minimum, not the whole suite.

**Framing and reassembly:** a run split at every single-byte boundary; a sequence gap mid-run; a type change mid-run; a truncated header; a truncated payload; a run exceeding the message bound; a run abandoned past its deadline; a non-zero reserved bit; an unknown type; an unknown version; connection and aggregate-reassembly caps at their limit and one above without eviction or child impact; empty `OUTPUT`; `OUTPUT` at 65536 bytes and a larger child read split into contiguous records; contiguous output offsets, inclusive record range and half-open byte range, including both empty and nonempty status encodings; output acknowledgement zero/at-high-water/above-high-water; record-sequence and byte-offset exhaustion without wrap or silent continuation.

**Identity and rendezvous:** POSIX tag `01`; Windows tag `02` exact 25-byte length; unknown/wrong-length tags refused; the wide-prefix boundaries, including a status descriptor containing a Windows native path whose canonical WTF-8 encoding exceeds 4096 bytes and a value over 1 MiB refused before publication; a `HELLO` naming a different identity; nonzero hello flags refused; an identity/status probe producing no preamble, attach ACK or lease change; exact expected generation accepted; discovery `HELLO` returning the actual nonzero generation; a supervisor forbidden to use discovery for adoption; a superseded generation on every frame type; generation `1` from an unsupervised holder accepted; generation `0` refused on every controller frame except the initial discovery `HELLO`; marker CRC/length/magic/reserved-field failures; marker replaced between read and connect; wrong pipe prefix, length, case or hexadecimal grammar; marker/root reparse points; inherited or broad DACLs; remote pipe clients; a four-byte pre-authentication preface split at every boundary and still accepted within deadline; EOF/deadline before four bytes refused; impersonation/token-query/reversion failure; wrong `TokenUser` rejected with the four buffered bytes never parsed.

**Private launch channels:** supervised allocation starting at 2 and a discriminator carrying generation 1 refused; missing discriminator with inherited generation variables producing an unsupervised holder with those variables stripped; every selector grammar boundary; exact V21/V22 records; each short prefix, one extra byte, missing EOF, duplicated inherited writer and deadline; wrong magic/format/flags/reserved/generation; instrumentation wrong/zero PID or nonce; selector handles outside the explicit inheritance list; launch selector absent from the requested child; instrumentation selector/nonce present only for that child and removed before its first application instruction; POSIX constructor ACK, descendant preload with no ACK variables, Linux `LD_PRELOAD` colon/dollar/whitespace path refusal, macOS `DYLD_INSERT_LIBRARIES` colon path refusal, and Windows missing export/nonzero initializer/wrong-architecture failure. Every failure leaves no published rendezvous or running requested child, and neither channel substitutes for the other.

**Geometry:** both dimensions zero; one zero and one not; each range boundary; the cell-product limit.

**Preamble and replay:** all twelve mode groups emitted for exact tracked state in the exact order of §6; alternate-buffer selection preceding scroll-region restoration; arbitrary pre-existing combinations of mouse bits cleared before tracked bits are set; invalid/abandoned state clearing exactness and producing exactly one zero-length preamble before an ACK whose mode-exact bit is clear; RIS restoring exactness; no cursor position present anywhere in the payload; every representable tracked mode set by the child and restated on attach; a missing, second or post-`ATTACH_ACK` preamble refused; probes receiving none; preamble bytes absent from the output stream and from offset arithmetic; empty replay; complete replay from record 1; a 4 MiB whole-record retention boundary; discarded-prefix `GAP` then retained records; output arriving during replay ordered afterwards; a reconnecting controller discarding duplicates; status bits 2–3 rejected when nonzero; no checkpoint frame or buffer-exactness claim.

**Arbitration:** a `QUERY` frame emitted to the lease holder before the query bytes are forwarded, carrying a correlation id that the matching `QUERY_REPLY` echoes; a reply whose correlation id was never issued discarded; each query class split at every byte boundary; a query answered by the lease-holding viewer within the deadline and the holder staying silent; the same query with the viewer silent and the holder answering; a tracked-mode query after exactness loss receiving no synthetic reply and resuming only after RIS; OB-20 suppressing holder replies without suppressing a viewer reply; a duplicate query; a reply with a superseded lease epoch discarded; a partial write of a reply completed; every frozen reply byte-compared, with no trailing NUL.

**Receipts and replay:** a fully written input; a pre-write refusal and partial-write refusal; a receipt whose incarnation does not match; replay of each written/refused outcome at exactly the high-water mark with identical flags/correlation/source/bytes returning the cached receipt payload in a newly sequenced frame **with no re-evaluation or second write**; the same id with any metadata or byte difference refused as `BAD_SEQUENCE`; an id below or more than one above refused; request-id exhaustion without wrap; lease-epoch reset and exhaustion without wrap; superseded lease refused. Receipt-required input covers unavailable/edge/wrong-capability source, notice refusal/timeout, producer replacement after prepared ACK, no PTY write before a still-current prepared ACK, write failure plus cancel, application-id conflict while a binding is retained, safe id reuse after resolution under a later never-reused tuple, and a normal transport receipt remaining distinct from the later application event.

**Termination:** each of the five outcomes and method values; POSIX `SIGTERM` foreground-group targeting plus child-group fallback and `SIGKILL` force/escalation; a graceful termination escalating at its deadline; an operation exceeding the whole-operation deadline reported as `INDETERMINATE`; a mismatched incarnation leaving the session untouched; Windows CTRL_BREAK and `TerminateJobObject(..., 0xC000013A)` paths; a breakaway/WMI-created survivor setting the survivor bit without claiming it was terminated.

**Liveness:** heartbeat cadence; heartbeat flags changing immediately on child exit; a quiet session producing no `WAKEUP` and still heartbeating; heartbeat absence invalidating verified-live evidence at its threshold; stale only after a fresh probe positively establishes listener absence, otherwise `indeterminate`; Linux UUID, macOS `kern.boottime` and Windows WMI boot-identity encodings; unavailable/all-zero identity never matching; age from the matching monotonic clock only; Windows WMI timeout falling back without blocking publication.

**Windows event commit:** initial commit; every crash boundary around body write/flush and commit write/flush; either slot torn; uncommitted tail ignored and never adopted by a successor generation; inactive body rewrite; prefix length/hash mismatch; wrong self-slot/generation/epoch; equal conflicting valid commits; commit-index exhaustion; status descriptor exactly matching the selected commit; confirmed retirement removing all four slots and the directory.

**Semantic producer:** same-user check before `MOOS`; disabled/unwritable event stream refusing ingress and never issuing a false durable ACK; zero-epoch pre-assignment refusal; stale token/generation; edge vs stateful, including no edge `semantic-source` lifecycle event and refusal of an edge snapshot assertion; stateful replacement and source-epoch fencing/exhaustion without wrap; snapshot required after connect and after degraded recovery; heartbeat loss to degraded and disconnect to disconnected, never idle; JSON duplicate keys/depth/member/UTF-8/size failures; new source sequence skip/reuse/exhaustion without wrap; retries at the newest and oldest retained positions returning their original durable positions in newly sequenced ACK frames; an evicted old sequence refused; event-id/sequence payload conflict; retained-snapshot byte-budget exhaustion refused before state change; tuple-retention exhaustion refused before ACK; durable ACK only after event commit; empty and nonempty provider session/turn ids; all pending-correlation limits/deadlines/expiry reasons with producer/source-epoch provenance; wrong application tuple/source/generation/epoch/producer refused; provider receipt never synthesized by Moor.

**Event stream:** every case in §8.4.5 of the specification, event schema-v2 exact key sets and branch fields, canonical base64, canonical bounded `ts` spellings including 0, a millisecond fraction and the u64-millisecond maximum, Windows full-u32 exit codes and known holder termination method, plus OB-28's limiting-axis precedence and final transaction for seq/epoch/commit with the session continuing after it.
