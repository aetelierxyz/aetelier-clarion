use clarion::{
    ARM_COUNT, ArmAggregates, ClarionError, STATE_DISCRIMINATOR, State,
    WINDOW_DISCRIMINATOR, WINDOW_SIZE, Window, state_address, window_address,
};
use solana_program_test::tokio;
use solana_signer::Signer;
use solana_system_interface::program as system_program;

use crate::support::{
    FOREIGN_PROGRAM_ID, GENESIS_SLOT, Harness, MERKLE_ROOT, MIN_REVEAL_LAG_SLOTS,
    PROGRAM_ID, WINDOW_LEN, assert_rejected, commit_instruction, data_account,
    grid_commit_instruction, state_key, window_end, window_key, window_start,
};

#[tokio::test]
async fn commit_requires_authority() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(0) + 1);
    let authority = harness.authority.pubkey();
    let intruder = harness.intruder.insecure_clone();

    let by_intruder = grid_commit_instruction(&intruder.pubkey(), 0);
    assert_rejected(
        harness.send(by_intruder, &[&intruder]).await,
        ClarionError::AuthorityMismatch,
    );

    let mut unsigned = grid_commit_instruction(&authority, 0);
    unsigned.accounts[0].is_signer = false;
    assert_rejected(
        harness.send(unsigned, &[]).await,
        ClarionError::AuthoritySignatureMissing,
    );

    assert_eq!(harness.state().await.next_window_id, 0);
    assert_eq!(harness.account(&window_key(0)).await, None);
}

#[tokio::test]
async fn commit_sequential() {
    let mut harness = Harness::start_initialized().await;
    let commit_slot = window_end(1) + 1;
    harness.warp_to(commit_slot);

    assert_rejected(harness.commit(1).await, ClarionError::WindowIdNotSequential);
    assert_eq!(harness.state().await.next_window_id, 0);

    harness.commit(0).await.unwrap();

    let (window_key, bump) = window_address(&PROGRAM_ID, 0);
    let account = harness.account(&window_key).await.unwrap();
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), WINDOW_SIZE);
    assert_eq!(
        account.lamports,
        harness.rent().await.minimum_balance(WINDOW_SIZE)
    );
    assert_eq!(
        Window::unpack(&account.data),
        Ok(Window {
            discriminator: WINDOW_DISCRIMINATOR,
            window_id: 0,
            slot_start: GENESIS_SLOT,
            slot_end: GENESIS_SLOT + WINDOW_LEN - 1,
            merkle_root: MERKLE_ROOT,
            commit_slot,
            reveal_slot: 0,
            aggregates: [ArmAggregates::default(); ARM_COUNT],
            bump,
        })
    );
    assert_eq!(harness.state().await.next_window_id, 1);

    assert_rejected(harness.commit(0).await, ClarionError::WindowIdNotSequential);
    assert_rejected(harness.commit(2).await, ClarionError::WindowIdNotSequential);

    harness.commit(1).await.unwrap();

    let second = harness.window(1).await;
    assert_eq!(
        (second.window_id, second.slot_start, second.slot_end),
        (
            1,
            GENESIS_SLOT + WINDOW_LEN,
            GENESIS_SLOT + 2 * WINDOW_LEN - 1
        )
    );
    assert_eq!(harness.state().await.next_window_id, 2);
}

#[tokio::test]
async fn commit_grid_mismatch() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(1) + 1);
    let authority = harness.authority.pubkey();
    let (start, end) = (window_start(0), window_end(0));

    for (slot_start, slot_end, expected) in [
        (start + 1, end, ClarionError::SlotStartOffGrid),
        (start - 1, end, ClarionError::SlotStartOffGrid),
        (start, end + 1, ClarionError::SlotEndOffGrid),
        (start, end - 1, ClarionError::SlotEndOffGrid),
        (
            window_start(1),
            window_end(1),
            ClarionError::SlotStartOffGrid,
        ),
    ] {
        let off_grid = commit_instruction(&authority, 0, slot_start, slot_end);
        assert_rejected(harness.send_as_authority(off_grid).await, expected);
    }

    assert_eq!(harness.state().await.next_window_id, 0);
    assert_eq!(harness.account(&window_key(0)).await, None);
}

