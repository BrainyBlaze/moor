//! Typed process-creation mitigation policy encoding.

/// The ordinary two-bit mitigation states used by the Windows SDK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum Mitigation {
    /// Let the child executable and operating system choose.
    #[default]
    Defer = 0,
    /// Force the mitigation on.
    AlwaysOn = 1,
    /// Force the mitigation off.
    AlwaysOff = 2,
}

/// Mandatory-ASLR modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum RelocateImages {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Relocate images even when they are not dynamic-base compatible.
    AlwaysOn = 1,
    /// Do not force relocation.
    AlwaysOff = 2,
    /// Relocate images and reject images without relocation data.
    RequireRelocations = 3,
}

/// Dynamic-code policy modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum DynamicCode {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Prohibit dynamic code.
    Prohibit = 1,
    /// Do not prohibit dynamic code.
    Allow = 2,
    /// Prohibit dynamic code while allowing the child to opt out.
    ProhibitWithOptOut = 3,
}

/// Control Flow Guard modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum ControlFlowGuard {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Enable Control Flow Guard.
    AlwaysOn = 1,
    /// Disable Control Flow Guard.
    AlwaysOff = 2,
    /// Enable Control Flow Guard with export suppression.
    ExportSuppression = 3,
}

/// Microsoft-signed binary policy modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum SignedBinaries {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Permit only Microsoft-signed binaries.
    MicrosoftOnly = 1,
    /// Do not restrict binary signatures.
    AlwaysOff = 2,
    /// Permit Microsoft Store binaries in addition to Microsoft binaries.
    MicrosoftAndStore = 3,
}

/// Non-system-font policy modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum FontDisable {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Block non-system fonts.
    Block = 1,
    /// Allow non-system fonts.
    Allow = 2,
    /// Audit non-system font loads.
    Audit = 3,
}

/// Loader integrity continuity modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum LoaderIntegrity {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Enforce loader integrity continuity.
    AlwaysOn = 1,
    /// Disable enforcement.
    AlwaysOff = 2,
    /// Audit violations.
    Audit = 3,
}

/// Module-tampering protection modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum ModuleTampering {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Enable module-tampering protection.
    AlwaysOn = 1,
    /// Disable protection.
    AlwaysOff = 2,
    /// Enable protection without inheriting it into descendants.
    NoInherit = 3,
}

/// CET user shadow-stack modes.
///
/// Runtime support depends on the Windows release, processor architecture,
/// hardware capabilities, and child executable. Representability here does
/// not imply that the current host accepts the policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum CetShadowStacks {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Enable user shadow stacks.
    AlwaysOn = 1,
    /// Disable user shadow stacks.
    AlwaysOff = 2,
    /// Enable strict shadow-stack mode.
    Strict = 3,
}

/// CET set-context instruction-pointer validation modes.
///
/// Runtime support depends on the Windows release, processor architecture,
/// hardware capabilities, and child executable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum UserCetContextIpValidation {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Enable validation.
    AlwaysOn = 1,
    /// Disable validation.
    AlwaysOff = 2,
    /// Enable relaxed validation.
    Relaxed = 3,
}

/// Modes for blocking binaries without CET or EH continuation metadata.
///
/// Runtime support depends on the Windows release, processor architecture,
/// and executable metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum BlockNonCetBinaries {
    /// Defer to the child.
    #[default]
    Defer = 0,
    /// Block binaries without CET metadata.
    AlwaysOn = 1,
    /// Disable blocking.
    AlwaysOff = 2,
    /// Block binaries without EH continuation metadata.
    NonEhContinuation = 3,
}

/// A complete SDK 10.0.22621 process-creation mitigation policy.
///
/// Setters replace one field. Reserved values and combined raw policy words
/// cannot be represented.
///
/// # Runtime support
///
/// This type mirrors the policy fields in Windows SDK 10.0.22621; it is not a
/// claim that every field works on every supported Windows installation.
/// Availability varies by individual policy, Windows release, processor
/// architecture, hardware, and child executable. windows-spawn does not weaken
/// a requested policy. Unsupported policies return the process-creation error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MitigationPolicy {
    words: [u64; 2],
}

