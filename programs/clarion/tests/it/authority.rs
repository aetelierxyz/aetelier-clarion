use clarion::{
    AuthorityProposal, ClarionError, PROPOSAL_DISCRIMINATOR, PROPOSAL_SIZE, State,
    proposal_address,
};
use solana_program::{instruction::AccountMeta, pubkey::Pubkey};
use solana_program_test::tokio;
use solana_signer::Signer;

use crate::support::{
    FOREIGN_PROGRAM_ID, Harness, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, accept_instruction,
    aggregates, assert_rejected, cancel_instruction, data_account,
    grid_commit_instruction, proposal_key, propose_instruction, reveal_instruction,
    state_key, window_end,
};

const STRAY_PROPOSAL_KEY: Pubkey = Pubkey::new_from_array([204; 32]);
const STRAY_STATE_KEY: Pubkey = Pubkey::new_from_array([201; 32]);
const ACCOMPLICE: Pubkey = Pubkey::new_from_array([205; 32]);

fn proposal_for(proposed_authority: &Pubkey) -> AuthorityProposal {
    AuthorityProposal {
        discriminator: PROPOSAL_DISCRIMINATOR,
        proposed_authority: *proposed_authority,
        bump: proposal_address(&PROGRAM_ID).1,
    }
}

fn proposal_data(proposed_authority: &Pubkey) -> Vec<u8> {
    borsh::to_vec(&proposal_for(proposed_authority)).unwrap()
}

async fn proposal_rent(harness: &mut Harness) -> u64 {
    harness.rent().await.minimum_balance(PROPOSAL_SIZE)
}

async fn assert_proposal_closed(harness: &mut Harness) {
    assert_eq!(harness.account(&proposal_key()).await, None);
    assert_eq!(harness.lamports(&proposal_key()).await, 0);
}

#[tokio::test]
async fn rotation_hands_commit_and_reveal_to_the_accepted_authority() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.insecure_clone();
    let successor = harness.successor.insecure_clone();
    let initialized = harness.state().await;
    let rent = proposal_rent(&mut harness).await;
    let authority_before = harness.lamports(&authority.pubkey()).await;

    harness.propose(&successor.pubkey()).await.unwrap();

    let account = harness.account(&proposal_key()).await.unwrap();
    assert_eq!(account.owner, *PROGRAM_ID);
    assert_eq!(account.data.len(), PROPOSAL_SIZE);
    assert_eq!(account.lamports, rent);
    assert_eq!(
        AuthorityProposal::unpack(&account.data),
        Ok(proposal_for(&successor.pubkey()))
    );
    assert_eq!(
        harness.lamports(&authority.pubkey()).await,
        authority_before - rent
    );
    assert_eq!(harness.state().await, initialized);

    let successor_before = harness.lamports(&successor.pubkey()).await;
    harness.accept(&successor).await.unwrap();

    assert_eq!(
        harness.state().await,
        State {
            authority: successor.pubkey(),
            ..initialized
        }
    );
    assert_proposal_closed(&mut harness).await;
    assert_eq!(
        harness.lamports(&successor.pubkey()).await,
        successor_before + rent
    );

    let commit_slot = window_end(0) + 1;
    harness.warp_to(commit_slot);
    assert_rejected(harness.commit(0).await, ClarionError::AuthorityMismatch);
    harness
        .send(
            grid_commit_instruction(&successor.pubkey(), 0),
            &[&successor],
        )
        .await
        .unwrap();
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::AuthorityMismatch,
    );
    harness
        .send(
            reveal_instruction(&successor.pubkey(), 0, aggregates()),
            &[&successor],
        )
        .await
        .unwrap();

    let revealed = harness.window(0).await;
    assert!(revealed.is_revealed());
    assert_eq!(revealed.aggregates, aggregates());
    assert_eq!(harness.state().await.next_window_id, 1);
    assert_rejected(
        harness.propose(&harness.intruder.pubkey()).await,
        ClarionError::AuthorityMismatch,
    );

    harness
        .send(
            propose_instruction(&successor.pubkey(), &authority.pubkey()),
            &[&successor],
        )
        .await
        .unwrap();
    assert_eq!(harness.proposal().await, proposal_for(&authority.pubkey()));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
}