#[tokio::test]
async fn commit_future_window() {
    let mut harness = Harness::start_initialized().await;

    assert_rejected(harness.commit(0).await, ClarionError::WindowNotClosed);

    harness.warp_to(window_end(0));
    assert_rejected(harness.commit(0).await, ClarionError::WindowNotClosed);
    assert_eq!(harness.state().await.next_window_id, 0);
    assert_eq!(harness.account(&window_key(0)).await, None);

    harness.warp_to(window_end(0) + 1);
    harness.commit(0).await.unwrap();
    assert_eq!(harness.window(0).await.commit_slot, window_end(0) + 1);

    assert_rejected(harness.commit(1).await, ClarionError::WindowNotClosed);
}

#[tokio::test]
async fn commit_succeeds_on_prefunded_window_address() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(1) + 1);
    let rent = harness.rent().await;
    let rent_exempt_minimum = rent.minimum_balance(WINDOW_SIZE);
    let underfunded = rent.minimum_balance(0);
    let overfunded = rent_exempt_minimum + 1_000;
    harness.fund(&window_key(0), underfunded).await;
    harness.fund(&window_key(1), overfunded).await;

    harness.commit(0).await.unwrap();
    harness.commit(1).await.unwrap();

    let topped_up = harness.account(&window_key(0)).await.unwrap();
    let untouched = harness.account(&window_key(1)).await.unwrap();
    assert!(underfunded < rent_exempt_minimum);
    assert_eq!(topped_up.lamports, rent_exempt_minimum);
    assert_eq!(untouched.lamports, overfunded);
    for account in [topped_up, untouched] {
        assert_eq!(account.owner, *PROGRAM_ID);
        assert_eq!(account.data.len(), WINDOW_SIZE);
    }
    assert_eq!(harness.window(0).await.merkle_root, MERKLE_ROOT);
    assert_eq!(harness.window(1).await.window_id, 1);
    assert_eq!(harness.state().await.next_window_id, 2);
}

#[tokio::test]
async fn commit_rejects_an_existing_window_account() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(0) + 1);
    harness.set_account(
        &window_key(0),
        data_account(&PROGRAM_ID, vec![0; WINDOW_SIZE]),
    );

    assert_rejected(
        harness.commit(0).await,
        ClarionError::WindowAlreadyCommitted,
    );
    assert_eq!(harness.state().await.next_window_id, 0);
}

#[tokio::test]
async fn commit_rejects_an_occupied_window_address() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(0) + 1);

    for occupied in [
        data_account(&system_program::ID, vec![0; WINDOW_SIZE]),
        data_account(&FOREIGN_PROGRAM_ID, Vec::new()),
    ] {
        harness.set_account(&window_key(0), occupied.clone());
        assert_rejected(
            harness.commit(0).await,
            ClarionError::WindowAlreadyCommitted,
        );
        assert_eq!(harness.account(&window_key(0)).await, Some(occupied));
        assert_eq!(harness.state().await.next_window_id, 0);
    }
}

#[tokio::test]
async fn commit_rejects_bounds_overflowing_u64() {
    let mut harness = Harness::start().await;
    let genesis_slot = u64::MAX - 5;
    let state = State {
        discriminator: STATE_DISCRIMINATOR,
        authority: harness.authority.pubkey(),
        genesis_slot,
        window_len: WINDOW_LEN,
        min_reveal_lag_slots: MIN_REVEAL_LAG_SLOTS,
        next_window_id: 0,
        bump: state_address(&PROGRAM_ID).1,
    };
    harness.set_account(
        &state_key(),
        data_account(&PROGRAM_ID, borsh::to_vec(&state).unwrap()),
    );
    let authority = harness.authority.pubkey();

    let overflowing = commit_instruction(&authority, 0, genesis_slot, u64::MAX);
    assert_rejected(
        harness.send_as_authority(overflowing).await,
        ClarionError::WindowBoundsOverflow,
    );
}
