use std::fmt;

use clarion::{
    STATE_DISCRIMINATOR, STATE_SIZE, State, WINDOW_DISCRIMINATOR, WINDOW_SIZE, Window,
    window_bounds,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    State,
    Window,
}

impl fmt::Display for AccountKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::State => "state",
            Self::Window => "window",
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("{account} account holds {actual} bytes, expected {expected}")]
    Length {
        account: AccountKind,
        expected: usize,
        actual: usize,
    },
    #[error("{account} account discriminator is {actual}, expected {expected}")]
    Discriminator {
        account: AccountKind,
        expected: u8,
        actual: u8,
    },
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error(
    "window {window_id} has no u64 bounds on the grid of genesis_slot {genesis_slot} \
     and window_len {window_len}"
)]
pub struct BoundsError {
    pub genesis_slot: u64,
    pub window_len: u64,
    pub window_id: u64,
}

pub fn decode_state(data: &[u8]) -> Result<State, DecodeError> {
    State::unpack(data).map_err(|_| {
        layout_error(AccountKind::State, data, STATE_SIZE, STATE_DISCRIMINATOR)
    })
}

pub fn decode_window(data: &[u8]) -> Result<Window, DecodeError> {
    Window::unpack(data).map_err(|_| {
        layout_error(AccountKind::Window, data, WINDOW_SIZE, WINDOW_DISCRIMINATOR)
    })
}

pub fn window_slots(state: &State, window_id: u64) -> Result<(u64, u64), BoundsError> {
    window_bounds(state.genesis_slot, state.window_len, window_id).ok_or(BoundsError {
        genesis_slot: state.genesis_slot,
        window_len: state.window_len,
        window_id,
    })
}

fn layout_error(
    account: AccountKind,
    data: &[u8],
    size: usize,
    discriminator: u8,
) -> DecodeError {
    match data.first() {
        Some(&actual) if data.len() == size => DecodeError::Discriminator {
            account,
            expected: discriminator,
            actual,
        },
        _ => DecodeError::Length {
            account,
            expected: size,
            actual: data.len(),
        },
    }
}

#[cfg(test)]
mod tests {
    use clarion::{ARM_COUNT, ArmAggregates};
    use solana_program::pubkey::Pubkey;

    use super::*;

    fn state() -> State {
        State {
            discriminator: STATE_DISCRIMINATOR,
            authority: Pubkey::new_from_array([7; 32]),
            genesis_slot: 1_000,
            window_len: 150,
            min_reveal_lag_slots: 151,
            next_window_id: 3,
            bump: 254,
        }
    }

    fn window() -> Window {
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
        Window {
            discriminator: WINDOW_DISCRIMINATOR,
            window_id: 2,
            slot_start: 1_300,
            slot_end: 1_449,
            merkle_root: [9; 32],
            commit_slot: 1_500,
            reveal_slot: 1_651,
            aggregates: [arm; ARM_COUNT],
            bump: 253,
        }
    }

    fn state_bytes() -> [u8; STATE_SIZE] {
        let mut data = [0u8; STATE_SIZE];
        state().pack(&mut data).unwrap();
        data
    }

    fn window_bytes() -> [u8; WINDOW_SIZE] {
        let mut data = [0u8; WINDOW_SIZE];
        window().pack(&mut data).unwrap();
        data
    }

    #[test]
    fn decode_state_returns_every_packed_field() {
        assert_eq!(decode_state(&state_bytes()), Ok(state()));
    }

    #[test]
    fn decode_window_returns_every_packed_field() {
        assert_eq!(decode_window(&window_bytes()), Ok(window()));
    }

    #[test]
    fn decode_state_rejects_another_length() {
        let data = state_bytes();
        let oversized = [data.as_slice(), &[0]].concat();

        for (candidate, actual) in [
            (&[][..], 0),
            (&data[..STATE_SIZE - 1], STATE_SIZE - 1),
            (oversized.as_slice(), STATE_SIZE + 1),
            (&window_bytes()[..], WINDOW_SIZE),
        ] {
            assert_eq!(
                decode_state(candidate),
                Err(DecodeError::Length {
                    account: AccountKind::State,
                    expected: 66,
                    actual,
                })
            );
        }
    }

    #[test]
    fn decode_window_rejects_another_length() {
        let data = window_bytes();
        let oversized = [data.as_slice(), &[0]].concat();

        for (candidate, actual) in [
            (&[][..], 0),
            (&data[..WINDOW_SIZE - 1], WINDOW_SIZE - 1),
            (oversized.as_slice(), WINDOW_SIZE + 1),
            (&state_bytes()[..], STATE_SIZE),
        ] {
            assert_eq!(
                decode_window(candidate),
                Err(DecodeError::Length {
                    account: AccountKind::Window,
                    expected: 250,
                    actual,
                })
            );
        }
    }

    #[test]
    fn decode_state_rejects_another_discriminator() {
        for actual in [0, WINDOW_DISCRIMINATOR, u8::MAX] {
            let mut data = state_bytes();
            data[0] = actual;

            assert_eq!(
                decode_state(&data),
                Err(DecodeError::Discriminator {
                    account: AccountKind::State,
                    expected: 1,
                    actual,
                })
            );
        }
    }

    #[test]
    fn decode_window_rejects_another_discriminator() {
        for actual in [0, STATE_DISCRIMINATOR, u8::MAX] {
            let mut data = window_bytes();
            data[0] = actual;

            assert_eq!(
                decode_window(&data),
                Err(DecodeError::Discriminator {
                    account: AccountKind::Window,
                    expected: 2,
                    actual,
                })
            );
        }
    }

    #[test]
    fn decode_errors_name_the_account_and_both_values() {
        assert_eq!(
            decode_state(&[]).unwrap_err().to_string(),
            "state account holds 0 bytes, expected 66"
        );
        let mut data = window_bytes();
        data[0] = 1;
        assert_eq!(
            decode_window(&data).unwrap_err().to_string(),
            "window account discriminator is 1, expected 2"
        );
    }

    #[test]
    fn window_slots_follow_the_grid_of_the_state() {
        assert_eq!(window_slots(&state(), 0), Ok((1_000, 1_149)));
        assert_eq!(window_slots(&state(), 3), Ok((1_450, 1_599)));
    }

    #[test]
    fn window_slots_reject_a_window_beyond_u64() {
        let late = State {
            genesis_slot: u64::MAX - 100,
            ..state()
        };

        assert_eq!(
            window_slots(&late, 0),
            Err(BoundsError {
                genesis_slot: u64::MAX - 100,
                window_len: 150,
                window_id: 0,
            })
        );
        assert_eq!(
            window_slots(&state(), u64::MAX),
            Err(BoundsError {
                genesis_slot: 1_000,
                window_len: 150,
                window_id: u64::MAX,
            })
        );
    }
}
