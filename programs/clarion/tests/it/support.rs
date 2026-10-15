use std::sync::{LazyLock, Once};

use base64::{Engine, prelude::BASE64_STANDARD};
use borsh::BorshDeserialize;
use clarion::{
    ARM_COUNT, Arm, ArmAggregates, AuthorityProposal, ClarionError, ClarionEvent,
    ClarionInstruction, State, Window, proposal_address, state_address, window_address,
    window_bounds,
};
use solana_account::{Account, AccountSharedData};
use solana_keypair::Keypair;
use solana_program::{
    account_info::AccountInfo,
    bpf_loader,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction, InstructionError},
    program_stubs::{self, SyscallStubs},
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
pub const DATA_LOG_PREFIX: &str = "Program data: ";
pub const NATIVE_DATA_LOG_PREFIX: &str = "Program log: Program data: ";

pub struct Sent {
    pub result: Result<(), TransactionError>,
    pub logs: Vec<String>,
}

pub struct Harness {
    pub context: ProgramTestContext,
    pub authority: Keypair,
    pub intruder: Keypair,
    pub successor: Keypair,
    pub program: Keypair,
}

impl Harness {
    pub async fn start() -> Self {
        let authority = Keypair::new_from_array([1; 32]);
        let intruder = Keypair::new_from_array([2; 32]);
        let successor = Keypair::new_from_array([3; 32]);
        let program = program_keypair();
        let mut program_test = ProgramTest::new(
            "clarion",
            *PROGRAM_ID,
            processor!(clarion::process_instruction),
        );
        for signer in [&authority, &intruder, &successor] {
            program_test.add_account(signer.pubkey(), system_account(SIGNER_LAMPORTS));
        }
        let context = program_test.start_with_context().await;
        route_native_log_data_into_transaction_logs();
        Self {
            context,
            authority,
            intruder,
            successor,
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

    pub async fn clock_slot(&mut self) -> u64 {
        let clock: Clock = self.context.banks_client.get_sysvar().await.unwrap();
        clock.slot
    }

    pub async fn reveal_after_lag(&mut self, window_id: u64) -> u64 {
        let earliest = self.window(window_id).await.commit_slot + MIN_REVEAL_LAG_SLOTS;
        let reveal_slot = earliest.max(self.clock_slot().await + 1);
        self.warp_to(reveal_slot);
        self.reveal(window_id, aggregates()).await.unwrap();
        reveal_slot
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
        self.send_all(&[instruction], signers).await
    }

    pub async fn send_all(
        &mut self,
        instructions: &[Instruction],
        signers: &[&Keypair],
    ) -> Result<(), BanksClientError> {
        let blockhash = self.context.get_new_latest_blockhash().await.unwrap();
        let mut signing = vec![&self.context.payer];
        signing.extend_from_slice(signers);
        let transaction = Transaction::new_signed_with_payer(
            instructions,
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
        let signers = self.authority_signers(&instruction);
        let signers: Vec<&Keypair> = signers.iter().collect();
        self.send(instruction, &signers).await
    }

    pub async fn send_logged(
        &mut self,
        instructions: &[Instruction],
        signers: &[&Keypair],
    ) -> Sent {
        let blockhash = self.context.get_new_latest_blockhash().await.unwrap();
        let mut signing = vec![&self.context.payer];
        signing.extend_from_slice(signers);
        let transaction = Transaction::new_signed_with_payer(
            instructions,
            Some(&self.context.payer.pubkey()),
            &signing,
            blockhash,
        );
        let outcome = self
            .context
            .banks_client
            .process_transaction_with_metadata(transaction)
            .await
            .unwrap();
        Sent {
            result: outcome.result,
            logs: outcome.metadata.unwrap().log_messages,
        }
    }

    pub async fn send_logged_as_authority(&mut self, instruction: Instruction) -> Sent {
        let signers = self.authority_signers(&instruction);
        let signers: Vec<&Keypair> = signers.iter().collect();
        self.send_logged(&[instruction], &signers).await
    }

    fn authority_signers(&self, instruction: &Instruction) -> Vec<Keypair> {
        let mut signers = vec![self.authority.insecure_clone()];
        if instruction
            .accounts
            .iter()
            .any(|account| account.is_signer && account.pubkey == *PROGRAM_ID)
        {
            signers.push(self.program.insecure_clone());
        }
        signers
    }

    pub async fn runs_sbf(&mut self) -> bool {
        self.account(&PROGRAM_ID).await.unwrap().owner == bpf_loader::id()
    }

    pub async fn events(&mut self, logs: &[String]) -> Vec<ClarionEvent> {
        let prefix = if self.runs_sbf().await {
            DATA_LOG_PREFIX
        } else {
            NATIVE_DATA_LOG_PREFIX
        };
        let program = PROGRAM_ID.to_string();
        let consumed = format!("Program {program} consumed ");
        let success = format!("Program {program} success");
        logs.iter()
            .enumerate()
            .filter(|(_, line)| line.contains(DATA_LOG_PREFIX))
            .map(|(index, line)| {
                let closing = logs[index + 1..]
                    .iter()
                    .find(|next| !next.starts_with(&consumed));
                assert_eq!(closing, Some(&success), "{line} is not the last frame line");
                let payload = line
                    .strip_prefix(prefix)
                    .unwrap_or_else(|| panic!("{line} does not start with {prefix}"));
                let bytes = BASE64_STANDARD.decode(payload).unwrap();
                ClarionEvent::try_from_slice(&bytes).unwrap()
            })
            .collect()
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

    pub async fn propose(
        &mut self,
        new_authority: &Pubkey,
    ) -> Result<(), BanksClientError> {
        let instruction = propose_instruction(&self.authority.pubkey(), new_authority);
        self.send_as_authority(instruction).await
    }

    pub async fn accept(
        &mut self,
        new_authority: &Keypair,
    ) -> Result<(), BanksClientError> {
        let instruction = accept_instruction(&new_authority.pubkey());
        self.send(instruction, &[new_authority]).await
    }

    pub async fn cancel(&mut self) -> Result<(), BanksClientError> {
        let instruction = cancel_instruction(&self.authority.pubkey());
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

    pub async fn proposal(&mut self) -> AuthorityProposal {
        let account = self.account(&proposal_key()).await.unwrap();
        AuthorityProposal::unpack(&account.data).unwrap()
    }

    pub async fn lamports(&mut self, address: &Pubkey) -> u64 {
        self.account(address)
            .await
            .map_or(0, |account| account.lamports)
    }
}

fn route_native_log_data_into_transaction_logs() {
    static ROUTED: Once = Once::new();
    ROUTED.call_once(|| {
        let inner = program_stubs::set_syscall_stubs(Box::new(PlaceholderStubs));
        program_stubs::set_syscall_stubs(Box::new(DataLoggingStubs { inner }));
    });
}

struct PlaceholderStubs;

impl SyscallStubs for PlaceholderStubs {}

struct DataLoggingStubs {
    inner: Box<dyn SyscallStubs>,
}

impl SyscallStubs for DataLoggingStubs {
    fn sol_log(&self, message: &str) {
        self.inner.sol_log(message);
    }

    fn sol_log_compute_units(&self) {
        self.inner.sol_log_compute_units();
    }

    fn sol_remaining_compute_units(&self) -> u64 {
        self.inner.sol_remaining_compute_units()
    }

    fn sol_invoke_signed(
        &self,
        instruction: &Instruction,
        account_infos: &[AccountInfo],
        signers_seeds: &[&[&[u8]]],
    ) -> ProgramResult {
        self.inner
            .sol_invoke_signed(instruction, account_infos, signers_seeds)
    }

    fn sol_get_sysvar(
        &self,
        sysvar_id_addr: *const u8,
        var_addr: *mut u8,
        offset: u64,
        length: u64,
    ) -> u64 {
        self.inner
            .sol_get_sysvar(sysvar_id_addr, var_addr, offset, length)
    }

    fn sol_get_clock_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_clock_sysvar(var_addr)
    }

    fn sol_get_epoch_schedule_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_epoch_schedule_sysvar(var_addr)
    }

    fn sol_get_fees_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_fees_sysvar(var_addr)
    }

    fn sol_get_rent_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_rent_sysvar(var_addr)
    }

    fn sol_get_epoch_rewards_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_epoch_rewards_sysvar(var_addr)
    }

