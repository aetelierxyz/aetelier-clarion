use solana_keypair::{Keypair, Signer};
use solana_program::instruction::Instruction;
use solana_transaction::{Hash, SignerError, Transaction};

pub fn authority_transaction(
    instruction: Instruction,
    authority: &Keypair,
    cosigners: &[&Keypair],
    blockhash: Hash,
) -> Result<Transaction, SignerError> {
    let mut transaction =
        Transaction::new_with_payer(&[instruction], Some(&authority.pubkey()));
    let signers: Vec<&Keypair> = std::iter::once(authority)
        .chain(cosigners.iter().copied())
        .collect();
    transaction.try_sign(&signers, blockhash)?;
    Ok(transaction)
}

#[cfg(test)]
mod tests {
    use clarion::{ARM_COUNT, ArmAggregates};

    use super::*;
    use crate::instruction::{commit, init, reveal};

    const BLOCKHASH: Hash = Hash::new_from_array([3; 32]);

    fn authority() -> Keypair {
        Keypair::new_from_array([1; 32])
    }

    fn program() -> Keypair {
        Keypair::new_from_array([42; 32])
    }

    fn assert_signed_in_key_order(transaction: &Transaction, signers: &[&Keypair]) {
        let message = transaction.message_data();
        let expected: Vec<_> = signers
            .iter()
            .map(|signer| signer.sign_message(&message))
            .collect();

        assert_eq!(transaction.signatures, expected);
        assert_eq!(
            transaction.message.account_keys.get(..signers.len()),
            Some(
                signers
                    .iter()
                    .map(|signer| signer.pubkey())
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );
        assert_eq!(transaction.message.recent_blockhash, BLOCKHASH);
    }

    #[test]
    fn authority_pays_for_and_alone_signs_commit() {
        let authority = authority();
        let instruction =
            commit(&program().pubkey(), &authority.pubkey(), 4, 40, 49, [8; 32]);

        let transaction =
            authority_transaction(instruction.clone(), &authority, &[], BLOCKHASH)
                .unwrap();

        assert_signed_in_key_order(&transaction, &[&authority]);
        assert_eq!(transaction.message.header.num_required_signatures, 1);
        assert_eq!(transaction.message.header.num_readonly_signed_accounts, 0);
        assert_eq!(transaction.message.instructions.len(), 1);
        assert_eq!(
            transaction
                .message
                .instructions
                .first()
                .map(|compiled| &compiled.data),
            Some(&instruction.data)
        );
    }

    #[test]
    fn authority_pays_for_reveal_although_the_instruction_reads_it() {
        let authority = authority();
        let instruction = reveal(
            &program().pubkey(),
            &authority.pubkey(),
            4,
            [ArmAggregates::default(); ARM_COUNT],
        );
        assert_eq!(
            instruction.accounts.first().map(|meta| meta.is_writable),
            Some(false)
        );

        let transaction =
            authority_transaction(instruction, &authority, &[], BLOCKHASH).unwrap();

        assert_signed_in_key_order(&transaction, &[&authority]);
        assert_eq!(transaction.message.header.num_required_signatures, 1);
        assert_eq!(transaction.message.header.num_readonly_signed_accounts, 0);
    }

    #[test]
    fn init_carries_the_program_signature_after_the_authority() {
        let authority = authority();
        let program = program();
        let instruction = init(&program.pubkey(), &authority.pubkey(), 1_000, 150, 151);

        let transaction =
            authority_transaction(instruction, &authority, &[&program], BLOCKHASH)
                .unwrap();

        assert_signed_in_key_order(&transaction, &[&authority, &program]);
        assert_eq!(transaction.message.header.num_required_signatures, 2);
        assert_eq!(transaction.message.header.num_readonly_signed_accounts, 1);
    }

    #[test]
    fn init_without_the_program_keypair_is_not_signed() {
        let authority = authority();
        let instruction = init(&program().pubkey(), &authority.pubkey(), 1_000, 150, 151);

        assert_eq!(
            authority_transaction(instruction, &authority, &[], BLOCKHASH),
            Err(SignerError::NotEnoughSigners)
        );
    }

    #[test]
    fn cosigner_the_instruction_does_not_name_is_rejected() {
        let authority = authority();
        let program = program();
        let instruction =
            commit(&program.pubkey(), &authority.pubkey(), 4, 40, 49, [8; 32]);

        assert_eq!(
            authority_transaction(instruction, &authority, &[&program], BLOCKHASH),
            Err(SignerError::KeypairPubkeyMismatch)
        );
    }
}
