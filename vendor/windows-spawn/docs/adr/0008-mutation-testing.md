# 0008 — Mutation testing is a scheduled audited gate

Status: accepted (2026-08-02)

## Context

Process-tree, suspended-process, and pipe-EOF tests make a complete mutation
run too expensive for each pull request. Broad exclusions would hide ownership
and cleanup faults.

## Decision

Run four shards weekly and on manual dispatch. Address each survivor with a
test or exclude its exact expression only when it is equivalent for every
value allowed at that site. `.cargo/mutants.toml` may not exclude a whole file
or mutation class.

Suspended-process tests retain an independent process handle and use direct
Win32 cleanup before reporting a failed termination assertion. Cleanup bypasses
the mutated crate path so changes to `Drop`, `Child::kill`, or
`TerminateProcess` cannot leave a suspended process on the runner.

The xtask starts `cargo mutants` through `windows-spawn` with
`DropPolicy::KillTree`. The private kill-on-close Job contains descendants on
normal exit, timeout, test failure, or runner termination. Local runs use a
unique ignored output directory; CI writes the upload under `mutants.out`.

The four exclusions are equivalent because:

- `MitigationPolicy::replace` clears the destination bits before OR or XOR.
- `DUPLICATE_SAME_ACCESS` and `DUPLICATE_CLOSE_SOURCE` occupy disjoint bits.
- `PROCESS_CREATE_PROCESS` and `PROCESS_DUP_HANDLE` occupy disjoint bits.
- Public `CreationFlags` cannot contain the private
  `CREATE_UNICODE_ENVIRONMENT` bit added by `create_process`.

## Consequences

Mutation results remain actionable, the expensive suite stays off the pull
request path, and each exclusion has a reviewable equivalence proof.
