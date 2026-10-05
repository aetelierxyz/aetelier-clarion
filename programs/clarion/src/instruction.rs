use borsh::{BorshDeserialize, BorshSerialize};

use crate::{
    error::ClarionError,
    state::{ARM_COUNT, ArmAggregates},
};

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClarionInstruction {
    Init {
        genesis_slot: u64,
        window_len: u64,
        min_reveal_lag_slots: u64,
    },
    Commit {
        window_id: u64,
        slot_start: u64,
        slot_end: u64,
        merkle_root: [u8; 32],
    },
    Reveal {
        window_id: u64,
        aggregates: [ArmAggregates; ARM_COUNT],
    },
}

impl ClarionInstruction {
    pub fn unpack(data: &[u8]) -> Result<Self, ClarionError> {
        Self::try_from_slice(data).map_err(|_| ClarionError::InstructionDataMalformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ARM_AGGREGATES_SIZE;

    fn init() -> ClarionInstruction {
        ClarionInstruction::Init {
            genesis_slot: 1_000,
            window_len: 150,
            min_reveal_lag_slots: 151,
        }
    }

    fn commit() -> ClarionInstruction {
        ClarionInstruction::Commit {
            window_id: 4,
            slot_start: 1_600,
            slot_end: 1_749,
            merkle_root: [8; 32],
        }
    }

    fn reveal() -> ClarionInstruction {
        let arm = ArmAggregates {
            tx_submitted: 20,
            tx_landed: 18,
            stl_p50_slots: 1,
            stl_p90_slots: 3,
            cu_price_p50_micro: 1_000,
            cu_price_p90_micro: 9_000,
            fee_total_lamports: 100_000,
            tip_total_lamports: 50_000,
        };
        ClarionInstruction::Reveal {
            window_id: 4,
            aggregates: [arm; ARM_COUNT],
        }
    }

    #[test]
    fn variants_encode_with_a_one_byte_tag_in_declaration_order() {
        let init_bytes = borsh::to_vec(&init()).unwrap();
        let commit_bytes = borsh::to_vec(&commit()).unwrap();
        let reveal_bytes = borsh::to_vec(&reveal()).unwrap();

        assert_eq!(init_bytes[0], 0);
        assert_eq!(init_bytes.len(), 1 + 3 * 8);
        assert_eq!(init_bytes[1..9], 1_000u64.to_le_bytes());
        assert_eq!(init_bytes[9..17], 150u64.to_le_bytes());
        assert_eq!(init_bytes[17..25], 151u64.to_le_bytes());

        assert_eq!(commit_bytes[0], 1);
        assert_eq!(commit_bytes.len(), 1 + 3 * 8 + 32);
        assert_eq!(commit_bytes[1..9], 4u64.to_le_bytes());
        assert_eq!(commit_bytes[9..17], 1_600u64.to_le_bytes());
        assert_eq!(commit_bytes[17..25], 1_749u64.to_le_bytes());
        assert_eq!(commit_bytes[25..57], [8; 32]);

        assert_eq!(reveal_bytes[0], 2);
        assert_eq!(reveal_bytes.len(), 1 + 8 + ARM_COUNT * ARM_AGGREGATES_SIZE);
        assert_eq!(reveal_bytes[1..9], 4u64.to_le_bytes());
    }

    #[test]
    fn unpack_round_trips_every_variant() {
        for instruction in [init(), commit(), reveal()] {
            let bytes = borsh::to_vec(&instruction).unwrap();

            assert_eq!(ClarionInstruction::unpack(&bytes), Ok(instruction));
        }
    }

    #[test]
    fn unpack_rejects_empty_data() {
        assert_eq!(
            ClarionInstruction::unpack(&[]),
            Err(ClarionError::InstructionDataMalformed)
        );
    }

    #[test]
    fn unpack_rejects_an_unknown_variant() {
        for tag in 3..=u8::MAX {
            let mut bytes = borsh::to_vec(&init()).unwrap();
            bytes[0] = tag;

            assert_eq!(
                ClarionInstruction::unpack(&bytes),
                Err(ClarionError::InstructionDataMalformed)
            );
        }
    }

    #[test]
    fn unpack_rejects_trailing_bytes() {
        for instruction in [init(), commit(), reveal()] {
            let mut bytes = borsh::to_vec(&instruction).unwrap();
            bytes.push(0);

            assert_eq!(
                ClarionInstruction::unpack(&bytes),
                Err(ClarionError::InstructionDataMalformed)
            );
        }
    }

    #[test]
    fn unpack_rejects_truncated_data() {
        for instruction in [init(), commit(), reveal()] {
            let bytes = borsh::to_vec(&instruction).unwrap();

            for length in 1..bytes.len() {
                assert_eq!(
                    ClarionInstruction::unpack(&bytes[..length]),
                    Err(ClarionError::InstructionDataMalformed)
                );
            }
        }
    }
}
