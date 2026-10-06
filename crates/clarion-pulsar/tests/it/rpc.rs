use std::time::{Duration, Instant};

use clarion_pulsar::{
    rpc::{
        BlockProduction, CallPolicy, ClusterNode, EpochInfo, LeaderSlots, Production,
        RpcClient, RpcError, USER_AGENT, parse_result,
    },
    targets::{Target, select_targets},
};

use crate::support::{
    CannedRpc, Framing, HangingHttp, RpcReply, client, closed_port, fast_policy, fixture,
    fixture_text,
};

const FIXTURE: &str = "get_cluster_nodes.json";
const KEY_0: &str = "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V";
const KEY_1: &str = "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC";
const KEY_9: &str = "J69vQ4CoH3AA221Q8BGxAyPq2rXeDhqug5gSrVMG3wKJ";
const SYNTHETIC_PROVIDER_KEY: &str = "k3y-0000-synthetic-provider-token";

fn node(pubkey: &str, gossip: Option<&str>, tpu_quic: Option<&str>) -> ClusterNode {
    ClusterNode {
        pubkey: pubkey.to_owned(),
        gossip: gossip.map(str::to_owned),
        tpu_quic: tpu_quic.map(str::to_owned),
        version: None,
    }
}

fn canned_reply(method: &str) -> RpcReply {
    match method {
        "getClusterNodes" => RpcReply::ok(fixture_text(FIXTURE)),
        "getGenesisHash" => RpcReply::ok(
            r#"{"jsonrpc":"2.0","result":"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG","id":1}"#,
        ),
        "getEpochInfo" => RpcReply::ok(fixture_text("get_epoch_info.json")),
        "getVoteAccounts" => RpcReply::ok(fixture_text("get_vote_accounts.json")),
        "getBlockProduction" => RpcReply::ok(fixture_text("get_block_production.json")),
        "getLeaderSchedule" => RpcReply::ok(fixture_text("get_leader_schedule.json")),
        _ => RpcReply::ok(
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#,
        ),
    }
}

fn method_not_found(_method: &str) -> RpcReply {
    canned_reply("unsupported")
}

fn genesis(hash: &str) -> RpcReply {
    RpcReply::result(&serde_json::json!(hash))
}

fn keyed(url: &str) -> String {
    format!("{url}/v1/{SYNTHETIC_PROVIDER_KEY}?api-key={SYNTHETIC_PROVIDER_KEY}")
}

