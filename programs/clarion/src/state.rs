use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::pubkey::Pubkey;

use crate::error::ClarionError;

pub const STATE_SEED: &[u8] = b"clarion";
pub const WINDOW_SEED: &[u8] = b"window";

pub const STATE_DISCRIMINATOR: u8 = 1;
pub const WINDOW_DISCRIMINATOR: u8 = 2;

pub const ARM_COUNT: usize = 4;

pub const STATE_SIZE: usize = 66;
pub const ARM_AGGREGATES_SIZE: usize = 44;
pub const WINDOW_SIZE: usize = 250;

#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq,
)]
pub struct ArmAggregates {
    pub tx_submitted: u32,
    pub tx_landed: u32,
    pub stl_p50_slots: u16,
    pub stl_p90_slots: u16,
    pub cu_price_p50_micro: u64,
    pub cu_price_p90_micro: u64,
    pub fee_total_lamports: u64,
    pub tip_total_lamports: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub discriminator: u8,
    pub authority: Pubkey,
    pub genesis_slot: u64,
    pub window_len: u64,
    pub min_reveal_lag_slots: u64,
    pub next_window_id: u64,
    pub bump: u8,
}

impl State {
    pub fn unpack(data: &[u8]) -> Result<Self, ClarionError> {
        if data.first() != Some(&STATE_DISCRIMINATOR) {
            return Err(ClarionError::StateDiscriminatorMismatch);
        }
        Self::try_from_slice(data).map_err(|_| ClarionError::StateDataMalformed)
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ClarionError> {
        if data.len() != STATE_SIZE {
            return Err(ClarionError::StateDataMalformed);
        }
        borsh::to_writer(data, self).map_err(|_| ClarionError::StateDataMalformed)
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub discriminator: u8,
    pub window_id: u64,
    pub slot_start: u64,
    pub slot_end: u64,
    pub merkle_root: [u8; 32],
    pub commit_slot: u64,
    pub reveal_slot: u64,
    pub aggregates: [ArmAggregates; ARM_COUNT],
    pub bump: u8,
}

impl Window {
    pub fn unpack(data: &[u8]) -> Result<Self, ClarionError> {
        if data.first() != Some(&WINDOW_DISCRIMINATOR) {
            return Err(ClarionError::WindowDiscriminatorMismatch);
        }
        Self::try_from_slice(data).map_err(|_| ClarionError::WindowDataMalformed)
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ClarionError> {
        if data.len() != WINDOW_SIZE {
            return Err(ClarionError::WindowDataMalformed);
        }
        borsh::to_writer(data, self).map_err(|_| ClarionError::WindowDataMalformed)
    }

    pub fn is_revealed(&self) -> bool {
        self.reveal_slot != 0
    }
}

pub fn state_address(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STATE_SEED], program_id)
}

pub fn window_address(program_id: &Pubkey, window_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[WINDOW_SEED, &window_id.to_le_bytes()], program_id)
}

