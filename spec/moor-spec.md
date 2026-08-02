# moor — behavioural specification for an independent implementation

**Status:** implementation-handoff candidate. **Every section is written and normative.** §14 is the decisions register: it records the resolution of all 42 obligations, and names the one downstream runtime gate that remains open together with who owns it. Wire schema version 3 and event schema version 2 accompany this document and are the artefacts an implementer builds against (§0.2).
**Audience:** the implementation team. You will build a program that replaces the existing one completely.

**The program is called `moor`.** A mooring is not ownership and not a launch: it is what holds something in place while others come alongside and cast off. That is exactly this program's job — the child lives on its own, viewers attach and detach, and nothing changes when they leave.

**The name is load-bearing, not decoration.** §2.2 derives the session root and §4.4.1 derives the environment keys from the invoked base name, each with its own frozen transformation, so a copy invoked under a different name gets a different root and different keys — by construction, not by special case. The distribution therefore **MUST install the same executable under both names**: `moor` is the canonical entrypoint and `atch` is the compatibility entrypoint. Invoking the compatibility name yields byte-identical legacy identifiers, so callers written against the old name keep working without a translation layer or an entry on OB-24's list. Nothing is renamed for them; they invoke the name they always invoked.
**Author:** source-exposed specification team.

---

## 0. How this document is used

This is the *only* description of the program you are asked to build. It is written from observable behaviour: what the program is given, what it must produce, and which properties must hold. It deliberately contains no source code, no function or file names, and no line references from any existing implementation. If you find yourself wanting one, the specification is incomplete — ask, and it will be extended, in behavioural terms.

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

### 0.1.1 Implementation size ceiling **[normative]**

The complete first-party production implementation is capped at **5,000 source lines across all components in aggregate**. The count includes every nonblank, non-comment physical line in every first-party source file compiled, interpreted, packaged, or executed as part of Moor: command surfaces, holder, controller/client code, semantic transport, platform backends, Windows bootstrap/insertion helpers, libraries, and build-time source that implements runtime behaviour. Generated production source counts at its generated line total; moving handwritten logic into a generator or helper does not remove it from the budget. Normal compiler, linker, and package-manager outputs are not source.

Tests, conformance fixtures, this specification and its documentation, and unmodified vendored third-party dependencies are outside the 5,000-line count. A modified vendored file becomes first-party for this rule. Code MUST remain normally formatted; minifying it or combining independent statements solely to evade the line budget is nonconforming. Every release candidate reports the per-file count and aggregate total together with its conformance evidence.

The cap is an architecture constraint, not permission to omit behaviour. Every requirement in this document still applies; if an implementation cannot satisfy both the contract and the cap, its architecture must be simplified rather than its conformance claim narrowed silently.

### 0.2 Compatibility with what exists today

A working implementation already runs in production and callers are built against it. Compatibility is **not uniform across callers**, and pretending otherwise makes this document unimplementable:

- **External callers — humans at a shell, scripts, anything on-disk** — are unchanged **except for the twelve corrections enumerated under OB-24**. Same command lines, same layout, same exit codes everywhere else. This bullet once said "they must never learn that the implementation changed"; that was an absolute the document could not keep, and OB-24 replaced it with a finite, named list.
- **The supervising daemon is part of an atomic, coordinated cutover.** It ships in the same release as the new holder and speaks the full protocol from the first moment. There is no mixed-version production window, no negotiating down to a partial dialect, no permanent fallback, and no dual runtime after cutover.

The second bullet exists because the frozen protocol requires exchanges the current supervisor does not perform. A specification that demanded both "the full protocol" and "no caller changes" would force the implementer to violate one of them silently — and they would pick the one that is not tested.

The compatibility bar is not "works for the common case". It is: for a given input, the new program and the reference produce the same observable output, byte for byte, wherever this document says so. Conformance vectors accompany this specification for the parts where "the same" must be exact.

Where this document requires behaviour that the current implementation does **not** have, it is marked **[NEW]**. Those are deliberate corrections of defects found in production; they are requirements, not optional improvements, and each states the failure it prevents.

**Several `[NEW]` clauses diverge from the first bullet above, which is why that bullet no longer states an absolute.** A root with permissive mode now fails where it succeeded (§2.2); an unknown `--option` now fails where it created a session (§3.1); malformed numeric operands and trailing operands now change status and output (§3.6). Each is a defensible safety correction and each is visible to an external caller — so "external callers are unchanged" and "these corrections ship" cannot both be true. Marking a clause `[NEW]` records the divergence; it does not grant an exemption from a guarantee stated as absolute.

**OB-24 resolved this**: the first bullet permits a finite, enumerated list of breaking safety corrections with a stated migration, and §14.1 enumerates all twelve. Strict parity was the alternative and was rejected, because it would mean deliberately reproducing known defects in a program written from scratch to remove them. What was never available was leaving both sentences in the document and letting the implementer discover the conflict.

**One cutover obligation is recorded here so it is not lost between documents.** The supervisor resolves which holder binary to run in a fixed order, and the order is exact:

1. **`DESK_MOOR_BIN`**, with surrounding whitespace trimmed. If it is non-empty, it is the answer or there is no answer: a value that does not name an executable regular file is a **fatal error, not a reason to try the next candidate**. An operator who points this variable somewhere has stated an intent, and silently running a different binary than the one they named is the worst available outcome — it is indistinguishable from success.
2. The holder shipped in the same release, at `libexec/moor` relative to the supervisor's own installation.
3. An **absolute** path obtained by resolving the name against the search path.

A bare name resolved at run time is never used. An atomic cutover depends on that order: a bare name would let a stale copy elsewhere on the machine answer for the new protocol, which is precisely the mixed-version window §0.2 forbids. This belongs to the cutover's definition of done rather than to the program's own behaviour, but it is a condition of the guarantee above.

---

## 1. What the program is

A session holder for terminal programs. It runs a child program under a pseudo-terminal, keeps that child alive independently of any viewer, and lets viewers attach and detach at will. A session outlives the terminal that created it: closing the window, losing the network, or killing the viewer leaves the child running.

Compare it to a terminal multiplexer with everything removed except this one job. There are no windows, no panes, no status bar, no configuration language, no scrollback UI, no copy mode.

### 1.1 The transparency guarantee — the defining property

**The program MUST NOT interpose its own terminal emulator between the child and the viewer.** On Linux and macOS this is literal byte transport over a pseudo-terminal. On Windows the platform pseudo-console is necessarily a terminal boundary: ConPTY translates legacy Console API calls into a UTF-8 virtual-terminal stream and translates virtual-terminal input back into console input. Moor MUST pass the bytes at the ConPTY boundary unchanged and MUST NOT add another parser, renderer, or normaliser. A legacy Win32 console application therefore is not promised byte identity with its Console API calls; a VT-native child is promised byte identity from the ConPTY byte boundary onward.

Bytes the child writes reach the attached viewer unaltered and in order. Bytes the viewer types reach the child unaltered and in order. The program does not rewrite or normalise the stream in either direction. Its only parsing or side-channel copies are the narrowly scoped cases this document names explicitly: detach-key detection (§6), logging (§7), terminal-state observation and bounded mode tracking (§9), and capability arbitration (§10). None changes the child's output bytes delivered to a viewer.

This is the property that makes the program worth building, and it is the property most easily lost. Consequences that MUST hold:

- Mouse reporting, bracketed paste, focus events, and every other private mode work exactly as they do without the program in the path.
- Application cursor keys, alternate screen switching, and scroll regions are the child's business; the program has no opinion.
- Colour depth is not reduced. Sixel, Kitty graphics, and any other byte sequence the viewer's terminal understands pass through.
- A program that queries the terminal (cursor position, device attributes, colour palette) receives the lease-holding *viewer's* answer when that viewer responds within the arbitration deadline. Only after that opportunity may the holder answer one of its frozen synthetic classes, and only when it supplied the terminal identity itself (§10.2.7).
- No sequence is emitted into the child's input stream that the viewer did not type, except in the two cases below, which are the *only* permitted exceptions and are specified normatively elsewhere:
  1. **Redraw on attach** (§6), when the operator has opted in.
  2. **Terminal capability arbitration** (§10). A child that asks the terminal what it is must receive an answer even when no viewer is attached, or headless terminal programs hang or degrade. **Two classes are excepted and receive no synthetic answer**: a cursor-position query, which only something tracking the cursor can answer (§9.1), and any identity query when the terminal identity was inherited rather than supplied by the holder (§4.4.2) — in both cases the holder would have to invent the answer, and silence is honest where invention is not. Exactly one responder answers a given query: the attached viewer when there is one able to answer within the deadline, otherwise the holder answers on a frozen, documented set of query classes. An observer never answers. A reply that is unsolicited, duplicated, of the wrong class, or belonging to a superseded generation or lease MUST NOT reach the child. The exact byte sequences, the query grammar, the opt-out, and the environment the holder presents are frozen in §10.

*This exception is load-bearing and was nearly lost.* An earlier draft of this document forbade synthetic replies outright, which would have removed a mechanic the product depends on: without it a headless terminal program receives no answer to its capability query and either stalls or falls back to a degraded rendering mode. Transparency means the program does not *alter* the stream; it does not mean the program is absent from it.

A separate viewer-only exception to byte-for-byte viewer delivery exists and MUST be stated, because omitting it makes the conformance test below untrue: on attach the holder sends the viewer a **terminal-state preamble** restating the modes the child established before that viewer arrived when its bounded tracker remains exact (§5.2). It is not a third exception to the child-input rule above: none of its bytes may reach the child. When tracking is inexact the required frame is empty and the acknowledgement says so rather than emitting guessed controls. Preamble bytes were never written by the child. They are addressed to the viewer only, are not part of the child's output stream, and MUST NOT be logged or advance any output cursor. Without an exact preamble a viewer that arrives mid-session may render the session wrongly, and the degraded flag makes that limitation explicit.

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
- **A path** (contains a separator) identifies the session's filesystem rendezvous object at exactly that location: a Unix-domain socket on Linux and macOS, and a marker file naming a protected local byte-stream pipe on Windows (§12.2). The program MUST NOT create parent directories for this form; if the parent does not exist, the operation fails (§13). This is the form an automated supervisor uses, because it places sessions in a directory it controls.

Names MUST be treated as opaque native path values. Two names are the same session only when their **tagged canonical identities** match (OB-17): tag `01` plus the lexically resolved absolute socket-path bytes on Linux and macOS; tag `02` plus the Windows marker's volume serial number and 128-bit file identifier, queried from its same-directory staged file and required to be unchanged after atomic publication. The tag is part of the identity. A Windows spelling, case fold, alias, or reparse target is never substituted for the marker's file identity.

**[NEW] Naming must not let one session destroy another.** The program keeps companion files beside a socket, named by appending a suffix to the session name. That makes the name space self-colliding: a session legitimately named so that it *is* another session's companion file exists today, and the reference has been observed to remove a **live** session's socket while acting on a different name — reporting success, leaving the holder and its child running with no reachable socket and no way for the user to get back in.

No operation on one session may unlink, truncate, or otherwise disturb another's socket or files, for any pair of names. Meeting this requires a deliberate decision, and that decision belongs in this document, not to the implementer. Whichever is chosen, an operation MUST re-verify the target's type and identity immediately before removing it, never act on a name alone.

**There are three candidate shapes, not two.** An earlier pass of this document offered "a reserved suffix, or a namespace that cannot collide" — and then, while explaining the second, included a reserved directory that session paths are forbidden to name. That is a restriction, filed under the label that promises none. Two materially different products were hidden behind one word, so a decision to take "the option that restricts nothing" would not have determined what gets built. They are separated here:

| shape | what it restricts | how the collision becomes impossible |
|---|---|---|
| **reserved suffix** | the final component of every bare or path-form session name may not collide with a reserved suffix | a companion file's name can never be a legal session name |
| **reserved state root** | path-form session names may not designate anything inside a reserved directory | companion files live where no session may be placed |
| **non-addressable carrier** | nothing | companion state is not reachable by any filesystem path a caller can name |

A subdirectory *without* the accompanying prohibition is not a fourth option — it is simply broken. §2.1 accepts a path-form name designating a socket at exactly the path given, so a caller can place a session **inside** the directory meant to hold companion state and the collision returns by another route. The prohibition is what makes the second shape work, and the prohibition is the restriction.

**All three are externally observable, so all three depend on the compatibility boundary.** The first makes a name that works today fail at creation. The second does the same for a path-form name. The third moves companion files, so anything opening one by path stops finding it — and §0.2 promises external callers the same layout, not merely the same commands. Under strict parity **none** of the three closes this defect, exactly as none of the ancestry encodings fixes the colon (§4.4.1). The compatibility boundary decides whether this obligation can be discharged at all.

**Choosing a shape does not discharge this obligation on its own.** Each carries a migration that must be chosen with it, and a session does not end by itself — a holder lives until its child exits (§4.5) and nothing a peer does may end it (§5.1), so nothing here ages out.

- The two restricting shapes need: an inventory of names that become illegal, and then one of — drain them deliberately, tolerate them indefinitely, or require that none remain before the new version is live.
- The carrier shape needs: which component owns the old files and which owns the new, whether the transition reads or writes both, what happens when the two disagree, when the old location is abandoned, and who removes what is left. A holder from before the change can keep writing to the old location for as long as it runs, so moving files races a live writer, leaving them preserves the collision for those sessions, and ignoring them loses `tail` and the exit records.

This obligation is closed only when a shape **and** its migration are both written here.

### 2.2 The per-user session root

Bare names resolve inside a directory that is private to the invoking user.

**The location is frozen per platform.** On Linux and macOS, verified against the reference: the system temporary directory, containing a directory named with a leading dot, the program's invoked base name **exactly as invoked**, a hyphen, and the invoking user's numeric id — `/tmp/.<invoked-basename>-<uid>` — created with owner-only permissions, `0700`. On Windows the base directory is the exact result of `GetTempPathW` (including its documented `TMP`, `TEMP`, `USERPROFILE`, Windows-directory precedence), followed by `.<invoked-basename>-<string SID>`. Moor validates existence and access rather than assuming `GetTempPathW` did. The final root is opened without following a reparse point, MUST NOT itself be a reparse point, is owned by the invoking user's SID, and has a protected DACL granting full control only to that SID and `LOCAL_SYSTEM`; inherited ACEs and every other allow ACE are forbidden.

What is invariant everywhere is the property, not the spelling: **the root is reachable only by the invoking user and the operating-system account that must administer it** (`LOCAL_SYSTEM` on Windows). A copy invoked as `mo-or.probe2` uses `/tmp/.mo-or.probe2-1000` on POSIX: hyphens and dots are preserved, nothing is case-folded, nothing is substituted.

**This is not the same derivation as the session variable**, and an earlier pass of this document said it was. The root uses the raw base name; the environment key applies the byte-level transformation frozen in §4.4.1, which is a different function and is not restated here — a summary of it in this section would be a second definition able to drift from the first, which is how the "same derivation" error arose. They must be implemented as two transformations of one input, never as one shared helper: the shared-helper mistake produces a program whose root and whose environment variable disagree about what session it is in.

**[NEW] The root's ownership and protection are enforced, not assumed.** Verified against the reference: a pre-existing POSIX root owned by the caller but with permissions `0755` is adopted silently, the mode is left as it is, and the command succeeds. The replacement MUST refuse to operate on a root that is not a directory, is owned by another identity, is a symbolic link or reparse point, or has permissions broader than the exact platform rule above. It exits non-zero with a diagnostic naming the path and offending attribute (§13.1). It MUST NOT repair the protection: silently tightening it hides that somebody else created the directory, and the fact that they did is the thing worth knowing.

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

**Which tokens are session names.** The first operand is a session name unless it is exactly one of the command tokens or legacy mode tokens in §3.6. There is no lookup, no prefix matching, and no fuzzy correction: an unrecognised word is a session name, which is why `moor mysession` works at all.

**[NEW] A token beginning with `-` is never an implicit session name.** Verified against the reference: an unrecognised single-dash token is rejected as an invalid mode, but an unrecognised *double-dash* token is silently accepted as a session name — so a mistyped long option creates a session named after the typo instead of reporting the typo. The two spellings MUST behave the same, and that behaviour MUST be rejection. A caller that genuinely wants a session name starting with `-` introduces it with `--`. *Failure prevented:* a typo that silently launches a shell nobody knows about, holding a pseudo-terminal until the machine is rebooted.

### 3.2 Session-creating commands

| command | creates | attaches | holder runs |
|---|---|---|---|
| `new <session> [command...]` | yes | yes | background |
| `start <session> [command...]` | yes | no | background |
| `run <session> [command...]` | yes | no | **foreground** |
| `attach <session>` | no — fails if absent | yes | — |

Requirements common to the creating commands:

- If `command...` is omitted, the exact candidate order is: nonempty `SHELL`; then, on Linux/macOS, the nonempty shell field returned for the invoking uid by the system account database, otherwise `/bin/sh`; on Windows, nonempty `COMSPEC`, otherwise `cmd.exe` in the directory returned by `GetSystemDirectoryW` (§12.7). Each value is one native executable path, never a command line. The first nonempty candidate is authoritative: if it cannot be executed, startup fails with 127 rather than silently selecting a different shell. The shell is **not** started as a login shell and no login flag is passed. (Verified against the reference; an earlier draft of this document said "login shell", which would have changed startup-file behaviour for every session.)
- A **create-only** command — `new`, `n`, `-c`, `start`, `s`, `-n`, `run`, `-N` — MUST fail against a session that is already live, rather than replacing it. The **create-or-attach** forms — the bare form and `-A` — attach instead, which is their purpose; they are not exceptions to this rule but a different operation (§3.6, §3.7).
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
- `list [-a]` — enumerate sessions in the per-user root (§2.2) only; sessions addressed by path are not listed. Discovery is a **union of two independent sources** (§4.5): the addressable rendezvous objects present in the root (POSIX sockets or Windows markers), and the durable exit records — each source enumerated on its own, because either artefact may exist with or without the other. The two are then merged by name and classified by the cross-product below, which also fixes what each of `list`, `list -a` and `rm` does with every combination. An empty result prints exactly `(no sessions)` and exits **0**.

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

  **[NEW] The full cross-product.** A rendezvous object may be present or absent; an exit record may be present or absent. All four combinations occur — a holder that crashed after writing its record but before unlinking its socket or marker, a cleanup interrupted midway, a new session created over an old record.

  | rendezvous object | exit record | liveness | `list` | `list -a` | what `rm` removes |
  |---|---|---|---|---|---|
  | present | absent | probed (§2.3) | live / `[stale]` / `[indeterminate]` | same | the rendezvous object, only if `stale` |
  | present | present | **probed — the rendezvous decides** | live / `[stale]` / `[indeterminate]` | same | rendezvous object **and** record, only if `stale` |
  | absent | present | stale | *not shown* | `[exited]` | the record |
  | absent | absent | the name does not exist | — | — | nothing; `does not exist` |

  Three things in that table are **choices this document is making**, not facts read off the reference, and they are labelled as such because the reference has no behaviour here to copy:

  - **The rendezvous decides when both exist.** It may have a live holder behind it; an exit record is by definition historical. Trusting the record would report a running session as finished — the one error that cannot be recovered by looking again.
  - **A combined entry renders identically to a rendezvous-only one**, in both `list` and `list -a`. The record adds no information the probe did not already establish, and a distinct rendering would expose an internal artefact as though it were a session state.
  - **Removal takes both artefacts.** Removing one and leaving the other is how the residue becomes unremovable: the next `rm` finds the survivor and reports it as a fresh entry.

  Shape is never a fourth liveness state. Nothing may be attached to, delivered to, or killed differently because of which artefacts exist; shape decides only discovery, rendering, and what removal unlinks.

  **`[indeterminate]` is a new word in a parsed output stream**, and §0.2 requires that to be declared rather than slipped in. It is added because the alternative is worse: today such a POSIX socket is rendered as `[attached]`, which tells a supervisor that a session it does not own is one of its own working sessions (§2.3). A caller that does not know the word sees an unrecognised status, which is the correct outcome — better than a recognised and wrong one.

