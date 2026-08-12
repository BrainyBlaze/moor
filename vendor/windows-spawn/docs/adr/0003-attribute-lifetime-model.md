# 0003 — Attribute storage belongs to the spawn transaction

Status: accepted (2026-08-02)

## Context

`UpdateProcThreadAttribute` retains pointers until `CreateProcessW`. The list
requires aligned storage and each value requires a stable address. The
pseudoconsole is an exception: its `HPCON` value, rather than its address, is
passed as `lpValue`.

## Decision

Keep the attribute list private to one `SpawnTransaction`. Give its backing
allocation word alignment and each normal value stable owned storage. Retain
both until after `DeleteProcThreadAttributeList`. Pass the borrowed
pseudoconsole value according to its Win32 contract.

Derive the native attribute-list pointer from the backing allocation whenever
it is needed. Do not store a second, self-reference-like raw pointer. Snapshot
the `HPCON` value when `SpawnOptions::pseudoconsole` is called while retaining
the original capability lifetime with a private marker.

`SpawnOptions<'a>` carries the lifetime of borrowed Jobs, parent process, and
pseudoconsole capabilities; reusable `Command` does not borrow them.

## Consequences

- No public raw attribute API or self-referential builder.
- Attribute pointers cannot outlive their values.
- Delete the list before its values and backing storage.
- Moving `AttributeList` cannot stale a duplicated raw pointer field.
- Keep the unsafe lifetime proof inside `sys`.