    fn sol_get_last_restart_slot(&self, var_addr: *mut u8) -> u64 {
        self.inner.sol_get_last_restart_slot(var_addr)
    }

    fn sol_get_epoch_stake(&self, vote_address: *const u8) -> u64 {
        self.inner.sol_get_epoch_stake(vote_address)
    }

    unsafe fn sol_memcpy(&self, dst: *mut u8, src: *const u8, n: usize) {
        unsafe { self.inner.sol_memcpy(dst, src, n) }
    }

    unsafe fn sol_memmove(&self, dst: *mut u8, src: *const u8, n: usize) {
        unsafe { self.inner.sol_memmove(dst, src, n) }
    }

    unsafe fn sol_memcmp(
        &self,
        s1: *const u8,
        s2: *const u8,
        n: usize,
        result: *mut i32,
    ) {
        unsafe { self.inner.sol_memcmp(s1, s2, n, result) }
    }

    unsafe fn sol_memset(&self, s: *mut u8, c: u8, n: usize) {
        unsafe { self.inner.sol_memset(s, c, n) }
    }

    fn sol_get_return_data(&self) -> Option<(Pubkey, Vec<u8>)> {
        self.inner.sol_get_return_data()
    }

    fn sol_set_return_data(&self, data: &[u8]) {
        self.inner.sol_set_return_data(data);
    }

