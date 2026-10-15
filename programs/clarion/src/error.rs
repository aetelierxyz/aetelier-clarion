use std::fmt;

use solana_program::program_error::ProgramError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "idl", derive(codama::CodamaErrors))]
#[repr(u32)]
pub enum ClarionError {
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "instruction data does not decode"))
    )]
    InstructionDataMalformed = 100,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "account list length does not match the instruction"))
    )]
    AccountCountMismatch = 101,
    #[cfg_attr(feature = "idl", codama(error(message = "authority did not sign")))]
    AuthoritySignatureMissing = 102,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "system program account has another id"))
    )]
    SystemProgramMismatch = 103,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "state account is not the derived address"))
    )]
    StateAddressMismatch = 104,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "state account already exists"))
    )]
    StateAlreadyInitialized = 105,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "state account is not owned by the program"))
    )]
    StateOwnerMismatch = 106,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "state account has another discriminator"))
    )]
    StateDiscriminatorMismatch = 107,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "state account data does not decode"))
    )]
    StateDataMalformed = 108,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window account is not the derived address"))
    )]
    WindowAddressMismatch = 109,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window account already exists"))
    )]
    WindowAlreadyCommitted = 110,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window account is not owned by the program"))
    )]
    WindowOwnerMismatch = 111,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window account has another discriminator"))
    )]
    WindowDiscriminatorMismatch = 112,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window account data does not decode"))
    )]
    WindowDataMalformed = 113,
    #[cfg_attr(feature = "idl", codama(error(message = "window_len is zero")))]
    WindowLenZero = 114,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "min_reveal_lag_slots is zero"))
    )]
    MinRevealLagZero = 115,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "signer is not the stored authority"))
    )]
    AuthorityMismatch = 116,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "window_id is not next_window_id"))
    )]
    WindowIdNotSequential = 117,
    #[cfg_attr(feature = "idl", codama(error(message = "window bounds overflow u64")))]
    WindowBoundsOverflow = 118,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "slot_start is off the window grid"))
    )]
    SlotStartOffGrid = 119,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "slot_end is off the window grid"))
    )]
    SlotEndOffGrid = 120,
    #[cfg_attr(feature = "idl", codama(error(message = "slot_end has not passed")))]
    WindowNotClosed = 121,
    #[cfg_attr(feature = "idl", codama(error(message = "window is already revealed")))]
    WindowAlreadyRevealed = 123,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "min_reveal_lag_slots has not elapsed since commit"))
    )]
    RevealLagNotElapsed = 124,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "tx_landed exceeds tx_submitted"))
    )]
    LandedExceedsSubmitted = 125,
    #[cfg_attr(feature = "idl", codama(error(message = "program id did not sign")))]
    ProgramSignatureMissing = 126,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "previous window is not revealed"))
    )]
    PreviousWindowNotRevealed = 127,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "aggregates are not in arm order"))
    )]
    ArmOutOfOrder = 128,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "min_reveal_lag_slots exceeds window_len"))
    )]
    RevealLagExceedsWindowLen = 129,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "genesis_slot is before the current slot"))
    )]
    GenesisSlotPassed = 130,
    #[cfg_attr(
        feature = "idl",
        codama(error(
            message = "genesis_slot is more than MAX_GENESIS_LEAD_SLOTS ahead of the current slot"
        ))
    )]
    GenesisSlotTooFar = 131,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "new authority is the stored authority"))
    )]
    AuthorityUnchanged = 132,
    #[cfg_attr(
        feature = "idl",
        codama(error(
            message = "authority proposal account is not the derived address"
        ))
    )]
    ProposalAddressMismatch = 133,
    #[cfg_attr(
        feature = "idl",
        codama(error(
            message = "authority proposal account is not owned by the program"
        ))
    )]
    ProposalOwnerMismatch = 134,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "authority proposal account has another discriminator"))
    )]
    ProposalDiscriminatorMismatch = 135,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "authority proposal account data does not decode"))
    )]
    ProposalDataMalformed = 136,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "authority proposal account does not exist"))
    )]
    ProposalMissing = 137,
    #[cfg_attr(
        feature = "idl",
        codama(error(message = "signer is not the proposed authority"))
    )]
    ProposedAuthorityMismatch = 138,
}

