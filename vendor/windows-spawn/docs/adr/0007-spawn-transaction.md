# 0007 — Process creation is transactional

Status: accepted (2026-08-02)

## Context

A spawn can acquire pipes, null handles, local and remote duplicates, a private
Job, attribute storage, and process/thread handles. Any acquisition can fail;
distributed cleanup state permits leaks and double closes.

## Decision

`SpawnTransaction` exclusively owns temporary resources. Before commit, its
`Drop` implementation terminates a created process and rolls back all state.
Commit moves only the process handle, public pipe endpoints, lifecycle policy,
and retained Job ownership into `Child`; thread and temporary handles close.

Private `Running` and `Suspended` marker types parameterize both the plan and
transaction. Each state exposes only its corresponding commit, so a wrong
commit is unrepresentable instead of rejected at runtime. A `HandleTransfer`
owner keeps the effective parent, local or remote duplicates, and inheritance
list together.

`SuspendedChild` represents the suspended state. Its consuming `resume` is the
only normal transition to `Child`; dropping it first terminates the process or
its private Job. Resume succeeds only when `ResumeThread` reports the expected
previous suspend count of exactly one. External suspension or resumption makes
the transition fail and the process roll back.

## Consequences

- No shared ownership of raw handles.
- No public partial-initialization state.
- Running and suspended commits are selected by the type system.
- Cleanup follows ownership instead of error-site flags.
- Failure-injection and handle-count tests can verify rollback without exposing
  transaction internals.