- `current` — print the session the invoking process is running inside. Sessions nest, and the command prints the **whole ancestry**, outermost first, not only the innermost. Outside any session it fails with exit **1**; verified against the reference, it prints nothing at all in that case, and this document freezes that silence rather than adding a diagnostic — the shell idiom `if name=$(… current); then` depends on the empty capture. Attaching to the session one is already inside, or to any of its ancestors, MUST be refused rather than producing a loop.

  **[NEW] That refusal is decided from live process ancestry, not from the environment (OB-41).** The session variable is inherited by every descendant and is never cleared, so a process whose ancestor session has long since ended still carries it — and deciding from the variable then refuses an attach that would have been perfectly safe. The holder walks the actual process ancestry of the caller and refuses only when a live holder for the target session is genuinely among its ancestors. The variable stays descriptive: it tells a program where it is, it does not decide what may be attached.

  Verified against the reference, the rendering is the **final path component** of each generation, joined outermost-first by the three characters space-greater-space: a session `curtest` prints `curtest`, and a session `inner` created inside `outer` prints `outer > inner`. Trailing operands are accepted and ignored, which §3.6 corrects.

  The ancestry is carried in the child's environment (§4.4), and its encoding is frozen there. It is not free choice: `current` is the parser of whatever §4.4 writes.

### 3.4 Input and log commands

- `push <session>` — read the invoker's standard input and deliver it to the child as if typed. Terminates when standard input reaches end of file. This exists so an automated caller can inject input without holding a terminal. **[NEW]** Naming a session that does not exist MUST produce the same diagnostic shape as every other command (§13). Verified against the reference, this one command instead surfaces a raw system error carrying the absolute socket path, disclosing the session root's layout and breaking the uniformity a caller matches on.
- `tail [-f] [-n N] <session>` — print the last `N` lines of the session log (§7), default 10; with `-f`, continue printing as the log grows. See §3.6 for the numeric grammar of `N`, which is **[NEW]**.
- `clear [<session>]` — truncate the session log to empty without disturbing the session. Naming a session that does not exist succeeds silently with exit **0**. Verified against the reference, and **deliberately frozen rather than corrected**: `clear` asserts an end state ("this log is empty") which already holds, and blind `clear` calls in cleanup scripts are the expected use. This is the one place where the inconsistency with `tail` and `kill` is intended; an implementer who "fixes" it breaks callers.

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
| `-C <size>` | cap the session log (§7). `0` disables logging. Accepts a plain byte count or a suffixed form such as `128k` or `4m`. Default is one mebibyte. Numeric grammar in §3.6. |
| `-2 <path>` | send the child's standard error to the named file instead of the pseudo-terminal (§4.6). |
| `-T <path>` | write the bounded event stream (§8) to the named sink path (§8.1). |
| `-S <path>` | load the named launch-time instrumentation object into the initial child before its first application instruction (§4.7). |
| `-d <path>` | **[NEW, OB-32]** run the child with this working directory. |

The working-directory option is **[NEW]**: the current program accepts an argument vector and no directory, so every automated caller wraps its child in a shell that changes directory first — adding a process to every session, changing which process receives terminal-generated signals, and making a failed directory change indistinguishable from a failed command. The path must be an existing directory the invoking user can enter; if it cannot be entered the session is not created and the diagnostic names the directory, distinctly from a child that could not be executed (§13.1). Without `-d` the child inherits the creating process's directory, as today.

**Option placement is a three-phase grammar, and an earlier pass of this document stated it wrongly.** That pass claimed options are recognised only before the operands and that a token after the session name goes to the child. It based this on a probe that passed `-q` to `/bin/true`, which ignores its arguments — so the probe could not distinguish "consumed as an option" from "handed to the child", and it was recorded as if it could.

Verified with an option whose effect is observable:

| placement | result |
|---|---|
| before the command token — `-T <path> start <session> …` | **rejected**, `Invalid mode '-T'`, exit 1 |
| between command and session — `start -T <path> <session> …` | accepted, option takes effect |
| after the session name — `start <session> -T <path> …` | **accepted**, option takes effect |

**That table is the whole of what is currently established, and it was established with one option.** An earlier pass generalised it into a parser algorithm — "options are recognised after the command token, on either side of the session name; the child's arguments begin at the first non-option token; `--` terminates option processing" — which is a plausible reading of three data points about `-T` and is not a measurement. Options that take no value, options with attached values, options repeated in two phases at once, and the legacy and bare forms were not tested at all, and there is no reason to assume they share `-T`'s phases.

The table is the measurement; **OB-4 is the decision built on it**: options are recognised after the command token and on either side of the session name, the child's arguments begin at the first token that is neither an option nor an option's value, and `--` ends option processing in every phase — for modern commands, legacy modes and the bare form alike. Every spelling in §3.6 gets a vector at each of the three positions, which is what turns the generalisation from a guess into a tested rule.

### 3.6 The frozen token grammar

The reference accepts more spellings than its help text lists. All of them are in production use and all of them MUST be implemented.

**Command tokens.** The legacy tokens are **not** aliases of the modern commands. Grouping them into equivalence sets is wrong, and an earlier pass of this document did exactly that — it filed `-i` under attach when `-i` is `current`, and `-n`/`-N` under `new` when neither attaches. Every token therefore gets its own row, and every row gets its own differential vector.

Behaviour was established black-box against the reference, under a real pseudo-terminal where attaching required one.

| token(s) | operands | session missing | session live | attaches | holder | announces on create |
|---|---|---|---|---|---|---|
| `attach`, `a`, `-a` | name only — a command is `Invalid number of arguments` | fail `session '<name>' does not exist`, **1** | attach | yes | — | — |
| bare `<name>` (§3.1) | name, optional command | create, then attach | attach | yes | background | `session '<name>' created` |
| `-A` | name, optional command | create, then attach | attach | yes | background | **silent** |
| `new`, `n` | name, optional command | create, then attach | fail, **1** | yes | background | `session '<name>' created` |
| `-c` | name, optional command | create, then attach | fail, **1** | yes | background | **silent** |
| `start`, `s` | name, optional command | create | fail, **1** | no | background | `session '<name>' started` |
| `-n` | name, optional command | create | fail, **1** | no | background | **silent** |
| `run` | name, optional command | create | fail `session '<name>' is already running`, **1** | no | **foreground** | silent |
| `-N` | name, optional command | create | fail `session '<name>' is already running`, **1** | no | **foreground** | silent |
| `push`, `p`, `-p` | name only | fail, **1** | — | — | — | — |
| `kill`, `k` | name only, accepts `-f` | fail, **1** | terminate | — | — | `session '<name>' stopped`; with `-f`, `session '<name>' killed` |
| `-k` | name only, **rejects `-f`** | fail, **1** | terminate | — | — | as above |
| `list`, `l`, `ls`, `-l` | `-a` | — | — | — | — | — |
| `current`, `-i` | none | — | — | — | — | — |
| `rm` | `-a`, optional name | fail, **1** | refuse (§3.3) | — | — | three frozen forms, §3.3 |
| `clear` | optional name | succeed, **0** (§3.4) | truncate | — | — | — |
| `tail` | `-f`, `-n N`, name | fail `no log for session '<name>'`, **1** | — | — | — | — |
| `--help`, `-h`, `?`, no arguments | none | — | — | — | — | — |
| `--version` | none | — | — | — | — | — |

Three distinctions in that table are easy to lose and each breaks a caller:

- **`-A` and the bare form differ only in whether they announce.** Both create-or-attach; the bare form prints `session '<name>' created` and `-A` prints nothing. A script that greps for that line works under one spelling and not the other.
- **`-c` is create-only, `-A` is create-or-attach.** Against a live session `-c` fails and `-A` attaches. Collapsing them turns a guard into a no-op: a caller using `-c` to assert "this session did not previously exist" would silently start attaching instead.
- **`-n` is `start` and `-N` is `run`.** One returns as soon as the session is up; the other blocks for the child's entire life. A supervisor that treats them as interchangeable either hangs forever or proceeds before the session exists.

**Only `run` and `-N` put the holder in the invoking process.** Every other creating form leaves a holder in the background and, where it attaches, the invoking process is merely a *viewer* of it. The distinction is not observable by timing — an attaching form also blocks — and an earlier pass of this document got it wrong for exactly that reason, recording the holder as running "in the caller" wherever the command did not return promptly. The observable test is what survives the invoking process: kill the client of `new`, `-A`, `-c` or the bare form and the session is still listed; kill `run` or `-N` and the session is gone, because there the holder *was* the process.

- **`-k` is not `kill` with a different name.** `kill -f` and `k -f` terminate and exit 0; `-k -f` fails with `Invalid number of arguments`. The legacy spelling takes a session name and nothing else. Merging the rows would make a working command line fail, or a failing one succeed — in both directions silently.

**Success messages are frozen too, not only creation announcements.** Callers match on these: `session '<name>' stopped` for a graceful termination and `session '<name>' killed` for a forced one — two different strings, so a caller can tell which path ran. Removal has **three** distinct forms depending on whether it was addressed by name or in bulk; they are tabulated in §3.3 and are not interchangeable. The `announces` column above covers creation only because that is where the *spellings* diverge; every message named anywhere in §3 is part of the frozen surface.

**Usage and version.** Verified against the reference: `--version` prints exactly one line naming the program and its version to **standard output** and exits **0**. The usage forms print that same line followed by the full usage text, also to standard output, and exit **0** — including the no-argument invocation. Neither writes to standard error. A conformance vector fixes the version line's shape; the version string itself is the implementation's own.

**Diagnostic streams.** Two conventions exist in the reference and both are observable, so both are frozen:

- **Argument and command errors** — unknown mode, missing operand, unparsable option value — go to **standard output**, as `<program-name>: <message>` followed by a second line `Try '<program-name> --help' for more information.`, exit **1**. `<program-name>` is the program's name **as invoked**, not a fixed literal.
- **Child-startup failure** goes to **standard error**, as `<program-name>: could not execute <path>: <system error>`, terminated **CRLF**, exit **127**. The CRLF is not an accident: the message is delivered through the pseudo-terminal, where a bare newline would leave the cursor mid-column.

An implementation that routes all diagnostics to standard error is more conventional and MUST NOT be built: callers redirect these streams and parse what lands.

**[NEW] Numeric operands are parsed strictly.** Verified against the reference, every numeric operand currently accepts the whole byte string and takes what it can: `-C -5` and `-C 99999999999999999999` are accepted silently, and `tail -n garbage`, `tail -n -1` and `tail -n 0` each yield exactly one line instead of an error or the documented default. The replacement MUST accept an optional suffix where documented and otherwise require a non-empty decimal string, reject a negative value, reject a value that does not fit the field, and reject trailing bytes — with the argument-error diagnostic and exit **1** above. *Failure prevented:* a size that wraps to a small cap, silently disabling the log a caller believed was on.

**[NEW] Trailing operands are rejected.** Verified against the reference, `list unexpected extra` exits 0 and ignores the extra words; `current` behaves the same way. A caller who typed a session name after `list` believing it filters gets a full listing and no warning. Any command given more operands than it defines MUST fail with the argument-error diagnostic.

### 3.7 Every command against all three liveness states

§2.3 replaced a two-valued notion of liveness with three values. That change is worthless until each command says what it does with the third, and the rest of §3 was written when only two existed — so read literally it creates a session on top of an indeterminate one, unlinks it, and hides it from `list`. This table supersedes any such reading.

`indeterminate` means: something is listening, and we could not establish that it is ours (§2.3).

| command | verified-live | stale | indeterminate |
|---|---|---|---|
| bare `<name>`, `-A` | attach | replace residue, create, attach | **refuse**, exit 1 |
| `attach`, `a`, `-a` | attach | fail `session '<name>' does not exist` | **refuse**, exit 1 |
| `new`, `n`, `-c` | fail `already running` | replace residue, create, attach | **refuse**, exit 1 |
| `start`, `s`, `-n`, `run`, `-N` | fail `already running` | replace residue, create | **refuse**, exit 1 |
| `push`, `p`, `-p` | deliver | fail `does not exist` | **refuse, deliver nothing**, exit 1 |
| `kill`, `k`, `-k` | terminate | fail — nothing is running to stop | **refuse to terminate**, exit 1 |
| `rm <name>` | refuse, `is running` | remove | **refuse**, exit 1 |
| `rm -a` | skip | remove | **skip**, and say so |
| `list` | render live | render by artefact shape — `[stale]` or `[exited]` (§3.3) | render **`[indeterminate]`** |
| `tail`, `clear` | operate on the log | operate on the log | operate on the log |

The refusals are the point of the section:

- **Nothing indeterminate is ever destroyed.** Not unlinked, not terminated, not replaced. The rendezvous may belong to a stranger's process, or to a successor of the session the caller meant, and neither is ours to end (§5.1).
- **Nothing indeterminate is ever written to.** `push` in particular MUST NOT deliver, because delivering is how the reference reports success against a socket with no child behind it.
- **Refusal is loud.** Each refusal exits non-zero with a diagnostic that names the state — a caller must be able to distinguish "there is no such session" from "there is something there and I could not identify it", because those call for opposite responses: create versus investigate.
- **`rm -a` reports what it skipped.** A bulk removal that silently leaves entries behind teaches the operator that the residue is unremovable.

**`tail` and `clear` are liveness-independent** because they act on the log file, not the rendezvous. This is deliberate and MUST NOT be "corrected" into a liveness check — reading the log of a session whose holder has died is a primary diagnostic path. Equally, their success MUST NOT be read as evidence of liveness by any caller.

**`list` gets a bounded total budget.** Classifying a rendezvous now costs a handshake with a deadline, so a root holding many sessions could otherwise take the number of sessions times that deadline. `list` MUST bound the *whole* operation, probe concurrently, and render every rendezvous it could not resolve within the budget as `[indeterminate]` — which is exactly what it is. A listing that hangs is a listing nobody runs.

---

## 4. Running the child

### 4.1 The pseudo-terminal

The child runs with a pseudo-terminal, with standard input, output, and (unless `-2` is given) standard error attached to it. **Terminal-generated events reach the child and not the holder** — on Linux and macOS by making the child a session leader with that terminal as its controlling terminal, on Windows by giving it a pseudo-console the holder owns (§12.3). The invariant is the routing, not the mechanism.

### 4.2 Terminal settings

When a viewer creates a Linux or macOS session, the child's terminal settings MUST be initialised from that viewer's terminal, so the child starts with the same line discipline, control characters, and modes the user already had. When no viewer is present at creation (`start` from a non-terminal caller), the child MUST receive a sane default configuration rather than an uninitialised one: canonical input, echo on, standard control characters, and a defined input and output speed.

**On Linux and macOS the field list is every setting of the creating viewer's terminal** — input, output, control and local modes, every control character, and both speeds. Transferring a subset is what produces a child whose line discipline differs from the terminal the user was just using, in ways that appear only under an editor or a full-screen program. A control character the platform does not define is left at that platform's default rather than zeroed. This is conformance-tested against a viewer with non-default settings, not asserted (**OB-5**).

**Windows has no POSIX terminal-attribute set to transfer.** The session is created through ConPTY with the viewer's character-cell size or the 80x24 default, and carries UTF-8 virtual-terminal bytes at that boundary. Console mode flags, line discipline and control-character bindings belong to the viewer's console or to the application behind ConPTY and are not copied as a substitute for POSIX `termios`. The Windows conformance lane separately covers VT-native and legacy Console API children; it MUST NOT claim POSIX control-character parity for the legacy lane.

### 4.3 Window size

The child's terminal has a size. It is set from the creating viewer's size, or to a sane default (80 columns by 24 rows) when there is no viewer at creation. Whenever the size changes — because a viewer is granted the input lease with a different requested size, or because the lease holder's terminal is resized — the child MUST be told, and MUST be told only when the value actually changes. An observer that does not hold the lease never changes session geometry (§6.1).

A resize MUST NOT be inferred from a viewer that has no terminal. An automated attach without a terminal MUST leave the child's size untouched, because a supervisor attaching to inspect a session must not shrink it to nothing. This is a real failure mode: a session left at a tiny size by an inspecting client is indistinguishable, to the user, from a corrupted display.

**This requires an explicit encoding, not an omission.** The attach exchange carries the desired size as ordinary fields with a valid range that does not include "none", so an implementation cannot express "do not change it" by leaving them out. §10 MUST freeze one representation — a reserved sentinel meaning *preserve*, or a separate presence flag — and MUST make a half-specified value (one dimension present, the other not) an explicit protocol error rather than a guess. Conformance vectors MUST cover the sentinel, both mixed cases, and the range boundaries.

### 4.4 Environment

The child's environment is the environment of the process that created the session, plus the variables this document explicitly owns: the ancestry carriers below, the terminal-identity matrix of §4.4.2, the generation pair of §10.1, the semantic token of §10.3 when enabled, the platform preload variable modified by `-S`, and the private-channel selector/nonce variables used only during launch (§10.1.1, §4.7). Every variable outside that closed set passes through unchanged. The supervised-launch selector is consumed by the holder and never reaches the requested child. The instrumentation selector and nonce reach only the requested child and are consumed and removed by the instrumentation initializer before its first application instruction. The generation pair may be rejected or stripped because it is a freshness fence rather than configuration; the other owned variables follow their own exact rules. No unrelated variable is inspected, rewritten, or removed.

#### 4.4.1 The session variable

One legacy variable records the session the child is running inside, and OB-6 adds one versioned companion that carries the same ancestry without delimiter ambiguity. Their **names are derived from the program's own invoked base name**, so a program installed under a different name writes differently named variables, and a session created by one name is invisible to `current` run under another. This is load-bearing in both directions: it is what lets a renamed or vendored copy coexist with a system-wide one, and it is what makes either variable name un-hardcodable in the implementation.

**The derivation is frozen, and it is not the root's derivation (§2.2).** Verified against the reference, applied to the invoked base name in this order:

1. Each ASCII letter is upper-cased. Bytes outside ASCII are not letters and are not case-folded.
2. Every **byte** that is not an ASCII letter or digit — hyphen, dot, anything else — becomes an underscore. This is a byte-by-byte transformation, not a character-by-character one: a base name containing a two-byte character yields **two** underscores, and a base name that is not valid UTF-8 is transformed without complaint because no decoding is attempted.
3. The result is truncated so that the **complete key, including the `_SESSION` suffix, is at most 127 bytes**; the name portion is therefore capped at 119 bytes. Truncation counts bytes and may split a multi-byte character — which is harmless here only because step 2 has already replaced every such byte with an underscore.
4. `_SESSION` is appended.

