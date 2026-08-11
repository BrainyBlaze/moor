# `atch` compatibility-name retirement design

**Date:** 2026-08-11  
**Status:** approved by the operator through Moor issue #25  
**Scope:** Moor repository only

## Goal

Ship, package, document, and test exactly one command name: `moor`. Remove the
named `atch` compatibility product now that Desk has completed its native Moor
cutover.

The generic invoked-basename contract remains intact. A user-created copy or
symlink under an arbitrary name still derives its displayed program name,
session root, and environment keys from that name. The implementation must not
add an `atch` blacklist or canonicalize arbitrary invocations to `moor`.

## Chosen approach

Use targeted retirement:

- remove the `atch` Cargo binary target;
- build, package, hash, archive, and smoke only `moor` in every workflow;
- replace the `atch`-named CLI regression with an arbitrary renamed-copy
  regression;
- amend the current behavioural specification and implementation plan so they
  require only the `moor` distribution name while retaining generic renamed-copy
  behavior;
- add a quality gate based on `cargo metadata` that requires `moor` to be the
  package's sole binary target;
- update every integrity anchor for the amended behavioural specification.

This approach removes the requested surface without changing production Rust
logic or adding production LOC.

## Alternatives rejected

1. Canonicalize every invocation to `moor`. This would collapse basename-derived
   isolation and break the still-required renamed-copy behavior.
2. Remove only the packaged artifact. This would leave the Cargo target,
   specification, tests, and workflows contradictory and would allow accidental
   reintroduction.

## Contract amendments

`spec/moor-spec.md` will say that the distribution installs only `moor`. Its
examples will use a generic renamed copy instead of presenting `atch` as a
supported compatibility entrypoint.

This is a later behavioural amendment, not a wire-layout change. Therefore:

- recompute the SHA-256 of `spec/moor-spec.md`;
- update the matching table entry in `spec/README.md`;
- update the literal hash check in `.github/workflows/quality.yml`;
- leave `spec/moor-wire-schema.md` and its schema version/digest unchanged.

Historical records may retain `atch` when they describe past state. Current
requirements and active release automation may not.

## Build and release flow

Cargo exposes one binary target named `moor`. Hosted and self-hosted workflows
build that target, copy one executable into each evidence directory, calculate
one artifact hash, and smoke `moor --version` plus `moor --help`. Artifact names,
identity files, and retention behavior otherwise remain unchanged.

The quality workflow first asserts that Cargo metadata contains exactly one bin
target named `moor`, then performs the release build. This makes a restored
`atch` target fail before packaging.

## Tests and verification

Use a red/green sequence:

1. Run the sole-bin metadata assertion against the current two-target manifest;
   it must fail.
2. Remove the target and update current surfaces.
3. Re-run the assertion; it must pass.

The CLI regression will create an arbitrary symlink such as `moor-copy` and
verify that its version output uses that invoked basename. This preserves the
generic contract without keeping `atch` as a named compatibility surface.

Final verification includes:

- exact `atch` inventory, allowing only explicitly historical references;
- Cargo metadata target assertion;
- specification hash verification;
- `cargo fmt --all -- --check`;
- strict Clippy across all targets and features;
- all-target/all-feature tests;
- release build of `moor`;
- production LOC count, which must not increase;
- hosted quality and native evidence for the exact pushed SHA.

## Boundaries

- No Desk repository changes.
- No production Rust changes are expected.
- No wire or event schema changes.
- No change to arbitrary renamed-copy behavior.
- Issue #4's former requirement to exercise both packaged names is superseded by
  issue #25; native evidence must exercise the sole packaged `moor` command.
