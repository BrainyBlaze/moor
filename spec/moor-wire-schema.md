# Wire schema and conformance vectors — version 4

**Companion artefact to [moor-spec.md](./moor-spec.md).** §10.2 of that document fixes what this schema must satisfy; this file fixes the shapes. Where the two disagree the specification wins and this file is a defect.

**Version:** `wire-schema-4`. Revision 4 increments the controller version byte to `04` because it changes frozen layouts: the status descriptor gains a mandatory geometry pair, the superseded event layout `01` is refused, and the reference tool's legacy command grammar is gone. There is no `03` decoder — a v3 peer is refused as an unknown version, which is precisely what a version increment buys over another in-place amendment. The schema label and controller version byte are `04`, and the published document digest identifies this amended dialect. No mixed old/new dialect is supported. Every later frozen-layout change requires a version increment rather than another in-place edit. Semantic-producer frames use their separately versioned `MOOS` header (§14). Integer layouts are portable, while native paths are raw bytes on POSIX and canonical WTF-8 on Windows. This revision freezes the Windows marker (§12), the portable event/log/lifecycle commit record (§13), and private launch records (§15). It is referenced by the specification as *the accompanying vectors* (§0.2).

**Integer encoding:** all multi-byte integers are unsigned, **little-endian**, of the stated width. There is no variable-length encoding anywhere.

---

## 1. Frame header

Every frame begins with a fixed 24-byte header.

| offset | width | field | value / notes |
|---|---|---|---|
| 0 | 4 | magic | the four bytes `4D 4F 4F 52` in file order, in that order — the program's name in ASCII. It is a **byte sequence, not an integer**, so it is unaffected by byte order |
| 4 | 1 | version | `04` for this schema |
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

Wherever a field is described as *length-prefixed* without the word *wide*, it is this plain two-byte form:

| width | field |
|---|---|
| 2 | byte count, unsigned, little-endian, `0000..FFFF` (0..65535) |
| n | the bytes themselves |

The count is the exact byte count; no terminator, alignment, variable-width integer, or implicit character count is present. A field-specific bound may be smaller and wins where stated. **The bytes are raw and opaque unless the field explicitly says text or native path.** POSIX native paths are raw bytes. Windows native paths are canonical WTF-8 converted losslessly from UTF-16, including unpaired surrogates; a decoder MUST reject a non-canonical or non-round-tripping form. A length of zero is legal only where the field says so; it is distinct from absence.

#### 1.1.1 Wide identity and native-path prefixes

Every field explicitly described as *wide-length-prefixed* uses a 4-byte unsigned little-endian byte count followed by that many bytes. The count is at most **1 MiB** (`00100000`); the enclosing frame and reassembled-message bounds still apply independently. Only canonical session identities and native path fields use this form. This is deliberately separate from the plain u16 prefix: a valid Windows native path can exceed 65535 canonical WTF-8 bytes, so applying §1.1's representable limit to a working directory or event-stream path would make an otherwise valid session impossible to describe. An over-limit value is refused before publication rather than truncated.

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
| `03` | `ATTACH` | controller → holder | geometry (§4), then 1 byte flags: bit 0 = request a fresh viewer lease, bit 1 = `NON_VT`, bits 2..7 zero |
| `04` | `ATTACH_ACK` | holder → controller | the status descriptor, §5 |
| `05` | `TERMINAL_STATE` | holder → controller | the preamble, §6 |
| `06` | `OUTPUT` | holder → controller | 8 bytes record sequence, 8 bytes byte offset, then the raw bytes to end of payload |
| `07` | `OUTPUT_ACK` | controller → holder | 8 bytes: highest record sequence consumed |
| `08` | `GAP` | holder → controller | 8 bytes first lost sequence, 8 bytes last lost sequence |
| `09` | `INPUT` | controller → holder | §7.1 |
| `0A` | `INPUT_RECEIPT` | holder → controller | §7 |
| `0B` | `RESIZE` | controller → holder | 4 bytes lease epoch, then geometry (§4) |
| `0C` | `QUERY_REPLY` | controller → holder | u64 correlation id, u32 lease epoch, 1 byte echoed class (§8), then plain length-prefixed reply bytes |
| `0D` | `STATUS` | controller → holder | empty |
| `0E` | `STATUS_REPLY` | holder → controller | the status descriptor, §5 |
| `0F` | `TERMINATE` | controller → holder | §9 |
| `10` | `TERMINATE_RESULT` | holder → controller | outcome, containment result, termination method, then length-prefixed diagnostic (§9.2) |
| `11` | `WAKEUP` | holder → controller | empty — the event stream advanced (OB-30). Legal at EVERY post-`HELLO_ACK` phase, including between `HELLO_ACK` and `ATTACH_ACK`: a durable advance does not wait for the controller to finish attaching, and a controller MUST accept it there rather than fault the handshake |
| `12` | `HEARTBEAT` | holder → controller | §10 |
| `13` | `ERROR` | either | 2 bytes code (§11), then a nonempty length-prefixed diagnostic |
| `14` | `QUERY` | holder → controller | u64 correlation id, u32 lease epoch, 1 byte class (§8), then plain length-prefixed exact query bytes |
| `15` | `LEASE_REQUEST` | controller → holder | 40 bytes, §7.4 |
| `16` | `LEASE_RESULT` | holder → controller | 24 bytes, §7.4 |
| `17` | `LEASE_RELEASE` | controller → holder | 20 bytes, §7.4 |
| `18` | `LEASE_KEEPALIVE` | controller → holder | 20 bytes, §7.4 |
| `19` | `LOG_CLEAR` | controller → holder | 24 bytes, §7.6 |
| `1A` | `LOG_CLEAR_RESULT` | holder → controller | 32 bytes, §7.6 |

An unknown type closes the connection with `UNKNOWN_TYPE`. It is never skipped.

Every unassigned payload flag bit and enum value is reserved. A controller frame carrying one is `MALFORMED_FRAME`; it is never ignored as a forward-compatible extension. In particular, termination containment bits 4–7 are zero. Controller types `15..1A` forbid `MORE`; any fragmentation or payload length other than the exact value above is `MALFORMED_FRAME`.

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

Both dimensions zero (`0 x 0`) mean *preserve both* (OB-19). Exactly one zero is `HALF_SPECIFIED_GEOMETRY` and changes nothing. A real geometry has columns and rows independently in `1..32767`; both operands are widened before multiplication and their product must be at most `2,000,000`. A value outside either per-dimension range or above the product bound is `MALFORMED_FRAME`. Windows conversion is checked before either u16 enters a signed operating-system API.

---

## 5. The status descriptor

Carried by `ATTACH_ACK` and `STATUS_REPLY` (OB-39).

| width | field | obligation |
|---|---|---|
| var | canonical session identity, wide-length-prefixed | OB-17 |
| 4 | generation | §10.1 |
| 16 | holder incarnation | — |
| 1 | event storage layout: `00` disabled, `02` portable four-slot committed directory on every platform. `01` named a superseded pre-amendment layout that no conforming holder ever emitted; a validator MUST reject it — an acceptor for a value no producer can produce is a forgery hole, not compatibility | OB-39 |
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
| 2 | **terminal columns** — the holder's stored geometry, mandatory and nonzero | §4 |
| 2 | **terminal rows** — the holder's stored geometry, mandatory and nonzero | §4 |
| 8 | retained history, first output record sequence present; zero iff empty | — |
| 8 | retained history, last output record sequence present, inclusive; zero iff empty | — |
| 8 | retained history, first byte offset still present | — |
| 8 | retained history, exclusive end byte offset | — |
| 1 | flags: bit 0 = retained raw output is complete from record 1/byte 0; bit 1 = tracked terminal-mode state exact; bits 2–3 zero; bit 4 = this controller owns the input lease; bit 5 = at least one fully attached viewer exists; bit 6 = requested child running; bit 7 = configured event store writable | §6.7, OB-39 |
| 4 | **lease epoch** — the current epoch, whether or not this controller holds it | §6.1 |
| 1 | semantic flags: bit 0 = at least one stateful source is exact; bit 1 = at least one stateful source has degraded or disconnected evidence; bit 2 = at least one source can prepare application-receipt input; bits 3–7 zero | §10.3 |
| 2 | holder-wide pending application-receipt correlation count (0–512) | §10.3.4 |
| 1 | health flags: bit 0 = log store writable; bit 1 = lifecycle store writable; bit 2 = terminal observer exact; bit 3 = query delegation still allocatable; bits 4..7 zero | specification §§8.5, 9.4, 10.2.7 |
| 4 | selected log epoch; zero when logging is disabled | specification §7.3 |
| 8 | selected log commit index; zero when logging is disabled | specification §7.3 |
| 8 | retained log start coordinate; zero when logging is disabled | specification §7.3 |
| 8 | retained log exclusive end coordinate; zero when logging is disabled | specification §7.3 |

