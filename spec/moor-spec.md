# moor — behavioural specification for an independent implementation

**Status:** authoritative amended implementation handoff. **Every section is written and normative.** §14 records the resolution of all 42 obligations and names the sole downstream runtime gate that remains open with its owner. Wire schema 4 and event schema 2 accompany this document. The original pre-implementation amendment changed the then-current schema 3 in place; revision 4 is the frozen-layout version increment that this document's own rule requires, and it retires every legacy surface: the reference tool's dash-spelled command grammar, the colon-joined ancestry carrier, and the folded exit branch. The three authoritative inputs are named in §0; `spec/README.md` publishes the two normative document digests, for `moor-spec.md` and `moor-wire-schema.md`, that identify the amended handoff. Any later frozen-layout change requires a version increment.
**Audience:** the implementation team. You will build a program that replaces the existing one completely.

**The program is called `moor`.** A mooring is not ownership and not a launch: it is what holds something in place while others come alongside and cast off. That is exactly this program's job — the child lives on its own, viewers attach and detach, and nothing changes when they leave.

**The name is load-bearing, not decoration.** §2.2 derives the session root and §4.4.1 derives the environment keys from the invoked base name, each with its own frozen transformation, so a copy invoked under a different name gets a different root and different keys — by construction, not by special case. The distribution **MUST install only the canonical `moor` entrypoint**; it ships no compatibility alias. A user-created copy or symlink invoked under another name still receives the distinct identifiers required by the general rule, but that name is not a packaged entrypoint.
**Author:** source-exposed specification team.

---

## 0. How this document is used

This is the *only* description of the program you are asked to build. It is written from observable behaviour: what the program is given, what it must produce, and which properties must hold. It deliberately contains no source code, no function or file names, and no line references from any existing implementation. If you find yourself wanting one, the specification is incomplete — ask, and it will be extended, in behavioural terms.

Only `spec/README.md`, this `spec/moor-spec.md`, and `spec/moor-wire-schema.md` are authoritative implementation inputs. The behavioural specification wins if it conflicts with the wire artifact. No implementation, existing test, workaround note, prior holder history, design brief, or third errata document can add or override behavior.

Anything not stated here is not required. Where a behaviour is deliberately unconstrained, this document says so explicitly; treat silence elsewhere as a gap to report rather than a licence to invent.

Two audiences read a claim in this document differently, so the language is fixed:

- **MUST** — a conformance requirement. A differential test exists or will exist for it.
- **SHOULD** — expected, but a deviation that is documented and justified is acceptable.
- **MAY** — free choice.

### 0.1 What is being built, and for whom

This is the specification of **our** session holder — the program our product needs. It is not a description of any existing tool. Two kinds of caller drive it and both are first-class throughout this document:

- **A human at a shell**, who creates a session, works in it, detaches, and comes back.
- **A supervising daemon**, which creates and drives dozens of sessions at once, never has a terminal of its own, and depends on the program to report what is happening inside each one.

The second caller is the reason this program exists in the form specified here. Its requirements — the event stream (§8), the observed state it consumes (§9), generation fencing (§10), the security model around a shared session root (§11) — are not extensions bolted onto a terminal tool. They are load-bearing, and a submission that implements the human-facing behaviour well and the supervisor's contract loosely has not met this specification.

### 0.1.1 Implementation quality **[normative]**

There is no source-line ceiling. The implementation MUST remain normally formatted, reviewable, tested, and maintainable. Shared policy and state machines SHOULD remain centralized where doing so makes their invariants clearer, but reducing a line count never justifies omitted behaviour, compressed control flow, duplicated risk, weakened validation, or a narrower conformance claim.

Every release candidate reports the per-file and aggregate first-party production line count for review visibility. The count is informational: CI MUST NOT reject a candidate solely because it exceeds a numeric source-line threshold. Behavioural conformance, security properties, native-platform evidence, test quality, and maintainability are the release criteria.

### 0.2 Compatibility with what exists today

A working implementation already runs in production and callers are built against it. Compatibility is **not uniform across callers**, and pretending otherwise makes this document unimplementable:

- **External callers — humans at a shell, scripts, anything on-disk** — are unchanged **except for the twenty-six corrections enumerated under OB-24**. Same command lines, same layout, same exit codes everywhere else. This bullet once said "they must never learn that the implementation changed"; that was an absolute the document could not keep, and OB-24 replaced it with a finite, named list.
- **The supervising daemon is part of an atomic, coordinated cutover.** It ships in the same release as the new holder and speaks the full protocol from the first moment. There is no mixed-version production window, no negotiating down to a partial dialect, no permanent fallback, and no dual runtime after cutover.

The second bullet exists because the frozen protocol requires exchanges the current supervisor does not perform. A specification that demanded both "the full protocol" and "no caller changes" would force the implementer to violate one of them silently — and they would pick the one that is not tested.

The compatibility bar is not "works for the common case". It is: for a given input, the new program and the reference produce the same observable output, byte for byte, wherever this document says so. Conformance vectors accompany this specification for the parts where "the same" must be exact.

Where this document requires behaviour that the current implementation does **not** have, it is marked **[NEW]**. Those are deliberate corrections of defects found in production; they are requirements, not optional improvements, and each states the failure it prevents.

**Several `[NEW]` clauses diverge from the first bullet above, which is why that bullet no longer states an absolute.** A root with permissive mode now fails where it succeeded (§2.2); an unknown `--option` now fails where it created a session (§3.1); malformed numeric operands and trailing operands now change status and output (§3.6). Each is a defensible safety correction and each is visible to an external caller — so "external callers are unchanged" and "these corrections ship" cannot both be true. Marking a clause `[NEW]` records the divergence; it does not grant an exemption from a guarantee stated as absolute.

**OB-24 resolved this**: the first bullet permits a finite, enumerated list of breaking safety corrections with a stated migration, and §14.1 enumerates all twenty-six. Strict parity was the alternative and was rejected, because it would mean deliberately reproducing known defects in a program written from scratch to remove them. What was never available was leaving both sentences in the document and letting the implementer discover the conflict.

**One cutover obligation is recorded here so it is not lost between documents.** The supervisor resolves which holder binary to run in a fixed order, and the order is exact:

1. An **explicit operator-specified binary path**, supplied through whatever configuration surface the supervisor exposes, with surrounding whitespace trimmed. If one is set, it is the answer or there is no answer: a value that does not name an executable regular file is a **fatal error, not a reason to try the next candidate**. An operator who specifies a path has stated an intent, and silently running a different binary than the one they named is the worst available outcome — it is indistinguishable from success.
2. The holder shipped in the same release, at `libexec/moor` relative to the supervisor's own installation.
3. An **absolute** path obtained by resolving the name against the search path.

A bare name resolved at run time is never used. An atomic cutover depends on that order: a bare name would let a stale copy elsewhere on the machine answer for the new protocol, which is precisely the mixed-version window §0.2 forbids. This belongs to the cutover's definition of done rather than to the program's own behaviour, but it is a condition of the guarantee above.

---

## 1. What the program is

A session holder for terminal programs. It runs a child program under a pseudo-terminal, keeps that child alive independently of any viewer, and lets viewers attach and detach at will. A session outlives the terminal that created it: closing the window, losing the network, or killing the viewer leaves the child running.

Compare it to a terminal multiplexer with everything removed except this one job. There are no windows, no panes, no status bar, no configuration language, no scrollback UI, no copy mode.

### 1.1 The transparency guarantee — the defining property

**The program MUST NOT interpose its own terminal emulator between the child and the viewer.** This is literal byte transport over a pseudo-terminal: Moor MUST pass the bytes unchanged and MUST NOT add another parser, renderer, or normaliser.

Bytes the child writes reach the attached viewer unaltered and in order. Bytes the viewer types reach the child unaltered and in order. The program does not rewrite or normalise the stream in either direction. Its only parsing or side-channel copies are the narrowly scoped cases this document names explicitly: detach-key detection (§6), logging (§7), terminal-state observation and bounded mode tracking (§9), and capability arbitration (§10). None changes the child's output bytes delivered to a viewer. The only observation-induced delay is §10.2.7's possible-query candidate: at most 32 bytes for at most 50 ms; arbitrary output is never delayed.

This is the property that makes the program worth building, and it is the property most easily lost. Consequences that MUST hold:

- Mouse reporting, bracketed paste, focus events, and every other private mode work exactly as they do without the program in the path.
- Application cursor keys, alternate screen switching, and scroll regions are the child's business; the program has no opinion.
- Colour depth is not reduced. Sixel, Kitty graphics, and any other byte sequence the viewer's terminal understands pass through.
- A program that queries the terminal (cursor position, device attributes, colour palette) receives the lease-holding *viewer's* answer only when that viewer supplies a valid reply within the arbitration deadline. Only after that opportunity may the holder answer one of its frozen synthetic classes, and only when it supplied the terminal identity itself (§10.2.7).
- No sequence is emitted into the child's input stream that the viewer did not type, except in the two cases below, which are the *only* permitted exceptions and are specified normatively elsewhere:
  1. **Redraw on attach** (§6), when the operator has opted in.
  2. **Terminal capability arbitration** (§10). A recognized query is first offered to an eligible attached viewer; after that opportunity the holder may synthesize only a frozen answer it can support honestly. This lets headless programs receive answers where the holder has sufficient knowledge without inventing one where it does not. A cursor-position query receives no synthetic answer, because only something tracking the cursor can answer it (§9.1); identity queries remain silent when the terminal identity was inherited rather than supplied by the holder (§4.4.2), and §10 names the other silence branches. At most one responder answers a given query. Exactly one answer is delivered only when a valid viewer reply is selected or holder synthesis is eligible; otherwise silence is a valid result. An observer never answers. A reply that is unsolicited, duplicated, of the wrong class, or belonging to a superseded generation or lease MUST NOT reach the child. The exact byte sequences, the query grammar, the opt-out, and the environment the holder presents are frozen in §10.

*This exception is load-bearing and was nearly lost.* An earlier draft of this document forbade synthetic replies outright, which would have removed a mechanic the product depends on: without it a headless terminal program receives no answer to its capability query and either stalls or falls back to a degraded rendering mode. Transparency means the program does not *alter* the stream; it does not mean the program is absent from it.

A separate viewer-only exception to byte-for-byte viewer delivery exists and MUST be stated, because omitting it makes the conformance test below untrue: on attach the holder sends the viewer a **terminal-state preamble** restating the modes the child established before that viewer arrived when its bounded tracker remains exact (§5.2). It is not a third exception to the child-input rule above: none of its bytes may reach the child. The required frame is empty when tracking is inexact, or when the viewer requested `NON_VT`; only the inexact branch clears tracked exactness in the acknowledgement. Preamble bytes were never written by the child. They are addressed to the viewer only, are not part of the child's output stream, and MUST NOT be logged or advance any output cursor. Without an exact preamble a VT viewer that arrives mid-session may render the session wrongly, and the degraded flag makes that limitation explicit.

A conformance test for this section therefore compares the **child's output stream** with and without the holder in the path and requires the two captures to be identical, while accounting separately for the preamble and for arbitration replies (§10). A test that naively compares everything the viewer receives will fail against a correct implementation — and an implementer who then "fixes" the code to satisfy it has deleted the preamble and broken mid-session attach.

### 1.2 What the program is not responsible for

- Rendering. It never draws anything on the viewer's screen except its own diagnostic lines (§13).
- Terminal emulation state. It does not know what is on the screen.
- Reflow on resize. That is the child's job; the program only propagates the size.

---

## 2. Naming, discovery, and the session root

### 2.1 Session names

A session is identified by a name supplied on the command line. Two forms are accepted and MUST be distinguished by whether the name contains a path separator:

- **A bare name** (no separator) identifies a session inside the per-user session root (§2.2). This is the form a human types.
- **A path** (contains a separator) identifies the session's filesystem rendezvous object at exactly that location: a Unix-domain socket (§12.2). The program MUST NOT create parent directories for this form; if the parent does not exist, the operation fails (§13). This is the form an automated supervisor uses, because it places sessions in a directory it controls.

Names MUST be treated as opaque native path values. Two names are the same session only when their **tagged canonical identities** match (OB-17): tag `01` plus the lexically resolved absolute socket-path bytes. The tag is part of the identity. A spelling, alias, or symbolic-link target is never substituted for the resolved path bytes.

**[NEW] Naming must not let one session destroy another.** OB-1 chooses the reserved-suffix shape. The final component of every bare or path-form session name is rejected if it collides, under §14.1's platform comparison, with `.log`, `.events`, `.exit`, or `.instrument`. No spelling is normalized into another name. Before enforcement, the migration inventories and deliberately drains every colliding or platform-alias spelling.

**Companion paths are one frozen native-path mapping.** Let `R` be the resolved native rendezvous path, preserving its parent and final component. The log directory is obtained by appending `.log` to the final native path component of `R`, not by joining a child named `.log`; the lifecycle directory is obtained by appending `.exit` to the final native path component the same way. For example, `/a/name` maps to `/a/name.log` and `/a/name.exit`, never `/a/name/.log` or `/a/name/.exit`. The caller-supplied event path must be a fully qualified absolute native path and is then used exactly as supplied to `-T`; Moor neither resolves it against a working directory, rewrites its spelling, appends `.events`, nor derives an event path from `R`. The `.events` spelling remains reserved against session-name collision. Instrumentation keeps §4.7's mapping: the owner-only root contains the stage whose final component is exactly `<H>.instrument`, where that section defines `H`.

No operation on one session may unlink, truncate, clear, or otherwise disturb another session's rendezvous, store, or stage. Every destructive operation addresses the target relative to an already verified parent/container, does not follow a symbolic link, and re-verifies the target's object identity immediately before mutation; it never acts on a name alone. The rendezvous has its independent stale-object fence. Outside the creator's identity-recorded prepublication rollback in §11.6, removing any companion additionally requires valid owning lifecycle evidence; missing or disagreeing evidence makes every companion unowned and non-removable.

### 2.2 The per-user session root

Bare names resolve inside a directory that is private to the invoking user.

**The location is frozen per platform.** On Linux and macOS, verified against the reference: the system temporary directory, containing a directory named with a leading dot, the program's invoked base name **exactly as invoked**, a hyphen, and the invoking user's numeric id — `/tmp/.<invoked-basename>-<uid>` — created with owner-only permissions, `0700`. The final root is opened without following a symbolic link, MUST NOT itself be a symbolic link, and is owned by the invoking user with mode exactly `0700`.

What is invariant everywhere is the property, not the spelling: **the root is reachable only by the invoking user**. A copy invoked as `mo-or.probe2` uses `/tmp/.mo-or.probe2-1000`: hyphens and dots are preserved, nothing is case-folded, nothing is substituted.

**This is not the same derivation as the session variable**, and an earlier pass of this document said it was. The root uses the raw base name; the environment key applies the byte-level transformation frozen in §4.4.1, which is a different function and is not restated here — a summary of it in this section would be a second definition able to drift from the first, which is how the "same derivation" error arose. They must be implemented as two transformations of one input, never as one shared helper: the shared-helper mistake produces a program whose root and whose environment variable disagree about what session it is in.

**[NEW] The root's ownership and protection are enforced, not assumed.** Verified against the reference: a pre-existing POSIX root owned by the caller but with permissions `0755` is adopted silently, the mode is left as it is, and the command succeeds. The replacement MUST refuse to operate on a root that is not a directory, is owned by another identity, is a symbolic link, or has permissions broader than the exact rule above. It exits non-zero with a diagnostic naming the path and offending attribute (§13.1). It MUST NOT repair the protection: silently tightening it hides that somebody else created the directory, and the fact that they did is the thing worth knowing.

Local socket addresses are short — far shorter than a filesystem path — and a naive implementation therefore refuses sessions in deeply nested directories. **That capability MUST be preserved:** the reference reaches a socket whose *full* path exceeds the address limit by binding and connecting relative to its directory, so only the final component must fit. An implementation that simply rejects long paths has removed working behaviour and will be rejected.

What MUST fail, loudly, is a *final component* that cannot fit: the program says so rather than silently truncating and operating on a different session than the caller named. Resolving the directory MUST NOT rely on changing the process-wide working directory, which races with anything else in the process.

### 2.3 Liveness **[NEW — three states, not two]**

The reference defines a session as live if its socket accepts a connection. **That definition is wrong, and it is wrong in the direction that hurts**: accepting a connection proves that *something* is listening, not that it is one of our sessions.

Demonstrated against the reference with an ordinary local socket, owned by nobody in particular, that accepts and immediately closes:

- `list` reports it as a session, and decorates it **`[attached]`**.
- `push` delivers to it and exits **0**. The caller is told its input reached a child. There is no child.
- `kill` fails with `did not stop` — having already tried to terminate something it never identified.
- `rm` refuses to remove it because it "is running".

The result is a permanent phantom: an entry the user cannot kill, cannot remove, and which absorbs input while reporting success. A supervisor that derives its running set this way — and the production one does — will hold a session open forever against a socket that belongs to something else entirely.

Liveness therefore has **three** values, and every operation MUST branch on all three:

- **verified-live.** The peer's identity was checked (§11) *and* a bounded identity exchange completed, confirming this is our holder for this session. Only this state authorises attaching, delivering input, or reporting the session as running.
- **stale.** The absence of a listener was positively established. Only this state authorises removal.
- **indeterminate.** Anything else: a connection that succeeded but did not complete the exchange, a peer whose identity does not check out, a timeout, a malformed reply. This is not an error state to be smoothed away into one of the other two.

Rules that follow, and that a conforming implementation is tested on:

- **Indeterminate fails closed.** It is never reported as running, never attached to, never given input, and — critically — **never removed and never killed**. Destroying something you could not identify is how a stranger's process gets terminated, and how a live session gets unlinked (§2.1).
- **`list` MUST render the three states distinguishably.** A phantom that is indistinguishable from a working session is the whole defect.
- **A probe MUST NOT have side effects on the child.** Determining liveness may complete a handshake; it MUST NOT deliver a byte to the child. The reference's use of an empty input delivery as a liveness probe is exactly the conflation this section removes.
- **Only a stale rendezvous object may be replaced.** An indeterminate one blocks creation of a same-named session, with a diagnostic saying so, rather than being cleared.

The bounded exchange, its deadline, and the identity it establishes are frozen with the handshake in §10; §11 owns the peer-identity check. This section fixes what the *answers* mean.

---

## 3. Command surface

The program is invoked either with a session name directly, or with a subcommand. Both forms are required.

**The whole of this section is a frozen interface.** Shells, scripts, and on-disk automation are bound by §0.2 to keep working unchanged, and they were written against every spelling the reference accepts — not against the spelling its own help text advertises. A submission that implements the documented command names and rejects the undocumented ones has broken callers that this document promised not to break. The differential suite MUST therefore invoke **every** spelling in §3.6, not one canonical form per operation.

### 3.1 Implicit form

`<program> [<session> [command...]]` — attach to the named session, or create it running the given command and then attach, according to the session's liveness state (§3.7, which is authoritative for all three states). With no arguments at all, print usage and exit **0**. (Verified against the reference; a non-zero status here would be a breaking change and is not one this document makes.)

It is **not** a synonym for any single command in §3.2, and an earlier pass of this document wrongly said it was equivalent to `attach` when live and `new` when not. It differs from both: `attach` rejects a trailing command operand while this form accepts one, and `new` *fails* against a live session while this form attaches to it. §3.6 gives its own row and §3.7 gives its behaviour in each liveness state; those two are authoritative.

**Which tokens are session names.** The first operand is a session name unless it is exactly one of the command tokens in §3.6. There is no lookup, no prefix matching, and no fuzzy correction: an unrecognised word is a session name, which is why `moor mysession` works at all.

**[NEW] A token beginning with `-` is never an implicit session name.** Verified against the reference: an unrecognised single-dash token is rejected as an invalid mode, but an unrecognised *double-dash* token is silently accepted as a session name — so a mistyped long option creates a session named after the typo instead of reporting the typo. The two spellings MUST behave the same, and that behaviour MUST be rejection. A caller that genuinely wants a session name starting with `-` introduces it with `--`. *Failure prevented:* a typo that silently launches a shell nobody knows about, holding a pseudo-terminal until the machine is rebooted.

### 3.2 Session-creating commands

| command | creates | attaches | holder runs |
|---|---|---|---|
| `new <session> [command...]` | yes | yes | background |
| `start <session> [command...]` | yes | no | background |
| `run <session> [command...]` | yes | no | **foreground** |
| `attach <session>` | no — fails if absent | yes | — |

Requirements common to the creating commands:

- If `command...` is omitted, the exact candidate order is: nonempty `SHELL`; then the nonempty shell field returned for the invoking uid by the system account database, otherwise `/bin/sh` (§12.7). Each value is one native executable path, never a command line. The first nonempty candidate is authoritative: if it cannot be executed, startup fails with 127 rather than silently selecting a different shell. The shell is **not** started as a login shell and no login flag is passed. (Verified against the reference; an earlier draft of this document said "login shell", which would have changed startup-file behaviour for every session.)
- A **create-only** command — `new`, `n`, `start`, `s`, `run` — MUST fail against a session that is already live, rather than replacing it. The **create-or-attach** form — the bare form — attaches instead when it carries no create-only option, which is its purpose; with any create-only option it takes the same live-session refusal (§3.6, §3.7).
- `start` MUST NOT return until the session is either fully established or has failed. **"Accepting connections" is not the bar** — §2.3 shows that an arbitrary listener accepts connections. Two distinct gates exist and each has its own owner:

  **The launcher gate**, which `start` itself must pass before exiting zero: every caller-supplied sink and instrumentation object validated and opened (§4.6, §4.7), the child confirmed to have started, and only then the platform rendezvous made reachable and published. No path may leave a half-built holder reachable — the ordering is the guarantee.

  **The adoption gate**, which a supervising caller must pass before it treats the session as its own: a bounded identity exchange, the terminal-state preamble fully applied, and an acknowledgement carrying the exact generation it launched (§10). Until that completes the session is `indeterminate` (§2.3), not running.

  Neither gate implies the other. A clean zero from `start` says the holder built itself correctly; it says nothing about whether the holder answering at that rendezvous a moment later is the one that was just started. Conflating them is how a supervisor adopts a stranger's session, or a predecessor's.
- `run` keeps the holder in the foreground and is intended for supervision by a process manager. The exit status is the holder's.

### 3.3 Lifecycle commands

- `kill [-f] <session>` — stop the session. Without `-f`, request termination and allow a grace period for the child to exit on its own before forcing it. With `-f`, force immediately. Killing a session that is not live MUST be reported, not silently succeed.
- `rm [-a] [<session>]` — remove stale session residue; §3.7 is authoritative for what it does in each liveness state. A session that is not stale MUST NOT be removed by this command under any flag.

  **Three message forms, all exit 0, and they are distinct — a caller matching one does not match another:**

  | invocation | output |
  |---|---|
  | `rm <name>`, removed | `session '<name>' removed` |
  | `rm -a`, per entry removed | `removed <name>` |
  | `rm -a`, closing line | `<count> session(s) removed` |
  | `rm -a`, nothing to do | `nothing to remove` |

  **[NEW]** `rm -a` reports every entry it skipped, one line each, naming why — `skipped <name> (running)` or `skipped <name> (indeterminate)`. The closing count counts removals only. The status stays **0** when the only reason for skipping was that a session was live or indeterminate: those are correct outcomes, not failures, and a non-zero status would make routine cleanup look broken. `-q` suppresses the per-entry and closing lines as informational (§13.4); it does **not** suppress skip lines, because a skip is the operator's cue that residue remains.

  `list` entries and `rm -a` entry lines are ordered by rendered-name bytes ascending. Filesystem enumeration order and locale collation never participate.

- `list [-a]` — enumerate sessions in the per-user root (§2.2) only; sessions addressed by path are not listed. Discovery is a **union of two independent sources** (§4.5): the addressable rendezvous objects present in the root (sockets), and the durable exit records — each source enumerated on its own, because either artefact may exist with or without the other. The two are then merged by name and classified by the cross-product below, which also fixes what each of `list`, `list -a` and `rm` does with every combination. An empty result prints exactly `(no sessions)` and exits **0**.

  The line grammar is **frozen**, because a supervisor parses it — it is an interface, not a convenience. The rendered name is left-justified in a field of **24 bytes**, followed by a single space and `since <age-text>`, followed — only when a status applies — by a space and one bracketed word. A known age-text is `<n>s ago`, `<n>m ago`, `<n>h ago`, or `<n>d ago`; when boot identity/monotonic arithmetic is unavailable it is exactly `unknown`, with no trailing `ago`. A name of 24 rendered bytes or fewer is padded to the field width; a longer name is **not** truncated and not realigned, so the separator collapses to the single space and later columns shift right. A parser MUST NOT assume fixed offsets.

  **Rendered bytes, not display columns.** The reference pads raw name bytes, but that makes the parsed line injectable by a newline or by its own delimiters. Breaking correction 4 applies OB-29's reversible ASCII rendering first and then pads that byte string. A session whose POSIX name bytes are UTF-8 `é` therefore renders as `\xC3\xA9` — eight ASCII bytes — and is followed by 16 spaces. A rendered name longer than 24 bytes is not truncated. Conformance vectors MUST cover a short safe ASCII name, a rendered name of exactly 24 bytes, one past it, a multi-byte name, a space, a backslash, a quote, `>`, `[`/`]`, newline and invalid UTF-8 bytes.

  The bracketed vocabulary is **closed**, and it is four words, not three:

  | word | liveness state (§2.3) | meaning |
  |---|---|---|
  | *(none)* | verified-live | no viewer attached |
  | `[attached]` | verified-live | a viewer is attached |
  | `[exited]` | **stale** | no rendezvous object; a durable exit record survives. Shown only with `-a` (§4.5) |
  | `[stale]` | **stale** | a rendezvous object with no listener, left by a holder that died |
  | `[indeterminate]` | indeterminate | **[NEW]** something is listening and could not be identified as ours |

  **These are five renderings of three states, not five states** — but the two stale renderings differ in *what is on disk*, and an earlier pass of this document blurred that in the sentence immediately after defining it.

  Precisely: **liveness** has three values and every behavioural decision branches on those (§2.3). What is on disk under a name is an independent, two-by-two question, and an earlier pass of this document listed only two of the four cells and then introduced a third in the paragraph below the table.

  **[NEW] The full cross-product.** A rendezvous object may be present or absent; an exit record may be present or absent. All four combinations occur — a holder that crashed after writing its record but before unlinking its socket, a cleanup interrupted midway, a new session created over an old record. In this table, "exit record" means a lifecycle companion whose selected commit and manifest independently pass §11.6's ownership checks for the requested session. A directory merely occupying the `.exit` path, or a manifest that disagrees, is not an exit record and grants no removal authority.

  | rendezvous object | exit record | liveness | `list` | `list -a` | what `rm` removes |
  |---|---|---|---|---|---|
  | present | absent | probed (§2.3) | live / `[stale]` / `[indeterminate]` | same | the rendezvous object, only if `stale` |
  | present | present | **probed — the rendezvous decides** | live / `[stale]` / `[indeterminate]` | same | rendezvous object **and** record, only if `stale` |
  | absent | present | stale | *not shown* | `[exited]` | the record |
  | absent | absent | the name does not exist | — | — | nothing; `does not exist` |

  Three things in that table are **choices this document is making**, not facts read off the reference, and they are labelled as such because the reference has no behaviour here to copy:

  - **The rendezvous decides when both exist.** It may have a live holder behind it; an exit record is by definition historical. Trusting the record would report a running session as finished — the one error that cannot be recovered by looking again.
  - **A combined entry renders identically to a rendezvous-only one**, in both `list` and `list -a`. The record adds no information the probe did not already establish, and a distinct rendering would expose an internal artefact as though it were a session state.
  - **Removal takes both independently fenced artefacts.** When the combined row contains a positively stale rendezvous and a valid lifecycle companion, `rm` removes both even when they came from different historical generations; generation equality between those two stale artefacts is neither required nor inferred. §11.6 revalidates each object under its own fence. Removing only one is how residue becomes unremovable, while treating a missing or disagreeing manifest as ownership is how another session's companion gets deleted.

  Shape is never a fourth liveness state. Nothing may be attached to, delivered to, or killed differently because of which artefacts exist; shape decides only discovery, rendering, and what removal unlinks.

  **`[indeterminate]` is a new word in a parsed output stream**, and §0.2 requires that to be declared rather than slipped in. It is added because the alternative is worse: today such a POSIX socket is rendered as `[attached]`, which tells a supervisor that a session it does not own is one of its own working sessions (§2.3). A caller that does not know the word sees an unrecognised status, which is the correct outcome — better than a recognised and wrong one.