impl MitigationPolicy {
    /// Creates a policy which defers every field.
    #[must_use]
    pub const fn new() -> Self {
        Self { words: [0, 0] }
    }

    /// Returns the encoded words for diagnostics.
    #[must_use]
    pub const fn words(self) -> [u64; 2] {
        self.words
    }

    /// Enables or removes the legacy DEP-enable bit.
    #[must_use]
    pub const fn dep(mut self, enable: bool) -> Self {
        self.words[0] = set_bit(self.words[0], 0, enable);
        self
    }

    /// Enables or removes the legacy DEP ATL-thunk-emulation bit.
    #[must_use]
    pub const fn dep_atl_thunk(mut self, enable: bool) -> Self {
        self.words[0] = set_bit(self.words[0], 1, enable);
        self
    }

    /// Enables or removes the legacy SEHOP bit.
    #[must_use]
    pub const fn sehop(mut self, enable: bool) -> Self {
        self.words[0] = set_bit(self.words[0], 2, enable);
        self
    }

    /// Sets mandatory image relocation.
    #[must_use]
    pub const fn relocate_images(mut self, value: RelocateImages) -> Self {
        self.words[0] = replace(self.words[0], 8, value as u64);
        self
    }

    /// Sets heap termination on corruption.
    #[must_use]
    pub const fn heap_terminate(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 12, value as u64);
        self
    }

    /// Sets bottom-up ASLR.
    #[must_use]
    pub const fn bottom_up_aslr(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 16, value as u64);
        self
    }

    /// Sets high-entropy ASLR.
    #[must_use]
    pub const fn high_entropy_aslr(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 20, value as u64);
        self
    }

    /// Sets strict invalid-handle checking.
    #[must_use]
    pub const fn strict_handle_checks(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 24, value as u64);
        self
    }

    /// Sets the Win32k system-call-disable mitigation.
    #[must_use]
    pub const fn disable_win32k_system_calls(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 28, value as u64);
        self
    }

    /// Sets extension-point disabling.
    #[must_use]
    pub const fn disable_extension_points(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 32, value as u64);
        self
    }

    /// Sets dynamic-code policy.
    #[must_use]
    pub const fn dynamic_code(mut self, value: DynamicCode) -> Self {
        self.words[0] = replace(self.words[0], 36, value as u64);
        self
    }

    /// Sets Control Flow Guard policy.
    #[must_use]
    pub const fn control_flow_guard(mut self, value: ControlFlowGuard) -> Self {
        self.words[0] = replace(self.words[0], 40, value as u64);
        self
    }

    /// Sets signed-binary loading policy.
    #[must_use]
    pub const fn signed_binaries(mut self, value: SignedBinaries) -> Self {
        self.words[0] = replace(self.words[0], 44, value as u64);
        self
    }

    /// Sets non-system-font policy.
    #[must_use]
    pub const fn font_disable(mut self, value: FontDisable) -> Self {
        self.words[0] = replace(self.words[0], 48, value as u64);
        self
    }

    /// Sets remote-image blocking.
    #[must_use]
    pub const fn block_remote_images(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 52, value as u64);
        self
    }

    /// Sets low-integrity-label image blocking.
    #[must_use]
    pub const fn block_low_label_images(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 56, value as u64);
        self
    }

    /// Sets System32 image preference.
    #[must_use]
    pub const fn prefer_system32_images(mut self, value: Mitigation) -> Self {
        self.words[0] = replace(self.words[0], 60, value as u64);
        self
    }

    /// Sets loader integrity continuity.
    #[must_use]
    pub const fn loader_integrity(mut self, value: LoaderIntegrity) -> Self {
        self.words[1] = replace(self.words[1], 4, value as u64);
        self
    }

    /// Sets strict Control Flow Guard.
    #[must_use]
    pub const fn strict_control_flow_guard(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 8, value as u64);
        self
    }

    /// Sets module-tampering protection.
    #[must_use]
    pub const fn module_tampering(mut self, value: ModuleTampering) -> Self {
        self.words[1] = replace(self.words[1], 12, value as u64);
        self
    }

    /// Sets restricted indirect branch prediction.
    #[must_use]
    pub const fn restrict_indirect_branch_prediction(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 16, value as u64);
        self
    }

    /// Sets permission for a broker to downgrade dynamic-code policy.
    #[must_use]
    pub const fn allow_downgrade_dynamic_code(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 20, value as u64);
        self
    }

    /// Sets speculative-store-bypass disabling.
    #[must_use]
    pub const fn disable_speculative_store_bypass(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 24, value as u64);
        self
    }

    /// Sets CET user shadow stacks.
    #[must_use]
    pub const fn cet_user_shadow_stacks(mut self, value: CetShadowStacks) -> Self {
        self.words[1] = replace(self.words[1], 28, value as u64);
        self
    }

    /// Sets CET set-context instruction-pointer validation.
    #[must_use]
    pub const fn user_cet_context_ip_validation(
        mut self,
        value: UserCetContextIpValidation,
    ) -> Self {
        self.words[1] = replace(self.words[1], 32, value as u64);
        self
    }

    /// Sets blocking of binaries without CET metadata.
    #[must_use]
    pub const fn block_non_cet_binaries(mut self, value: BlockNonCetBinaries) -> Self {
        self.words[1] = replace(self.words[1], 36, value as u64);
        self
    }

    /// Sets extended Control Flow Guard.
    #[must_use]
    pub const fn extended_control_flow_guard(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 40, value as u64);
        self
    }

    /// Sets ARM64 user-mode instruction-pointer authentication.
    #[must_use]
    pub const fn pointer_authentication(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 44, value as u64);
        self
    }

    /// Sets CET dynamic APIs to out-of-process-only mode.
    #[must_use]
    pub const fn cet_dynamic_apis_out_of_process(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 48, value as u64);
        self
    }

    /// Sets restricted CPU-core sharing.
    #[must_use]
    pub const fn restrict_core_sharing(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 52, value as u64);
        self
    }

    /// Sets FSCTL system-call disabling.
    #[must_use]
    pub const fn disable_fsctl_system_calls(mut self, value: Mitigation) -> Self {
        self.words[1] = replace(self.words[1], 56, value as u64);
        self
    }
}

