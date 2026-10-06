use std::sync::LazyLock;

use clarion_program_client::{
    ARM_COUNT, ArmAggregates, ClarionError, State, Window, authority_transaction, commit,
    decode_state, decode_window, init, parse_aggregates, parse_merkle_root, reveal,
    state_address, window_address, window_slots,
};
use solana_account::Account;
use solana_keypair::{Keypair, Signer};
use solana_program::{
    instruction::{Instruction, InstructionError},
    pubkey::Pubkey,
};
use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext, processor};
use solana_sdk_ids::system_program;
use solana_transaction::TransactionError;

pub static PROGRAM_ID: LazyLock<Pubkey> = LazyLock::new(|| program_keypair().pubkey());
pub const GENESIS_SLOT: u64 = 100;
pub const WINDOW_LEN: u64 = 10;
pub const MIN_REVEAL_LAG_SLOTS: u64 = 5;
pub const AUTHORITY_LAMPORTS: u64 = 10_000_000_000;
pub const MERKLE_ROOT_HEX: &str =
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
pub const AGGREGATES_JSON: &str = r#"[
    {"tx_submitted": 100, "tx_landed": 91, "stl_p50_slots": 1, "stl_p90_slots": 2,
     "cu_price_p50_micro": 1000, "cu_price_p90_micro": 9000,
     "fee_total_lamports": 500000, "tip_total_lamports": 0},
    {"tx_submitted": 200, "tx_landed": 192, "stl_p50_slots": 2, "stl_p90_slots": 4,
     "cu_price_p50_micro": 2000, "cu_price_p90_micro": 18000,
     "fee_total_lamports": 1000000, "tip_total_lamports": 250000},
    {"tx_submitted": 300, "tx_landed": 293, "stl_p50_slots": 3, "stl_p90_slots": 6,
     "cu_price_p50_micro": 0, "cu_price_p90_micro": 0,
     "fee_total_lamports": 1500000, "tip_total_lamports": 0},
    {"tx_submitted": 400, "tx_landed": 400, "stl_p50_slots": 4, "stl_p90_slots": 8,
     "cu_price_p50_micro": 4000, "cu_price_p90_micro": 36000,
     "fee_total_lamports": 2000000, "tip_total_lamports": 0}
]"#;

pub struct Harness {
    pub context: ProgramTestContext,
    pub authority: Keypair,
    pub program: Keypair,
}

impl Harness {
    pub async fn start() -> Self {
        let authority = Keypair::new_from_array([1; 32]);
        let mut program_test = ProgramTest::new(
            "clarion",
            *PROGRAM_ID,
            processor!(clarion::process_instruction),
        );
        program_test.add_account(
            authority.pubkey(),
            Account {
                lamports: AUTHORITY_LAMPORTS,
                owner: system_program::ID,
                ..Account::default()
            },
        );
        let context = program_test.start_with_context().await;
        Self {
            context,
            authority,
            program: program_keypair(),
        }
    }

    pub async fn start_initialized() -> Self {
        let mut harness = Self::start().await;
        let instruction = init(
            &PROGRAM_ID,
            &harness.authority.pubkey(),
            GENESIS_SLOT,
            WINDOW_LEN,
            MIN_REVEAL_LAG_SLOTS,
        );
        harness.send(instruction).await.unwrap();
        harness
    }

    pub async fn start_committed() -> (Self, u64) {
        let mut harness = Self::start_initialized().await;
        let commit_slot = harness.commit_once_closed(0).await;
        (harness, commit_slot)
    }

    pub async fn commit_once_closed(&mut self, window_id: u64) -> u64 {
        let state = self.state().await;
        let (slot_start, slot_end) = window_slots(&state, window_id).unwrap();
        let commit_slot = slot_end + 1;
        self.warp_to(commit_slot);
        let instruction = commit(
            &PROGRAM_ID,
            &self.authority.pubkey(),
            window_id,
            slot_start,
            slot_end,
            merkle_root(),
        );
        self.send(instruction).await.unwrap();
        commit_slot
    }

    pub async fn reveal(
        &mut self,
        window_id: u64,
        aggregates: [ArmAggregates; ARM_COUNT],
    ) -> Result<(), BanksClientError> {
        let instruction =
            reveal(&PROGRAM_ID, &self.authority.pubkey(), window_id, aggregates);
        self.send(instruction).await
    }

    pub fn warp_to(&mut self, slot: u64) {
        self.context.warp_to_slot(slot).unwrap();
    }

    pub async fn send(
        &mut self,
        instruction: Instruction,
    ) -> Result<(), BanksClientError> {
        let blockhash = self.context.get_new_latest_blockhash().await.unwrap();
        let cosigners: Vec<&Keypair> = instruction
            .accounts
            .iter()
            .any(|meta| meta.is_signer && meta.pubkey == *PROGRAM_ID)
            .then_some(&self.program)
            .into_iter()
            .collect();
        let transaction =
            authority_transaction(instruction, &self.authority, &cosigners, blockhash)
                .unwrap();
        self.context
            .banks_client
            .process_transaction(transaction)
            .await
    }

    pub async fn exists(&mut self, address: &Pubkey) -> bool {
        self.context
            .banks_client
            .get_account(*address)
            .await
            .unwrap()
            .is_some()
    }

    pub async fn account(&mut self, address: &Pubkey) -> Account {
        self.context
            .banks_client
            .get_account(*address)
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn state(&mut self) -> State {
        let account = self.account(&state_key()).await;
        decode_state(&account.data).unwrap()
    }

    pub async fn window(&mut self, window_id: u64) -> Window {
        let account = self.account(&window_key(window_id)).await;
        decode_window(&account.data).unwrap()
    }
}

fn program_keypair() -> Keypair {
    Keypair::new_from_array([42; 32])
}

pub fn state_key() -> Pubkey {
    state_address(&PROGRAM_ID).0
}

pub fn window_key(window_id: u64) -> Pubkey {
    window_address(&PROGRAM_ID, window_id).0
}

pub fn merkle_root() -> [u8; 32] {
    parse_merkle_root(MERKLE_ROOT_HEX).unwrap()
}

pub fn aggregates() -> [ArmAggregates; ARM_COUNT] {
    parse_aggregates(AGGREGATES_JSON).unwrap()
}

pub fn assert_rejected(result: Result<(), BanksClientError>, expected: ClarionError) {
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(expected.code()))
    );
}
