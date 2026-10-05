use clarion::{
    ClarionError, STATE_DISCRIMINATOR, STATE_SIZE, State, WINDOW_DISCRIMINATOR,
    WINDOW_SIZE, state_address,
};
use solana_program::{instruction::AccountMeta, pubkey::Pubkey};
use solana_program_test::tokio;
use solana_signer::Signer;

use crate::support::{
    FOREIGN_PROGRAM_ID, GENESIS_SLOT, Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID,
    WINDOW_LEN, aggregates, assert_rejected, data_account, grid_commit_instruction,
    init_instruction, reveal_instruction, state_key, window_end, window_key,
};

const STRAY_STATE_KEY: Pubkey = Pubkey::new_from_array([201; 32]);
const STRAY_WINDOW_KEY: Pubkey = Pubkey::new_from_array([202; 32]);

fn forged_state_data(authority: &Pubkey) -> Vec<u8> {
    let state = State {
        discriminator: STATE_DISCRIMINATOR,
        authority: *authority,
        genesis_slot: GENESIS_SLOT,
        window_len: WINDOW_LEN,
        min_reveal_lag_slots: 1,
        next_window_id: 0,
        bump: state_address(&PROGRAM_ID).1,
    };
    borsh::to_vec(&state).unwrap()
}

#[tokio::test]
async fn wrong_owner() {
    let mut harness = Harness::start().await;
    let commit_slot = window_end(0) + 1;
    harness.warp_to(commit_slot);
    let authority = harness.authority.pubkey();
    let intruder = harness.intruder.insecure_clone();

    assert_rejected(harness.commit(0).await, ClarionError::StateOwnerMismatch);

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();
    harness.set_account(
        &STRAY_STATE_KEY,
        data_account(&FOREIGN_PROGRAM_ID, forged_state_data(&intruder.pubkey())),
    );
    let mut commit = grid_commit_instruction(&intruder.pubkey(), 0);
    commit.accounts[1].pubkey = STRAY_STATE_KEY;
    assert_rejected(
        harness.send(commit, &[&intruder]).await,
        ClarionError::StateOwnerMismatch,
    );
    assert_eq!(harness.state().await.next_window_id, 0);

    harness.commit(0).await.unwrap();
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let committed = harness.window(0).await;

    let mut reveal = reveal_instruction(&intruder.pubkey(), 0, aggregates());
    reveal.accounts[1].pubkey = STRAY_STATE_KEY;
    assert_rejected(
        harness.send(reveal, &[&intruder]).await,
        ClarionError::StateOwnerMismatch,
    );

    harness.set_account(
        &STRAY_WINDOW_KEY,
        data_account(&FOREIGN_PROGRAM_ID, borsh::to_vec(&committed).unwrap()),
    );
    let mut reveal = reveal_instruction(&authority, 0, aggregates());
    reveal.accounts[2].pubkey = STRAY_WINDOW_KEY;
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::WindowOwnerMismatch,
    );

    let mut reveal = reveal_instruction(&authority, 1, aggregates());
    reveal.accounts[2].pubkey = window_key(1);
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::WindowOwnerMismatch,
    );
    assert_eq!(harness.window(0).await, committed);
}

#[tokio::test]
async fn wrong_pda() {
    let mut harness = Harness::start().await;
    let commit_slot = window_end(1) + 1;
    harness.warp_to(commit_slot);
    let authority = harness.authority.pubkey();
    let intruder = harness.intruder.insecure_clone();

    let mut init =
        init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS);
    init.accounts[1].pubkey = STRAY_STATE_KEY;
    assert_rejected(
        harness.send_as_authority(init).await,
        ClarionError::StateAddressMismatch,
    );
    assert_eq!(harness.account(&STRAY_STATE_KEY).await, None);

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();
    harness.set_account(
        &STRAY_STATE_KEY,
        data_account(&PROGRAM_ID, forged_state_data(&intruder.pubkey())),
    );
    let mut commit = grid_commit_instruction(&intruder.pubkey(), 0);
    commit.accounts[1].pubkey = STRAY_STATE_KEY;
    assert_rejected(
        harness.send(commit, &[&intruder]).await,
        ClarionError::StateAddressMismatch,
    );

    let mut commit = grid_commit_instruction(&authority, 0);
    commit.accounts[2].pubkey = window_key(1);
    assert_rejected(
        harness.send_as_authority(commit).await,
        ClarionError::WindowAddressMismatch,
    );
    assert_eq!(harness.state().await.next_window_id, 0);

    harness.commit(0).await.unwrap();
    harness.commit(1).await.unwrap();
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let committed = harness.window(0).await;

    let mut reveal = reveal_instruction(&intruder.pubkey(), 0, aggregates());
    reveal.accounts[1].pubkey = STRAY_STATE_KEY;
    assert_rejected(
        harness.send(reveal, &[&intruder]).await,
        ClarionError::StateAddressMismatch,
    );

    let mut reveal = reveal_instruction(&authority, 0, aggregates());
    reveal.accounts[2].pubkey = window_key(1);
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::WindowAddressMismatch,
    );

    harness.set_account(
        &STRAY_WINDOW_KEY,
        data_account(&PROGRAM_ID, borsh::to_vec(&committed).unwrap()),
    );
    let mut reveal = reveal_instruction(&authority, 0, aggregates());
    reveal.accounts[2].pubkey = STRAY_WINDOW_KEY;
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::WindowAddressMismatch,
    );
    assert_eq!(harness.window(0).await, committed);
    assert!(!harness.window(1).await.is_revealed());
}

