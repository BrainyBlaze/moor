# 0001 — Why a new crate rather than `firehazard`

Status: accepted (2026-08-01)

## Context

`firehazard` provides a safe RAII `ThreadAttributeList` builder for the
attributes targeted here. Its published version remains `0.0.0`, its broader
sandboxing scope includes tokens, ACLs, AppContainers, and debugging, and Job
libraries do not provide equivalent process-attribute integration.

## Decision

Build a crate limited to process creation, with stable releases and release
gates.

## Consequences

- This duplicates part of `firehazard`. Reconsider the crate if `firehazard`
  publishes a stable process-creation API with regular releases.
- Reject token and ACL features; direct users to `rappct` or `firehazard`.
- Interoperate with existing Job libraries by adopting foreign Job handles
  (ADR 0004).
