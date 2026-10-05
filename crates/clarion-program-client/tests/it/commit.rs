use clarion_program_client::{
    ARM_COUNT, AccountKind, ArmAggregates, ClarionError, DecodeError, STATE_SIZE, State,
    WINDOW_DISCRIMINATOR, WINDOW_SIZE, Window, commit, decode_state, decode_window,
    window_address, window_slots,
};
use solana_keypair::Signer;
use solana_program_test::tokio;

use crate::support::{
    GENESIS_SLOT, Harness, PROGRAM_ID, WINDOW_LEN, assert_rejected, merkle_root,
    window_key,
};

#[tokio::test]
async fn commit_on_the_decoded_grid_creates_the_window_and_advances_the_state() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let initialized = harness.state().await;
    let (slot_start, slot_end) = window_slots(&initialized, 0).unwrap();
    let commit_slot = slot_end + 1;
    harness.warp_to(commit_slot);

    harness
        .send(commit(
            &PROGRAM_ID,
            &authority,
            0,
            slot_start,
            slot_end,
            merkle_root(),
        ))
        .await
        .unwrap();

    let (window_key, bump) = window_address(&PROGRAM_ID, 0);
    let account = harness.account(&window_key).await;
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), WINDOW_SIZE);
    assert_eq!(
        decode_window(&account.data),
        Ok(Window {
            discriminator: WINDOW_DISCRIMINATOR,
            window_id: 0,
            slot_start: GENESIS_SLOT,
            slot_end: GENESIS_SLOT + WINDOW_LEN - 1,
            merkle_root: merkle_root(),
            commit_slot,
            reveal_slot: 0,
            aggregates: [ArmAggregates::default(); ARM_COUNT],
            bump,
        })
    );
    assert_eq!(
        harness.state().await,
        State {
            next_window_id: 1,
            ..initialized
        }
    );
}

#[tokio::test]
async fn consecutive_windows_land_in_their_own_accounts() {
    let (mut harness, first_commit_slot) = Harness::start_committed().await;

    let second_commit_slot = harness.commit_once_closed(1).await;

    let first = harness.window(0).await;
    let second = harness.window(1).await;
    assert_eq!(first.window_id, 0);
    assert_eq!(first.commit_slot, first_commit_slot);
    assert_eq!(second.window_id, 1);
    assert_eq!(second.slot_start, first.slot_end + 1);
    assert_eq!(second.slot_end, first.slot_end + WINDOW_LEN);
    assert_eq!(second.commit_slot, second_commit_slot);
    assert_eq!(second.bump, window_address(&PROGRAM_ID, 1).1);
    assert_eq!(harness.state().await.next_window_id, 2);
}

#[tokio::test]
async fn commit_off_the_decoded_grid_is_rejected_by_the_program() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let state = harness.state().await;
    let (slot_start, slot_end) = window_slots(&state, 0).unwrap();
    harness.warp_to(slot_end + 2);

    assert_rejected(
        harness
            .send(commit(
                &PROGRAM_ID,
                &authority,
                0,
                slot_start + 1,
                slot_end,
                merkle_root(),
            ))
            .await,
        ClarionError::SlotStartOffGrid,
    );
    assert_rejected(
        harness
            .send(commit(
                &PROGRAM_ID,
                &authority,
                0,
                slot_start,
                slot_end + 1,
                merkle_root(),
            ))
            .await,
        ClarionError::SlotEndOffGrid,
    );

    assert_eq!(harness.state().await, state);
}

#[tokio::test]
async fn on_chain_window_does_not_decode_as_a_state() {
    let (mut harness, _) = Harness::start_committed().await;

    let account = harness.account(&window_key(0)).await;

    assert_eq!(
        decode_state(&account.data),
        Err(DecodeError::Length {
            account: AccountKind::State,
            expected: STATE_SIZE,
            actual: WINDOW_SIZE,
        })
    );
}
