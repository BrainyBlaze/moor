# Windows finalization plan

The exact `d5c39d4` native run rejected the merge candidate. This plan closes the
invalid test boundary and the four production defects found by holistic review before
Moor main moves.

## 1. Separate the two console-input contracts

- Keep the legacy child for ordered ASCII `A -> resize -> B` behavior.
- Use a VT-native CP65001 child for exact `A + emoji + é + NUL + Z` bytes.
- Require whole-vector equality and a post-sender 500 ms quiet interval.
- Keep the low-level `INPUT_RECORD -> UTF-8` unit vector as the translation proof.

## 2. Enforce the semantic-token environment fence

- Start each real child from a parent poisoned with
  `MOOR_SESSION_SEMANTIC_TOKEN=poison`.
- Without `-T`, require the variable to be absent.
- With `-T`, require a fresh lowercase 32-hex value.
- Strip the variable from both bootstrap and requested-child builders; inject only the
  token minted for the current launch.

## 3. Finalize successful prepublication exits

- Race an exclusive competing marker against `cmd /c exit 23` for `start` and `run`.
- Observe a natural exit for the same bounded allowance used on Unix.
- Delete only Moor's unpublished marker/instrument stages.
- Commit lifecycle/event/log final state without publishing a Moor rendezvous.
- Require `start` status 1 plus the frozen diagnostic and `run` status 23; require the
  retained session to appear as `[exited]` after the competing marker is removed.

## 4. Preserve cleanup on terminal console-control events

- Classify C/BREAK as repeatable notifications and CLOSE/LOGOFF/SHUTDOWN as terminal
  notifications.
- The handler records/wakes only. For a terminal notification it must not return to
  Windows before the main holder exits, because returning `TRUE` authorizes immediate
  process termination.
- Close a real outer pseudoconsole and require durable graceful lifecycle retirement.

## 5. Make the uncertain deadline nonblocking

- Add a native-abandon hook that `Runtime::drive` calls before returning the 10-second
  indeterminate outcome.
- On Windows close the kill-on-close job first, then retire HPCON on a detached,
  non-joining thread.
- Make all residual `Pseudo` drops non-joining so error teardown cannot reintroduce the
  same hang.
- Unit-test both the runtime abandon call and the non-joining close carrier.

## 6. Gate and release

- Run formatting, focused tests, full host tests, strict Clippy, Windows GNU checks and
  reviewed handoff hashes.
- Push one exact candidate and require Quality plus all eight hosted native jobs green.
- Build static-CRT Windows candidate and test-harness artifacts once; reject PE imports
  of `VCRUNTIME*.dll` or `MSVCP*.dll`.
- Rehash the exact x64 candidate on clean Dockur Server 2019 and execute the archived
  harness there. Do not tag until the complete manual QA matrix passes.