#[tokio::test]
async fn propose_overwrites_a_pending_proposal_without_charging_rent_twice() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let intruder = harness.intruder.insecure_clone();
    let rent = proposal_rent(&mut harness).await;
    harness.propose(&successor.pubkey()).await.unwrap();
    let authority_before = harness.lamports(&authority).await;

    harness.propose(&intruder.pubkey()).await.unwrap();

    assert_eq!(harness.proposal().await, proposal_for(&intruder.pubkey()));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
    assert_eq!(harness.lamports(&authority).await, authority_before);

    assert_rejected(
        harness.accept(&successor).await,
        ClarionError::ProposedAuthorityMismatch,
    );
    harness.accept(&intruder).await.unwrap();
    assert_eq!(harness.state().await.authority, intruder.pubkey());
    assert_proposal_closed(&mut harness).await;
}

#[tokio::test]
async fn cancel_closes_the_proposal_to_the_authority_and_frees_the_address() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let initialized = harness.state().await;
    let rent = proposal_rent(&mut harness).await;
    harness.propose(&successor.pubkey()).await.unwrap();
    let authority_before = harness.lamports(&authority).await;

    harness.cancel().await.unwrap();

    assert_proposal_closed(&mut harness).await;
    assert_eq!(harness.lamports(&authority).await, authority_before + rent);
    assert_eq!(harness.state().await, initialized);
    assert_rejected(
        harness.accept(&successor).await,
        ClarionError::ProposalMissing,
    );

    harness.propose(&successor.pubkey()).await.unwrap();
    assert_eq!(harness.proposal().await, proposal_for(&successor.pubkey()));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
}

#[tokio::test]
async fn propose_completes_a_prefunded_proposal_address() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.pubkey();
    let rent = proposal_rent(&mut harness).await;
    harness.fund(&proposal_key(), rent - 1).await;
    let authority_before = harness.lamports(&authority).await;

    harness.propose(&successor).await.unwrap();

    assert_eq!(harness.proposal().await, proposal_for(&successor));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
    assert_eq!(harness.lamports(&authority).await, authority_before - 1);
}

#[tokio::test]
async fn authority_instructions_require_their_signature() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.pubkey();

    let mut unsigned = propose_instruction(&authority, &successor);
    unsigned.accounts[0].is_signer = false;
    assert_rejected(
        harness.send(unsigned, &[]).await,
        ClarionError::AuthoritySignatureMissing,
    );
    assert_eq!(harness.account(&proposal_key()).await, None);

    harness.propose(&successor).await.unwrap();
    for mut unsigned in [
        accept_instruction(&successor),
        cancel_instruction(&authority),
    ] {
        unsigned.accounts[0].is_signer = false;
        assert_rejected(
            harness.send(unsigned, &[]).await,
            ClarionError::AuthoritySignatureMissing,
        );
    }
    assert_eq!(harness.proposal().await, proposal_for(&successor));
    assert_eq!(harness.state().await.authority, authority);
}

#[tokio::test]
async fn propose_requires_the_stored_authority() {
    let mut harness = Harness::start_initialized().await;
    let intruder = harness.intruder.insecure_clone();

    assert_rejected(
        harness
            .send(
                propose_instruction(&intruder.pubkey(), &intruder.pubkey()),
                &[&intruder],
            )
            .await,
        ClarionError::AuthorityMismatch,
    );
    assert_eq!(harness.account(&proposal_key()).await, None);
}

#[tokio::test]
async fn propose_rejects_the_stored_authority_as_the_new_one() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();

    assert_rejected(
        harness.propose(&authority).await,
        ClarionError::AuthorityUnchanged,
    );
    assert_eq!(harness.account(&proposal_key()).await, None);
}

#[tokio::test]
async fn propose_rejects_a_substituted_system_program() {
    let mut harness = Harness::start_initialized().await;
    let mut propose =
        propose_instruction(&harness.authority.pubkey(), &harness.successor.pubkey());
    propose.accounts[3].pubkey = FOREIGN_PROGRAM_ID;

    assert_rejected(
        harness.send_as_authority(propose).await,
        ClarionError::SystemProgramMismatch,
    );
    assert_eq!(harness.account(&proposal_key()).await, None);
}

