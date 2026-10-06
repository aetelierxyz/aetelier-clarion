pub mod account;
pub mod instruction;
pub mod json;
pub mod root;
pub mod transaction;

pub use account::{
    AccountKind, BoundsError, DecodeError, decode_state, decode_window, window_slots,
};
pub use clarion::{
    ARM_AGGREGATES_SIZE, ARM_COUNT, ArmAggregates, ClarionError, ClarionInstruction,
    STATE_DISCRIMINATOR, STATE_SEED, STATE_SIZE, State, WINDOW_DISCRIMINATOR,
    WINDOW_SEED, WINDOW_SIZE, Window, state_address, window_address, window_bounds,
};
pub use instruction::{commit, init, reveal};
pub use json::{JsonError, parse_aggregates, render_window};
pub use root::{MERKLE_ROOT_HEX_LEN, RootError, merkle_root_hex, parse_merkle_root};
pub use transaction::authority_transaction;
