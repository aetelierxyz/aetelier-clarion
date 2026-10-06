use std::fmt;

use clarion::{ARM_COUNT, ArmAggregates, Window};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use solana_program::pubkey::Pubkey;
use thiserror::Error;

use crate::root::merkle_root_hex;

#[derive(Debug, Error)]
pub enum JsonError {
    #[error("aggregates JSON does not parse: {0}")]
    Aggregates(#[source] serde_json::Error),
    #[error("aggregates JSON holds {actual} arms, expected {expected}")]
    ArmCount { expected: usize, actual: usize },
    #[error("window does not render as JSON: {0}")]
    Window(#[source] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ArmAggregates", deny_unknown_fields)]
struct ArmFields {
    tx_submitted: u32,
    tx_landed: u32,
    stl_p50_slots: u16,
    stl_p90_slots: u16,
    cu_price_p50_micro: u64,
    cu_price_p90_micro: u64,
    fee_total_lamports: u64,
    tip_total_lamports: u64,
}

#[derive(Serialize)]
#[serde(transparent)]
struct Arm(#[serde(with = "ArmFields")] ArmAggregates);

struct ArmVisitor;

impl<'de> Visitor<'de> for ArmVisitor {
    type Value = Arm;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "an arm object with named fields, positional arrays are not accepted",
        )
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Arm, A::Error> {
        ArmFields::deserialize(MapAccessDeserializer::new(map)).map(Arm)
    }
}

impl<'de> Deserialize<'de> for Arm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ArmVisitor)
    }
}

#[derive(Serialize)]
struct WindowFields {
    address: String,
    discriminator: u8,
    window_id: u64,
    slot_start: u64,
    slot_end: u64,
    merkle_root: String,
    commit_slot: u64,
    reveal_slot: u64,
    aggregates: [Arm; ARM_COUNT],
    bump: u8,
}

pub fn parse_aggregates(text: &str) -> Result<[ArmAggregates; ARM_COUNT], JsonError> {
    let arms: Vec<ArmAggregates> = serde_json::from_str::<Vec<Arm>>(text)
        .map_err(JsonError::Aggregates)?
        .into_iter()
        .map(|arm| arm.0)
        .collect();
    <[ArmAggregates; ARM_COUNT]>::try_from(arms).map_err(|arms| JsonError::ArmCount {
        expected: ARM_COUNT,
        actual: arms.len(),
    })
}

pub fn render_window(address: &Pubkey, window: &Window) -> Result<String, JsonError> {
    let fields = WindowFields {
        address: address.to_string(),
        discriminator: window.discriminator,
        window_id: window.window_id,
        slot_start: window.slot_start,
        slot_end: window.slot_end,
        merkle_root: merkle_root_hex(&window.merkle_root),
        commit_slot: window.commit_slot,
        reveal_slot: window.reveal_slot,
        aggregates: window.aggregates.map(Arm),
        bump: window.bump,
    };
    serde_json::to_string_pretty(&fields).map_err(JsonError::Window)
}

#[cfg(test)]
mod tests {
    use clarion::WINDOW_DISCRIMINATOR;
    use serde_json::{Value, json};

    use super::*;

    fn arm(seed: u32) -> ArmAggregates {
        ArmAggregates {
            tx_submitted: 100 + seed,
            tx_landed: 90 + seed,
            stl_p50_slots: 1,
            stl_p90_slots: 4,
            cu_price_p50_micro: 1_000 + u64::from(seed),
            cu_price_p90_micro: 9_000 + u64::from(seed),
            fee_total_lamports: 500_000 + u64::from(seed),
            tip_total_lamports: 250_000 + u64::from(seed),
        }
    }

    fn arm_json(seed: u32) -> Value {
        json!({
            "tx_submitted": 100 + seed,
            "tx_landed": 90 + seed,
            "stl_p50_slots": 1,
            "stl_p90_slots": 4,
            "cu_price_p50_micro": 1_000 + seed,
            "cu_price_p90_micro": 9_000 + seed,
            "fee_total_lamports": 500_000 + seed,
            "tip_total_lamports": 250_000 + seed,
        })
    }

    fn arms_json(count: u32) -> String {
        Value::Array((1..=count).map(arm_json).collect()).to_string()
    }

    fn window() -> Window {
        Window {
            discriminator: WINDOW_DISCRIMINATOR,
            window_id: 2,
            slot_start: 1_300,
            slot_end: 1_449,
            merkle_root: [0x0a; 32],
            commit_slot: 1_500,
            reveal_slot: 1_651,
            aggregates: [arm(1), arm(2), arm(3), arm(4)],
            bump: 253,
        }
    }

    #[test]
    fn parses_four_arms_in_array_order() {
        let aggregates = parse_aggregates(&arms_json(4)).unwrap();

        assert_eq!(aggregates, [arm(1), arm(2), arm(3), arm(4)]);
    }