**Both clock fields are present, always.** The wall clock is for display; age is computed from the monotonic value, and only when the boot identity matches the consumer's own — otherwise age is reported as unknown rather than wrong (OB-31). This is the resolution of "monotonic basis *or* boot identity": it is both, and the boot identity is what makes the monotonic value comparable.

The geometry pair is the holder's own stored size, the same value a `RESIZE` updates, and it is present from child birth onward: an interactive creation takes the viewer's size and a headless one is assigned 24x80, so there is no "unknown" state and no zero encoding. It is written after the child exists and before any replay, updated only after a native resize actually succeeds, and validated against the §4 bounds — a descriptor whose pair is zero, out of range, or over the area cap is malformed. Columns precede rows, matching `RESIZE`. This is the authoritative answer to "what size is this session", so no consumer needs to keep a second copy.

Linux/WSL carries the 16 parsed UUID bytes from `/proc/sys/kernel/random/boot_id`; macOS carries little-endian `kern.boottime` seconds in bytes 0–7, microseconds in bytes 8–11 and ASCII `MAC1` in bytes 12–15; Windows carries documented WMI `LastBootUpTime` converted to UTC FILETIME ticks in little-endian bytes 0–7 with bytes 8–15 zero. Sixteen zero bytes mean unavailable and never compare equal. The matching monotonic clocks and failure rules are frozen in specification §12.6. The active event fields are copied from the same validated layout-`02` commit record a reader would select, not from uncommitted writer state. Layout `00` uses empty event identity, slot `FF`, and zero commit index/length/hash. Disabled logging clears health bit 0 and zeros all four log fields; before child exit a successfully initialized lifecycle store sets health bit 1. Observer exactness is independent of tracked-mode exactness. A probe and an input-only connection never set viewer-presence bit 5.

---

## 6. The terminal-state preamble

`TERMINAL_STATE` carries a plain length-prefixed run of raw bytes that a viewer writes into its own emulator. It is sent **exactly once per attaching connection, immediately after `ATTACH_ACK`**, and **carries the connection's own generation** like every other frame — the header rule of §1 admits no exception. Its connection-locality is expressed by the fact that it carries **no record sequence and no byte offset**, so it cannot advance any output cursor and is never logged (§10.2.6). A zero-length run is legal in exactly two branches: tracked-mode exactness is false, in which case the ACK that preceded it already cleared its mode-exact bit; or the attach set `NON_VT`, in which case the ACK retains the actual exactness bit but no terminal controls are sent. A probe or input-only connection receives no preamble.

Attach output order is exact and is revision 4's status-first sequence: `ATTACH_ACK`; `TERMINAL_STATE`; `LEASE_RESULT` when the attach requested a fresh viewer lease; the frozen `GAP`/`OUTPUT` replay baseline; then live output. The descriptor opens the prefix so the viewer holds the authoritative geometry and replay window before a single terminal byte arrives; a decoder MUST refuse a descriptor after terminal state, which was the retired v3 order. The requested attach geometry is applied to the native pty BEFORE the descriptor is built, and a native refusal fails the attach closed — the holder closes the link rather than attach a viewer under a descriptor claiming a size the pty does not have. The attach/grant transaction is atomic, and it COMMITS on the successful enqueue of the token-bearing `LEASE_RESULT` — the last prefix frame the grant owes. Any earlier failure — the deadline, the native resize, or any prefix frame that cannot enqueue, the token frame itself included — rolls the fresh grant back entirely, reservation and epoch allocation both. The token is the only way to exercise an epoch and it never left the holder, so the epoch was never consumed: the next fresh controller receives that very number, even though a failed prefix may already have shown it inside a descriptor to a link the holder then closed. A resumed viewer's failure instead preserves its known epoch/token reservation exactly as ordinary link loss does. The native geometry is the honest exception to the unwinding: once the native resize has succeeded, a later prefix failure may leave that geometry applied — a native effect may already be visible and is never guessed back with a compensating resize — and the next status descriptor reports the geometry actually in force. Prefix followers are fenced symmetrically on the decode side: after the descriptor, a viewer MUST refuse `OUTPUT`, `GAP`, `INPUT_RECEIPT`, `QUERY`, and `LEASE_RESULT` until the mandatory `TERMINAL_STATE` has arrived. The attach becomes fully attached before the first item is queued, so its own ACK already reflects viewer presence. A busy lease leaves it attached as an observer and still returns the refused result in that position. No replay or live output may overtake this prefix.

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

Each `OUTPUT` payload carries 1–65536 child bytes. The holder retains complete newest records up to 4 MiB of payload per holder incarnation. After `ATTACH_ACK`, `TERMINAL_STATE`, and the optional attach-requested `LEASE_RESULT`, every attach receives a frozen baseline: `GAP{1, first_retained-1}` when `first_retained > 1`, then every record from the ACK's inclusive first/last range, then later live records. Empty history emits neither. The connection serialises output that arrives during replay after that baseline. A controller with an existing cursor discards duplicate record sequences; a new controller applies all of them. Bit 0 in the ACK is set exactly when the empty history is still at byte offset zero or the nonempty retained range begins at record 1/byte 0. This is raw replay only: there is no checkpoint frame in revision 4.

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

### 7.4 Lease-control payloads

Types `15..18` have fixed, unfragmented payloads. `LEASE_REQUEST` is exactly 40 bytes:

| offset | width | field |
|---:|---:|---|
| 0 | 1 | operation: `00` fresh, `01` resume |
| 1 | 1 | role: `00` viewer, `01` input-only |
| 2 | 2 | reserved, zero |
| 4 | 4 | expected lease epoch |
| 8 | 16 | expected holder incarnation |
| 24 | 16 | resume token |

A fresh request has zero epoch, all-zero incarnation, and all-zero token. A resume request has nonzero epoch/incarnation/token and requires exact generation, holder incarnation, epoch, token, and original role against one unexpired reservation. `LEASE_RESULT` is exactly 24 bytes:

| offset | width | field |
|---:|---:|---|
| 0 | 1 | outcome: `00` granted, `01` resumed, `02` released, `03` refused |
| 1 | 1 | reason: `00` none, `01` busy, `02` bad epoch, `03` bad token, `04` bad role, `05` not held, `06` exhausted, `07` bad incarnation |
| 2 | 1 | role: `00` viewer, `01` input-only |
| 3 | 1 | reserved, zero |
| 4 | 4 | lease epoch |
| 8 | 16 | resume token |

Grant/resume has reason zero, a nonzero epoch, and a fresh nonzero token. Release has reason zero, the released nonzero epoch, and an all-zero token. Refusal has a nonzero reason, reports the current allocated epoch (zero before any allocation), and carries an all-zero token. It never reveals another controller's token. `LEASE_RELEASE` and `LEASE_KEEPALIVE` are each exactly 20 bytes: u32 current epoch followed by the exact 16-byte current token. An all-zero generated token is discarded; random-source failure refuses without consuming an epoch.

### 7.5 Closed lease state and connection phases

The holder starts with allocated epoch zero and no owner. A fresh grant is the only transition that allocates `previous epoch + 1` and resets input-request high-water to zero. Release and deadline expiry invalidate the token without incrementing the allocated epoch. Epoch `FFFFFFFF` may be granted once; after its release or expiry every fresh request refuses/exhausted and the counter never wraps. Fresh-decision order is active or reserved owner → busy; otherwise epoch exhausted → exhausted; otherwise allocate → granted. There is no queue or forced steal.

Every valid owner `INPUT`, `RESIZE`, `QUERY_REPLY`, or `LEASE_KEEPALIVE` refreshes the ten-second responsiveness deadline. An idle client sends keepalive every three seconds. A valid keepalive has no response. An invalid one receives `ERROR(LEASE_NOT_HELD)` and only that connection closes. `LEASE_RELEASE` always receives a result: an exact tuple releases; a mismatch refuses/not-held without mutation. Transport loss reserves role, epoch, token, request high-water, complete cached request, and cached receipt only until the original deadline. Exact resume before expiry preserves epoch/request state and rotates the token atomically.

Each authenticated controller connection is in exactly one phase:

| phase | viewer | owns lease | legal state-changing frames |
|---|---:|---:|---|
| `U` authenticated/unattached | no | no | `ATTACH`; fresh input-only `LEASE_REQUEST`; resume `LEASE_REQUEST` for either original role; `LOG_CLEAR` |
| `I` input-only | no | yes | `INPUT`, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `R` resumed viewer, attach pending | no | yes | `ATTACH` without request bit, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `O` attached observer | yes | no | fresh viewer `LEASE_REQUEST`, `LOG_CLEAR` |
| `V` attached lease viewer | yes | yes | `INPUT`, `RESIZE`, `QUERY_REPLY`, `LEASE_KEEPALIVE`, `LEASE_RELEASE`, `LOG_CLEAR` |
| `C` closing | no | no | none |

`STATUS` is legal only in `U`, `O`, and `V`; `OUTPUT_ACK` only in `O` and `V`; authenticated termination retains §9's rules. Any other phase/frame combination is `MALFORMED_FRAME` and changes no state. A fresh viewer lease is requested only by `ATTACH` shorthand in `U` or a fresh viewer request in `O`; a viewer-role fresh request in `U` is malformed. A fresh/resumed input-only grant enters `I`. Viewer resume enters `R`; its request-bit-clear `ATTACH` within the two-second identity deadline enters `V` and produces the normal ACK/preamble/baseline without a second lease result. `ATTACH` is illegal in `I`.

Lease loss has this closed transition table:

| cause | phase before | phase after | notification and retained state |
|---|---|---|---|
| successful explicit release | `I` | `U` | send released result; invalidate token and cached request |
| successful explicit release | `R` | `U` | send released result; invalidate token and cached request |
| successful explicit release | `V` | `O` | send released result; invalidate token/cache; viewer stays attached |
| active responsiveness deadline | `I` | `U` | no unsolicited result; invalidate token and cache |
| active responsiveness deadline | `R` | `U` | no unsolicited result; invalidate token and cache |
| active responsiveness deadline | `V` | `O` | no unsolicited result; invalidate token/cache; viewer stays attached |
| transport loss before deadline | `I`, `R`, or `V` | connection removed | retain role, epoch, token, request high-water, complete cached request, and cached receipt until original deadline; attached viewer detaches immediately |
| reservation deadline | no connection | no lease | invalidate retained token/cache; do not increment epoch |

Every transition removing a live owner resolves its outstanding queries in allocation order before transport state is discarded. An attach without a fresh-lease request sends preserve geometry, except that the immediate attach of an already resumed viewer may send valid nonzero geometry. A request-bit attach may send valid nonzero geometry, but it is applied only on grant; busy leaves an `O` observer and geometry unchanged. `push` uses `HELLO`, fresh input-only request, sequential input/replay, explicit release, and no attach/preamble/replay/output/geometry/viewer presence.

### 7.6 Ordered log clear

`LOG_CLEAR` is exactly 24 bytes: expected 16-byte holder incarnation followed by the u64 selected log commit index observed in status. `LOG_CLEAR_RESULT` is exactly 32 bytes:

| offset | width | field |
|---:|---:|---|
| 0 | 1 | outcome: `00` cleared, `01` already empty or disabled, `02` refused |
| 1 | 1 | reason: `00` none, `01` stale status, `02` store unavailable, `03` store corrupt |
| 2 | 2 | reserved, zero |
| 4 | 4 | resulting log epoch |
| 8 | 8 | observed/prior commit index `P`, echoed exactly |
| 16 | 8 | resulting commit index |
| 24 | 8 | cleared-through child-output coordinate |

These are same-user connection operations, not lease operations. Outcomes `00/00` and `01/00` are CLI success; `02` is exit 1. Let `E` be the assigned child-output end at the ordered barrier:

| outcome / reason | resulting epoch | resulting index | cleared-through | mutation |
|---|---:|---:|---:|---|
| `00 / 00` cleared | selected nonzero epoch | newly selected index | `E` | empty replacement selected |
| `01 / 00` already empty | current nonzero epoch | current selected index, which may exceed `P` only because earlier admitted work completed first | `E` | none at barrier |
| `01 / 00` disabled | `0` | `0` | `0` | none; valid only when `P == 0` |
| `02 / 01` stale status | current selected epoch, or `0` when disabled | current selected index, or `0` when disabled | current selected end, or `0` when disabled | none |
| `02 / 02` unavailable | `0` | `0` | `0` | none claimed |
| `02 / 03` corrupt | `0` | `0` | `0` | none claimed |

Admission order is stale incarnation; disabled-state validation; unavailable/corrupt health; observed-index mismatch; enqueue barrier. `P` is checked once at admission. At the barrier after every earlier log job, return already-empty only when the selected body is empty and its end equals `E`; otherwise select an empty `[E,E)` replacement. Later output remains after the barrier. Once a clear body or commit write begins, a missed two-second progress deadline or ambiguous commit flush closes/quarantines the log lane and connection without a result; a lost connection after submission is likewise indeterminate. The CLI never retries automatically.

## 8. Capability arbitration

The holder detects a query **incrementally**, across arbitrary read boundaries (§10.2.7), never by matching within a single read. Detection runs a state machine over the child's output. It delays at most 32 candidate bytes for at most 50 ms; exceeding either bound releases the bytes unchanged as ordinary output and reports query-scanner degradation. A byte run that resembles a query while inside another sequence is not one.

Let `CSI7` be bytes `1B 5B` and `CSI8` byte `9B`. The five frozen query classes accept either introducer followed by exactly one listed tail; nothing else is recognized:

| value | class | exact query tail after CSI | holder synthesis |
|---|---|---|---|
| `01` | primary device attributes | `63` or `30 63` | matched CSI then `3F 36 32 3B 34 63` |
| `02` | secondary device attributes | `3E 63` or `3E 30 63` | matched CSI then `3E 31 3B 34 37 3B 30 63` |
| `03` | terminal name/version | `3E 30 71` | CSI7 maps to `1B 50 3E 7C 6B 69 74 74 79 28 30 2E 34 37 2E 30 29 1B 5C`; CSI8 maps to `90 3E 7C 6B 69 74 74 79 28 30 2E 34 37 2E 30 29 9C` |
| `04` | private-mode report | `3F <mode> 24 70`, canonical decimal mode `0..4294967295` | matched CSI, `3F <same-mode> 3B <state> 24 79`, where state is `31` set, `32` reset, or `30` only for a mode outside §6 |
| `05` | cursor-position report | `36 6E` | never synthesized |

**No reply contains a trailing NUL.** The current implementation appends one because it measures its buffer rather than its string; that byte is not part of any reply and a child that reads it sees stray input.

Accepted viewer replies are also closed:

- class `01`: `CSI ? P[;P]* c`, one through 16 canonical decimal parameters, each `0..65535`;
- class `02`: `CSI > P;P;P c`, exactly three canonical decimal parameters, each `0..4294967295`;
- class `03`: `DCS > | T ST`, either matched 7-bit `DCS=1B 50`/`ST=1B 5C` or matched C1 `DCS=90`/`ST=9C`, with `T` 1..128 bytes in `20..7E`;
- class `04`: `CSI ? <same-mode> ; S $ y`, echoing the exact query mode bytes, with `S` one canonical digit `0..4`;
- class `05`: `CSI R;C R`, with canonical decimal row and column each `1..65535`.

In the four CSI reply rows, CSI is `CSI7` or `CSI8`. Canonical decimal is `0` or a nonzero digit followed by digits. Omitted parameters, leading zeros, embedded C0/C1 bytes, an unlisted private prefix, class mismatch, mixed DCS/ST forms, or any trailing byte are invalid. A malformed reply is discarded while its correlation remains pending.

**The holder answers only when it supplied the identity itself.** §4.4.2 of the specification preserves an inherited terminal identity and injects one only when none was inherited. So there are two cases, and conflating them is a real contradiction an earlier version of this schema contained:

- **The holder injected the identity** (nothing was inherited). It knows what it claimed, the replies above match it exactly, and it answers.
- **The identity was inherited** from some other terminal. The holder does not know that terminal's device attributes, cannot obtain them, and **must not fabricate them**. It does not answer these three classes at all; the query passes through, and an attached viewer answers or nothing does.

This is the same rule as the cursor-position query, for the same reason: a synthetic answer is only honest when the holder is the thing being asked about. A child told by its environment that it is talking to one terminal and answered with another's attributes behaves erratically in ways that are extremely hard to diagnose.

**Cursor position is viewer-only.** A cursor-position report can only be answered by something that knows where the cursor is, and §9.1 of the specification forbids the holder from knowing that. When a fully attached VT-capable viewer holds the lease, the query is delegated and that viewer has the sole 250 ms opportunity to answer. **When no eligible viewer exists — including an observer, a `NON_VT` lease owner, or no attached viewer — the query is not answered at all**; silence is the honest outcome. Synthesising a position would require the screen model this document exists to keep out.