The versioned companion applies the same first two steps, truncates the transformed base-name portion to 116 bytes, and appends `_SESSION_V2`, so that complete key is also at most 127 bytes.

The byte-level statement matters because the surrounding surfaces once disagreed about it. **OB-29 settled one contract across all of them**: native path representation on native surfaces (POSIX bytes, Windows UTF-16); canonical WTF-8 when a Windows path crosses a byte or JSON surface; and canonical padded base64 for the tagged identity in JSON (§8.4.1.1).

Line-oriented human surfaces — `list`, diagnostics and `current` — use one exact reversible ASCII rendering. First obtain POSIX native bytes or canonical Windows WTF-8. Bytes in `[A-Za-z0-9._/-]` are emitted unchanged; every other byte is emitted as the four ASCII bytes `\xHH`, with uppercase hexadecimal. Backslash is therefore always escaped, so decoding is unambiguous. Padding and width rules count the rendered bytes. This also escapes spaces, quotes, brackets, `>`, non-ASCII bytes and every control byte: no legal name can inject a line, imitate `current`'s ` > ` delimiter or `list`'s status grammar, and no surface silently drops a byte. This subsection freezes only its own key transformation; OB-29 freezes the shared rendering.

A copy invoked as `mo-or.probe2` writes `MO_OR_PROBE2_SESSION` while rooting itself at `/tmp/.mo-or.probe2-1000`. A 130-character base name yields a key of exactly 127 bytes. Truncation is silent and is a genuine collision risk between two long names sharing a 119-byte prefix; it is frozen here because callers already read these keys, and any change to it belongs in a `[NEW]` clause of its own rather than in an implementer's judgement.

Each ancestry entry is the **absolute addressable rendezvous path** of the session, not the bare name: the socket path on Linux and macOS, the marker path on Windows. The legacy `_SESSION` value remains byte-for-byte compatible: POSIX carries native path bytes, Windows carries native UTF-16, and nested entries are joined outermost-first by a colon. It is deliberately retained for existing consumers even though that delimiter is ambiguous.

`_SESSION_V2` is the authoritative carrier for new readers. Its value is ASCII `v2:` followed by one or more canonical padded-base64 entries joined by `:`. A POSIX entry encodes the exact native path bytes. A Windows entry encodes the canonical WTF-8 round-trip of the native UTF-16 path, including unpaired surrogates. The base64 alphabet contains no colon, so splitting first and decoding second is unambiguous; an empty entry, non-canonical base64, unknown prefix, or non-round-tripping Windows value is malformed. The holder writes both carriers on every new session and appends the same new path to both when sessions nest.

`current` uses a valid `_SESSION_V2` when present and falls back to the legacy carrier only for a session created before the coordinated cutover. When both are present, decoding V2 and rejoining its native entries with colons MUST reproduce the legacy value exactly; a mismatch or malformed V2 is reported and never silently downgraded. The migration inventories and drains every pre-cutover session whose legacy value is ambiguous before the new reader is declared live; unaffected legacy sessions remain readable during the drain. Existing consumers may continue reading `_SESSION`, while the new `current` and supervisor consume V2. Before launching a nested child the holder constructs both complete values and verifies that the platform can carry the resulting environment; overflow is a launch refusal before publication, never truncation or loss of outer ancestry.

**[NEW] The separator MUST be unambiguous.** Verified against the reference: a session whose name contains a colon is reported by `current` as two nested sessions — `has:colon` prints as `has > colon`. A single real session is displayed as an ancestry that does not exist. This contradicts §2.1, which requires names to be opaque bytes, and it is not cosmetic: the value is how a program inside a session learns which socket it belongs to, and a caller that splits on the separator addresses the wrong path or a path that does not exist.

The specification requires an encoding that round-trips every byte a session name may legally contain. What is **not** permitted is the current arrangement, in which the ambiguity is unrepresented and the consumer guesses.

**This is OB-6, and it is a product decision.** Four candidates, each assessed on round-trip fidelity, what an existing consumer does with a new value, what a new consumer does with a legacy value, compatibility class under §0.2, and what migration it needs:

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

**OB-6 chooses D, with the exact dual-carrier contract above.** C cannot close the actual contract: the ancestry contains absolute paths, so a Windows drive designator contains a colon even when the caller's session component does not, and a POSIX parent path or invoked base name may contain one too. Restricting only the session operand therefore leaves the carrier ambiguous while claiming it is fixed. D is an additive migration for existing consumers and an OB-24 correction for `current` on affected values. OB-1 remains a separate reserved-suffix naming rule; it no longer pretends to solve ancestry encoding.

#### 4.4.2 Terminal identity **[frozen — Desk mechanic]**

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

When the child exits, the holder writes exactly one durable lifecycle exit record (§7.4), notifies attached viewers, and then terminates. If the optional event stream is enabled and still writable, it also commits exactly one `exit` event (§8) before closing that stream. A stream already closed by OB-28 or failed storage cannot truthfully accept that event; its prior exhaustion record or stream-writable-false signal is the explicit evidence of the omission.

**This covers only a child whose end the holder observes.** A holder that is killed outright, crashes, or is lost to power failure may write neither record, so a consumer that treats the absence of an exit record as proof the session continues is wrong, and one that treats a closed connection as proof the child exited is also wrong. OB-38 separates the two facts: exactly one lifecycle exit record for an observed child ending, an event-stream exit only while that stream remains writable, and holder loss reported by an external observer without fabricating a child exit.

**The addressable rendezvous object is removed on a clean exit.** Verified against the POSIX reference: after a session whose child ran to completion, no socket remains in the root, a plain `list` prints `(no sessions)`, and only `list -a` shows the session — as `[exited]`. Native Windows has the same observable rule: the pipe is closed and the marker removed. An earlier pass of this document asserted the opposite, that the socket file remains; it does not.

`[exited]` is therefore **a durable exit record with no rendezvous object beside it**, not a rendering of leftover rendezvous state. `[stale]` is the other case: a socket or marker with no listener, left by a holder that died without cleaning up. Both are the **stale** liveness state of §2.3 — absence of a listener is positively established in either — and neither adds a fourth state; they differ in which artefact survived.

This makes discovery a **union of two sources**, and §3.3 must say so rather than describing `list` as an enumeration of sockets: the rendezvous objects present in the root, plus the durable exit records — which may or may not have a rendezvous object beside them. §3.3 carries the full cross-product and deduplication rule; a plain `list` shows entries that have a rendezvous object, and `-a` adds those that have only a record. Which artefact carries the exit record, how long it is retained, and who deletes it are settled with the session log in §7, since the two share a lifetime.

### 4.6 Redirected standard error **[NEW]**

`-2 <path>` sends the child's standard error to a file instead of the pseudo-terminal. Verified against the reference, this option currently fails in three ways at once, and all three are corrected here.

- **A path whose parent directory does not exist is silently ignored.** The session starts, the exit status is **0**, and the child's diagnostics are discarded. The caller believes it has a log.
- **A path that does not exist is not created**, again silently. Same outcome.
- **A path that blocks on open — a named pipe with no reader — hangs the creating process indefinitely.** The rendezvous is published *before* the open completes, so the session is reachable and running while `start` has not returned. Worse, when that session is then killed by another party, the blocked caller wakes and exits **0** reporting that the session started — a success report for a session that no longer exists.

The third is the serious one. It breaks the launcher gate of §3.2 — which requires every caller-supplied sink to be validated and opened *before* the rendezvous is published — and it breaks it in the direction that cannot be defended against: the caller is told everything is fine.

Requirements:

- The **creating process** — not the forked child — opens and validates the sink, **before the rendezvous is published** and before any child is launched. Only the opened descriptor or handle is passed onward; the path is never re-opened later, so there is nothing to swap between check and use.
- The open MUST NOT be able to block. The target MUST be a regular file, opened append-only, without following a symbolic link or Windows reparse point, and owned by the invoking user. On POSIX its mode is exactly `0600`. On Windows it has a protected non-inheriting DACL granting full control only to the invoking user and `LOCAL_SYSTEM`; inherited ACEs and every other allow ACE are forbidden.
- **Any failure is fatal.** The command exits non-zero with a diagnostic (§13) and leaves **no** session behind: no rendezvous object, no holder, no child. A session that runs with its diagnostics going nowhere while reporting success is precisely the failure this document exists to prevent (§0.1).
- Support for pipes, devices, and other non-regular targets is **absent**. If a later pass adds it, it gets an explicit bounded handshake with a deadline and a defined timeout failure — never an implicit blocking open.

**The file MUST already exist** and is not created, exactly as the event sink is not (§8.1) — a caller that has not pre-created it has not established the ownership and permissions the rules above depend on. It is **not** constrained to the session root: unlike the event sink it is not read back by a supervisor after a restart, so its location carries no addressing requirement, and confining an operator's diagnostic file to a private temporary directory would make the option useless for its main purpose. That is the one justified difference between the two sinks (§11.4).

### 4.7 Launch-time instrumentation **[NEW]**

`-S <path>` loads an instrumentation module into the **initial requested child before its first application instruction**. On Linux and macOS the object is a shared library loaded through the platform preload mechanism. On Windows it is a matching-architecture DLL inserted while the process is suspended and acknowledged from inside that process before it is resumed. `-S` proves that one initial process loaded the named module. It is **not** an authorisation boundary, a process-containment boundary, or a promise that every descendant remains instrumented.

Verified against the reference, the control silently does nothing when it fails. A missing shared object, or a regular file that is not a shared object at all, produces exit **0** and a child that runs **without** the library. The dynamic loader's complaint goes to the pseudo-terminal, where the caller — an automated supervisor with no viewer attached — never sees it. The caller believes the child is constrained. It is not.

Instrumentation that fails open, silently, is worse than a stated absence, because the caller cannot distinguish the two.

Requirements:

- **The integration contract, frozen:** the object is an existing regular file owned by the invoking user, named by absolute native path, and not reached through a symbolic link or Windows reparse point. On POSIX its permissions are no broader than `0755` with no group/other write; on Windows it has a protected DACL with no write grant outside the invoking user and `LOCAL_SYSTEM`. The object architecture MUST match the initial child. The holder's command line and environment **may** expose the path to other processes of the same user, and this is accepted: same-user processes are already trusted (§11.1), and hiding it would imply a boundary that does not exist.
- **The POSIX loader encoding is exact.** Linux prepends the absolute path to `LD_PRELOAD`, separated from an inherited nonempty value by one ASCII space; because that loader has no escape syntax and expands dynamic-string tokens, a path containing `:`, `$`, or ASCII whitespace is refused before launch. macOS prepends it to `DYLD_INSERT_LIBRARIES`, separated from an inherited nonempty value by one colon; a path containing `:` is refused. The resulting loader variable is inherited normally, so ordinary dynamically linked descendants generally load the module too; set-id, static, loader-scrubbed, or explicitly replaced environments may not. The acknowledgement below proves only the requested initial child. On Windows the guarantee ends at that child and descendants are not automatically injected. A caller needing descendant coverage requires a separately specified producer/launcher contract and MUST NOT infer it from `-S`.
- The instrumentation object MUST be validated by the **creating process** before the rendezvous is published, on the same terms as §4.6.
- **The initial child MUST acknowledge that the module loaded over OB-22's separate private channel**, and `start` MUST NOT report success until it has. The holder creates a one-way byte stream, inherits only its write end into the requested child, selects it with `DESK_MOOR_INSTRUMENT_CHANNEL`, and supplies a fresh 16-byte challenge as 32 lowercase hexadecimal digits in `DESK_MOOR_INSTRUMENT_NONCE`. The selector grammar is canonical unsigned decimal descriptor text on POSIX and 1–16 lowercase hexadecimal digits without `0x` for a nonzero 64-bit Windows handle. Both values are private launch material, not authorisation.
- **The module-side ABI is frozen.** On POSIX a load constructor performs the acknowledgement. On Windows the DLL exports the exact ASCII symbol `MoorInstrumentationInitV1`; after loading the DLL into the still-suspended requested process, the insertion path invokes that no-argument initializer there and requires it to return unsigned value zero. In either case the initializer parses the selector and nonce, removes both environment variables before any application instruction can run, writes §15.2 of the companion schema's exact 36-byte acknowledgement, closes the inherited write end, and only then reports initializer success. A later POSIX descendant that inherits the preload variable but neither private variable loads the module normally and emits no acknowledgement.
- The holder accepts the acknowledgement only when the record is followed by EOF within 2 seconds and its generation, requested-child PID, and nonce all match the values for this launch. A short or long record, missing EOF, inherited duplicate write handle, malformed selector/nonce, wrong PID/generation/nonce, nonzero Windows initializer result, timeout, or any channel error fails the unpublished launch. Validating the file is not enough: a file can be a well-formed object of the wrong architecture, or be rejected by the loader or Windows insertion mechanism for reasons no static check predicts. The only trustworthy evidence is from inside the requested initial child after the module's initialization ran.
- A missing, malformed, wrong-architecture, or loader-rejected library **fails closed**: no session, non-zero exit, diagnostic naming the cause. There is no "run the requested child uninstrumented" path — not as a fallback, not behind a flag, not with a warning.
- **Windows launch order is normative:** create the bootstrap suspended with only its explicit inherited handles, assign it to the session job, establish its private control channel, and let it create the requested child suspended as a new process group. Insert the matching-architecture DLL into that requested child, accept only that process's in-module acknowledgement, and only then resume its initial thread. An ACK from the bootstrap or insertion helper is not sufficient. Any failure tears down the unpublished job and rendezvous state. The insertion mechanism is not frozen to a particular third-party library; shipped x64 and arm64 implementations each have to pass this behavior against the real binary.

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
- Generation fencing, event-stream commit recovery, and bounded raw-output replay survive a real restart: create a session, attach, **restart the supervisor while the holder keeps running**, and observe that adoption re-establishes identity, reports replay exactness honestly, and refuses a superseded generation. Wire v3 has no screen checkpoint (§6.7). Restarting the *holder* is not required and is not achievable — it owns the pseudo-terminal and no successor can reopen it by name (OB-26).

A submission whose evidence for any of the above is a component test has not met this section, regardless of the component's quality.

## 6. Attaching, detaching, and redraw

### 6.1 Multiple viewers

A session accepts more than one viewer at once. All attached viewers receive the same output. Exactly one viewer at a time holds the **input lease**: only the lease holder may send input or change the size. The holder starts with lease epoch `0`; the first grant allocates `1`, and every later handover increments it, so a reply or input frame from a previous holder is refused rather than applied. The unsigned 32-bit epoch never wraps or reuses a value. After epoch `FFFFFFFF` is released, no new lease can be granted and the request fails with `RESOURCE_EXHAUSTED`; the session and existing viewers remain alive. A lease is released explicitly, or by a deadline when its holder stops responding.

*Why a lease and not free-for-all:* several viewers of one session would otherwise fight over the child's window size, and a late reply from a departed viewer would be injected as input.

### 6.2 The detach key

