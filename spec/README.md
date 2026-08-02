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

## Clean-room boundary

Implement from the files in this directory only. Do not inspect an existing
holder implementation, its tests, workaround inventories, research notes, or
decision briefs. Their relevant conclusions have already been converted into
behavioural requirements in the specification.

When a requirement is ambiguous or incomplete, report the gap to the
specification team in behavioural terms. Do not infer the answer from an
existing product or invent an implementation-specific contract.

## Integrity

The reviewed handoff artifacts have these SHA-256 digests:

| file | SHA-256 |
|---|---|
| `moor-spec.md` | `e102b1e49d59bfd0360a4b1d93a252446e8bdfcad302bd0536d895058e0626f0` |
| `moor-wire-schema.md` | `ee7911a07f1b7213a7a04f5f9869a5c6c917419a1f8171c418d8ceb7ab0f8df2` |

Verify them from this directory with:

```sh
sha256sum moor-spec.md moor-wire-schema.md
```