**A tracked-mode query is synthetic only while tracked-mode exactness is true.** If §6's exactness bit is clear, the lease holder may still answer, but after its 250 ms opportunity a silent viewer is followed by holder silence. The `state` byte `30` is reserved for a mode outside §6 and MUST NOT be used to disguise unknown tracked state. After RIS restores exactness, synthetic set/reset answers may resume.

**The OB-20 opt-out disables every holder-generated reply in this section.** It does not suppress a lease-holding viewer's reply and it does not alter terminal-identity injection.

**Arbitration.** At most one responder answers. A recognized query is delegated only to a fully attached VT-capable lease viewer. The holder allocates a nonzero u64 correlation, queues `QUERY` before forwarding the raw query bytes, and waits **250 ms**. A valid `QUERY_REPLY` must echo that correlation, the current u32 lease epoch, and the exact one-byte class before its plain length-prefixed reply. A selected valid viewer reply is the only answer. Otherwise the holder synthesizes exactly one eligible answer or remains silent. Observers and `NON_VT` viewers are treated as no eligible viewer; no correlation is consumed for the immediate synthesis-or-silence path. Duplicate, expired, unsolicited, wrong-generation, wrong-epoch, wrong-class, malformed, and superseded replies are discarded and never reach the child. A partial write of the selected reply to child input is completed.

Correlations start at 1 per holder incarnation, are never reused, and never wrap. At most 64 may be outstanding. If 64 are already outstanding, overload wins before counter allocation: report the slow-control failure, disconnect the eligible viewer while reserving its lease, cancel outstanding correlations in allocation/child-output order, resolve each by the no-live-viewer rule, then resolve the new query last. If fewer than 64 are outstanding but no successor id exists, report `RESOURCE_EXHAUSTED`, perform the same ordered disconnect/cancellation, and permanently clear status health bit 3. `FFFFFFFFFFFFFFFF` may be allocated once; after it resolves, the first later eligible query takes that exhaustion branch and later queries use the no-viewer rule directly. Transport loss and explicit lease release perform the same ordered cancellation before discarding owner state. None of these branches terminates the child or prevents a later input lease.

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
| possible-query recognition | 50 ms and 32 candidate bytes | release unchanged as ordinary output; mark the query scanner degraded for that episode |
| recognized-query viewer opportunity | 250 ms | apply §8's holder synthesis-or-silence rule |
| launch-time instrumentation load acknowledgement (OB-22) | 2 s | the launch fails; no session is left behind |
| live lease keepalive cadence while otherwise idle | 3 s | client sends `LEASE_KEEPALIVE` |
| lease responsiveness/reservation | 10 s from last valid owner activity | connected owner releases to its phase or disconnected reservation expires; allocated epoch does not increment |
| oldest admitted log-clear/store operation | 2 s | close and quarantine that store lane; only an already-issued commit flush may finish, and no body or other write may be issued; a submitted clear without a complete valid result is indeterminate |
| incomplete reassembly run | 5 s | `REASSEMBLY_TIMEOUT` |
| heartbeat absence before verified-live evidence expires | 15 s | invalidate that evidence and begin a fresh bounded identity probe; `indeterminate` until the probe resolves |

## 10. `HEARTBEAT` and liveness

| width | field |
|---|---|
| 8 | holder monotonic milliseconds |
| 1 | flags: bit 0 = child running; bit 1 = event store writable; bit 2 = log store writable; bit 3 = lifecycle store writable; bit 4 = terminal observer exact; bits 5..7 zero |

Sent every **5 seconds** while a controller is attached, and queued immediately on any flag change. Event, log, lifecycle, and scanner degradation therefore remain observable even when the event store cannot append a diagnostic.

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

## 13. Portable event/log/lifecycle commit record

Every enabled store is a directory containing exactly four regular, non-link slots: `body.0`, `body.1`, `commit.0`, and `commit.1`. A commit slot is initially empty and invalid or contains exactly this **92-byte** record. All integers are unsigned little-endian.

| offset | width | field |
|---|---|---|
| 0 | 8 | ASCII magic `MOORCMT1` (`4D 4F 4F 52 43 4D 54 31`) |
| 8 | 1 | format `01` |
| 9 | 1 | self commit slot `00` or `01` |
| 10 | 1 | named body slot, `00` or `01` |
| 11 | 1 | kind: `01` event, `02` log, `03` lifecycle |
| 12 | 4 | nonzero session wire generation |
| 16 | 4 | logical epoch |
| 20 | 4 | flags/reserved, zero |
| 24 | 8 | strictly increasing nonzero commit index |
| 32 | 8 | committed body prefix length |
| 40 | 8 | logical start coordinate |
| 48 | 8 | logical exclusive end coordinate |
| 56 | 32 | SHA-256 of exactly the committed body prefix |
| 88 | 4 | CRC-32C over bytes 0..87 |

A commit is valid only when the file is exactly 92 bytes; self slot matches its filename; body slot and kind are assigned; generation is nonzero and matches the requested store; every range/reserved check passes; index is nonzero; CRC is valid; the named body is at least the committed length; the prefix hash matches; and the kind rule below passes. Empty commit slots remain invalid initialization state. Both commits are validated independently. If both are valid with unequal indexes, the greater index selects. Equal valid indexes are corruption and fail closed even if every other field agrees: the two filename-specific self-slot bytes necessarily differ. With no valid commit the store is unusable and is never reset or adopted. Readers ignore every body byte after the selected prefix.

Kind rules are exact:

- **event (`01`):** prefix is nonempty canonical event-schema-2 JSONL within the 256 KiB cap and its bounded OB-12/OB-28 overages, begins with exactly one header, ends in LF, and contains no malformed or unknown record; commit epoch equals header epoch and coordinates equal header `first_retained` and exclusive `next_seq`;
- **log (`02`):** prefix is arbitrary raw bytes, its length is exactly `end-start` and no greater than the configured cap, and epoch increments on every body replacement but not growth; empty is valid;
- **lifecycle (`03`):** prefix is at most 4 MiB and exactly one canonical lifecycle JSON object plus LF, epoch is `1`, and `start==end`; the coordinate is zero for `running` and the final child-output end for `exited`.

Creation exclusively creates every log/lifecycle directory. The event directory is likewise created exclusively unless the caller handed off specification §8.1's exact validated empty directory object. In either case Moor creates all four slots exclusively and adopts no pre-existing slot. It flushes the directory entries, writes and flushes `body.0`, and writes and flushes index 1 in `commit.0`. Event initialization is the canonical header at epoch 0 and `[0,0)`; log initialization is empty at epoch 1 and `[0,0)`; lifecycle initialization is the canonical `running` record at epoch 1 and `[0,0)`. Every enabled initial store must revalidate before rendezvous publication.

Writers never mutate selected-prefix bytes. Growth removes an older uncommitted tail, writes from the selected length, flushes the body, writes the alternate commit at offset zero, truncates it to 92 bytes, and flushes it. Replacement writes/truncates/flushes the inactive body from offset zero, then writes/truncates/flushes the alternate commit that selects it. Namespace creation/removal is durably flushed. Failure before a new commit selects leaves the prior commit authoritative. A commit-flush timeout is ambiguous: either the prior or the one submitted commit may later validate, so the store closes permanently and no rollback or later write is guessed. `WAKEUP`, durable semantic acknowledgement, and durable consumer-cursor advance occur only after selection is durable.

One writer holds the portable exclusive lease on `commit.0` for the store lifetime: nonblocking `flock(LOCK_EX)` on POSIX or an exclusive `LockFileEx` byte-range lock through a non-delete-sharing handle on Windows. Lifecycle, event, then log is the acquisition order; reverse is release order. A reader may validate either slot without acquiring the writer lease. No successor generation adopts any predecessor directory, slot, prefix, or commit.

No counter wraps. Event sequence/epoch/commit exhaustion uses the specification's whole-transaction `seq`, then `epoch`, then `commit` precedence. Log index `FFFFFFFFFFFFFFFF` may select one last suffix/clear, after which logging is unwritable; epoch `FFFFFFFF` is the last replacement epoch. Lifecycle permits exactly initialization and exit, so recovered state unable to admit exit is corrupt. Crash recovery on every platform may select only the prior commit or at most the one submitted candidate—never a torn body, uncommitted tail, equal-index guess, or platform sidecar.

### 13.1 Canonical lifecycle body

