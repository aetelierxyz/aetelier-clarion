use std::fmt;

use solana_program::program_error::ProgramError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ClarionError {
    InstructionDataMalformed = 100,
    AccountCountMismatch = 101,
    AuthoritySignatureMissing = 102,
    SystemProgramMismatch = 103,
    StateAddressMismatch = 104,
    StateAlreadyInitialized = 105,
    StateOwnerMismatch = 106,
    StateDiscriminatorMismatch = 107,
    StateDataMalformed = 108,
    WindowAddressMismatch = 109,
    WindowAlreadyCommitted = 110,
    WindowOwnerMismatch = 111,
    WindowDiscriminatorMismatch = 112,
    WindowDataMalformed = 113,
    WindowLenZero = 114,
    MinRevealLagZero = 115,
    AuthorityMismatch = 116,
    WindowIdNotSequential = 117,
    WindowBoundsOverflow = 118,
    SlotStartOffGrid = 119,
    SlotEndOffGrid = 120,
    WindowNotClosed = 121,
    WindowAlreadyRevealed = 123,
    RevealLagNotElapsed = 124,
    LandedExceedsSubmitted = 125,
    ProgramSignatureMissing = 126,
}

impl ClarionError {
    const ALL: [Self; 26] = [
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

    const CODES: [(ClarionError, u32); 26] = [
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
        for unassigned in [99, 122, 127] {
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