A configured control character detaches the viewer without disturbing the child. It is consumed by the holder and **never reaches the child**. The default is the character conventionally written `^\`; `-e` sets another and `-E` disables detection entirely, in which case no keystroke detaches.

To send the detach character *to the child*, type it twice: the first is consumed, the second passes through. This is the only doubling rule, and it exists only while detection is enabled.

Detaching leaves the child running and its terminal settings untouched.

### 6.3 Redraw on attach

`-r` selects how the child is prompted to repaint: `none` sends nothing; `ctrl_l` sends the conventional redraw character; `winch` re-sends the current size, which most full-screen programs treat as a reason to repaint.

**This is one of only two sequences the holder may put into the child's input stream** (§1.1), and it happens only when the operator has selected it. `none` is the default for an automated attach, because a supervisor inspecting a session must not make the child redraw.

### 6.4 Clearing the viewer's screen

`-R` selects what the viewer's screen is doing when the session appears: `none` leaves it, `move` repositions the cursor. These bytes go **to the viewer only** and never to the child, and like the preamble (§1.1) they must not be logged or advance any output cursor.

### 6.5 Suspend

A viewer may suspend itself. `-z` disables that, so the keystroke passes through to the child instead. A suspended viewer is still attached; the session and its child continue.

### 6.6 When a viewer disappears

A viewer whose terminal vanishes — the connection drops, the process dies, the window closes — is detached. **The session is unaffected** (§5.1). No output is lost from the session's point of view; the departed viewer simply stops receiving it. If the departed viewer held the input lease, the lease is released by its deadline and another viewer may take it.

### 6.7 Bounded raw-output replay

Moor retains raw child output so a new viewer can establish a bounded baseline without asking the child to repeat its whole history. This is byte retention, not a screen model or terminal checkpoint:

- Each `OUTPUT` record carries at most 64 KiB of child bytes. The holder retains the newest complete records whose payload bytes total at most **4 MiB**. When the next complete record crosses the bound, whole oldest records are discarded until the bound holds; a record is never retained partially.
- On every successful attach, the holder freezes the retained first/last descriptor placed in `ATTACH_ACK`, sends a `GAP` for `1..first-1` when a prefix was discarded, then sends every retained `OUTPUT` record in sequence before later live records on that connection. Output arriving during the baseline is ordered behind the frozen baseline. A controller that already consumed some records discards duplicates by record sequence; a fresh emulator applies the whole retained run.
- Empty history has no `GAP` and no `OUTPUT`. A retained run beginning at record 1/byte 0 is complete raw history for this holder incarnation. Any later start is explicitly degraded: an exact modes preamble can restore the tracked modes, but Moor does not claim that a suffix reconstructs the screen. If tracked-mode exactness was lost, the mandatory preamble is empty rather than asserting guessed state.
- Replay uses the same per-viewer 4 MiB child-payload backpressure bound as live output. The frozen baseline may pin immutable retained-record buffers for that viewer, and those bytes count against its bound; global eviction therefore cannot mutate an in-flight baseline and no second unbounded copy exists. A viewer that cannot drain the baseline before later live bytes cross its bound is disconnected without affecting the child.

There is **no checkpoint carrier in wire v3** and no main/alternate-screen exactness claim. Earlier status fields advertising those facts had no frame capable of carrying a checkpoint and would have invited two incompatible implementations. The only exactness facts are whether raw history starts at byte zero and whether the bounded terminal-mode scanner still knows its tracked state.

## 7. The session log

### 7.1 What is written

The child's output, as bytes, in order. Nothing else: not input, not the preamble (§1.1), not diagnostics, not arbitration replies. The log is what the child produced, so that `tail` shows what the child said.

### 7.2 The cap

`-C` bounds the log; `0` disables it entirely. The default is one mebibyte.

Reaching the cap must not stop the session and must not lose the newest output — those are the two failures worth naming, and they are the ones a naive implementation picks. **The retention policy at the cap is frozen in a single decision** and applies uniformly: after each child-output write, the retained log is exactly the newest `min(cap, previous-retained-bytes + new-bytes)` bytes. The oldest prefix is discarded, including the oldest prefix of one write when that write alone is larger than the cap; the newest `cap` bytes of that write remain. The file never exceeds the cap. A reader that was following is told the exact byte range containing its position is no longer present rather than silently resuming somewhere else. That last clause is the same rule as §8.4.4's gap reporting, for the same reason.

Log positions use the same zero-based absolute child-output byte coordinates as controller `OUTPUT`; rotation never renumbers them. If total child output is `E` bytes and the log retains `R`, the retained log represents `[E-R,E)`. This coordinate is behavioral state even though the file itself contains only the raw `R` child bytes (§7.1).

### 7.3 `tail` and `clear`

`tail` prints the last *N* lines and, with `-f`, continues as the log grows. **A follower survives the log reaching its cap**: it is told about the discontinuity and continues, rather than stopping or silently jumping. If its next absolute byte is `F` and rotation advances the retained start to `R > F`, it writes exactly `<program-name>: log gap: child-output bytes [<F>,<R>) were discarded` plus LF to standard error, then resumes at `R`. The two numbers are canonical unsigned decimal u64 values. Several rotations observed together coalesce into that one maximal half-open range. This is a diagnostic and `-q` does not suppress it. The implementation mechanism is free, but inferring success from a file offset after rewrite is not: the reported coordinates and resumed bytes must match the holder's absolute child-output positions.

A reader is **not** guaranteed complete lines. The log is a byte stream from a program that may emit anything, including a final fragment; `tail` does not wait for a newline that may never come.

`clear` truncates the log to empty without disturbing the session (§3.4).

Conformance parks a follower before the retained start, crosses the cap with several writes and with one write larger than the cap, and byte-compares the one coalesced gap diagnostic, the resumed suffix, and the unchanged child/session lifetime.

### 7.4 The exit record **[OB-9]**

When a session ends, a durable record of how it ended survives the addressable rendezvous object. It is what `list -a` renders as `[exited]` (§3.3, §4.5) and what `rm` removes.

- It is stored **beside the session's other companion state**, under whichever namespace OB-1 selects.
- It carries the exit outcome — the distinction of §13.2 — and the timestamps of §10.2.5.
- It is retained until removed by `rm`, or until a new session of the same name is created, whichever comes first. **It is not aged out on a timer**: a timer would make a session's history disappear while an operator was still looking for it.
- The session log shares this lifetime, because the two are read together when diagnosing why a session ended.

## 8. The event stream

This is how the supervisor learns what is happening inside a session without attaching to it or parsing its screen. It is the most important interface in this document after §1.1, and the one whose failure is hardest to notice: a supervisor that receives no events shows a confidently wrong picture rather than an obviously broken one.

### 8.1 Enabling and the path contract

The stream is named by `-T <path>` (§3.5). Its filesystem shape is platform-specific because Windows cannot make the POSIX rename design crash-safe while arbitrary readers hold the file. The JSON bytes are identical; only their storage commit differs.

- **Linux and macOS:** `<path>` is the single event file. It MUST already exist and be empty when the session is created, be a regular file opened without following a symbolic link, be owned by the invoking user with mode exactly `0600`, and reside inside the session root. Compaction follows §8.4.2's replacement protocol.
- **Windows:** `<path>` is an already existing **empty directory** inside the session root. It is opened without following a reparse point, MUST NOT itself be a reparse point, and has the same owner and protected-DACL rule as the root. Before the marker is published, the creating process creates exactly four protected, non-reparse regular files inside it: `body.0.jsonl`, `body.1.jsonl`, `commit.0`, and `commit.1`. They are created once and are never renamed, replaced, or unlinked while the session is live. Publication begins only after `body.0.jsonl` and `commit.0` contain a flushed valid initial commit (§8.4.2). Any extra entry, pre-existing slot, or non-empty directory is refusal rather than adoption.

The caller pre-creates the top-level sink object; the holder creates only Windows' fixed interior slots. This is breaking correction 12 under OB-24. A session that runs without its event stream looks healthy while being invisible, so every validation or initialization failure is fatal and occurs before rendezvous publication.

### 8.2 Format

One JSON object per line, UTF-8, newline-terminated, appended in observation order. The first line of the file is the **header** record defined in §8.4.1; every line after it is an **event** record.

The two record classes do not share a field set, and an earlier pass of this document said every object carried `seq` and `epoch` while §8.4.1 forbade `seq` on the header. The split is:

- **Common to both:** `type`, and `ts` — seconds since the Unix epoch as a JSON number with millisecond precision.
- **Header only:** `v`, `session`, `generation`, `epoch`, `next_seq`, `first_retained` (§8.4.1).
- **Event only:** `epoch`, `seq`, `kind`, and the per-type fields below.

Nine event types exist — three derived from terminal bytes, three carrying semantic-producer provenance, one reporting missing application evidence, one describing the stream, and one describing the child exit:

| type | additional fields | meaning |
|---|---|---|
| `ready` | — | a terminal capability query was observed **being emitted by the child**, once per session. This records only that signal. It is not evidence that the child is interactive, healthy, or trustworthy: any program can emit the sequence, so no consumer may treat `ready` as proof of anything beyond "this byte sequence was seen" |
| `state` | `state`, `title`, `truncated` | the child's activity classification changed (§9); `title` carries the observed title **bounded and encoded per §9.4** — never verbatim. A title is arbitrary bytes chosen by whatever the user ran, and this line must remain well-formed UTF-8 JSON |
| `link` | `uri`, `truncated` | the child emitted a hyperlink. `truncated` is `true` when the value was shortened by the bounds of §9.4, so a consumer never treats a shortened target as complete |
| `semantic-source` | `source`, `producer`, `source_epoch`, `status`, `reason` | a **stateful** source connection changed state. `status` is `"connected"`, `"exact"`, `"degraded"`, or `"disconnected"`. `connected`/`exact` require `reason:""`; `degraded` requires `"heartbeat-timeout"`; `disconnected` requires one of `"transport-closed"`, `"superseded"`, or `"session-ending"`. No other pairing is legal. Edge-source connect and disconnect produce no `semantic-source` record because an edge connection claims no continuing state. A lost stateful source makes its evidence degraded; it never means the application became idle. An event-sink failure cannot durably append `stream-unwritable`; status/heartbeat carries that condition instead |
| `semantic-assertion` | `source`, `producer`, `source_epoch`, `source_seq`, `event_id`, `assertion_kind`, `payload` | an authenticated producer assertion accepted through §10.3. `assertion_kind` is `"transition"` or `"snapshot"` and preserves the producer-wire assertion kind; `"snapshot"` is legal only for a stateful source, while an edge source may publish only `"transition"`. The common event `kind` independently says whether this JSON line is a newly published `transition` or a compaction `snapshot`. The 16-byte `producer` and `event_id` values are canonical padded base64. `payload` is canonical padded base64 of the producer's exact validated UTF-8 JSON object; Moor preserves it and does not interpret provider keys |
| `application-receipt` | `source`, `producer`, `source_epoch`, `source_seq`, `event_id`, `application_request_id`, `lease_epoch`, `request_id`, `status`, `provider_session`, `provider_turn` | a producer asserted an application outcome correlated to one written `INPUT`. `status` is `"accepted"` or `"refused"`. Identifier and provider fields use canonical padded base64. `provider_session` and `provider_turn` may encode zero bytes when the producer has no such identifier; the keys remain present and the empty canonical-base64 spelling is `""`. This is evidence **from the named producer**, not evidence that Moor independently observed application behavior |
| `application-receipt-missing` | `source`, `producer`, `source_epoch`, `application_request_id`, `lease_epoch`, `request_id`, `reason` | Moor had no correlated producer receipt at a defined diagnostic point. The producer and source epoch are those selected before the terminal write. `reason` is `"deadline"`, `"source-lost"`, or `"retention-expired"`. This record is explicitly absence of evidence, never a refusal or application outcome; a later valid receipt may still follow a deadline/source-loss record while the correlation remains retained |
| `stream-exhausted` | `axis` | **[NEW, OB-28]** the stream cannot durably admit the requested operation on the named axis and is closed. `axis` is `"seq"`, `"epoch"`, or, on the Windows storage layout, `"commit"`; `kind` is always `"transition"`. The exact final-allocation algorithm is in §8.4.1 and OB-28. In particular, a sequence-exhaustion record consumes the one sequence position kept in reserve and may therefore carry a value below the numeric maximum when a multi-record compaction no longer fits. The session continues; the supervisor learns through the liveness surface (OB-30) that the stream is no longer writable |
| `exit` | `ended` plus its branch fields | the child ended. `ended:"exited"` carries `code`; on POSIX `ended:"signalled"` carries `signal`; on Windows a holder-caused stop carries `ended:"terminated"`, `code`, and `method:"graceful"\|"forced"`. A Windows exit not caused by the holder remains `"exited"` because the platform supplies only the unsigned 32-bit exit code. Any fields from another branch make the record malformed |

**Event schema version 2 has closed key sets.** The header keys are exactly those in §8.4.1. Every event has exactly the common event fields plus the additional fields in the table; no other key is legal at `v:2`. A duplicate key is malformed even when both occurrences have the same value; a reader must detect it rather than let a convenience parser silently keep one occurrence. `application-receipt`, `application-receipt-missing`, `stream-exhausted`, and `exit` are occurrence-only and require `kind:"transition"`; a committed `kind:"snapshot"` on any of those types is malformed. The other types may carry `kind:"snapshot"` only when §8.4.4 requires that compaction restatement. All base64 is standard padded canonical base64. `source_epoch` and `lease_epoch` are JSON numbers in the u32 range. `source_seq` and `request_id` are **JSON strings containing canonical unsigned decimal u64 values**: either `"0"` or `[1-9][0-9]*`, no sign, whitespace or leading zero, and within `0`–`18446744073709551615`; their owning wire rules require them to be nonzero in these events. They are strings because an ordinary JSON-number parser cannot preserve their full u64 identity above 2⁵³−1. The event stream's own `seq` remains the bounded JSON number defined in §8.4.1. POSIX exit code is a JSON number 0–255 and Windows exit code is a JSON number 0–4294967295.

A reader MUST be able to parse the file with a line-oriented JSON reader and no knowledge of this program. A partially written final line MUST be tolerated by readers and MUST NOT be produced across a restart: see §8.4.

### 8.3 Emission rule

A `state` event is emitted when the classification **changes**, not on every title the child sets. A child that repaints the same idle title fifty times produces one event. This is a requirement, not an optimisation: the supervisor treats each event as a transition and would otherwise see a storm of identical transitions.

### 8.4 Bounding, compaction, and recovery **[NEW]**

The stream MUST NOT grow without limit, and a restarting supervisor MUST NOT be made to re-read the whole history.

An earlier pass of this document required only that the program "compact in place" and that compaction be "atomic from a reader's point of view to the extent the platform allows". **That is not a protocol, and it is not implementable.** Both natural readings were tested against the production reader and both lose data silently:

- **Replace the file by rename.** The reader holds its descriptor open and never learns the file it is reading was replaced. Every record written after compaction is lost, with no error anywhere.
- **Truncate the same file and rewrite.** If the new content reaches or passes the reader's stored byte offset, the reader resumes reading from the middle of a record, reports malformed input, and loses the very event that triggered compaction.

The reader cannot be blamed for either: a byte offset into a file that is rewritten underneath it carries no information about whether it is still valid. So the specification, not the reader, must supply that information.

#### 8.4.1 Records, and the two kinds of them

The file is a sequence of **records**, one JSON object per line. A record is either the header or an event; the **nine** event types of §8.2 are event types and do not include the header.

An earlier pass of this document required the header's `seq` to be "the sequence number of the record that follows it" *and* required every record to carry a `seq` increasing by exactly one. Those two rules together say `h = h + 1`, which nothing satisfies, and for an empty stream the record that follows does not exist. **The header is therefore not a sequenced record.** It is the file's preamble, it carries no `seq` of its own, and it names where the body begins.

- **The header is the first line of every committed body**, exactly once, with `"v":2` and `"type":"header"`. Its keys are exactly: `v`, `type`, `ts`, `session` (§8.4.1.1), `generation` (§10.1, or JSON `null` for a session that has none), `epoch`, `next_seq` (the `seq` the first event record after this header carries), and `first_retained` (§8.4.4). No other key may appear at schema version 2; a reader encountering one MUST stop rather than guess. Adding or changing a key is a `v` increment, not an extension.
- **Every *event* record carries `epoch` and `seq`**, and `seq` increases by exactly one from one event record to the next. It does **not** reset when the epoch changes: the epoch says which physical body you are reading, the sequence says how much of the stream you have consumed. An empty stream is a header with `next_seq` equal to `first_retained`, and no further lines.
- **`ts`** is seconds since the Unix epoch as a nonnegative JSON number derived from an unsigned 64-bit millisecond count, on the header and on every event record. Its canonical spelling is the decimal whole-second quotient with no leading zeros, followed, when the millisecond remainder is nonzero, by `.` and exactly three decimal digits. Thus the field has a finite maximum spelling of 21 bytes, and admission calculations in §8.4.4 reserve that maximum rather than assuming today's clock width. A negative, exponent-form, over-range, or non-canonical spelling is malformed.
- **`epoch`** is an unsigned 32-bit integer starting at `0` for a new stream and increasing by exactly one per compaction. **`seq`** is an unsigned integer starting at `0` and bounded above by **2⁵³−1**. Neither wraps.

  **Their exhaustion is not the same condition as generation exhaustion, and an earlier pass of this document delegated it there wrongly.** A generation is exhausted *before* anything is launched: nothing is created, the command exits non-zero, and no state exists to clean up (§10.1). A sequence or epoch is exhausted with a **live child, a published rendezvous, and an attached supervisor** — long after `start` returned zero. "Handled the same way" is not available: there is no command left to fail.

  **Final allocation is preflighted as one transaction.** Before serialising an event, the writer calculates the complete record set the operation requires: one transition without compaction, or every synthetic snapshot plus the triggering transition with compaction. Ordinary work may consume sequence values only while at least one further sequence position remains available for a final diagnostic.

  - If that complete set will not fit while preserving the diagnostic position, the triggering event is not accepted or published. The writer appends exactly one `stream-exhausted{axis:"seq"}` at the current next sequence directly to the current body, without compaction, commits it, and permanently marks the stream unwritable. Because a compaction can need several sequence values, this final record can legitimately have a `seq` below 2⁵³−1; every larger value remains unused. A semantic producer receives `SEM_RESOURCE_EXHAUSTED`, never a false durable ACK.
  - If a cap-triggering operation would require an epoch after `4294967295`, sequence capacity is checked first. When it suffices, the writer appends the triggering transition followed by `stream-exhausted{axis:"epoch"}` in epoch `4294967295`, without another compaction, commits both as one final transaction, and closes the stream. The accepted trigger therefore survives exactly once.
  - On Windows, if the next storage commit is `FFFFFFFFFFFFFFFF`, sequence and epoch checks run first. When neither is limiting, the writer includes the triggering transition and `stream-exhausted{axis:"commit"}` in the body update and publishes both with that final commit index. A compaction required by the trigger includes its complete snapshot baseline in the same update. The final commit is never used for an ordinary transaction that would leave the stream apparently writable.

  The limiting-axis precedence is therefore `seq`, then `epoch`, then Windows `commit`. A final direct append or final commit may exceed the 256 KiB cap by exactly the records in this terminal transaction; no later event is admitted. Recovery treats a committed `stream-exhausted` as permanent stream closure. If storage I/O itself fails before the diagnostic commits, the writer cannot fabricate durability: it closes semantic ingress and reports stream-writable false through status/heartbeat instead.

  The bound on `seq` is not arbitrary. A 64-bit sequence written as a JSON number cannot be read back exactly by a conforming JSON parser above 2⁵³−1 — the value silently becomes a nearby one, which for a cursor means silently resuming at the wrong record. Capping at the largest exactly-representable integer keeps every standard reader correct without requiring a special parser. At the reference's observed event rate this bound is not reachable in any real session; it exists so that the failure, if ever approached, is a defined terminal condition rather than a rounding error.

##### 8.4.1.1 The `session` field

§2.1 makes names opaque native path values and defines two names as the same session **if and only if their tagged canonical identities match**. A JSON string cannot directly carry that binary identity, so the header needs an encoding, and it needs to say *which* bytes it encodes — an earlier pass of this document said "the session name as a string" in one paragraph and "base64 of the raw bytes" seven lines later, which are two different fields wearing one name.

- The value is the **tagged canonical session identity** frozen by **OB-17**. On Linux and macOS it is tag `01` followed by the socket's absolute path with `.` and `..` resolved lexically, **without** following symbolic links, canonicalised once before publication. On Windows it is tag `02`, the staged marker's volume serial number as little-endian u64, and its 16-byte `FILE_ID_INFO.FileId`; the same-directory atomic rename must preserve those exact values and the final marker is re-opened and revalidated before any connection is admitted. It is never the command-line spelling.
- Those tagged identity bytes are encoded with **standard base64 including padding**, over the alphabet `A`–`Z`, `a`–`z`, `0`–`9`, `+`, `/`. Line breaks are never inserted. A decoder MUST reject non-canonical input rather than accept it leniently.
- There is **no companion display field**. A reader that wants something human-readable decodes and renders it under its own rules; a second field would be a second identity, and identities that can disagree eventually do.

**These are design choices, not measurements.** The reference has no header record at all, so nothing above was observed. The choices this document is making are: **that the identity is whatever OB-17 defines rather than the argv spelling**, padded canonical base64, the closed key set, and stopping on an unknown key. Each is justified where it is stated; none is a reported fact. There is no longer a "resolved path as identity" choice — an earlier pass made one, and this subsection withdrew it in favour of OB-17.

**Both the encoding and the input bytes are now frozen.** The kind tag prevents a Windows marker identity from being confused with path bytes. The same tagged live identity keys the header, acknowledgements and destructive fence, which is what makes those surfaces comparable. The supervisor's durable generation allocator deliberately uses the separate logical session key defined by §10.1.2: a Windows marker identity does not exist before launch and changes on every publication.

##### 8.4.1.2 Snapshots and transitions

- **Every event record carries a `kind`: `transition` or `snapshot`.** A `transition` records something that just happened. A `snapshot` restates knowledge that was already published, so that a reader arriving after compaction is not blind to it.

  **A consumer MUST NOT treat a `snapshot` as an occurrence.** Without this, compaction republishes `ready` as a second readiness, and the latest `link` as a hyperlink the child just emitted — the exact "restart replays history as current" defect §8.4 exists to prevent. Distinguishing by type alone cannot work: a snapshot of a `link` *is* a `link`.
- `semantic-assertion.assertion_kind` is a different axis. A newly accepted producer message always has event `kind:"transition"`, including when its `assertion_kind` is `"snapshot"`; compaction may later restate the latest exact stateful assertion as event `kind:"snapshot"` while preserving `assertion_kind:"snapshot"`. The first is a publication occurrence, the second a storage resync. Conflating them makes the record that triggers compaction change semantic meaning.
- **The record that triggered compaction appears exactly once**, as a `transition`, after the snapshots. It is never also represented among them.

#### 8.4.2 Compaction and append have a platform-specific crash-safe commit

An earlier pass of this document required same-inode truncate-and-rewrite and forbade rename. **That is not crash-safe and the prohibition was reasoned from a false premise.**

Not crash-safe: truncating and then writing leaves an interval in which the file contains a prefix of a record and nothing else. A crash inside that interval was demonstrated — the file was left holding five bytes, a fragment of one key. No amount of care inside the writer closes that window, because the window is between two system calls.

False premise: rename was forbidden on the grounds that it detaches readers holding an open descriptor. That is true of a reader that never checks, but §8.4.3 already requires the consumer to change, and the consumer is inside the coordinated cutover (§0.2). Forbidding the only crash-safe construction to protect a reader this document was already rewriting was an error.

**Linux and macOS use replacement:**

- Initialization writes the complete schema-v2 header and its newline to the already opened empty sink and calls `fsync` before the rendezvous is published. An ordinary append writes the complete newline-terminated record to the current authoritative descriptor and calls `fsync` before advancing the durable cursor, sending `WAKEUP`, or returning any durable semantic acknowledgement. A write or `fsync` failure leaves that record uncommitted, permanently marks the stream unwritable, and admits no later append. This is the POSIX storage commit referred to elsewhere in this document; an in-memory write or page-cache visibility is not durability.
- The replacement is composed in full, written to a temporary file **in the same directory**, flushed to stable storage, and then **atomically renamed over** the target. The directory itself is then flushed, so the rename survives a power loss rather than only a process crash.
- The temporary file is created with the same ownership and permission requirements as the sink itself (§8.1), and is removed if the replacement is abandoned. A temporary file left behind by holder loss is ignored by readers and removed only during confirmed rollback/stale-session cleanup; a successor holder never adopts it or the old sink.
- **The writer swaps to the new file before any further append.** Emitting and compacting are serialised: after the rename the writer's descriptor refers to the new body, and the old descriptor is closed. An earlier pass of this document said "ordinary appends still go to the open file", which after a rename means the *old, unlinked* body — a demonstrated loss, where a transition written just after compaction survived only in a file with no name, while the path held the snapshots alone. No append may target a descriptor whose file has been replaced.
- **Readers detect replacement by file identity**, not by size: a reader re-examines the path and compares its device/inode identity against the one it has open (§8.4.3). A change means reopen and re-read from the header.
- **A record is written as one complete line, newline included.** A reader that finds a final line without a terminating newline retains the bytes and does not consume them — an append may be in progress.

**Torn tails are never spliced into a later record.** While the holder is live, a short write is completed by the same serialised append operation; an unrecoverable write/flush failure permanently marks the stream unwritable and no later append is attempted. After holder loss, readers consume only the newline-terminated prefix and report holder loss separately. OB-26 forbids pretending a successor writer can resume the PTY, and §8.1 forbids a new generation from adopting a nonempty sink, so no writer startup path appends to the fragment. Confirmed retirement removes the generation's sink and any fragment with it.

**Failure of the replacement is not silent.** Running out of space, or a failed temporary-file write or file flush before rename, leaves the previous body in place and removes the temporary file. A failed rename likewise leaves the previous path authoritative. Once rename succeeds, however, the path already names the complete replacement: a subsequent directory-flush failure cannot truthfully be described as leaving the previous body in place and MUST NOT attempt a guessed rollback. It marks the stream unwritable, reports that namespace durability is unknown, and admits no later event. Every failure is reported through the same diagnostic path as any other sink failure. It MUST NOT leave the session running with an apparently healthy event stream: §8.1 refuses to *start* a session whose sink cannot be honoured, and a sink that has become unwritable afterwards is the same condition discovered later. The specification's rule is that the supervisor learns — a session that goes silently unobserved is the failure §8 exists to prevent.

**Windows uses two fixed bodies and two fixed commit slots; rename is forbidden.** The exact 76-byte commit record is in §13 of the companion wire schema. A valid record names its own commit slot, one body slot, the generation and event epoch, a strictly increasing u64 commit index, the committed body length, a SHA-256 of exactly that prefix, and a CRC-32C over the preceding record bytes.

- Initialization writes the schema-v2 header to `body.0.jsonl`, flushes that body, writes commit index 1 to `commit.0`, and flushes the commit. Only then may the marker be published.
- An ordinary append writes one complete JSON line to the active body and calls `FlushFileBuffers`; it then rewrites the **other** commit slot with the next commit index, the new length and prefix hash, and flushes that commit slot. The previous commit remains the fallback until the second flush succeeds.
- Compaction writes the complete replacement into the inactive body from offset zero, truncates it to the exact new length, and flushes it. It then writes and flushes the other commit slot pointing at that body. The active body changes only with that successful commit.
- Recovery validates both 76-byte commit records (size, magic, reserved fields, self-slot, generation, CRC), then the named body prefix (length, SHA-256, a schema-v2 header, complete JSON lines and final newline). It chooses the greater valid commit index. Equal valid indexes with different bytes are corruption and fail closed. With no valid commit the stream is unusable and is reported, never reset. Bytes after the chosen length are uncommitted tail and are ignored. A successor generation never truncates and adopts these files; confirmed retirement removes the whole sink directory.
- Commit index zero is invalid. `FFFFFFFFFFFFFFFF` is reserved for OB-28's final transaction and never wraps. That transaction contains the accepted triggering transition followed by `stream-exhausted{axis:"commit"}`, unless the higher-priority sequence or epoch rule supplies the final diagnostic instead. A failure before the commit-slot flush leaves the previous commit authoritative; a failure after it leaves the new commit authoritative. There is no interval in which a torn record is chosen.

A semantic-producer `ACK` saying accepted is sent only after the corresponding JSON line has crossed the platform commit above: the ordinary-append `fsync`, or the replacement-file flush plus rename and directory flush when compaction occurs, on POSIX; body flush plus commit-slot flush on Windows. This makes a retry after a lost ACK deduplicable without pretending that an in-memory queue was durable.

#### 8.4.3 The cursor is file identity plus sequence, never a byte offset alone

A reader's position is `(storage identity, seq)`. On Linux and macOS storage identity is device and inode. On Windows it is `(commit_index, body_slot, body_length, body_sha256)` from the selected commit; an open-handle file identifier alone cannot say which committed prefix is active. A byte offset MAY be cached as an optimisation; it is never authoritative.

On resume the reader validates before it trusts. POSIX compares the path's file identity with the open file. Windows revalidates both commits and selects the authoritative prefix. If storage identity changed, reopen or switch body and read from the header. Otherwise read from the cached offset and check that the first record carries the expected `epoch` and next `seq`. On any mismatch, re-read from the committed body's start — cheap precisely because §8.4.1 keeps it bounded.

This is what makes the failure *detectable*. The defect in the designs this replaces is not that the reader lost its place; it is that it lost its place and could not tell.

**This changes the consumer.** The production reader today holds its descriptor indefinitely, detects replacement only by the file having shrunk below its offset, and stores a byte offset alone. It cannot implement the above unchanged. That is permitted, and only permitted, because the supervisor ships in the atomic coordinated cutover of §0.2 — an external caller could not be asked for this.

#### 8.4.4 The bound, what compaction discards, and how a reader learns

The bound is a **byte cap on the file**, frozen at 256 KiB, with compaction triggered by the first append that would exceed it. A record-count cap is not used: the consumer's cost is dominated by bytes read, and a title is orders of magnitude larger than an exit code.

The bound also governs **admission**, not just cleanup after the fact. The *compaction baseline* is the exact header plus every snapshot §8.4.4 requires, excluding the one triggering transition. Before publication, the initial header plus space for the maximum legal terminal `ready`, `state`, and `link` snapshots must fit the cap or launch with `-T` fails. Before accepting a new semantic source or complete stateful snapshot that would change the retained set, Moor projects the resulting baseline using the exact bytes it would serialize, reserving those maximum terminal snapshots even when the child has not emitted them yet and the largest legal `semantic-source` status line for every admitted **stateful** source. If that projection exceeds 256 KiB, the semantic operation is refused with `SEM_RESOURCE_EXHAUSTED` before acknowledgement or state change. Later terminal observations and mandatory stateful-source degradation/disconnection records therefore always fit the reserved baseline; occurrence-only events may still invoke OB-12's one-trigger overage. This byte-budget rule is independent of wire-schema-3's 64-source resource cap: the count cap bounds holder state and connections, while this projection proves that the exact retained JSON for the sources actually admitted, including their accepted 32 KiB snapshots, still fits durable storage.

**Compaction discards history, and the earlier text was wrong to imply otherwise.** The new body contains, in order: the header; snapshots for terminal `ready`, `state`, and latest `link`; the latest `semantic-source` state for each **stateful** source sorted by raw source id; the latest accepted stateful `semantic-assertion` whose `assertion_kind` is `"snapshot"` for each stateful source in the same order, restated with event `kind:"snapshot"`; and the triggering record exactly once as a transition. Application receipts, missing-receipt diagnostics, edge assertions, and older transitions are occurrences and are not invented as current snapshots. Every omitted transition is gone; what the protocol guarantees is that **no loss is undetectable**, not that all history survives.

The mechanism that makes loss detectable is `first_retained` in the header: the lowest `seq` still present in the body.

- A reader whose next expected `seq` is **at or above** `first_retained` has missed nothing and continues.
- A reader whose next expected `seq` is **below** `first_retained` has a **gap**. It MUST NOT silently jump: it applies the snapshots to rebuild current knowledge, records that occurrences between its cursor and `first_retained` are unrecoverable, and reports the gap to its own consumer.
- Applying snapshots is a resync, not a replay: a `snapshot` is never counted as something that just happened (§8.4.1).

**`link` is an occurrence, not a latest-value.** This has to be decided rather than left to the reader, because the two readings disagree exactly across a gap. A hyperlink the child emitted is an event in a history; the snapshot preserves only the most recent one, so hyperlinks emitted before a gap are genuinely lost and the reader is told so. A consumer that needs every hyperlink must consume faster than the cap, and this document says so rather than implying a completeness it cannot deliver. `state` and `ready`, by contrast, *are* latest-value facts, which is why their snapshots fully restore them.

#### 8.4.5 The conformance matrix

These cases MUST each have a vector, because each one broke something:

| case | required outcome |
|---|---|
| compaction between the reader's stat and its read | identity change detected, reopened, no *undetected* loss |
| reader lagging behind `first_retained` when compaction runs | gap detected and reported, snapshots applied, no silent jump |
| post-compaction append | lands in the new body, reachable through the path — never in the replaced one |
| torn tail after holder loss | readers consume only the complete prefix; no successor writer adopts or appends |
| temporary file left by holder loss | ignored by readers, removed only with confirmed stale/rollback cleanup, never adopted |
| replacement shorter than the old offset | detected |
| replacement exactly the old offset | detected — the case a shrink-check misses |
| replacement longer than the old offset | detected — the case a shrink-check misses |
| two compactions between reads | detected; the second body is not mistaken for the first |
| **crash before the rename** | the complete previous body is intact; no fragment |
| **crash after the rename** | the complete new body is intact; no fragment |
| **crash mid-append** | final line torn but every earlier record intact; the torn line is not consumed |
| POSIX initial header or ordinary append before successful `fsync` | no rendezvous publication for the initial failure; no durable cursor advance, `WAKEUP`, or semantic durable ACK for the append failure |
| POSIX compaction after rename but before successful directory flush | complete replacement may be visible, but namespace durability is reported unknown, the stream closes unwritable, and no semantic durable ACK is sent |
| torn final line | not consumed, completed on the next read |
| cursor past end of file | treated as invalid, re-read, never an error to the operator |
| the record that triggered compaction | present after compaction, exactly once, as a `transition` |
| `ready` and `link` present as snapshots | not reported as new occurrences |
| Windows crash during inactive-body rewrite | previous commit/body remains authoritative |
| Windows crash during alternate commit-slot rewrite | torn slot rejected; previous valid slot selected |
| Windows crash after alternate commit flush | new committed prefix selected |
| Windows body with bytes beyond committed length | tail ignored by readers; old slots are removed only after confirmed retirement and never adopted by a new generation |
| Windows equal commit indexes with different valid records | corruption, fail closed; never choose by slot number |
| semantic source heartbeat loss/disconnect/reconnect | durable `degraded`/`disconnected` transition for a stateful source, new source epoch after reconnect, snapshot required before exactness; never inferred idle |
| edge source connect/assert/disconnect | no `semantic-source` event; only producer-wire `transition` assertions are accepted, and a producer-wire `snapshot` is refused |
| every legal and one illegal `semantic-source` status/reason pairing | legal pairs persist exactly; an illegal pair is malformed and never interpreted |
| semantic event retry after lost durable ACK | original event position returned; no duplicate line |
| application receipt with wrong tuple, source, generation or expired id | refused; no event appended and no pending request resolved |
| `source_seq`/`request_id` at `2^53`, u64 maximum, with a leading zero, and above u64 maximum | the first two round-trip exactly as canonical decimal strings; the latter two are malformed and refused |
| duplicate JSON key in a committed header or event | malformed and refused; no last-key-wins or first-key-wins interpretation |

*Failure prevented:* recovery cost that grew with session age; a restart that replayed every historical transition as current; a compaction that silently severed the supervisor from the session it believed it was watching; and a crash that left the sink holding a fragment of one record.

## 9. Observed terminal state

The program watches the child's output for a small, fixed set of signals and reports them through §8. This is the single narrow exception to §1.1: it observes, and it never alters, delays, or withholds a byte because of what it saw.

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

- A control sequence split across read boundaries MUST be recognised. The scanner retains an incomplete sequence and resumes on the next read. *Failure prevented:* titles and links that happened to straddle a boundary were silently missed, so the derived state was wrong rather than absent — the worst outcome for a consumer that trusts the signal (§5.3).
- Numeric parameters MUST be parsed with a defined result for every input, rejecting out-of-range values rather than wrapping (§5.3).
- The scanner MUST be bounded in memory against a sequence that never terminates, and MUST recover — resuming normal observation — after abandoning one.
- Titles and link targets are **bounded**, not verbatim, and the bounds are frozen in OB-15: a title at 255 bytes, a link target at 2048, truncated at a UTF-8 character boundary, with invalid UTF-8 and embedded NUL replaced by the Unicode replacement character before bounding. Truncation sets the `truncated` flag on the record (§8.2) so a consumer never mistakes a shortened value for a complete one.
- Beyond bounding, the program MUST NOT interpret, resolve, or rewrite them.

## 10. The session protocols

§10.1 fixes generation identity; §10.2 fixes the framed protocol. Together they carry what §2.3, §3.2 and §8.4 depend on. The exact field layouts are in the conformance vectors (§0.2); these sections fix what those vectors must satisfy.

### 10.1 Generation identity **[normative]**

A **generation** is a number identifying one attempt to run one session. It is what makes it possible to say "the holder answering now is the one I started" rather than "something is listening on the path I used".

**The two variables.** The generation is carried into a session through the environment, under two names with different audiences:

- **`<BASENAME>_GENERATION`** — derived from the invoked base name by §4.4.1's byte transformation, but truncated independently so the complete key, including the `_GENERATION` suffix, is at most 127 bytes: the transformed base-name portion is capped at 116 bytes, then `_GENERATION` is appended. Thus `moor` reads `MOOR_GENERATION` and a copy invoked as `atch` reads `ATCH_GENERATION`. Read by the holder, and the value carried on the wire.
- **`DESK_SESSION_GENERATION`** — read by the semantic producer running inside the child, so that what the child reports about itself can be attributed to the same attempt. This one is **not** derived: it belongs to the supervisor's vocabulary, not the holder's, and a child's self-reports must remain attributable no matter which name the holder was invoked under.

  An earlier pass named the first variable as a fixed literal. That was inconsistent: with `_SESSION` derived and `_GENERATION` fixed, a copy invoked as `atch` would read `ATCH_SESSION` alongside `MOOR_GENERATION` — two halves of one identity disagreeing about which program they belong to.

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

**OB-16 fixes it as an inherited private channel** whose other end the launcher holds. The launcher passes only its read handle to the holder and sets `DESK_MOOR_LAUNCH_CHANNEL` to select it: canonical unsigned decimal descriptor text on POSIX, or 1–16 lowercase hexadecimal digits without `0x` for a nonzero 64-bit Windows handle. That environment value is only a selector and proves nothing. The launcher writes §15.1 of the companion schema's exact 32-byte launch record and closes its write end. Supervision is established only when the selected handle is one of the explicit inherited handles and yields that record followed by EOF within 2 seconds, with its generation in the supervised range and equal to both generation variables. The holder consumes and closes the channel, removes the selector before creating the requested child, and never places the handle in the child's inheritance list. A missing selector means unsupervised; a present but malformed selector, wrong handle type, timeout, short/long record, bad reserved byte, or generation mismatch is a failed launch, never a downgrade to unsupervised.

A nested descendant therefore inherits neither the selector nor the handle, while a manually forged channel remains only a same-user freshness assertion and gains no authority (§11.1). This channel is distinct from the instrumentation-load acknowledgement channel of OB-22 — reusing one for both would make an unsupervised `-S` invocation indistinguishable from a supervised launch.

With such a discriminator present, the rules are:

- **Discriminator present, pair present, equal, in the supervised range 2–4294967295** — a supervised launch. Adopt the generation.
- **Discriminator present, pair broken** — one-sided, unequal, out of range, zero, unparsable — refuse to start (§13.1). Guessing which side is authoritative would attribute the session to an attempt that is not the one running.
- **Discriminator absent** — an unsupervised session, whatever the environment says. The session has no generation, and the holder **strips both variables** from the child's environment so they are not inherited further. A stale value from an ancestor must not travel down.

#### 10.1.2 Who owns the allocator

Allocation is durable state, and **OB-18 names its owner: the supervisor, not the holder.** The holder is told its generation and never allocates one. The first allocation for a new logical session key is `2`; every later allocation is the next larger value, with failed attempts burning their values as specified below. The store lives beside the supervisor's own state, is written before the launch, and is recovered by reading it; an unreadable store is a refusal to launch, never a reset. It is keyed by the supervisor's durable logical session key, which is never recycled for another logical session. That key identifies the supervisor's lineage for a named session and survives ordinary Moor `rm`, failed launch cleanup, and later recreation under the same name. It is **not** keyed by OB-17's live rendezvous identity: a Windows marker file id does not exist before launch and deliberately changes when a new marker file is published. The adoption gate binds the preallocated generation and logical launch to the live OB-17 identity plus holder incarnation after publication.

What is already fixed: **allocation is serialised across processes.** Two launchers racing for the same logical session must not both receive the same generation. The allocation is performed under an exclusive claim on that durable logical session key, and the claim is held until the number is committed.

**Durable ordering.** The generation is allocated and committed to stable storage **before** the process is started — never after, never concurrently. The consequences are requirements, not side effects:

- **A failed attempt burns its number.** If the launch fails at any point, that generation is spent. The next attempt uses a strictly greater one. Reuse is forbidden even when the failed attempt demonstrably left nothing behind, because "demonstrably" is exactly the judgement that is unreliable during a partial failure.
- **The record of a spent generation outlives removal of the session.** Removing a session's residue MUST NOT reset the counter: a later session of the same name must not be able to present a generation an earlier one already used, or an acknowledgement from the dead one authenticates the live one.
- **Generations are strictly increasing per session, never reused.**

**Exhaustion.** A legacy allocator may admit a wider range than the wire field carries, but a conforming supervisor applies §10.1's u32 limit before it commits or launches; silently narrowing a larger value would eventually produce zero, the one value that must never recur. **Wrapping is forbidden.** When the next generation would exceed the admissible range, allocation fails: the session is not started, the command exits non-zero, and the diagnostic says the generation space is exhausted rather than reporting a generic launch failure. This is a terminal condition an operator must be able to recognise. Recovery requires an explicit supervisor administration operation that retires the entire logical session lineage and every stored adoption/cursor binding before assigning a fresh never-before-used logical key. Moor `rm`, residue cleanup, marker replacement, and an ordinary retry do not perform that operation and never reset the counter.

**Use on the wire.** The acknowledgement that completes the adoption gate (§3.2), and every subsequent record, carries the exact generation. A record or acknowledgement bearing any other generation is **refused** — not coerced to the current one, not accepted with a warning, not logged and processed. A superseded generation is precisely the case this mechanism exists to catch.

### 10.2 The framed protocol

This section is normative for **what the protocol must guarantee and what it must carry**. The exact field layouts, integer widths, byte order, frozen constants, deadlines and error codes are in the companion artefact — **[moor-wire-schema.md](./moor-wire-schema.md), version `wire-schema-3`** — which an implementer builds against directly. That file also freezes the Windows marker, Windows event commit, and semantic-producer frames. Where the two disagree this section wins and the schema is a defect.

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

1. The controller connects. **Peer identity is checked before any byte of the payload is parsed** (§11); Windows' fixed uninterpreted preface handling is §12.2.
2. The controller sends `HELLO` with protocol version, canonical identity and either the exact nonzero generation it expects or the generation-zero discovery sentinel. Its payload flags are reserved zero: hello never requests an input lease or changes child/viewer state. A supervising caller adopting a launch already knows the generation it allocated and MUST send that exact value; it may not turn a mismatch into success by using discovery. A human attach or side-effect-free liveness probe that has no allocator state may use discovery.
3. The holder sends `HELLO_ACK` with canonical identity, actual nonzero generation and holder incarnation. A side-effect-free liveness probe may stop here, or request `STATUS`/`STATUS_REPLY`, then close. No preamble or attach acknowledgement is sent on that path.
4. A viewer that will attach sends `ATTACH` with geometry and its lease-request flag. The holder always sends the **terminal-state preamble** frame (§10.2.6): the complete canonical restoration block when tracked-mode state is exact, or an empty payload when it is not. It then sends `ATTACH_ACK` carrying the generation, exactness flag and retained-history descriptor (§10.2.5). After the acknowledgement it sends §6.7's frozen `GAP`/`OUTPUT` baseline and then live output on the same ordered connection.

The holder accepts a discovery sentinel only on the first `HELLO`, returns its actual nonzero generation in both the ACK header and payload, and requires that value on every later frame. Zero on any other controller frame is `GENERATION_MISMATCH`. The identity/adoption gate completes only when all four stages have completed through `ATTACH_ACK`, the mandatory preamble was applied first, and the acknowledged generation **equals** the one the caller launched. The viewer's display baseline completes separately when it has processed through the `last retained` record named by that ACK; identity success must not be confused with screen exactness. Silence, malformation, a mismatched peer, or a mismatched generation is a failure — not a retry, not a downgrade. Discovery can establish a human attach or probe but can never satisfy a supervisor's adoption gate. Until that gate completes the session is `indeterminate` (§2.3), not running.

**The whole exchange is bounded.** A peer that connects and then says nothing must not hold a caller past the deadline; the deadline is stated and the timeout is a distinct, reported outcome.

#### 10.2.5 What the acknowledgement and the status descriptor carry **[OB-39]**

A controller attaching to a holder it did not just start must be able to learn everything it would otherwise reconstruct from the filesystem. The acknowledgement, or a bounded status request, carries:

- canonical session identity (OB-17) and generation;
- **holder incarnation** — which run of the holder this is, distinct from the generation;
- **event-stream identity and storage commit** — which path and platform layout this session is writing, plus the active Windows body slot/commit identity when applicable, so a restarted supervisor does not guess (OB-39);
- **start metadata** — wall-clock start for display, monotonic start for arithmetic, **and** a boot identity that makes the monotonic value comparable. OB-31 resolves this as all three, not a choice among them: age is computed from the monotonic pair only when the boot identity matches the consumer's own, and is otherwise reported unknown rather than wrong;
- the child's **working directory** (OB-32);
- **child identity** — process identifier, a containment-set token, and a reuse-resistant birth token (OB-35). On POSIX the token is the process-group identifier; on Windows it is an opaque nonzero u32 minted by the holder and unique within that holder incarnation, never a fabricated job-object identifier;
- the **retained-history descriptor**: the first/last retained output record sequence, the half-open retained byte-offset range, whether that raw history begins at byte zero, and whether the tracked terminal-mode state is exact or degraded (§6.7). There is no screen checkpoint or main/alternate-buffer exactness field in wire v3.

That last field is load-bearing: a consumer must learn *how much history actually remains* before it decides whether to replay or to re-baseline, because the holder cannot replay from an arbitrary offset.

#### 10.2.6 The terminal-state preamble

On every attach the holder sends a preamble frame. When tracked-mode state is exact, it restates the complete state, including explicit resets for modes the child left at their defaults (§1.1, §5.2). A fresh viewer's prior state is unknown, so omitting a known default is not equivalent to restoring it. When the bounded scanner has lost exactness, the frame has an empty payload and the ACK clears its exactness flag; emitting guessed controls would be worse than reporting degradation. Requirements:

- It is **connection-local**: it is addressed to this viewer and is outside durable output history, but its ordinary frame header still carries the connection's exact generation. It carries no record sequence and no output offset, and **must not advance any output cursor or be logged**.
- It is sent **exactly once per attaching connection**, and **before** the attach acknowledgement. A probe that never sends `ATTACH` receives none. A second preamble, a missing preamble on attach, or one after the acknowledgement is a protocol error.
- Its **contents are frozen**: the tracked mode set is enumerated in §6 of the companion schema — twelve modes, and nothing else. §9.1 defers this here, and §5.2 makes it load-bearing: a viewer that arrives mid-session renders wrongly without it.

#### 10.2.7 Capability arbitration

§1.1 fixes the principle: exactly one responder answers a given query — the attached viewer when one can answer within the deadline, otherwise the holder, on a frozen set of query classes. An observer never answers.

**The holder tells the viewer what it is being asked**, on a control frame carrying a correlation identifier, before forwarding the query bytes. The viewer's reply echoes that identifier; a reply echoing an identifier the holder never issued is discarded. Without it the correlation exists only in the reply, which is to say it does not exist.

**The holder answers only about itself.** §4.4.2 preserves an inherited terminal identity and injects one only when none was inherited, so when the identity came from elsewhere the holder does not know that terminal's attributes and MUST NOT fabricate them: it stays silent and lets the viewer answer, or nothing does. A synthetic answer is honest only when the holder is the thing being asked about.

**Cursor position is the exception, and it is viewer-only.** Only something that knows where the cursor is can answer it, and §9.1 forbids the holder from tracking that. With a viewer attached the query passes through and the viewer answers; with no viewer attached **it is not answered at all**. Silence is the honest outcome — synthesising a position would require exactly the screen model this document exists to keep out. This section must freeze the mechanism, and the current implementation is unsound in five ways that the replacement must not reproduce:

- **The query grammar is incremental, not substring matching.** The current implementation matches within a single read, so a query split across reads is missed entirely. Detection must survive arbitrary splitting (§5.3).
- **The response bytes are exact**, and contain **no trailing NUL**. The current implementation includes one.
- **Partial writes are handled.** A reply written partially must be completed, not dropped.
- **Escape-sequence context is respected**: a byte run that merely resembles a query inside another sequence is not one.
- **Arbitration is real.** The holder answers only after establishing that no attached viewer will, within the stated deadline. The current implementation answers regardless.

**A tracked-mode query also requires exact tracked state.** While §6's tracked-mode exactness is true, the holder may synthesise the frozen set/reset answer for a tracked mode. After an invalid, abandoned, or unrepresentable state change clears exactness, the lease-holding viewer still gets the ordinary opportunity to answer, but a silent viewer is followed by holder silence — never a guessed set/reset and never state `0`, which is reserved for a syntactically valid query of a mode outside the tracked set. A reset-to-initial-state sequence may restore exactness as frozen in the companion schema; only then may synthetic tracked-mode answers resume.

**The OB-20 opt-out disables every holder-synthesised reply in this section.** It does not suppress the viewer's opportunity to answer and does not alter the environment identity. Treating it as a reason to discard a viewer reply would turn an opt-out from holder emulation into an input filter.

A reply that is unsolicited, duplicated, of the wrong class, or belonging to a superseded generation or lease MUST NOT reach the child. The opt-out is OB-20; the identity it must agree with is §4.4.2.

**The query classes and the exact reply bytes are enumerated in §8 of the companion schema**, together with the arbitration deadline. **There is no test coverage of this behaviour in the current implementation at all**, so the vectors are the only thing that will hold it: split queries at every boundary, duplicates, a query answered by both parties, a superseded lease, and a partial write.

#### 10.2.8 Size preservation **[OB-19]**

The attach exchange carries the desired size as ordinary fields whose valid range does not include "none", so an automated attach cannot express *do not change it* by omission (§4.3). **OB-19 freezes the representation: zero in either dimension means preserve**, chosen because zero is already outside the valid range of a real size and so cannot be confused with one. Both zero preserves both; one zero and one not is an explicit protocol error rather than a guess. Vectors cover the sentinel, both mixed cases, and the range boundaries.

#### 10.2.9 Input and the transport receipt **[OB-36]**

Input frames carry the generation and are refused if it does not match (§10.1). Wire v3 also carries a flags byte. With `APPLICATION_RECEIPT_REQUIRED` clear, no application-correlation fields are present. With it set, the frame carries a nonzero 16-byte application request id and a source id before the terminal bytes; the complete metadata and bytes are part of replay identity.

The holder returns a **transport receipt** stating what it actually knows: the frame was accepted, the generation and incarnation matched, and the write to the pseudo-terminal **completed**. There is no success value meaning *queued but not yet written* — a receipt is sent when the write is done, or it reports refusal with a frozen numeric cause. A queue slot is not delivery, and a status conflating them would be read as one.

**The receipt carries a request identity, and a retry is safe.** The identity is carried explicitly — it is *not* the frame sequence, which cannot serve: a fragmented input spans several, a retry would have to reuse one, and a reconnect resets the counter. A controller whose input went unacknowledged resends the same request; the holder recognises it, **writes nothing and performs no admission side effect a second time**, and returns the cached written-or-refused receipt payload in a newly sequenced frame. One request is in flight at a time, and a change of input lease resets the numbering. Without this, the only recovery from an unanswered input is to risk writing it twice — which for an agent prompt means submitting it twice.

**The receipt MUST state what it does not prove.** It does not establish that the program running under the terminal read the bytes, parsed them, or acted on them. No session holder can establish that — it is outside what a holder observes. A consumer that treats a transport receipt as evidence of consumption has made an error this document names explicitly. §10.3 supplies a carrier for downstream OB-37 evidence; only a conforming provider integration can supply the fact.

When application evidence is required, the holder accepts the input only if the named source has an active stateful semantic connection advertising both input-notice and application-receipt capabilities. Before writing a byte to the pseudo-terminal it sends that source an `INPUT_NOTICE` carrying the application id, lease/request tuple, byte count and SHA-256 of the exact terminal bytes, and receives the matching prepared acknowledgement within 2 seconds from the still-current producer instance. Failure refuses the input with nothing written and names the reason in the receipt code. After a completed PTY write the correlation becomes eligible for an application receipt. A failed or incomplete write cancels it, returns a refused transport receipt carrying the actual completed byte count and `INPUT_WRITE_FAILED`, and caches that outcome so an exact replay writes no further bytes. This ordering makes the correlation available before the child can consume the bytes without claiming the producer has acted.

#### 10.2.10 Destructive requests

A request to terminate a session names the expected session identity, the generation, and the holder incarnation. The holder refuses atomically on any mismatch. A name alone is not sufficient authority: between the check and the command the named rendezvous may belong to a successor, and the operation would kill it. The identity re-check in §2.1 covers unlinking a filesystem object; it does not cover killing a listener.

Outcomes are the algebra of **OB-33** and are reported distinctly: terminated, already gone, refused on identity, indeterminate, failed.

#### 10.2.11 Notification and liveness **[OB-30]**

Two surfaces, deliberately separate:

- **A coalescible wakeup** telling a consumer that the event stream has advanced, so no consumer polls. Coalescing is explicit: several records may produce one wakeup, and the consumer reads the durable stream to learn what happened.
- **A holder and stream liveness signal**, distinct from the above. Silence on the wakeup channel is the normal state of a quiet session and MUST NOT be readable as death. Conflating the two is how a healthy idle session gets reported as lost.

The holder sends a heartbeat every 5 seconds and immediately when child-running or stream-writable changes. Fifteen seconds without one invalidates the connection's verified-live evidence and triggers a fresh bounded identity probe. Until that probe positively establishes a listener's absence or completes an authenticated exchange, the session is `indeterminate` under §2.3 — heartbeat loss alone is never proof that the holder is gone.

#### 10.2.12 Error taxonomy

Every refusal names its cause from a frozen set, at minimum: unknown version, unknown type, oversized frame, malformed frame, bad sequence, reassembly aborted, generation mismatch, identity mismatch, unauthorised peer, deadline exceeded, and resource exhausted. A consumer branches on these — a single generic failure is what forces the guessing this document keeps removing.

### 10.3 Semantic producer ingress **[NEW — ownership, provenance, recovery and correlation]**

Moor owns a local transport for facts that a provider integration can authoritatively emit. Moor does **not** decide what those facts mean. Semantic ingress is available only while the durable event stream is enabled and writable: without `-T`, no semantic token is injected and a `MOOS` hello is refused with `SEM_CAPABILITY_ABSENT`; after the sink becomes unwritable, no further semantic event can be accepted or acknowledged as durable, current semantic connections are closed with `SEM_RESOURCE_EXHAUSTED`, and controller status/heartbeat exposes the unwritable stream. The ownership boundary is exact:

- a provider adapter owns whether an assertion is true and the point in the provider lifecycle from which it is emitted;
- Moor owns same-user transport, holder-incarnation freshness, source provenance, ordering, deduplication, bounded recovery and durable publication into event schema v2;
- Desk owns provider-specific payload interpretation, precedence, lifecycle reduction and every product action derived from the assertion.

#### 10.3.1 Endpoint, discovery and freshness

Semantic producers connect to the same addressable session rendezvous as controllers. After the OS peer-identity check and before parsing any payload, the first four bytes select `MOOR` controller wire v3 or `MOOS` semantic wire v1. They are separate protocols; a frame from one is never accepted in the other.

When the event stream is enabled, the holder mints a cryptographically random 16-byte semantic token once per holder incarnation and injects its lowercase 32-hex encoding as `DESK_SESSION_SEMANTIC_TOKEN` into the initial child. It does not define that variable when the stream is disabled. Producers discover the rendezvous through the existing derived `_SESSION` value and carry the current `DESK_SESSION_GENERATION` when supervised. The token is a **freshness and session-binding value, not authorisation**: same-user peer identity remains the security decision, and another process of that user is trusted by §11.1. A token from an older holder or another session is refused.

Each producer identifies a stable ASCII source id (1–128 bytes, `[A-Za-z0-9._-]`), a random 16-byte producer instance, a mode, and capabilities. `stateful` means the connection claims continuing knowledge and heartbeats; `edge` means a one-shot invocation that may report an occurrence but whose silence or exit says nothing. The first accepted connection fixes that source id's mode for the holder incarnation; a later hello changing edge to stateful or vice versa is `SEM_SOURCE_CONFLICT`, not a reinterpretation of existing events. Only one stateful connection per source is current. A same-mode replacement receives a new nonzero `source_epoch`, supersedes the old connection, and requires a complete snapshot before that source is exact. An edge source never becomes exact, emits no `semantic-source` lifecycle record on connect or disconnect, and may publish only producer-wire `transition` assertions.

#### 10.3.2 Ordering, deduplication and durable ACK

Within one source epoch, newly accepted semantic event `source_seq` starts at 1 and advances by exactly one. Each assertion or application receipt also carries a producer-chosen 16-byte `event_id`. The holder retains the last 512 accepted `(source_seq, event_id, SHA-256 of the exact complete reassembled event payload, durable position)` tuples for that source epoch for the holder lifetime; the digest excludes transport headers and frame sequence so a retry may be fragmented differently. A retry of any retained tuple with identical payload bytes returns a newly sequenced duplicate ACK naming the original durable position and appends nothing; the same id or sequence with different bytes is `SEM_EVENT_CONFLICT`. A sequence below the high-water mark that is no longer retained, or one above high-water plus one, is refused as bad sequence. The unsigned 64-bit source sequence never wraps: `FFFFFFFFFFFFFFFF` may be the final accepted event of an epoch, after which another new event is `SEM_RESOURCE_EXHAUSTED` and a stateful producer must reconnect into a new epoch and snapshot again. The 512-entry bound is admission control: when the holder cannot retain the required tuple it refuses before acknowledging, never silently weakens deduplication.

An accepted ACK includes the durable event `(epoch, seq)` and is sent only after the event-storage commit in §8.4.2. A refused ACK carries a frozen semantic error code; a connection-level refusal that cannot identify an event uses `SEMANTIC_ERROR` instead. A lost ACK can therefore be retried without a second event. Assertions are a UTF-8 JSON object at most 32 KiB, maximum depth 64 and at most 1024 members per object; duplicate keys, non-finite numbers, invalid UTF-8 and any non-object top level are rejected. Moor validates these envelope properties, preserves the exact bytes as canonical padded base64, and does not interpret provider keys.

#### 10.3.3 Recovery is degraded, never guessed idle

A stateful producer sends a complete producer-wire `snapshot` before transitions. Its first publication is a `semantic-assertion` event with `kind:"transition"` and `assertion_kind:"snapshot"`; only compaction-generated restatements use event `kind:"snapshot"`. Until that assertion is durably committed, its `semantic-source` status is `connected`, not `exact`. Heartbeats are every 5 seconds; 15 seconds without one changes the source to `degraded` and records why. A transport close changes it to `disconnected`. Either condition removes exactness, and even the same connection can regain `exact` only by durably publishing a fresh complete snapshot. A new connection gets a new source epoch and must snapshot again. Neither loss nor silence is converted to `idle`, `ready`, `done`, or any provider state. Desk may fall back to lower-quality evidence under its own lattice, but it can see from provenance that the high-quality source is absent.

Edge assertions are durable occurrences. They cannot establish continuing exact state and are not retained as state snapshots during compaction. An edge producer sending producer-wire assertion kind `snapshot` is refused with `SEM_INVALID_PAYLOAD`; accepting it as an occurrence would preserve a field that falsely claims continuing state, while retaining it would be worse. The latest exact stateful snapshot and latest stateful-source status are retained as §8.4.4 specifies.

#### 10.3.4 Application correlation and the limit of OB-37

For a required application receipt, the controller chooses an application request id not used by another pending or retained correlation in this holder and reuses it under the same lease/request tuple only for an exact replay of the same `INPUT`. The id is not a generation-long uniqueness ledger: after its correlation resolves or expires it may be used with a later, never-reused lease/request tuple, and an old receipt still cannot match because every tuple field is checked. Before writing, Moor binds the correlation to the then-current source epoch and producer instance. Moor retains at most **512 written correlations total per holder** for 10 minutes; any one source may consume the whole allowance, but no source receives a separate 512-entry pool. The status descriptor's pending count is this same holder-wide value. Admission fails before the PTY write when that bound is full. At 60 seconds without a receipt Moor emits `application-receipt-missing{reason:"deadline"}` but retains the correlation. The bound producer's first heartbeat loss to `degraded` or transport close to `disconnected` emits `reason:"source-lost"`; a later transition between those two states does not emit it again. Final expiry emits `reason:"retention-expired"` and removes the correlation. Each reason is emitted at most once per correlation, and every missing record names the bound producer and source epoch. A valid accepted **or refused** provider receipt must arrive from the same producer connection and match source, source epoch, application id, lease epoch and request id of a correlation whose transport write completed. It resolves and removes the correlation only after the `application-receipt` event is durable. The retained semantic-event deduplication check runs first: an exact retry after resolution returns the original durable position and does not try to resolve again; a new event naming a resolved or expired tuple is unknown. Mismatch, pre-write receipt, stale generation/source epoch, superseded producer, or unknown/expired request fails closed.

This carrier makes provider proof possible; it does not manufacture it. **OB-37 remains a per-provider runtime gate.** A provider/version closes its gate only after a real shipped integration demonstrates that: the application id reaches a named authoritative provider point; that point emits the matching receipt; wrong, missing, stale and duplicated ids fail closed; crash/reconnect behavior follows this section; and Desk consumes the durable event end to end. Until that evidence exists for the provider actually deployed, Desk retains screen observation, repeated-submit handling and the durable `semantic-unknown` state. A schema entry or a green isolated encoder test is not that evidence (§5.5).

## 11. Security model

### 11.1 What is trusted

**The invoking user is trusted. Nothing else is.** Not the child's output (§5.3), not a connecting peer, not a caller-supplied path, not the contents of any file the holder did not itself write.

Authorisation is **same-user**: a session may be driven only by a process running as the user that created it. There is no delegation, no group access, no capability handed to another account. A holder discovering otherwise refuses and says so.

### 11.2 Peer identity

**Every accepted connection has its peer's identity checked before a single byte of its payload is parsed** (§5.5). On Windows the fixed pre-authentication read required by `ImpersonateNamedPipeClient` is an inert bounded read, not parsing (§12.2); those bytes are not interpreted or admitted to protocol state until identity succeeds. A connection from another user consumes nothing beyond that fixed authentication buffer — no capability, no lease, no generation, no session state, no reassembly state — and is closed with a stated error.

The mechanism is platform-specific and is reached through the abstraction §5.4 requires; an unsupported platform fails to build rather than omitting the check.

**On Windows this determines the carrier.** Windows offers no peer-credential mechanism on a Unix-domain socket, so the check would be unimplementable over one; the addressable object is a protected marker and the carrier is its protected named pipe, whose impersonated client token supplies the identity (§12.2). The alternative — one socket type everywhere, and on Windows no check at all — was rejected.

### 11.3 The rendezvous **[OB-21]**

- The socket, or on Windows marker and named pipe (§12.2), is created **reachable only by the invoking user and the platform administrator identity explicitly permitted in §2.2** — mode `0600` on Linux and macOS, the protected DACL on Windows. A **bare name** places the addressable object inside the enforced root. A **path form** places it exactly where the caller said, outside that root: the final socket/marker/pipe protection still applies, the parent is never created, and parent-directory safety is the caller's responsibility.
- It is **published atomically**: the POSIX socket or Windows marker is never observable at its final path in a state where connecting would reach a half-built holder (§3.2, §12.2).
- Before unlinking a socket or marker, its type and identity are re-verified immediately beforehand (§2.1), and it is unlinked only in the `stale` state (§3.7). A Windows reparse point is never treated as stale session residue.

### 11.4 Caller-supplied paths — one rule **[OB-21]**

The event sink (§8.1), redirected standard error (§4.6), and launch-time instrumentation object (§4.7) are three caller-supplied paths. They share one validation rule; their required object types remain explicit at the owning sections because the Windows event sink is a directory rather than a regular file:

1. The **creating process** opens and validates the path — never a forked child, never later.
2. Validation happens **before the rendezvous is published**, so a failure leaves no session behind.
3. The open cannot block and cannot follow a symbolic link or Windows reparse point.
4. The target is owned by the invoking user and has the exact POSIX mode/protection rule or protected Windows DACL required by its owning section. Standard error uses exact owner-only protection; the executable instrumentation object may be readable/executable but never writable by group/other or another Windows principal (§4.7). Standard error and the instrumentation object are regular files; the event target is a regular file on POSIX and a directory containing four fixed regular files on Windows (§8.1).
5. Only already validated open descriptors or handles are passed onward. On Windows the creating process opens the directory reparse-safe, creates and opens the four slots against that verified object before publication, and passes the slot handles; no component reopens the caller's path and thereby permits substitution between check and use.
6. Any failure is fatal and reported (§13.1).

The event sink additionally resides inside the session root (§8.1), because a privileged supervisor reads it and its location is part of what makes it addressable after a restart (§10.2.5). Its platform-specific file/directory shape is a carrier requirement, not an exception to validation.

### 11.5 The ownership fence on destructive operations

Terminating a session requires **proven ownership**, not a matching name. A listener that answers but does not complete the identity exchange, or answers with the wrong generation, is `indeterminate` (§2.3) and **MUST NOT be terminated** — it may be a stranger's process, or a successor to the session the caller meant.

After a failed launch, terminating is permitted only against the launch identity that failed. Retirement succeeds only when termination completed **and** the addressable rendezvous object is gone; an uncertain outcome is recorded for retry rather than reported as success (§10.2.10, OB-33).

### 11.6 The event sink's lifecycle **[OB-27]**

- Pre-created with owner-only protection **before** the session starts, by the creating process: one empty file on POSIX; one empty directory on Windows, followed by creation and opening of the four fixed slots required by §8.1.
- **Bound to the generation** that owns it: a sink from a previous generation is never adopted.
- **Removed only on confirmed termination or confirmed rollback** — never on a merely attempted one. POSIX removes the file. Windows closes and removes the four fixed slots and then the now-empty directory, all after retirement is confirmed.
- **A write failure after the session is running is reported, not swallowed.** §8.1 refuses to start a session whose sink cannot be honoured; a sink that becomes unwritable afterwards is the same condition discovered later, and the supervisor must learn. A session that goes silently unobserved is the failure §8 exists to prevent.

The window between creation and binding is where a sink can be substituted, which is why the creating process opens and validates every object before publication and passes only those open descriptors or handles.

### 11.7 What the child inherits

Before the child starts: **the child inherits nothing the holder did not intend.** On Linux and macOS every descriptor not explicitly required is closed and inherited signal dispositions and the signal mask are reset (§5.5); on Windows inheritance is granted per handle and granted to none but the required ones, and the POSIX signal-disposition clause does not apply (§12.3). No handle belonging to the holder's own logging or state is left open in either case. A child must not be able to write to the session log, event sink or holder rendezvous.

## 12. Platform behaviour

The supported families are Linux, macOS and native Windows. "Any Linux" is not a support claim. Release conformance MUST cover the concrete matrix in §12.8; builds outside it may work but are not represented as supported until added with the same evidence. WSL1 and WSL2 are Linux lanes, not substitutes for native Windows.

### 12.1 What is genuinely portable

Controller wire v3, semantic wire v1, JSON event schema v2, checksums, integer byte order, generations, correlations and provenance are portable. Native path values are not: POSIX surfaces carry raw bytes; Windows byte/JSON surfaces carry canonical WTF-8 converted losslessly from UTF-16, including unpaired surrogates. A decoder rejects overlong, non-canonical or non-round-tripping WTF-8.

### 12.2 Windows rendezvous and peer identity **[decision]**

Linux and macOS publish the Unix-domain socket itself. Windows publishes a protected marker file at the addressable path; the marker names a protected local named pipe. The marker's binary layout is frozen in wire-schema-3.

Before marker publication, the holder reads exactly 16 bytes from the operating system's cryptographic random source and encodes them as 32 lowercase hexadecimal digits. The pipe name is exactly `\\.\pipe\moor-` followed by those digits; no other generated spelling is conforming. It creates the first byte-mode listening instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`. A name collision or failure to obtain that first instance fails the unpublished launch; Moor never connects clients to a pre-existing instance. After that ownership proof, the same holder may create further listening instances under the identical name, DACL and local-only/overlapped/byte-mode flags **without** `FILE_FLAG_FIRST_PIPE_INSTANCE`, as Windows requires for concurrent clients. No peer receives pipe-instance-creation rights. The initial secured instance and all holder state needed for the identity exchange exist before the marker is renamed into place.

