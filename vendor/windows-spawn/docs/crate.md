# Windows process creation with explicit ownership

`windows-spawn` owns a complete `CreateProcessW` transaction for explicit
handle transfer, ordered Job attachment, typed mitigation policies, `ConPTY`,
and suspended inspection. Use [`std::process::Command`] for portable child
processes.

Non-Windows targets expose no public API. They support dependency-graph checks,
not process creation.

# Platform contract

The core process-creation transaction and `ConPTY` integration require Windows 10
version 1809 or later. Individual [`MitigationPolicy`] fields can require newer
Windows versions, a particular processor architecture, hardware support, or
compatible executable metadata. CET policies are a notable example, and
pointer authentication is ARM64-specific.

The crate does not preflight or silently weaken requested mitigations. When
Windows cannot apply a requested combination, spawning returns the operating
system error from `CreateProcessW`. Nested Job behavior likewise remains
subject to Jobs already imposed by the host.

# Commands and executable lookup

- `.bat` and `.cmd` programs are rejected. Invoke `cmd.exe` explicitly when a
  shell boundary is intended.
- Executable lookup follows Rust's safe Windows search behavior, does not
  search the current directory, and passes the resolved path as
  `lpApplicationName`.
- [`Command::raw_arg`] appends already-encoded Windows command-line syntax. It
  does not invoke a shell and must only receive syntax appropriate for the
  target executable's parser.
- Raw attribute injection, creation flags, and mitigation constructors are not
  exposed.

# Handle and capability ownership

[`Command`] stores reusable execution intent. [`Command::arg_handle`] and
[`Command::env_handle`] take a private, non-inheritable duplicate. The source
handle may then be closed; each spawn transfers a new duplicate.

Immediately before `CreateProcessW`, the crate creates only the inheritable
duplicates required for standard I/O and argument or environment handoff. It
lists exactly those values in `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` and closes
the temporary duplicates as soon as process creation returns. The source
handle is never made inheritable in place, and there is no public escape hatch
for retaining an arbitrary inheritable duplicate.

Handle-handoff values form an application protocol:

- `arg_handle` appends a decimal value to the child's command line;
  `env_handle` writes one into an environment variable.
- The receiving program must parse the value and adopt or borrow it according
  to that protocol.
- A child-visible numeric value belongs to the child's handle table and must
  not be assumed to match the source process's value.
- With an alternate parent, the resource is duplicated into the effective
  parent's handle table before the child-visible value is lowered.

Windows retains a process-wide reverse race: unrelated broad-inheritance spawns
can receive a short-lived inheritable duplicate. Avoid concurrent broad
inheritance when transferred handles are sensitive. Version 0.1 does not use a
helper process because that would change parent identity and failure semantics.
See
[ADR 0005](https://github.com/P4suta/windows-spawn/blob/main/docs/adr/0005-handle-transfer-and-reverse-race.md).

[`SpawnOptions`] borrows one-spawn capabilities such as Jobs, an alternate
parent, or a pseudoconsole. A borrowed `ConPTY` remains owned by the terminal
library implementing [`AsPseudoConsole`]. That library defines when terminal
pipes close and when terminal EOF occurs. Pseudoconsole process creation sets
`STARTF_USESTDHANDLES` with all three standard-handle slots null and does not
put standard handles in the inheritance list. This prevents a hosted child
from falling back to redirected standard handles owned by the parent.

# Drop, wait, and EOF contract

- Dropping a normal [`Child`] detaches by default, matching
  [`std::process::Child`].
- [`DropPolicy::KillTree`] owns a private innermost Job. Dropping the child
  terminates the root and its descendants.
- [`Child::wait_with_output`] drains stdout and stderr concurrently. With
  `KillTree`, it terminates remaining descendants after the root exits before
  joining the readers. This guarantees pipe EOF even when a grandchild
  inherited a writer. Both reader threads are joined even when one reader
  fails or panics.
- Dropping [`SuspendedChild`] before [`SuspendedChild::resume`] terminates the
  suspended process. Its ID, process handle, and primary-thread handle are
  available before resume. `resume(self)` is consuming, so a second transition
  is unrepresentable. The transition requires the primary thread's previous
  suspend count to be exactly one; external changes are rejected and rolled
  back.

# Transaction and security boundary

Process creation uses a validation plan and an owning transaction. The
transaction owns pipes, temporary duplicates, attributes, Jobs, and
process/thread handles. Success transfers durable resources to [`Child`] or
[`SuspendedChild`]; errors roll back the rest.

The private validation plan and transaction carry running or suspended marker
types. Their state-specific commits make a mismatched internal transition
unrepresentable.

This crate is not a sandbox, cross-platform process facade, async runtime, or
process supervisor. Tokens, ACLs, `AppContainer`, LPAC, capability SIDs, and
async supervision are outside its scope. Callers building an isolation boundary
must supply and audit those controls separately.

# Further reading

- [Compile-checked examples](https://github.com/P4suta/windows-spawn/tree/main/examples)
- [Architecture decision records](https://github.com/P4suta/windows-spawn/tree/main/docs/adr)
- [Security policy](https://github.com/P4suta/windows-spawn/security/policy)
