use clarion_program_client::{
    AccountKind, ClarionError, DecodeError, STATE_DISCRIMINATOR, STATE_SIZE, State,
    WINDOW_SIZE, decode_state, decode_window, init, state_address,
};
use solana_keypair::Signer;
use solana_program::instruction::AccountMeta;
use solana_program_test::tokio;

use crate::support::{
    GENESIS_SLOT, Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, WINDOW_LEN, assert_rejected,
    state_key,
};

#[tokio::test]
async fn init_creates_a_state_that_decodes_to_the_submitted_parameters() {
    let mut harness = Harness::start().await;
    let authority = harness.authority.pubkey();

    harness
        .send(init(&PROGRAM_ID, &authority, 1_000, 150, 151))
        .await
        .unwrap();

    let (state_key, bump) = state_address(&PROGRAM_ID);
    let account = harness.account(&state_key).await;
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), STATE_SIZE);
    assert_eq!(
        decode_state(&account.data),
        Ok(State {
            discriminator: STATE_DISCRIMINATOR,
            authority,
            genesis_slot: 1_000,
            window_len: 150,
            min_reveal_lag_slots: 151,
            next_window_id: 0,
            bump,
        })
    );
}

#[tokio::test]
async fn init_is_paid_by_the_authority_and_cosigned_by_the_program_keypair() {
    let mut harness = Harness::start().await;
    let authority = harness.authority.pubkey();
    let instruction = init(&PROGRAM_ID, &authority, 1_000, 150, 151);
    assert_eq!(
        instruction.accounts.last(),
        Some(&AccountMeta::new_readonly(*PROGRAM_ID, true))
    );
    let payer = harness.context.payer.pubkey();
    let payer_lamports = harness.account(&payer).await.lamports;
    let authority_lamports = harness.account(&authority).await.lamports;

    harness.send(instruction).await.unwrap();

    let state_lamports = harness.account(&state_key()).await.lamports;
    assert_eq!(harness.account(&payer).await.lamports, payer_lamports);
    assert!(
        harness.account(&authority).await.lamports < authority_lamports - state_lamports
    );
}

#[tokio::test]
async fn init_whose_program_account_does_not_sign_is_rejected() {
    let mut harness = Harness::start().await;
    let authority = harness.authority.pubkey();
    let mut instruction = init(&PROGRAM_ID, &authority, 1_000, 150, 151);
    for meta in instruction
        .accounts
        .iter_mut()
        .filter(|meta| meta.pubkey == *PROGRAM_ID)
    {
        meta.is_signer = false;
    }

    assert_rejected(
        harness.send(instruction).await,
        ClarionError::ProgramSignatureMissing,
    );

    assert!(!harness.exists(&state_key()).await);
}

#[tokio::test]
async fn second_init_is_rejected_and_leaves_the_state_unchanged() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let initialized = harness.state().await;

    assert_rejected(
        harness.send(init(&PROGRAM_ID, &authority, 7, 8, 9)).await,
        ClarionError::StateAlreadyInitialized,
    );

    assert_eq!(harness.state().await, initialized);
    assert_eq!(initialized.genesis_slot, GENESIS_SLOT);
    assert_eq!(initialized.window_len, WINDOW_LEN);
    assert_eq!(initialized.min_reveal_lag_slots, MIN_REVEAL_LAG_SLOTS);
}

#[tokio::test]
async fn on_chain_state_does_not_decode_as_a_window() {
    let mut harness = Harness::start_initialized().await;

    let account = harness.account(&state_key()).await;

    assert_eq!(
        decode_window(&account.data),
        Err(DecodeError::Length {
            account: AccountKind::Window,
            expected: WINDOW_SIZE,
            actual: STATE_SIZE,
        })
    );
}