The running lifecycle prefix is exactly one canonical JSON object plus LF with this closed key order: `v`, `type`, `phase`, `session`, `generation`, `wire_generation`, `incarnation`, `start_wall_ms`, `start_mono_ms`, `boot_id`, `path_encoding`, `event_path`, `instrument_path`. Values are respectively `2`, `"lifecycle"`, `"running"`, canonical padded-base64 tagged session identity, allocated u32 or JSON `null`, nonzero wire u32, padded-base64 16-byte incarnation, canonical decimal-string u64 wall/monotonic starts, padded-base64 16-byte boot identity, `"posix-bytes"` or `"windows-wtf8"`, and canonical padded base64 of the exact native event and immutable staged-instrument paths or JSON `null`. `instrument_path` never carries the caller's `-S` spelling.

The exited replacement changes phase to `"exited"`, retains those common values, then appends `end_wall_ms`, `output_end`, `ended`, its branch key, and `method`. The first two are canonical decimal-string u64. Revision 4 makes the exit MECHANISM and the holder's termination INTENT two orthogonal, mandatory axes. `ended` names only the mechanism: POSIX normal `ended:"exited",code:<u8>`; POSIX signal `ended:"signalled",signal:<positive platform signal>` with no code; Windows `ended:"exited",code:<u32>`. `method:"none"|"graceful"|"forced"` separately states whether the holder was asked to end the child — `none` means the holder had no termination state, and it never claims who outside the holder caused a signal. This is exactly the distinction the retired Windows-only `ended:"terminated"` branch folded into one value while POSIX carried nothing, which made a holder-initiated wire terminate byte-identical to an external `SIGTERM` and cost a real investigation its answer. The lifecycle record's `v` is `2`; a validator accepts only the v2 shape — there is no dual validator. The foreground shell status is not serialized as the child outcome. The selected lifecycle coordinate equals `output_end` on exit.

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
| 16 | holder-fresh token decoded from `MOOR_SESSION_SEMANTIC_TOKEN` |
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

These records are carried on distinct inherited byte streams. They are not controller or semantic frames and have no 24-byte header or CRC. The discriminator and instrumentation channels each require their one exact fixed-length record followed immediately by EOF within the stated 2-second deadline; extra bytes or a duplicate inherited writer are failure, not an extension. The background result stream in §15.3 instead carries its closed sequence of exact 12-byte records.

### 15.1 Supervised-launch discriminator

The launcher writes exactly one 32-byte record to the private one-way channel selected by `<BASENAME>_LAUNCH_CHANNEL`, closes its write end, and the holder requires EOF after byte 32. This record is a freshness discriminator, not authorisation; same-user trust remains §11.1 of the specification.

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

With `-S`, the holder creates a different one-way byte stream and inherits only its write end into the requested initial child. `MOOR_INSTRUMENT_CHANNEL` selects that end using canonical unsigned decimal descriptor text on POSIX or 1–16 lowercase hexadecimal digits without `0x` for a nonzero 64-bit Windows handle. `MOOR_INSTRUMENT_NONCE` is exactly 32 lowercase hexadecimal digits encoding the holder's fresh 16-byte challenge. The module initializer consumes and removes both variables before any application instruction, writes exactly this 36-byte record, and closes the write end.

| offset | width | field |
|---|---|---|
| 0 | 8 | magic bytes `4D 4F 4F 52 49 4E 53 33` (`MOORINS3`) |
| 8 | 1 | acknowledgement format `01` |
| 9 | 1 | flags, zero |
| 10 | 2 | reserved, zero |
| 12 | 4 | session wire generation: `1` unsupervised or the exact supervised generation |
| 16 | 4 | nonzero operating-system PID of the requested initial child |
| 20 | 16 | exact nonce decoded from `MOOR_INSTRUMENT_NONCE` |

The holder requires the expected generation, requested-child PID and nonce, followed by EOF, within 2 seconds. A selector/nonce with any other grammar, a handle outside the requested child's explicit inheritance set, a non-byte-stream handle, a short or long record, missing EOF, inherited duplicate writer, bad magic/format/reserved field, zero/wrong PID, wrong generation/nonce, or timeout fails the unpublished launch. On POSIX the writer is the module's load constructor. On Windows the injected DLL exports and runs `MoorInstrumentationInitV1` inside the still-suspended requested process as specified in §4.7; its unsigned return value must also be zero. This acknowledgement is separate from §15.1 and cannot establish supervision.

### 15.3 Holder-to-creator background result

Every record on the private holder-to-creator result stream is exactly 12 bytes:

| offset | width | field |
|---:|---:|---|
| 0 | 4 | ASCII magic `MORR` (`4D 4F 52 52`) |
| 4 | 1 | format `01` |
| 5 | 1 | state: `01` store-adopted, `02` ready, `03` failed |
| 6 | 2 | little-endian result code; zero for store-adopted/ready, frozen failure code for failed |
| 8 | 4 | little-endian session wire generation; nonzero and exact for the launch |

No flag, padding, trailing diagnostic, or alternate length exists. `store-adopted` is sent only after the holder owns every writer lease and captured every created object identity; after it the creator never deletes by path. `ready` follows only after all initial commits, the child-launch gate, and rendezvous publication. `failed` is permitted only before `ready` and reports a known failure. EOF/failure before adoption leaves rollback with the creator after confirmed holder death. Loss after adoption is resolved by identity probe and otherwise remains indeterminate; it never transfers deletion authority back by guess. Foreground `run` crosses the same states internally without this stream.

## 16. Serialized vectors

Byte-exact records an implementation must produce and accept. Every checksum is a real CRC-32C over that record's declared domain. **V7–V10 plus V18 are one logical replay sequence** and must be evaluated in protocol order.

**V1** — side-effect-free `HELLO`, exact generation 7, sequence 1; hello flags are reserved zero

```
4D 4F 4F 52 04 01 00 00 07 00 00 00 01 00 00 00
21 00 00 00 3E C8 F1 24 4D 4F 4F 52 04 00 00 16
00 00 00 01 2F 74 6D 70 2F 2E 6D 6F 6F 72 2D 31
30 30 30 2F 62 75 69 6C 64
```

**V2** — `OUTPUT`, record sequence 42, byte offset 4096, payload `hi`

```
4D 4F 4F 52 04 06 00 00 07 00 00 00 09 00 00 00
12 00 00 00 9D 65 ED 09 2A 00 00 00 00 00 00 00
00 10 00 00 00 00 00 00 68 69
```

**V3** — `ATTACH` with geometry 0×0 — *preserve both* (OB-19) — requesting the lease

```
4D 4F 4F 52 04 03 00 00 07 00 00 00 02 00 00 00
05 00 00 00 2D 90 AF 9C 00 00 00 00 01
```

**V4** — `RESIZE`, lease epoch 3, geometry 80×24 — payload is 8 bytes: epoch then geometry

```
4D 4F 4F 52 04 0B 00 00 07 00 00 00 0B 00 00 00
08 00 00 00 66 62 C8 F5 03 00 00 00 50 00 18 00
```

**V5** — `RESIZE` with 80×0 — half-specified, MUST be refused with `HALF_SPECIFIED_GEOMETRY`

```
4D 4F 4F 52 04 0B 00 00 07 00 00 00 0C 00 00 00
08 00 00 00 62 67 91 0F 03 00 00 00 50 00 00 00
```

**V6** — any frame with generation 0 — MUST be refused; shown as `OUTPUT_ACK`

```
4D 4F 4F 52 04 07 00 00 00 00 00 00 03 00 00 00
08 00 00 00 C2 BB 32 88 01 00 00 00 00 00 00 00
```

**V7** — a `MORE` run of two `INPUT` frames. The first carries lease epoch 3, request id 1 and flags `00`, then `AAAA`; the continuation carries `BB` and no metadata. They reassemble to one request whose data is `AAAABB`

```
4D 4F 4F 52 04 09 01 00 07 00 00 00 14 00 00 00
11 00 00 00 2B BD A3 90 03 00 00 00 01 00 00 00
00 00 00 00 00 41 41 41 41 4D 4F 4F 52 04 09 00
00 07 00 00 00 15 00 00 00 02 00 00 00 4E AD DE
06 42 42
```

**V8** — the `INPUT_RECEIPT` answering V7: lease epoch 3, request id 1, 6 bytes written, status `00`, result code zero. This is the receipt payload the holder caches

```
4D 4F 4F 52 04 0A 00 00 07 00 00 00 0A 00 00 00
2B 00 00 00 F2 7F 7D 6E 03 00 00 00 01 00 00 00
00 00 00 00 07 00 00 00 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 06 00 00 00 00 00 00 00
00 00 00
```