    fn sol_log_data(&self, fields: &[&[u8]]) {
        let encoded: Vec<String> = fields
            .iter()
            .map(|field| BASE64_STANDARD.encode(field))
            .collect();
        self.inner
            .sol_log(&format!("{DATA_LOG_PREFIX}{}", encoded.join(" ")));
    }

    fn sol_get_processed_sibling_instruction(&self, index: usize) -> Option<Instruction> {
        self.inner.sol_get_processed_sibling_instruction(index)
    }

    fn sol_get_stack_height(&self) -> u64 {
        self.inner.sol_get_stack_height()
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

pub fn proposal_key() -> Pubkey {
    proposal_address(&PROGRAM_ID).0
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
    let mut accounts = vec![
        AccountMeta::new(*authority, true),
        AccountMeta::new(state_key(), false),
        AccountMeta::new(window_key(window_id), false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    if let Some(previous_window_id) = window_id.checked_sub(1) {
        accounts.push(AccountMeta::new_readonly(
            window_key(previous_window_id),
            false,
        ));
    }
    accounts
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

pub fn propose_instruction(authority: &Pubkey, new_authority: &Pubkey) -> Instruction {
    instruction(
        &ClarionInstruction::ProposeAuthority {
            new_authority: *new_authority,
        },
        vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new_readonly(state_key(), false),
            AccountMeta::new(proposal_key(), false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    )
}

pub fn accept_instruction(new_authority: &Pubkey) -> Instruction {
    instruction(
        &ClarionInstruction::AcceptAuthority,
        vec![
            AccountMeta::new(*new_authority, true),
            AccountMeta::new(state_key(), false),
            AccountMeta::new(proposal_key(), false),
        ],
    )
}

pub fn cancel_instruction(authority: &Pubkey) -> Instruction {
    instruction(
        &ClarionInstruction::CancelAuthorityProposal,
        vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new_readonly(state_key(), false),
            AccountMeta::new(proposal_key(), false),
        ],
    )
}

pub fn aggregates() -> [ArmAggregates; ARM_COUNT] {
    [
        arm_aggregates(Arm::FanOut, 1),
        arm_aggregates(Arm::TipLane, 2),
        arm_aggregates(Arm::RpcDefault, 3),
        arm_aggregates(Arm::FixedPriorityFee, 4),
    ]
}

pub fn uniform_aggregates(seed: u32) -> [ArmAggregates; ARM_COUNT] {
    Arm::ALL.map(|arm| arm_aggregates(arm, seed))
}

pub fn arm_aggregates(arm: Arm, seed: u32) -> ArmAggregates {
    ArmAggregates {
        arm,
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
