use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    entrypoint::ProgramResult, log::sol_log_data, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::state::{ARM_COUNT, ArmAggregates};

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "idl", derive(codama::CodamaEvents))]
pub enum ClarionEvent {
    Initialized {
        authority: Pubkey,
        genesis_slot: u64,
        window_len: u64,
        min_reveal_lag_slots: u64,
    },
    Committed {
        window_id: u64,
        slot_start: u64,
        slot_end: u64,
        merkle_root: [u8; 32],
        commit_slot: u64,
    },
    Revealed {
        window_id: u64,
        reveal_slot: u64,
        #[cfg_attr(feature = "idl", codama(type = array(link("armAggregates"), 4)))]
        aggregates: [ArmAggregates; ARM_COUNT],
    },
    AuthorityProposed {
        authority: Pubkey,
        proposed_authority: Pubkey,
    },
    AuthorityAccepted {
        previous_authority: Pubkey,
        authority: Pubkey,
    },
    AuthorityProposalCancelled {
        authority: Pubkey,
        proposed_authority: Pubkey,
    },
}

impl ClarionEvent {
    pub fn emit(&self) -> ProgramResult {
        let bytes = borsh::to_vec(self).map_err(|_| ProgramError::BorshIoError)?;
        sol_log_data(&[&bytes]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ARM_AGGREGATES_SIZE, Arm};

    const AUTHORITY: Pubkey = Pubkey::new_from_array([1; 32]);
    const SUCCESSOR: Pubkey = Pubkey::new_from_array([2; 32]);

    fn aggregates() -> [ArmAggregates; ARM_COUNT] {
        Arm::ALL.map(|arm| ArmAggregates {
            arm,
            tx_submitted: 20,
            tx_landed: 18,
            stl_p50_slots: 1,
            stl_p90_slots: 3,
            cu_price_p50_micro: 1_000,
            cu_price_p90_micro: 9_000,
            fee_total_lamports: 100_000,
            tip_total_lamports: 50_000,
        })
    }

    fn every_variant() -> [ClarionEvent; 6] {
        [
            ClarionEvent::Initialized {
                authority: AUTHORITY,
                genesis_slot: 1_000,
                window_len: 150,
                min_reveal_lag_slots: 151,
            },
            ClarionEvent::Committed {
                window_id: 4,
                slot_start: 1_600,
                slot_end: 1_749,
                merkle_root: [8; 32],
                commit_slot: 1_760,
            },
            ClarionEvent::Revealed {
                window_id: 4,
                reveal_slot: 1_911,
                aggregates: aggregates(),
            },
            ClarionEvent::AuthorityProposed {
                authority: AUTHORITY,
                proposed_authority: SUCCESSOR,
            },
            ClarionEvent::AuthorityAccepted {
                previous_authority: AUTHORITY,
                authority: SUCCESSOR,
            },
            ClarionEvent::AuthorityProposalCancelled {
                authority: AUTHORITY,
                proposed_authority: SUCCESSOR,
            },
        ]
    }

    #[test]
    fn variants_encode_with_a_one_byte_tag_in_declaration_order() {
        for (tag, event) in every_variant().into_iter().enumerate() {
            let bytes = borsh::to_vec(&event).unwrap();

            assert_eq!(bytes[0], u8::try_from(tag).unwrap());
        }
    }

    #[test]
    fn encoded_lengths_follow_the_field_sizes() {
        let lengths = every_variant().map(|event| borsh::to_vec(&event).unwrap().len());

        assert_eq!(
            lengths,
            [
                1 + 32 + 3 * 8,
                1 + 3 * 8 + 32 + 8,
                1 + 2 * 8 + ARM_COUNT * ARM_AGGREGATES_SIZE,
                1 + 2 * 32,
                1 + 2 * 32,
                1 + 2 * 32,
            ]
        );
        assert_eq!(lengths, [57, 65, 197, 65, 65, 65]);
    }

    #[test]
    fn initialized_layout_follows_field_order() {
        let bytes = borsh::to_vec(&every_variant()[0]).unwrap();

        assert_eq!(bytes[1..33], [1; 32]);
        assert_eq!(bytes[33..41], 1_000u64.to_le_bytes());
        assert_eq!(bytes[41..49], 150u64.to_le_bytes());
        assert_eq!(bytes[49..57], 151u64.to_le_bytes());
    }

    #[test]
    fn committed_layout_follows_field_order() {
        let bytes = borsh::to_vec(&every_variant()[1]).unwrap();

        assert_eq!(bytes[1..9], 4u64.to_le_bytes());
        assert_eq!(bytes[9..17], 1_600u64.to_le_bytes());
        assert_eq!(bytes[17..25], 1_749u64.to_le_bytes());
        assert_eq!(bytes[25..57], [8; 32]);
        assert_eq!(bytes[57..65], 1_760u64.to_le_bytes());
    }

    #[test]
    fn revealed_layout_follows_field_order() {
        let bytes = borsh::to_vec(&every_variant()[2]).unwrap();

        assert_eq!(bytes[1..9], 4u64.to_le_bytes());
        assert_eq!(bytes[9..17], 1_911u64.to_le_bytes());
        for (index, entry) in aggregates().iter().enumerate() {
            let offset = 17 + index * ARM_AGGREGATES_SIZE;
            assert_eq!(
                bytes[offset..offset + ARM_AGGREGATES_SIZE],
                borsh::to_vec(entry).unwrap()[..]
            );
        }
    }

    #[test]
    fn authority_layouts_follow_field_order() {
        for event in &every_variant()[3..] {
            let bytes = borsh::to_vec(event).unwrap();

            assert_eq!(bytes[1..33], [1; 32]);
            assert_eq!(bytes[33..65], [2; 32]);
        }
    }

    #[test]
    fn every_variant_round_trips() {
        for event in every_variant() {
            let bytes = borsh::to_vec(&event).unwrap();

            assert_eq!(ClarionEvent::try_from_slice(&bytes).unwrap(), event);
        }
    }

    #[test]
    fn decoding_rejects_an_unknown_tag() {
        let mut bytes = borsh::to_vec(&every_variant()[3]).unwrap();
        for tag in 6..=u8::MAX {
            bytes[0] = tag;

            assert!(ClarionEvent::try_from_slice(&bytes).is_err());
        }
    }

    #[test]
    fn emit_succeeds_for_every_variant() {
        for event in every_variant() {
            assert_eq!(event.emit(), Ok(()));
        }
    }
}