**V9** — **a replay of V7** — same lease epoch 3, same request id 1, identical bytes. The holder MUST write nothing and return the cached V8 receipt payload in a newly sequenced frame (V18)

```
4D 4F 4F 52 04 09 00 00 07 00 00 00 16 00 00 00
13 00 00 00 A2 31 BB E9 03 00 00 00 01 00 00 00
00 00 00 00 00 41 41 41 41 42 42
```

**V10** — **same request id, different bytes** — lease epoch 3, request id 1, payload `DIFFERENT`. MUST be refused with `BAD_SEQUENCE` and **nothing written**

```
4D 4F 4F 52 04 09 00 00 07 00 00 00 17 00 00 00
16 00 00 00 CE D7 E0 06 03 00 00 00 01 00 00 00
00 00 00 00 00 44 49 46 46 45 52 45 4E 54
```

**V11** — `ERROR` carrying `GENERATION_MISMATCH` (9)

```
4D 4F 4F 52 04 13 00 00 07 00 00 00 0D 00 00 00
1E 00 00 00 27 2C 49 3D 09 00 1A 00 67 65 6E 65
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

**V13** — portable initial event commit, using a Windows tag-`02` identity fixture whose volume-serial bytes are `00..07` and file-id bytes are `08..17`. The body is exactly the following 137 UTF-8 bytes, including its final LF:

```jsonl
{"v":2,"type":"header","ts":0,"session":"AgABAgMEBQYHCAkKCwwNDg8QERITFBUWFw==","generation":7,"epoch":0,"next_seq":0,"first_retained":0}
```

Its SHA-256 is `2c71e92870774150f5dbeec34f19052d82874f6eb4ac4bc9f8d4bf7ad743edfb`; kind is event `01`, epoch and coordinates are zero, and commit slot 0/body slot 0/index 1 is exactly 92 bytes:

```
4D 4F 4F 52 43 4D 54 31 01 00 00 01 07 00 00 00
00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
89 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 2C 71 E9 28 70 77 41 50
F5 DB EE C3 4F 19 05 2D 82 87 4F 6E B4 AC 4B C9
F8 D4 BF 7A D7 43 ED FB 28 95 8D 91
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
4D 4F 4F 52 04 09 00 00 07 00 00 00 1E 00 00 00
2A 00 00 00 90 59 C0 BE 03 00 00 00 02 00 00 00
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
4D 4F 4F 52 04 0A 00 00 07 00 00 00 0B 00 00 00
2B 00 00 00 D5 02 41 27 03 00 00 00 01 00 00 00
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

**V23** — portable empty-log initial commit: generation 7, epoch 1, kind `02`, index 1, empty body and coordinates `[0,0)`. The empty-body SHA-256 is `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.

```
4D 4F 4F 52 43 4D 54 31 01 00 00 02 07 00 00 00
01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 E3 B0 C4 42 98 FC 1C 14
9A FB F4 C8 99 6F B9 24 27 AE 41 E4 64 9B 93 4C
A4 95 99 1B 78 52 B8 55 CE 64 F3 A0
```

**V24** — portable canonical-running lifecycle initial commit. The exact 286-byte body, including LF, is:

```jsonl
{"v":2,"type":"lifecycle","phase":"running","session":"AS9z","generation":7,"wire_generation":7,"incarnation":"AgICAgICAgICAgICAgICAg==","start_wall_ms":"1","start_mono_ms":"2","boot_id":"AwMDAwMDAwMDAwMDAwMDAw==","path_encoding":"posix-bytes","event_path":null,"instrument_path":null}
```

Its SHA-256 is `fae71fcf6cad5e79d0abe5fe8463803409e50ec83f2cf57993660c8ba0c5224e`; kind is `03`, generation 7, epoch 1, index 1, and coordinates are `[0,0)`:

```
4D 4F 4F 52 43 4D 54 31 01 00 00 03 07 00 00 00
01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
1E 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 FA E7 1F CF 6C AD 5E 79
D0 AB E5 FE 84 63 80 34 09 E5 0E C8 3F 2C F5 79
93 66 0C 8B A0 C5 22 4E D7 28 8F 9E
```

**V25** — POSIX `STATUS_REPLY`, layout `02`. Session identity is tag `01` plus `/tmp/.moor-1000/build`; event path is `/tmp/events`; holder incarnation is `00..0F`; selected event slot/index/length are `0/1/133`. That event prefix is exactly this LF-terminated header, whose SHA-256 is `2bbeefb637546612d6a3a6bd7cbdb7be2942d6daddc733395445f9edd788b64b`:

```jsonl
{"v":2,"type":"header","ts":0,"session":"AS90bXAvLm1vb3ItMTAwMC9idWlsZA==","generation":7,"epoch":0,"next_seq":0,"first_retained":0}
```

Start wall/monotonic are `1/2`, boot identity is `03` repeated 16, working directory `/tmp`, PID `0x1234`, containment token `0x5678`, birth token `10..1F`, geometry `80x24` (columns then rows, per the mandatory revision-4 pair), and retained output is empty at coordinate zero. Main flags are `E3` (complete, tracked exact, viewer present, child running, event writable), lease epoch is 3, semantic flags/count are zero, health flags are `0F`, and selected log epoch/index/range are `1/1/[0,0)`. The frame uses generation 7, sequence 1, and this exact 248-byte status payload:

```
4D 4F 4F 52 04 0E 00 00 07 00 00 00 01 00 00 00
F8 00 00 00 68 D0 95 1E 16 00 00 00 01 2F 74 6D
70 2F 2E 6D 6F 6F 72 2D 31 30 30 30 2F 62 75 69
6C 64 07 00 00 00 00 01 02 03 04 05 06 07 08 09
0A 0B 0C 0D 0E 0F 02 0B 00 00 00 2F 74 6D 70 2F
65 76 65 6E 74 73 00 01 00 00 00 00 00 00 00 85
00 00 00 00 00 00 00 2B BE EF B6 37 54 66 12 D6
A3 A6 BD 7C BD B7 BE 29 42 D6 DA DD C7 33 39 54
45 F9 ED D7 88 B6 4B 01 00 00 00 00 00 00 00 02
00 00 00 00 00 00 00 03 03 03 03 03 03 03 03 03
03 03 03 03 03 03 03 04 00 00 00 2F 74 6D 70 34
12 00 00 78 56 00 00 10 11 12 13 14 15 16 17 18
19 1A 1B 1C 1D 1E 1F 50 00 18 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 E3 03 00 00 00
00 00 00 0F 01 00 00 00 01 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

**V26** — `NON_VT` attach and its required empty preamble. The attach preserves geometry and sets only flag bit 1. The preamble payload is the plain u16 zero length, not an absent frame; tracked exactness may remain set in the ACK. The attach uses controller-direction sequence 2; under revision 4's status-first prefix the empty preamble FOLLOWS the sequence-2 `ATTACH_ACK`, so it carries holder-direction sequence 3:

```
4D 4F 4F 52 04 03 00 00 07 00 00 00 02 00 00 00
05 00 00 00 2D 90 AF 9C 00 00 00 00 02

4D 4F 4F 52 04 05 00 00 07 00 00 00 03 00 00 00
02 00 00 00 37 2D 59 98 00 00
```

**V27** — private-mode query/reply with correlation `0102030405060708`, lease epoch 3, echoed class `04`, and plain u16 byte lengths. The query is `CSI7 ?2004$p`; the accepted reply is `CSI7 ?2004;1$y`. Both directions use frame sequence 3:

```
4D 4F 4F 52 04 14 00 00 07 00 00 00 03 00 00 00
18 00 00 00 5A C7 16 3B 08 07 06 05 04 03 02 01
03 00 00 00 04 09 00 1B 5B 3F 32 30 30 34 24 70

4D 4F 4F 52 04 0C 00 00 07 00 00 00 03 00 00 00
1A 00 00 00 F6 71 B4 D2 08 07 06 05 04 03 02 01
03 00 00 00 04 0B 00 1B 5B 3F 32 30 30 34 3B 31
24 79
```

**V28** — expanded `HEARTBEAT`, generation 7/sequence 4, monotonic value `0102030405060708`, with all five defined health bits set and reserved bits clear:

```
4D 4F 4F 52 04 12 00 00 07 00 00 00 04 00 00 00
09 00 00 00 2C A1 A4 A1 08 07 06 05 04 03 02 01
1F
```