- `current` — print the session the invoking process is running inside. Sessions nest, and the command prints the **whole ancestry**, outermost first, not only the innermost. Outside any session it fails with exit **1**; verified against the reference, it prints nothing at all in that case, and this document freezes that silence rather than adding a diagnostic — the shell idiom `if name=$(… current); then` depends on the empty capture. Attaching to the session one is already inside, or to any of its ancestors, MUST be refused rather than producing a loop.

  **[NEW] That refusal is decided from live process ancestry, not from the environment (OB-41).** The session variable is inherited by every descendant and is never cleared, so a process whose ancestor session has long since ended still carries it — and deciding from the variable then refuses an attach that would have been perfectly safe. The holder walks the actual process ancestry of the caller and refuses only when a live holder for the target session is genuinely among its ancestors. The variable stays descriptive: it tells a program where it is, it does not decide what may be attached.

  Verified against the reference, the rendering is the **final path component** of each generation, joined outermost-first by the three characters space-greater-space: a session `curtest` prints `curtest`, and a session `inner` created inside `outer` prints `outer > inner`. Trailing operands are accepted and ignored, which §3.6 corrects.

  The ancestry is carried in the child's environment (§4.4), and its encoding is frozen there. It is not free choice: `current` is the parser of whatever §4.4 writes.

### 3.4 Input and log commands

- `push <session>` — read the invoker's standard input and deliver it to the child as if typed. Terminates when standard input reaches end of file. This exists so an automated caller can inject input without holding a terminal. **[NEW]** Naming a session that does not exist MUST produce the same diagnostic shape as every other command (§13). Verified against the reference, this one command instead surfaces a raw system error carrying the absolute socket path, disclosing the session root's layout and breaking the uniformity a caller matches on.
- `tail [-f] [-n N] <session>` — print the last `N` lines of the session log (§7), default 10; with `-f`, continue printing as the log grows. `N` is canonical unsuffixed u32 decimal; `tail -n 0` emits no existing lines and, with `-f`, follows only bytes committed later.
- `clear [<session>]` — make the session log empty without disturbing the session. With no name it targets the innermost valid current session; when there is no valid current session it succeeds silently. Naming a session that does not exist likewise succeeds silently with exit **0**. Verified against the reference, and **deliberately frozen rather than corrected**: `clear` asserts an end state ("this log is empty") which already holds, and blind `clear` calls in cleanup scripts are the expected use. Against a verified-live session it uses the ordered `LOG_CLEAR` operation of §10.2.15; against confirmed stale residue it commits an empty replacement offline under §11.6's writer lease; against indeterminate residue it exits 1 without mutation. A submitted clear whose durable result cannot be determined uses §13.3's frozen indeterminate diagnostic and is never retried automatically.

### 3.5 Options