const fn replace(word: u64, shift: u32, value: u64) -> u64 {
    (word & !(3_u64 << shift)) | (value << shift)
}

const fn set_bit(word: u64, shift: u32, value: bool) -> u64 {
    if value {
        word | (1_u64 << shift)
    } else {
        word & !(1_u64 << shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setters_replace_only_their_field() {
        let first = MitigationPolicy::new()
            .dynamic_code(DynamicCode::ProhibitWithOptOut)
            .font_disable(FontDisable::Audit)
            .dynamic_code(DynamicCode::Allow);
        assert_eq!(first.words(), [(2_u64 << 36) | (3_u64 << 48), 0]);

        let second = MitigationPolicy::new()
            .cet_user_shadow_stacks(CetShadowStacks::Strict)
            .block_non_cet_binaries(BlockNonCetBinaries::NonEhContinuation);
        assert_eq!(second.words(), [0, (3_u64 << 28) | (3_u64 << 36)]);
    }

    #[test]
    fn legacy_bits_do_not_touch_two_bit_fields() {
        let policy = MitigationPolicy::new()
            .relocate_images(RelocateImages::RequireRelocations)
            .dep(true)
            .dep_atl_thunk(true)
            .sehop(true);
        assert_eq!(policy.words()[0], 7 | (3_u64 << 8));

        let cleared = policy.dep(false).dep_atl_thunk(false).sehop(false);
        assert_eq!(cleared.words()[0], 3_u64 << 8);
        assert_eq!(MitigationPolicy::new().dep(true).dep(true).words()[0], 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_sdk_22621_field_has_the_expected_encoding() {
        macro_rules! field {
            ($policy:expr, $word:expr, $shift:expr, $value:expr) => {{
                let mut expected = [0_u64; 2];
                expected[$word] = ($value as u64) << $shift;
                assert_eq!($policy.words(), expected);
            }};
        }

        field!(MitigationPolicy::new().dep(true), 0, 0, 1);
        field!(MitigationPolicy::new().dep_atl_thunk(true), 0, 1, 1);
        field!(MitigationPolicy::new().sehop(true), 0, 2, 1);
        field!(
            MitigationPolicy::new().relocate_images(RelocateImages::RequireRelocations),
            0,
            8,
            3
        );
        field!(
            MitigationPolicy::new().heap_terminate(Mitigation::AlwaysOn),
            0,
            12,
            1
        );
        field!(
            MitigationPolicy::new().bottom_up_aslr(Mitigation::AlwaysOn),
            0,
            16,
            1
        );
        field!(
            MitigationPolicy::new().high_entropy_aslr(Mitigation::AlwaysOn),
            0,
            20,
            1
        );
        field!(
            MitigationPolicy::new().strict_handle_checks(Mitigation::AlwaysOn),
            0,
            24,
            1
        );
        field!(
            MitigationPolicy::new().disable_win32k_system_calls(Mitigation::AlwaysOn),
            0,
            28,
            1
        );
        field!(
            MitigationPolicy::new().disable_extension_points(Mitigation::AlwaysOn),
            0,
            32,
            1
        );
        field!(
            MitigationPolicy::new().dynamic_code(DynamicCode::ProhibitWithOptOut),
            0,
            36,
            3
        );
        field!(
            MitigationPolicy::new().control_flow_guard(ControlFlowGuard::ExportSuppression),
            0,
            40,
            3
        );
        field!(
            MitigationPolicy::new().signed_binaries(SignedBinaries::MicrosoftAndStore),
            0,
            44,
            3
        );
        field!(
            MitigationPolicy::new().font_disable(FontDisable::Audit),
            0,
            48,
            3
        );
        field!(
            MitigationPolicy::new().block_remote_images(Mitigation::AlwaysOn),
            0,
            52,
            1
        );
        field!(
            MitigationPolicy::new().block_low_label_images(Mitigation::AlwaysOn),
            0,
            56,
            1
        );
        field!(
            MitigationPolicy::new().prefer_system32_images(Mitigation::AlwaysOn),
            0,
            60,
            1
        );
        field!(
            MitigationPolicy::new().loader_integrity(LoaderIntegrity::Audit),
            1,
            4,
            3
        );
        field!(
            MitigationPolicy::new().strict_control_flow_guard(Mitigation::AlwaysOn),
            1,
            8,
            1
        );
        field!(
            MitigationPolicy::new().module_tampering(ModuleTampering::NoInherit),
            1,
            12,
            3
        );
        field!(
            MitigationPolicy::new().restrict_indirect_branch_prediction(Mitigation::AlwaysOn),
            1,
            16,
            1
        );
        field!(
            MitigationPolicy::new().allow_downgrade_dynamic_code(Mitigation::AlwaysOn),
            1,
            20,
            1
        );
        field!(
            MitigationPolicy::new().disable_speculative_store_bypass(Mitigation::AlwaysOn),
            1,
            24,
            1
        );
        field!(
            MitigationPolicy::new().cet_user_shadow_stacks(CetShadowStacks::Strict),
            1,
            28,
            3
        );
        field!(
            MitigationPolicy::new()
                .user_cet_context_ip_validation(UserCetContextIpValidation::Relaxed),
            1,
            32,
            3
        );
        field!(
            MitigationPolicy::new().block_non_cet_binaries(BlockNonCetBinaries::NonEhContinuation),
            1,
            36,
            3
        );
        field!(
            MitigationPolicy::new().extended_control_flow_guard(Mitigation::AlwaysOn),
            1,
            40,
            1
        );
        field!(
            MitigationPolicy::new().pointer_authentication(Mitigation::AlwaysOn),
            1,
            44,
            1
        );
        field!(
            MitigationPolicy::new().cet_dynamic_apis_out_of_process(Mitigation::AlwaysOn),
            1,
            48,
            1
        );
        field!(
            MitigationPolicy::new().restrict_core_sharing(Mitigation::AlwaysOn),
            1,
            52,
            1
        );
        field!(
            MitigationPolicy::new().disable_fsctl_system_calls(Mitigation::AlwaysOn),
            1,
            56,
            1
        );
    }
}