    #[test]
    fn parses_each_field_into_its_own_slot() {
        let text = r#"[
            {"tx_submitted": 1, "tx_landed": 2, "stl_p50_slots": 3, "stl_p90_slots": 4,
             "cu_price_p50_micro": 5, "cu_price_p90_micro": 6,
             "fee_total_lamports": 7, "tip_total_lamports": 8},
            {"tip_total_lamports": 18, "fee_total_lamports": 17,
             "cu_price_p90_micro": 16, "cu_price_p50_micro": 15,
             "stl_p90_slots": 14, "stl_p50_slots": 13, "tx_landed": 12, "tx_submitted": 11},
            {"tx_submitted": 0, "tx_landed": 0, "stl_p50_slots": 0, "stl_p90_slots": 0,
             "cu_price_p50_micro": 0, "cu_price_p90_micro": 0,
             "fee_total_lamports": 0, "tip_total_lamports": 0},
            {"tx_submitted": 4294967295, "tx_landed": 4294967295,
             "stl_p50_slots": 65535, "stl_p90_slots": 65535,
             "cu_price_p50_micro": 18446744073709551615,
             "cu_price_p90_micro": 18446744073709551615,
             "fee_total_lamports": 18446744073709551615,
             "tip_total_lamports": 18446744073709551615}
        ]"#;

        let aggregates = parse_aggregates(text).unwrap();

