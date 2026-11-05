use clarion_program_client::{
    AuthorityProposal, ClarionError, PROPOSAL_DISCRIMINATOR, PROPOSAL_SIZE, State,
    accept_authority, cancel_authority_proposal, commit, decode_proposal,
    proposal_address, propose_authority, reveal, window_slots,
};
use solana_keypair::Signer;
use solana_program_test::tokio;

use crate::support::{
    Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, aggregates, assert_rejected, merkle_root,
    proposal_key,
};

#[tokio::test]
async fn accepted_proposal_moves_commit_and_reveal_to_the_new_authority() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let initialized = harness.state().await;

    harness
        .send(propose_authority(
            &PROGRAM_ID,
            &authority,
            &successor.pubkey(),
        ))
        .await
        .unwrap();

    let account = harness.account(&proposal_key()).await;
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), PROPOSAL_SIZE);
    assert_eq!(
        decode_proposal(&account.data),
        Ok(AuthorityProposal {
            discriminator: PROPOSAL_DISCRIMINATOR,
            proposed_authority: successor.pubkey(),
            bump: proposal_address(&PROGRAM_ID).1,
        })
    );

    harness
        .send_signed_by(
            accept_authority(&PROGRAM_ID, &successor.pubkey()),
            &successor,
        )
        .await
        .unwrap();

    assert_eq!(
        harness.state().await,
        State {
            authority: successor.pubkey(),
            ..initialized
        }
    );
    assert!(!harness.exists(&proposal_key()).await);

    let (slot_start, slot_end) = window_slots(&initialized, 0).unwrap();
    let commit_slot = slot_end + 1;
    harness.warp_to(commit_slot);
    let by_old_authority = commit(
        &PROGRAM_ID,
        &authority,
        0,
        slot_start,
        slot_end,
        merkle_root(),
    );
    assert_rejected(
        harness.send(by_old_authority).await,
        ClarionError::AuthorityMismatch,
    );
    harness
        .send_signed_by(
            commit(
                &PROGRAM_ID,
                &successor.pubkey(),
                0,
                slot_start,
                slot_end,
                merkle_root(),
            ),
            &successor,
        )
        .await
        .unwrap();
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    harness
        .send_signed_by(
            reveal(&PROGRAM_ID, &successor.pubkey(), 0, aggregates()),
            &successor,
        )
        .await
        .unwrap();

    assert_eq!(harness.window(0).await.aggregates, aggregates());
}

#[tokio::test]
async fn cancelled_proposal_cannot_be_accepted() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let initialized = harness.state().await;
    harness
        .send(propose_authority(
            &PROGRAM_ID,
            &authority,
            &successor.pubkey(),
        ))
        .await
        .unwrap();

    harness
        .send(cancel_authority_proposal(&PROGRAM_ID, &authority))
        .await
        .unwrap();

    assert!(!harness.exists(&proposal_key()).await);
    assert_rejected(
        harness
            .send_signed_by(
                accept_authority(&PROGRAM_ID, &successor.pubkey()),
                &successor,
            )
            .await,
        ClarionError::ProposalMissing,
    );
    assert_eq!(harness.state().await, initialized);
}

#[tokio::test]
async fn propose_of_the_stored_authority_is_rejected_as_unchanged() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();

    assert_rejected(
        harness
            .send(propose_authority(&PROGRAM_ID, &authority, &authority))
            .await,
        ClarionError::AuthorityUnchanged,
    );
    assert!(!harness.exists(&proposal_key()).await);
}
