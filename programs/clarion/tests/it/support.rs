use std::sync::LazyLock;

use clarion::{
    ARM_COUNT, ArmAggregates, ClarionError, ClarionInstruction, State, Window,
    state_address, window_address, window_bounds,
};
use solana_account::{Account, AccountSharedData};
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    rent::Rent,
};
use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext, processor};
use solana_signer::Signer;
use solana_system_interface::{
    instruction as system_instruction, program as system_program,
};
use solana_transaction::{Transaction, TransactionError};

pub static PROGRAM_ID: LazyLock<Pubkey> = LazyLock::new(|| program_keypair().pubkey());
pub const FOREIGN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([77; 32]);
pub const GENESIS_SLOT: u64 = 100;
pub const WINDOW_LEN: u64 = 10;
pub const MIN_REVEAL_LAG_SLOTS: u64 = 5;
pub const MERKLE_ROOT: [u8; 32] = [7; 32];
pub const SIGNER_LAMPORTS: u64 = 10_000_000_000;

pub struct Harness {
    pub context: ProgramTestContext,
    pub authority: Keypair,
    pub intruder: Keypair,
    pub program: Keypair,
}

impl Harness {
    pub async fn start() -> Self {
        let authority = Keypair::new_from_array([1; 32]);
        let intruder = Keypair::new_from_array([2; 32]);
        let program = program_keypair();
        let mut program_test = ProgramTest::new(
            "clarion",
            *PROGRAM_ID,
            processor!(clarion::process_instruction),
        );
        for signer in [&authority, &intruder] {
            program_test.add_account(signer.pubkey(), system_account(SIGNER_LAMPORTS));
        }
        let context = program_test.start_with_context().await;
        Self {
            context,
            authority,
            intruder,
            program,
        }
    }

    pub async fn start_initialized() -> Self {
        let mut harness = Self::start().await;
        harness
            .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
            .await
            .unwrap();
        harness
    }

    pub async fn start_committed() -> (Self, u64) {
        let mut harness = Self::start_initialized().await;
        let commit_slot = window_end(0) + 1;
        harness.warp_to(commit_slot);
        harness.commit(0).await.unwrap();
        (harness, commit_slot)
    }

    pub fn warp_to(&mut self, slot: u64) {
        self.context.warp_to_slot(slot).unwrap();
    }

    pub fn set_account(&mut self, address: &Pubkey, account: Account) {
        self.context
            .set_account(address, &AccountSharedData::from(account));
    }

    pub async fn send(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(), BanksClientError> {
        let blockhash = self.context.get_new_latest_blockhash().await.unwrap();
        let mut signing = vec![&self.context.payer];
        signing.extend_from_slice(signers);
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&self.context.payer.pubkey()),
            &signing,
            blockhash,
        );
        self.context
            .banks_client
            .process_transaction(transaction)
            .await
    }

    pub async fn send_as_authority(
        &mut self,
        instruction: Instruction,
    ) -> Result<(), BanksClientError> {
        let authority = self.authority.insecure_clone();
        let program = self.program.insecure_clone();
        let mut signers = vec![&authority];
        if instruction
            .accounts
            .iter()
            .any(|account| account.is_signer && account.pubkey == *PROGRAM_ID)
        {
            signers.push(&program);
        }
        self.send(instruction, &signers).await
    }

    pub async fn init(
        &mut self,
        genesis_slot: u64,
        window_len: u64,
        min_reveal_lag_slots: u64,
    ) -> Result<(), BanksClientError> {
        let instruction = init_instruction(
            &self.authority.pubkey(),
            genesis_slot,
            window_len,
            min_reveal_lag_slots,
        );
        self.send_as_authority(instruction).await
    }

    pub async fn commit(&mut self, window_id: u64) -> Result<(), BanksClientError> {
        let instruction = grid_commit_instruction(&self.authority.pubkey(), window_id);
        self.send_as_authority(instruction).await
    }

    pub async fn reveal(
        &mut self,
        window_id: u64,
        aggregates: [ArmAggregates; ARM_COUNT],
    ) -> Result<(), BanksClientError> {
        let instruction =
            reveal_instruction(&self.authority.pubkey(), window_id, aggregates);
        self.send_as_authority(instruction).await
    }

    pub async fn fund(&mut self, address: &Pubkey, lamports: u64) {
        let instruction =
            system_instruction::transfer(&self.context.payer.pubkey(), address, lamports);
        self.send(instruction, &[]).await.unwrap();
    }

    pub async fn rent(&mut self) -> Rent {
        self.context.banks_client.get_rent().await.unwrap()
    }

    pub async fn account(&mut self, address: &Pubkey) -> Option<Account> {
        self.context
            .banks_client
            .get_account(*address)
            .await
            .unwrap()
    }

    pub async fn state(&mut self) -> State {
        let account = self.account(&state_key()).await.unwrap();
        State::unpack(&account.data).unwrap()
    }

    pub async fn window(&mut self, window_id: u64) -> Window {
        let account = self.account(&window_key(window_id)).await.unwrap();
        Window::unpack(&account.data).unwrap()
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

pub fn window_start(window_id: u64) -> u64 {
    window_bounds(GENESIS_SLOT, WINDOW_LEN, window_id)
        .unwrap()
        .0
}

pub fn window_end(window_id: u64) -> u64 {
    window_bounds(GENESIS_SLOT, WINDOW_LEN, window_id)
        .unwrap()
        .1
}

pub fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        owner: system_program::ID,
        ..Account::default()
    }
}