        assert_eq!(
            aggregates,
            [
                ArmAggregates {
                    tx_submitted: 1,
                    tx_landed: 2,
                    stl_p50_slots: 3,
                    stl_p90_slots: 4,
                    cu_price_p50_micro: 5,
                    cu_price_p90_micro: 6,
                    fee_total_lamports: 7,
                    tip_total_lamports: 8,
                },
                ArmAggregates {
                    tx_submitted: 11,
                    tx_landed: 12,
                    stl_p50_slots: 13,
                    stl_p90_slots: 14,
                    cu_price_p50_micro: 15,
                    cu_price_p90_micro: 16,
                    fee_total_lamports: 17,
                    tip_total_lamports: 18,
                },
                ArmAggregates::default(),
                ArmAggregates {
                    tx_submitted: u32::MAX,
                    tx_landed: u32::MAX,
                    stl_p50_slots: u16::MAX,
                    stl_p90_slots: u16::MAX,
                    cu_price_p50_micro: u64::MAX,
                    cu_price_p90_micro: u64::MAX,
                    fee_total_lamports: u64::MAX,
                    tip_total_lamports: u64::MAX,
                },
            ]
        );
    }

    #[test]
    fn rejects_any_arm_count_other_than_four() {
        for actual in [0, 1, 3, 5, 8] {
            let error = parse_aggregates(&arms_json(actual)).unwrap_err();

            assert!(
                matches!(
                    error,
                    JsonError::ArmCount { expected: 4, actual: found }
                        if u32::try_from(found) == Ok(actual)
                ),
                "{error}"
            );
        }
    }

    #[test]
    fn arm_count_error_names_both_counts() {
        assert_eq!(
            parse_aggregates(&arms_json(3)).unwrap_err().to_string(),
            "aggregates JSON holds 3 arms, expected 4"
        );
    }

    #[test]
    fn landed_above_submitted_passes_parsing() {
        let mut arms: Vec<Value> = (1..=4).map(arm_json).collect();
        arms[2]["tx_submitted"] = json!(10);
        arms[2]["tx_landed"] = json!(11);

        let aggregates = parse_aggregates(&Value::Array(arms).to_string()).unwrap();

        assert_eq!(aggregates[2].tx_submitted, 10);
        assert_eq!(aggregates[2].tx_landed, 11);
    }

    #[test]
    fn rejects_a_missing_field() {
        for field in [
            "tx_submitted",
            "tx_landed",
            "stl_p50_slots",
            "stl_p90_slots",
            "cu_price_p50_micro",
            "cu_price_p90_micro",
            "fee_total_lamports",
            "tip_total_lamports",
        ] {
            let mut arms: Vec<Value> = (1..=4).map(arm_json).collect();
            arms[1].as_object_mut().unwrap().remove(field).unwrap();

            let error = parse_aggregates(&Value::Array(arms).to_string()).unwrap_err();

            assert!(matches!(error, JsonError::Aggregates(_)), "{error}");
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[test]
    fn rejects_an_unknown_field() {
        let mut arms: Vec<Value> = (1..=4).map(arm_json).collect();
        arms[0]["landing_probability"] = json!(1);

        let error = parse_aggregates(&Value::Array(arms).to_string()).unwrap_err();

        assert!(matches!(error, JsonError::Aggregates(_)), "{error}");
        assert!(error.to_string().contains("landing_probability"), "{error}");
    }

    #[test]
    fn rejects_values_outside_the_field_width() {
        for (field, value) in [
            ("tx_submitted", json!(4_294_967_296u64)),
            ("stl_p90_slots", json!(65_536)),
            ("fee_total_lamports", json!(-1)),
            ("tip_total_lamports", json!(1.5)),
            ("cu_price_p50_micro", json!("7")),
        ] {
            let mut arms: Vec<Value> = (1..=4).map(arm_json).collect();
            arms[3][field] = value;

            let error = parse_aggregates(&Value::Array(arms).to_string()).unwrap_err();

            assert!(
                matches!(error, JsonError::Aggregates(_)),
                "{field}: {error}"
            );
        }
    }

    #[test]
    fn rejects_text_that_is_not_an_array_of_objects() {
        for text in [
            "",
            "{}",
            "null",
            "[1, 2, 3, 4]",
            "[[], [], [], []]",
            "[[1, 2, 3, 4, 5, 6, 7, 8], [1, 2, 3, 4, 5, 6, 7, 8], \
             [1, 2, 3, 4, 5, 6, 7, 8], [1, 2, 3, 4, 5, 6, 7, 8]]",
            "not json",
        ] {
            let error = parse_aggregates(text).unwrap_err();

            assert!(matches!(error, JsonError::Aggregates(_)), "{text}: {error}");
        }
    }

    #[test]
    fn positional_arm_is_rejected_as_a_sequence_with_its_position() {
        let mut arms: Vec<Value> = (1..=4).map(arm_json).collect();
        arms[2] = json!([103, 93, 1, 4, 1_003, 9_003, 500_003, 250_003]);

        let error = parse_aggregates(&Value::Array(arms).to_string()).unwrap_err();

        assert!(matches!(error, JsonError::Aggregates(_)), "{error}");
        assert!(
            error.to_string().contains(
                "invalid type: sequence, expected an arm object with named fields, \
                 positional arrays are not accepted"
            ),
            "{error}"
        );
        assert!(error.to_string().contains("line 1 column"), "{error}");
    }

    #[test]
    fn rejects_a_duplicate_field() {
        let text = arms_json(4).replacen(
            "\"tx_landed\":91",
            "\"tx_landed\":91,\"tx_landed\":92",
            1,
        );
        assert_ne!(text, arms_json(4));

        let error = parse_aggregates(&text).unwrap_err();

        assert!(matches!(error, JsonError::Aggregates(_)), "{error}");
        assert!(
            error.to_string().contains("duplicate field `tx_landed`"),
            "{error}"
        );
    }

    #[test]
    fn parse_errors_render_on_one_line() {
        let error =
            parse_aggregates("[\n  {\n    \"tx_submitted\": 1\n  }\n]").unwrap_err();

        assert!(!error.to_string().contains('\n'), "{error}");
    }

    #[test]
    fn renders_every_window_field_with_base58_address_and_hex_root() {
        let address = Pubkey::new_from_array([0; 32]);

        let rendered: Value =
            serde_json::from_str(&render_window(&address, &window()).unwrap()).unwrap();

        assert_eq!(
            rendered,
            json!({
                "address": "11111111111111111111111111111111",
                "discriminator": 2,
                "window_id": 2,
                "slot_start": 1_300,
                "slot_end": 1_449,
                "merkle_root": "0a".repeat(32),
                "commit_slot": 1_500,
                "reveal_slot": 1_651,
                "aggregates": [arm_json(1), arm_json(2), arm_json(3), arm_json(4)],
                "bump": 253,
            })
        );
    }

    #[test]
    fn renders_fields_in_account_layout_order() {
        let rendered =
            render_window(&Pubkey::new_from_array([3; 32]), &window()).unwrap();
        let positions: Vec<usize> = [
            "\"address\"",
            "\"discriminator\"",
            "\"window_id\"",
            "\"slot_start\"",
            "\"slot_end\"",
            "\"merkle_root\"",
            "\"commit_slot\"",
            "\"reveal_slot\"",
            "\"aggregates\"",
            "\"tx_submitted\"",
            "\"tx_landed\"",
            "\"stl_p50_slots\"",
            "\"stl_p90_slots\"",
            "\"cu_price_p50_micro\"",
            "\"cu_price_p90_micro\"",
            "\"fee_total_lamports\"",
            "\"tip_total_lamports\"",
            "\"bump\"",
        ]
        .iter()
        .map(|key| rendered.find(key).unwrap())
        .collect();

        assert!(positions.is_sorted(), "{rendered}");
    }

    #[test]
    fn renders_u64_extremes_without_loss() {
        let extreme = Window {
            window_id: u64::MAX,
            slot_end: u64::MAX,
            aggregates: [ArmAggregates {
                fee_total_lamports: u64::MAX,
                ..arm(1)
            }; ARM_COUNT],
            ..window()
        };

        let rendered: Value = serde_json::from_str(
            &render_window(&Pubkey::new_from_array([3; 32]), &extreme).unwrap(),
        )
        .unwrap();

        assert_eq!(rendered["window_id"].as_u64(), Some(u64::MAX));
        assert_eq!(rendered["slot_end"].as_u64(), Some(u64::MAX));
        assert_eq!(
            rendered["aggregates"][3]["fee_total_lamports"].as_u64(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn rendered_aggregates_parse_back_to_the_window_aggregates() {
        let rendered: Value = serde_json::from_str(
            &render_window(&Pubkey::new_from_array([3; 32]), &window()).unwrap(),
        )
        .unwrap();

        let aggregates = parse_aggregates(&rendered["aggregates"].to_string()).unwrap();

        assert_eq!(aggregates, window().aggregates);
    }
}