The marker is written to a new sibling in the same directory and flushed. While holding that verified regular non-reparse file open, the creator queries its volume serial and `FILE_ID_INFO.FileId`; that tag-`02` identity is used in the initial event header, which is committed before publication. The marker is then atomically renamed into place, re-opened by final path with reparse-point semantics, and required to have the exact same file identity, type, owner and DACL before the holder admits a connection. A mismatch tears down the still-unadmitted pipe and removes the final marker only after rechecking that it is the staged file. The final marker grants read only to the invoking user and `LOCAL_SYSTEM`. A reader performs the same final-path type/owner/DACL checks before parsing, verifies the record CRC and generation, and then connects to the named pipe. It never follows a marker reparse target and never trusts a pipe name from an unverified marker.

The named pipe is byte mode, overlapped, local-only (`PIPE_REJECT_REMOTE_CLIENTS`) and protected by a non-inheriting DACL granting only the invoking user and `LOCAL_SYSTEM` the individual read/write/synchronise rights needed to connect. `FILE_GENERIC_WRITE` is not granted because it includes pipe-instance creation.

Windows impersonation is tied to the last client message read, so connect alone is insufficient. A connection-authentication worker with no session capabilities accumulates exactly four bytes into a fixed buffer under the 2-second identity deadline, accepting arbitrary short reads but never requesting or consuming a fifth byte, and does not interpret them. It then calls `ImpersonateNamedPipeClient`, reads `TokenUser`, requires an exact SID match to the invoking user, and calls `RevertToSelf`. Only after successful reversion is the pipe and buffered preface admitted to the normal event loop, where those bytes are first parsed as `MOOR` or `MOOS`. EOF or deadline with fewer than four bytes, impersonation failure, token-query failure, SID mismatch, or reversion failure closes the connection and discards the buffer before any protocol payload is parsed; on reversion failure the isolated worker itself is retired rather than allowing an impersonated thread to execute session work. Marker protection, first-instance creation, pipe protection, the bounded pre-authentication accumulation, token verification and successful reversion are all required; none substitutes for another.

