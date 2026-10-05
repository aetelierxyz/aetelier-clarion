use thiserror::Error;

pub const MERKLE_ROOT_HEX_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RootError {
    #[error("merkle root holds {actual} characters, expected {MERKLE_ROOT_HEX_LEN} hex")]
    Length { actual: usize },
    #[error("merkle root character {character:?} at position {position} is not hex")]
    Character { position: usize, character: char },
}

pub fn parse_merkle_root(text: &str) -> Result<[u8; 32], RootError> {
    let actual = text.chars().count();
    if actual != MERKLE_ROOT_HEX_LEN {
        return Err(RootError::Length { actual });
    }
    let nibbles = text
        .chars()
        .enumerate()
        .map(|(position, character)| {
            character
                .to_digit(16)
                .and_then(|digit| u8::try_from(digit).ok())
                .ok_or(RootError::Character {
                    position,
                    character,
                })
        })
        .collect::<Result<Vec<u8>, RootError>>()?;
    let (pairs, _) = nibbles.as_chunks::<2>();
    let mut root = [0u8; 32];
    for (byte, [high, low]) in root.iter_mut().zip(pairs) {
        *byte = high << 4 | low;
    }
    Ok(root)
}

pub fn merkle_root_hex(root: &[u8; 32]) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCENDING_HEX: &str =
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn ascending() -> [u8; 32] {
        std::array::from_fn(|index| u8::try_from(index).unwrap())
    }

    #[test]
    fn parses_sixty_four_lowercase_hex_characters() {
        assert_eq!(parse_merkle_root(ASCENDING_HEX), Ok(ascending()));
        assert_eq!(parse_merkle_root(&"ff".repeat(32)), Ok([0xff; 32]));
        assert_eq!(parse_merkle_root(&"0".repeat(64)), Ok([0; 32]));
    }

    #[test]
    fn parses_uppercase_and_mixed_case() {
        assert_eq!(
            parse_merkle_root(&ASCENDING_HEX.to_uppercase()),
            Ok(ascending())
        );
        assert_eq!(parse_merkle_root(&"aB".repeat(32)), Ok([0xab; 32]));
    }

    #[test]
    fn high_nibble_comes_first() {
        let text = format!("{}{}", "a1", "0".repeat(62));
        let mut expected = [0u8; 32];
        expected[0] = 0xa1;

        assert_eq!(parse_merkle_root(&text), Ok(expected));
    }

    #[test]
    fn rejects_another_length() {
        for actual in [0, 1, 63, 65, 128] {
            assert_eq!(
                parse_merkle_root(&"a".repeat(actual)),
                Err(RootError::Length { actual })
            );
        }
    }

    #[test]
    fn rejects_a_prefixed_root() {
        let prefixed = format!("0x{ASCENDING_HEX}");

        assert_eq!(
            parse_merkle_root(&prefixed),
            Err(RootError::Length { actual: 66 })
        );
    }

    #[test]
    fn rejects_a_character_outside_hex_and_reports_its_position() {
        let mut text = "0".repeat(64);
        text.replace_range(10..11, "g");

        assert_eq!(
            parse_merkle_root(&text),
            Err(RootError::Character {
                position: 10,
                character: 'g',
            })
        );
        assert_eq!(
            parse_merkle_root(&format!(" {}", "0".repeat(63))),
            Err(RootError::Character {
                position: 0,
                character: ' ',
            })
        );
    }

    #[test]
    fn rejects_sixty_four_characters_that_are_not_ascii() {
        let text = format!("{}é", "0".repeat(63));

        assert_eq!(
            parse_merkle_root(&text),
            Err(RootError::Character {
                position: 63,
                character: 'é',
            })
        );
    }

    #[test]
    fn errors_render_on_one_line() {
        assert_eq!(
            RootError::Length { actual: 3 }.to_string(),
            "merkle root holds 3 characters, expected 64 hex"
        );
        assert_eq!(
            RootError::Character {
                position: 10,
                character: 'g',
            }
            .to_string(),
            "merkle root character 'g' at position 10 is not hex"
        );
    }

    #[test]
    fn hex_rendering_is_lowercase_and_zero_padded() {
        assert_eq!(merkle_root_hex(&ascending()), ASCENDING_HEX);
        assert_eq!(merkle_root_hex(&[0xAB; 32]), "ab".repeat(32));
    }

    #[test]
    fn rendering_and_parsing_round_trip() {
        for root in [[0u8; 32], [0xff; 32], ascending()] {
            assert_eq!(parse_merkle_root(&merkle_root_hex(&root)), Ok(root));
        }
    }
}