**V29** — fresh viewer lease grant followed by explicit release. The fresh request carries operation/role zero and 36 zero freshness bytes; the grant allocates epoch 3 and token `00..0F`; release echoes that tuple; released result carries outcome `02`, epoch 3, and zero token. Controller sequences are 4 then 5; holder sequences are 5 then 6:

```
4D 4F 4F 52 04 15 00 00 07 00 00 00 04 00 00 00
28 00 00 00 E9 9A 80 98 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00

4D 4F 4F 52 04 16 00 00 07 00 00 00 05 00 00 00
18 00 00 00 7B 45 6E 47 00 00 00 00 03 00 00 00
00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F

4D 4F 4F 52 04 17 00 00 07 00 00 00 05 00 00 00
14 00 00 00 6F EA 86 AD 03 00 00 00 00 01 02 03
04 05 06 07 08 09 0A 0B 0C 0D 0E 0F

4D 4F 4F 52 04 16 00 00 07 00 00 00 06 00 00 00
18 00 00 00 12 C2 2A 9C 02 00 00 00 03 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

**V30** — ordered clear request and every `LOG_CLEAR_RESULT` row. The request uses incarnation `00..0F`, observed index `P=5`, generation 7, controller sequence 6:

```
4D 4F 4F 52 04 19 00 00 07 00 00 00 06 00 00 00
18 00 00 00 FF 34 2D 9D 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 05 00 00 00 00 00 00 00
```

The six independent result fixtures are, in order: cleared (`epoch=3,P=5,index=7,E=9`); already empty (`epoch=2,P=5,index=6,E=9`); disabled (`epoch=0,P=0,index=0,end=0`); stale status (`epoch=2,P=5,index=6,current-end=8`); unavailable (`P=5`, other numeric fields zero); corrupt (`P=5`, other numeric fields zero). Holder sequences are 7 through 12:

```
4D 4F 4F 52 04 1A 00 00 07 00 00 00 07 00 00 00
20 00 00 00 8B 88 87 B4 00 00 00 00 03 00 00 00
05 00 00 00 00 00 00 00 07 00 00 00 00 00 00 00
09 00 00 00 00 00 00 00

4D 4F 4F 52 04 1A 00 00 07 00 00 00 08 00 00 00
20 00 00 00 55 89 E5 0C 01 00 00 00 02 00 00 00
05 00 00 00 00 00 00 00 06 00 00 00 00 00 00 00
09 00 00 00 00 00 00 00

4D 4F 4F 52 04 1A 00 00 07 00 00 00 09 00 00 00
20 00 00 00 72 F4 D9 45 01 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00

4D 4F 4F 52 04 1A 00 00 07 00 00 00 0A 00 00 00
20 00 00 00 1B 73 9D 9E 02 01 00 00 02 00 00 00
05 00 00 00 00 00 00 00 06 00 00 00 00 00 00 00
08 00 00 00 00 00 00 00

4D 4F 4F 52 04 1A 00 00 07 00 00 00 0B 00 00 00
20 00 00 00 3C 0E A1 D7 02 02 00 00 00 00 00 00
05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00