#[tokio::test]
async fn authority_instructions_reject_another_proposal_address() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();

    let mut propose = propose_instruction(&authority, &successor.pubkey());
    propose.accounts[2].pubkey = STRAY_PROPOSAL_KEY;
    assert_rejected(
        harness.send_as_authority(propose).await,
        ClarionError::ProposalAddressMismatch,
    );
    assert_eq!(harness.account(&STRAY_PROPOSAL_KEY).await, None);

    harness.propose(&successor.pubkey()).await.unwrap();
    let mut accept = accept_instruction(&successor.pubkey());
    accept.accounts[2].pubkey = STRAY_PROPOSAL_KEY;
    let mut cancel = cancel_instruction(&authority);
    cancel.accounts[2].pubkey = STRAY_PROPOSAL_KEY;
    for stray in [None, Some(proposal_data(&successor.pubkey()))] {
        if let Some(data) = stray {
            harness.set_account(&STRAY_PROPOSAL_KEY, data_account(&PROGRAM_ID, data));
        }
        assert_rejected(
            harness.send(accept.clone(), &[&successor]).await,
            ClarionError::ProposalAddressMismatch,
        );
        assert_rejected(
            harness.send_as_authority(cancel.clone()).await,
            ClarionError::ProposalAddressMismatch,
        );
    }

    assert_eq!(harness.state().await.authority, authority);
    assert_eq!(harness.proposal().await, proposal_for(&successor.pubkey()));
}

#[tokio::test]
async fn authority_instructions_reject_a_proposal_of_another_owner() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let foreign = data_account(&FOREIGN_PROGRAM_ID, proposal_data(&successor.pubkey()));
    harness.set_account(&proposal_key(), foreign.clone());

    assert_rejected(
        harness.propose(&successor.pubkey()).await,
        ClarionError::ProposalOwnerMismatch,
    );
    assert_rejected(
        harness.accept(&successor).await,
        ClarionError::ProposalOwnerMismatch,
    );
    assert_rejected(harness.cancel().await, ClarionError::ProposalOwnerMismatch);

    assert_eq!(harness.account(&proposal_key()).await, Some(foreign));
    assert_eq!(harness.state().await.authority, authority);
}

#[tokio::test]
async fn authority_instructions_reject_proposal_data_that_does_not_decode() {
    let mut harness = Harness::start_initialized().await;
    let successor = harness.successor.insecure_clone();
    let mut another_discriminator = proposal_data(&successor.pubkey());
    another_discriminator[0] = PROPOSAL_DISCRIMINATOR + 1;
    let truncated = proposal_data(&successor.pubkey())[..PROPOSAL_SIZE - 1].to_vec();
    let mut another_bump = proposal_for(&successor.pubkey());
    another_bump.bump = another_bump.bump.wrapping_sub(1);

    for (data, expected) in [
        (
            another_discriminator,
            ClarionError::ProposalDiscriminatorMismatch,
        ),
        (truncated, ClarionError::ProposalDataMalformed),
        (
            borsh::to_vec(&another_bump).unwrap(),
            ClarionError::ProposalAddressMismatch,
        ),
    ] {
        harness.set_account(&proposal_key(), data_account(&PROGRAM_ID, data));

        assert_rejected(harness.propose(&successor.pubkey()).await, expected);
        assert_rejected(harness.accept(&successor).await, expected);
        assert_rejected(harness.cancel().await, expected);
    }
    assert_eq!(harness.state().await.authority, harness.authority.pubkey());
}

#[tokio::test]
async fn accept_and_cancel_require_a_pending_proposal() {
    let mut harness = Harness::start_initialized().await;
    let successor = harness.successor.insecure_clone();
    let initialized = harness.state().await;

    assert_rejected(
        harness.accept(&successor).await,
        ClarionError::ProposalMissing,
    );
    assert_rejected(harness.cancel().await, ClarionError::ProposalMissing);

    let rent = proposal_rent(&mut harness).await;
    harness.fund(&proposal_key(), rent).await;
    assert_rejected(
        harness.accept(&successor).await,
        ClarionError::ProposalMissing,
    );
    assert_rejected(harness.cancel().await, ClarionError::ProposalMissing);
    assert_eq!(harness.state().await, initialized);
}

#[tokio::test]
async fn accept_requires_the_proposed_authority() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.insecure_clone();
    let successor = harness.successor.pubkey();
    let intruder = harness.intruder.insecure_clone();
    harness.propose(&successor).await.unwrap();

    for signer in [&intruder, &authority] {
        assert_rejected(
            harness.accept(signer).await,
            ClarionError::ProposedAuthorityMismatch,
        );
    }
    assert_eq!(harness.state().await.authority, authority.pubkey());
    assert_eq!(harness.proposal().await, proposal_for(&successor));
}

#[tokio::test]
async fn cancel_requires_the_stored_authority() {
    let mut harness = Harness::start_initialized().await;
    let successor = harness.successor.insecure_clone();
    let intruder = harness.intruder.insecure_clone();
    harness.propose(&successor.pubkey()).await.unwrap();

    for signer in [&intruder, &successor] {
        assert_rejected(
            harness
                .send(cancel_instruction(&signer.pubkey()), &[signer])
                .await,
            ClarionError::AuthorityMismatch,
        );
    }
    assert_eq!(harness.proposal().await, proposal_for(&successor.pubkey()));
}