| option | effect |
|---|---|
| `-e <char>` | set the detach key (§6). Default is the control character conventionally written `^\`. |
| `-E` | disable the detach key entirely: no keystroke detaches. |
| `-r <method>` | how the child is prompted to repaint on attach: `none`, `ctrl_l`, or `winch` (§6). |
| `-R <method>` | how the viewer's screen is cleared on attach: `none` or `move`. |
| `-z` | disable the suspend key, so the viewer cannot suspend the session holder. |
| `-q` | suppress the program's own informational messages (§13). |
| `-t` | disable assumptions about the viewer being VT100-compatible. |
| `-C <size>` | cap the session log (§7). `0` disables logging. Accepts a plain byte count or a suffix `k`, `m`, or `g`, ASCII-case-insensitively. Default is one mebibyte. Numeric grammar in §3.6. |
| `-2 <path>` | send the child's standard error to the named file instead of the pseudo-terminal (§4.6). |
| `-T <path>` | write the bounded event stream (§8) to the named portable store directory (§8.1). |
| `-S <path>` | load the named launch-time instrumentation object into the initial child before its first application instruction (§4.7). |
| `-d <path>` | **[NEW, OB-32]** run the child with this working directory. |

The working-directory option is **[NEW]**: the current program accepts an argument vector and no directory, so every automated caller wraps its child in a shell that changes directory first — adding a process to every session, changing which process receives terminal-generated signals, and making a failed directory change indistinguishable from a failed command. The path must be an existing directory the invoking user can enter; if it cannot be entered the session is not created and the diagnostic names the directory, distinctly from a child that could not be executed (§13.1). Without `-d` the child inherits the creating process's directory, as today.

**Option placement is a three-phase grammar.** Options may surround the session operand after the command token and until the first child-command operand. In the bare form the session is consumed first, after which options are recognised until the child command. `--` ends option recognition and may introduce a dash-leading session. A token before a modern command, such as `-T <path> start`, is an invalid mode. Every surviving spelling in §3.6 gets a conformance vector in every legal phase, and every removed spelling gets a rejection vector.

Repeated booleans are idempotent. Repeated scalar options use the last occurrence; among mutually exclusive choices, including `-e` and `-E`, the last occurrence wins. Defaults are `-r none`, `-R none`, detach byte `1C`, suspend byte `1A`, log cap 1 MiB, and tail count 10.

`-e` accepts either one printable ASCII byte `20..7E` or exactly two bytes of caret notation: `^@` through `^_` map to `00..1F`, and `^?` maps to `7F`. Lowercase after `^` is invalid; a literal caret is the one-byte argument `^`. No locale or Unicode decoding participates.

### 3.6 The frozen token grammar

The reference accepts more spellings than its help text lists. All of them are in production use and all of them MUST be implemented.

**Command tokens.** Revision v2 removed the reference tool's dash-spelled command tokens (`-a`, `-A`, `-c`, `-n`, `-N`, `-p`, `-k`, `-l`, `-i`) from the grammar: the word commands and their short word forms below are the entire command surface. A removed spelling MUST be rejected as `Invalid mode '<token>'` — never reinterpreted as a neighbouring command and never allowed to fall through to the bare form, where `-A session` would have silently created a session named `-A`. Each removed spelling gets its own rejection vector, because the old rows were not aliases of one another and a regression could resurrect any one of them independently.

Behaviour of the remaining rows was established black-box against the reference, under a real pseudo-terminal where attaching required one.

| token(s) | operands | session missing | session live | attaches | holder | announces on create |
|---|---|---|---|---|---|---|
| `attach`, `a` | name only — a command is `Invalid number of arguments` | fail `session '<name>' does not exist`, **1** | attach | yes | — | — |
| bare `<name>` (§3.1) | name, optional command | create, then attach | attach, unless a create-only option was supplied — then fail `already running`, **1** | yes | background | `session '<name>' created` |
| `new`, `n` | name, optional command | create, then attach | fail, **1** | yes | background | `session '<name>' created` |
| `start`, `s` | name, optional command | create | fail, **1** | no | background | `session '<name>' started` |
| `run` | name, optional command | create | fail `session '<name>' is already running`, **1** | no | **foreground** | silent |
| `push`, `p` | name only | fail, **1** | — | — | — | — |
| `kill`, `k` | name only, accepts `-f` | fail, **1** | terminate | — | — | `session '<name>' stopped`; with `-f`, `session '<name>' killed` |
| `list`, `l`, `ls` | `-a` | — | — | — | — | — |
| `current` | none | — | — | — | — | — |
| `rm` | exactly one of `-a` or name | fail, **1** | refuse (§3.3) | — | — | three frozen forms, §3.3 |
| `clear` | optional name | succeed, **0** (§3.4) | ordered committed live clear | — | — | — |
| `tail` | `-f`, `-n N`, name | fail `no log for session '<name>'`, **1** | — | — | — | — |
| `--help`, `-h`, `?`, no arguments | none | — | — | — | — | — |
| `--version` | none | — | — | — | — | — |

**Only `run` puts the holder in the invoking process.** Every other creating form leaves a holder in the background and, where it attaches, the invoking process is merely a *viewer* of it. The distinction is not observable by timing — an attaching form also blocks — and an earlier pass of this document got it wrong for exactly that reason, recording the holder as running "in the caller" wherever the command did not return promptly. The observable test is what survives the invoking process: kill the client of `new` or the bare form and the session is still listed; kill `run` and the session is gone, because there the holder *was* the process.

**Success messages are frozen too, not only creation announcements.** Callers match on these: `session '<name>' stopped` for a graceful termination and `session '<name>' killed` for a forced one — two different strings, so a caller can tell which path ran. Removal has **three** distinct forms depending on whether it was addressed by name or in bulk; they are tabulated in §3.3 and are not interchangeable. The `announces` column above covers creation only because that is where the *spellings* diverge; every message named anywhere in §3 is part of the frozen surface.

**Usage and version.** Let `<p>` be the OB-29 rendering of the invoked basename and `<v>` the build's canonical SemVer 2.0.0 release token: ASCII, no leading `v`, and no whitespace. `--version` writes exactly `<p> <v>` plus LF to **standard output** and exits **0**. Help, `-h`, `?`, and no arguments write that version line followed immediately by this exact LF-terminated block to standard output and exit **0**; neither surface writes standard error:

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

`<p>` is substituted on every line; the angle-bracket operand spellings remain literal. Attach/create options are accepted by the bare form, `new`/`n`, `start`/`s`, `run`, and `attach`/`a`. Create-only options are accepted exactly by the bare form, `new`/`n`, `start`/`s`, and `run`; attach and every non-creating command reject them through the option-ownership diagnostic. A create-or-attach form carrying any create-only option against a verified-live session takes the frozen create-only-against-live refusal rather than attaching or ignoring the option. No command silently ignores one. `-q` is additionally accepted by `kill` and `rm`. Lifecycle, input, and log commands accept only the options displayed on their own help row. `rm -a` accepts no name; `rm` without either `-a` or a name is an argument error. Combining `-t` with `-R move` always reports `Invalid value 'move' for option '-R'`, regardless of token order.

**Diagnostic streams.** Two conventions exist in the reference and both are observable, so both are frozen:

- **Argument and command errors** — unknown mode, missing operand, unparsable option value — go to **standard output**, as `<program-name>: <message>` followed by a second line `Try '<program-name> --help' for more information.`, exit **1**. `<program-name>` is the program's name **as invoked**, not a fixed literal.
- **Child-startup failure** goes to **standard error**, as `<program-name>: could not execute <path>: <system error>`, terminated **CRLF**, exit **127**. The CRLF is not an accident: the message is delivered through the pseudo-terminal, where a bare newline would leave the cursor mid-column.

An implementation that routes all diagnostics to standard error is more conventional and MUST NOT be built: callers redirect these streams and parse what lands.

**[NEW] Numeric operands are parsed strictly.** Verified against the reference, every numeric operand currently accepts the whole byte string and takes what it can: `-C -5` and `-C 99999999999999999999` are accepted silently, and `tail -n garbage`, `tail -n -1` and `tail -n 0` each yield exactly one line instead of an error or the documented default. `-C` accepts a canonical u64 decimal byte count, or that decimal followed by exactly one ASCII-case-insensitive `k`, `m`, or `g`, multiplying by 1024, 1048576, or 1073741824; multiplication overflow is invalid. `tail -n` accepts canonical **unsuffixed** u32 decimal, including zero. Every numeric spelling is nonempty, has no sign, whitespace, leading zero except the value `0`, or trailing byte. Invalid input uses the argument-error diagnostic and exits **1**. *Failure prevented:* a size that wraps to a small cap, silently disabling the log a caller believed was on.

**[NEW] Trailing operands are rejected.** Verified against the reference, `list unexpected extra` exits 0 and ignores the extra words; `current` behaves the same way. A caller who typed a session name after `list` believing it filters gets a full listing and no warning. Any command given more operands than it defines MUST fail with the argument-error diagnostic.

### 3.7 Every command against all three liveness states

§2.3 replaced a two-valued notion of liveness with three values. That change is worthless until each command says what it does with the third, and the rest of §3 was written when only two existed — so read literally it creates a session on top of an indeterminate one, unlinks it, and hides it from `list`. This table supersedes any such reading.

`indeterminate` means: something is listening, and we could not establish that it is ours (§2.3).

| command | verified-live | stale | indeterminate |
|---|---|---|---|
| bare `<name>` | attach, unless a create-only option was supplied — then fail `already running`, exit 1 | replace residue, create, attach | **refuse**, exit 1 |
| `attach`, `a` | attach | fail `session '<name>' is not running` | **refuse**, exit 1 |
| `new`, `n` | fail `already running` | replace residue, create, attach | **refuse**, exit 1 |
| `start`, `s`, `run` | fail `already running` | replace residue, create | **refuse**, exit 1 |
| `push`, `p` | deliver | fail `session '<name>' is not running` | **refuse, deliver nothing**, exit 1 |
| `kill`, `k` | terminate | fail — nothing is running to stop | **refuse to terminate**, exit 1 |
| `rm <name>` | refuse, `is running` | remove | **refuse**, exit 1 |
| `rm -a` | skip | remove | **skip**, and say so |
| `list` | render live | render by artefact shape — `[stale]` or `[exited]` (§3.3) | render **`[indeterminate]`** |
| `tail` | read the selected committed log | read the selected committed log | read the selected committed log |
| `clear` | use ordered live `LOG_CLEAR` | clear offline under the writer lease | **refuse without mutation**, exit 1 |

The refusals are the point of the section:

- **Nothing indeterminate is ever destroyed.** Not unlinked, not terminated, not replaced. The rendezvous may belong to a stranger's process, or to a successor of the session the caller meant, and neither is ours to end (§5.1).
- **Nothing indeterminate is ever written to.** `push` in particular MUST NOT deliver, because delivering is how the reference reports success against a socket with no child behind it.
- **Refusal is loud.** Each refusal exits non-zero with a diagnostic that names the state — a caller must be able to distinguish "there is no such session" from "there is something there and I could not identify it", because those call for opposite responses: create versus investigate.
- **`rm -a` reports what it skipped.** A bulk removal that silently leaves entries behind teaches the operator that the residue is unremovable.

**`tail` is liveness-independent** because it is read-only and selects a durable committed log. A live `clear` is necessarily coordinated with the holder, a confirmed-stale `clear` is an offline committed replacement under the writer lease, and an indeterminate `clear` cannot prove there is no live writer and therefore refuses without mutation. Success of either command MUST NOT be read as evidence of liveness by any caller.

**`list` gets a bounded total budget.** Classifying a rendezvous now costs a handshake with a deadline, so a root holding many sessions could otherwise take the number of sessions times that deadline. `list` MUST bound the *whole* operation, probe concurrently, and render every rendezvous it could not resolve within the budget as `[indeterminate]` — which is exactly what it is. A listing that hangs is a listing nobody runs.

---

## 4. Running the child

### 4.1 The pseudo-terminal

The child runs with a pseudo-terminal, with standard input, output, and (unless `-2` is given) standard error attached to it. **Terminal-generated events reach the child and not the holder** — by making the child a session leader with that terminal as its controlling terminal (§12.3).

### 4.2 Terminal settings

When a viewer creates a Linux or macOS session, the child's terminal settings MUST be initialised from that viewer's terminal, so the child starts with the same line discipline, control characters, and modes the user already had. When no viewer is present at creation (`start` from a non-terminal caller), the child MUST receive a sane default configuration rather than an uninitialised one: canonical input, echo on, standard control characters, and a defined input and output speed.

**On Linux and macOS the field list is every setting of the creating viewer's terminal** — input, output, control and local modes, every control character, and both speeds. Transferring a subset is what produces a child whose line discipline differs from the terminal the user was just using, in ways that appear only under an editor or a full-screen program. A control character the platform does not define is left at that platform's default rather than zeroed. This is conformance-tested against a viewer with non-default settings, not asserted (**OB-5**).

For a headless POSIX creation every mode word starts zero and every control-character slot starts `_POSIX_VDISABLE`. Moor then sets `c_iflag=ICRNL|IXON`, `c_oflag=OPOST|ONLCR`, `c_cflag=CS8|CREAD`, and `c_lflag=ISIG|ICANON|IEXTEN|ECHO|ECHOE|ECHOK|ECHOCTL|ECHOKE`. Input and output speeds are `B38400`. Control bytes are `VINTR=03`, `VQUIT=1C`, `VERASE=7F`, `VKILL=15`, `VEOF=04`, `VSTART=11`, `VSTOP=13`, `VSUSP=1A`, `VREPRINT=12`, `VDISCARD=0F`, `VWERASE=17`, `VLNEXT=16`, `VMIN=01`, and `VTIME=00`; `VEOL` and `VEOL2` remain disabled. Linux also leaves `VSWTC` disabled. macOS sets `VDSUSP=19` and `VSTATUS=14`. A listed symbolic flag or control unavailable on its named supported platform is a build failure, not a silently different default.

### 4.3 Window size

The child's terminal has a size. It is set from the creating viewer's size, or to 80 columns by 24 rows when there is no viewer at creation. A real geometry has each dimension in `1..32767`; validation widens both operands before multiplication and requires the product to be at most `2,000,000`. Preserve is exactly `0 x 0`; a mixed-zero pair is malformed and changes nothing.

Whenever the size changes — because a viewer is granted the input lease with a different requested size, or because the lease holder's terminal is resized — the child MUST be told, and MUST otherwise be told only when the value actually changes. The sole exception is an explicitly selected `winch` redraw: it sends one `RESIZE` even when the validated geometry equals the current value (§6.3). An observer that does not hold the lease never changes session geometry (§6.1).

A resize MUST NOT be inferred from a viewer that has no terminal. An automated attach without a terminal MUST leave the child's size untouched, because a supervisor attaching to inspect a session must not shrink it to nothing. This is a real failure mode: a session left at a tiny size by an inspecting client is indistinguishable, to the user, from a corrupted display.

**This requires an explicit encoding, not an omission.** The attach exchange carries both dimensions as ordinary unsigned fields. `0 x 0` means preserve; either mixed-zero form is a protocol error. Conformance vectors cover preserve, both mixed cases, `1`, `32767`, `32768`, products immediately below, at, and above `2,000,000`, and multiplication/conversion overflow.

### 4.4 Environment

The child's environment is the environment of the process that created the session, plus the variables this document explicitly owns: the ancestry carriers below, the terminal-identity matrix of §4.4.2, the generation pair of §10.1, the semantic token of §10.3 when enabled, the platform preload variable modified by `-S`, and the private-channel selector/nonce variables used only during launch (§10.1.1, §4.7). Every variable outside that closed set passes through unchanged. The supervised-launch selector is consumed by the holder and never reaches the requested child. The instrumentation selector and nonce reach only the requested child and are consumed and removed by the instrumentation initializer before its first application instruction. The generation pair may be rejected or stripped because it is a freshness fence rather than configuration; the other owned variables follow their own exact rules. No unrelated variable is inspected, rewritten, or removed.

#### 4.4.1 The session variable

One versioned variable records the session the child is running inside, carrying the full ancestry without delimiter ambiguity. Its **name is derived from the program's own invoked base name**, so a program installed under a different name writes a differently named variable, and a session created by one name is invisible to `current` run under another. This is load-bearing in both directions: it is what lets a renamed or vendored copy coexist with a system-wide one, and it is what makes the variable name un-hardcodable in the implementation. Revision 4 removed the reference tool's colon-joined legacy carrier entirely: it is neither written nor read, because its delimiter was ambiguous for any name containing a colon.

**The derivation is frozen, and it is not the root's derivation (§2.2).** Verified against the reference, applied to the invoked base name in this order:

1. Each ASCII letter is upper-cased. Bytes outside ASCII are not letters and are not case-folded.
2. Every **byte** that is not an ASCII letter or digit — hyphen, dot, anything else — becomes an underscore. This is a byte-by-byte transformation, not a character-by-character one: a base name containing a two-byte character yields **two** underscores, and a base name that is not valid UTF-8 is transformed without complaint because no decoding is attempted.
3. The result is truncated so that the **complete key, including the `_SESSION_V2` suffix, is at most 127 bytes**; the name portion is therefore capped at 116 bytes. Truncation counts bytes and may split a multi-byte character — which is harmless here only because step 2 has already replaced every such byte with an underscore.
4. `_SESSION_V2` is appended.

The byte-level statement matters because the surrounding surfaces once disagreed about it. **OB-29 settled one contract across all of them**: native path representation on native surfaces (POSIX bytes); and canonical padded base64 for the tagged identity in JSON (§8.4.1.1).

Line-oriented human surfaces — `list`, diagnostics and `current` — use one exact reversible ASCII rendering. First obtain the POSIX native bytes. Bytes in `[A-Za-z0-9._/-]` are emitted unchanged; every other byte is emitted as the four ASCII bytes `\xHH`, with uppercase hexadecimal. Backslash is therefore always escaped, so decoding is unambiguous. Padding and width rules count the rendered bytes. This also escapes spaces, quotes, brackets, `>`, non-ASCII bytes and every control byte: no legal name can inject a line, imitate `current`'s ` > ` delimiter or `list`'s status grammar, and no surface silently drops a byte. This subsection freezes only its own key transformation; OB-29 freezes the shared rendering.

A copy invoked as `mo-or.probe2` writes `MO_OR_PROBE2_SESSION_V2` while rooting itself at `/tmp/.mo-or.probe2-1000`. A 130-character base name yields a key of exactly 127 bytes. Truncation is silent and is a genuine collision risk between two long names sharing a 116-byte prefix; it is frozen here because callers already read these keys, and any change to it belongs in a `[NEW]` clause of its own rather than in an implementer's judgement.

Each ancestry entry is the **absolute addressable rendezvous path** of the session, not the bare name: the socket path. Entries are ordered outermost-first.

`_SESSION_V2` is the authoritative carrier for new readers. Its value is ASCII `v2:` followed by one or more canonical padded-base64 entries joined by `:`. An entry encodes the exact native path bytes. The base64 alphabet contains no colon, so splitting first and decoding second is unambiguous; an empty entry, non-canonical base64, or unknown prefix is malformed. The holder writes the carrier on every new session and appends the new path to it when sessions nest.

`current` reads `_SESSION_V2` and nothing else. A malformed value is reported and never guessed at; the absence of the variable means "outside any session". A contradictory legacy-carrier value in the environment changes nothing — it is not consulted, which is pinned by mutation. Before launching a nested child the holder constructs the complete value and verifies that the platform can carry the resulting environment; overflow is a launch refusal before publication, never truncation or loss of outer ancestry.

**[NEW] The separator MUST be unambiguous.** Verified against the reference: a session whose name contains a colon is reported by `current` as two nested sessions — `has:colon` prints as `has > colon`. A single real session is displayed as an ancestry that does not exist. This contradicts §2.1, which requires names to be opaque bytes, and it is not cosmetic: the value is how a program inside a session learns which socket it belongs to, and a caller that splits on the separator addresses the wrong path or a path that does not exist.

The specification requires an encoding that round-trips every byte a session name may legally contain. What is **not** permitted is the retired arrangement, in which the ambiguity was unrepresented and the consumer guessed.

**This is OB-6, and it was a product decision; the analysis below is retained as its decision record.** Four candidates, each assessed on round-trip fidelity, what an existing consumer does with a new value, what a new consumer does with a legacy value, compatibility class under §0.2, and what migration it needs:

| | round-trips all bytes | old consumer reading a new value | new consumer reading a legacy value | compatibility class | migration needed |
|---|---|---|---|---|---|
| **A** escape `:` and `\` in place | yes | correct for unaffected names; **silently wrong** for names containing `:` or `\` | **ambiguous forever** — a legacy value containing `\:` is indistinguishable from a new escape | breaking for affected values: env bytes *and* `current` output | needs a version discriminator to be safe at all |
| **B** length-prefixed `<count>:<bytes>` | yes | **deterministically misparsed** — it splits on the colon and may return a plausible false ancestry without any error | unambiguous **to a new parser**: a legacy value begins with `/`, a new one with a digit | breaking for all values | flag day; the new parser can detect the old form, the old parser cannot detect the new one |
| **C** forbid `:` in session names | yes, vacuously | correct always | correct always, except a colon-named session created before the change and still running | breaking at **creation** only; no value's bytes change | reject at creation, **plus a disposal plan for sessions that already exist** — see below; they do not expire on their own |
| **D** versioned second carrier | only for a **new** producer read by a **new** consumer | ignores the new carrier and reads the untouched legacy one — **still silently wrong** for colon names | a legacy-only session has no new carrier, so the fallback lands on the same ambiguous string — **still wrong** | see below — this is the load-bearing cell | dual-write/read rollout, precedence when the new carrier is missing or malformed, an explicit mismatch action, legacy-only fallback semantics, and retirement |

Three corrections to an earlier pass of this table, each of which moved the ranking:

- **A is not uniquely capable of being confidently wrong.** An earlier pass claimed B "fails loudly"; it does not. An old consumer splits `<count>:<bytes>` on the colon and can return a false ancestry successfully. B's self-discrimination helps only a parser that already knows to look for it — which is never the old one. Both A and B are silently misread by existing consumers; they differ in whether a *new* consumer can recover.
- **D is not additive, if it closes this obligation.** An earlier pass called D free of OB-24. It is not. Correcting `current` for a colon name **is** the observable change — that is the whole point of OB-6 — and it changes external output relative to the reference. D can dual-write a second carrier while leaving `current` producing the old wrong ancestry, but then the defect is not fixed and OB-6 stays open. D buys the gentlest *migration*, not an exemption.
- **D's migration is not "none".** It needs everything in its last cell above, including a disagreement rule this document has not chosen.

**Option C's migration needs a sub-decision, because nothing ends a session by itself.** An earlier pass of this table wrote "pre-existing sessions age out", which is not a migration — it is an assumption that the problem removes itself. It does not: a holder lives until its child exits (§4.5), and §5.1 forbids anything a peer does from terminating a session. There is no time-to-live and no upper bound on a session's life. A colon-named session started the day before the restriction can outlive the release indefinitely, and the restriction cannot make it disappear.

Whoever chooses C must therefore also choose one of:

- **Inventory and drain.** Enumerate colon-named sessions across the fleet, terminate or rename them deliberately, and only then enforce the restriction. Honest and bounded, and it requires operator action on live work.
- **Indefinite legacy tolerance.** Enforce the restriction only at creation and accept that pre-existing colon-named sessions keep reporting a false ancestry for as long as they run. The defect is then bounded to a shrinking, known set rather than fixed.
- **Atomic cutover with proven termination.** Restrict creation and require that every pre-existing session be gone before the new version is considered live — the strongest guarantee and the most disruptive.

The transitional behaviour of `current` must be named alongside whichever is chosen: while a legacy colon-named session is still running, `current` either keeps reporting the false ancestry, or reports the correct one — and the second is an externally observable correction, which puts it back under OB-24 exactly as the other options are.

**The conclusion that matters, and it constrains the choice rather than making it:** under strict external parity there is **no** option that corrects `current` for colon-named sessions. Correcting it is, by construction, a change an external caller can observe. So OB-6 cannot be closed in any variant unless OB-24 permits a finite enumerated exception — and if OB-24 chooses strict parity, the honest outcome is that this defect is documented and left in place, not that some encoding quietly avoids it.

**OB-6 chose D for the original amendment — the versioned second carrier with a dual-carrier migration.** C could not close the actual contract: the ancestry contains absolute paths, so a parent path or invoked base name may contain a colon even when the caller's session component does not. Restricting only the session operand therefore leaves the carrier ambiguous while claiming it is fixed. D was an additive migration for existing consumers and an OB-24 correction for `current` on affected values. OB-1 remains a separate reserved-suffix naming rule; it no longer pretends to solve ancestry encoding.

**Revision 4 completes and retires that migration.** The coordinated cutover finished; the versioned carrier stands alone, and the legacy carrier is neither written nor read (§4.4.1). The analysis above records WHY the carrier exists and why its encoding round-trips every byte; the dual-write contract it once required is no longer part of the normative surface.

#### 4.4.2 Terminal identity **[frozen — compatibility mechanic]**

The holder presents a terminal identity to the child. This is not an implementation detail: programs inside the session enable or disable features based on what they believe the terminal is, and the identity the environment claims MUST agree with the answers capability arbitration gives (§10). A child that reads one identity from the environment and receives a different one from a device-attributes query behaves erratically in ways that are extremely hard to diagnose.

Verified against the reference, on session creation:

**`TERM` is left exactly as inherited.** If the creating process had none, the child has none. The holder never synthesises it, never defaults it, never rewrites it.

The other three are governed by the matrix below. It is a matrix rather than a sentence because an earlier pass of this document stated the rule as prose — "if `TERM_PROGRAM` is set, `TERM_PROGRAM_VERSION` is not set at all" — which was generalised from a case where the version happened to be absent. It is wrong: an inherited version **is** preserved. Prose invited that error; the matrix cannot express it.

| inherited `TERM_PROGRAM` | inherited `TERM_PROGRAM_VERSION` | child sees `TERM_PROGRAM` | child sees `TERM_PROGRAM_VERSION` |
|---|---|---|---|
| set | set | preserved unchanged | **preserved unchanged** |
| set | absent | preserved unchanged | **absent** |
| absent | set | `kitty` | **`0.47.0` — the inherited value is overwritten** |
| absent | absent | `kitty` | `0.47.0` |

| inherited `LC_TERMINAL` | child sees |
|---|---|
| set | preserved unchanged |
| absent | `kitty` |

Two properties of that matrix are load-bearing and easy to lose:

- **The version follows the program variable, not itself.** When `TERM_PROGRAM` is absent the holder is asserting an identity, so it owns the version too and overwrites whatever was inherited. When `TERM_PROGRAM` is present the holder asserts nothing and touches neither. An implementation that gates each variable on its own presence produces a child claiming one terminal at another terminal's version — the single most confusing state for a program doing capability detection.
- **`LC_TERMINAL` is gated independently** of the pair. It is not part of the unit.

Nothing is ever **removed** from the environment by this mechanism.

**This clause has a known cost, and OB-40 decided to pay it in version 1.** Variables that identify the *viewer* — the terminal instance a session is attached to — go stale the moment the session is reattached from somewhere else, and a running process's environment cannot be corrected afterwards. A program inside the session may therefore address a terminal instance that is no longer there. That is a real defect and it is **retained deliberately**, not overlooked: correcting it needs a survey of which variables real terminal emulators set per instance, which is a question about those emulators rather than about this program. It is not on OB-24's list because nothing changes. A version 2 may revisit it.

The identity strings are frozen values of this specification, not a claim about any real terminal, and the synthetic capability replies of §10 MUST report the same identity. An opt-out environment variable disables the synthetic replies; its exact name, the grammar of values that count as "set", and whether it also suppresses the environment injection above are frozen in §10 — they MUST be answered together, because a child that is told it is talking to one terminal and then gets no answer from it is worse off than one told nothing.

### 4.5 Exit

When the child exits, the holder writes exactly one durable lifecycle exit record (§7.4), notifies attached viewers, and then terminates. If the optional event stream is enabled and still writable, it also commits exactly one `exit` event (§8) before closing that stream. A stream already closed by OB-28 or failed storage cannot truthfully accept that event; its prior exhaustion record or event-writable-false status/heartbeat signal is the explicit evidence of the omission.

**This covers only a child whose end the holder observes.** A holder that is killed outright, crashes, or is lost to power failure may write neither record, so a consumer that treats the absence of an exit record as proof the session continues is wrong, and one that treats a closed connection as proof the child exited is also wrong. OB-38 separates the two facts: exactly one lifecycle exit record for an observed child ending, an event-stream exit only while that stream remains writable, and holder loss reported by an external observer without fabricating a child exit.

**The addressable rendezvous object is removed on a clean exit.** Verified against the POSIX reference: after a session whose child ran to completion, no socket remains in the root, a plain `list` prints `(no sessions)`, and only `list -a` shows the session — as `[exited]`. An earlier pass of this document asserted the opposite, that the socket file remains; it does not.

`[exited]` is therefore **a durable exit record with no rendezvous object beside it**, not a rendering of leftover rendezvous state. `[stale]` is the other case: a socket with no listener, left by a holder that died without cleaning up. Both are the **stale** liveness state of §2.3 — absence of a listener is positively established in either — and neither adds a fourth state; they differ in which artefact survived.

This makes discovery a **union of two sources**, and §3.3 must say so rather than describing `list` as an enumeration of sockets: the rendezvous objects present in the root, plus the durable exit records — which may or may not have a rendezvous object beside them. §3.3 carries the full cross-product and deduplication rule; a plain `list` shows entries that have a rendezvous object, and `-a` adds those that have only a record. Which artefact carries the exit record, how long it is retained, and who deletes it are settled with the session log in §7, since the two share a lifetime.

### 4.6 Redirected standard error **[NEW]**

`-2 <path>` sends the child's standard error to a file instead of the pseudo-terminal. Verified against the reference, this option currently fails in three ways at once, and all three are corrected here.

- **A path whose parent directory does not exist is silently ignored.** The session starts, the exit status is **0**, and the child's diagnostics are discarded. The caller believes it has a log.
- **A path that does not exist is not created**, again silently. Same outcome.
- **A path that blocks on open — a FIFO with no reader — hangs the creating process indefinitely.** The rendezvous is published *before* the open completes, so the session is reachable and running while `start` has not returned. Worse, when that session is then killed by another party, the blocked caller wakes and exits **0** reporting that the session started — a success report for a session that no longer exists.

The third is the serious one. It breaks the launcher gate of §3.2 — which requires every caller-supplied sink to be validated and opened *before* the rendezvous is published — and it breaks it in the direction that cannot be defended against: the caller is told everything is fine.

Requirements:

- The **creating process** — not the forked child — opens and validates the sink, **before the rendezvous is published** and before any child is launched. Only the opened descriptor or handle is passed onward; the path is never re-opened later, so there is nothing to swap between check and use.
- The open MUST NOT be able to block. The target MUST be a regular file, opened append-only, without following a symbolic link, and owned by the invoking user with mode exactly `0600`.
- **Any failure is fatal.** The command exits non-zero with a diagnostic (§13) and leaves **no** session behind: no rendezvous object, no holder, no child. A session that runs with its diagnostics going nowhere while reporting success is precisely the failure this document exists to prevent (§0.1).
- Support for pipes, devices, and other non-regular targets is **absent**. If a later pass adds it, it gets an explicit bounded handshake with a deadline and a defined timeout failure — never an implicit blocking open.

**The standard-error file MUST already exist** and is never created. This deliberately contrasts with the event target, which alone may be absent for exclusive directory creation or may be a validated-empty caller directory (§8.1). The standard-error file is **not** constrained to the session root: unlike the event store it is not read back by a supervisor after a restart, so its location carries no addressing requirement, and confining an operator's diagnostic file to a private temporary directory would make the option useless for its main purpose (§11.4).

### 4.7 Launch-time instrumentation **[NEW]**

`-S <path>` loads an instrumentation module into the **initial requested child before its first application instruction**. The object is a shared library loaded through the platform preload mechanism. `-S` proves that one initial process loaded the named module. It is **not** an authorisation boundary, a process-containment boundary, or a promise that every descendant remains instrumented.

Verified against the reference, the control silently does nothing when it fails. A missing shared object, or a regular file that is not a shared object at all, produces exit **0** and a child that runs **without** the library. The dynamic loader's complaint goes to the pseudo-terminal, where the caller — an automated supervisor with no viewer attached — never sees it. The caller believes the child is constrained. It is not.

Instrumentation that fails open, silently, is worse than a stated absence, because the caller cannot distinguish the two.

Requirements:

- **The caller-object contract, frozen:** the object is an existing regular file owned by the invoking user, named by absolute native path, and not reached through a symbolic link. Its permissions are no broader than `0755` with no group/other write. The object architecture MUST match the initial child. The creating process opens and validates it once before publication; no later component receives or reopens the caller's path.
- **Immutable staging is mandatory.** From the validated handle the creator copies the exact bytes into an exclusively created file in the enforced owner-only per-user root. Its basename is `<H>.instrument`, where `H` is 64 lowercase hexadecimal digits encoding SHA-256 over, in order, the wide-length-prefixed canonical session identity, little-endian wire generation, and holder incarnation. The creator flushes the bytes, sets mode `0500`, closes the write handle, reopens the stage without following links, and verifies both its recorded file identity and SHA-256. Only this immutable stage path is placed in a loader variable.
- **The POSIX loader encoding is exact for the stage path.** Linux prepends the immutable stage path to `LD_PRELOAD`, separated from an inherited nonempty value by one ASCII space. macOS prepends it to `DYLD_INSERT_LIBRARIES`, separated from an inherited nonempty value by one colon. The generated stage spelling contains neither platform delimiter nor loader expansion syntax. The resulting loader variable is inherited normally, so ordinary dynamically linked descendants generally load the same staged bytes; set-id, static, loader-scrubbed, or explicitly replaced environments may not. The acknowledgement below proves only the requested initial child. A caller needing descendant coverage requires a separately specified producer/launcher contract and MUST NOT infer it from `-S`.
- **The initial child MUST acknowledge that the module loaded over OB-22's separate private channel**, and `start` MUST NOT report success until it has. The holder creates a one-way byte stream, inherits only its write end into the requested child, selects it with `MOOR_INSTRUMENT_CHANNEL`, and supplies a fresh 16-byte challenge as 32 lowercase hexadecimal digits in `MOOR_INSTRUMENT_NONCE`. The selector grammar is canonical unsigned decimal descriptor text. Both values are private launch material, not authorisation.
- **The module-side ABI is frozen.** A load constructor performs the acknowledgement: it parses the selector and nonce, removes both environment variables before any application instruction can run, writes §15.2 of the companion schema's exact 36-byte acknowledgement, closes the inherited write end, and only then reports success. A later descendant that inherits the preload variable but neither private variable loads the module normally and emits no acknowledgement.
- The holder accepts the acknowledgement only when the record is followed by EOF within 2 seconds and its generation, requested-child PID, and nonce all match the values for this launch. A short or long record, missing EOF, inherited duplicate write handle, malformed selector/nonce, wrong PID/generation/nonce, timeout, or any channel error fails the unpublished launch. Validating the file is not enough: a file can be a well-formed object of the wrong architecture, or be rejected by the loader for reasons no static check predicts. The only trustworthy evidence is from inside the requested initial child after the module's initialization ran.
- Any validation, staging, architecture, loader, insertion, or acknowledgement failure **fails closed**: no session, non-zero exit, and §13.3's exact instrumentation-rejection row with its closed `<cause>`. There is no "run the requested child uninstrumented" path — not as a fallback, not behind a flag, not with a warning.

The staged file remains for the session lifetime so POSIX descendants inheriting the preload variable can load the same bytes. The holder keeps a validated read handle and revalidates its identity after the in-module load acknowledgement. Rollback and `rm` remove it only under the same confirmed-identity fence as the other companions. `.instrument` is part of the reserved-suffix grammar and this substitution is an explicit OB-24 compatibility correction.

### 4.8 Child lookup and the publication boundary

On POSIX a command containing `/` is executed exactly; a command without `/` uses inherited `PATH` with the platform's normal `execvp` search semantics, and `argv[0]` is the command operand exactly as supplied. Any supplied program or later argument containing NUL is rejected. `-d` is applied before lookup so relative executable paths resolve from the requested child directory.

Exec success is the close-on-exec launch pipe reaching EOF. Failure before that point is child-start status 127 and publishes nothing. `attach`, the bare form, and `new`/`n` validate that the caller has the required controlling terminal before starting anything; without one they exit 1, write `<program-name>: no controlling terminal` plus LF to standard error, and leave no child or residue. Headless `start`/`s` and `run` use §4.2's default terminal and 80x24 geometry.

If successful exec is followed by observed child exit before atomic rendezvous publication, Moor commits the lifecycle `exited` record and any enabled final event/log state without ever publishing the rendezvous. `run` returns the child's normal status, or 1 for a POSIX signal. Every background or attaching creator returns 1, writes `<program-name>: child exited before session publication` plus LF to standard error, and emits no created/started message. The residue is discoverable only as `[exited]` under `list -a`. Exit observed after publication follows ordinary session semantics: background creation has succeeded and returns 0, while `run` returns the child outcome. Publication is the sole boundary; scheduler timing cannot select another rule.

---

## 5. Failure modes this implementation must be immune to

Every item below is a defect that occurred in production in the current implementation. They are listed not as history but as requirements: the new program MUST be constructed so that each is impossible by design, and each has an observable test. A submission that reproduces any of them has failed conformance regardless of how well it performs elsewhere.

These are the concrete meaning of "robust" in this project. They cluster into three lessons, and the lessons matter more than the individual bugs, because the next defect will be a new member of one of these families.

### 5.1 A stranger must not be able to end a session

**Nothing a peer does — connecting, disconnecting, stalling, or flooding — may terminate the session or its child.** The child is the user's work. Losing it is the worst outcome the program has, worse than refusing a viewer, worse than dropping output, worse than exiting with an error.

- **[NEW] A slow reader MUST NOT kill the session.** When a viewer cannot accept output as fast as the child produces it, the program MUST apply backpressure — buffer up to a bounded, documented limit, then disconnect *that viewer* — and MUST NOT treat a would-block condition on one connection as a fatal condition for the process. *Failure prevented:* the current implementation exits the whole holder when a write to a non-blocking peer would block, so a single momentarily-busy consumer destroys the child and the user's unsaved work under nothing more unusual than a burst of output.
- **A peer that connects and disconnects without speaking MUST be released completely.** No descriptor, buffer, or table entry may outlive it. *Failure prevented:* accepted descriptors accumulated from probes that closed early, because a closed socket reads as ready forever; the holder eventually exhausted its descriptor table and became unable to accept anyone.
- **A peer that sends a partial message MUST NOT be dropped for slowness alone.** Message boundaries are the program's responsibility to reassemble across reads. *Failure prevented:* a large control message that arrived split across reads was rejected as malformed and the legitimate client was disconnected under load.
- **Connection admission is bounded per holder:** at most 16 accepted sockets may be awaiting their four-byte protocol preface/initial hello, at most 64 authenticated controller connections may exist, and at most 64 authenticated semantic connections may exist. The 17th/65th peer is closed or receives `RESOURCE_EXHAUSTED` when enough protocol state exists to encode it; no existing peer or child is disturbed. Aggregate in-progress reassembly storage is 64 MiB; only the connection whose next fragment would exceed it is refused. Semantic source-id state is capped at 64 distinct ids per holder incarnation, including disconnected ids whose fixed mode/epoch history must remain; a 65th id receives pre-assignment `SEM_RESOURCE_EXHAUSTED`. Reconnecting an existing id does not consume another id slot. These caps bound descriptors, deadlines, reassembly and per-source deduplication tables together rather than bounding each object in isolation.

### 5.2 State must not leak between messages, sessions, or restarts

- **Reassembly state MUST be reset on every completion, successful or not.** After a message is delivered, no byte of it may appear in the next one. *Failure prevented:* the accumulated length survived a successful reassembly, so the following fragmented message was silently concatenated with the previous one's bytes.
- **A viewer that attaches MUST be told the terminal state the child established**, not merely granted a connection. A viewer that starts fresh has witnessed nothing: modes the child enabled before it arrived are invisible to it, and it will render the session wrongly until the child happens to set them again. *Failure prevented:* a restarted supervisor rebuilt its emulator with no knowledge of the child's active modes.
- **[NEW] Recovery state MUST be bounded and MUST distinguish "already seen" from "new".** A restart may not re-deliver the entire history as though it had just happened, and the record it reads from may not grow without limit. *Failure prevented:* recovery cost grew with total session age, and every restart republished the whole event history.

### 5.3 Parsing hostile bytes must be total

The child's output is not trusted input. It is arbitrary bytes chosen by whatever the user ran, which may be a program acting on data from anywhere.

- **Every numeric field parsed out of the byte stream MUST have a defined result for every input.** Values beyond the representable range MUST be rejected, not wrapped. *Failure prevented:* unchecked decimal accumulation in mode and command parsing overflowed, which is undefined behaviour in the current implementation's language and allowed attacker-influenced output to be interpreted as a different, valid command.
- **A control sequence split across read boundaries MUST be recognised.** The parser retains partial sequences and resumes; it does not discard the tail of a read. *Failure prevented:* titles, links, and readiness signals that happened to straddle a boundary were invisible, so state derived from them was silently wrong rather than absent — the worst class of error for a supervisor that trusts the signal.
- The parser MUST be bounded in memory for any input, including a sequence that never terminates.

### 5.4 The portability rule

**Platform-specific facilities MUST be reached through an abstraction with an implementation per supported platform, and an unsupported platform MUST fail to build rather than silently omit the check.** *Failure prevented:* the peer-identity check was written against one operating system's interface, so the program could not be built for the other at all — and the tempting repair, compiling it out, would have silently removed an authorisation check.

### 5.5 A passing test proves nothing about code the program never reaches

This is a rule about evidence, and it is in this section because it is how the other four failures survived review.

The current implementation contains several security and durability facilities that are **fully implemented and have passing tests, but are not reached by the shipped program**: the peer-identity check, the normalisation of inherited signal state, the closing of inherited descriptors, and its durable generation/journal machinery. Each has a green test. None of them runs when a user starts a session. The test suite reports a program that is safe; the binary on disk is a program that is not.

This is a worse condition than an untested feature, because an untested feature is visibly untested. Here the evidence actively misleads: a reviewer checking whether peer identity is verified finds a correct implementation and a passing test, and stops.

**Every requirement in this document is a requirement about the shipped binary.** Conformance MUST be demonstrated by driving that binary through its real interfaces — creating sessions, connecting through the platform rendezvous, restarting supervisors while holders survive — and observing the result from outside. A unit test that exercises a component in isolation is welcome as a development aid and is **not** admissible as conformance evidence for any requirement here.

Specifically, and as a floor rather than a complete list, these MUST be shown against a running session:

- The identity of a connecting peer is checked on **every** accepted connection, **before** any byte from it is parsed. A connection from another user consumes nothing: no capability, no lease, no generation, no state, no buffer.
- Inherited signal dispositions and the inherited signal mask are reset, so a child does not start with a signal the holder's parent happened to block or ignore.
- Every descriptor not explicitly required is closed before the child starts, so nothing the creating process held leaks into the child.
- Generation fencing, event-stream commit recovery, and bounded raw-output replay survive a real restart: create a session, attach, **restart the supervisor while the holder keeps running**, and observe that adoption re-establishes identity, reports replay exactness honestly, and refuses a superseded generation. Wire v4 has no screen checkpoint (§6.7). Restarting the *holder* is not required and is not achievable — it owns the pseudo-terminal and no successor can reopen it by name (OB-26).

A submission whose evidence for any of the above is a component test has not met this section, regardless of the component's quality.

## 6. Attaching, detaching, and redraw

### 6.1 Multiple viewers

A session accepts more than one viewer at once. All fully attached viewers receive the same output. Exactly one controller at a time holds the **input lease**: only a lease-holding viewer may send viewer input or change geometry, while `push` holds the same lease in an input-only role. The holder starts with allocated epoch `0`; each fresh grant allocates the previous value plus one, while resume, release, and expiry preserve the allocated value. Epoch `FFFFFFFF` may be granted once and never wraps. A transport loss reserves its lease until the original responsiveness deadline so the same user can resume with the exact freshness tuple; §10.2.14 freezes every transition.

`ATTACH` is the only operation that creates a viewer. A connection becomes **fully attached** atomically when the holder freezes its replay descriptor and changes its phase before queuing the preamble; status and `list` count it from that instant. An input-only `push` connection and a probe never count as viewers. Attach output order is revision 4's status-first sequence: `ATTACH_ACK`, terminal-state preamble, `LEASE_RESULT` when requested, frozen replay, then live output. The requested attach geometry is applied to the native terminal BEFORE the descriptor is built; a native refusal fails the attach closed — the holder closes the link. That failing attach is one atomic transaction FOR THE LEASE, committing on the successful enqueue of the token-bearing lease result: any earlier failure — deadline, native resize, or any prefix frame that cannot enqueue, the token frame itself included — rolls a fresh grant back entirely, reservation and epoch allocation both. The token never left the holder, so the epoch was never consumed and the next fresh controller receives that very number; a resumed viewer's failure instead preserves its known epoch/token reservation exactly as ordinary link loss does. The native geometry is the honest exception to that unwinding: once the native resize has succeeded, a LATER prefix failure may leave the applied geometry standing — a native effect may already be visible and is never guessed back with a compensating resize — and the next status descriptor simply reports the geometry actually in force. A `NON_VT` viewer may own input and geometry but receives an empty preamble, is never a query delegate, and receives none of the viewer-generated controls below; raw child output remains unchanged.

*Why a lease and not free-for-all:* several viewers of one session would otherwise fight over the child's window size, and a late reply from a departed viewer would be injected as input.

### 6.2 The detach key

A configured control character begins a detach attempt without disturbing the child. Its first occurrence is consumed by the viewer and does not reach the child; only the doubled-byte escape below sends one copy onward. The default is the character conventionally written `^\`; `-e` sets another and `-E` disables detection entirely, in which case no keystroke detaches.

Detach doubling uses 250 ms measured monotonically. The first configured detach byte is consumed and arms the timer. If the next byte before expiry is the same byte, the arm is cancelled and exactly one detach byte is sent to the child. A different next byte is sent unchanged and then detach completes; expiry or input EOF also completes detach. `-E` bypasses this state machine completely.

Only key-up and modifier-only events are transparent metadata: they are forwarded unchanged, never match the detach byte, and never count as "the next byte" for doubling, so releasing the key or pressing a modifier cannot cancel an armed detach. Every other key-down is an atomic occurrence, and a key-down that carries no character — an arrow, a function key — is exactly the *different* next occurrence: while armed, its whole carrier is forwarded and detach completes. A NUL character on a key-down is a real character, distinct from a key-up. A repeat count of `N` is `N` occurrences processed in order: each completed detach pair forwards one equivalent canonical carrier with a repeat count of `1`, never the original `Rc`, and an odd remainder leaves the doubling timer armed. The first consumed detach occurrence removes its whole carrier sequence from the forwarded stream. With `-E` there is no active detach byte, nothing on this path is recognized or consumed, and every byte — carrier or not — passes unchanged.

Configured detach-byte matching has priority over the fixed suspend-byte handling and over `-z` pass-through. Consequently, when `-e ^Z` selects byte `1A`, that byte arms detach doubling whether or not `-z` is present; it neither suspends the viewer nor passes through on its first occurrence. The suspend rule in §6.5 sees byte `1A` only when it is not the active detach byte. When `-E` wins option precedence there is no active detach byte, so §6.5 applies normally.

Detaching leaves the child running and its terminal settings untouched.

### 6.3 Redraw on attach

`-r` selects how the child is prompted to repaint: `none` sends nothing; `ctrl_l` sends exactly byte `0C`; `winch` sends one `RESIZE` even when the validated geometry is unchanged. Same-size `winch` is the sole exception to §4.3's change-only resize rule.

**This is one of only two sequences the holder may put into the child's input stream** (§1.1), and it happens only when the operator has selected it. `none` is the default for an automated attach, because a supervisor inspecting a session must not make the child redraw.

Redraw occurs only if the connection owns the lease after attach; a busy observer never prompts the child. The holder queues the non-interleaved ACK/terminal-state/optional-lease-result prefix and complete frozen replay, then performs the selected `ctrl_l` write or same-size `winch`. Output caused by the prompt is live output ordered after the baseline.

### 6.4 Clearing the viewer's screen

`-R` selects what the viewer's screen is doing when the session appears: `none` leaves it, while `move` emits exactly bytes `1B 5B 48` locally after `TERMINAL_STATE` and before replay. These bytes go **to the viewer only** and never to the child, and like the preamble (§1.1) they must not be logged or advance any output cursor. `move` with `-t` is an invalid option combination.

### 6.5 Suspend

Suspend is local viewer byte `1A`; unless `-z` is set it suspends only the viewer process and is not sent on the controller connection. With `-z` it passes through to the child instead. A suspended viewer is still attached; the session and its child continue.

### 6.6 When a viewer disappears

A viewer whose terminal vanishes — the connection drops, the process dies, the window closes — is detached immediately. **The session is unaffected** (§5.1). No output is lost from the session's point of view; the departed viewer simply stops receiving it. If it held the input lease, that lease is reserved only for the remainder of its original ten-second responsiveness deadline; a timely exact resume preserves the epoch and rotates its token, otherwise expiry releases it without incrementing the allocated epoch.

### 6.7 Bounded raw-output replay

Moor retains raw child output so a new viewer can establish a bounded baseline without asking the child to repeat its whole history. This is byte retention, not a screen model or terminal checkpoint:

- Each `OUTPUT` record carries at most 64 KiB of child bytes. The holder retains the newest complete records whose payload bytes total at most **4 MiB**. When the next complete record crosses the bound, whole oldest records are discarded until the bound holds; a record is never retained partially.
- On every successful attach, the holder freezes the retained first/last descriptor placed in `ATTACH_ACK`, sends a `GAP` for `1..first-1` when a prefix was discarded, then sends every retained `OUTPUT` record in sequence before later live records on that connection. Output arriving during the baseline is ordered behind the frozen baseline. A controller that already consumed some records discards duplicates by record sequence; a fresh emulator applies the whole retained run.
- Empty history has no `GAP` and no `OUTPUT`. A retained run beginning at record 1/byte 0 is complete raw history for this holder incarnation. Any later start is explicitly degraded: an exact modes preamble can restore the tracked modes, but Moor does not claim that a suffix reconstructs the screen. If tracked-mode exactness was lost, the mandatory preamble is empty rather than asserting guessed state.
- Replay uses the same per-viewer 4 MiB child-payload backpressure bound as live output. The frozen baseline may pin immutable retained-record buffers for that viewer, and those bytes count against its bound; global eviction therefore cannot mutate an in-flight baseline and no second unbounded copy exists. A viewer that cannot drain the baseline before later live bytes cross its bound is disconnected without affecting the child.

There is **no checkpoint carrier in wire v4** and no main/alternate-screen exactness claim. Earlier status fields advertising those facts had no frame capable of carrying a checkpoint and would have invited two incompatible implementations. The only exactness facts are whether raw history starts at byte zero and whether the bounded terminal-mode scanner still knows its tracked state.

## 7. The session log

### 7.1 What is written

The child's output, as bytes, in order. Nothing else: not input, not the preamble (§1.1), not diagnostics, not arbitration replies. The log is what the child produced, so that `tail` shows what the child said.

### 7.2 The cap

`-C` bounds the log; `0` disables it entirely. The default is one mebibyte.

Reaching the cap must not stop the session and must not lose the newest output — those are the two failures worth naming, and they are the ones a naive implementation picks. **The retention policy at the cap is frozen in a single decision** and applies uniformly: after each child-output write, the selected committed log is exactly the newest `min(cap, previous-retained-bytes + new-bytes)` bytes. The oldest prefix is discarded, including the oldest prefix of one write when that write alone is larger than the cap; the newest `cap` bytes of that write remain. The selected body prefix never exceeds the cap. A reader that was following is told the exact byte range containing its position is no longer present rather than silently resuming somewhere else. That last clause is the same rule as §8.4.4's gap reporting, for the same reason.

Log positions use the same zero-based absolute child-output byte coordinates as controller `OUTPUT`; rotation never renumbers them. If total child output is `E` bytes and the log retains `R`, the retained log represents `[E-R,E)`. This coordinate is behavioral state even though the file itself contains only the raw `R` child bytes (§7.1).

### 7.3 `tail` and `clear`

`tail` prints the last *N* lines and, with `-f`, continues as the log grows. It follows selected commit indexes, not file offsets. **A follower survives the log reaching its cap**: it is told about the discontinuity and continues, rather than stopping or silently jumping. If its next absolute byte is `F` and rotation advances the retained start to `R > F`, it writes exactly `<program-name>: log gap: child-output bytes [<F>,<R>) were discarded` plus LF to standard error, then resumes at `R`. The two numbers are canonical unsigned decimal u64 values. Several rotations observed together coalesce into that one maximal half-open range. This is a diagnostic and `-q` does not suppress it. The reported coordinates and resumed bytes must match the holder's absolute child-output positions.

A reader is **not** guaranteed complete lines. The log is a byte stream from a program that may emit anything, including a final fragment; `tail` does not wait for a newline that may never come.

`clear` selects an empty body whose committed range is `[E,E)`, where `E` is the assigned child-output end at the linearization barrier. A live clear uses §10.2.15; a confirmed-stale clear commits directly only while holding §11.6's writer lease. An indeterminate possible writer is refusal without mutation. No clear truncates the currently selected body in place.

Conformance parks a follower before the retained start, crosses the cap with several writes and with one write larger than the cap, and byte-compares the one coalesced gap diagnostic, the resumed suffix, and the unchanged child/session lifetime.

### 7.4 The lifecycle store **[OB-9]**

The lifecycle companion is a holder-created portable committed directory using §8.4.2. Its selected body is at most 4 MiB and contains exactly one canonical JSON object plus LF. The index-1 `running` record is committed before rendezvous publication. Its closed key set occurs in exactly this order: `v`, `type`, `phase`, `session`, `generation`, `wire_generation`, `incarnation`, `start_wall_ms`, `start_mono_ms`, `boot_id`, `path_encoding`, `event_path`, `instrument_path`. Values are respectively `2`, `"lifecycle"`, `"running"`, canonical padded-base64 identity, allocated u32 or `null`, wire u32, padded-base64 16 bytes, canonical decimal-string u64, canonical decimal-string u64, padded-base64 16 bytes, `"posix-bytes"`, and padded base64 of each exact native encoding or `null`. `instrument_path` encodes §4.7's immutable staged path, never the caller's operand. This record is the cleanup manifest after holder loss; it is not rendered `[exited]`.

The index-2 `exited` replacement changes `phase` to `"exited"`, retains every common value, and appends keys `end_wall_ms`, `output_end`, `ended`, then the branch keys below. The new u64 values are canonical decimal strings. Outcome branches match event schema 2 exactly:

- POSIX normal exit is `ended:"exited",code:<u8>`;
- POSIX signal is `ended:"signalled",signal:<positive platform signal number>` with no `code`;
- and every closed record additionally carries `method:"none"|"graceful"|"forced"` — the holder's own termination intent, orthogonal to the mechanism above. `none` states the holder had no termination state; it never claims who outside the holder caused a signal. Revision 4 removed the folded `ended:"terminated"` branch: mechanism and intent are separate axes on every platform.

The foreground shell status for a POSIX signal remains 1 and is not stored as the child code. All u64 values that can exceed JSON's exact integer range are canonical decimal strings; identities and opaque byte values use canonical padded base64.

At observed child end the holder freezes the outcome and final output coordinate. Exit ordering is exactly:

1. replace and commit the lifecycle store with phase `exited`;
2. commit the event-stream `exit` transaction when the event store remains writable;
3. remove the rendezvous object;
4. close the stores and terminate.

An already-dispatched log job receives only its existing deadline. Lifecycle exit and event exit each receive at most two seconds; total storage waiting is therefore at most six seconds inside §12.3's ten-second whole-shutdown bound. If lifecycle exit fails or misses its deadline, the holder reports storage failure, leaves the rendezvous and `running` manifest as stale evidence, and exits by the whole-shutdown deadline; it never unlinks into an undiscoverable state. Only an `exited` record without a rendezvous renders `[exited]`.

The lifecycle record and log are retained until `rm` or stale replacement removes them under the identity fence. They are not aged out. A successor generation never adopts their bodies or commits.

## 8. The event stream

This is how the supervisor learns what is happening inside a session without attaching to it or parsing its screen. It is the most important interface in this document after §1.1, and the one whose failure is hardest to notice: a supervisor that receives no events shows a confidently wrong picture rather than an obviously broken one.

### 8.1 Enabling and the path contract

The stream is named by `-T <path>` (§3.5). The operand MUST be an absolute native path. Moor validates and retains the original native operand without converting it to another spelling. A non-absolute operand is rejected before any cleanup, store mutation, child creation, or rendezvous publication using §13.3's event-target row with cause `not-absolute`; the `<path>` displayed there is the original operand under OB-29 rendering. The accepted `<path>` names a directory containing exactly four regular, non-link slots named `body.0`, `body.1`, `commit.0`, and `commit.1`. The status descriptor's event-layout value `02` means this portable four-slot committed directory. Value `01` is the superseded POSIX single-file layout and is never emitted by an amended holder; disabled storage is `00`.

The event directory may be absent for Moor to create exclusively after confirmed-stale cleanup, or may be the exact validated empty directory object handed off by this caller. A present directory must be inside the session root, owner-only (`0700`), opened without following a symbolic link, and contain no entry. Moor creates all four slots exclusively against that opened directory; an extra entry or pre-existing slot is refusal, never adoption. Slots use mode `0600`. The lifecycle launch record retains the event directory's exact native path for cleanup.

A session that runs without its requested event stream looks healthy while being invisible, so every validation or initialization failure is fatal and occurs before rendezvous publication. Publication begins only after `body.0` and `commit.0` form §8.4.2's durable valid initial commit. A successor generation never adopts any slot, body prefix, or commit from an older generation.

### 8.2 Format

One JSON object per line, UTF-8, newline-terminated, appended in observation order. The first line of the file is the **header** record defined in §8.4.1; every line after it is an **event** record.

The two record classes do not share a field set, and an earlier pass of this document said every object carried `seq` and `epoch` while §8.4.1 forbade `seq` on the header. The split is:

- **Common to both:** `type`, and `ts` — seconds since the Unix epoch as a JSON number with millisecond precision.
- **Header only:** `v`, `session`, `generation`, `epoch`, `next_seq`, `first_retained` (§8.4.1).
- **Event only:** `epoch`, `seq`, `kind`, and the per-type fields below.

Ten event types exist — four derived from terminal observation, three carrying semantic-producer provenance, one reporting missing application evidence, one describing the stream, and one describing the child exit:

| type | additional fields | meaning |
|---|---|---|
| `ready` | — | a terminal capability query was observed **being emitted by the child**, once per session. This records only that signal. It is not evidence that the child is interactive, healthy, or trustworthy: any program can emit the sequence, so no consumer may treat `ready` as proof of anything beyond "this byte sequence was seen" |
| `state` | `state`, `title`, `truncated` | the child's activity classification changed (§9); `title` carries the observed title **bounded and encoded per §9.4** — never verbatim. A title is arbitrary bytes chosen by whatever the user ran, and this line must remain well-formed UTF-8 JSON |
| `link` | `uri`, `truncated` | the child emitted a hyperlink. `truncated` is `true` when the value was shortened by the bounds of §9.4, so a consumer never treats a shortened target as complete |
| `observer-degraded` | `scanner`, `reason` | a transition-only, never-snapshotted report of the first scanner abandonment in a degradation episode (§9.4). `scanner` is `"osc"` or `"query"`; `reason` is `"cancelled"`, `"malformed"`, `"limit"`, or `"deadline"` |
| `semantic-source` | `source`, `producer`, `source_epoch`, `status`, `reason` | a **stateful** source connection changed state. `status` is `"connected"`, `"exact"`, `"degraded"`, or `"disconnected"`. `connected`/`exact` require `reason:""`; `degraded` requires `"heartbeat-timeout"`; `disconnected` requires one of `"transport-closed"`, `"superseded"`, or `"session-ending"`. No other pairing is legal. Edge-source connect and disconnect produce no `semantic-source` record because an edge connection claims no continuing state. A lost stateful source makes its evidence degraded; it never means the application became idle. An event-sink failure cannot durably append `stream-unwritable`; status/heartbeat carries that condition instead |
| `semantic-assertion` | `source`, `producer`, `source_epoch`, `source_seq`, `event_id`, `assertion_kind`, `payload` | an authenticated producer assertion accepted through §10.3. `assertion_kind` is `"transition"` or `"snapshot"` and preserves the producer-wire assertion kind; `"snapshot"` is legal only for a stateful source, while an edge source may publish only `"transition"`. The common event `kind` independently says whether this JSON line is a newly published `transition` or a compaction `snapshot`. The 16-byte `producer` and `event_id` values are canonical padded base64. `payload` is canonical padded base64 of the producer's exact validated UTF-8 JSON object; Moor preserves it and does not interpret provider keys |
| `application-receipt` | `source`, `producer`, `source_epoch`, `source_seq`, `event_id`, `application_request_id`, `lease_epoch`, `request_id`, `status`, `provider_session`, `provider_turn` | a producer asserted an application outcome correlated to one written `INPUT`. `status` is `"accepted"` or `"refused"`. Identifier and provider fields use canonical padded base64. `provider_session` and `provider_turn` may encode zero bytes when the producer has no such identifier; the keys remain present and the empty canonical-base64 spelling is `""`. This is evidence **from the named producer**, not evidence that Moor independently observed application behavior |
| `application-receipt-missing` | `source`, `producer`, `source_epoch`, `application_request_id`, `lease_epoch`, `request_id`, `reason` | Moor had no correlated producer receipt at a defined diagnostic point. The producer and source epoch are those selected before the terminal write. `reason` is `"deadline"`, `"source-lost"`, or `"retention-expired"`. This record is explicitly absence of evidence, never a refusal or application outcome; a later valid receipt may still follow a deadline/source-loss record while the correlation remains retained |
| `stream-exhausted` | `axis` | **[NEW, OB-28]** the stream cannot durably admit the requested operation on the named axis and is closed. `axis` is `"seq"`, `"epoch"`, or `"commit"`; `kind` is always `"transition"`. The exact portable final-allocation algorithm is in §8.4.1 and OB-28. In particular, a sequence-exhaustion record consumes the one sequence position kept in reserve and may therefore carry a value below the numeric maximum when a multi-record transaction no longer fits. The session continues; status/heartbeat reports that the stream is no longer writable |
| `exit` | `ended`, its branch field, and `method` | the child ended. `ended:"exited"` carries `code`; on POSIX `ended:"signalled"` carries `signal`. Every record carries the mandatory `method:"none"\|"graceful"\|"forced"` — the holder's termination intent, orthogonal to the mechanism, `none` when the holder had none. Any fields from another branch make the record malformed |

**Event schema version 2 has closed key sets.** The header keys are exactly those in §8.4.1. Every event has exactly the common event fields plus the additional fields in the table; no other key is legal at `v:2`. A duplicate key is malformed even when both occurrences have the same value; a reader must detect it rather than let a convenience parser silently keep one occurrence. `observer-degraded`, `application-receipt`, `application-receipt-missing`, `stream-exhausted`, and `exit` are occurrence-only and require `kind:"transition"`; a committed `kind:"snapshot"` on any of those types is malformed. The other types may carry `kind:"snapshot"` only when §8.4.4 requires that compaction restatement. All base64 is standard padded canonical base64. `source_epoch` and `lease_epoch` are JSON numbers in the u32 range. `source_seq` and `request_id` are **JSON strings containing canonical unsigned decimal u64 values**: either `"0"` or `[1-9][0-9]*`, no sign, whitespace or leading zero, and within `0`–`18446744073709551615`; their owning wire rules require them to be nonzero in these events. They are strings because an ordinary JSON-number parser cannot preserve their full u64 identity above 2⁵³−1. The event stream's own `seq` remains the bounded JSON number defined in §8.4.1. The exit code is a JSON number 0–255.

One canonical serializer applies to event, lifecycle, and delivery-control JSON. Object members occur in each schema's stated order with no insignificant whitespace. Output is UTF-8 and emits non-ASCII scalar values directly. Quote and backslash escape as `\"` and `\\`; backspace, tab, LF, form feed, and carriage return as `\b`, `\t`, `\n`, `\f`, and `\r`; every other U+0000..U+001F scalar as `\u00XX` with uppercase hexadecimal. Slash and U+2028/U+2029 are not escaped. Booleans are exactly `true` and `false`. Only Unicode scalar-value strings reach the serializer; title/link normalization replaces malformed UTF-8 and NUL before serialization, while native identities remain padded-base64 carriers. Arrays preserve semantic order and duplicate members are impossible.

