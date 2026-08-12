# 0002 — windows-spawn owns its `CreateProcessW` call

Status: accepted (2026-08-02)

## Context

Stable `std::process::Command` cannot supply a `STARTUPINFOEXW` attribute list;
its raw extension is nightly-only and unsafe. A public attribute-list wrapper
would split command-line lowering, standard I/O, inheritance, process handles,
and rollback across APIs.

## Decision

`Command` stores reusable intent, `SpawnPlan` validates and normalizes it, and
`SpawnTransaction` owns temporary OS resources. Only the private `sys` module
calls Win32.

Return `std::process::ExitStatus`, `std::process::Output`, and `std::io::Error`.
Use a distinct `Child` because stable std cannot adopt this transaction's
process and pipe handles.

## Consequences

- Test quoting, environment ordering, executable lookup, standard I/O, output
  draining, and exit codes in this crate.
- One transaction owns rollback for every process-creation error.
- Keep raw flags, attribute lists, and `windows-sys` types private.
- Revisit this decision if std gains a safe, complete attribute interface.
