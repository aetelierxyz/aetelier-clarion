use clarion::{ARM_COUNT, ArmAggregates, ClarionError, Window};
use solana_program_test::tokio;
use solana_signer::Signer;

use crate::support::{
    Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, aggregates, arm, assert_rejected,
    data_account, reveal_instruction, window_end, window_key,
};

#[tokio::test]
async fn reveal_before_lag() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let committed = harness.window(0).await;

    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::RevealLagNotElapsed,
    );

    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS - 1);
    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::RevealLagNotElapsed,
    );

    assert_eq!(harness.window(0).await, committed);
    assert!(!committed.is_revealed());
}

#[tokio::test]
async fn reveal_rejects_a_commit_slot_ahead_of_the_clock() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let mut window = harness.window(0).await;
    window.commit_slot = commit_slot + 1_000;
    harness.set_account(
        &window_key(0),
        data_account(&PROGRAM_ID, borsh::to_vec(&window).unwrap()),
    );

    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::RevealLagNotElapsed,
    );
    assert_eq!(harness.window(0).await, window);
}

#[tokio::test]
async fn reveal_after_lag() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let committed = harness.window(0).await;
    let state = harness.state().await;
    let reveal_slot = commit_slot + MIN_REVEAL_LAG_SLOTS;
    harness.warp_to(reveal_slot);

    harness.reveal(0, aggregates()).await.unwrap();

    let revealed = harness.window(0).await;
    assert_eq!(
        revealed,
        Window {
            reveal_slot,
            aggregates: aggregates(),
            ..committed
        }
    );
    assert!(revealed.is_revealed());
    assert_eq!(revealed.commit_slot, commit_slot);
    assert_eq!(revealed.aggregates[3], arm(4));
    assert_eq!(harness.state().await, state);
}

#[tokio::test]
async fn reveal_succeeds_for_a_nonzero_window_id() {
    let mut harness = Harness::start_initialized().await;
    let commit_slot = window_end(1) + 1;
    harness.warp_to(commit_slot);
    harness.commit(0).await.unwrap();
    harness.commit(1).await.unwrap();
    let reveal_slot = commit_slot + MIN_REVEAL_LAG_SLOTS;
    harness.warp_to(reveal_slot);
    let untouched = harness.window(0).await;

    harness.reveal(1, aggregates()).await.unwrap();

    let revealed = harness.window(1).await;
    assert_eq!(revealed.reveal_slot, reveal_slot);
    assert_eq!(revealed.aggregates, aggregates());
    assert_eq!(harness.window(0).await, untouched);
}

#[tokio::test]
async fn reveal_twice() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    harness.reveal(0, aggregates()).await.unwrap();
    let revealed = harness.window(0).await;

    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::WindowAlreadyRevealed,
    );
    assert_rejected(
        harness
            .reveal(0, [ArmAggregates::default(); ARM_COUNT])
            .await,
        ClarionError::WindowAlreadyRevealed,
    );

    harness.warp_to(commit_slot + 2 * MIN_REVEAL_LAG_SLOTS);
    assert_rejected(
        harness.reveal(0, [arm(9); ARM_COUNT]).await,
        ClarionError::WindowAlreadyRevealed,
    );
    assert_eq!(harness.window(0).await, revealed);
}

#[tokio::test]
async fn reveal_landed_exceeds_submitted() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let committed = harness.window(0).await;

    for index in 0..ARM_COUNT {
        let mut exceeding = aggregates();
        exceeding[index].tx_landed = exceeding[index].tx_submitted + 1;

        assert_rejected(
            harness.reveal(0, exceeding).await,
            ClarionError::LandedExceedsSubmitted,
        );
    }
    assert_eq!(harness.window(0).await, committed);

    let mut all_landed = aggregates();
    for arm in &mut all_landed {
        arm.tx_landed = arm.tx_submitted;
    }
    harness.reveal(0, all_landed).await.unwrap();
    assert_eq!(harness.window(0).await.aggregates, all_landed);
}

#[tokio::test]
async fn reveal_requires_authority() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let authority = harness.authority.pubkey();
    let intruder = harness.intruder.insecure_clone();

    let by_intruder = reveal_instruction(&intruder.pubkey(), 0, aggregates());
    assert_rejected(
        harness.send(by_intruder, &[&intruder]).await,
        ClarionError::AuthorityMismatch,
    );

    let mut unsigned = reveal_instruction(&authority, 0, aggregates());
    unsigned.accounts[0].is_signer = false;
    assert_rejected(
        harness.send(unsigned, &[]).await,
        ClarionError::AuthoritySignatureMissing,
    );

    assert!(!harness.window(0).await.is_revealed());
}