Header key order is `v,type,ts,session,generation,epoch,next_seq,first_retained`. Every event begins `type,ts,epoch,seq,kind`, followed by the table's additional fields from left to right; `observer-degraded` therefore appends `scanner,reason`. Branch-only exit keys follow `ended` in §7.4's branch order. The header timestamp is the original stream-creation time and survives compaction. A snapshot retains the timestamp of the transition whose knowledge it restates; every newly published transition in an ordered transaction uses its own observation time. No map iteration order is normative input.

A reader MUST be able to parse the file with a line-oriented JSON reader and no knowledge of this program. A partially written final line MUST be tolerated by readers and MUST NOT be produced across a restart: see §8.4.

### 8.3 Emission rule

A `state` event is emitted when the classification **changes**, not on every title the child sets. A child that repaints the same idle title fifty times produces one event. This is a requirement, not an optimisation: the supervisor treats each event as a transition and would otherwise see a storm of identical transitions.

### 8.4 Bounding, compaction, and recovery **[NEW]**

The stream MUST NOT grow without limit, and a restarting supervisor MUST NOT be made to re-read the whole history. Compaction is a complete inactive-body replacement selected only by §8.4.2's portable commit record. A byte offset or open handle alone cannot identify a selected prefix, so readers carry §8.4.3's commit identity and sequence. This is the complete crash/recovery protocol; no platform-specific sidecar or replacement rule supplements it.

#### 8.4.1 Records, and the two kinds of them

The selected body is a sequence of **records**, one JSON object per line. A record is either the header or an event; the **ten** event types of §8.2 are event types and do not include the header.

An earlier pass of this document required the header's `seq` to be "the sequence number of the record that follows it" *and* required every record to carry a `seq` increasing by exactly one. Those two rules together say `h = h + 1`, which nothing satisfies, and for an empty stream the record that follows does not exist. **The header is therefore not a sequenced record.** It is the file's preamble, it carries no `seq` of its own, and it names where the body begins.

- **The header is the first line of every committed body**, exactly once, with `"v":2` and `"type":"header"`. Its keys are exactly: `v`, `type`, `ts`, `session` (§8.4.1.1), `generation` (§10.1, or JSON `null` for a session that has none), `epoch`, `next_seq`, and `first_retained` (§8.4.4). `next_seq` is the exclusive next unallocated event sequence and equals the commit end coordinate. When events are retained they are dense from `first_retained` through `next_seq-1`; an empty event run has equality. No other key may appear at schema version 2; a reader encountering one MUST stop rather than guess. Adding or changing a key is a `v` increment, not an extension.
- **Every *event* record carries `epoch` and `seq`**, and `seq` increases by exactly one from one event record to the next. It does **not** reset when the epoch changes: the epoch says which physical body you are reading, the sequence says how much of the stream you have consumed. An empty stream is a header with `next_seq` equal to `first_retained`, and no further lines.
- **`ts`** is seconds since the Unix epoch as a nonnegative JSON number derived from an unsigned 64-bit millisecond count, on the header and on every event record. Its canonical spelling is the decimal whole-second quotient with no leading zeros, followed, when the millisecond remainder is nonzero, by `.` and exactly three decimal digits. Thus the field has a finite maximum spelling of 21 bytes, and admission calculations in §8.4.4 reserve that maximum rather than assuming today's clock width. A negative, exponent-form, over-range, or non-canonical spelling is malformed.
- **`epoch`** is an unsigned 32-bit integer starting at `0` for a new stream and increasing by exactly one per compaction. **`seq`** is an unsigned integer starting at `0` and bounded above by **2⁵³−1**. Neither wraps.

  **Their exhaustion is not the same condition as generation exhaustion, and an earlier pass of this document delegated it there wrongly.** A generation is exhausted *before* anything is launched: nothing is created, the command exits non-zero, and no state exists to clean up (§10.1). A sequence or epoch is exhausted with a **live child, a published rendezvous, and an attached supervisor** — long after `start` returned zero. "Handled the same way" is not available: there is no command left to fail.

  **Final allocation is preflighted as one transaction.** Before serialising any event record, the writer calculates the complete ordered transition set the operation requires under §8.4.1.3. The complete record set is those transitions without compaction, or the snapshot baseline followed by every one of those transitions when compaction is required. Sequence positions, including those consumed by snapshots, may be allocated only when that whole record set fits and at least one further sequence position remains available for a final diagnostic. No prefix is allocated, accepted, or published on its own.

  - If that complete record set will not fit while preserving the diagnostic position, none of the operation's ordered transition records is allocated or published and no snapshot is emitted. A rejectable operation is refused as a whole; a mandatory holder-observed fact may still change live non-event state but gains no partial durable representation. The writer appends exactly one `stream-exhausted{axis:"seq"}` at the current next sequence directly to the current body, without compaction, commits it, and permanently marks the stream unwritable. Because a compaction transaction can need several sequence values, this final record can legitimately have a `seq` below 2⁵³−1; every larger value remains unused. A semantic producer receives `SEM_RESOURCE_EXHAUSTED`, never a false durable ACK.
  - If appending the complete ordered transition set would require compaction into an epoch after `4294967295`, sequence capacity is checked first. When it suffices, the writer appends every transition in the set, in order, followed by `stream-exhausted{axis:"epoch"}` in epoch `4294967295`, without another compaction. It commits that whole final transaction and closes the stream. Every admitted transition therefore survives exactly once; no prefix may commit alone.
  - On every platform, if the next storage commit is `FFFFFFFFFFFFFFFF`, sequence and epoch checks run first. When neither is limiting, the writer includes the complete ordered transition set followed by `stream-exhausted{axis:"commit"}` and publishes that whole transaction with the final commit index. When those transitions require compaction, the same update contains the complete snapshot baseline before them. The final commit is never used for an ordinary transaction that would leave the stream apparently writable, and no prefix may commit alone.

  The limiting-axis precedence is therefore `seq`, then `epoch`, then portable `commit`. A final transaction may exceed the 256 KiB cap by exactly its required snapshot baseline, complete ordered transition set, and exhaustion diagnostic; no later event is admitted. Recovery treats a committed `stream-exhausted` as permanent stream closure. If storage I/O itself fails before the diagnostic commits, the writer cannot fabricate durability: it closes semantic ingress and reports event-writable false through status/heartbeat instead.

  The bound on `seq` is not arbitrary. A 64-bit sequence written as a JSON number cannot be read back exactly by a conforming JSON parser above 2⁵³−1 — the value silently becomes a nearby one, which for a cursor means silently resuming at the wrong record. Capping at the largest exactly-representable integer keeps every standard reader correct without requiring a special parser. At the reference's observed event rate this bound is not reachable in any real session; it exists so that the failure, if ever approached, is a defined terminal condition rather than a rounding error.

##### 8.4.1.1 The `session` field

§2.1 makes names opaque native path values and defines two names as the same session **if and only if their tagged canonical identities match**. A JSON string cannot directly carry that binary identity, so the header needs an encoding, and it needs to say *which* bytes it encodes — an earlier pass of this document said "the session name as a string" in one paragraph and "base64 of the raw bytes" seven lines later, which are two different fields wearing one name.

- The value is the **tagged canonical session identity** frozen by **OB-17**. It is tag `01` followed by the socket's absolute path with `.` and `..` resolved lexically, **without** following symbolic links, canonicalised once before publication. It is never the command-line spelling.
- Those tagged identity bytes are encoded with **standard base64 including padding**, over the alphabet `A`–`Z`, `a`–`z`, `0`–`9`, `+`, `/`. Line breaks are never inserted. A decoder MUST reject non-canonical input rather than accept it leniently.
- There is **no companion display field**. A reader that wants something human-readable decodes and renders it under its own rules; a second field would be a second identity, and identities that can disagree eventually do.

**These are design choices, not measurements.** The reference has no header record at all, so nothing above was observed. The choices this document is making are: **that the identity is whatever OB-17 defines rather than the argv spelling**, padded canonical base64, the closed key set, and stopping on an unknown key. Each is justified where it is stated; none is a reported fact. There is no longer a "resolved path as identity" choice — an earlier pass made one, and this subsection withdrew it in favour of OB-17.