impl ClarionError {
    const ALL: [Self; 38] = [
        Self::InstructionDataMalformed,
        Self::AccountCountMismatch,
        Self::AuthoritySignatureMissing,
        Self::SystemProgramMismatch,
        Self::StateAddressMismatch,
        Self::StateAlreadyInitialized,
        Self::StateOwnerMismatch,
        Self::StateDiscriminatorMismatch,
        Self::StateDataMalformed,
        Self::WindowAddressMismatch,
        Self::WindowAlreadyCommitted,
        Self::WindowOwnerMismatch,
        Self::WindowDiscriminatorMismatch,
        Self::WindowDataMalformed,
        Self::WindowLenZero,
        Self::MinRevealLagZero,
        Self::AuthorityMismatch,
        Self::WindowIdNotSequential,
        Self::WindowBoundsOverflow,
        Self::SlotStartOffGrid,
        Self::SlotEndOffGrid,
        Self::WindowNotClosed,
        Self::WindowAlreadyRevealed,
        Self::RevealLagNotElapsed,
        Self::LandedExceedsSubmitted,
        Self::ProgramSignatureMissing,
        Self::PreviousWindowNotRevealed,
        Self::ArmOutOfOrder,
        Self::RevealLagExceedsWindowLen,
        Self::GenesisSlotPassed,
        Self::GenesisSlotTooFar,
        Self::AuthorityUnchanged,
        Self::ProposalAddressMismatch,
        Self::ProposalOwnerMismatch,
        Self::ProposalDiscriminatorMismatch,
        Self::ProposalDataMalformed,
        Self::ProposalMissing,
        Self::ProposedAuthorityMismatch,
    ];

    pub const fn code(self) -> u32 {
        self as u32
    }

    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|error| error.code() == code)
    }
}

impl fmt::Display for ClarionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InstructionDataMalformed => "instruction data does not decode",
            Self::AccountCountMismatch => {
                "account list length does not match the instruction"
            }
            Self::AuthoritySignatureMissing => "authority did not sign",
            Self::SystemProgramMismatch => "system program account has another id",
            Self::StateAddressMismatch => "state account is not the derived address",
            Self::StateAlreadyInitialized => "state account already exists",
            Self::StateOwnerMismatch => "state account is not owned by the program",
            Self::StateDiscriminatorMismatch => "state account has another discriminator",
            Self::StateDataMalformed => "state account data does not decode",
            Self::WindowAddressMismatch => "window account is not the derived address",
            Self::WindowAlreadyCommitted => "window account already exists",
            Self::WindowOwnerMismatch => "window account is not owned by the program",
            Self::WindowDiscriminatorMismatch => {
                "window account has another discriminator"
            }
            Self::WindowDataMalformed => "window account data does not decode",
            Self::WindowLenZero => "window_len is zero",
            Self::MinRevealLagZero => "min_reveal_lag_slots is zero",
            Self::AuthorityMismatch => "signer is not the stored authority",
            Self::WindowIdNotSequential => "window_id is not next_window_id",
            Self::WindowBoundsOverflow => "window bounds overflow u64",
            Self::SlotStartOffGrid => "slot_start is off the window grid",
            Self::SlotEndOffGrid => "slot_end is off the window grid",
            Self::WindowNotClosed => "slot_end has not passed",
            Self::WindowAlreadyRevealed => "window is already revealed",
            Self::RevealLagNotElapsed => {
                "min_reveal_lag_slots has not elapsed since commit"
            }
            Self::LandedExceedsSubmitted => "tx_landed exceeds tx_submitted",
            Self::ProgramSignatureMissing => "program id did not sign",
            Self::PreviousWindowNotRevealed => "previous window is not revealed",
            Self::ArmOutOfOrder => "aggregates are not in arm order",
            Self::RevealLagExceedsWindowLen => "min_reveal_lag_slots exceeds window_len",
            Self::GenesisSlotPassed => "genesis_slot is before the current slot",
            Self::GenesisSlotTooFar => {
                "genesis_slot is more than MAX_GENESIS_LEAD_SLOTS ahead of the current slot"
            }
            Self::AuthorityUnchanged => "new authority is the stored authority",
            Self::ProposalAddressMismatch => {
                "authority proposal account is not the derived address"
            }
            Self::ProposalOwnerMismatch => {
                "authority proposal account is not owned by the program"
            }
            Self::ProposalDiscriminatorMismatch => {
                "authority proposal account has another discriminator"
            }
            Self::ProposalDataMalformed => {
                "authority proposal account data does not decode"
            }
            Self::ProposalMissing => "authority proposal account does not exist",
            Self::ProposedAuthorityMismatch => "signer is not the proposed authority",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ClarionError {}

