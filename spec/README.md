# Moor implementation handoff

This directory is the complete clean-room input for the Moor implementation
team.

## Authoritative artifacts

1. [moor-spec.md](./moor-spec.md) is the normative behavioural specification.
2. [moor-wire-schema.md](./moor-wire-schema.md) freezes the wire layouts and
   conformance vectors required by the specification.

If the two artifacts disagree, the behavioural specification wins and the wire
schema must be reported as defective. This README defines the handoff procedure
only; it adds no behavioural requirements.

`wire-schema-4` and `event-schema-2` are the current revisions; `wire-schema-3` received one in-place amendment before any
conforming implementation existed. No mixed old/amended dialect is supported.
A later platform amendment narrowed the supported families to Linux and macOS: identity tag `01` is the only rendezvous
identity, and `path_encoding` is `posix-bytes` only.
Every later change to a frozen layout requires the corresponding version
increment.

A later behavioural amendment retired the packaged compatibility command name.
It changed no frozen wire or event layout, so only the behavioural-spec digest
changed.

## Clean-room boundary

Implement from this README and the two authoritative artifacts above only. Do
not inspect an existing holder implementation, its tests, workaround
inventories, research notes, or decision briefs. Their relevant conclusions
have already been converted into behavioural requirements in the
specification.

When a requirement is ambiguous or incomplete, report the gap to the
specification team in behavioural terms. Do not infer the answer from an
existing product or invent an implementation-specific contract.

## Integrity

The reviewed handoff artifacts have these SHA-256 digests:

| file | SHA-256 |
|---|---|
| `moor-spec.md` | `f76a8ead39292d7f67446813ded5db27e8e6f999289118e2a96425fb6b678e06` |
| `moor-wire-schema.md` | `d00dc527d41975c29640de6b8ca0fabab47d14a951653e376e9246742e4fd52a` |

Verify them from this directory with:

```sh
sha256sum moor-spec.md moor-wire-schema.md
```