pub fn data_account(owner: &Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner: *owner,
        ..Account::default()
    }
}

fn instruction(payload: &ClarionInstruction, accounts: Vec<AccountMeta>) -> Instruction {
    Instruction {
        program_id: *PROGRAM_ID,
        accounts,
        data: borsh::to_vec(payload).unwrap(),
    }
}

fn init_accounts(authority: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*authority, true),
        AccountMeta::new(state_key(), false),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(*PROGRAM_ID, true),
    ]
}

fn commit_accounts(authority: &Pubkey, window_id: u64) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*authority, true),
        AccountMeta::new(state_key(), false),
        AccountMeta::new(window_key(window_id), false),
        AccountMeta::new_readonly(system_program::ID, false),
    ]
}

fn reveal_accounts(authority: &Pubkey, window_id: u64) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(*authority, true),
        AccountMeta::new_readonly(state_key(), false),
        AccountMeta::new(window_key(window_id), false),
    ]
}

pub fn init_instruction(
    authority: &Pubkey,
    genesis_slot: u64,
    window_len: u64,
    min_reveal_lag_slots: u64,
) -> Instruction {
    instruction(
        &ClarionInstruction::Init {
            genesis_slot,
            window_len,
            min_reveal_lag_slots,
        },
        init_accounts(authority),
    )
}

pub fn commit_instruction(
    authority: &Pubkey,
    window_id: u64,
    slot_start: u64,
    slot_end: u64,
) -> Instruction {
    instruction(
        &ClarionInstruction::Commit {
            window_id,
            slot_start,
            slot_end,
            merkle_root: MERKLE_ROOT,
        },
        commit_accounts(authority, window_id),
    )
}

pub fn grid_commit_instruction(authority: &Pubkey, window_id: u64) -> Instruction {
    commit_instruction(
        authority,
        window_id,
        window_start(window_id),
        window_end(window_id),
    )
}

pub fn reveal_instruction(
    authority: &Pubkey,
    window_id: u64,
    aggregates: [ArmAggregates; ARM_COUNT],
) -> Instruction {
    instruction(
        &ClarionInstruction::Reveal {
            window_id,
            aggregates,
        },
        reveal_accounts(authority, window_id),
    )
}

pub fn aggregates() -> [ArmAggregates; ARM_COUNT] {
    [arm(1), arm(2), arm(3), arm(4)]
}

pub fn arm(seed: u32) -> ArmAggregates {
    ArmAggregates {
        tx_submitted: 100 * seed,
        tx_landed: 90 * seed,
        stl_p50_slots: 1,
        stl_p90_slots: 4,
        cu_price_p50_micro: 1_000 * u64::from(seed),
        cu_price_p90_micro: 9_000 * u64::from(seed),
        fee_total_lamports: 500_000 * u64::from(seed),
        tip_total_lamports: 250_000 * u64::from(seed),
    }
}

pub fn assert_rejected(result: Result<(), BanksClientError>, expected: ClarionError) {
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(expected.code()))
    );
}