4D 4F 4F 52 04 1A 00 00 07 00 00 00 0C 00 00 00
20 00 00 00 38 0B F8 2D 02 03 00 00 00 00 00 00
05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00
```

**V31** — all private background-result states, each exactly 12 bytes. Generation is 7; store-adopted and ready use result zero and are the two-record success sequence. Failed uses frozen sample code `0x1234` as an independent alternative and never follows ready:

```
4D 4F 52 52 01 01 00 00 07 00 00 00
4D 4F 52 52 01 02 00 00 07 00 00 00
4D 4F 52 52 01 03 34 12 07 00 00 00
```

**V32** — exact geometry, numeric-size, and same-size-redraw fixtures.

Geometry bytes are the four bytes `columns:u16-le, rows:u16-le`:

| columns × rows | exact bytes | required result |
|---:|---|---|
| `0 × 0` | `00 00 00 00` | preserve both |
| `0 × 1` | `00 00 01 00` | `HALF_SPECIFIED_GEOMETRY` |
| `1 × 0` | `01 00 00 00` | `HALF_SPECIFIED_GEOMETRY` |
| `1 × 1` | `01 00 01 00` | valid |
| `2000 × 1000` | `D0 07 E8 03` | valid exact product `2,000,000` |
| `2001 × 1000` | `D1 07 E8 03` | malformed product `2,001,000` |
| `32767 × 61` | `FF 7F 3D 00` | valid product `1,998,787` |
| `32767 × 62` | `FF 7F 3E 00` | malformed product `2,031,554` |
| `32768 × 1` | `00 80 01 00` | malformed dimension |

CLI numeric fixtures are byte strings, parsed without locale:

| operand | surface | required result |
|---|---|---:|
| `0` | `-C` | `0` bytes |
| `1k`, `1K` | `-C` | `1024` bytes |
| `2m`, `2M` | `-C` | `2097152` bytes |
| `3g`, `3G` | `-C` | `3221225472` bytes |
| `18446744073709551615` | `-C` | `18446744073709551615` bytes |
| `18014398509481983k` | `-C` | `18446744073709550592` bytes |
| `18014398509481984k` | `-C` | invalid checked-multiplication overflow |
| `01k`, `1kb` | `-C` | invalid spelling |
| `1k` | `tail -n` | invalid because tail counts are unsuffixed u32 |

For the same-size redraw fixture, current and requested geometry are both `80 × 24`, the attach wins its lease, and redraw is `winch`: after the ACK/terminal-state/lease-result/replay prefix, exactly one platform resize notification is issued even though stored geometry remains `80 × 24`. Redraw `none` issues none; `ctrl_l` writes exactly byte `0C` instead of issuing a resize. No other unchanged geometry causes a notification.

**V33** — `WAKEUP` interposed between `HELLO_ACK` and `ATTACH_ACK`. `WAKEUP` is legal at EVERY post-`HELLO_ACK` phase (§7), and this is the exceptional pre-attach window an independent implementer would otherwise guess about. The sequence numbers are the real ones: `HELLO_ACK` consumed holder-direction sequence 1, so the interposed `WAKEUP` carries sequence 2, the `ATTACH_ACK` that follows carries sequence 3, and the terminal-state preamble carries sequence 4. The frame is header-only — kind `11`, generation 7, zero payload — and its CRC-32C covers the twenty header bytes:

```
4D 4F 4F 52 04 11 00 00 07 00 00 00 02 00 00 00
00 00 00 00 52 17 53 91
```

A controller receiving this frame before its `ATTACH_ACK` continues its handshake unchanged: the frame creates no attach state, and the prefix that follows is byte-identical to the one an uninterrupted attach would deliver, at sequence numbers shifted by exactly one.

---

## 17. Required conformance coverage

Beyond the serialized frames above, each case below is a vector the shipped binary must satisfy. This list is the minimum, not the whole suite.

**Framing and reassembly:** a run split at every single-byte boundary; a sequence gap mid-run; a type change mid-run; a truncated header; a truncated payload; a run exceeding the message bound; a run abandoned past its deadline; a non-zero reserved bit; an unknown type; an unknown version; connection and aggregate-reassembly caps at their limit and one above without eviction or child impact; empty `OUTPUT`; `OUTPUT` at 65536 bytes and a larger child read split into contiguous records; contiguous output offsets, inclusive record range and half-open byte range, including both empty and nonempty status encodings; output acknowledgement zero/at-high-water/above-high-water; record-sequence and byte-offset exhaustion without wrap or silent continuation.

**Identity and rendezvous:** POSIX tag `01`; Windows tag `02` exact 25-byte length; unknown/wrong-length tags refused; plain-u16 prefix boundaries 0/65535/65536 and wide-prefix boundaries, including a status descriptor containing a Windows native path whose canonical WTF-8 encoding exceeds 65535 bytes and a value over 1 MiB refused before publication; a `HELLO` naming a different identity; nonzero hello flags refused; an identity/status probe producing no preamble, attach ACK or lease change; exact expected generation accepted; discovery `HELLO` returning the actual nonzero generation; a supervisor forbidden to use discovery for adoption; a superseded generation on every frame type; generation `1` from an unsupervised holder accepted; generation `0` refused on every controller frame except the initial discovery `HELLO`; marker CRC/length/magic/reserved-field failures; marker replaced between read and connect; wrong pipe prefix, length, case or hexadecimal grammar; marker/root reparse points; inherited or broad DACLs; remote pipe clients; a four-byte pre-authentication preface split at every boundary and still accepted within deadline; EOF/deadline before four bytes refused; impersonation/token-query/reversion failure; wrong `TokenUser` rejected with the four buffered bytes never parsed.

**Private launch channels:** supervised allocation starting at 2 and a discriminator carrying generation 1 refused; missing discriminator with inherited generation variables producing an unsupervised holder with those variables stripped; every selector grammar boundary; exact V21/V22 records; each short prefix, one extra byte, missing EOF, duplicated inherited writer and deadline; wrong magic/format/flags/reserved/generation; instrumentation wrong/zero PID or nonce; selector handles outside the explicit inheritance list; launch selector absent from the requested child; instrumentation selector/nonce present only for that child and removed before its first application instruction; POSIX constructor ACK, descendant preload with no ACK variables, Linux `LD_PRELOAD` colon/dollar/whitespace path refusal, macOS `DYLD_INSERT_LIBRARIES` colon path refusal, and Windows missing export/nonzero initializer/wrong-architecture failure. V31 covers every `MORR` state, wrong magic/format/state, nonzero success result, zero generation, short/long record, failed before adoption, ready before adoption, loss before adoption, and loss after adoption. Every failure leaves no published rendezvous or running requested child, and no private channel substitutes for another.

**Geometry and CLI numeric fixtures:** V32's preserve pair, both mixed-zero orders, `1`, `32767`, and `32768` per-dimension edges, exact `2,000,000` product, one product above, and widened `32767×61/62` pair; checked Windows signed conversion; ASCII-case-insensitive `k/m/g` and uppercase forms, maximum u64 unsuffixed value, maximum nonoverflowing suffixed value, multiplication overflow, leading zero, trailing byte, and suffix refusal by `tail -n`. The same-size `winch` fixture issues exactly one notification after the frozen attach prefix; ordinary unchanged geometry does not.

**Preamble and replay:** all twelve mode groups emitted for exact tracked state in the exact order of §6; alternate-buffer selection preceding scroll-region restoration; arbitrary pre-existing combinations of mouse bits cleared before tracked bits are set; invalid/abandoned state clearing exactness and producing exactly one plain-zero-length preamble before an ACK whose mode-exact bit is clear; V26 `NON_VT` producing the same empty payload while the ACK retains actual exactness; RIS restoring exactness; no cursor position present anywhere in the payload; every representable tracked mode set by the child and restated on attach; a missing, second or pre-`ATTACH_ACK` preamble refused; probes/input-only connections receiving none; exact ACK/preamble/optional-result/replay/live ordering; preamble bytes absent from output/offset arithmetic; empty replay; complete replay from record 1; a 4 MiB whole-record retention boundary; discarded-prefix `GAP` then retained records; output arriving during replay ordered afterwards; a reconnecting controller discarding duplicates; main status bits 2–3 rejected when nonzero; no checkpoint frame or buffer-exactness claim.

**Arbitration:** all five numeric classes in both CSI7/C1 forms, every query/reply grammar edge, private-mode exact byte echo, matched/mixed DCS/ST, cursor viewer-only silence, and V27's u64 correlation/u32 epoch/echoed class/plain-u16 lengths; a `QUERY` queued before its raw bytes; every candidate split at every boundary; 32-byte and 50 ms recognition edges; valid viewer reply at/beyond 250 ms; at most one answer; eligible synthesis and every silence branch; `NON_VT`/observer/no-viewer without identifier allocation; 64-outstanding overload precedence; final u64 id once and permanent exhaustion; cancellation order on disconnect/release; unsolicited, duplicate, expired, malformed, class-mismatched, wrong-generation/epoch and superseded replies discarded; OB-20 suppressing synthesis only; partial child-input write completed; every frozen reply byte-compared with no trailing NUL.

**Lease, phases, and ordered clear:** every `15..1A` exact length/direction and `MORE` refusal; fresh viewer/input-only grant, busy, resume with atomic token rotation, mismatch reasons without token disclosure, explicit release result, valid/invalid keepalive, connected deadline transitions, transport reservation/expiry, epoch `FFFFFFFF` once then permanent exhaustion, random-token failure without epoch consumption, every legal/illegal connection-phase frame, attach shorthand/busy observer/observer upgrade/viewer resume/push, query cancellation before owner state removal, and V29's exact sequence. V30 covers clear admission precedence, all six outcome/reason rows, earlier work advancing the selected index, disabled `P==0`, clear barrier ordering, and missing/ambiguous result without retry.

**Receipts and replay:** a fully written input; a pre-write refusal and partial-write refusal; a receipt whose incarnation does not match; replay of each written/refused outcome at exactly the high-water mark with identical flags/correlation/source/bytes returning the cached receipt payload in a newly sequenced frame **with no re-evaluation or second write**; the same id with any metadata or byte difference refused as `BAD_SEQUENCE`; an id below or more than one above refused; request-id exhaustion without wrap; lease-epoch reset and exhaustion without wrap; superseded lease refused. Receipt-required input covers unavailable/edge/wrong-capability source, notice refusal/timeout, producer replacement after prepared ACK, no PTY write before a still-current prepared ACK, write failure plus cancel, application-id conflict while a binding is retained, safe id reuse after resolution under a later never-reused tuple, and a normal transport receipt remaining distinct from the later application event.

**Termination:** each of the five outcomes and method values; POSIX `SIGTERM` foreground-group targeting plus child-group fallback and `SIGKILL` force/escalation; a graceful termination escalating at its deadline; an operation exceeding the whole-operation deadline reported as `INDETERMINATE`; a mismatched incarnation leaving the session untouched; Windows CTRL_BREAK and `TerminateJobObject(..., 0xC000013A)` paths; a breakaway/WMI-created survivor setting the survivor bit without claiming it was terminated.

**Status and liveness:** V25 on a native POSIX lane byte-compared through its layout-`02` event commit and appended health/log tail; disabled event/log zero encodings; selected event/log metadata copied only from validated commits; main viewer/child/event bits and requesting-controller lease bit; input-only/probe viewer exclusion; independent tracked/observer exactness; every reserved bit refused. V28 and each individual heartbeat flag transition queue immediately; five-second cadence; a quiet session producing no `WAKEUP` and still heartbeating; 15-second absence invalidating verified-live evidence; stale only after a fresh probe positively establishes listener absence, otherwise `indeterminate`; Linux UUID, macOS `kern.boottime`, and Windows WMI boot identity; unavailable/all-zero identity never matching; age from matching monotonic clock only; Windows WMI timeout without blocking publication.

**Portable committed stores on every native OS:** V13 event, V23 empty log, and V24 canonical running lifecycle initial commits; event/log/lifecycle kind and coordinate rules; every body-write/body-flush/commit-write/commit-flush byte boundary for growth and replacement; either slot torn; uncommitted tail ignored; inactive body rewrite; wrong total length/magic/format/self slot/body slot/kind/generation/epoch/flags/index/length/start/end/hash/CRC; any equal independently valid indexes as corruption; no-valid-commit failure; event sequence/epoch/index, log epoch/index, and lifecycle exit-frontier exhaustion without wrap; single writer lease and acquisition order; reader during writer lease; ambiguous final flush selecting only prior/submitted candidate; status matching selected commit; confirmed identity-fenced retirement; no successor adoption. Linux, macOS, and Windows each run the complete event, log, and lifecycle matrix rather than treating one family as the storage oracle.

**Semantic producer:** same-user check before `MOOS`; disabled/unwritable event stream refusing ingress and never issuing a false durable ACK; zero-epoch pre-assignment refusal; stale token/generation; edge vs stateful, including no edge `semantic-source` lifecycle event and refusal of an edge snapshot assertion; stateful replacement and source-epoch fencing/exhaustion without wrap; snapshot required after connect and after degraded recovery; heartbeat loss to degraded and disconnect to disconnected, never idle; JSON duplicate keys/depth/member/UTF-8/size failures; new source sequence skip/reuse/exhaustion without wrap; retries at the newest and oldest retained positions returning their original durable positions in newly sequenced ACK frames; an evicted old sequence refused; event-id/sequence payload conflict; retained-snapshot byte-budget exhaustion refused before state change; tuple-retention exhaustion refused before ACK; durable ACK only after event commit; empty and nonempty provider session/turn ids; all pending-correlation limits/deadlines/expiry reasons with producer/source-epoch provenance; wrong application tuple/source/generation/epoch/producer refused; provider receipt never synthesized by Moor.

**Event stream:** every case in §8.4.5 of the specification, event schema-v2 exact key sets and branch fields, canonical base64, canonical bounded `ts` spellings including 0, a millisecond fraction and the u64-millisecond maximum, Windows full-u32 exit codes and known holder termination method, plus OB-28's limiting-axis precedence and final transaction for seq/epoch/commit with the session continuing after it.