fn error_chain(error: &RpcError) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(" | ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[test]
fn cluster_nodes_fixture_parses_with_null_and_missing_fields() {
    let nodes: Vec<ClusterNode> = parse_result(&fixture(FIXTURE)).unwrap();

    assert_eq!(
        nodes,
        vec![
            ClusterNode {
                version: Some("3.1.13".to_owned()),
                ..node(
                    "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V",
                    Some("192.0.2.10:8001"),
                    Some("192.0.2.10:8009"),
                )
            },
            ClusterNode {
                version: Some("3.1.13".to_owned()),
                ..node(
                    "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC",
                    Some("192.0.2.20:8001"),
                    Some("198.51.100.7:11228"),
                )
            },
            node(
                "D1qNfH759x48mNvFoacjBzbso16tpyftzJGrFkvc52Uj",
                Some("192.0.2.30:8001"),
                None
            ),
            ClusterNode {
                version: Some("1.18.26".to_owned()),
                ..node(
                    "3PCWRNyvSZnSi47hM9w88uF96jwiBAM9CoU5y6RA39em",
                    Some("192.0.2.40:8001"),
                    None
                )
            },
            ClusterNode {
                version: Some("3.1.13".to_owned()),
                ..node(
                    "HAzUQNXVVQ6qrAXBBTVHkkFouc8rBJz5DGiCKFrDtSa9",
                    None,
                    Some("203.0.113.50:8009"),
                )
            },
            node(
                "3QpLzoyAR7Yav7Mmbmve64jFQCMcy9wjrL75W4cvXwAf",
                None,
                Some("[2001:db8::60]:8009"),
            ),
        ]
    );
}

#[test]
fn cluster_nodes_fixture_yields_only_the_entries_with_tpu_quic() {
    let nodes: Vec<ClusterNode> = parse_result(&fixture(FIXTURE)).unwrap();

    let targets = select_targets(&nodes);

    assert_eq!(
        targets,
        vec![
            Target {
                pubkey: "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V".to_owned(),
                tpu_quic: "192.0.2.10:8009".parse().unwrap(),
                gossip_ip: Some("192.0.2.10".parse().unwrap()),
            },
            Target {
                pubkey: "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC".to_owned(),
                tpu_quic: "198.51.100.7:11228".parse().unwrap(),
                gossip_ip: Some("192.0.2.20".parse().unwrap()),
            },
            Target {
                pubkey: "HAzUQNXVVQ6qrAXBBTVHkkFouc8rBJz5DGiCKFrDtSa9".to_owned(),
                tpu_quic: "203.0.113.50:8009".parse().unwrap(),
                gossip_ip: None,
            },
            Target {
                pubkey: "3QpLzoyAR7Yav7Mmbmve64jFQCMcy9wjrL75W4cvXwAf".to_owned(),
                tpu_quic: "[2001:db8::60]:8009".parse().unwrap(),
                gossip_ip: None,
            },
        ]
    );
}

#[tokio::test]
async fn client_posts_json_rpc_requests_and_decodes_results() {
    let mut rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let genesis_hash = client.genesis_hash().await.unwrap();
    let nodes = client.cluster_nodes().await.unwrap();

    assert_eq!(genesis_hash, "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG");
    assert_eq!(nodes.kept.len(), 6);
    assert_eq!(nodes.dropped, 0);
    let first = rpc.requests.recv().await.unwrap();
    let second = rpc.requests.recv().await.unwrap();
    assert_eq!(first.body["jsonrpc"], "2.0");
    assert_eq!(first.method(), "getGenesisHash");
    assert_eq!(second.method(), "getClusterNodes");
}

#[tokio::test]
async fn every_request_carries_the_tool_user_agent() {
    let mut rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    client.genesis_hash().await.unwrap();

    let request = rpc.requests.recv().await.unwrap();
    assert_eq!(request.user_agent.as_deref(), Some(USER_AGENT));
    assert!(USER_AGENT.starts_with("clarion-pulsar/"));
}

#[tokio::test]
async fn epoch_info_fixture_gives_the_epoch_and_its_first_slot() {
    let rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let info = client.epoch_info().await.unwrap();

    assert_eq!(
        info,
        EpochInfo {
            epoch: 7,
            absolute_slot: 1_000,
            slot_index: 200
        }
    );
    assert_eq!(info.first_slot(), Some(800));
}

#[tokio::test]
async fn epoch_info_with_slot_index_beyond_absolute_slot_fails() {
    let rpc = CannedRpc::scripted(vec![RpcReply::result(&serde_json::json!({
        "absoluteSlot": 5, "slotIndex": 6, "epoch": 1
    }))])
    .await;

    let error = client(&rpc.url, fast_policy())
        .epoch_info()
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        RpcError::EpochInfo {
            absolute_slot: 5,
            slot_index: 6
        }
    ));
}

#[tokio::test]
async fn vote_accounts_fixture_keeps_valid_accounts_and_counts_the_invalid_one() {
    let rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let accounts = client.vote_accounts().await.unwrap();

    assert_eq!(accounts.dropped, 1);
    assert_eq!(accounts.kept.current.len(), 3);
    assert_eq!(accounts.kept.delinquent.len(), 2);
    assert!(
        accounts
            .kept
            .current
            .iter()
            .all(|account| account.vote_pubkey != "not-a-vote-account")
    );
}

#[tokio::test]
async fn block_production_fixture_is_keyed_by_valid_identity() {
    let rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let production = client.block_production().await.unwrap();

    assert_eq!(production.dropped, 1);
    assert_eq!(
        production.kept,
        BlockProduction::from([
            (
                KEY_0.to_owned(),
                Production {
                    leader_slots: 8,
                    blocks_produced: 7
                }
            ),
            (
                KEY_9.to_owned(),
                Production {
                    leader_slots: 4,
                    blocks_produced: 4
                }
            ),
        ])
    );
}

