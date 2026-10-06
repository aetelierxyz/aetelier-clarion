use clarion_program_client::{
    ArmAggregates, ClarionError, Window, decode_window, merkle_root_hex,
    parse_aggregates, render_window,
};
use serde_json::Value;
use solana_program_test::tokio;

use crate::support::{
    AGGREGATES_JSON, Harness, MERKLE_ROOT_HEX, MIN_REVEAL_LAG_SLOTS, aggregates,
    assert_rejected, window_key,
};

#[tokio::test]
async fn reveal_writes_the_aggregates_parsed_from_json() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let committed = harness.window(0).await;
    let state = harness.state().await;
    let reveal_slot = commit_slot + MIN_REVEAL_LAG_SLOTS;
    harness.warp_to(reveal_slot);

    harness.reveal(0, aggregates()).await.unwrap();

    let account = harness.account(&window_key(0)).await;
    let revealed = decode_window(&account.data).unwrap();
    assert_eq!(
        revealed,
        Window {
            reveal_slot,
            aggregates: aggregates(),
            ..committed
        }
    );
    assert!(revealed.is_revealed());
    assert_eq!(
        revealed.aggregates[1],
        ArmAggregates {
            tx_submitted: 200,
            tx_landed: 192,
            stl_p50_slots: 2,
            stl_p90_slots: 4,
            cu_price_p50_micro: 2_000,
            cu_price_p90_micro: 18_000,
            fee_total_lamports: 1_000_000,
            tip_total_lamports: 250_000,
        }
    );
    assert_eq!(harness.state().await, state);
}

#[tokio::test]
async fn reveal_of_the_third_window_leaves_the_first_two_committed() {
    let (mut harness, _) = Harness::start_committed().await;
    harness.commit_once_closed(1).await;
    let commit_slot = harness.commit_once_closed(2).await;
    let earlier = [harness.window(0).await, harness.window(1).await];
    let committed = harness.window(2).await;
    let reveal_slot = commit_slot + MIN_REVEAL_LAG_SLOTS;
    harness.warp_to(reveal_slot);

    harness.reveal(2, aggregates()).await.unwrap();

    assert_eq!(committed.window_id, 2);
    assert_eq!(
        harness.window(2).await,
        Window {
            reveal_slot,
            aggregates: aggregates(),
            ..committed
        }
    );
    assert_eq!([harness.window(0).await, harness.window(1).await], earlier);
    assert!(earlier.iter().all(|window| !window.is_revealed()));
}

#[tokio::test]
async fn landed_above_submitted_parses_but_is_rejected_on_chain() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let committed = harness.window(0).await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS);
    let exceeding = parse_aggregates(
        &AGGREGATES_JSON.replace("\"tx_landed\": 293", "\"tx_landed\": 301"),
    )
    .unwrap();
    assert_eq!(exceeding[2].tx_submitted, 300);
    assert_eq!(exceeding[2].tx_landed, 301);

    assert_rejected(
        harness.reveal(0, exceeding).await,
        ClarionError::LandedExceedsSubmitted,
    );

    assert_eq!(harness.window(0).await, committed);
}

#[tokio::test]
async fn reveal_before_the_lag_is_rejected_and_leaves_the_window_committed() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let committed = harness.window(0).await;
    harness.warp_to(commit_slot + MIN_REVEAL_LAG_SLOTS - 1);

    assert_rejected(
        harness.reveal(0, aggregates()).await,
        ClarionError::RevealLagNotElapsed,
    );

    assert_eq!(harness.window(0).await, committed);
    assert!(!committed.is_revealed());
}

#[tokio::test]
async fn revealed_window_renders_as_json_with_its_address_and_hex_root() {
    let (mut harness, commit_slot) = Harness::start_committed().await;
    let reveal_slot = commit_slot + MIN_REVEAL_LAG_SLOTS;
    harness.warp_to(reveal_slot);
    harness.reveal(0, aggregates()).await.unwrap();
    let window = harness.window(0).await;

    let rendered: Value =
        serde_json::from_str(&render_window(&window_key(0), &window).unwrap()).unwrap();

    assert_eq!(rendered["address"], window_key(0).to_string());
    assert_eq!(rendered["discriminator"], 2);
    assert_eq!(rendered["window_id"], 0);
    assert_eq!(rendered["slot_start"], window.slot_start);
    assert_eq!(rendered["slot_end"], window.slot_end);
    assert_eq!(rendered["merkle_root"], MERKLE_ROOT_HEX);
    assert_eq!(
        rendered["merkle_root"],
        merkle_root_hex(&window.merkle_root)
    );
    assert_eq!(rendered["commit_slot"], commit_slot);
    assert_eq!(rendered["reveal_slot"], reveal_slot);
    assert_eq!(
        rendered["aggregates"],
        serde_json::from_str::<Value>(AGGREGATES_JSON).unwrap()
    );
    assert_eq!(rendered["bump"], window.bump);
}