#[tokio::test]
async fn rejects_a_state_whose_stored_bump_is_not_the_derived_one() {
    let mut harness = Harness::start_initialized().await;
    harness.warp_to(window_end(0) + 1);
    let mut state = harness.state().await;
    state.bump = state.bump.wrapping_sub(1);
    harness.set_account(
        &state_key(),
        data_account(&PROGRAM_ID, borsh::to_vec(&state).unwrap()),
    );

    assert_rejected(harness.commit(0).await, ClarionError::StateAddressMismatch);
    assert_eq!(harness.account(&window_key(0)).await, None);
}

#[tokio::test]
async fn rejects_a_window_whose_stored_bump_is_not_the_derived_one() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let mut window = harness.window(0).await;
    window.bump = window.bump.wrapping_sub(1);
    harness.set_account(
        &window_key(0),
        data_account(&PROGRAM_ID, borsh::to_vec(&window).unwrap()),
    );

    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::WindowAddressMismatch,
    );
    assert_eq!(harness.window(0).await, window);
}

#[tokio::test]
async fn wrong_discriminator() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + WINDOW_LEN + MIN_REVEAL_LAG_SLOTS);
    let authority = harness.authority.pubkey();
    let state = harness.state().await;
    let committed = harness.window(0).await;

    let mut reveal = reveal_instruction(&authority, 0, aggregates());
    reveal.accounts[2].pubkey = state_key();
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::WindowDiscriminatorMismatch,
    );

    let mut reveal = reveal_instruction(&authority, 0, aggregates());
    reveal.accounts[1].pubkey = window_key(0);
    assert_rejected(
        harness.send_as_authority(reveal).await,
        ClarionError::StateDiscriminatorMismatch,
    );

    let mut commit = grid_commit_instruction(&authority, 1);
    commit.accounts[1].pubkey = window_key(0);
    assert_rejected(
        harness.send_as_authority(commit).await,
        ClarionError::StateDiscriminatorMismatch,
    );

    assert_eq!(harness.state().await, state);
    assert_eq!(harness.window(0).await, committed);
}

#[tokio::test]
async fn rejects_account_data_of_another_length() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let mut truncated_window = vec![0; WINDOW_SIZE - 1];
    truncated_window[0] = WINDOW_DISCRIMINATOR;
    let mut oversized_state = vec![0; STATE_SIZE + 1];
    oversized_state[0] = STATE_DISCRIMINATOR;

    harness.set_account(&window_key(0), data_account(&PROGRAM_ID, truncated_window));
    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::WindowDataMalformed,
    );

    harness.set_account(&state_key(), data_account(&PROGRAM_ID, oversized_state));
    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::StateDataMalformed,
    );
    assert_rejected(harness.commit(1).await, ClarionError::StateDataMalformed);
}

#[tokio::test]
async fn rejects_instruction_data_that_does_not_decode_exactly() {
    let mut harness = Harness::start().await;
    let authority = harness.authority.pubkey();
    let init =
        init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS);

    let mut trailing = init.clone();
    trailing.data.push(0);
    let mut truncated = init.clone();
    truncated.data.pop();
    let mut unknown_variant = init.clone();
    unknown_variant.data[0] = 3;
    let mut empty = init;
    empty.data.clear();

    for malformed in [trailing, truncated, unknown_variant, empty] {
        assert_rejected(
            harness.send_as_authority(malformed).await,
            ClarionError::InstructionDataMalformed,
        );
    }
    assert_eq!(harness.account(&state_key()).await, None);
}

#[tokio::test]
async fn rejects_an_account_list_of_another_length() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + WINDOW_LEN + MIN_REVEAL_LAG_SLOTS);
    let authority = harness.authority.pubkey();
    let extra = AccountMeta::new_readonly(FOREIGN_PROGRAM_ID, false);

    for complete in [
        init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS),
        grid_commit_instruction(&authority, 1),
        reveal_instruction(&authority, 0, aggregates()),
    ] {
        let mut short = complete.clone();
        short.accounts.pop();
        let mut long = complete;
        long.accounts.push(extra.clone());

        for miscounted in [short, long] {
            assert_rejected(
                harness.send_as_authority(miscounted).await,
                ClarionError::AccountCountMismatch,
            );
        }
    }
    assert_eq!(harness.state().await.next_window_id, 1);
    assert!(!harness.window(0).await.is_revealed());
}

#[tokio::test]
async fn rejects_a_substituted_system_program() {
    let mut harness = Harness::start().await;
    harness.warp_to(window_end(0) + 1);
    let authority = harness.authority.pubkey();

    let mut init =
        init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS);
    init.accounts[2].pubkey = FOREIGN_PROGRAM_ID;
    assert_rejected(
        harness.send_as_authority(init).await,
        ClarionError::SystemProgramMismatch,
    );
    assert_eq!(harness.account(&state_key()).await, None);

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();
    let mut commit = grid_commit_instruction(&authority, 0);
    commit.accounts[3].pubkey = FOREIGN_PROGRAM_ID;
    assert_rejected(
        harness.send_as_authority(commit).await,
        ClarionError::SystemProgramMismatch,
    );
    assert_eq!(harness.state().await.next_window_id, 0);
}
