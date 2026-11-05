use clarion::{ClarionError, ClarionEvent};
use solana_program::instruction::{Instruction, InstructionError};
use solana_program_test::tokio;
use solana_signer::Signer;
use solana_transaction::TransactionError;

use crate::support::{
    DATA_LOG_PREFIX, GENESIS_SLOT, Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, Sent,
    WINDOW_LEN, accept_instruction, aggregates, cancel_instruction,
    grid_commit_instruction, init_instruction, proposal_key, propose_instruction,
    reveal_instruction, window_end,
};

fn assert_rejected_silently(sent: &Sent, expected: ClarionError) {
    assert_eq!(
        sent.result,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::Custom(expected.code())
        ))
    );
    assert!(
        sent.logs.iter().all(|line| !line.contains(DATA_LOG_PREFIX)),
        "{:?}",
        sent.logs
    );
}

#[tokio::test]
async fn init_emits_initialized_with_the_stored_state() {
    let mut harness = Harness::start().await;
    let instruction = init_instruction(
        &harness.authority.pubkey(),
        GENESIS_SLOT,
        WINDOW_LEN,
        MIN_REVEAL_LAG_SLOTS,
    );

    let sent = harness.send_logged_as_authority(instruction).await;

    assert_eq!(sent.result, Ok(()));
    let state = harness.state().await;
    assert_eq!(
        harness.events(&sent.logs).await,
        [ClarionEvent::Initialized {
            authority: state.authority,
            genesis_slot: state.genesis_slot,
            window_len: state.window_len,
            min_reveal_lag_slots: state.min_reveal_lag_slots,
        }]
    );
    assert_eq!(state.authority, harness.authority.pubkey());
}

#[tokio::test]
async fn commit_emits_committed_with_the_stored_window() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();

    for window_id in [0, 1] {
        harness.warp_to(window_end(window_id) + 1);
        let sent = harness
            .send_logged_as_authority(grid_commit_instruction(&authority, window_id))
            .await;

        assert_eq!(sent.result, Ok(()));
        let window = harness.window(window_id).await;
        assert_eq!(
            harness.events(&sent.logs).await,
            [ClarionEvent::Committed {
                window_id: window.window_id,
                slot_start: window.slot_start,
                slot_end: window.slot_end,
                merkle_root: window.merkle_root,
                commit_slot: window.commit_slot,
            }]
        );
        assert_eq!(window.window_id, window_id);
        assert_eq!(harness.state().await.next_window_id, window_id + 1);
        harness.reveal_after_lag(window_id).await;
    }
}

#[tokio::test]
async fn reveal_emits_revealed_with_the_stored_aggregates() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let instruction = reveal_instruction(&harness.authority.pubkey(), 0, aggregates());

    let sent = harness.send_logged_as_authority(instruction).await;

    assert_eq!(sent.result, Ok(()));
    let window = harness.window(0).await;
    assert_eq!(
        harness.events(&sent.logs).await,
        [ClarionEvent::Revealed {
            window_id: window.window_id,
            reveal_slot: window.reveal_slot,
            aggregates: window.aggregates,
        }]
    );
    assert_eq!(window.reveal_slot, commit_slot + MIN_REVEAL_LAG_SLOTS);
    assert_eq!(window.aggregates, aggregates());
}

#[tokio::test]
async fn propose_emits_authority_proposed_on_create_and_on_overwrite() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();

    for proposed in [harness.successor.pubkey(), harness.intruder.pubkey()] {
        let sent = harness
            .send_logged_as_authority(propose_instruction(&authority, &proposed))
            .await;

        assert_eq!(sent.result, Ok(()));
        let proposal = harness.proposal().await;
        assert_eq!(
            harness.events(&sent.logs).await,
            [ClarionEvent::AuthorityProposed {
                authority: harness.state().await.authority,
                proposed_authority: proposal.proposed_authority,
            }]
        );
        assert_eq!(proposal.proposed_authority, proposed);
    }
}

#[tokio::test]
async fn accept_emits_authority_accepted_with_the_handover() {
    let mut harness = Harness::start_initialized().await;
    let previous = harness.state().await.authority;
    let successor = harness.successor.insecure_clone();
    harness.propose(&successor.pubkey()).await.unwrap();

    let sent = harness
        .send_logged(&[accept_instruction(&successor.pubkey())], &[&successor])
        .await;

    assert_eq!(sent.result, Ok(()));
    let state = harness.state().await;
    assert_eq!(
        harness.events(&sent.logs).await,
        [ClarionEvent::AuthorityAccepted {
            previous_authority: previous,
            authority: state.authority,
        }]
    );
    assert_eq!(state.authority, successor.pubkey());
    assert_eq!(previous, harness.authority.pubkey());
}