#[tokio::test]
async fn leader_schedule_is_requested_for_the_given_slot_and_counted() {
    let mut rpc = CannedRpc::by_method(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let schedule = client.leader_schedule(800).await.unwrap().unwrap();

    assert_eq!(schedule.dropped, 1);
    assert_eq!(
        schedule.kept,
        LeaderSlots::from([
            (KEY_0.to_owned(), 4),
            (KEY_1.to_owned(), 0),
            (KEY_9.to_owned(), 8)
        ])
    );
    let request = rpc.requests.recv().await.unwrap();
    assert_eq!(request.method(), "getLeaderSchedule");
    assert_eq!(request.body["params"], serde_json::json!([800]));
}

#[tokio::test]
async fn null_leader_schedule_is_none() {
    let rpc = CannedRpc::scripted(vec![RpcReply::result(&serde_json::Value::Null)]).await;

    let schedule = client(&rpc.url, fast_policy())
        .leader_schedule(800)
        .await
        .unwrap();

    assert_eq!(schedule, None);
}

#[tokio::test]
async fn genesis_hash_that_is_not_a_32_byte_key_fails() {
    for hash in ["devnet", "\u{1b}[2J", "1111", ""] {
        let rpc = CannedRpc::scripted(vec![genesis(hash)]).await;

        let error = client(&rpc.url, fast_policy())
            .genesis_hash()
            .await
            .unwrap_err();

        assert!(matches!(error, RpcError::InvalidGenesisHash), "{hash:?}");
    }
}

#[tokio::test]
async fn nodes_with_invalid_pubkeys_or_versions_are_screened() {
    let rpc = CannedRpc::scripted(vec![RpcReply::result(&serde_json::json!([
        { "pubkey": KEY_0, "tpuQuic": "192.0.2.10:8009", "version": "3.1.13\u{1b}]0;x\u{7}" },
        { "pubkey": "\u{1b}[31mred", "tpuQuic": "192.0.2.11:8009" },
        { "pubkey": "8zUFfLHADcabAoM9YFZYEosLosi2GDmsDuzfcSEkxG", "tpuQuic": "192.0.2.12:8009" },
        { "pubkey": KEY_1, "version": "3.1.13" }
    ]))])
    .await;

    let nodes = client(&rpc.url, fast_policy())
        .cluster_nodes()
        .await
        .unwrap();

    assert_eq!(nodes.dropped, 2);
    assert_eq!(
        nodes
            .kept
            .iter()
            .map(|node| (node.pubkey.as_str(), node.version.as_deref()))
            .collect::<Vec<_>>(),
        [(KEY_0, None), (KEY_1, Some("3.1.13"))]
    );
}

#[tokio::test]
async fn client_surfaces_json_rpc_errors_without_retrying() {
    let mut rpc = CannedRpc::by_method(method_not_found).await;
    let client = client(&rpc.url, fast_policy());

    let error = client.cluster_nodes().await.unwrap_err();

    assert!(matches!(
        error,
        RpcError::Call {
            method: "getClusterNodes",
            attempts: 1,
            limit: 5,
            ..
        }
    ));
    assert!(matches!(
        error.root(),
        RpcError::Remote { code: -32601, .. }
    ));
    assert_eq!(rpc.methods().len(), 1);
}

#[tokio::test]
async fn rate_limit_is_retried_after_the_seconds_in_retry_after() {
    let mut rpc = CannedRpc::scripted(vec![
        RpcReply::status(429, "Too Many Requests").header("retry-after", "1"),
        genesis(KEY_0),
    ])
    .await;
    let client = client(&rpc.url, fast_policy());

    let started = Instant::now();
    let hash = client.genesis_hash().await.unwrap();

    assert_eq!(hash, KEY_0);
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(rpc.methods(), ["getGenesisHash", "getGenesisHash"]);
}

#[tokio::test]
async fn node_behind_is_retried_after_the_seconds_in_retry_after() {
    let mut rpc = CannedRpc::scripted(vec![
        RpcReply::remote_error(-32005, "Node is behind by 42 slots")
            .header("retry-after", "1"),
        genesis(KEY_0),
    ])
    .await;

    let started = Instant::now();
    client(&rpc.url, fast_policy())
        .genesis_hash()
        .await
        .unwrap();

    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(rpc.methods().len(), 2);
}

#[tokio::test]
async fn server_error_is_retried_after_the_seconds_in_retry_after() {
    let mut rpc = CannedRpc::scripted(vec![
        RpcReply::status(503, "Service Unavailable").header("retry-after", "1"),
        genesis(KEY_0),
    ])
    .await;

    let started = Instant::now();
    client(&rpc.url, fast_policy())
        .genesis_hash()
        .await
        .unwrap();

    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(rpc.methods().len(), 2);
}

#[tokio::test]
async fn rate_limit_without_retry_after_uses_the_backoff() {
    let mut rpc = CannedRpc::scripted(vec![
        RpcReply::status(429, "Too Many Requests"),
        RpcReply::status(429, "Too Many Requests"),
        genesis(KEY_0),
    ])
    .await;
    let policy = CallPolicy {
        first_backoff: Duration::from_millis(100),
        ..fast_policy()
    };

    let started = Instant::now();
    client(&rpc.url, policy).genesis_hash().await.unwrap();

    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(2_500), "{elapsed:?}");
    assert_eq!(rpc.methods().len(), 3);
}

