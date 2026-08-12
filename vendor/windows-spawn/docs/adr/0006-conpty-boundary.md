# 0006 — ConPTY integration is a borrowed capability

Status: accepted (2026-08-02)

## Context

The crate must attach an existing pseudoconsole without depending on a terminal
library or owning its `HPCON`. A safe raw constructor could accept invalid or
prematurely closed values.

## Decision

Use the unsafe `AsPseudoConsole` trait. Implementors guarantee a stable,
nonzero, live `HPCON` for the full borrow and retain ownership. The raw method
is public so terminal libraries can implement the bridge.

A terminal library may depend on windows-spawn and implement the trait. That
downstream owns ConPTY creation, pipes, waits, runtime integration, and
lifecycle. windows-spawn owns only command lowering, attributes, Jobs, and
`CreateProcessW`; it has no dependency on, checkout of, or CI pin to a
particular terminal library.

The builder snapshots the `HPCON` numeric value and keeps only a lifetime
marker, so the stored options do not require dynamic dispatch. During process
creation ConPTY uses an explicit startup-I/O mode: `STARTF_USESTDHANDLES` is
set while `hStdInput`, `hStdOutput`, and `hStdError` remain null. No standard
handle is added to `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, and a spawn with no
other handle transfer passes `bInheritHandles = FALSE`.

This deliberately follows the production `ConptyConnection` implementation in
Microsoft Terminal. The shorter Microsoft Learn walkthrough zero-initializes
`STARTUPINFOEXW` and leaves `STARTF_USESTDHANDLES` clear. That sample explains
the pseudoconsole attribute but does not isolate a hosted process from the
calling process's normal standard-handle slots. In a parent with redirected
standard I/O, following it literally can let the child use the parent's pipes
instead of ConPTY. The explicit null slots make that ownership boundary
testable and deterministic.

## Consequences

- Ordinary users pass a safe borrow and do not construct raw `HPCON` values.
- Dependency and validation flow only from a terminal library to
  windows-spawn; windows-spawn does not name a downstream integration target.
- Pseudoconsole use conflicts with explicit standard streams and leaves the
  ordinary startup handles explicitly null.

## References

- [Microsoft Terminal `ConptyConnection.cpp`](https://github.com/microsoft/terminal/blob/fbda436dc654cf551dd196b2667ef95d3e0a7262/src/cascadia/TerminalConnection/ConptyConnection.cpp)
- [Creating a Pseudoconsole session](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)
