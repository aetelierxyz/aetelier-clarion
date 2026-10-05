use clarion::{
    ARM_COUNT, ArmAggregates, ClarionInstruction, state_address, window_address,
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use solana_sdk_ids::system_program;

pub fn init(
    program_id: &Pubkey,
    authority: &Pubkey,
    genesis_slot: u64,
    window_len: u64,
    min_reveal_lag_slots: u64,
) -> Instruction {
    Instruction::new_with_borsh(
        *program_id,
        &ClarionInstruction::Init {
            genesis_slot,
            window_len,
            min_reveal_lag_slots,
        },
        vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(state_address(program_id).0, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(*program_id, true),
        ],
    )
}

pub fn commit(
    program_id: &Pubkey,
    authority: &Pubkey,
    window_id: u64,
    slot_start: u64,
    slot_end: u64,
    merkle_root: [u8; 32],
) -> Instruction {
    Instruction::new_with_borsh(
        *program_id,
        &ClarionInstruction::Commit {
            window_id,
            slot_start,
            slot_end,
            merkle_root,
        },
        vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(state_address(program_id).0, false),
            AccountMeta::new(window_address(program_id, window_id).0, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    )
}

pub fn reveal(
    program_id: &Pubkey,
    authority: &Pubkey,
    window_id: u64,
    aggregates: [ArmAggregates; ARM_COUNT],
) -> Instruction {
    Instruction::new_with_borsh(
        *program_id,
        &ClarionInstruction::Reveal {
            window_id,
            aggregates,
        },
        vec![
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new_readonly(state_address(program_id).0, false),
            AccountMeta::new(window_address(program_id, window_id).0, false),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM_ID: Pubkey = Pubkey::new_from_array([42; 32]);
    const AUTHORITY: Pubkey = Pubkey::new_from_array([9; 32]);

    fn arm(seed: u32) -> ArmAggregates {
        ArmAggregates {
            tx_submitted: 100 * seed,
            tx_landed: 90 * seed,
            stl_p50_slots: 1,
            stl_p90_slots: 4,
            cu_price_p50_micro: 1_000,
            cu_price_p90_micro: 9_000,
            fee_total_lamports: 500_000,
            tip_total_lamports: 250_000,
        }
    }

    #[test]
    fn init_accounts_follow_the_program_order_and_privileges() {
        let instruction = init(&PROGRAM_ID, &AUTHORITY, 1_000, 150, 151);

        assert_eq!(instruction.program_id, PROGRAM_ID);
        assert_eq!(
            instruction.accounts,
            vec![
                AccountMeta {
                    pubkey: AUTHORITY,
                    is_signer: true,
                    is_writable: true,
                },
                AccountMeta {
                    pubkey: state_address(&PROGRAM_ID).0,
                    is_signer: false,
                    is_writable: true,
                },
                AccountMeta {
                    pubkey: system_program::ID,
                    is_signer: false,
                    is_writable: false,
                },
                AccountMeta {
                    pubkey: PROGRAM_ID,
                    is_signer: true,
                    is_writable: false,
                },
            ]
        );
        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Init {
                genesis_slot: 1_000,
                window_len: 150,
                min_reveal_lag_slots: 151,
            })
        );
    }

    #[test]
    fn commit_accounts_follow_the_program_order_and_privileges() {
        let instruction = commit(&PROGRAM_ID, &AUTHORITY, 4, 1_600, 1_749, [8; 32]);

        assert_eq!(instruction.program_id, PROGRAM_ID);
        assert_eq!(
            instruction.accounts,
            vec![
                AccountMeta {
                    pubkey: AUTHORITY,
                    is_signer: true,
                    is_writable: true,
                },
                AccountMeta {
                    pubkey: state_address(&PROGRAM_ID).0,
                    is_signer: false,
                    is_writable: true,
                },
                AccountMeta {
                    pubkey: window_address(&PROGRAM_ID, 4).0,
                    is_signer: false,
                    is_writable: true,
                },
                AccountMeta {
                    pubkey: system_program::ID,
                    is_signer: false,
                    is_writable: false,
                },
            ]
        );
        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Commit {
                window_id: 4,
                slot_start: 1_600,
                slot_end: 1_749,
                merkle_root: [8; 32],
            })
        );
    }

    #[test]
    fn reveal_accounts_follow_the_program_order_and_privileges() {
        let aggregates = [arm(1), arm(2), arm(3), arm(4)];

        let instruction = reveal(&PROGRAM_ID, &AUTHORITY, 4, aggregates);

        assert_eq!(instruction.program_id, PROGRAM_ID);
        assert_eq!(
            instruction.accounts,
            vec![
                AccountMeta {
                    pubkey: AUTHORITY,
                    is_signer: true,
                    is_writable: false,
                },
                AccountMeta {
                    pubkey: state_address(&PROGRAM_ID).0,
                    is_signer: false,
                    is_writable: false,
                },
                AccountMeta {
                    pubkey: window_address(&PROGRAM_ID, 4).0,
                    is_signer: false,
                    is_writable: true,
                },
            ]
        );
        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Reveal {
                window_id: 4,
                aggregates,
            })
        );
    }

    #[test]
    fn window_account_follows_the_window_id() {
        let fifth = commit(&PROGRAM_ID, &AUTHORITY, 5, 0, 0, [0; 32]);
        let sixth = reveal(&PROGRAM_ID, &AUTHORITY, 6, [arm(1); ARM_COUNT]);

        assert_eq!(
            fifth.accounts.get(2).map(|meta| meta.pubkey),
            Some(window_address(&PROGRAM_ID, 5).0)
        );
        assert_eq!(
            sixth.accounts.get(2).map(|meta| meta.pubkey),
            Some(window_address(&PROGRAM_ID, 6).0)
        );
    }
}