**Both the encoding and the input bytes are now frozen.** The kind tag keeps the identity kind explicit so a future kind can never be confused with path bytes. The same tagged live identity keys the header, acknowledgements and destructive fence, which is what makes those surfaces comparable. The supervisor's durable generation allocator deliberately uses the separate logical session key defined by §10.1.2, which exists before launch and survives republication.

##### 8.4.1.2 Snapshots and transitions

- **Every event record carries a `kind`: `transition` or `snapshot`.** A `transition` records something that just happened. A `snapshot` restates knowledge that was already published, so that a reader arriving after compaction is not blind to it.

  **A consumer MUST NOT treat a `snapshot` as an occurrence.** Without this, compaction republishes `ready` as a second readiness, and the latest `link` as a hyperlink the child just emitted — the exact "restart replays history as current" defect §8.4 exists to prevent. Distinguishing by type alone cannot work: a snapshot of a `link` *is* a `link`.
- `semantic-assertion.assertion_kind` is a different axis. A newly accepted producer message always has event `kind:"transition"`, including when its `assertion_kind` is `"snapshot"`; compaction may later restate the latest exact stateful assertion as event `kind:"snapshot"` while preserving `assertion_kind:"snapshot"`. The first is a publication occurrence, the second a storage resync. Conflating them makes a producer assertion in the transaction that causes compaction change semantic meaning.
- **Every transition in the transaction that causes compaction appears exactly once**, in its defined order after the snapshots. No transition from that transaction is also represented among the snapshots, and no prefix of the ordered set may be selected alone.

##### 8.4.1.3 Whole-transaction admission

Admission operates on one transaction containing zero or more snapshots followed by one or more ordered transitions. The writer serializes and preflights the complete transaction before changing in-memory or durable state. Required multi-transition transactions include:

- stateful-source replacement: old `disconnected/superseded`, then every newly due `application-receipt-missing{reason:"source-lost"}` bound to that old producer in original completed-PTY-write order, then new `connected`;
- any other source loss: its `degraded` or `disconnected` transition, then every newly due source-lost missing-receipt record for that producer in original completed-PTY-write order;
- session ending: for each stateful source sorted by raw source id, its `disconnected/session-ending` transition followed by its newly due source-lost missing-receipt records in original completed-PTY-write order, then the `exit` event after all sources.

Sequence, epoch, commit, queue, and byte-cap rules apply to the whole transaction. No prefix is accepted as though the operation completed, and the final `stream-exhausted` transaction names the limiting axis while leaving state consistent with exactly what durably committed. Output record partitioning is deliberately unconstrained inside the existing 1..65536-byte payload invariant; conformance checks byte coordinates, ordering, retention bounds, and gap honesty rather than operating-system read boundaries.

Old semantic-epoch deduplication entries are discarded when their producer is superseded and no pending correlation names that epoch; pending correlations retain only the exact tuples they still require. Rejectable producer/input operations preflight before changing state and refuse on a bound or axis failure. Holder-observed facts that cannot be rejected update live state; when whole-transaction preflight succeeds, their complete ordered transition set is enqueued without waiting for disk, and when an axis is limiting §8.4.1's final-allocation rule emits no transition prefix. Stateful replacement and session ending change their related in-memory facts atomically in the same order as the transaction. If durability becomes ambiguous, that candidate is the last possible event state and the event lane closes; no later transaction may contradict either selected frontier.

#### 8.4.2 The portable four-slot committed store

Events, logs, and lifecycle state share this exact record. All integers are unsigned little-endian:

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

A commit file is valid only when it is exactly 92 bytes, its self slot matches its filename, every enum/range/reserved check passes, its CRC is valid, the named body is at least the committed length, the prefix hash matches, and the kind-specific body rules below pass. Empty commit files are invalid initialization state. The two commits are validated independently. Any two independently valid commits with equal indexes are corruption and fail closed; their self-slot bytes must differ, so valid records in different commit files cannot be byte-identical. Otherwise the greatest valid index is authoritative. Readers ignore body bytes after its selected prefix. With no valid commit the store is unusable and is reported, never reset.

Creation exclusively creates the directory where Moor owns it and creates all four empty regular, non-link slots. For the caller-precreated event exception, Moor adopts only the already validated empty directory object and still creates every slot exclusively. Creation durably flushes the directory entries, writes and flushes `body.0`, then writes index 1 to `commit.0` and flushes it. Event index 1 contains the canonical schema header with epoch 0 and coordinates `[0,0)`. Log index 1 has an empty body, epoch 1, and `[0,0)`. Lifecycle index 1 contains §7.4's canonical `running` record, epoch 1, and `[0,0)`. Rendezvous publication is forbidden until every enabled initial store independently validates.

Writers never mutate bytes inside the selected prefix. Growth removes any older uncommitted tail, writes from the selected prefix length, flushes the body, writes the alternate commit at offset zero, truncates it to 92 bytes, and flushes it. Replacement writes the inactive body from offset zero, truncates and flushes it, then writes and flushes the alternate commit pointing to it. File creation and removal additionally flush the containing directory. A failure before a new valid commit is selected leaves the prior commit authoritative. A commit-flush timeout is explicitly ambiguous: either the old or submitted commit may later validate, so no rollback is guessed and that store closes permanently. A quarantined worker may finish only that already-issued flush (§8.5).

Kind rules are exact:

- **event:** the prefix is nonempty canonical schema-v2 JSONL within the 256 KiB cap and its bounded overage exceptions, starts with exactly one header, ends in LF, and contains no malformed or unknown record; commit epoch equals header epoch and commit coordinates equal header `first_retained` and `next_seq`;
- **log:** the prefix is arbitrary raw bytes whose length is exactly `end-start` and at most the configured cap; epoch increments on every body replacement but not growth, and empty is valid;
- **lifecycle:** the prefix is at most 4 MiB and exactly one canonical JSON object plus LF; epoch is 1, start equals end, and that coordinate is zero for `running` or the final child-output end for `exited`.

No counter wraps. Event sequence/epoch/commit exhaustion uses §8.4.1's whole-transaction precedence on every platform. Log commit `FFFFFFFFFFFFFFFF` may be used once for the newest representable suffix or a clear, after which logging becomes permanently unwritable; epoch `0xFFFF_FFFF` is the last valid epoch, and a replacement that would require its successor `0x1_0000_0000` (`4294967296`) is refused before mutation and likewise closes logging. Lifecycle has exactly initialization and exit commits, so recovered index/epoch state that cannot admit the exit update is corruption rather than a reset.

`WAKEUP`, durable semantic acknowledgement, and durable consumer-cursor advance occur only after the selected commit record is durable. A complete newline present in a body before a failed flush remains outside the selected prefix and is not consumed. No new holder adopts any old body, prefix, commit, store directory, or legacy companion layout.

#### 8.4.3 The cursor is selected commit identity plus sequence

A reader's position is `(commit_index, body_slot, committed_length, body_sha256, seq)` from the selected commit on every platform. A byte offset and open-handle file identifier MAY be cached as optimisations; neither is authoritative because neither identifies which committed prefix is selected.

On resume the reader revalidates both commits and selects the authoritative prefix before it trusts a cursor. If commit identity changed, it switches body or prefix and reads from the header. Otherwise it may read from the cached offset but must check that the first event carries the expected epoch and sequence. On any mismatch it re-reads from the selected prefix's start — cheap precisely because §8.4.1 keeps it bounded.

This is what makes the failure *detectable*. The defect in the designs this replaces is not that the reader lost its place; it is that it lost its place and could not tell.

**This changes the consumer.** A reader that stores a byte offset alone cannot implement the above. The supervisor ships the portable commit-aware cursor in §0.2's atomic coordinated cutover; no amended consumer adopts a legacy cursor as though it named a selected prefix.

#### 8.4.4 The bound, what compaction discards, and how a reader learns

The bound is a **byte cap on the file**, frozen at 256 KiB, with compaction triggered by the first whole-transaction append that would exceed it. A record-count cap is not used: the consumer's cost is dominated by bytes read, and a title is orders of magnitude larger than an exit code.

The bound also governs **admission**, not just cleanup after the fact. The *compaction baseline* is the exact header plus every snapshot §8.4.4 requires from the event-lane state immediately before the transaction being admitted in lane order. That state includes the effects of every earlier queued accepted transaction even when its commit is not durable yet, and excludes the complete ordered transition set of the transaction being admitted. Admission serializes and projects against that ordered predecessor state. A queued transaction may be written only after its predecessor selects durably; if an earlier candidate fails or is ambiguous, §8.5 closes the lane and discards every later candidate, so no later projected baseline is rebased or published over a missing predecessor. Before publication, the initial header plus space for the maximum legal terminal `ready`, `state`, and `link` snapshots must fit the cap or launch with `-T` fails. Before accepting a new semantic source or complete stateful snapshot that would change the retained set, Moor projects the resulting baseline using the exact bytes it would serialize, reserving those maximum terminal snapshots even when the child has not emitted them yet and the largest legal `semantic-source` status line for every admitted **stateful** source. If that projection exceeds 256 KiB, the semantic operation is refused with `SEM_RESOURCE_EXHAUSTED` before acknowledgement or state change. Later terminal observations and mandatory stateful-source degradation/disconnection transactions therefore always fit the reserved baseline; occurrence-only transition sets may still invoke OB-12's one-transaction overage. This byte-budget rule is independent of wire-schema-4's 64-source resource cap: the count cap bounds holder state and connections, while this projection proves that the exact retained JSON for the sources actually admitted, including their accepted 32 KiB snapshots, still fits durable storage.

**Compaction discards history, and the earlier text was wrong to imply otherwise.** The new body contains, in order: the header; snapshots for terminal `ready`, `state`, and latest `link`; the latest `semantic-source` state for each **stateful** source sorted by raw source id; the latest accepted stateful `semantic-assertion` whose `assertion_kind` is `"snapshot"` for each stateful source in the same order, restated with event `kind:"snapshot"`; and then the complete ordered transition set of the admitted transaction, each transition exactly once. Application receipts, missing-receipt diagnostics, edge assertions, and older transitions are occurrences and are not invented as current snapshots. Every omitted transition is gone; what the protocol guarantees is that **no loss is undetectable**, not that all history survives.

The mechanism that makes loss detectable is `first_retained` in the header: the lowest `seq` still present in the body.

- A reader whose next expected `seq` is **at or above** `first_retained` has missed nothing and continues.
- A reader whose next expected `seq` is **below** `first_retained` has a **gap**. It MUST NOT silently jump: before snapshots or retained events it emits §8.6's exact delivery-control gap record, then applies the snapshots to rebuild current knowledge; occurrences between its cursor and `first_retained` are unrecoverable.
- Applying snapshots is a resync, not a replay: a `snapshot` is never counted as something that just happened (§8.4.1).

**`link` is an occurrence, not a latest-value.** This has to be decided rather than left to the reader, because the two readings disagree exactly across a gap. A hyperlink the child emitted is an event in a history; the snapshot preserves only the most recent one, so hyperlinks emitted before a gap are genuinely lost and the reader is told so. A consumer that needs every hyperlink must consume faster than the cap, and this document says so rather than implying a completeness it cannot deliver. `state` and `ready`, by contrast, *are* latest-value facts, which is why their snapshots fully restore them.

#### 8.4.5 The conformance matrix

These cases MUST each have a vector, because each one broke something:

| case | required outcome |
|---|---|
| commit selection changes between validation and read | both commits revalidated, new selected identity followed, no *undetected* loss |
| reader lagging behind `first_retained` when compaction runs | one delivery-control gap is emitted, snapshots applied, no silent jump |
| post-compaction append | grows only the newly selected body prefix and is reachable through the next commit |
| crash during inactive-body rewrite | previous selected commit/body remains authoritative |
| crash during alternate commit rewrite | torn/wrong-size/CRC-invalid slot rejected; previous valid slot selected |
| crash after alternate commit flush | new committed prefix selected |
| body bytes beyond committed length | tail ignored by readers and never spliced into later committed data |
| equal valid commit indexes | corruption, fail closed; never choose by slot number |
| no valid commit | store unusable and reported; never reset or adopted |
| commit-flush timeout | store quarantined; either prior or at most the one submitted commit may later select; no later write |
| initial commit fails validation | no rendezvous publication and rollback only by recorded object identity |
| cursor offset lies beyond selected prefix | cached offset discarded and selected body re-read from its header |
| a multi-transition transaction that triggers compaction | every transition present after compaction exactly once and in order; no prefix or duplicate |
| `ready` and `link` present as snapshots | not reported as new occurrences |
| `observer-degraded` in a candidate compaction baseline | never snapshotted; only its original transition remains until ordinary retention discards it |
| sequence, epoch, and commit exhaust together | one diagnostic for the first limiting axis in `seq`, `epoch`, `commit` precedence |
| a multi-transition set reaches each exhaustion boundary | sequence refuses every transition and commits only its diagnostic; epoch/commit preserve the complete ordered set plus diagnostic; no prefix is selected |
| semantic source heartbeat loss/disconnect/reconnect | durable `degraded`/`disconnected` transition for a stateful source, new source epoch after reconnect, snapshot required before exactness; never inferred idle |
| edge source connect/assert/disconnect | no `semantic-source` event; only producer-wire `transition` assertions are accepted, and a producer-wire `snapshot` is refused |
| every legal and one illegal `semantic-source` status/reason pairing | legal pairs persist exactly; an illegal pair is malformed and never interpreted |
| semantic event retry after lost durable ACK | original event position returned; no duplicate line |
| application receipt with wrong tuple, source, generation or expired id | refused; no event appended and no pending request resolved |
| `source_seq`/`request_id` at `2^53`, u64 maximum, with a leading zero, and above u64 maximum | the first two round-trip exactly as canonical decimal strings; the latter two are malformed and refused |
| duplicate JSON key in a committed header or event | malformed and refused; no last-key-wins or first-key-wins interpretation |

*Failure prevented:* recovery cost that grew with session age; a restart that replayed every historical transition as current; a compaction that silently severed the supervisor from the session it believed it was watching; and a crash that selected a fragment as committed state.

### 8.5 Bounded durable-I/O lanes

The shared store engine runs one worker lane per enabled store so a stuck log flush cannot block events or lifecycle. The event lane accepts at most 64 queued whole transactions and 512 KiB of serialized queue bytes, including the one bounded cap-overage transaction. Each is serialized against the event-lane state after every earlier admitted transaction and may reach the writer only in that order. The log lane accepts at most 64 chunks and 1 MiB. The lifecycle lane accepts one update of at most 4 MiB. Queue admission and serialization occur without waiting for storage.

Initial store commits run in parallel under one two-second launch gate. At runtime the oldest item in each lane must obtain a selected durable commit within two seconds of admission. Queue overflow on a mandatory holder observation, an I/O error, or a missed progress deadline atomically closes only that store, clears later queued work, and quarantines its worker. A semantic request that would cross the event bound is instead refused before state change with `SEM_RESOURCE_EXHAUSTED`. The main PTY loop never joins or waits for a stuck worker. A worker returning after quarantine may finish only an already-issued commit flush and must not issue another write; readers may therefore select either the previous or that one submitted candidate, never a later one.

Closing the event lane clears event-writable in status/heartbeat, refuses new semantic ingress before state change, sends no false acknowledgement, and drops later observational events. Closing the log lane clears log-writable, stops copying output to the log while viewer delivery continues, makes live `clear` fail, and makes `tail -f` drain the last selected prefix then exit 1 with `<program-name>: log store is unavailable` plus LF on standard error. Closing the lifecycle lane clears its health bit; if the child later exits the holder retains the rendezvous as stale evidence. None of these transitions terminates or backpressures a running child.

### 8.6 Delivery-control gap and dead-letter records

OB-14 uses canonical JSONL delivery-control schema 1, separate from event schema 2 and controller `GAP`. If a consumer expects sequence `F` and the selected header has `first_retained=R>F`, it emits this LF-terminated record before snapshots or retained events:

```json
{"v":1,"type":"gap","session":"<session>","generation":null,"epoch":0,"first_seq":0,"last_seq":0}
```

The displayed values are typed placeholders: `session` exactly copies the header string; `generation` copies its u32 or `null`; `epoch` copies its u32; `first_seq=F`; and `last_seq=R-1`. Keys remain in the displayed order and numeric spellings follow schema v2. Multiple unseen compactions coalesce into one maximal inclusive range. The record has no Moor event sequence and does not advance the Moor cursor.

OB-25 keys a delivery failure count by exact `(session,generation,epoch,seq,SHA-256(record-bytes))`, where record bytes include the source LF. Success clears it. Failures one and two durably retain the count and do not advance the source cursor. On the third failure one downstream transaction atomically stores this LF-terminated record and advances past the source record:

```json
{"v":1,"type":"dead-letter","session":"<session>","generation":null,"epoch":0,"seq":0,"attempts":3,"record":"<base64>","reason":"consumer-transaction-failed"}
```

Typed placeholders copy the header/event values; `record` is canonical padded base64 of the exact LF-terminated source record. If the atomic downstream transaction fails, the cursor does not advance. Dead letters are durable queryable supervisor state, are never replayed or deleted automatically, and remain until explicit administrative disposition or retirement of the durable logical session lineage.

## 9. Observed terminal state

The program watches the child's output for a small, fixed set of signals and reports them through §8. Observation never alters the raw bytes. Title, link, and mode observation never delays forwarding; only §10.2.7's possible-query recognizer may delay at most 32 candidate bytes for at most 50 ms before releasing them unchanged or arbitrating a recognized query.

### 9.1 What is observed

Only these, and nothing else:

- **The window title.** Used for the activity classification in §9.2.
- **Hyperlinks** the child emits, reported as `link`.
- **A capability query emitted by the child**, which establishes `ready`. It is the child's *outgoing* query that is observed, not any answer to it; nothing is inferred about whether an answer arrived (§8.2).

These are the signals **reported to a consumer**. Separately, and not reported as events, the program tracks the terminal **modes** the child has enabled, so that a viewer attaching later can be told the state the child established (§5.2). That recovery state is load-bearing and MUST NOT be omitted: without it a viewer that arrives mid-session renders the child wrongly. Its exact contents and the sequences that update it are specified in §10 with the attach handshake.

Beyond those two purposes the program MUST NOT maintain a screen model, track cursor position, or interpret any other sequence. Anything it does not recognise passes through untouched and unremarked.

### 9.2 The activity classification

The child signals that it is working by prefixing its title with a spinner drawn from the Braille pattern block; a title without such a prefix means it is not working.

- A title classifies as **busy** when it begins with a Braille pattern character **immediately followed by an ASCII space**.
- Any other title classifies as **idle**.

The trailing space is part of the rule, not incidental. Verified against the reference: a title of `⠋working` classifies **idle**, while `⠙ spaced` classifies **busy**. An implementation that keys on the Braille character alone reports false activity for any title that merely starts with one.

This is a convention the child cooperates in, not a measurement of the process. The specification treats it as such, and §9.3 makes the resulting uncertainty explicit rather than hiding it.

### 9.3 Three states, not two **[load-bearing]**

The classification a consumer sees MUST be one of three values, and the third is not an error case:

- **busy** — observed, and the child says it is working.
- **idle** — observed, and the child says it is not.
- **not yet observed** — no title has been seen in this session. The program has no evidence.

"Not yet observed" MUST NOT be reported as idle. A session that has never spoken is not a session that is resting, and the difference is the difference between a supervisor waiting correctly and a supervisor confidently delivering work to a program that is not listening. Any implementation that collapses the third value into the second has failed this section.

### 9.4 Parsing requirements

The reported grammar is closed:

- title is `OSC 0 ; <text> TERM` or `OSC 2 ; <text> TERM`;
- hyperlink is `OSC 8 ; <params> ; <uri> TERM`, including nonempty-target open and empty-target close;
- readiness is the first recognized complete query from §10.2.7's five classes;
- OSC is either bytes `1B 5D` or byte `9D`; TERM is BEL `07`, ST `1B 5C`, or C1 ST `9C`;
- title text and URI are arbitrary bytes; hyperlink params are 0..1024 bytes excluding `07`, `1B`, `9C`, and `;`.

The title/link scanner retains at most 65,536 bytes from introducer through newest byte and recognizes sequences across arbitrary read boundaries. CAN `18` or SUB `1A` cancels an incomplete control string. An OSC with a missing selector or semicolon, a forbidden params byte, or another control introducer before termination is malformed and abandoned. On cancellation, malformed input, or byte 65,537, the scanner returns to ground without changing observed title/link state. It reprocesses the first abandoning byte when that byte can itself begin ESC, OSC, or CSI; otherwise it scans forward to the next such introducer. Numeric parameters have a total parser and out-of-range values are malformed, never wrapped. Observation never changes the forwarded raw bytes.

Every abandonment starts or continues a degradation episode for the affected scanner. The first abandonment in an episode emits one transition-only, never-snapshotted `observer-degraded` event when the event store remains writable. `scanner` is `"osc"` or `"query"`; `reason` is `"cancelled"`, `"malformed"`, `"limit"`, or `"deadline"`. An episode ends when that scanner next reaches ground and consumes an ordinary byte or recognizes a complete valid sequence. During the episode observer-exact is false in status and heartbeat; tracked-mode exactness is independent. When events are disabled or unwritable, those health fields are the required carrier.

Titles and link targets are **bounded**, not verbatim: a title at 255 bytes and a link target at 2048, truncated at a UTF-8 scalar boundary after invalid UTF-8 and embedded NUL are replaced by the Unicode replacement character. Truncation sets the record flag. Beyond normalization and bounding, Moor does not interpret, resolve, or rewrite either value.

## 10. The session protocols

§10.1 fixes generation identity; §10.2 fixes the framed protocol. Together they carry what §2.3, §3.2 and §8.4 depend on. The exact field layouts are in the conformance vectors (§0.2); these sections fix what those vectors must satisfy.

### 10.1 Generation identity **[normative]**

A **generation** is a number identifying one attempt to run one session. It is what makes it possible to say "the holder answering now is the one I started" rather than "something is listening on the path I used".

**The two variables.** The generation is carried into a session through the environment, under two names with different audiences:

- **`<BASENAME>_GENERATION`** — derived from the invoked base name by §4.4.1's byte transformation, but truncated independently so the complete key, including the `_GENERATION` suffix, is at most 127 bytes: the transformed base-name portion is capped at 116 bytes, then `_GENERATION` is appended. Thus `moor` reads `MOOR_GENERATION` and a user-created copy invoked as `moor-copy` reads `MOOR_COPY_GENERATION`. Read by the holder, and the value carried on the wire.
- **`MOOR_SESSION_GENERATION`** — read by the semantic producer running inside the child, so that what the child reports about itself can be attributed to the same attempt. This one is **not** derived: it is a fixed name in Moor's own vocabulary, so that a child's self-reports remain attributable no matter which name the holder was invoked under. A supervisor sets it, but the name belongs to Moor, not to any particular supervisor.

  An earlier pass named the first variable as a fixed literal. That was inconsistent: with `_SESSION_V2` derived and `_GENERATION` fixed, a renamed copy would read its derived `<BASENAME>_SESSION_V2` alongside `MOOR_GENERATION` — two halves of one identity disagreeing about which program they belong to.

They MUST carry the **same value** for a given session. They are separate names because they are read by separate programs with separate lifetimes, and collapsing them would make the child's self-reports unattributable after a restart.

**Grammar and range.** An unsigned decimal integer: one or more digits `0`–`9`, no sign, no leading `+`, no leading zeros, no whitespace, no thousands separators.

The real wire-generation range is **1 through 4294967295** — a 32-bit unsigned integer excluding zero. Within it, `1` is reserved for an unsupervised holder and the supervisor-allocated range is **2 through 4294967295**. This is frozen at the narrowest of the three ranges in play today, and deliberately so: the wire field is 32 bits, the child's semantic producer requires a positive integer, and the durable allocator admits a wider range than either. A generation valid only to the allocator is a generation that narrows silently on its way to the wire. A supervised launch carrying `1` is invalid rather than observationally colliding with the unsupervised sentinel.

**Zero is not a generation.** It is what a wrapped 32-bit counter produces and what a failed numeric parse produces, so admitting it would make the two most likely corruption results indistinguishable from a legitimate value. The last allocatable generation is 4294967295; there is no successor, and reaching it is the exhaustion condition below.

**Precedence and inheritance.** The launching caller **overwrites** both variables unconditionally; an inherited value is never adopted, never merged, never used as a starting point. Both are inherited by the child and by the child's own descendants, which is intended: a program deep inside a session must be able to attribute itself. A session created directly by a human, with no launching supervisor, has **no supervisor-allocated generation**: both environment variables are absent, and absent is a distinct state from zero.

**On the wire such a session uses generation 1.** Zero is never a generation. It appears only as the initial controller `HELLO` sentinel defined in §10.2.4, where it means “discover the authenticated holder's current generation”; the holder never echoes or adopts zero. Every accepted post-hello frame carries the nonzero generation learned or asserted there. Generation 1 is the lowest real wire value and is what an unsupervised session presents. The launch descriptor of OB-16 establishes supervision, while the disjoint numeric range prevents a supervised holder from being mistaken for the unsupervised wire identity: the supervisor starts at 2 and never issues 1. In the event stream the header's `generation` is `null` for an unsupervised session, because that field records *allocation*, not the wire value.

**[NEW] The holder enforces the pair; it does not merely carry it.** §4.4 says the caller's environment passes through unchanged, and for every other variable that remains true. These two are different, because they are a freshness fence rather than configuration, and "unchanged" and "the two always agree" cannot both hold. Verified against the reference, every broken state passes through today and the session starts with exit 0: one variable set and the other absent, the two set to different values, both set to zero, and both set to non-numeric text. Each of those produces a session whose wire identity and whose self-reports disagree — which is precisely the confusion the fence exists to detect.

What the holder does with the inherited pair depends on a fact the environment cannot carry, so the rules are given in §10.1.1 rather than here.

#### 10.1.1 A generation is a freshness fence, not an authorisation token

This distinction was got wrong in an earlier pass and the error propagated, so it is stated first.

**Any process running as the invoking user can set these variables.** They are ordinary environment. They prove nothing about *who* launched the session; they only distinguish one attempt from another. That is a freshness fence — it answers "is the holder answering me the one I started, or a predecessor?" — and it is genuinely valuable. It is not authorisation, and §11's peer-identity check is not replaceable by it.

