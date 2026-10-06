use clarion::{ClarionError, STATE_DISCRIMINATOR, STATE_SIZE, State, state_address};
use solana_program_test::tokio;
use solana_signer::Signer;
use solana_system_interface::program as system_program;

use crate::support::{
    FOREIGN_PROGRAM_ID, GENESIS_SLOT, Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID,
    WINDOW_LEN, assert_rejected, data_account, init_instruction, state_key,
};

#[tokio::test]
async fn init_once() {
    let mut harness = Harness::start().await;
    let (state_key, bump) = state_address(&PROGRAM_ID);
    let initialized = State {
        discriminator: STATE_DISCRIMINATOR,
        authority: harness.authority.pubkey(),
        genesis_slot: GENESIS_SLOT,
        window_len: WINDOW_LEN,
        min_reveal_lag_slots: MIN_REVEAL_LAG_SLOTS,
        next_window_id: 0,
        bump,
    };

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();

    let account = harness.account(&state_key).await.unwrap();
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), STATE_SIZE);
    assert_eq!(
        account.lamports,
        harness.rent().await.minimum_balance(STATE_SIZE)
    );
    assert_eq!(State::unpack(&account.data), Ok(initialized));

    assert_rejected(
        harness
            .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
            .await,
        ClarionError::StateAlreadyInitialized,
    );
    let intruder = harness.intruder.insecure_clone();
    let program = harness.program.insecure_clone();
    let takeover = init_instruction(&intruder.pubkey(), 1, 2, 3);
    assert_rejected(
        harness.send(takeover, &[&intruder, &program]).await,
        ClarionError::StateAlreadyInitialized,
    );
    assert_eq!(harness.state().await, initialized);
}

#[tokio::test]
async fn init_requires_signer() {
    let mut harness = Harness::start().await;
    let rent = harness.rent().await;
    harness
        .fund(&state_key(), rent.minimum_balance(STATE_SIZE))
        .await;
    let program = harness.program.insecure_clone();
    let mut unsigned = init_instruction(
        &harness.authority.pubkey(),
        GENESIS_SLOT,
        WINDOW_LEN,
        MIN_REVEAL_LAG_SLOTS,
    );
    unsigned.accounts[0].is_signer = false;

    assert_rejected(
        harness.send(unsigned, &[&program]).await,
        ClarionError::AuthoritySignatureMissing,
    );
    let account = harness.account(&state_key()).await.unwrap();
    assert_eq!(account.owner, system_program::ID);
    assert!(account.data.is_empty());
}

#[tokio::test]
async fn init_requires_program_signature() {
    let mut harness = Harness::start().await;
    let prefunded = harness.rent().await.minimum_balance(STATE_SIZE);
    harness.fund(&state_key(), prefunded).await;
    let authority = harness.authority.insecure_clone();
    let intruder = harness.intruder.insecure_clone();
    let init = init_instruction(
        &authority.pubkey(),
        GENESIS_SLOT,
        WINDOW_LEN,
        MIN_REVEAL_LAG_SLOTS,
    );
    let mut unsigned = init.clone();
    unsigned.accounts[3].is_signer = false;
    let mut substituted = init;
    substituted.accounts[3].pubkey = intruder.pubkey();

    for (attempt, signers) in [
        (unsigned, vec![&authority]),
        (substituted, vec![&authority, &intruder]),
    ] {
        assert_rejected(
            harness.send(attempt, &signers).await,
            ClarionError::ProgramSignatureMissing,
        );
        let account = harness.account(&state_key()).await.unwrap();
        assert_eq!(account.owner, system_program::ID);
        assert!(account.data.is_empty());
        assert_eq!(account.lamports, prefunded);
    }
}

#[tokio::test]
async fn init_rejects_zero_params() {
    let mut harness = Harness::start().await;

    assert_rejected(
        harness.init(GENESIS_SLOT, 0, MIN_REVEAL_LAG_SLOTS).await,
        ClarionError::WindowLenZero,
    );
    assert_rejected(
        harness.init(GENESIS_SLOT, WINDOW_LEN, 0).await,
        ClarionError::MinRevealLagZero,
    );
    assert_rejected(
        harness.init(GENESIS_SLOT, 0, 0).await,
        ClarionError::WindowLenZero,
    );
    assert_eq!(harness.account(&state_key()).await, None);

    harness.init(0, 1, 1).await.unwrap();
    let state = harness.state().await;
    assert_eq!(
        (
            state.genesis_slot,
            state.window_len,
            state.min_reveal_lag_slots
        ),
        (0, 1, 1)
    );
}

#[tokio::test]
async fn init_rejects_bounds_overflowing_u64() {
    let mut harness = Harness::start().await;

    for (genesis_slot, window_len, min_reveal_lag_slots) in [
        (u64::MAX, 1, 1),
        (u64::MAX - 5, 10, 1),
        (0, u64::MAX, 1),
        (GENESIS_SLOT, WINDOW_LEN, u64::MAX),
        (u64::MAX - 1, 1, 1),
        (0, u64::MAX - 1, 2),
    ] {
        assert_rejected(
            harness
                .init(genesis_slot, window_len, min_reveal_lag_slots)
                .await,
            ClarionError::WindowBoundsOverflow,
        );
    }
    assert_eq!(harness.account(&state_key()).await, None);

    harness.init(u64::MAX - 2, 1, 1).await.unwrap();
    assert_eq!(harness.state().await.genesis_slot, u64::MAX - 2);

    let mut harness = Harness::start().await;
    harness.init(0, u64::MAX - 1, 1).await.unwrap();
    assert_eq!(harness.state().await.window_len, u64::MAX - 1);
}

#[tokio::test]
async fn init_rejects_an_occupied_state_address() {
    let mut harness = Harness::start().await;

    for occupied in [
        data_account(&system_program::ID, vec![0; STATE_SIZE]),
        data_account(&FOREIGN_PROGRAM_ID, Vec::new()),
    ] {
        harness.set_account(&state_key(), occupied.clone());
        assert_rejected(
            harness
                .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
                .await,
            ClarionError::StateAlreadyInitialized,
        );
        assert_eq!(harness.account(&state_key()).await, Some(occupied));
    }
}

#[tokio::test]
async fn init_succeeds_on_prefunded_state_address() {
    let mut harness = Harness::start().await;
    let rent = harness.rent().await;
    let prefunded = rent.minimum_balance(0);
    harness.fund(&state_key(), prefunded).await;

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();

    let account = harness.account(&state_key()).await.unwrap();
    assert!(prefunded < rent.minimum_balance(STATE_SIZE));
    assert_eq!(account.lamports, rent.minimum_balance(STATE_SIZE));
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(harness.state().await.authority, harness.authority.pubkey());
}

#[tokio::test]
async fn init_tops_up_a_state_address_one_lamport_short() {
    let mut harness = Harness::start().await;
    let rent_exempt_minimum = harness.rent().await.minimum_balance(STATE_SIZE);
    harness.fund(&state_key(), rent_exempt_minimum - 1).await;

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();

    let account = harness.account(&state_key()).await.unwrap();
    assert_eq!(account.lamports, rent_exempt_minimum);
}
