use std::ops::Range;

use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_keypair::{Keypair, Signer};
use solana_program::{instruction::Instruction, pubkey::Pubkey};
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_transaction::{Hash, SignerError, Transaction};
use thiserror::Error;

pub const BUFFER_METADATA_SIZE: usize = 37;
pub const PACKET_DATA_SIZE: usize = 1280 - 40 - 8;
const BUFFER_STATE_TAG: u32 = 1;
const AUTHORITY_OFFSET: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderAccount {
    pub owner: Pubkey,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferMetadata {
    pub authority: Option<Pubkey>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BufferError {
    #[error("account {address} does not exist")]
    Absent { address: Pubkey },
    #[error(
        "account {address} is owned by {owner}, expected the upgradeable loader {}",
        bpf_loader_upgradeable::ID
    )]
    WrongOwner { address: Pubkey, owner: Pubkey },
    #[error(
        "account {address} holds {data_len} bytes, fewer than the \
         {BUFFER_METADATA_SIZE}-byte buffer metadata"
    )]
    Truncated { address: Pubkey, data_len: usize },
    #[error("account {address} is not a buffer: loader state tag {tag}")]
    NotABuffer { address: Pubkey, tag: u32 },
    #[error("buffer {address} carries authority option tag {tag}")]
    AuthorityTag { address: Pubkey, tag: u8 },
    #[error("buffer {address} has no authority and can no longer be written")]
    NoAuthority { address: Pubkey },
    #[error("buffer {address} authority is {stored}, the authority keypair holds {held}")]
    WrongAuthority {
        address: Pubkey,
        stored: Pubkey,
        held: Pubkey,
    },
    #[error(
        "buffer {address} holds {data_len} bytes, expected {BUFFER_METADATA_SIZE} + \
         program length {program_len} = {}",
        BUFFER_METADATA_SIZE + .program_len
    )]
    SizeMismatch {
        address: Pubkey,
        data_len: usize,
        program_len: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerLayout {
    FeePayerIsAuthority,
    SeparateFeePayer,
}

impl SignerLayout {
    pub fn of(authority: &Pubkey, fee_payer: &Pubkey) -> Self {
        if authority == fee_payer {
            Self::FeePayerIsAuthority
        } else {
            Self::SeparateFeePayer
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BufferStatus {
    pub buffer: String,
    pub authority: Option<String>,
    pub data_len: usize,
    pub program_len: usize,
    pub chunk_bytes: usize,
    pub chunks_total: usize,
    pub chunks_matching: usize,
    pub chunks_missing: usize,
    pub complete: bool,
    pub program_sha256: String,
    pub buffer_program_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteTransaction(Transaction);

impl WriteTransaction {
    pub fn new(
        buffer: &Pubkey,
        offset: u32,
        bytes: &[u8],
        authority: &Keypair,
        fee_payer: &Keypair,
        blockhash: Hash,
    ) -> Result<Self, SignerError> {
        let instruction =
            write_instruction(buffer, &authority.pubkey(), offset, bytes.to_vec());
        let mut transaction =
            Transaction::new_with_payer(&[instruction], Some(&fee_payer.pubkey()));
        let signers: Vec<&Keypair> =
            match SignerLayout::of(&authority.pubkey(), &fee_payer.pubkey()) {
                SignerLayout::FeePayerIsAuthority => vec![authority],
                SignerLayout::SeparateFeePayer => vec![fee_payer, authority],
            };
        transaction.try_sign(&signers, blockhash)?;
        Ok(Self(transaction))
    }

    pub fn transaction(&self) -> &Transaction {
        &self.0
    }

    pub fn wire_bytes(&self) -> Vec<u8> {
        wire_bytes(&self.0)
    }
}

pub fn write_instruction(
    buffer: &Pubkey,
    authority: &Pubkey,
    offset: u32,
    bytes: Vec<u8>,
) -> Instruction {
    solana_loader_v3_interface::instruction::write(buffer, authority, offset, bytes)
}

fn wire_bytes(transaction: &Transaction) -> Vec<u8> {
    bincode::serialize(transaction).unwrap_or_default()
}

fn unsigned_write_size(layout: SignerLayout, chunk_bytes: usize) -> usize {
    let buffer = Pubkey::new_from_array([1; 32]);
    let authority = Pubkey::new_from_array([2; 32]);
    let fee_payer = match layout {
        SignerLayout::FeePayerIsAuthority => authority,
        SignerLayout::SeparateFeePayer => Pubkey::new_from_array([3; 32]),
    };
    let instruction = write_instruction(&buffer, &authority, 0, vec![0; chunk_bytes]);
    let transaction = Transaction::new_with_payer(&[instruction], Some(&fee_payer));
    wire_bytes(&transaction).len()
}

pub fn max_chunk_bytes(layout: SignerLayout) -> usize {
    let mut chunk_bytes = PACKET_DATA_SIZE.saturating_sub(unsigned_write_size(layout, 0));
    while chunk_bytes > 0 && unsigned_write_size(layout, chunk_bytes) > PACKET_DATA_SIZE {
        chunk_bytes -= 1;
    }
    chunk_bytes
}

pub fn decode_buffer(
    address: &Pubkey,
    account: Option<&LoaderAccount>,
) -> Result<BufferMetadata, BufferError> {
    let account = account.ok_or(BufferError::Absent { address: *address })?;
    if account.owner != bpf_loader_upgradeable::ID {
        return Err(BufferError::WrongOwner {
            address: *address,
            owner: account.owner,
        });
    }
    let metadata = account.data.first_chunk::<BUFFER_METADATA_SIZE>().ok_or(
        BufferError::Truncated {
            address: *address,
            data_len: account.data.len(),
        },
    )?;
    let (tag, rest) = metadata.split_first_chunk::<4>().unwrap_or((&[0; 4], &[]));
    let tag = u32::from_le_bytes(*tag);
    if tag != BUFFER_STATE_TAG {
        return Err(BufferError::NotABuffer {
            address: *address,
            tag,
        });
    }
    let authority = match rest.first() {
        Some(0) => None,
        Some(1) => metadata
            .get(AUTHORITY_OFFSET..)
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .map(Pubkey::new_from_array),
        other => {
            return Err(BufferError::AuthorityTag {
                address: *address,
                tag: other.copied().unwrap_or_default(),
            });
        }
    };
    Ok(BufferMetadata { authority })
}

pub fn check_writable(
    address: &Pubkey,
    account: Option<&LoaderAccount>,
    program_len: usize,
    authority: &Pubkey,
) -> Result<(), BufferError> {
    let metadata = decode_buffer(address, account)?;
    let stored = metadata
        .authority
        .ok_or(BufferError::NoAuthority { address: *address })?;
    if stored != *authority {
        return Err(BufferError::WrongAuthority {
            address: *address,
            stored,
            held: *authority,
        });
    }
    let data_len = account.map_or(0, |account| account.data.len());
    if data_len != BUFFER_METADATA_SIZE + program_len {
        return Err(BufferError::SizeMismatch {
            address: *address,
            data_len,
            program_len,
        });
    }
    Ok(())
}

pub fn buffer_program(data: &[u8]) -> &[u8] {
    data.get(BUFFER_METADATA_SIZE..).unwrap_or_default()
}

pub fn chunk_ranges(
    program_len: usize,
    chunk_bytes: usize,
) -> impl Iterator<Item = Range<usize>> {
    (0..program_len)
        .step_by(chunk_bytes.max(1))
        .map(move |start| start..program_len.min(start + chunk_bytes.max(1)))
}

pub fn differing_chunks(
    program: &[u8],
    buffer_program: &[u8],
    chunk_bytes: usize,
) -> Vec<Range<usize>> {
    chunk_ranges(program.len(), chunk_bytes)
        .filter(|range| program.get(range.clone()) != buffer_program.get(range.clone()))
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn buffer_status(
    address: &Pubkey,
    account: Option<&LoaderAccount>,
    program: &[u8],
    chunk_bytes: usize,
) -> Result<BufferStatus, BufferError> {
    let metadata = decode_buffer(address, account)?;
    let data = account
        .map(|account| account.data.as_slice())
        .unwrap_or_default();
    let stored = buffer_program(data);
    let chunks_total = chunk_ranges(program.len(), chunk_bytes).count();
    let chunks_missing = differing_chunks(program, stored, chunk_bytes).len();
    Ok(BufferStatus {
        buffer: address.to_string(),
        authority: metadata.authority.map(|authority| authority.to_string()),
        data_len: data.len(),
        program_len: program.len(),
        chunk_bytes,
        chunks_total,
        chunks_matching: chunks_total - chunks_missing,
        chunks_missing,
        complete: chunks_missing == 0 && stored.len() == program.len(),
        program_sha256: sha256_hex(program),
        buffer_program_sha256: sha256_hex(stored),
    })
}

#[cfg(test)]
mod tests {
    use solana_loader_v3_interface::{
        instruction::UpgradeableLoaderInstruction, state::UpgradeableLoaderState,
    };
    use solana_program::instruction::AccountMeta;

    use super::*;

    const BLOCKHASH: Hash = Hash::new_from_array([5; 32]);

    fn buffer_key() -> Pubkey {
        Pubkey::new_from_array([7; 32])
    }

    fn authority() -> Keypair {
        Keypair::new_from_array([1; 32])
    }

    fn fee_payer() -> Keypair {
        Keypair::new_from_array([2; 32])
    }

    fn buffer_account(authority: Option<Pubkey>, program: &[u8]) -> LoaderAccount {
        let mut data = bincode::serialize(&UpgradeableLoaderState::Buffer {
            authority_address: authority,
        })
        .unwrap();
        data.resize(BUFFER_METADATA_SIZE, 0);
        data.extend_from_slice(program);
        LoaderAccount {
            owner: bpf_loader_upgradeable::ID,
            data,
        }
    }

    fn program(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 251) as u8 + 1).collect()
    }

    fn signed_size(layout: SignerLayout, chunk_bytes: usize) -> usize {
        let authority = authority();
        let fee_payer = match layout {
            SignerLayout::FeePayerIsAuthority => authority.insecure_clone(),
            SignerLayout::SeparateFeePayer => fee_payer(),
        };
        let transaction = WriteTransaction::new(
            &buffer_key(),
            u32::MAX,
            &vec![0xff; chunk_bytes],
            &authority,
            &fee_payer,
            BLOCKHASH,
        )
        .unwrap();
        bincode::serialize(transaction.transaction()).unwrap().len()
    }

    #[test]
    fn metadata_size_matches_the_loader_buffer_metadata() {
        assert_eq!(
            UpgradeableLoaderState::size_of_buffer_metadata(),
            BUFFER_METADATA_SIZE
        );
        assert_eq!(PACKET_DATA_SIZE, 1232);
    }

    #[test]
    fn max_chunk_fills_the_packet_for_both_signer_layouts() {
        for layout in [
            SignerLayout::FeePayerIsAuthority,
            SignerLayout::SeparateFeePayer,
        ] {
            let chunk_bytes = max_chunk_bytes(layout);

            assert!(chunk_bytes > 900, "{layout:?} {chunk_bytes}");
            assert_eq!(
                signed_size(layout, chunk_bytes),
                PACKET_DATA_SIZE,
                "{layout:?}"
            );
            assert!(signed_size(layout, chunk_bytes + 1) > PACKET_DATA_SIZE);
        }
    }

    #[test]
    fn separate_fee_payer_costs_one_signature_and_one_key() {
        assert_eq!(
            max_chunk_bytes(SignerLayout::FeePayerIsAuthority)
                - max_chunk_bytes(SignerLayout::SeparateFeePayer),
            64 + 32
        );
    }

    #[test]
    fn signer_layout_follows_key_equality() {
        let authority = authority().pubkey();

        assert_eq!(
            SignerLayout::of(&authority, &authority),
            SignerLayout::FeePayerIsAuthority
        );
        assert_eq!(
            SignerLayout::of(&authority, &fee_payer().pubkey()),
            SignerLayout::SeparateFeePayer
        );
    }

    #[test]
    fn write_instruction_follows_the_loader_v3_layout() {
        let authority = authority().pubkey();
        let bytes = vec![9, 8, 7];

        let instruction =
            write_instruction(&buffer_key(), &authority, 0x0102_0304, bytes);

        assert_eq!(instruction.program_id, bpf_loader_upgradeable::ID);
        assert_eq!(
            instruction.accounts,
            vec![
                AccountMeta::new(buffer_key(), false),
                AccountMeta::new_readonly(authority, true),
            ]
        );
        assert_eq!(
            instruction.data,
            [
                &1u32.to_le_bytes()[..],
                &0x0102_0304u32.to_le_bytes(),
                &3u64.to_le_bytes(),
                &[9, 8, 7],
            ]
            .concat()
        );
        assert_eq!(
            bincode::deserialize::<UpgradeableLoaderInstruction>(&instruction.data)
                .unwrap(),
            UpgradeableLoaderInstruction::Write {
                offset: 0x0102_0304,
                bytes: vec![9, 8, 7],
            }
        );
    }

    #[test]
    fn write_transaction_is_signed_by_fee_payer_then_authority() {
        let authority = authority();
        let fee_payer = fee_payer();

        let transaction = WriteTransaction::new(
            &buffer_key(),
            0,
            &[1],
            &authority,
            &fee_payer,
            BLOCKHASH,
        )
        .unwrap();

        let transaction = transaction.transaction();
        assert_eq!(transaction.signatures.len(), 2);
        assert_eq!(
            transaction.message.account_keys.get(..2),
            Some(&[fee_payer.pubkey(), authority.pubkey()][..])
        );
        assert!(transaction.verify().is_ok());
        assert_eq!(transaction.message.instructions.len(), 1);
    }

    #[test]
    fn write_transaction_with_one_key_carries_one_signature() {
        let authority = authority();

        let transaction = WriteTransaction::new(
            &buffer_key(),
            0,
            &[1],
            &authority,
            &authority,
            BLOCKHASH,
        )
        .unwrap();

        assert_eq!(transaction.transaction().signatures.len(), 1);
        assert!(transaction.transaction().verify().is_ok());
        assert_eq!(
            transaction.wire_bytes(),
            bincode::serialize(transaction.transaction()).unwrap()
        );
    }

    #[test]
    fn chunk_ranges_end_with_the_partial_chunk() {
        let ranges: Vec<_> = chunk_ranges(10, 4).collect();

        assert_eq!(ranges, vec![0..4, 4..8, 8..10]);
        assert_eq!(chunk_ranges(8, 4).count(), 2);
        assert_eq!(chunk_ranges(0, 4).count(), 0);
    }

    #[test]
    fn differing_chunks_selects_only_the_chunks_that_differ() {
        let program = program(10);
        let mut stored = program.clone();
        stored[5] = 0;
        stored[9] = 0;

        assert_eq!(differing_chunks(&program, &stored, 4), vec![4..8, 8..10]);
        assert_eq!(
            differing_chunks(&program, &program, 4),
            Vec::<Range<usize>>::new()
        );
    }

    #[test]
    fn differing_chunks_counts_the_last_partial_chunk_alone() {
        let program = program(10);
        let mut stored = program.clone();
        stored[9] = 0;

        assert_eq!(differing_chunks(&program, &stored, 4), vec![8..10]);
    }

    #[test]
    fn chunks_beyond_a_short_buffer_differ() {
        let program = program(10);

        assert_eq!(
            differing_chunks(&program, program.get(..6).unwrap(), 4),
            vec![4..8, 8..10]
        );
    }

    #[test]
    fn buffer_metadata_decodes_the_authority() {
        let authority = authority().pubkey();

        assert_eq!(
            decode_buffer(&buffer_key(), Some(&buffer_account(Some(authority), &[1]))),
            Ok(BufferMetadata {
                authority: Some(authority)
            })
        );
        assert_eq!(
            decode_buffer(&buffer_key(), Some(&buffer_account(None, &[1]))),
            Ok(BufferMetadata { authority: None })
        );
    }

    #[test]
    fn absent_account_is_refused() {
        assert_eq!(
            decode_buffer(&buffer_key(), None),
            Err(BufferError::Absent {
                address: buffer_key()
            })
        );
    }

    #[test]
    fn account_of_another_owner_is_refused() {
        let mut account = buffer_account(Some(authority().pubkey()), &[1]);
        account.owner = solana_sdk_ids::bpf_loader::ID;

        let error = decode_buffer(&buffer_key(), Some(&account)).unwrap_err();

        assert_eq!(
            error,
            BufferError::WrongOwner {
                address: buffer_key(),
                owner: solana_sdk_ids::bpf_loader::ID,
            }
        );
        assert!(
            error
                .to_string()
                .contains(&bpf_loader_upgradeable::ID.to_string())
        );
    }

    #[test]
    fn loader_state_other_than_buffer_is_refused() {
        for state in [
            UpgradeableLoaderState::Uninitialized,
            UpgradeableLoaderState::Program {
                programdata_address: buffer_key(),
            },
            UpgradeableLoaderState::ProgramData {
                slot: 1,
                upgrade_authority_address: None,
            },
        ] {
            let mut data = bincode::serialize(&state).unwrap();
            data.resize(UpgradeableLoaderState::size_of_programdata_metadata(), 0);
            let tag = u32::from_le_bytes(*data.first_chunk::<4>().unwrap());
            let account = LoaderAccount {
                owner: bpf_loader_upgradeable::ID,
                data,
            };

            assert_eq!(
                decode_buffer(&buffer_key(), Some(&account)),
                Err(BufferError::NotABuffer {
                    address: buffer_key(),
                    tag,
                })
            );
        }
    }

    #[test]
    fn account_shorter_than_the_metadata_is_refused() {
        let account = LoaderAccount {
            owner: bpf_loader_upgradeable::ID,
            data: vec![1, 0, 0, 0],
        };

        assert_eq!(
            decode_buffer(&buffer_key(), Some(&account)),
            Err(BufferError::Truncated {
                address: buffer_key(),
                data_len: 4,
            })
        );
    }

    #[test]
    fn authority_option_tag_other_than_zero_or_one_is_refused() {
        let mut account = buffer_account(Some(authority().pubkey()), &[1]);
        account.data[4] = 2;

        assert_eq!(
            decode_buffer(&buffer_key(), Some(&account)),
            Err(BufferError::AuthorityTag {
                address: buffer_key(),
                tag: 2,
            })
        );
    }

    #[test]
    fn writable_buffer_passes_every_check() {
        let authority = authority().pubkey();
        let program = program(10);

        assert_eq!(
            check_writable(
                &buffer_key(),
                Some(&buffer_account(Some(authority), &[0; 10])),
                program.len(),
                &authority,
            ),
            Ok(())
        );
    }

    #[test]
    fn buffer_of_another_authority_is_refused() {
        let held = authority().pubkey();
        let stored = fee_payer().pubkey();

        assert_eq!(
            check_writable(
                &buffer_key(),
                Some(&buffer_account(Some(stored), &[0; 10])),
                10,
                &held,
            ),
            Err(BufferError::WrongAuthority {
                address: buffer_key(),
                stored,
                held,
            })
        );
    }

    #[test]
    fn buffer_without_authority_is_refused() {
        assert_eq!(
            check_writable(
                &buffer_key(),
                Some(&buffer_account(None, &[0; 10])),
                10,
                &authority().pubkey(),
            ),
            Err(BufferError::NoAuthority {
                address: buffer_key()
            })
        );
    }

    #[test]
    fn buffer_of_another_size_is_refused_with_both_lengths() {
        let authority = authority().pubkey();

        for stored_len in [9, 11] {
            let error = check_writable(
                &buffer_key(),
                Some(&buffer_account(Some(authority), &vec![0; stored_len])),
                10,
                &authority,
            )
            .unwrap_err();

            assert_eq!(
                error,
                BufferError::SizeMismatch {
                    address: buffer_key(),
                    data_len: BUFFER_METADATA_SIZE + stored_len,
                    program_len: 10,
                }
            );
            assert!(
                error.to_string().ends_with(&format!(
                    "holds {} bytes, expected 37 + program length 10 = 47",
                    BUFFER_METADATA_SIZE + stored_len
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn writable_checks_owner_and_state_first() {
        let mut account = buffer_account(Some(authority().pubkey()), &[0; 10]);
        account.owner = solana_sdk_ids::system_program::ID;

        assert!(matches!(
            check_writable(&buffer_key(), Some(&account), 10, &authority().pubkey()),
            Err(BufferError::WrongOwner { .. })
        ));
    }

    #[test]
    fn status_counts_matching_and_missing_chunks() {
        let authority = authority().pubkey();
        let program = program(10);
        let mut stored = program.clone();
        stored[0] = 0;

        let status = buffer_status(
            &buffer_key(),
            Some(&buffer_account(Some(authority), &stored)),
            &program,
            4,
        )
        .unwrap();

        assert_eq!(
            status,
            BufferStatus {
                buffer: buffer_key().to_string(),
                authority: Some(authority.to_string()),
                data_len: 47,
                program_len: 10,
                chunk_bytes: 4,
                chunks_total: 3,
                chunks_matching: 2,
                chunks_missing: 1,
                complete: false,
                program_sha256: sha256_hex(&program),
                buffer_program_sha256: sha256_hex(&stored),
            }
        );
    }

    #[test]
    fn status_is_complete_only_for_an_exact_copy() {
        let program = program(10);
        let exact = buffer_account(None, &program);
        let longer = buffer_account(None, &[program.as_slice(), &[0]].concat());

        let exact = buffer_status(&buffer_key(), Some(&exact), &program, 4).unwrap();
        let longer = buffer_status(&buffer_key(), Some(&longer), &program, 4).unwrap();

        assert!(exact.complete);
        assert_eq!(exact.program_sha256, exact.buffer_program_sha256);
        assert!(!longer.complete);
        assert_eq!(longer.chunks_missing, 0);
    }

    #[test]
    fn sha256_hex_matches_the_known_digest_of_abc() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