The consequence is a rule that could not be implemented as written:

**A supervised session and a nested unsupervised one are observationally identical to the holder.** A session launched by the daemon, and a session a human starts *inside* an existing session, can present the same argv, the same working directory, the same user, and an equal, valid, in-range generation pair — because the nested one inherited it through ordinary environment inheritance (verified against the reference). The holder cannot tell them apart, so the earlier requirement — adopt the pair when supervised, strip it when unsupervised — asks it to branch on a fact it does not have.

**The launcher MUST therefore supply a discriminator that inheritance cannot forge.** The environment is the wrong channel by construction: anything placed in it is inherited by every descendant, which is exactly the confusion to be avoided.

**OB-16 fixes it as an inherited private channel** whose other end the launcher holds. The launcher passes only its read handle to the holder and sets `<BASENAME>_LAUNCH_CHANNEL` — derived from the invoked base name by §4.4.1's byte transformation, truncated independently so the complete key including the `_LAUNCH_CHANNEL` suffix is at most 127 bytes: the transformed base-name portion is capped at 112 bytes, then `_LAUNCH_CHANNEL` is appended, so `moor` reads `MOOR_LAUNCH_CHANNEL` — to select it: canonical unsigned decimal descriptor text. That environment value is only a selector and proves nothing. The launcher writes §15.1 of the companion schema's exact 32-byte launch record and closes its write end. Supervision is established only when the selected handle is one of the explicit inherited handles and yields that record followed by EOF within 2 seconds, with its generation in the supervised range and equal to both generation variables. The holder consumes and closes the channel, removes the selector before creating the requested child, and never places the handle in the child's inheritance list. A missing selector means unsupervised; a present but malformed selector, wrong handle type, timeout, short/long record, malformed record, generation-carrier failure, generation mismatch, or channel I/O error is a failed launch using §13.3's one platform-independent template, never a downgrade to unsupervised.

A nested descendant therefore inherits neither the selector nor the handle, while a manually forged channel remains only a same-user freshness assertion and gains no authority (§11.1). This channel is distinct from the instrumentation-load acknowledgement channel of OB-22 — reusing one for both would make an unsupervised `-S` invocation indistinguishable from a supervised launch.

With such a discriminator present, the rules are:

- **Discriminator present, pair present, equal, in the supervised range 2–4294967295** — a supervised launch. Adopt the generation.
- **Discriminator present, pair broken** — either or both carriers missing, unequal, out of range, zero, or unparsable — refuse to start (§13.3). Guessing which side is authoritative would attribute the session to an attempt that is not the one running.
- **Discriminator absent** — an unsupervised session, whatever the environment says. The session has no generation, and the holder **strips both variables** from the child's environment so they are not inherited further. A stale value from an ancestor must not travel down.

#### 10.1.2 Who owns the allocator

Allocation is durable state, and **OB-18 names its owner: the supervisor, not the holder.** The holder is told its generation and never allocates one. The first allocation for a new logical session key is `2`; every later allocation is the next larger value, with failed attempts burning their values as specified below. The store lives beside the supervisor's own state, is written before the launch, and is recovered by reading it; an unreadable store is a refusal to launch, never a reset. It is keyed by the supervisor's durable logical session key, which is never recycled for another logical session. That key identifies the supervisor's lineage for a named session and survives ordinary Moor `rm`, failed launch cleanup, and later recreation under the same name. It is **not** keyed by OB-17's live rendezvous identity, which does not exist before launch. The adoption gate binds the preallocated generation and logical launch to the live OB-17 identity plus holder incarnation after publication.

What is already fixed: **allocation is serialised across processes.** Two launchers racing for the same logical session must not both receive the same generation. The allocation is performed under an exclusive claim on that durable logical session key, and the claim is held until the number is committed.

**Durable ordering.** The generation is allocated and committed to stable storage **before** the process is started — never after, never concurrently. The consequences are requirements, not side effects:

- **A failed attempt burns its number.** If the launch fails at any point, that generation is spent. The next attempt uses a strictly greater one. Reuse is forbidden even when the failed attempt demonstrably left nothing behind, because "demonstrably" is exactly the judgement that is unreliable during a partial failure.
- **The record of a spent generation outlives removal of the session.** Removing a session's residue MUST NOT reset the counter: a later session of the same name must not be able to present a generation an earlier one already used, or an acknowledgement from the dead one authenticates the live one.
- **Generations are strictly increasing per session, never reused.**

**Exhaustion.** A legacy allocator may admit a wider range than the wire field carries, but a conforming supervisor applies §10.1's u32 limit before it commits or launches; silently narrowing a larger value would eventually produce zero, the one value that must never recur. **Wrapping is forbidden.** When the next generation would exceed the admissible range, allocation fails: the session is not started, the command exits non-zero, and it uses §13.3's exact generation-exhaustion row rather than reporting a generic launch failure. This is a terminal condition an operator must be able to recognise. Recovery requires an explicit supervisor administration operation that retires the entire logical session lineage and every stored adoption/cursor binding before assigning a fresh never-before-used logical key. Moor `rm`, residue cleanup, socket republication, and an ordinary retry do not perform that operation and never reset the counter.

**Use on the wire.** The acknowledgement that completes the adoption gate (§3.2), and every subsequent record, carries the exact generation. A record or acknowledgement bearing any other generation is **refused** — not coerced to the current one, not accepted with a warning, not logged and processed. A superseded generation is precisely the case this mechanism exists to catch.

### 10.2 The framed protocol

This section is normative for **what the protocol must guarantee and what it must carry**. The exact field layouts, integer widths, byte order, frozen constants, deadlines and error codes are in the companion artefact — **[moor-wire-schema.md](./moor-wire-schema.md), version `wire-schema-4`** — which an implementer builds against directly. That file also freezes the portable committed-store record and semantic-producer frames. Where the two disagree this section wins and the schema is a defect.

#### 10.2.1 Framing

- Every message is a self-delimiting frame with a fixed-size header giving at least: a magic value identifying the protocol, a version, a type, the generation (§10.1), and the payload length.
- **A reader never assumes message boundaries.** The transport delivers arbitrary byte runs; a frame is processed only when the whole of it has arrived, and a partial frame is retained.
- The payload length is bounded. A frame declaring more than the bound is a protocol error, not an allocation.
- **Unknown frame types are refused, not skipped.** A holder or controller receiving a type it does not know closes that connection with a stated error. Skipping is what lets a version mismatch look like success until it silently loses a message.

#### 10.2.2 Fragmentation and reassembly

A payload larger than the frame bound is split into a run of frames, all of the same type, each but the last marked as continued.

Requirements, each of which prevents a defect seen in production:

- **The run is reassembled across reads**, never rejected for arriving in pieces.
- **A gap in the run's sequence aborts the run** with a stated error. It does not skip and continue.
- **A frame of a different type inside a run aborts the run.** Two unrelated messages must never be spliced.
- **A run that exceeds the total payload bound aborts.**
- **An incomplete run has a deadline.** A peer that begins a run and never finishes it must not hold reassembly state indefinitely.
- **Reassembly state is reset on every completion, successful or aborted** (§5.2). No byte of one message may appear in the next.

Conformance vectors MUST include: a run split at every single-byte boundary, a sequence gap, a type change mid-run, a truncated header, a truncated payload, an oversized run, and a run abandoned past its deadline.

#### 10.2.3 Sequencing and acknowledgement

- Output carries a **record sequence** and a zero-based **byte offset**, so a consumer can detect loss and state where it resumed. The companion schema freezes nonempty records, contiguous offsets, the status descriptor's half-open retained range, and fail-closed u64 exhaustion; no implementation may choose inclusive endpoints or wrap either coordinate.
- Acknowledgement of output is **asynchronous and bounded**: a controller that falls further behind than the bound is disconnected, and it reconnects and re-baselines.
- **Acknowledgement never gates the child.** The read loop from the pseudo-terminal is never blocked waiting for a controller — that is the rule whose violation kills sessions (§5.1). Backpressure is resolved by dropping the slow consumer, never by stalling the child.
- When the holder knows output was lost, it says so explicitly rather than leaving a silent hole; the consumer's recovery is to re-baseline.

#### 10.2.4 The identity exchange and the adoption gate

The exchange that §2.3 and §3.2 depend on is staged so an identity probe never becomes an attach:

1. The controller connects. **Peer identity is checked before any byte of the payload is parsed** (§11).
2. The controller sends `HELLO` with protocol version, canonical identity and either the exact nonzero generation it expects or the generation-zero discovery sentinel. Its payload flags are reserved zero: hello never requests an input lease or changes child/viewer state. A supervising caller adopting a launch already knows the generation it allocated and MUST send that exact value; it may not turn a mismatch into success by using discovery. A human attach or side-effect-free liveness probe that has no allocator state may use discovery.
3. The holder sends `HELLO_ACK` with canonical identity, actual nonzero generation and holder incarnation. A side-effect-free liveness probe may stop here, or request `STATUS`/`STATUS_REPLY`, then close. No preamble or attach acknowledgement is sent on that path.
4. A viewer that will attach sends `ATTACH` with geometry and flags: bit 0 requests a fresh viewer lease, bit 1 is `NON_VT`, and bits 2..7 are zero. The holder accepts the attach atomically, freezing replay and entering attached phase before it queues anything. It sends `ATTACH_ACK` with the status descriptor, then the **terminal-state preamble** (§10.2.6), then `LEASE_RESULT` when bit 0 requested a lease, then §6.7's frozen `GAP`/`OUTPUT` baseline and live output on the same ordered connection. A busy lease request produces a refused/busy result but leaves the viewer attached as an observer.

The holder accepts a discovery sentinel only on the first `HELLO`, returns its actual nonzero generation in both the ACK header and payload, and requires that value on every later frame. Zero on any other controller frame is `GENERATION_MISMATCH`. The identity/adoption gate completes only when all four stages have completed through the mandatory preamble, whose `ATTACH_ACK` precedes it under revision 4, and the acknowledged generation **equals** the one the caller launched. The viewer's display baseline completes separately when it has processed through the `last retained` record named by that ACK; identity success must not be confused with screen exactness. Silence, malformation, a mismatched peer, or a mismatched generation is a failure — not a retry, not a downgrade. Discovery can establish a human attach or probe but can never satisfy a supervisor's adoption gate. Until that gate completes the session is `indeterminate` (§2.3), not running. The holder's atomic attach instant defines **fully attached**, so the status inside that same ACK already counts the new viewer; send failure detaches it again.

**The whole exchange is bounded.** A peer that connects and then says nothing must not hold a caller past the deadline; the deadline is stated and the timeout is a distinct, reported outcome.

#### 10.2.5 What the acknowledgement and the status descriptor carry **[OB-39]**

A controller attaching to a holder it did not just start must be able to learn everything it would otherwise reconstruct from the filesystem. The acknowledgement, or a bounded status request, carries:

- canonical session identity (OB-17) and generation;
- **holder incarnation** — which run of the holder this is, distinct from the generation;
- **event-stream identity and storage commit** — which path and layout this session is writing, plus the selected portable body slot, commit index, committed length, and hash for layout `02`, so a restarted supervisor does not guess (OB-39);
- **start metadata** — wall-clock start for display, monotonic start for arithmetic, **and** a boot identity that makes the monotonic value comparable. OB-31 resolves this as all three, not a choice among them: age is computed from the monotonic pair only when the boot identity matches the consumer's own, and is otherwise reported unknown rather than wrong;
- the child's **working directory** (OB-32);
- **child identity** — process identifier, a containment-set token, and a reuse-resistant birth token (OB-35). The token is the process-group identifier;
- the **retained-history descriptor**: the first/last retained output record sequence, the half-open retained byte-offset range, whether that raw history begins at byte zero, and whether the tracked terminal-mode state is exact or degraded (§6.7). There is no screen checkpoint or main/alternate-buffer exactness field in wire v4.

That last field is load-bearing: a consumer must learn *how much history actually remains* before it decides whether to replay or to re-baseline, because the holder cannot replay from an arbitrary offset.

The status descriptor's main flags use bit 4 for “this requesting controller owns the input lease”, bit 5 for “at least one fully attached viewer exists”, bit 6 for “requested child is running”, and bit 7 for “configured event store is writable”. A probe and an input-only `push` connection never set viewer presence.

One health-flags byte follows: bit 0 log store writable, bit 1 lifecycle store writable, bit 2 terminal observer exact, bit 3 query delegation still allocatable, and bits 4..7 zero. It is followed in order by u32 selected log epoch, u64 selected log commit index, u64 retained log start, and u64 retained log end. All four values are zero when logging is disabled; disabled logging clears health bit 0. Before child exit, a successfully initialized lifecycle store sets bit 1. Observer exactness is independent of the existing tracked-mode exactness flag.

#### 10.2.6 The terminal-state preamble

On every attach the holder sends a preamble frame. For a VT-capable viewer when tracked-mode state is exact, it restates the complete state, including explicit resets for modes the child left at their defaults (§1.1, §5.2). A fresh viewer's prior state is unknown, so omitting a known default is not equivalent to restoring it. When the bounded mode scanner has lost exactness, the frame is empty and the ACK clears tracked exactness. A `NON_VT` viewer also receives an empty frame even when tracking remains exact; in that branch the ACK may retain the actual exactness flag. These are the only two empty-preamble branches, and neither permits guessed controls. Requirements:

- It is **connection-local**: it is addressed to this viewer and is outside durable output history, but its ordinary frame header still carries the connection's exact generation. It carries no record sequence and no output offset, and **must not advance any output cursor or be logged**.
- It is sent **exactly once per attaching connection**, and **immediately after** the attach acknowledgement — revision 4's status-first order. A probe that never sends `ATTACH` receives none. A second preamble, a missing preamble on attach, or one arriving before its acknowledgement is a protocol error.
- Its **contents are frozen**: the tracked mode set is enumerated in §6 of the companion schema — twelve modes, and nothing else. §9.1 defers this here, and §5.2 makes it load-bearing: a viewer that arrives mid-session renders wrongly without it.

#### 10.2.7 Capability arbitration

At most one responder answers a recognized query. A valid reply from the fully attached, VT-capable lease viewer within 250 ms is selected as the one answer. Otherwise the holder applies the frozen synthesis-or-silence rule: an eligible synthesis produces exactly one answer, while a silence branch produces none. Observers and `NON_VT` viewers never answer.

Query classes are `01` primary device attributes, `02` secondary device attributes, `03` terminal name/version, `04` private-mode report, and `05` cursor-position report. Correlation identifiers are nonzero u64 values, start at 1 per holder incarnation, never wrap, and are never reused. `QUERY` carries u64 correlation, u32 lease epoch, one-byte class, then length-prefixed exact query bytes. `QUERY_REPLY` carries the same first three fields followed by length-prefixed reply bytes.

Let `CSI7` be bytes `1B 5B` and `CSI8` byte `9B`. The complete accepted query grammar is either introducer followed by exactly one listed tail:

| class | exact tail after CSI |
|---|---|
| `01` primary attributes | `63` or `30 63` |
| `02` secondary attributes | `3E 63` or `3E 30 63` |
| `03` terminal version | `3E 30 71` |
| `04` private mode | `3F <mode> 24 70`, where mode is canonical decimal `0..4294967295` |
| `05` cursor position | `36 6E` |

Accepted viewer replies are closed:

- class `01`: `CSI ? P[;P]* c`, one through 16 canonical decimal parameters, each `0..65535`;
- class `02`: `CSI > P;P;P c`, exactly three canonical decimal parameters, each `0..4294967295`;
- class `03`: `DCS > | T ST`, using either the matched 7-bit pair `DCS=1B 50`, `ST=1B 5C`, or matched C1 pair `DCS=90`, `ST=9C`; `T` is 1..128 bytes in `20..7E` and mixed forms are invalid;
- class `04`: `CSI ? <same-mode> ; S $ y`, echoing the exact query mode bytes, with `S` one canonical digit `0..4`;
- class `05`: `CSI R;C R`, with canonical decimal row and column each `1..65535`.

In the four CSI reply rows `CSI` is either `CSI7` or `CSI8`. Canonical decimal is `0` or a nonzero digit followed by digits, with no leading zero. No omitted parameter, embedded C0/C1 byte, private prefix not listed above, class mismatch, mixed representation, or trailing byte is accepted. Holder synthesis mirrors the query representation: CSI7 queries receive the existing 7-bit fixed reply, CSI8 queries its C1-CSI equivalent, and class `03` maps to the matched DCS/ST form. Fixed reply bodies remain as frozen in the companion schema, contain no trailing NUL, and partial child-input writes are completed. Class `05` is never synthesized.

The holder synthesizes identity replies only when it supplied §4.4.2's identity. A private-mode answer additionally requires exact tracked-mode state; after exactness loss a silent viewer is followed by silence, never a guessed answer. OB-20 disables all holder synthesis without suppressing viewer opportunity or altering environment identity.

At most 64 correlations may be outstanding. Admission first tests whether an eligible viewer owns the lease; otherwise it applies the no-viewer rule without consuming an identifier. If 64 are already outstanding, overload wins before the numeric counter is examined: disconnect that viewer as a slow control consumer and reserve its lease under §10.2.14, cancel every outstanding correlation in allocation/child-output order and resolve each immediately under the no-live-viewer rule, then resolve the newly recognized query last. Only after those decisions are serialized are its raw query bytes released to viewers. An already accepted viewer reply is never synthesized again.

If fewer than 64 are outstanding but no successor identifier exists, report `RESOURCE_EXHAUSTED` to and disconnect that viewer, perform the same ordered cancellation, and permanently clear query-health bit 3 for this incarnation. Correlation `FFFFFFFFFFFFFFFF` may be allocated once; after it resolves, the first later eligible query performs this exhaustion disconnect and subsequent queries use the no-viewer rule directly. This never terminates the child or prevents later input leases.

Transport loss and explicit lease release use the same ordered cancellation rule before discarding owner state. A malformed or class-mismatched reply is discarded while its correlation remains pending until valid reply, cancellation, or deadline. Duplicate, expired, unsolicited, wrong-generation, wrong-epoch, and superseded replies are discarded. A `NON_VT` lease viewer is treated as no eligible viewer.

Recognition delays only bytes that may form one supported query, never arbitrary terminal output. A candidate is limited to 32 bytes and a 50 ms recognition deadline; exceeding either releases it unchanged as ordinary output and reports query-scanner degradation under §9.4. Once recognized, `QUERY` is queued before those raw query bytes are forwarded. Split candidates, replies, cancellations, numeric boundaries, every grammar branch, identifier exhaustion, and partial child-input writes have conformance vectors.

#### 10.2.8 Size preservation **[OB-19]**

The attach exchange carries the desired size as ordinary fields, never by omission (§4.3). **OB-19 freezes the representation: both dimensions zero preserve; exactly one zero is malformed.** A real dimension is `1..32767`, both operands are widened before multiplication, and their product must be at most `2,000,000`. Vectors cover preserve, both mixed cases, each range edge, the product edge, and overflow.

#### 10.2.9 Input and the transport receipt **[OB-36]**

Input frames carry the generation and are refused if it does not match (§10.1). Wire v4 also carries a flags byte. With `APPLICATION_RECEIPT_REQUIRED` clear, no application-correlation fields are present. With it set, the frame carries a nonzero 16-byte application request id and a source id before the terminal bytes; the complete metadata and bytes are part of replay identity.

The holder returns a **transport receipt** stating what it actually knows: the frame was accepted, the generation and incarnation matched, and the write to the pseudo-terminal **completed**. There is no success value meaning *queued but not yet written* — a receipt is sent when the write is done, or it reports refusal with a frozen numeric cause. A queue slot is not delivery, and a status conflating them would be read as one.

**The receipt carries a request identity, and a retry is safe.** The identity is carried explicitly — it is *not* the frame sequence, which cannot serve: a fragmented input spans several, a retry would have to reuse one, and a reconnect resets the counter. A controller whose input went unacknowledged resends the same request; the holder recognises it, **writes nothing and performs no admission side effect a second time**, and returns the cached written-or-refused receipt payload in a newly sequenced frame. One request is in flight at a time. A fresh lease grant resets request numbering; resume preserves its high-water and cached request. Without this, the only recovery from an unanswered input is to risk writing it twice — which for an agent prompt means submitting it twice.

**The receipt MUST state what it does not prove.** It does not establish that the program running under the terminal read the bytes, parsed them, or acted on them. No session holder can establish that — it is outside what a holder observes. A consumer that treats a transport receipt as evidence of consumption has made an error this document names explicitly. §10.3 supplies a carrier for downstream OB-37 evidence; only a conforming provider integration can supply the fact.

When application evidence is required, the holder accepts the input only if the named source has an active stateful semantic connection advertising both input-notice and application-receipt capabilities. Before writing a byte to the pseudo-terminal it sends that source an `INPUT_NOTICE` carrying the application id, lease/request tuple, byte count and SHA-256 of the exact terminal bytes, and receives the matching prepared acknowledgement within 2 seconds from the still-current producer instance. Failure refuses the input with nothing written and names the reason in the receipt code. After a completed PTY write the correlation becomes eligible for an application receipt. A failed or incomplete write cancels it, returns a refused transport receipt carrying the actual completed byte count and `INPUT_WRITE_FAILED`, and caches that outcome so an exact replay writes no further bytes. This ordering makes the correlation available before the child can consume the bytes without claiming the producer has acted.

#### 10.2.10 Destructive requests

A request to terminate a session names the expected session identity, the generation, and the holder incarnation. The holder refuses atomically on any mismatch. A name alone is not sufficient authority: between the check and the command the named rendezvous may belong to a successor, and the operation would kill it. The identity re-check in §2.1 covers unlinking a filesystem object; it does not cover killing a listener.

Outcomes are the algebra of **OB-33** and are reported distinctly: terminated, already gone, refused on identity, indeterminate, failed.

#### 10.2.11 Notification and liveness **[OB-30]**

Two surfaces, deliberately separate:

- **A coalescible wakeup** telling a consumer that the event stream has advanced, so no consumer polls. Coalescing is explicit: several records may produce one wakeup, and the consumer reads the durable stream to learn what happened.
- **A holder and stream liveness signal**, distinct from the above. Silence on the wakeup channel is the normal state of a quiet session and MUST NOT be readable as death. Conflating the two is how a healthy idle session gets reported as lost.

The holder sends a heartbeat every 5 seconds. Its flags are bit 0 child running, bit 1 event store writable, bit 2 log store writable, bit 3 lifecycle store writable, bit 4 terminal observer exact, and bits 5..7 zero. A change to any flag queues an immediate heartbeat. Fifteen seconds without one invalidates the connection's verified-live evidence and triggers a fresh bounded identity probe. Until that probe positively establishes a listener's absence or completes an authenticated exchange, the session is `indeterminate` under §2.3 — heartbeat loss alone is never proof that the holder is gone.

#### 10.2.12 Error taxonomy

Every refusal names its cause from a frozen set, at minimum: unknown version, unknown type, oversized frame, malformed frame, bad sequence, reassembly aborted, generation mismatch, identity mismatch, unauthorised peer, deadline exceeded, and resource exhausted. A consumer branches on these — a single generic failure is what forces the guessing this document keeps removing.

#### 10.2.13 Lease and log-control frames

Controller values after existing `QUERY=14` are exact:

| value | frame | direction | exact payload |
|---|---|---|---|
| `15` | `LEASE_REQUEST` | controller to holder | byte 0 operation, byte 1 role, bytes 2..3 zero, u32 expected epoch, 16-byte expected holder incarnation, 16-byte resume token |
| `16` | `LEASE_RESULT` | holder to controller | byte 0 outcome, byte 1 reason, byte 2 role, byte 3 zero, u32 epoch, 16-byte resume token |
| `17` | `LEASE_RELEASE` | controller to holder | u32 epoch, then exact 16-byte current token |
| `18` | `LEASE_KEEPALIVE` | controller to holder | u32 epoch, then exact 16-byte current token |
| `19` | `LOG_CLEAR` | controller to holder | expected 16-byte holder incarnation, then u64 selected log commit index observed in status |
| `1A` | `LOG_CLEAR_RESULT` | holder to controller | byte outcome, byte reason, 2 zero bytes, u32 resulting log epoch, u64 observed/prior index, u64 resulting index, u64 cleared-through child-output coordinate |

Payload lengths are respectively 40, 24, 20, 20, 24, and 32 bytes. `MORE` is forbidden; any other length or fragmentation is `MALFORMED_FRAME`.

Lease-request operation is `00` fresh or `01` resume; role is `00` viewer or `01` input-only. Fresh carries zero epoch, incarnation, and token. Resume requires every field nonzero and exact against the unexpired reservation, including original role. Lease-result outcomes are `00` granted, `01` resumed, `02` released, and `03` refused. Reasons are `00` none, `01` busy, `02` bad epoch, `03` bad token, `04` bad role, `05` not held, `06` exhausted, and `07` bad incarnation. Grant/resume returns reason zero, nonzero epoch, and a fresh nonzero token; release returns reason zero, its nonzero epoch, and an all-zero token. Refusal has nonzero reason, reports the current allocated epoch, and carries an all-zero token. No result reveals another controller's token.

#### 10.2.14 Lease state, connection phases, attach, and `push`

The holder begins with allocated epoch 0 and no owner. A fresh grant alone allocates `previous allocated epoch + 1` and resets input-request high-water to zero. Release and expiry invalidate the token but do not increment the allocated epoch. `FFFFFFFF` may be granted once; after it is released or expires, fresh requests refuse exhausted forever. There is no queue, forced steal, or wrap; graceful handover is release followed by a new fresh request.

Every valid `INPUT`, `RESIZE`, `QUERY_REPLY`, or `LEASE_KEEPALIVE` from the owner refreshes its ten-second responsiveness deadline. A live lease client sends keepalive every three seconds while otherwise idle. Transport loss retains role, epoch, token, request high-water, complete cached request, and cached receipt for the remainder of the original deadline. A reconnect authenticated as the same user resumes only with exact generation, holder incarnation, epoch, token, and role; resume preserves the request state and atomically rotates the token. Deadline expiry releases without incrementing.