### 12.3 Child launch and terminal boundary

Native Windows support starts at Windows 10 version 1809 / Windows Server 2019 and uses ConPTY. The holder creates a pseudoconsole at the selected geometry and starts a small in-console bootstrap suspended with an explicit handle list. It creates a new no-breakaway job with kill-on-close, assigns the suspended bootstrap to that job, establishes the private bootstrap control/identity channel, and only then resumes the bootstrap. If job assignment or nested-job compatibility fails, launch fails before publication; the requested child is never run outside the promised containment as a fallback.

The bootstrap creates the **requested child suspended** as a new console process group, with only its explicit handle list, and reports its process handle, real PID and birth token to the holder. Job membership must already apply before any requested-child thread can run. When `-S` is present, a matching-architecture instrumentation path inserts the named DLL into that suspended requested child and receives the acknowledgement from inside that same process; an ACK from the bootstrap or helper is not sufficient. The holder resumes the requested child's initial thread only after that ACK, or immediately after identity/containment establishment when `-S` is absent. Any failure terminates the owned job and leaves no published rendezvous.

The bootstrap remains inside the pseudoconsole solely to perform control operations that require sharing that console. Moor passes ConPTY's UTF-8 VT byte stream unchanged. It does not claim that legacy Console API calls are themselves byte-transparent (§1.1).

