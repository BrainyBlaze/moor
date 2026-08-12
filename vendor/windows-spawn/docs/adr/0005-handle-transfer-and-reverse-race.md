# 0005 — Handle transfer and the reverse inheritance race

Status: accepted (2026-08-02)

## Context

`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limits target-child inheritance, but every
listed handle must be inheritable during `CreateProcessW`. Mutating the source
handle would change caller-owned state. A temporary inheritable duplicate
avoids that mutation but can still leak to a concurrent broad-inheritance spawn
in the same source process.

An alternate parent has a different handle table. Standard streams and
`arg_handle`/`env_handle` values require duplication into that table before
their child-visible numeric values are known.

## Decision

Create inheritable local duplicates immediately before spawn and close them
when process creation returns. Document the process-wide reverse race. The 0.1
series does not use a helper process because it would change parent identity
and failure semantics.

Duplicate configured handle arguments and environment values privately, then
duplicate and lower them for the selected parent during each spawn. Reclaim
remote temporaries with `DuplicateHandle` close-source semantics on success and
failure. Accept handles only through standard I/O or the argument/environment
handoff protocol.

## Consequences

- Never make source handles inheritable in place.
- Do not retain arbitrary inheritable duplicates between spawns.
- Prevent target-child over-inheritance; callers must avoid concurrent broad
  inheritance when transferred handles are sensitive.
- Give local and remote duplicates one deterministic cleanup owner.