`LEASE_RELEASE` always receives `LEASE_RESULT`: an exact current tuple releases, while any mismatch refuses/not-held without mutation. A valid keepalive has no response. An invalid one receives connection `ERROR(LEASE_NOT_HELD)` and closes only that connection. Tokens come from the platform cryptographic source; all-zero is rejected, and token-generation failure refuses a grant without consuming an epoch. Fresh-decision order is active or reserved lease → busy; otherwise allocated epoch `FFFFFFFF` → exhausted; otherwise allocate next → granted. A syntactically valid resume mismatch refuses without revealing on the CLI which freshness field differed.

Lease loss changes phases exactly:

| cause | phase before | phase after | notification and retained state |
|---|---|---|---|
| successful explicit release | `I` | `U` | send released result; invalidate token and cached request |
| successful explicit release | `R` | `U` | send released result; invalidate token and cached request |
| successful explicit release | `V` | `O` | send released result; viewer remains attached |
| responsiveness deadline expires while connected | `I` | `U` | no unsolicited result; invalidate token and cached request |
| responsiveness deadline expires while connected | `R` | `U` | no unsolicited result; invalidate token and cached request |
| responsiveness deadline expires while connected | `V` | `O` | no unsolicited result; viewer remains attached |
| transport loss before deadline | `I`, `R`, or `V` | connection removed | retain role, epoch, token, request high-water, complete cached request, and cached receipt until original deadline; attached viewer detaches immediately |
| reservation deadline expires | no connection | no lease | invalidate retained token/cache; do not increment allocated epoch |

Every transition removing a live owner resolves that connection's outstanding queries in allocation order before discarding its transport state. A timely input-only resume enters `I`; a timely viewer resume enters `R` and must attach before entering `V`. After an active deadline expiry, later frames are checked in the new phase, so a late keepalive receives `ERROR(LEASE_NOT_HELD)` and closes that connection.

Each authenticated controller connection is in one phase:

| phase | viewer | owns lease | legal state-changing frames |
|---|---:|---:|---|
| `U` authenticated/unattached | no | no | `ATTACH`; fresh input-only `LEASE_REQUEST`; resume `LEASE_REQUEST` for either original role; `LOG_CLEAR` |
| `I` input-only | no | yes | `INPUT`, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `R` resumed viewer, attach pending | no | yes | `ATTACH` without request bit, `LEASE_KEEPALIVE`, `LEASE_RELEASE` |
| `O` attached observer | yes | no | fresh viewer `LEASE_REQUEST`, `LOG_CLEAR` |
| `V` attached lease viewer | yes | yes | `INPUT`, `RESIZE`, `QUERY_REPLY`, `LEASE_KEEPALIVE`, `LEASE_RELEASE`, `LOG_CLEAR` |
| `C` closing | no | no | none |

`STATUS` is legal only in `U`, `O`, and `V`; `OUTPUT_ACK` only in `O` and `V`; termination keeps its authenticated-phase rules. Any other phase/frame combination is `MALFORMED_FRAME` without state change.

A fresh interactive connection performs `HELLO`, then `ATTACH`. The attach request bit asks for an atomic fresh viewer-role grant; busy yields observer phase `O` and refused/busy without failing attach. An observer later upgrades by fresh viewer request, entering `V` without another baseline. A resumed viewer performs `HELLO`, resume request in `U`, receives resumed result and rotated token, then sends `ATTACH` with request bit clear within the two-second identity deadline. It enters `V` and receives the ordinary ACK/preamble/baseline with no second lease result. Viewer-role fresh request is illegal in `U`; an input-only fresh grant or resume enters `I`, while a viewer resume enters `R`. `ATTACH` is illegal in `I`. Graceful viewer detach releases and waits for the released result before closing.

`push` performs `HELLO`, then a fresh input-only lease request. It receives no preamble, attach acknowledgement, replay, or viewer output. Busy fails loudly and writes nothing. Once granted it sends input requests sequentially, resumes safely after a lost receipt when necessary, explicitly releases, and exits. It never changes geometry or counts as a viewer.

An attach without a fresh-lease request must send preserve geometry `0 x 0`, except for the immediate attach of an already resumed viewer. A fresh-lease-request attach may carry a valid nonzero geometry; it is applied only when the policy grant succeeds, and it is applied BEFORE the prefix is built. A prefix failure after that native success does not undo it: the lease unwinds, the geometry stands, and the next status reports it. A busy result leaves the new observer in `O` with session geometry unchanged. The immediate attach of an already resumed viewer may carry valid nonzero geometry because it already owns the lease.

#### 10.2.15 Ordered log clear

`LOG_CLEAR_RESULT` outcomes are `00` cleared, `01` already empty or disabled, and `02` refused. Reasons are `00` none, `01` stale status (incarnation or observed index), `02` store unavailable, and `03` store corrupt. Outcomes 00/01 with reason zero are CLI success; refusal exits 1. `LOG_CLEAR` and its result are same-user connection operations, not lease operations.

Let `P` be the request's observed index and `E` the assigned child-output end at the clear barrier. Every result echoes `P` in `observed/prior index`; remaining fields and mutation are exact:

| outcome / reason | resulting epoch | resulting index | cleared-through | mutation |
|---|---:|---:|---:|---|
| `00 / 00` cleared | selected nonzero epoch | newly selected index | `E` | empty replacement selected |
| `01 / 00` already empty | current nonzero epoch | current selected index, which may exceed `P` only because earlier admitted work completed before the barrier | `E` | none at barrier |
| `01 / 00` disabled | `0` | `0` | `0` | none; valid only when `P == 0` |
| `02 / 01` stale status | current selected epoch, or `0` when disabled | current selected index, or `0` when disabled | current selected end, or `0` when disabled | none |
| `02 / 02` unavailable | `0` | `0` | `0` | none claimed |
| `02 / 03` corrupt | `0` | `0` | `0` | none claimed |

Admission order is stale holder incarnation; disabled-state validation; unavailable/corrupt health; observed-index mismatch; enqueue the clear barrier. The observed index is checked once at admission, not again after legitimate earlier work advances the store. At the barrier, after every earlier log job completes, already-empty is returned only when the selected body is empty and its selected end equals `E`; every other state selects an empty `[E,E)` replacement. Later output jobs remain ordered after the barrier. Success is returned only after that result is known.

Once a clear body or commit write begins, a missed progress deadline or commit-flush timeout is indeterminate: quarantine the log lane, close this controller connection without sending a result, never retry automatically, and permit at most that already-submitted commit to validate later. A lost connection after submission and before a complete valid result has the same outcome. The CLI exits 1 and writes exactly `<program-name>: log clear outcome for session '<name>' is indeterminate` plus LF to standard output.

### 10.3 Semantic producer ingress **[NEW — ownership, provenance, recovery and correlation]**

Moor owns a local transport for facts that a provider integration can authoritatively emit. Moor does **not** decide what those facts mean. Semantic ingress is available only while the durable event stream is enabled and writable: without `-T`, no semantic token is injected and a `MOOS` hello is refused with `SEM_CAPABILITY_ABSENT`; after the sink becomes unwritable, no further semantic event can be accepted or acknowledged as durable, current semantic connections are closed with `SEM_RESOURCE_EXHAUSTED`, and controller status/heartbeat exposes the unwritable stream. The ownership boundary is exact:

- a provider adapter owns whether an assertion is true and the point in the provider lifecycle from which it is emitted;
- Moor owns same-user transport, holder-incarnation freshness, source provenance, ordering, deduplication, bounded recovery and durable publication into event schema v2;
- the supervising consumer owns provider-specific payload interpretation, precedence, lifecycle reduction and every product action derived from the assertion.

#### 10.3.1 Endpoint, discovery and freshness

Semantic producers connect to the same addressable session rendezvous as controllers. After the OS peer-identity check and before parsing any payload, the first four bytes select `MOOR` controller wire v4 or `MOOS` semantic wire v1. They are separate protocols; a frame from one is never accepted in the other.

When the event stream is enabled, the holder mints a cryptographically random 16-byte semantic token once per holder incarnation and injects its lowercase 32-hex encoding as `MOOR_SESSION_SEMANTIC_TOKEN` into the initial child. It does not define that variable when the stream is disabled. Producers discover the rendezvous through the derived `_SESSION_V2` value and carry the current `MOOR_SESSION_GENERATION` when supervised. The token is a **freshness and session-binding value, not authorisation**: same-user peer identity remains the security decision, and another process of that user is trusted by §11.1. A token from an older holder or another session is refused.

Each producer identifies a stable ASCII source id (1–128 bytes, `[A-Za-z0-9._-]`), a random 16-byte producer instance, a mode, and capabilities. `stateful` means the connection claims continuing knowledge and heartbeats; `edge` means a one-shot invocation that may report an occurrence but whose silence or exit says nothing. The first accepted connection fixes that source id's mode for the holder incarnation; a later hello changing edge to stateful or vice versa is `SEM_SOURCE_CONFLICT`, not a reinterpretation of existing events. Only one stateful connection per source is current. A same-mode replacement receives a new nonzero `source_epoch`, supersedes the old connection, and requires a complete snapshot before that source is exact. An edge source never becomes exact, emits no `semantic-source` lifecycle record on connect or disconnect, and may publish only producer-wire `transition` assertions.

#### 10.3.2 Ordering, deduplication and durable ACK

Within one source epoch, newly accepted semantic event `source_seq` starts at 1 and advances by exactly one. Each assertion or application receipt also carries a producer-chosen 16-byte `event_id`. The holder retains the last 512 accepted `(source_seq, event_id, SHA-256 of the exact complete reassembled event payload, durable position)` tuples for that source epoch for the holder lifetime; the digest excludes transport headers and frame sequence so a retry may be fragmented differently. A retry of any retained tuple with identical payload bytes returns a newly sequenced duplicate ACK naming the original durable position and appends nothing; the same id or sequence with different bytes is `SEM_EVENT_CONFLICT`. A sequence below the high-water mark that is no longer retained, or one above high-water plus one, is refused as bad sequence. The unsigned 64-bit source sequence never wraps: `FFFFFFFFFFFFFFFF` may be the final accepted event of an epoch, after which another new event is `SEM_RESOURCE_EXHAUSTED` and a stateful producer must reconnect into a new epoch and snapshot again. The 512-entry bound is admission control: when the holder cannot retain the required tuple it refuses before acknowledging, never silently weakens deduplication.

An accepted ACK includes the durable event `(epoch, seq)` and is sent only after the event-storage commit in §8.4.2. A refused ACK carries a frozen semantic error code; a connection-level refusal that cannot identify an event uses `SEMANTIC_ERROR` instead. A lost ACK can therefore be retried without a second event. Assertions are a UTF-8 JSON object at most 32 KiB, maximum depth 64 and at most 1024 members per object; duplicate keys, non-finite numbers, invalid UTF-8 and any non-object top level are rejected. Moor validates these envelope properties, preserves the exact bytes as canonical padded base64, and does not interpret provider keys.

#### 10.3.3 Recovery is degraded, never guessed idle

A stateful producer sends a complete producer-wire `snapshot` before transitions. Its first publication is a `semantic-assertion` event with `kind:"transition"` and `assertion_kind:"snapshot"`; only compaction-generated restatements use event `kind:"snapshot"`. Until that assertion is durably committed, its `semantic-source` status is `connected`, not `exact`. Heartbeats are every 5 seconds; 15 seconds without one changes the source to `degraded` and records why. A transport close changes it to `disconnected`. Either condition removes exactness, and even the same connection can regain `exact` only by durably publishing a fresh complete snapshot. A new connection gets a new source epoch and must snapshot again. Neither loss nor silence is converted to `idle`, `ready`, `done`, or any provider state. A supervising consumer may rely on other evidence, but it can see from provenance that the high-quality source is absent rather than mistaking silence for a provider state.

Edge assertions are durable occurrences. They cannot establish continuing exact state and are not retained as state snapshots during compaction. An edge producer sending producer-wire assertion kind `snapshot` is refused with `SEM_INVALID_PAYLOAD`; accepting it as an occurrence would preserve a field that falsely claims continuing state, while retaining it would be worse. The latest exact stateful snapshot and latest stateful-source status are retained as §8.4.4 specifies.

#### 10.3.4 Application correlation and the limit of OB-37

For a required application receipt, the controller chooses an application request id not used by another pending or retained correlation in this holder and reuses it under the same lease/request tuple only for an exact replay of the same `INPUT`. The id is not a generation-long uniqueness ledger: after its correlation resolves or expires it may be used with a later, never-reused lease/request tuple, and an old receipt still cannot match because every tuple field is checked. Before writing, Moor binds the correlation to the then-current source epoch and producer instance. Moor retains at most **512 written correlations total per holder** for 10 minutes; any one source may consume the whole allowance, but no source receives a separate 512-entry pool. The status descriptor's pending count is this same holder-wide value. Admission fails before the PTY write when that bound is full. At 60 seconds without a receipt Moor emits `application-receipt-missing{reason:"deadline"}` but retains the correlation. The bound producer's first loss condition emits `reason:"source-lost"`: heartbeat timeout changing it to `degraded`, transport close changing it to `disconnected`, supersession by a replacement, or session ending. No later loss condition emits that reason again for the same correlation. Final expiry emits `reason:"retention-expired"` and removes the correlation. Each reason is emitted at most once per correlation, and every missing record names the bound producer and source epoch. A valid accepted **or refused** provider receipt must arrive from the same producer connection and match source, source epoch, application id, lease epoch and request id of a correlation whose transport write completed. It resolves and removes the correlation only after the `application-receipt` event is durable. The retained semantic-event deduplication check runs first: an exact retry after resolution returns the original durable position and does not try to resolve again; a new event naming a resolved or expired tuple is unknown. Mismatch, pre-write receipt, stale generation/source epoch, superseded producer, or unknown/expired request fails closed.

This carrier makes provider proof possible; it does not manufacture it. **OB-37 remains a per-provider runtime gate.** A provider/version closes its gate only after a real shipped integration demonstrates that: the application id reaches a named authoritative provider point; that point emits the matching receipt; wrong, missing, stale and duplicated ids fail closed; crash/reconnect behavior follows this section; and a supervising integration consumes the durable event end to end. Until that evidence exists for the provider actually deployed, the gate remains open and a supervising integration MUST NOT treat the receipt path as authoritative; Moor defines no consumer fallback state. A schema entry or a green isolated encoder test is not that evidence (§5.5).

## 11. Security model

### 11.1 What is trusted

**The invoking user is trusted. Nothing else is.** Not the child's output (§5.3), not a connecting peer, not a caller-supplied path, not the contents of any file the holder did not itself write.

Authorisation is **same-user**: a session may be driven only by a process running as the user that created it. There is no delegation, no group access, no capability handed to another account. A holder discovering otherwise refuses and says so.

### 11.2 Peer identity

**Every accepted connection has its peer's identity checked before a single byte of its payload is parsed** (§5.5). A connection from another user consumes nothing — no capability, no lease, no generation, no session state, no reassembly state — and is closed with a stated error.

The mechanism is platform-specific and is reached through the abstraction §5.4 requires; an unsupported platform fails to build rather than omitting the check.

### 11.3 The rendezvous **[OB-21]**

- The socket is created **reachable only by the invoking user** — mode `0600` (§12.2). A **bare name** places the addressable object inside the enforced root. A **path form** places it exactly where the caller said, outside that root: the final socket protection still applies, the parent is never created, and parent-directory safety is the caller's responsibility.
- It is **published atomically**: the socket is never observable at its final path in a state where connecting would reach a half-built holder (§3.2, §12.2).
- Before unlinking a socket, its type and identity are re-verified immediately beforehand (§2.1), and it is unlinked only in the `stale` state (§3.7).

### 11.4 Caller-supplied paths — one rule **[OB-21]**

The event directory (§8.1), redirected standard error (§4.6), and launch-time instrumentation object (§4.7) are three caller-supplied paths. They share one validation rule; their required object types remain explicit at the owning sections:

1. The **creating process** opens and validates the path — never a forked child, never later.
2. Validation happens **before the rendezvous is published**, so a failure leaves no session behind.
3. The open cannot block and cannot follow a symbolic link.
4. The target is owned by the invoking user and has the exact mode required by its owning section. Standard error uses exact owner-only protection; the executable instrumentation object may be readable/executable but never writable by group/other (§4.7). Standard error and the instrumentation object are regular files; the event target is a directory (§8.1).
5. Only already validated open descriptors or handles are passed onward. The creating process opens the event directory link-safe and creates the four slots against that verified object. It copies instrumentation bytes from the one validated caller handle to §4.7's immutable stage; no later component receives or reopens that caller path.
6. Any failure is fatal and reported (§13.1).

The event directory additionally resides inside the session root (§8.1), because a privileged supervisor reads it and its location is part of what makes it addressable after a restart (§10.2.5).

### 11.5 The ownership fence on destructive operations

Terminating a session requires **proven ownership**, not a matching name. A listener that answers but does not complete the identity exchange, or answers with the wrong generation, is `indeterminate` (§2.3) and **MUST NOT be terminated** — it may be a stranger's process, or a successor to the session the caller meant.

After a failed launch, terminating is permitted only against the launch identity that failed. Retirement succeeds only when termination completed **and** the addressable rendezvous object is gone; an uncertain outcome is recorded for retry rather than reported as success (§10.2.10, OB-33).

### 11.6 Store provisioning, cleanup, and writer exclusion **[OB-27]**

Creation performs this order and no other:

1. Resolve and probe the requested session without mutation. Verified-live or indeterminate refuses.
2. For stale residue, fence the rendezvous independently. On POSIX, hold the already verified parent-directory descriptor, use `fstatat(..., AT_SYMLINK_NOFOLLOW)` to require a socket and capture its device/inode identity when listener absence is positively established, then repeat that same no-follow type and identity check immediately before `unlinkat`. The socket itself is never opened, so this works on both Linux and macOS. A dead socket has no generation to compare because no authenticated exchange can complete; cleanup therefore never demands, guesses, or synthesizes a rendezvous generation.

   Independently open the exact `.exit` lifecycle path from §2.1 and validate its selected commit and manifest. Its `session` check is closed: it must equal tag `01` plus the requested lexically resolved absolute socket-path bytes.

   The lifecycle commit's session-wire-generation field must equal the manifest's `wire_generation`, and the manifest's generation pair must be internally consistent: `generation:null` with `wire_generation:1`, or an equal allocated `generation` and `wire_generation` in `2..4294967295`. No generation equality is required between that internally valid lifecycle state and the independently stale rendezvous. In particular, POSIX combined residue can contain a socket and lifecycle state from different historical generations while retaining the same path identity; §3.3's combined stale cross-product removes both under their independent fences.

   Only after those checks may cleanup consider the exact derived log, the exact event path and instrumentation stage named by that manifest, and the lifecycle directory. Each candidate must independently agree where it carries ownership data: the selected log commit has the manifest's `wire_generation`; the selected event commit has that `wire_generation`, its header `session` equals the manifest's `session`, and its header `generation` equals the manifest's `generation`; and the stage final component equals `<H>.instrument` recomputed from the manifest's session identity, wire generation, and incarnation. Cleanup revalidates every object immediately before removal. A missing, malformed, or disagreeing manifest or companion leaves that companion unowned and non-removable; it does not prevent removal of the independently fenced stale rendezvous. Cleanup never infers an event or instrumentation companion from the rendezvous spelling, and it never falls back to a guessed companion set when the manifest disagrees.
3. Provision the requested event target. If absent, create its directory exclusively. If present, accept only the exact validated empty directory handed off by this caller. Create all four slots exclusively; a leftover slot is refusal.
4. Create the log and lifecycle directories and optional instrumentation stage; commit every initial store record; then publish the rendezvous.

The resolved rendezvous, event, log, lifecycle, and instrumentation objects must be pairwise distinct by canonical path and opened file identity. Before publication the creator owns rollback: it records every created identity and removes only those same identities, in reverse order, after confirming child and holder did not survive. Once published the holder owns normal retirement and `rm` owns confirmed-stale cleanup. Uncertain termination removes nothing.

Background launch transfers rollback ownership over its private holder-to-creator result stream using exact 12-byte records: ASCII magic `MORR`, format byte `01`, state byte (`01` store-adopted, `02` ready, `03` failed), little-endian u16 result code, and little-endian u32 generation. `store-adopted` is sent only after the holder owns every writer lease and captured every object identity; after it, the creator never deletes by path. `ready` is sent only after initial commits, the child-launch gate, and rendezvous publication. EOF or failure before adoption leaves rollback with the creator after confirmed holder death; loss after adoption is resolved by identity probe and otherwise remains indeterminate. Foreground `run` crosses the same states internally.

Every store has one portable exclusive writer lease on `commit.0`: nonblocking `flock(LOCK_EX)`. The holder acquires lifecycle, event, then log leases before publication and holds them until close. Offline `clear`, `rm`, and stale replacement acquire the same applicable order under one two-second total deadline, re-probe liveness after acquisition, and release in reverse order. Failure to acquire or a changed probe is indeterminate and performs no mutation. This serializes competing offline commands and holder writes without a fifth file.

The event directory may be caller-created only under §8.1's validated-empty exception; log and lifecycle directories are always holder-created. Every store and stage is bound to the owning generation and incarnation and is removed only on confirmed retirement or rollback. A new generation never adopts a predecessor body, commit, manifest, stage, or legacy layout. Existing legacy log/exit residue is inventoried and drained before the amended reader is enabled.

A runtime store failure is reported through §10.2.5/§10.2.11 health, never swallowed. The owning behavior of a live or offline `clear` is §10.2.15 and §7.3; no second writer path exists.

### 11.7 What the child inherits

Before the child starts: **the child inherits nothing the holder did not intend.** Every descriptor not explicitly required is closed and inherited signal dispositions and the signal mask are reset (§5.5). No descriptor belonging to the holder's own logging or state is left open in either case. A child must not be able to write to the session log, event sink or holder rendezvous.

## 12. Platform behaviour

The supported families are Linux and macOS, and nothing else. "Any Linux" is not a support claim. Release conformance MUST cover the concrete matrix in §12.8; builds outside it may work but are not represented as supported until added with the same evidence.

### 12.1 What is genuinely portable

Controller wire v4, semantic wire v1, JSON event schema v2, the four-slot event/log/lifecycle store, checksums, integer byte order, generations, correlations and provenance are portable between the two families. Native path values are raw POSIX bytes on every surface.

### 12.2 Rendezvous and peer identity **[decision]**

Linux and macOS publish the Unix-domain socket itself at the addressable path (§11.3). Peer identity is the connecting process's effective user, read from the accepted socket by the platform's credential mechanism (§11.2); the same-user trust boundary of §11.1 is the whole of the authorisation model. There is no marker file, no secondary rendezvous object, and no pre-authentication preface: the first bytes a peer sends are parsed as `MOOR` or `MOOS`.

### 12.3 Child launch and terminal boundary

The holder allocates a pseudo-terminal at the selected geometry (§4.1), starts the requested child as the session leader of a new process group with the pty as its controlling terminal, and passes the child's byte stream unchanged in both directions (§1.1). When `-S` is present the instrumentation module is inserted through the platform's dynamic-loader preload mechanism (§4.7); its acknowledgement must arrive from inside the requested child before publication, and any failure terminates the child's process group and leaves no published rendezvous.

A signal handler only records an atomic flag and wakes the normal event loop; it never calls `exit`, allocates, logs, closes descriptors or runs cleanup. The first accepted termination notification setting that flag starts one ten-second monotonic whole-shutdown deadline. The normal wake path abandons peer waits and never waits for a store worker. Graceful child termination begins immediately, escalates at five seconds, and at ten seconds the holder closes remaining descriptors and exits, retaining rendezvous evidence whenever lifecycle durability or child termination is uncertain. A second notification escalates immediately but never resets either deadline. No peer response, flush, callback, diagnostic, or thread join may extend ten seconds. OB-42 conformance drives these paths through the shipped binary rather than a handler unit test.

### 12.4 Process containment and termination **[OB-34]**

Graceful termination sends `SIGTERM` to the terminal foreground process group; if no foreground group can be identified or that dispatch fails, it falls back to the requested child's process group. Force, `-f`, or escalation after 5 seconds uses the same targeting rule with `SIGKILL`. Descendants in other/detached groups are explicitly not reached, and Moor never claims causal descendants outside the signalled group were reached.

The status descriptor's 4-byte containment value is the child's process-group id. `TERMINATE_RESULT` states the mechanism and whether a known survivor escaped the covered set.

### 12.5 Portable committed storage

Linux and macOS use §8.4.2's fixed dual-body/dual-commit protocol for events, logs, and lifecycle. File identity alone is never a commit. Readers select the greatest independently valid 92-byte record and treat its body slot, prefix length, hash, and commit index as storage identity. The only platform differences are the durability primitives named in §8.4.2 and §11.6. Every release lane injects crashes at every byte boundary of body write, body flush, commit write, and commit flush, then recovers with either slot torn, both slots valid with unequal indexes, equal valid indexes as corruption, and uncommitted body tails.

### 12.6 Boot identity and start arithmetic

Wall-clock start is unsigned milliseconds since the Unix epoch on every platform. Monotonic start and the exact 16-byte boot identity are platform-specific but paired:

- **Linux:** monotonic milliseconds come from `CLOCK_BOOTTIME`. The identity is the 16 UUID bytes parsed from `/proc/sys/kernel/random/boot_id`; only the canonical UUID grammar (hex case-insensitive on input, exact `8-4-4-4-12` grouping) is accepted.
- **macOS:** monotonic milliseconds come from `mach_continuous_time` converted with its timebase. The identity encodes the `kern.boottime` `timeval`: unsigned seconds in little-endian bytes 0–7, microseconds 0–999999 in little-endian bytes 8–11, and ASCII `MAC1` in bytes 12–15.

If the platform identity source is unavailable, times out, is malformed, or cannot be converted exactly, the identity is sixteen zero bytes and **never compares equal**, including to another zero value; monotonic age is then unknown rather than guessed. A consumer computes age only when its own freshly read identity equals the holder's nonzero identity byte-for-byte, using its matching platform monotonic clock. Wall time is display metadata and never participates in age arithmetic.

### 12.7 Exit status domain and default child

