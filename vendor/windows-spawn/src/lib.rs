#![doc = include_str!("../docs/crate.md")]
#![deny(unsafe_code)]

// The rendered crate documentation comes from `docs/crate.md`, so the README
// would otherwise ship to crates.io without ever being compiled. Including it
// under `cfg(doctest)` type-checks and runs its examples without adding a
// second copy of the front page to the rendered docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_examples {}

#[cfg(windows)]
mod child;
#[cfg(windows)]
mod command;
#[cfg(windows)]
mod handles;
#[cfg(windows)]
mod mitigation;
#[cfg(windows)]
mod options;
#[cfg(windows)]
mod plan;
#[cfg(windows)]
#[allow(unsafe_code)]
mod sys;
#[cfg(windows)]
mod transaction;

#[cfg(windows)]
pub use crate::child::{Child, ChildStderr, ChildStdin, ChildStdout, SuspendedChild};
#[cfg(windows)]
pub use crate::command::Command;
#[cfg(windows)]
pub use crate::handles::{AsPseudoConsole, Job, ParentProcess, Stdio};
#[cfg(windows)]
pub use crate::mitigation::{
    BlockNonCetBinaries, CetShadowStacks, ControlFlowGuard, DynamicCode, FontDisable,
    LoaderIntegrity, Mitigation, MitigationPolicy, ModuleTampering, RelocateImages, SignedBinaries,
    UserCetContextIpValidation,
};
#[cfg(windows)]
pub use crate::options::{CreationFlags, DropPolicy, SpawnOptions};