#[tokio::test]
async fn server_errors_and_node_behind_are_retried() {
    let mut rpc = CannedRpc::scripted(vec![
        RpcReply::status(503, "Service Unavailable"),
        RpcReply::status(502, "Bad Gateway"),
        RpcReply::remote_error(-32005, "Node is behind by 42 slots"),
        genesis(KEY_0),
    ])
    .await;

    let hash = client(&rpc.url, fast_policy())
        .genesis_hash()
        .await
        .unwrap();

    assert_eq!(hash, KEY_0);
    assert_eq!(rpc.methods().len(), 4);
}

#[tokio::test]
async fn forbidden_fails_at_once_without_a_retry() {
    let mut rpc =
        CannedRpc::scripted(vec![RpcReply::status(403, "Forbidden"), genesis(KEY_0)])
            .await;

    let error = client(&rpc.url, fast_policy())
        .genesis_hash()
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        RpcError::Call {
            method: "getGenesisHash",
            attempts: 1,
            ..
        }
    ));
    assert!(matches!(error.root(), RpcError::Status(403)));
    assert_eq!(rpc.methods().len(), 1);
}

#[tokio::test]
async fn other_client_errors_fail_at_once() {
    for status in [400, 401, 404, 413] {
        let mut rpc =
            CannedRpc::scripted(vec![RpcReply::status(status, "no"), genesis(KEY_0)])
                .await;

        let error = client(&rpc.url, fast_policy())
            .genesis_hash()
            .await
            .unwrap_err();

        assert!(matches!(error.root(), RpcError::Status(code) if *code == status));
        assert_eq!(rpc.methods().len(), 1);
    }
}

#[tokio::test]
async fn five_attempts_are_made_and_the_error_names_method_and_count() {
    let mut rpc =
        CannedRpc::scripted(vec![RpcReply::status(429, "Too Many Requests"); 6]).await;

    let error = client(&rpc.url, fast_policy())
        .vote_accounts()
        .await
        .unwrap_err();

    assert_eq!(rpc.methods().len(), 5);
    assert_eq!(
        error.to_string(),
        "getVoteAccounts failed after 5 of 5 attempts: rpc returned http status 429"
    );
}

#[tokio::test]
async fn request_timeouts_are_retried() {
    let server = HangingHttp::spawn().await;
    let policy = CallPolicy {
        request_timeout: Duration::from_millis(100),
        ..fast_policy()
    };

    let error = client(&server.url, policy)
        .genesis_hash()
        .await
        .unwrap_err();

    assert!(matches!(error, RpcError::Call { attempts: 5, .. }));
    assert!(
        error
            .to_string()
            .ends_with("rpc transport failed: request timed out")
    );
}

