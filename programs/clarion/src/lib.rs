pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;

pub use error::ClarionError;
pub use instruction::ClarionInstruction;
pub use processor::process_instruction;
pub use state::{
    ARM_AGGREGATES_SIZE, ARM_COUNT, ArmAggregates, STATE_DISCRIMINATOR, STATE_SEED,
    STATE_SIZE, State, WINDOW_DISCRIMINATOR, WINDOW_SEED, WINDOW_SIZE, Window,
    state_address, window_address, window_bounds,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);