#[tokio::test]
async fn authority_instructions_reject_an_account_list_of_another_length() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    harness.propose(&successor.pubkey()).await.unwrap();
    let extra = AccountMeta::new_readonly(FOREIGN_PROGRAM_ID, false);

    for (complete, signers) in [
        (
            propose_instruction(&authority, &successor.pubkey()),
            vec![harness.authority.insecure_clone()],
        ),
        (
            accept_instruction(&successor.pubkey()),
            vec![successor.insecure_clone()],
        ),
        (
            cancel_instruction(&authority),
            vec![harness.authority.insecure_clone()],
        ),
    ] {
        let signers: Vec<_> = signers.iter().collect();
        let mut short = complete.clone();
        short.accounts.pop();
        let mut long = complete;
        long.accounts.push(extra.clone());

        for miscounted in [short, long] {
            assert_rejected(
                harness.send(miscounted, &signers).await,
                ClarionError::AccountCountMismatch,
            );
        }
    }
    assert_eq!(harness.state().await.authority, authority);
    assert_eq!(harness.proposal().await, proposal_for(&successor.pubkey()));
    assert_eq!(
        harness.account(&state_key()).await.unwrap().owner,
        *PROGRAM_ID
    );
}

#[tokio::test]
async fn authority_instructions_reject_a_substituted_state() {
    let mut harness = Harness::start_initialized().await;
    let successor = harness.successor.insecure_clone();
    let intruder = harness.intruder.insecure_clone();
    harness.propose(&successor.pubkey()).await.unwrap();
    let initialized = harness.state().await;
    let pending = harness.account(&proposal_key()).await;
    let forged = borsh::to_vec(&State {
        authority: intruder.pubkey(),
        ..initialized
    })
    .unwrap();

    for (owner, expected) in [
        (FOREIGN_PROGRAM_ID, ClarionError::StateOwnerMismatch),
        (*PROGRAM_ID, ClarionError::StateAddressMismatch),
    ] {
        harness.set_account(&STRAY_STATE_KEY, data_account(&owner, forged.clone()));

        for (mut instruction, signer) in [
            (
                propose_instruction(&intruder.pubkey(), &ACCOMPLICE),
                &intruder,
            ),
            (accept_instruction(&successor.pubkey()), &successor),
            (cancel_instruction(&intruder.pubkey()), &intruder),
        ] {
            instruction.accounts[1].pubkey = STRAY_STATE_KEY;
            assert_rejected(harness.send(instruction, &[signer]).await, expected);
        }
        assert_eq!(harness.account(&proposal_key()).await, pending);
        assert_eq!(harness.state().await, initialized);
    }
}

#[tokio::test]
async fn accept_then_propose_in_one_transaction_recreates_the_proposal() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.pubkey();
    let successor = harness.successor.insecure_clone();
    let rent = proposal_rent(&mut harness).await;
    harness.propose(&successor.pubkey()).await.unwrap();
    let successor_before = harness.lamports(&successor.pubkey()).await;

    harness
        .send_all(
            &[
                accept_instruction(&successor.pubkey()),
                propose_instruction(&successor.pubkey(), &authority),
            ],
            &[&successor],
        )
        .await
        .unwrap();

    assert_eq!(harness.state().await.authority, successor.pubkey());
    assert_eq!(harness.proposal().await, proposal_for(&authority));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
    assert_eq!(
        harness.lamports(&successor.pubkey()).await,
        successor_before
    );
}

#[tokio::test]
async fn cancel_then_propose_in_one_transaction_recreates_the_proposal() {
    let mut harness = Harness::start_initialized().await;
    let authority = harness.authority.insecure_clone();
    let successor = harness.successor.pubkey();
    let intruder = harness.intruder.pubkey();
    let initialized = harness.state().await;
    let rent = proposal_rent(&mut harness).await;
    harness.propose(&successor).await.unwrap();
    let authority_before = harness.lamports(&authority.pubkey()).await;

    harness
        .send_all(
            &[
                cancel_instruction(&authority.pubkey()),
                propose_instruction(&authority.pubkey(), &intruder),
            ],
            &[&authority],
        )
        .await
        .unwrap();

    assert_eq!(harness.state().await, initialized);
    assert_eq!(harness.proposal().await, proposal_for(&intruder));
    assert_eq!(harness.lamports(&proposal_key()).await, rent);
    assert_eq!(
        harness.lamports(&authority.pubkey()).await,
        authority_before
    );
}