On POSIX, a signal handler only records an atomic flag and wakes the normal event loop; it never calls `exit`, allocates, logs, closes descriptors or runs cleanup. Windows console-control callbacks follow the same record-and-wake rule. Cleanup and child termination run on the normal path, and shutdown remains bounded even while controller, PTY, log or event I/O is blocked. OB-42 conformance drives these paths through the shipped binary rather than a handler unit test.

### 12.4 Process containment and termination **[OB-34]**

- **Linux and macOS:** graceful termination sends `SIGTERM` to the terminal foreground process group; if no foreground group can be identified or that dispatch fails, it falls back to the requested child's process group. Force, `-f`, or escalation after 5 seconds uses the same targeting rule with `SIGKILL`. Descendants in other/detached groups are explicitly not reached.
- **Windows:** ordinary `CreateProcess` descendants inherit job membership while WMI/broker-created or explicitly breakaway processes may not. No breakaway flag is granted by Moor. Graceful termination asks the in-console bootstrap to send `CTRL_BREAK_EVENT` to the requested child's process group and waits 5 seconds. Force, escalation, bootstrap loss, or `-f` calls `TerminateJobObject` with exit code `0xC000013A` (`STATUS_CONTROL_C_EXIT`); that exact unsigned DWORD is preserved in the exit event and foreground `run` result. Only processes actually in the job are guaranteed to end; Moor never claims causal descendants outside it were reached.

The status descriptor's 4-byte containment value is the POSIX process-group id or, on Windows, a holder-minted opaque nonzero token unique for the holder incarnation. Win32 exposes a job handle, not a stable u32 job identifier, so no identifier is fabricated. `TERMINATE_RESULT` states the mechanism and whether a known survivor escaped the covered set.

### 12.5 Windows event storage

Windows uses the fixed dual-body/dual-commit protocol of §8.4.2, not replace-over-open. File identity alone is not a commit. Readers select the highest valid commit record and treat its body prefix/hash as storage identity. Crash injection MUST cover every byte boundary of body write, body flush, commit write and commit flush, plus recovery with either slot torn, both slots valid, equal conflicting commits, and uncommitted body tails.

### 12.6 Boot identity and start arithmetic

Wall-clock start is unsigned milliseconds since the Unix epoch on every platform. Monotonic start and the exact 16-byte boot identity are platform-specific but paired:

- **Linux and WSL:** monotonic milliseconds come from `CLOCK_BOOTTIME`. The identity is the 16 UUID bytes parsed from `/proc/sys/kernel/random/boot_id`; only the canonical UUID grammar (hex case-insensitive on input, exact `8-4-4-4-12` grouping) is accepted.
- **macOS:** monotonic milliseconds come from `mach_continuous_time` converted with its timebase. The identity encodes the `kern.boottime` `timeval`: unsigned seconds in little-endian bytes 0–7, microseconds 0–999999 in little-endian bytes 8–11, and ASCII `MAC1` in bytes 12–15.
- **Windows:** monotonic milliseconds come from `GetTickCount64`. The identity is derived from documented `Win32_OperatingSystem.LastBootUpTime`: convert the CIM datetime and its offset to UTC, convert that instant to unsigned 64-bit Windows FILETIME ticks, encode it little-endian in bytes 0–7, and set bytes 8–15 to zero. The WMI lookup has a 2-second whole-operation deadline and cannot delay rendezvous publication indefinitely. No undocumented system-information class is a conformance dependency.

If the platform identity source is unavailable, times out, is malformed, or cannot be converted exactly, the identity is sixteen zero bytes and **never compares equal**, including to another zero value; monotonic age is then unknown rather than guessed. A consumer computes age only when its own freshly read identity equals the holder's nonzero identity byte-for-byte, using its matching platform monotonic clock. Wall time is display metadata and never participates in age arithmetic.

### 12.7 Exit status domain and default child

POSIX child status is 0–255 or a signal. Windows `GetExitCodeProcess` supplies an unsigned 32-bit code. Event schema v2 and the durable exit record preserve all 32 bits; foreground `run` exits through `ExitProcess(code)` with the same DWORD. A shell that chooses to display that DWORD as signed does not change the wire value. When Moor itself terminated the Windows job, it additionally records `ended:"terminated"` with the known `method`; an external termination remains indistinguishable from an ordinary `"exited"` code.

The holder's own command-error statuses, including 127 for a child that could not be started, apply on all three families. On Windows, lookup uses the operating system's case-insensitive environment semantics: nonempty `SHELL`, then nonempty `COMSPEC`, then `GetSystemDirectoryW` joined with `cmd.exe`. A selected value is a native executable path with no embedded arguments; invalidity is a child-start failure, not permission to continue down the list. There is no account-database step.

### 12.8 Required release conformance matrix

Every release claiming all three families MUST publish results from at least these lanes against the shipped artifacts, not only libraries:

| family | minimum lanes |
|---|---|
| Linux glibc | Ubuntu 22.04 x86_64, kernel 5.15 or newer; Ubuntu 24.04 arm64 |
| Linux musl | Alpine 3.20 x86_64 and arm64 |
| WSL | WSL1 and WSL2 with Ubuntu 22.04 userland |
| macOS | macOS 13 or newer on Intel x86_64 and Apple Silicon arm64 |
| native Windows | Windows 10 1809 x64, Windows Server 2019 x64, and Windows 11 arm64; each with VT-native and legacy Console API children |

Every lane exercises create/attach/detach/input/replay/termination, same-user and wrong-user peers, generation/incarnation fencing, `-S` success plus wrong-architecture failure, event recovery, and real supervisor restart while the holder lives. The native-Windows lanes additionally exercise hostile inherited ACLs, marker/root/slot reparse points, remote-pipe refusal, impersonation failure, nested-job refusal, graceful CTRL_BREAK and forced job termination, every dual-slot crash prefix, UTF-16 names including an unpaired surrogate, full u32 exit codes, and x64/arm64 instrumentation. A missing lane narrows the release's stated support; it is not converted into a paper waiver.

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
| `attach` with no controlling terminal | **1** |
| `current` outside any session (prints nothing) | **1** |
| **[NEW]** any command refused because the session is `indeterminate` (§3.7) | **1**, with a diagnostic naming the state |
| **[NEW]** valid supervised-launch discriminator present but the inherited generation pair is one-sided, unequal, out of range, or unparsable (§10.1.1) | **1**, naming which condition failed; without a discriminator the launch is unsupervised and the inherited pair is stripped |
| **[NEW]** supervised-launch selector or private record invalid, mismatched, overlong, or timed out (§10.1.1) | **1**, naming the failed launch-channel condition; never downgraded to unsupervised |
| **[NEW]** `_SESSION_V2` malformed or non-round-tripping (§4.4.1) | **1**, standard error: `<program-name>: session ancestry v2 is malformed` |
| **[NEW]** decoded `_SESSION_V2` does not reproduce the legacy `_SESSION` value (§4.4.1) | **1**, standard error: `<program-name>: session ancestry carriers disagree` |
| **[NEW]** generation space exhausted for this session (§10.1) | **1**, `generation space exhausted for session '<name>'` — distinct from a generic launch failure, because no retry reaches it |
| **[NEW]** session root exists but is not an owner-only directory owned by the caller (§2.2) | **1**, naming the path and the offending mode or owner |
| **[NEW]** event sink rejected (§8.1) | **1** |
| **[NEW]** stderr sink rejected (§4.6) | **1** |
| **[NEW]** launch-time instrumentation object missing, rejected, wrong-architecture, or unacknowledged (§4.7) | **1** |

The child's status passes through unchanged across the full range, verified at 0, 7 and 255. The **127** for a failed start is the conventional shell value and is deliberate: a caller cannot otherwise distinguish "your program is not there" from "your program ran and returned 127", and it accepts that ambiguity in exchange for matching what every shell already does.

### 13.2 A terminated child reports 1, and that is not enough

