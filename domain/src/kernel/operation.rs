//! The single vocabulary of action families that run on the executor.

/// One family names its registry slot, original-operation lock, completion table and log label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    ContentColumn,
    CenterAffiliation,
    InstitutionRole,
    WorkforceRole,
    InstitutionAppointment,
    Grant,
    WorkforceAssignment,
    GrantSuspension,
    GrantResumption,
    GrantTermination,
    Invitation,
    InvitationAdjustment,
    InvitationWithdrawal,
    Initialization,
    Logout,
    Suspension,
    Resumption,
    MobileCompletion,
    Correction,
    Cancellation,
    SystemCorrection,
    NumberAllocation,
    NumberCorrection,
    NumberRecovery,
    /// Workforce mutable configuration: directories, department positions and subtype rules.
    WorkforceConfiguration,
    #[cfg(test)]
    Toy,
}

impl Family {
    pub(super) fn shares_completions(self) -> bool {
        matches!(
            self,
            Self::InstitutionRole
                | Self::WorkforceRole
                | Self::InstitutionAppointment
                | Self::Grant
                | Self::WorkforceAssignment
                | Self::GrantSuspension
                | Self::GrantResumption
                | Self::GrantTermination
                | Self::Invitation
                | Self::InvitationAdjustment
                | Self::InvitationWithdrawal
                | Self::Initialization
                | Self::Logout
                | Self::Suspension
                | Self::Resumption
                | Self::MobileCompletion
                | Self::Correction
                | Self::Cancellation
                | Self::SystemCorrection
                | Self::NumberAllocation
                | Self::NumberCorrection
                | Self::NumberRecovery
                | Self::WorkforceConfiguration
        )
    }

    /// The original-operation lock domain. The Workforce cleanup trigger derives the same key.
    pub(crate) fn lock(self) -> &'static [u8] {
        match self {
            Self::ContentColumn => b"content/column/operation",
            Self::CenterAffiliation => b"center/affiliation/operation",
            Self::InstitutionRole => b"access/role/operation",
            Self::WorkforceRole => b"access/workforce/role/operation",
            Self::InstitutionAppointment => b"access/appointment/operation",
            Self::Grant => b"access/grant/operation",
            Self::WorkforceAssignment => b"access/assignment/operation",
            Self::GrantSuspension => b"access/suspension/operation",
            Self::GrantResumption => b"access/resumption/operation",
            Self::GrantTermination => b"access/termination/operation",
            Self::Invitation => b"access/invitation/operation",
            Self::InvitationAdjustment => b"access/adjustment/operation",
            Self::InvitationWithdrawal => b"access/withdrawal/operation",
            Self::Initialization => b"access/initialize/operation",
            Self::Logout => b"identity/revocation/operation",
            Self::Suspension => b"identity/suspension/operation",
            Self::Resumption => b"identity/resumption/operation",
            Self::MobileCompletion => b"transfer/completion",
            Self::Correction | Self::SystemCorrection => b"identity/correction/operation",
            Self::Cancellation => b"identity/cancellation/operation",
            Self::NumberAllocation => b"workforce/allocation/operation",
            Self::NumberCorrection => b"workforce/correction/operation",
            Self::NumberRecovery => b"workforce/recovery/operation",
            Self::WorkforceConfiguration => b"workforce/completion/operation",
            #[cfg(test)]
            Self::Toy => b"testing/completion/operation",
        }
    }

    /// The completion table built from the kernel template, selected from this fixed mapping.
    pub(crate) fn completions(self) -> &'static str {
        match self {
            Self::ContentColumn => crate::kernel::completion::CONTENT,
            Self::CenterAffiliation => crate::kernel::completion::CENTER,
            Self::InstitutionRole
            | Self::WorkforceRole
            | Self::InstitutionAppointment
            | Self::Grant
            | Self::WorkforceAssignment
            | Self::GrantSuspension
            | Self::GrantResumption
            | Self::GrantTermination
            | Self::Invitation
            | Self::InvitationAdjustment
            | Self::InvitationWithdrawal
            | Self::Initialization => crate::kernel::completion::ACCESS,
            Self::WorkforceConfiguration
            | Self::NumberAllocation
            | Self::NumberCorrection
            | Self::NumberRecovery => crate::kernel::completion::WORKFORCE,
            Self::Logout
            | Self::Suspension
            | Self::Resumption
            | Self::MobileCompletion
            | Self::Correction
            | Self::Cancellation
            | Self::SystemCorrection => crate::kernel::completion::IDENTITY,
            #[cfg(test)]
            Self::Toy => "testing.completions",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ContentColumn => "content_column",
            Self::CenterAffiliation => "center_affiliation",
            Self::InstitutionRole => "institution_role",
            Self::WorkforceRole => "workforce_role",
            Self::InstitutionAppointment => "institution_appointment",
            Self::Grant => "grant",
            Self::WorkforceAssignment => "workforce_assignment",
            Self::GrantSuspension => "grant_suspension",
            Self::GrantResumption => "grant_resumption",
            Self::GrantTermination => "grant_termination",
            Self::Invitation => "invitation",
            Self::InvitationAdjustment => "invitation_adjustment",
            Self::InvitationWithdrawal => "invitation_withdrawal",
            Self::Initialization => "initialization",
            Self::Logout => "logout",
            Self::Suspension => "suspension",
            Self::Resumption => "resumption",
            Self::MobileCompletion => "mobile_completion",
            Self::Correction => "correction",
            Self::Cancellation => "cancellation",
            Self::SystemCorrection => "system_correction",
            Self::NumberAllocation => "number_allocation",
            Self::NumberCorrection => "number_correction",
            Self::NumberRecovery => "number_recovery",
            Self::WorkforceConfiguration => "workforce_configuration",
            #[cfg(test)]
            Self::Toy => "toy",
        }
    }
}