impl From<ClarionError> for ProgramError {
    fn from(error: ClarionError) -> Self {
        Self::Custom(error.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [(ClarionError, u32); 38] = [
        (ClarionError::InstructionDataMalformed, 100),
        (ClarionError::AccountCountMismatch, 101),
        (ClarionError::AuthoritySignatureMissing, 102),
        (ClarionError::SystemProgramMismatch, 103),
        (ClarionError::StateAddressMismatch, 104),
        (ClarionError::StateAlreadyInitialized, 105),
        (ClarionError::StateOwnerMismatch, 106),
        (ClarionError::StateDiscriminatorMismatch, 107),
        (ClarionError::StateDataMalformed, 108),
        (ClarionError::WindowAddressMismatch, 109),
        (ClarionError::WindowAlreadyCommitted, 110),
        (ClarionError::WindowOwnerMismatch, 111),
        (ClarionError::WindowDiscriminatorMismatch, 112),
        (ClarionError::WindowDataMalformed, 113),
        (ClarionError::WindowLenZero, 114),
        (ClarionError::MinRevealLagZero, 115),
        (ClarionError::AuthorityMismatch, 116),
        (ClarionError::WindowIdNotSequential, 117),
        (ClarionError::WindowBoundsOverflow, 118),
        (ClarionError::SlotStartOffGrid, 119),
        (ClarionError::SlotEndOffGrid, 120),
        (ClarionError::WindowNotClosed, 121),
        (ClarionError::WindowAlreadyRevealed, 123),
        (ClarionError::RevealLagNotElapsed, 124),
        (ClarionError::LandedExceedsSubmitted, 125),
        (ClarionError::ProgramSignatureMissing, 126),
        (ClarionError::PreviousWindowNotRevealed, 127),
        (ClarionError::ArmOutOfOrder, 128),
        (ClarionError::RevealLagExceedsWindowLen, 129),
        (ClarionError::GenesisSlotPassed, 130),
        (ClarionError::GenesisSlotTooFar, 131),
        (ClarionError::AuthorityUnchanged, 132),
        (ClarionError::ProposalAddressMismatch, 133),
        (ClarionError::ProposalOwnerMismatch, 134),
        (ClarionError::ProposalDiscriminatorMismatch, 135),
        (ClarionError::ProposalDataMalformed, 136),
        (ClarionError::ProposalMissing, 137),
        (ClarionError::ProposedAuthorityMismatch, 138),
    ];

    #[test]
    fn codes_are_stable() {
        for (error, code) in CODES {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn table_holds_every_variant_exactly_once() {
        assert_eq!(ClarionError::ALL.len(), CODES.len());
        for (error, _) in CODES {
            let held = ClarionError::ALL
                .iter()
                .filter(|entry| **entry == error)
                .count();
            assert_eq!(held, 1, "{error:?}");
        }
    }

    #[test]
    fn from_code_round_trips_every_code() {
        for (error, code) in CODES {
            assert_eq!(ClarionError::from_code(code), Some(error));
        }
        for unassigned in [99, 122, 139] {
            assert_eq!(ClarionError::from_code(unassigned), None);
        }
        let assigned = (0..=u32::from(u16::MAX))
            .filter(|code| ClarionError::from_code(*code).is_some())
            .count();
        assert_eq!(assigned, CODES.len());
    }

    #[test]
    fn converts_to_custom_program_error() {
        for (error, code) in CODES {
            assert_eq!(ProgramError::from(error), ProgramError::Custom(code));
        }
    }

    #[test]
    fn messages_are_distinct() {
        let mut messages: Vec<String> =
            CODES.iter().map(|(error, _)| error.to_string()).collect();
        messages.sort();
        messages.dedup();
        assert_eq!(messages.len(), CODES.len());
    }
}