Child status is 0–255 or a signal. Event schema v2 and the durable exit record carry `ended:"exited",code:<u8>` or `ended:"signalled",signal:<positive signal>`, and the mandatory `method` field carries `"graceful"` or `"forced"` when Moor itself asked the child to end, or `"none"` otherwise — an external `SIGTERM` records `method:"none"` and is distinguishable from a holder-initiated one by that axis alone.

The holder's own command-error statuses, including 127 for a child that could not be started, apply on both families. The default child is the nonempty `SHELL` variable, then the invoking user's login shell from the account database; a selected value is a native executable path with no embedded arguments, and invalidity is a child-start failure, not permission to continue down the list.

### 12.8 Required release conformance matrix

Every release MUST publish results from at least these lanes against the shipped artifacts, not only libraries:

| family | minimum lanes |
|---|---|
| Linux glibc | Ubuntu 22.04 x86_64, kernel 5.15 or newer; Ubuntu 24.04 arm64 |
| Linux musl | Alpine 3.20 x86_64 and arm64 |
| macOS | macOS 13 or newer on Intel x86_64 and Apple Silicon arm64 |

Every lane exercises create/attach/detach/input/replay/termination, lease acquire/release/resume/loss, query arbitration, live and stale clear, same-user and wrong-user peers, generation/incarnation fencing, `-S` staging plus wrong-architecture failure, event/log/lifecycle recovery, every dual-slot crash prefix, scanner degradation, canonical JSON/delivery-control vectors, byte-exact CLI vectors, and real supervisor restart while the holder lives. A missing lane narrows the release's stated support; it is not converted into a paper waiver.

## 13. Diagnostics and exit codes

Exit statuses are an interface. Scripts branch on them and the supervisor records them, so this section is frozen and differentially tested. The stream conventions are in §3.6 and are not repeated here.

### 13.1 Statuses

Verified against the reference unless marked **[NEW]**.

| situation | status |
|---|---|
| usage, `--help`, `--version`, no arguments | **0** |
| `list` with no sessions (prints `(no sessions)`) | **0** |
| `clear` on a session that does not exist (§3.4) | **0** |
| child ran and exited with status *N* | ***N*** |
| child was terminated by a signal | **1** |
| child could not be started (executable missing, not executable) | **127** |
| argument or command error — unknown mode, missing operand, unparsable value, **[NEW]** trailing operand | **1** |
| named session does not exist (`kill`, `rm`, `tail`, `push`) | **1** |
| attaching creator (`attach`, bare, `new`/`n`) with no controlling terminal | **1** |
| `current` outside any session (prints nothing) | **1** |
| **[NEW]** any command refused because the session is `indeterminate` (§3.7) | **1**, with a diagnostic naming the state |
| **[NEW]** submitted live clear loses a complete durable result (§10.2.15) | **1**, outcome indeterminate; never retry automatically |
| **[NEW]** log lane unavailable to live clear or exhausted `tail -f` (§8.5) | **1** |
| **[NEW]** child exits after successful start but before publication, background or attaching creator (§4.8) | **1** |
| **[NEW]** any failure while validating a present supervised-launch selector, its private channel/record, or its required generation carriers (§10.1.1) | **1**, §13.3's exact platform-independent supervised-launch row; never downgraded to unsupervised |
| **[NEW]** `_SESSION_V2` malformed or non-round-tripping (§4.4.1) | **1**, §13.3's exact ancestry-malformed row |
| **[NEW]** generation space exhausted for this session (§10.1) | **1**, §13.3's exact generation-exhaustion row — distinct from a generic launch failure, because no retry reaches it |
| **[NEW]** session root exists but is not an owner-only directory owned by the caller (§2.2) | **1**, naming the path and the offending mode or owner |
| **[NEW]** event sink rejected (§8.1) | **1** |
| **[NEW]** stderr sink rejected (§4.6) | **1** |
| **[NEW]** launch-time instrumentation object missing, rejected, wrong-architecture, or unacknowledged (§4.7) | **1** |

The child's status passes through unchanged across the full range, verified at 0, 7 and 255. The **127** for a failed start is the conventional shell value and is deliberate: a caller cannot otherwise distinguish "your program is not there" from "your program ran and returned 127", and it accepts that ambiguity in exchange for matching what every shell already does.

### 13.2 A terminated child reports 1, and that is not enough

Verified against the reference, a child killed by any POSIX signal yields foreground status **1**. The amended event and lifecycle records do not collapse that outcome: they carry `ended:"signalled"` and the positive platform signal number, with no `code`. A normally exiting child that returned 1 therefore remains distinguishable to durable consumers even though the shell status is the same.

This document resolves that asymmetrically, using the split in §0.2:

- **The holder's exit status stays 1.** Scripts on disk branch on it today and §0.2 promises them no change. The tempting convention of `128 + signal` MUST NOT be adopted; a caller comparing against 1 would silently stop matching.
- **[NEW] The event stream carries the truth, where the platform has it to give.** POSIX distinguishes a normal exit from a signal and names the signal; when Moor itself initiated graceful or forced termination it knows that fact and records it in the mandatory `method:"graceful"|"forced"` field, orthogonal to `ended` (§12.7); with no termination state it records `method:"none"`. It does not infer a method for an external termination.

The branch encoding is fixed in §8.2. This section owns only why the platform branches differ.

### 13.3 Message uniformity

Every diagnostic begins `<p>: `, where `<p>` and every displayed name/path/token use OB-29 rendering. Argument failures write exactly two LF-terminated lines to standard output, nothing to standard error, and exit 1:

```text
<p>: <message>
Try '<p> --help' for more information.
```

The argument-message set is closed: `Invalid mode '<x>'`, `Invalid number of arguments`, `Invalid session name '<x>'`, `Option '<o>' requires an argument`, `Invalid value '<x>' for option '<o>'`, and `Option '<o>' is not valid for '<command>'`. Unknown leading-dash tokens use `Invalid mode`; missing or excess operands use `Invalid number of arguments`; lexical name rejection uses `Invalid session name`; the remaining cases use their literal template. `-t` with `-R move` uses `Invalid value 'move' for option '-R'` regardless of token order.

Runtime branches are exact. `<error>` is the platform's nonempty single-line error text only on the legacy child-exec row. On the path/sink rejection rows, `<cause>` is exactly one of `missing`, `wrong-type`, `not-directory`, `not-searchable`, `link`, `wrong-owner`, `wrong-mode`, `not-empty`, `extra-entry`, `pre-existing-slot`, `not-absolute`, `outside-root`, `identity-changed`, `io-error`, `wrong-architecture`, or `load-unacknowledged`. `not-absolute` is legal only on the event-target and instrumentation-rejection rows; in either row `<path>` preserves the original operand spelling under OB-29 rendering, as §§4.7 and 8.1 require.

On the supervised-launch row, `<cause>` instead uses this separate closed vocabulary: `generation-missing`, `generation-malformed`, `generation-disagree`, `selector-invalid`, `channel-timeout`, `record-wrong-length`, `record-malformed`, `generation-mismatch`, or `io-error`. Validation proceeds through selector, channel completion, record shape, generation carriers, then record/carrier agreement; the first failing stage supplies the cause. `selector-invalid` covers a present noncanonical selector, a handle outside the explicit inheritance list, or the wrong handle type. `record-wrong-length` means either clean EOF before 32 bytes or observation of any 33rd byte, which rejects an overlong record immediately without waiting for EOF. `channel-timeout` means the record-plus-EOF condition remains absent or incomplete at the two-second deadline without either of those definitive length outcomes; an exact 32 bytes without EOF therefore times out. An exact-length record with bad magic, format, reserved bytes, or out-of-range record generation is `record-malformed`; another channel-system failure is `io-error`. Once selector, channel, and record validation succeeds, carrier precedence is presence, then grammar/range, then pair equality, then record equality: either carrier absent is `generation-missing` even if the present one is malformed; with both present, either bad grammar or range is `generation-malformed`; two valid unequal values are `generation-disagree`; and a valid equal pair different from the record is `generation-mismatch`. A wholly absent selector remains the unsupervised branch and is not a rejection.

| branch | exact message after `<p>: ` | stream / ending / status |
|---|---|---|
| absent session | `session '<name>' does not exist` | stdout / LF / 1 |
| stale session for live operation | `session '<name>' is not running` | stdout / LF / 1 |
| indeterminate session | `session '<name>' could not be identified` | stdout / LF / 1 |
| create-only against live | `session '<name>' is already running` | stdout / LF / 1 |
| `rm` against live | `session '<name>' is running` | stdout / LF / 1 |
| missing log for `tail` | `no log for session '<name>'` | stdout / LF / 1 |
| submitted clear with no determinate result | `log clear outcome for session '<name>' is indeterminate` | stdout / LF / 1 |
| no controlling terminal | `no controlling terminal` | stderr / LF / 1 |
| child exited before publication | `child exited before session publication` | stderr / LF / 1 |
| log lane unavailable | `log store is unavailable` | stderr / LF / 1 |
| supervised launch rejected | `supervised launch rejected (<cause>)` | stderr / LF / 1 |
| ancestry v2 malformed | `session ancestry v2 is malformed` | stderr / LF / 1 |
| generation space exhausted | `generation space exhausted for session '<name>'` | stderr / LF / 1 |
| working directory rejected | `could not enter <path> (<cause>)` | stderr / LF / 1 |
| root rejected | `session root rejected: <path> (<cause>)` | stderr / LF / 1 |
| standard-error sink rejected | `standard-error sink rejected: <path> (<cause>)` | stderr / LF / 1 |
| event target rejected | `event store rejected: <path> (<cause>)` | stderr / LF / 1 |
| instrumentation rejected | `instrumentation rejected: <path> (<cause>)` | stderr / LF / 1 |
| child exec failed | `could not execute <path>: <error>` | stderr / CRLF / 127 |

The two closed, row-specific `<cause>` sets above are the only CLI cause substitutions. More specific controller/storage causes remain protocol enums and conformance metadata, not localized additions to these lines. Existing exact success, skip, removal, list, `current`, and tail-gap lines remain frozen. `current` outside a session is the sole empty-output failure. Help/version/informational lines use stdout plus LF; no other stream or line-ending choice is implicit.

### 13.4 What `-q` suppresses

`-q` suppresses the program's **informational** messages — session created, session killed, session removed, the removal count. It MUST NOT suppress any diagnostic accompanying a non-zero exit. A caller uses `-q` to keep success quiet; a caller that also silenced failure would have no way to learn why a session did not start, and the sinks in §4.6 and §4.7 make "did not start" the common case for a misconfigured supervisor.

---

## 14. The decisions register

Every obligation this document raised is recorded here with its resolution. One remains genuinely open, and it is open because it cannot be answered by a session holder at all; everything else is decided, and the decision is stated so that an implementer never has to infer one.

**Where a decision was a judgement call rather than a measurement, it says so.** A reader who disagrees with one can change it in one place.

### 14.1 The three product choices — closed with defaults

These had no uniquely technical answer. They are closed here as normative product choices; an implementation or operator does not override them locally.

| id | decision |
|---|---|
| **OB-24** | A finite list of exactly twenty-six breaking safety corrections, frozen below with its migration |
| **OB-6** | The unambiguous versioned `_SESSION_V2` carrier is the sole ancestry variable; revision 4 retired the legacy carrier and the dual-write migration that introduced V2 |
| **OB-1** | Reserve `.log`, `.events`, `.exit`, and `.instrument` under byte-exact comparison; reject every bare/path final component that collides |

#### OB-24 — the complete compatibility exception

The breaking corrections are exactly these twenty-six and no unnamed remainder:

1. session-root ownership and permission enforcement;
2. rejection of unknown dash-leading tokens;
3. strict numeric operands and rejection of trailing operands;
4. versioned ancestry, delimiter-safe `current`, and control-byte name rendering;
5. alias-safe reservation of `.log`, `.events`, `.exit`, and `.instrument`;
6. three-state liveness rendering and refusal by creating, removal, kill, and push operations when identity is indeterminate;
7. the distinct stale-session `is not running` diagnostic;
8. uniform `push` diagnostics without a rendezvous-path leak;
9. `-2` failing closed for an unusable sink;
10. `-S` failing closed when instrumentation is not acknowledged;
11. self-attach refusal from live ancestry rather than an inherited claim;
12. the event sink becoming a portable four-slot committed directory on every platform, rejecting non-absolute `-T` operands before mutation, with optional validated-empty creator provisioning and live-clear RPC;
13. log and lifecycle companions becoming portable committed directories with exact manifests and frontiers;
14. the coordinated controller lease, reconnect, query, status, and log-clear protocol cutover;
15. expanded status/heartbeat health plus bounded storage-worker failure and timeout behavior;
16. byte-exact help, version, option ownership, defaults, diagnostics, streams, and line endings where the prior handoff had no fixture;
17. completed viewer controls, detach timing, `NON_VT`, same-size `winch`, and headless terminal defaults;
18. the closed terminal scanner and `observer-degraded` event;
19. geometry values from 1001 through 32767, subject to the 2,000,000-cell product cap;
20. corrected signalled-exit and platform-exit branches in events and lifecycle state;
21. canonical JSON, multi-record transaction, and portable exhaustion corrections needed for deterministic durable events;
22. the prepublication child-exit outcome;
23. immutable instrumentation staging and substitution of the staged loader path;
24. delivery-control gap and dead-letter records;
25. the ten-second whole-shutdown deadline and second-notification escalation rule;
26. `clear` refusing an indeterminate possible live writer, plus an explicit indeterminate result when a submitted clear loses its durable outcome.

The supervisor changes controller and event surfaces atomically; no mixed dialect exists. Existing legacy log/exit residue is inventoried and drained before the amended reader is enabled, and no new holder adopts an old companion layout. Corrections 4 and 5 also require the inventory-and-drain migrations below. Nothing is added to this list without a decision of the same weight. Strict parity was rejected because it would deliberately reproduce known defects; each named correction closes a specific way to lose control of a session silently.

#### OB-6 — versioned ancestry carrier

The holder writes the derived `_SESSION_V2` value frozen in §4.4.1: `v2:` plus colon-separated canonical padded-base64 encodings of the absolute native rendezvous paths; entries encode the exact native bytes. Revision 4 removed the colon-joined legacy carrier entirely — it is neither written nor read, and a contradictory legacy value in the environment changes nothing. `current` and the supervisor read V2 and nothing else. This is the only option that handles arbitrary parent/base-name paths without delimiter ambiguity.

#### OB-1 — reserved suffix grammar

The reserved suffixes are exactly **`.log`**, **`.events`**, **`.exit`**, and **`.instrument`**. §2.1 owns the only path mapping: `.log` and `.exit` are appended to the rendezvous final component, the fully qualified absolute `-T` event path is used exactly as supplied, and the immutable stage remains `<H>.instrument` in the owner-only root. The restriction applies to the final native path component of **both bare and path-form session names**; reservation of `.events` does not derive or rewrite an event path.

The suffix comparison is byte-exact: `.LOG` is a session name, `.log` is a reserved artifact suffix, and Moor never changes the caller's spelling into a different session name. The type and OB-17 identity rechecks still apply to every opened object and close aliases that cannot be excluded lexically.

The grammar is extensible only by a decision of this weight. Before enforcement, inventory and deliberately drain suffix-colliding sessions under the byte-exact comparison above. This keeps the on-disk layout flat and legible and avoids moving companion files without removing their ambiguity.

### 14.2 Engineering decisions — closed

| id | resolution |
|---|---|
| **OB-2** | A comparable monotonic age uses the largest whole unit that fits, rendered in `<age-text>` as `<n>s ago`, `<n>m ago`, `<n>h ago`, or `<n>d ago`, truncated toward zero. When boot identity is unavailable/different or monotonic subtraction would be negative, `<age-text>` is exactly `unknown`; wall-clock subtraction is never substituted |
| **OB-3** | Numeric operands are canonical decimal with no sign, leading zero except `0`, or trailing byte. `-C` alone permits one ASCII-case-insensitive `k/m/g` suffix with 1024 multipliers and checked u64 multiplication. `tail -n` is unsuffixed u32, including zero (§3.6) |
| **OB-4** | After a command token, options may surround the session until the first child-command operand. The bare form consumes the session first and then recognizes options until the child command. `--` ends recognition and may introduce a dash-leading session. Every spelling and phase has a vector |
| **OB-5** | Resolved in §4.2: the complete terminal settings are transferred |
| **OB-7** | The per-viewer output buffer is bounded at **4 MiB of child payload** across replay and live output. Pinned replay records count once for that viewer; framing metadata is separately fixed-size. A viewer that exceeds the payload bound is disconnected; the session is unaffected (§5.1, §6.7) |
| **OB-8** | `list` bounds the whole operation at **2 seconds**, probing concurrently. Anything unresolved within it renders `[indeterminate]` |
| **OB-9** | §7.4's lifecycle directory selects one canonical running/exited JSON record with exact key order, native-path manifest, platform exit branch, and final output coordinate; lifecycle commits before event exit and rendezvous removal |
| **OB-10** | `-T` names a fully qualified absolute native path used exactly as supplied for a portable four-slot directory. It may be absent for exclusive creation or the exact validated empty caller-created directory; slots are always Moor-created exclusively and no predecessor state is adopted |
| **OB-11** | §8.4.2's exact 92-byte portable commit selects between `body.0/body.1` on every platform by CRC, hash, kind, coordinates, and greatest valid index. Every crash prefix leaves the prior or one submitted candidate selectable, never a guessed fragment |
| **OB-12** | A `link` snapshot restores "the most recent hyperlink seen", nothing more; a consumer needing every hyperlink must consume faster than the cap (§8.4.4). Synthetic snapshots consume sequence numbers exactly as transitions do, so the sequence stays dense. If the header, snapshots, and complete ordered transition set together exceed the cap, the cap is exceeded for that one compaction rather than dropping any transition — the invariant that an admitted transaction survives without a partial prefix outranks the byte bound. OB-28 separately permits only its bounded terminal transaction to exceed the cap; after it commits, no later record is admitted |
| **OB-13** | A `state` snapshot carries the **last published** title, not the last observed one. A snapshot is defined as restating published knowledge (§8.4.1); carrying an unpublished title would make compaction the only way to learn it, which is the opposite of a snapshot's purpose |
| **OB-14** | Delivery-control schema 1 emits §8.6's exact canonical `gap` JSONL record before resync, copying session/generation/epoch and naming the maximal inclusive missing `first_seq..last_seq` range without consuming a Moor sequence or advancing its cursor |
| **OB-15** | A title is bounded at **255 bytes**, a link target at **2048 bytes**, truncated at a UTF-8 character boundary so the JSON line stays well-formed. Truncation sets a flag on the record. Invalid UTF-8 and embedded NUL are replaced with the Unicode replacement character before bounding — never dropped silently, never emitted raw |
| **OB-16** | The launcher passes a **private inherited descriptor** whose other end it holds. `<BASENAME>_LAUNCH_CHANNEL` selects it, and the exact 32-byte record plus EOF, 2-second deadline, generation check and stripping rules are frozen in §10.1.1 and wire §15.1. Its valid presence marks a supervised launch; a selector alone never does |
| **OB-17** | Canonical session identity is tagged: `01` plus the lexically resolved absolute socket-path bytes without symlink following. The tag is part of every comparison |
| **OB-18** | The **supervisor** owns the durable generation allocator, keyed by its non-recycled durable logical session key, not by the replaceable live rendezvous identity. Its first value is 2 because wire generation 1 is reserved for unsupervised holders; later values strictly increase. The logical key survives Moor `rm`, failed-launch cleanup, and ordinary same-name recreation. The holder is told its generation (§10.1) and never allocates. The store lives beside supervisor state, is written before launch, and is recovered by reading it; an unreadable store is refusal, not reset. Adoption later binds that generation to OB-17 identity and holder incarnation. Only an explicit supervisor operation that retires the whole lineage and its stored bindings may assign a fresh key after exhaustion |
| **OB-19** | Both dimensions zero preserve; exactly one zero is malformed. Each real dimension is `1..32767`, widened product at most `2,000,000` (§4.3, §10.2.8) |
| **OB-20** | The opt-out is a single environment variable named for the program, suffixed `_NO_TERM_AUTORESPONSE`, following §4.4.1's derivation. **Any non-empty value** counts as set. It suppresses the synthetic replies only; the environment identity of §4.4.2 is unaffected, because a child that believes it is talking to a terminal that then does not answer is worse off than one told nothing |
| **OB-21** | Resolved in §11.3 and §11.4 |
| **OB-22** | `-S` copies the one validated caller handle to §4.7's identity-bound immutable `.instrument` stage and passes only that stage path. The module signals load over a second private inherited channel; exact ACK/EOF/PID/generation/nonce rules prove only requested-process initialization |
| **OB-23** | The exact OSC/query scanners and resynchronization are §9.4. A 65,537th OSC byte, cancellation, malformed sequence, or recognition deadline starts a degradation episode; one transition-only `observer-degraded` record reports its scanner/reason, while status/heartbeat carry observer exactness if events cannot |
| **OB-25** | Delivery failures key by the exact source identity and record hash. Counts one/two persist without cursor advance; on three, one downstream transaction atomically stores §8.6's exact canonical dead-letter JSONL record and advances. Failure leaves the cursor, count, and source record retryable; dead letters persist until explicit disposition or lineage retirement |
| **OB-26** | "Restarting the holder" is **not** required and is not achievable — the holder owns the pseudo-terminal and no successor can reopen it by name. §5.5's conformance evidence means restarting the **supervisor** while the holder keeps running, and observing that adoption re-establishes correct state and that a superseded generation is refused |
| **OB-27** | Resolved in §11.6 |
| **OB-28** | Event `seq` (2⁵³−1), epoch (2³²−1), and portable commit index (2⁶⁴−1) never wrap. The entire transaction is preflighted with `seq`, then `epoch`, then `commit` precedence on every platform. Sequence refuses the complete ordered transition set and uses the reserved diagnostic position; epoch commits that complete set plus the diagnostic in the maximum epoch; commit publishes any required snapshots, the complete set, and the diagnostic at `FFFFFFFFFFFFFFFF`. No prefix commits alone. The one final transaction may exceed the cap; recovery never resumes after it |
| **OB-29** | One encoding contract for opaque names: native path representation on native surfaces (POSIX bytes), padded base64 for tagged identity in JSON (§8.4.1.1), padded base64 per native-path entry in `_SESSION_V2` (§4.4.1), and exact reversible ASCII on line-oriented human surfaces (`list`, diagnostics, `current`): `[A-Za-z0-9._/-]` unchanged, every other byte as uppercase `\xHH`. Width counts rendered bytes. No name can inject a delimiter or line, and no surface silently drops a byte |
| **OB-30** | `WAKEUP` coalescibly signals durable event advance. Five-second `HEARTBEAT` flags report child, event, log, lifecycle, and observer health and queue immediately on change. Fifteen seconds of absence triggers a fresh bounded probe and leaves the session indeterminate until resolved; silence on `WAKEUP` means nothing |
| **OB-31** | **Both**, not either: wall-clock start, monotonic start and boot identity. Linux uses `CLOCK_BOOTTIME` plus the parsed kernel boot UUID; macOS uses `mach_continuous_time` plus the frozen `kern.boottime` encoding. An unavailable all-zero identity never compares equal, so age becomes unknown (§12.6) |
| **OB-32** | Resolved as the `-d <path>` option in §3.5, with its own failure diagnostic distinct from a child that could not be executed |
| **OB-33** | Five outcomes — terminated, already gone, refused on identity, indeterminate, failed — encoded in `TERMINATE_RESULT`; 5-second escalation, 10-second whole-operation deadline, exceeded is `INDETERMINATE` |
| **OB-34** | Resolved in §12.4: foreground/child process groups with detached descendants excluded |
| **OB-35** | Child process identifier, 4-byte containment token, and 16-byte birth token. The containment token is the pgrp |
| **OB-36** | `INPUT_RECEIPT` remains exactly transport-written/refused. Wire v4 optionally binds the input to an application request id and semantic source, and requires a prepared `INPUT_NOTICE` before the PTY write. That enables, but does not fabricate, OB-37 evidence |
| **OB-38** | §7.4 commits exactly one lifecycle `exited` replacement for an observed child end and §8.2 commits one event `exit` only while event storage remains writable; holder loss uses OB-30 and never fabricates child exit |
| **OB-39** | `ATTACH_ACK` and `STATUS_REPLY` carry §10.2.5's complete descriptor: identity/incarnation, portable event frontier, starts/boot/cwd/child, replay/tracked exactness, fully-attached/child/event flags, store/observer/query health, and selected log epoch/index/range. Wire v4 still claims no screen checkpoint |
| **OB-40** | Closed as a known parity defect retained in version 1. §4.4.2 stands as written: nothing is removed from the environment, so a reattached session carries the viewer identity of the terminal that created it. This is deliberately retained, not left undecided; a version 2 may revisit it after surveying real terminal-emulator instance variables. It is not on OB-24 because version 1 changes no behavior |
| **OB-41** | Resolved in §3.3: refusal decided from live process ancestry; the session variable stays descriptive |
| **OB-42** | One ten-second monotonic deadline starts at the first accepted termination flag. Normal wake abandons peer/store waits, graceful termination starts immediately, five seconds escalates, and ten seconds closes handles/exits while retaining uncertain evidence. A second notification escalates but resets nothing; no response, flush, callback, diagnostic, or join extends either deadline (§12.3) |

### 14.3 Still open — one, with its owner

| id | why it cannot be closed here | owner |
|---|---|---|
| **OB-37** | Wire v4 and §10.3 now provide the application-correlation carrier, provenance, durable event and loss/recovery behavior. The remaining gate is **real provider authority**: Moor cannot make a provider read or act, and a frame named `APPLICATION_RECEIPT` is not proof that any given provider can truthfully emit it. Closure is therefore per provider/version only after shipped end-to-end runtime conformance at the named authoritative point | **The supervisor specification jointly with each provider integration.** Until the deployed provider passes that gate, the receipt path is not authoritative for that provider. No global closure follows from this document |

### 14.4 How this register is maintained

A decision changes here and nowhere else. A new deferral anywhere in this document adds a row to §14.3 in the same edit, with an owner that is a section able to supply the answer — never one that merely consumes it.