Verified against the reference: a child killed by any signal yields status **1** and an `exit` event carrying code **1**. A child killed by a segmentation fault, a child killed by the operator, and a child that simply returned 1 are indistinguishable — to the shell and to the supervisor alike.

This document resolves that asymmetrically, using the split in §0.2:

- **The holder's exit status stays 1.** Scripts on disk branch on it today and §0.2 promises them no change. The tempting convention of `128 + signal` MUST NOT be adopted; a caller comparing against 1 would silently stop matching.
- **[NEW] The event stream carries the truth, where the platform has it to give.** POSIX distinguishes a normal exit from a signal and names the signal. Windows normally supplies only an unsigned 32-bit exit code, so an externally ended process remains `ended:"exited"`; but when Moor itself initiated graceful CTRL_BREAK or forced job termination it knows that fact and records `ended:"terminated"` with `method:"graceful"|"forced"` (§12.7). It does not infer a method for an external termination.

The branch encoding is fixed in §8.2. This section owns only why the platform branches differ.

### 13.3 Message uniformity

Every diagnostic MUST be `<program-name>: <message>`, with `<program-name>` derived from the invoked base-name bytes and rendered through OB-29's same reversible ASCII rule. The derivation remains "as invoked"; the rendering only prevents that name from injecting the diagnostic line (§3.6). Two departures in the reference are **[NEW]** corrections:

- A missing session reports `session '<name>' does not exist` from `kill` and `rm`, `no log for session '<name>'` from `tail`, and a raw system error carrying the **absolute socket path** from `push`. The first two are frozen. The third MUST be brought into line: it leaks the session root's layout, and it is the only diagnostic a caller cannot match with the same pattern as the rest.

- **[NEW] "Does not exist" and "is not running" are different facts and MUST NOT share a message.** §3.7 makes `kill` fail against a *stale* session. A stale session leaves something behind — an orphaned rendezvous object, an exit record, or both (§3.3) — so the frozen `does not exist` would be false exactly in the case §3.7 added. A stale target reports `session '<name>' is not running`, whichever artefact shape it has; an absent one — nothing on disk under that name at all — keeps `session '<name>' does not exist`; an indeterminate one reports `session '<name>' could not be identified` (§3.7). Three states, three messages: a caller that cannot tell them apart cannot choose between creating, cleaning up, and investigating.
- A message MUST NOT be the empty string. `current` outside a session is the sole exception, frozen in §3.3 for the shell idiom that depends on it.

### 13.4 What `-q` suppresses

`-q` suppresses the program's **informational** messages — session created, session killed, session removed, the removal count. It MUST NOT suppress any diagnostic accompanying a non-zero exit. A caller uses `-q` to keep success quiet; a caller that also silenced failure would have no way to learn why a session did not start, and the sinks in §4.6 and §4.7 make "did not start" the common case for a misconfigured supervisor.

---

## 14. The decisions register

Every obligation this document raised is recorded here with its resolution. One remains genuinely open, and it is open because it cannot be answered by a session holder at all; everything else is decided, and the decision is stated so that an implementer never has to infer one.

**Where a decision was a judgement call rather than a measurement, it says so.** A reader who disagrees with one can change it in one place.

### 14.1 The three product choices — closed with defaults

These had no technically correct answer. They are closed here with the recommended default so the document is implementable; each is reversible in one edit, and the operator may override any of them.

| id | decision |
|---|---|
| **OB-24** | A finite list of exactly twelve breaking safety corrections, frozen below with its migration |
| **OB-6** | Dual-write the legacy ancestry plus the unambiguous versioned `_SESSION_V2` carrier; new readers use V2 |
| **OB-1** | Reserve `.log`, `.events`, and `.exit` under the platform comparison; reject every bare/path final component that collides or uses a Windows alias form |

#### OB-24 — the complete compatibility exception

The breaking corrections are exactly:

1. the session-root ownership and permission check (§2.2);
2. rejection of unknown dash-leading tokens (§3.1);
3. strict numeric operands and rejection of trailing operands (§3.6);
4. the versioned ancestry carrier, corrected `current` parsing for delimiter-bearing paths, and control-byte escaping on line-oriented name surfaces (OB-6, OB-29);
5. the alias-safe reserved-suffix restriction on every session name form (OB-1);
6. **refusal to act on an `indeterminate` session** — `list` gains the `[indeterminate]` word and the `since unknown` age for incomparable boot identities, while `rm`, `kill`, `push` and every creating form refuse rather than proceeding (§3.3, §3.7, OB-2);
7. **`session '<name>' is not running`** as a distinct diagnostic from `does not exist` (§13.3);
8. **uniform `push` diagnostics**, no longer leaking the socket path (§3.4);
9. **`-2` fails closed** where it previously ignored an unusable path silently (§4.6);
10. **`-S` fails closed** where it previously ran the child without the library (§4.7);
11. **self-attach refusal decided from live ancestry** rather than the inherited variable (§3.3, OB-41);
12. **the Windows `-T` sink changes from one pre-created file to one pre-created empty directory containing the holder-created fixed dual-body/dual-commit slots** (§8.1, §8.4.2).

The migration for correction 12 is an atomic coordinated cutover: the Windows supervisor creates an empty directory and consumes event schema v2/commit format 1 with wire v3; no mixed reader/writer window exists. Corrections 4 and 5 require their inventory-and-drain migrations below. Nothing is added to this list without a decision of the same weight. Strict parity was rejected because it would deliberately reproduce known defects; each named correction closes a specific way to lose control of a session silently.

#### OB-6 — versioned ancestry carrier

The holder dual-writes the legacy derived `_SESSION` value and the derived `_SESSION_V2` value frozen in §4.4.1. V2 is `v2:` plus colon-separated canonical padded-base64 encodings of the absolute native rendezvous paths; POSIX encodes native bytes and Windows encodes canonical round-tripping WTF-8. New `current` and supervisor code use V2, require exact agreement with the simultaneously written legacy value, and fall back only for a pre-cutover session with no V2. Before cutover, inventory and drain every legacy session whose absolute ancestry contains a colon, because its old carrier cannot be recovered unambiguously; inventory control-byte names as well, because OB-29 deliberately changes their human line rendering to escaped form. This is the only option that handles Windows drive paths and arbitrary parent/base-name paths while leaving the legacy environment bytes untouched for existing consumers.

#### OB-1 — reserved suffix grammar

The reserved suffixes are exactly **`.log`**, **`.events`**, and **`.exit`**. Companion state is the session rendezvous path with one of these appended, and the restriction applies to the final native path component of **both bare and path-form names**.

On POSIX the suffix comparison is byte-exact. On Windows it is ordinal ASCII-case-insensitive, and the final component is additionally refused when it contains `:` or ends in U+0020 SPACE or U+002E FULL STOP. The drive designator is outside the final component and is unaffected. Refusing `:` prevents a marker from being addressed as an alternate data stream; refusing trailing spaces/dots prevents Win32 path trimming from turning a spelling that passed the suffix check into the reserved spelling; case-insensitive comparison closes `.LOG` and equivalent aliases even on a volume configured for case-sensitive lookup. These are rejections, not normalisations: Moor never changes the caller's spelling into a different session name. The type and OB-17 identity rechecks still apply to every opened object and close aliases that cannot be excluded lexically.

The grammar is extensible only by a decision of this weight. Before enforcement, inventory and deliberately drain suffix-colliding sessions under the platform comparison above, including Windows case aliases, alternate-stream spellings, and trailing-space/dot spellings. This keeps the on-disk layout flat and legible and avoids moving companion files without removing their ambiguity.

### 14.2 Engineering decisions — closed

| id | resolution |
|---|---|
| **OB-2** | A comparable monotonic age uses the largest whole unit that fits, rendered in `<age-text>` as `<n>s ago`, `<n>m ago`, `<n>h ago`, or `<n>d ago`, truncated toward zero. When boot identity is unavailable/different or monotonic subtraction would be negative, `<age-text>` is exactly `unknown`; wall-clock subtraction is never substituted |
| **OB-3** | Numeric operands: a non-empty decimal string, optionally followed by one suffix from `k`, `m`, `g`, case-insensitive, multiplying by 1024, 1024², 1024³. No sign, no leading zeros, no trailing bytes. The value must fit the field it configures; the log cap and the sink cap are 64-bit, the line count is 32-bit. Anything else is the argument error of §3.6 |
| **OB-4** | Options are recognised after the command token and on either side of the session name. The child's arguments begin at the first token that is neither an option nor an option's value. `--` ends option processing in every phase. The same grammar applies to legacy modes and to the bare form. Every spelling in §3.6 gets a vector at each of the three positions |
| **OB-5** | Resolved in §4.2: POSIX transfers the complete terminal settings; Windows exposes the ConPTY UTF-8 VT boundary and geometry and makes no false POSIX line-discipline claim |
| **OB-7** | The per-viewer output buffer is bounded at **4 MiB of child payload** across replay and live output. Pinned replay records count once for that viewer; framing metadata is separately fixed-size. A viewer that exceeds the payload bound is disconnected; the session is unaffected (§5.1, §6.7) |
| **OB-8** | `list` bounds the whole operation at **2 seconds**, probing concurrently. Anything unresolved within it renders `[indeterminate]` |
| **OB-9** | Resolved in §7.4 |
| **OB-10** | The event sink must be **pre-created and empty**: a file on POSIX, a directory on Windows. Windows' four interior slots are created by the creating process before publication and are never adopted from an earlier generation |
| **OB-11** | Resolved in §8.4.2: POSIX uses flushed replacement plus directory flush; Windows uses two fixed body/commit slots and selects the greatest CRC/hash-valid commit. Every crash prefix has one prior or new committed body, never a guessed fragment |
| **OB-12** | A `link` snapshot restores "the most recent hyperlink seen", nothing more; a consumer needing every hyperlink must consume faster than the cap (§8.4.4). Synthetic snapshots consume sequence numbers exactly as transitions do, so the sequence stays dense. If the header, snapshots and triggering record together exceed the cap, the cap is exceeded for that one compaction rather than dropping the trigger — the invariant that an admitted triggering record survives outranks the byte bound. OB-28 separately permits only its bounded terminal transaction to exceed the cap; after it commits, no later record is admitted |
| **OB-13** | A `state` snapshot carries the **last published** title, not the last observed one. A snapshot is defined as restating published knowledge (§8.4.1); carrying an unpublished title would make compaction the only way to learn it, which is the opposite of a snapshot's purpose |
| **OB-14** | The gap is reported as a record in the consumer's own stream, carrying the sequence range that is unrecoverable. "Reports to its own consumer" is not a contract; a record is |
| **OB-15** | A title is bounded at **255 bytes**, a link target at **2048 bytes**, truncated at a UTF-8 character boundary so the JSON line stays well-formed. Truncation sets a flag on the record. Invalid UTF-8 and embedded NUL are replaced with the Unicode replacement character before bounding — never dropped silently, never emitted raw |
| **OB-16** | The launcher passes a **private inherited descriptor** whose other end it holds. `DESK_MOOR_LAUNCH_CHANNEL` selects it, and the exact 32-byte record plus EOF, 2-second deadline, generation check and stripping rules are frozen in §10.1.1 and wire §15.1. Its valid presence marks a supervised launch; a selector alone never does |
| **OB-17** | Canonical session identity is tagged: `01` plus the lexically resolved absolute socket-path bytes without symlink following on POSIX; `02` plus the Windows marker's little-endian volume serial and 128-bit file id, queried from the same-directory staged file and required to match after atomic publication. The tag is part of every comparison |
| **OB-18** | The **supervisor** owns the durable generation allocator, keyed by its non-recycled durable logical session key, not by the replaceable live rendezvous identity. Its first value is 2 because wire generation 1 is reserved for unsupervised holders; later values strictly increase. The logical key survives Moor `rm`, failed-launch cleanup, and ordinary same-name recreation. The holder is told its generation (§10.1) and never allocates. The store lives beside supervisor state, is written before launch, and is recovered by reading it; an unreadable store is refusal, not reset. Adoption later binds that generation to OB-17 identity and holder incarnation. Only an explicit supervisor operation that retires the whole lineage and its stored bindings may assign a fresh key after exhaustion |
| **OB-19** | A reserved sentinel: **zero in either dimension means preserve**. Zero is outside the valid range for a real size, so it cannot be confused with one. One dimension zero and the other not is a protocol error (§10.2.8) |
| **OB-20** | The opt-out is a single environment variable named for the program, suffixed `_NO_TERM_AUTORESPONSE`, following §4.4.1's derivation. **Any non-empty value** counts as set. It suppresses the synthetic replies only; the environment identity of §4.4.2 is unaffected, because a child that believes it is talking to a terminal that then does not answer is worse off than one told nothing |
| **OB-21** | Resolved in §11.3 and §11.4 |
| **OB-22** | The launch-time instrumentation module signals its own load over a **second, separate private inherited channel**, passed only with `-S`. The selector/nonce lifecycle, POSIX constructor, Windows `MoorInstrumentationInitV1` export, exact 36-byte record plus EOF, PID/generation/nonce checks and 2-second deadline are frozen in §4.7 and wire §15.2. The ACK proves only that requested initial process loaded and initialized the module, not descendant coverage or a security boundary |
| **OB-23** | The scanner is bounded at **64 KiB** of retained partial sequence. On exceeding it the partial sequence is abandoned, the scanner resumes at the next byte that can begin a sequence, and the abandonment is reported once — silently resuming is how a hostile stream becomes invisible |
| **OB-25** | The consumer commits its position **after** its own transaction commits, never before. A failed consumer redelivers; a repeatedly failing record enters an explicit dead-letter state and is reported. Committing first is a permanent, silent loss of the event |
| **OB-26** | "Restarting the holder" is **not** required and is not achievable — the holder owns the pseudo-terminal and no successor can reopen it by name. §5.5's conformance evidence means restarting the **supervisor** while the holder keeps running, and observing that adoption re-establishes correct state and that a superseded generation is refused |
| **OB-27** | Resolved in §11.6 |
| **OB-28** | Event axes are bounded — `seq` at 2⁵³−1 and `epoch` at 2³²−1 — and Windows' storage commit index is bounded at 2⁶⁴−1. None wraps; a committed final diagnostic permanently closes the stream while the session continues with stream-writable false. Allocation is preflighted over the whole record set and the limiting-axis precedence is `seq`, then `epoch`, then Windows `commit` (§8.4.1). *Seq exhaustion:* ordinary work always leaves one sequence position available; if the required transition/compaction set no longer fits, it is not admitted and the reserved position carries the final diagnostic, even when that position is below the numeric maximum. *Epoch exhaustion:* when another compaction would require epoch 2³², append the admitted triggering transition exactly once plus the diagnostic in epoch 2³²−1, without compaction. *Commit exhaustion:* commit the admitted triggering transition, any required snapshots, and the diagnostic together at index `FFFFFFFFFFFFFFFF`. If a higher-priority axis limits the same transaction, its single diagnostic wins and may use the final Windows commit. The final transaction alone may exceed the byte cap; recovery never resumes after it |
| **OB-29** | One encoding contract for opaque names: native path representation on native surfaces (POSIX bytes, Windows UTF-16), canonical WTF-8 when a Windows path crosses a byte/JSON protocol field, padded base64 for tagged identity in JSON (§8.4.1.1), padded base64 per native-path entry in `_SESSION_V2` (§4.4.1), and exact reversible ASCII on line-oriented human surfaces (`list`, diagnostics, `current`): `[A-Za-z0-9._/-]` unchanged, every other byte as uppercase `\xHH`. Width counts rendered bytes. No name can inject a delimiter or line, and no surface silently drops a byte |
| **OB-30** | Two carriers, both in the companion schema: a `WAKEUP` frame with no payload signals that the event stream advanced, and a `HEARTBEAT` frame every 5 seconds carries child-running and stream-writable flags. Absence past 15 seconds invalidates verified-live evidence and triggers a fresh bounded probe; the session is `indeterminate` until that probe resolves. Silence on `WAKEUP` means nothing |
| **OB-31** | **Both**, not either: wall-clock start, monotonic start and boot identity. Linux/WSL uses `CLOCK_BOOTTIME` plus the parsed kernel boot UUID; macOS uses `mach_continuous_time` plus the frozen `kern.boottime` encoding; Windows uses `GetTickCount64` plus documented WMI `LastBootUpTime` converted to FILETIME. An unavailable all-zero identity never compares equal, so age becomes unknown (§12.6) |
| **OB-32** | Resolved as the `-d <path>` option in §3.5, with its own failure diagnostic distinct from a child that could not be executed |
| **OB-33** | Five outcomes — terminated, already gone, refused on identity, indeterminate, failed — encoded in `TERMINATE_RESULT`; 5-second escalation, 10-second whole-operation deadline, exceeded is `INDETERMINATE` |
| **OB-34** | Resolved in §12.4: POSIX foreground/child groups with detached descendants excluded; Windows no-breakaway kill-on-close job plus in-console CTRL_BREAK and forced `TerminateJobObject`, covering only actual job members |
| **OB-35** | Child process identifier, 4-byte containment token, and 16-byte birth token. On POSIX the containment token is the pgrp; on Windows it is holder-minted and never presented as a Win32 job id |
| **OB-36** | `INPUT_RECEIPT` remains exactly transport-written/refused. Wire v3 optionally binds the input to an application request id and semantic source, and requires a prepared `INPUT_NOTICE` before the PTY write. That enables, but does not fabricate, OB-37 evidence |
| **OB-38** | Resolved in §4.5 and §8.2: exactly one lifecycle exit record for a child end the holder observes, plus exactly one event-stream `exit` only when that stream remains writable; holder loss is surfaced through the liveness path of OB-30, never as a fabricated child exit |
| **OB-39** | The status descriptor of §5 of the companion schema, carried by both `ATTACH_ACK` and `STATUS_REPLY`. Its output fields describe §6.7's bounded raw replay and tracked-mode exactness only; wire v3 has no screen checkpoint or main/alternate-buffer exactness claim |
| **OB-40** | Closed as a known parity defect retained in version 1. §4.4.2 stands as written: nothing is removed from the environment, so a reattached session carries the viewer identity of the terminal that created it. This is deliberately retained, not left undecided; a version 2 may revisit it after surveying real terminal-emulator instance variables. It is not on OB-24 because version 1 changes no behavior |
| **OB-41** | Resolved in §3.3: refusal decided from live process ancestry; the session variable stays descriptive |
| **OB-42** | Resolved in §12.3: handlers record and wake only; cleanup on the normal path; termination bounded while blocked on input or output; demonstrated against the shipped binary |

### 14.3 Still open — one, with its owner

| id | why it cannot be closed here | owner |
|---|---|---|
| **OB-37** | Wire v3 and §10.3 now provide the application-correlation carrier, provenance, durable event and loss/recovery behavior. The remaining gate is **real provider authority**: Moor cannot make a provider read or act, and a frame named `APPLICATION_RECEIPT` is not proof that Claude, Codex, OpenCode or another provider can truthfully emit it. Closure is therefore per provider/version only after shipped end-to-end runtime conformance at the named authoritative point | **The supervisor specification jointly with each provider integration.** Until the deployed provider passes that gate, screen observation, repeated-submit handling and `semantic-unknown` remain. No global closure follows from this document |

### 14.4 How this register is maintained

A decision changes here and nowhere else. A new deferral anywhere in this document adds a row to §14.3 in the same edit, with an owner that is a section able to supply the answer — never one that merely consumes it.
