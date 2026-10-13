use borsh::{BorshDeserialize, BorshSerialize};
use clarion::Arm;
use solana_keypair::Signature;
use solana_program::hash::hashv;
use thiserror::Error;

pub const LEAF_PREFIX: u8 = 0x00;
pub const NODE_PREFIX: u8 = 0x01;
pub const EMPTY_PREFIX: u8 = 0x02;

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultRow {
    pub signature: [u8; 64],
    pub arm: Arm,
    pub target_slot: u64,
    pub landed_slot: u64,
    pub cu_price_micro: u64,
    pub fee_lamports: u64,
    pub tip_lamports: u64,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("result rows hold signature {signature} more than once")]
pub struct DuplicateSignature {
    pub signature: Signature,
}

pub fn leaf_hash(row: &ResultRow) -> [u8; 32] {
    hashv(&[
        &[LEAF_PREFIX],
        &row.signature,
        &[row.arm as u8],
        &row.target_slot.to_le_bytes(),
        &row.landed_slot.to_le_bytes(),
        &row.cu_price_micro.to_le_bytes(),
        &row.fee_lamports.to_le_bytes(),
        &row.tip_lamports.to_le_bytes(),
    ])
    .to_bytes()
}

pub fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    hashv(&[&[NODE_PREFIX], left, right]).to_bytes()
}

pub fn empty_root() -> [u8; 32] {
    hashv(&[&[EMPTY_PREFIX]]).to_bytes()
}

pub fn merkle_root(rows: &[ResultRow]) -> Result<[u8; 32], DuplicateSignature> {
    let mut ordered: Vec<&ResultRow> = rows.iter().collect();
    ordered.sort_unstable_by_key(|row| row.signature);
    let repeated = ordered.windows(2).find_map(|pair| match pair {
        [first, second] if first.signature == second.signature => Some(first.signature),
        _ => None,
    });
    if let Some(signature) = repeated {
        return Err(DuplicateSignature {
            signature: Signature::from(signature),
        });
    }
    let mut level: Vec<[u8; 32]> = ordered.into_iter().map(leaf_hash).collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .filter_map(|pair| {
                pair.iter()
                    .copied()
                    .reduce(|left, right| node_hash(&left, &right))
            })
            .collect();
    }
    Ok(level.first().copied().unwrap_or_else(empty_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root::parse_merkle_root;

    const EMPTY_ROOT_HEX: &str =
        "dbc1b4c900ffe48d575b5da5c638040125f65db0fe3e24494b76ea986457d986";
    const FIRST_LEAF_HEX: &str =
        "e702d72639b9f2cac76f092231fd77a2d91e31b6a7190139e54a76dea32fda58";
    const THREE_ROW_ROOT_HEX: &str =
        "6bacd17f3ce30b022d003b03c83556842437db305d59cab13c00d78662961ea1";
    const FIVE_ROW_ROOT_HEX: &str =
        "b8d3da23094e821f07b13dbcc0d71fc9008d98be0e8d9163177d6e237e396f48";

    fn rows() -> [ResultRow; 5] {
        [
            ResultRow {
                signature: [1; 64],
                arm: Arm::FanOut,
                target_slot: 1_000,
                landed_slot: 1_002,
                cu_price_micro: 0,
                fee_lamports: 5_000,
                tip_lamports: 0,
            },
            ResultRow {
                signature: [2; 64],
                arm: Arm::RpcDefault,
                target_slot: 1_000,
                landed_slot: 0,
                cu_price_micro: 0,
                fee_lamports: 5_000,
                tip_lamports: 0,
            },
            ResultRow {
                signature: [3; 64],
                arm: Arm::FixedPriorityFee,
                target_slot: 1_001,
                landed_slot: 1_003,
                cu_price_micro: 10_000,
                fee_lamports: 5_600,
                tip_lamports: 0,
            },
            ResultRow {
                signature: [4; 64],
                arm: Arm::TipLane,
                target_slot: 1_001,
                landed_slot: 1_004,
                cu_price_micro: 0,
                fee_lamports: 5_000,
                tip_lamports: 10_000,
            },
            ResultRow {
                signature: [5; 64],
                arm: Arm::FanOut,
                target_slot: 1_002,
                landed_slot: 1_002,
                cu_price_micro: 0,
                fee_lamports: 5_000,
                tip_lamports: 0,
            },
        ]
    }

    fn hex(text: &str) -> [u8; 32] {
        parse_merkle_root(text).unwrap()
    }

    #[test]
    fn empty_window_root_is_the_sha256_of_0x02() {
        assert_eq!(empty_root(), hex(EMPTY_ROOT_HEX));
        assert_eq!(merkle_root(&[]), Ok(hex(EMPTY_ROOT_HEX)));
    }

    #[test]
    fn leaf_is_the_sha256_of_0x00_and_the_borsh_row() {
        let [first, ..] = rows();
        let encoded = borsh::to_vec(&first).unwrap();

        assert_eq!(encoded.len(), 105);
        assert_eq!(encoded[64], 0);
        assert_eq!(
            leaf_hash(&first),
            hashv(&[&[LEAF_PREFIX], &encoded]).to_bytes()
        );
        assert_eq!(leaf_hash(&first), hex(FIRST_LEAF_HEX));
    }

    #[test]
    fn single_row_root_is_its_leaf() {
        let [first, ..] = rows();

        assert_eq!(merkle_root(&[first]), Ok(leaf_hash(&first)));
    }

    #[test]
    fn three_rows_promote_the_odd_node_unchanged() {
        let [first, second, third, ..] = rows();
        let expected = node_hash(
            &node_hash(&leaf_hash(&first), &leaf_hash(&second)),
            &leaf_hash(&third),
        );

        let root = merkle_root(&[third, first, second]);

        assert_eq!(root, Ok(expected));
        assert_eq!(root, Ok(hex(THREE_ROW_ROOT_HEX)));
    }

    #[test]
    fn five_rows_match_the_reference_root() {
        let [first, second, third, fourth, fifth] = rows();

        assert_eq!(
            merkle_root(&[fifth, third, first, fourth, second]),
            Ok(hex(FIVE_ROW_ROOT_HEX))
        );
    }

    #[test]
    fn root_does_not_depend_on_row_order() {
        let ascending = rows();
        let mut descending = rows();
        descending.reverse();
        let mut rotated = rows();
        rotated.rotate_left(2);

        for reordered in [descending, rotated] {
            assert_eq!(merkle_root(&reordered), merkle_root(&ascending));
        }
    }

    #[test]
    fn every_row_field_changes_the_root() {
        let original = merkle_root(&rows()).unwrap();
        let edits: [fn(&mut ResultRow); 7] = [
            |row| row.signature[63] ^= 1,
            |row| row.arm = Arm::TipLane,
            |row| row.target_slot += 1,
            |row| row.landed_slot += 1,
            |row| row.cu_price_micro += 1,
            |row| row.fee_lamports += 1,
            |row| row.tip_lamports += 1,
        ];

        for edit in edits {
            let mut edited = rows();
            edit(&mut edited[2]);

            assert_ne!(merkle_root(&edited), Ok(original));
        }
    }

    #[test]
    fn rejects_a_repeated_signature() {
        let [first, second, ..] = rows();
        let repeated = ResultRow {
            fee_lamports: 7_000,
            ..first
        };

        let error = merkle_root(&[first, second, repeated]).unwrap_err();

        assert_eq!(error.signature, Signature::from([1; 64]));
        assert_eq!(
            error.to_string(),
            format!(
                "result rows hold signature {} more than once",
                Signature::from([1; 64])
            )
        );
    }
}