#[tokio::test]
async fn cancel_emits_authority_proposal_cancelled_with_the_withdrawn_proposal() {
    let mut harness = Harness::start_initialized().await;
    let successor = harness.successor.pubkey();
    harness.propose(&successor).await.unwrap();
    let proposal = harness.proposal().await;

    let sent = harness
        .send_logged_as_authority(cancel_instruction(&harness.authority.pubkey()))
        .await;

    assert_eq!(sent.result, Ok(()));
    assert_eq!(
        harness.events(&sent.logs).await,
        [ClarionEvent::AuthorityProposalCancelled {
            authority: harness.state().await.authority,
            proposed_authority: proposal.proposed_authority,
        }]
    );
    assert_eq!(harness.account(&proposal_key()).await, None);
}

#[tokio::test]
async fn each_instruction_of_a_transaction_emits_its_own_event_in_order() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    harness.propose(&successor.pubkey()).await.unwrap();

    let sent = harness
        .send_logged(
            &[
                accept_instruction(&successor.pubkey()),
                propose_instruction(&successor.pubkey(), &authority),
            ],
            &[&successor],
        )
        .await;

    assert_eq!(sent.result, Ok(()));
    assert_eq!(
        harness.events(&sent.logs).await,
        [
            ClarionEvent::AuthorityAccepted {
                previous_authority: authority,
                authority: successor.pubkey(),
            },
            ClarionEvent::AuthorityProposed {
                authority: successor.pubkey(),
                proposed_authority: authority,
            },
        ]
    );
}

#[tokio::test]
async fn rejected_init_and_commit_emit_nothing() {
    let mut harness = Harness::start().await;
    let authority = harness.authority.pubkey();

    let zero_window = init_instruction(&authority, GENESIS_SLOT, 0, MIN_REVEAL_LAG_SLOTS);
    let sent = harness.send_logged_as_authority(zero_window).await;
    assert_rejected_silently(&sent, ClarionError::WindowLenZero);

    harness
        .init(GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS)
        .await
        .unwrap();
    let again =
        init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS);
    let sent = harness.send_logged_as_authority(again).await;
    assert_rejected_silently(&sent, ClarionError::StateAlreadyInitialized);

    let open_window = grid_commit_instruction(&authority, 0);
    let sent = harness.send_logged_as_authority(open_window).await;
    assert_rejected_silently(&sent, ClarionError::WindowNotClosed);

    harness.warp_to(window_end(1) + 1);
    let skipped = grid_commit_instruction(&authority, 1);
    let sent = harness.send_logged_as_authority(skipped).await;
    assert_rejected_silently(&sent, ClarionError::WindowIdNotSequential);

    let garbage = Instruction {
        program_id: *PROGRAM_ID,
        accounts: Vec::new(),
        data: vec![9],
    };
    let sent = harness.send_logged(&[garbage], &[]).await;
    assert_rejected_silently(&sent, ClarionError::InstructionDataMalformed);
}

#[tokio::test]
async fn rejected_reveal_emits_nothing() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let authority = harness.authority.pubkey();

    let early = reveal_instruction(&authority, 0, aggregates());
    let sent = harness.send_logged_as_authority(early).await;
    assert_rejected_silently(&sent, ClarionError::RevealLagNotElapsed);

    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    harness.reveal(0, aggregates()).await.unwrap();
    let twice = reveal_instruction(&authority, 0, aggregates());
    let sent = harness.send_logged_as_authority(twice).await;
    assert_rejected_silently(&sent, ClarionError::WindowAlreadyRevealed);
}

#[tokio::test]
async fn rejected_authority_instructions_emit_nothing() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let intruder = harness.intruder.insecure_clone();

    let sent = harness
        .send_logged_as_authority(cancel_instruction(&authority))
        .await;
    assert_rejected_silently(&sent, ClarionError::ProposalMissing);

    let unchanged = propose_instruction(&authority, &authority);
    let sent = harness.send_logged_as_authority(unchanged).await;
    assert_rejected_silently(&sent, ClarionError::AuthorityUnchanged);

    let usurped = propose_instruction(&intruder.pubkey(), &intruder.pubkey());
    let sent = harness.send_logged(&[usurped], &[&intruder]).await;
    assert_rejected_silently(&sent, ClarionError::AuthorityMismatch);

    harness.propose(&successor.pubkey()).await.unwrap();
    let sent = harness
        .send_logged(&[accept_instruction(&intruder.pubkey())], &[&intruder])
        .await;
    assert_rejected_silently(&sent, ClarionError::ProposedAuthorityMismatch);

    let sent = harness
        .send_logged(&[cancel_instruction(&intruder.pubkey())], &[&intruder])
        .await;
    assert_rejected_silently(&sent, ClarionError::AuthorityMismatch);
}