pub fn window_bounds(
    genesis_slot: u64,
    window_len: u64,
    window_id: u64,
) -> Option<(u64, u64)> {
    let slot_start = genesis_slot.checked_add(window_id.checked_mul(window_len)?)?;
    let slot_end = slot_start.checked_add(window_len.checked_sub(1)?)?;
    Some((slot_start, slot_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            discriminator: STATE_DISCRIMINATOR,
            authority: Pubkey::new_from_array([7; 32]),
            genesis_slot: 1_000,
            window_len: 150,
            min_reveal_lag_slots: 150,
            next_window_id: 3,
            bump: 254,
        }
    }

    fn arm(seed: u32) -> ArmAggregates {
        ArmAggregates {
            tx_submitted: seed + 10,
            tx_landed: seed,
            stl_p50_slots: 1,
            stl_p90_slots: 4,
            cu_price_p50_micro: 5_000,
            cu_price_p90_micro: 90_000,
            fee_total_lamports: 123_456,
            tip_total_lamports: 654_321,
        }
    }

    fn window() -> Window {
        Window {
            discriminator: WINDOW_DISCRIMINATOR,
            window_id: 2,
            slot_start: 1_300,
            slot_end: 1_449,
            merkle_root: [9; 32],
            commit_slot: 1_500,
            reveal_slot: 1_650,
            aggregates: [arm(1), arm(2), arm(3), arm(4)],
            bump: 253,
        }
    }

    #[test]
    fn serialized_lengths_match_size_constants() {
        let arm_bytes = borsh::to_vec(&ArmAggregates::default()).unwrap();
        let state_bytes = borsh::to_vec(&state()).unwrap();
        let window_bytes = borsh::to_vec(&window()).unwrap();

        assert_eq!(arm_bytes.len(), ARM_AGGREGATES_SIZE);
        assert_eq!(arm_bytes.len(), 44);
        assert_eq!(state_bytes.len(), STATE_SIZE);
        assert_eq!(state_bytes.len(), 66);
        assert_eq!(window_bytes.len(), WINDOW_SIZE);
        assert_eq!(window_bytes.len(), 250);
        assert_eq!(ARM_COUNT * ARM_AGGREGATES_SIZE, 176);
    }

    #[test]
    fn discriminators_are_one_and_two() {
        assert_eq!(STATE_DISCRIMINATOR, 1);
        assert_eq!(WINDOW_DISCRIMINATOR, 2);
    }

    #[test]
    fn state_layout_follows_field_order() {
        let bytes = borsh::to_vec(&state()).unwrap();

        assert_eq!(bytes[0], STATE_DISCRIMINATOR);
        assert_eq!(bytes[1..33], [7; 32]);
        assert_eq!(bytes[33..41], 1_000u64.to_le_bytes());
        assert_eq!(bytes[41..49], 150u64.to_le_bytes());
        assert_eq!(bytes[49..57], 150u64.to_le_bytes());
        assert_eq!(bytes[57..65], 3u64.to_le_bytes());
        assert_eq!(bytes[65], 254);
    }

    #[test]
    fn state_layout_separates_window_len_from_min_reveal_lag_slots() {
        let state = State {
            min_reveal_lag_slots: 151,
            ..state()
        };
        let bytes = borsh::to_vec(&state).unwrap();

        assert_eq!(bytes[41..49], 150u64.to_le_bytes());
        assert_eq!(bytes[49..57], 151u64.to_le_bytes());
    }

    #[test]
    fn window_layout_follows_field_order() {
        let bytes = borsh::to_vec(&window()).unwrap();

        assert_eq!(bytes[0], WINDOW_DISCRIMINATOR);
        assert_eq!(bytes[1..9], 2u64.to_le_bytes());
        assert_eq!(bytes[9..17], 1_300u64.to_le_bytes());
        assert_eq!(bytes[17..25], 1_449u64.to_le_bytes());
        assert_eq!(bytes[25..57], [9; 32]);
        assert_eq!(bytes[57..65], 1_500u64.to_le_bytes());
        assert_eq!(bytes[65..73], 1_650u64.to_le_bytes());
        assert_eq!(bytes[73..117], borsh::to_vec(&arm(1)).unwrap()[..]);
        assert_eq!(bytes[205..249], borsh::to_vec(&arm(4)).unwrap()[..]);
        assert_eq!(bytes[249], 253);
    }

    #[test]
    fn arm_layout_follows_field_order() {
        let bytes = borsh::to_vec(&arm(5)).unwrap();

        assert_eq!(bytes[0..4], 15u32.to_le_bytes());
        assert_eq!(bytes[4..8], 5u32.to_le_bytes());
        assert_eq!(bytes[8..10], 1u16.to_le_bytes());
        assert_eq!(bytes[10..12], 4u16.to_le_bytes());
        assert_eq!(bytes[12..20], 5_000u64.to_le_bytes());
        assert_eq!(bytes[20..28], 90_000u64.to_le_bytes());
        assert_eq!(bytes[28..36], 123_456u64.to_le_bytes());
        assert_eq!(bytes[36..44], 654_321u64.to_le_bytes());
    }

    #[test]
    fn state_round_trips_through_pack_and_unpack() {
        let mut data = [0u8; STATE_SIZE];

        state().pack(&mut data).unwrap();

        assert_eq!(State::unpack(&data), Ok(state()));
    }

    #[test]
    fn window_round_trips_through_pack_and_unpack() {
        let mut data = [0u8; WINDOW_SIZE];

        window().pack(&mut data).unwrap();

        assert_eq!(Window::unpack(&data), Ok(window()));
    }

    #[test]
    fn state_unpack_rejects_another_discriminator() {
        let mut data = borsh::to_vec(&state()).unwrap();
        data[0] = WINDOW_DISCRIMINATOR;

        assert_eq!(
            State::unpack(&data),
            Err(ClarionError::StateDiscriminatorMismatch)
        );
        assert_eq!(
            State::unpack(&[]),
            Err(ClarionError::StateDiscriminatorMismatch)
        );
    }

    #[test]
    fn window_unpack_rejects_another_discriminator() {
        let state_bytes = borsh::to_vec(&state()).unwrap();

        assert_eq!(
            Window::unpack(&state_bytes),
            Err(ClarionError::WindowDiscriminatorMismatch)
        );
        assert_eq!(
            Window::unpack(&[]),
            Err(ClarionError::WindowDiscriminatorMismatch)
        );
    }

    #[test]
    fn unpack_rejects_truncated_and_oversized_data() {
        let state_bytes = borsh::to_vec(&state()).unwrap();
        let window_bytes = borsh::to_vec(&window()).unwrap();
        let oversized_state = [state_bytes.as_slice(), &[0]].concat();
        let oversized_window = [window_bytes.as_slice(), &[0]].concat();

        assert_eq!(
            State::unpack(&state_bytes[..STATE_SIZE - 1]),
            Err(ClarionError::StateDataMalformed)
        );
        assert_eq!(
            State::unpack(&oversized_state),
            Err(ClarionError::StateDataMalformed)
        );
        assert_eq!(
            Window::unpack(&window_bytes[..WINDOW_SIZE - 1]),
            Err(ClarionError::WindowDataMalformed)
        );
        assert_eq!(
            Window::unpack(&oversized_window),
            Err(ClarionError::WindowDataMalformed)
        );
    }

    #[test]
    fn pack_rejects_a_buffer_of_another_size() {
        assert_eq!(
            state().pack(&mut [0u8; STATE_SIZE - 1]),
            Err(ClarionError::StateDataMalformed)
        );
        assert_eq!(
            state().pack(&mut [0u8; STATE_SIZE + 1]),
            Err(ClarionError::StateDataMalformed)
        );
        assert_eq!(
            window().pack(&mut [0u8; WINDOW_SIZE - 1]),
            Err(ClarionError::WindowDataMalformed)
        );
        assert_eq!(
            window().pack(&mut [0u8; WINDOW_SIZE + 1]),
            Err(ClarionError::WindowDataMalformed)
        );
    }

    #[test]
    fn window_is_revealed_once_reveal_slot_is_set() {
        let committed = Window {
            reveal_slot: 0,
            ..window()
        };

        assert!(!committed.is_revealed());
        assert!(window().is_revealed());
    }

    #[test]
    fn addresses_derive_from_seeds_and_program_id() {
        let program_id = Pubkey::new_from_array([3; 32]);
        let other_program_id = Pubkey::new_from_array([4; 32]);

        let (state_key, state_bump) = state_address(&program_id);
        let (window_key, window_bump) = window_address(&program_id, 5);

        assert_eq!(
            Pubkey::create_program_address(&[b"clarion", &[state_bump]], &program_id),
            Ok(state_key)
        );
        assert_eq!(
            Pubkey::create_program_address(
                &[b"window", &5u64.to_le_bytes(), &[window_bump]],
                &program_id
            ),
            Ok(window_key)
        );
        assert_ne!(state_key, state_address(&other_program_id).0);
        assert_ne!(window_key, window_address(&program_id, 6).0);
        assert_ne!(window_key, window_address(&other_program_id, 5).0);
    }

    #[test]
    fn window_bounds_follow_the_grid() {
        assert_eq!(window_bounds(1_000, 150, 0), Some((1_000, 1_149)));
        assert_eq!(window_bounds(1_000, 150, 1), Some((1_150, 1_299)));
        assert_eq!(window_bounds(0, 1, 7), Some((7, 7)));
        assert_eq!(
            window_bounds(u64::MAX - 9, 10, 0),
            Some((u64::MAX - 9, u64::MAX))
        );
    }

    #[test]
    fn window_bounds_reject_overflow() {
        assert_eq!(window_bounds(0, u64::MAX, 2), None);
        assert_eq!(window_bounds(u64::MAX, 10, 1), None);
        assert_eq!(window_bounds(u64::MAX - 8, 10, 0), None);
        assert_eq!(window_bounds(1_000, 0, 0), None);
    }

    #[test]
    fn window_bounds_reject_a_product_overflow_alone() {
        assert_eq!(window_bounds(0, 1 << 63, 2), None);
    }

    #[test]
    fn window_bounds_reject_zero_window_len_at_genesis_zero() {
        assert_eq!(window_bounds(0, 0, 0), None);
    }
}
