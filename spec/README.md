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

`wire-schema-3` and `event-schema-2` received one in-place amendment before any
conforming implementation existed. No mixed old/amended dialect is supported.
Every later change to a frozen layout requires the corresponding version
increment.

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
| `moor-spec.md` | `05f644d676beaa73d3bcd7a3aef6fb5c715ed53babf853333bb364bf37063433` |
| `moor-wire-schema.md` | `8cc57dca672929ffab5d869e4c883d0e28d89070df9aa2fd619558a5f766869b` |

Verify them from this directory with:

```sh
sha256sum moor-spec.md moor-wire-schema.md
```