#[tokio::test]
async fn connection_errors_are_retried_and_the_url_never_reaches_the_error() {
    let address = closed_port().await;
    let url = keyed(&format!("http://{address}"));

    let error = client(&url, fast_policy())
        .genesis_hash()
        .await
        .unwrap_err();

    let chain = error_chain(&error);
    assert!(matches!(error, RpcError::Call { attempts: 5, .. }));
    assert!(!chain.contains(SYNTHETIC_PROVIDER_KEY), "{chain}");
    assert!(!chain.contains("http://"), "{chain}");
    assert!(!error.to_string().contains(&address.to_string()), "{error}");
    assert!(!format!("{error:?}").contains(SYNTHETIC_PROVIDER_KEY));
}

#[tokio::test]
async fn transport_error_text_names_the_failure_without_the_url() {
    let address = closed_port().await;
    let url = keyed(&format!("http://{address}"));
    let policy = CallPolicy {
        attempts: 1,
        ..fast_policy()
    };

    let error = client(&url, policy).genesis_hash().await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "getGenesisHash failed after 1 of 1 attempts: rpc transport failed: connection failed"
    );
}

#[tokio::test]
async fn announced_length_above_sixty_four_mebibytes_is_rejected_unread() {
    let mut rpc = CannedRpc::scripted(vec![
        genesis(KEY_0).framed(Framing::Announced(64 * 1024 * 1024 + 1)),
        genesis(KEY_0),
    ])
    .await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let error = client.genesis_hash().await.unwrap_err();

    assert!(matches!(
        error.root(),
        RpcError::BodyTooLarge { limit: 67_108_864 }
    ));
    assert_eq!(rpc.methods().len(), 1);
}

#[tokio::test]
async fn streamed_body_past_the_limit_is_rejected() {
    let body = format!(
        r#"{{"jsonrpc":"2.0","result":"{KEY_0}","id":1,"pad":"{}"}}"#,
        "x".repeat(2_048)
    );
    let rpc =
        CannedRpc::scripted(vec![RpcReply::ok(body).framed(Framing::UntilClose)]).await;
    let policy = CallPolicy {
        max_body_bytes: 1_024,
        ..fast_policy()
    };

    let error = client(&rpc.url, policy).genesis_hash().await.unwrap_err();

    assert!(matches!(
        error.root(),
        RpcError::BodyTooLarge { limit: 1_024 }
    ));
}

#[tokio::test]
async fn streamed_body_one_byte_past_the_limit_is_rejected() {
    let body = format!(r#"{{"jsonrpc":"2.0","result":"{KEY_0}","id":1}}"#);
    let limit = u64::try_from(body.len() - 1).unwrap();
    let rpc =
        CannedRpc::scripted(vec![RpcReply::ok(body).framed(Framing::UntilClose)]).await;
    let policy = CallPolicy {
        max_body_bytes: limit,
        ..fast_policy()
    };

    let error = client(&rpc.url, policy).genesis_hash().await.unwrap_err();

    assert!(matches!(
        error.root(),
        RpcError::BodyTooLarge { limit: reported } if *reported == limit
    ));
}

#[tokio::test]
async fn body_at_the_limit_is_accepted() {
    let body = format!(r#"{{"jsonrpc":"2.0","result":"{KEY_0}","id":1}}"#);
    let limit = u64::try_from(body.len()).unwrap();
    let rpc =
        CannedRpc::scripted(vec![RpcReply::ok(body).framed(Framing::UntilClose)]).await;
    let policy = CallPolicy {
        max_body_bytes: limit,
        ..fast_policy()
    };

    let hash = client(&rpc.url, policy).genesis_hash().await.unwrap();

    assert_eq!(hash, KEY_0);
}

#[tokio::test]
async fn remote_error_message_reaches_the_display_escaped() {
    let rpc = CannedRpc::scripted(vec![RpcReply::remote_error(
        -32602,
        "bad\u{1b}[2J\r\nparams",
    )])
    .await;

    let error = client(&rpc.url, fast_policy())
        .genesis_hash()
        .await
        .unwrap_err();

    let text = error.to_string();
    assert!(!text.chars().any(char::is_control), "{text:?}");
    assert!(text.ends_with("rpc returned error -32602: bad\\u{1b}[2J\\r\\nparams"));
}
