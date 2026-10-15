pub mod account;
pub mod instruction;
pub mod json;
pub mod merkle;
pub mod root;
pub mod transaction;

pub use account::{
    AccountKind, BoundsError, DecodeError, decode_proposal, decode_state, decode_window,
    window_slots,
};
pub use clarion::{
    ARM_AGGREGATES_SIZE, ARM_COUNT, Arm, ArmAggregates, AuthorityProposal, ClarionError,
    ClarionInstruction, PROPOSAL_DISCRIMINATOR, PROPOSAL_SEED, PROPOSAL_SIZE,
    STATE_DISCRIMINATOR, STATE_SEED, STATE_SIZE, State, WINDOW_DISCRIMINATOR,
    WINDOW_SEED, WINDOW_SIZE, Window, proposal_address, state_address, window_address,
    window_bounds,
};
pub use instruction::{
    accept_authority, cancel_authority_proposal, commit, init, propose_authority, reveal,
};
pub use json::{JsonError, parse_aggregates, render_authority, render_window};
pub use merkle::{
    DuplicateSignature, EMPTY_PREFIX, LEAF_PREFIX, NODE_PREFIX, RESULT_ROW_SIZE,
    ResultRow, empty_root, leaf_hash, leaf_hash_bytes, merkle_root,
    merkle_root_of_leaves, node_hash,
};
pub use root::{MERKLE_ROOT_HEX_LEN, RootError, merkle_root_hex, parse_merkle_root};
pub use transaction::authority_transaction;
