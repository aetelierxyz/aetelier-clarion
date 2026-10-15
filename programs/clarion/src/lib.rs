pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;

pub use error::ClarionError;
pub use instruction::ClarionInstruction;
pub use processor::process_instruction;
pub use state::{
    ARM_AGGREGATES_SIZE, ARM_COUNT, Arm, ArmAggregates, AuthorityProposal,
    MAX_GENESIS_LEAD_SLOTS, PROPOSAL_DISCRIMINATOR, PROPOSAL_SEED, PROPOSAL_SIZE,
    STATE_DISCRIMINATOR, STATE_SEED, STATE_SIZE, State, WINDOW_DISCRIMINATOR,
    WINDOW_SEED, WINDOW_SIZE, Window, proposal_address, state_address, window_address,
    window_bounds,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "clarion",
    project_url: "https://github.com/aetelierxyz/aetelier-clarion",
    contacts: "link:https://github.com/aetelierxyz/aetelier-clarion/security/advisories/new",
    policy: "Report vulnerabilities privately through GitHub security advisories on aetelierxyz/aetelier-clarion. No bug bounty.",
    preferred_languages: "en",
    source_code: "https://github.com/aetelierxyz/aetelier-clarion"
}
