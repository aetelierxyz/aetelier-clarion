use clarion_probe::{
    rpc::{ClusterNode, RpcClient, RpcError, parse_result},
    targets::{Target, select_targets},
};

use crate::support::{CannedRpc, RpcReply, fixture};

const FIXTURE: &str = "get_cluster_nodes.json";

fn node(pubkey: &str, gossip: Option<&str>, tpu_quic: Option<&str>) -> ClusterNode {
    ClusterNode {
        pubkey: pubkey.to_owned(),
        gossip: gossip.map(str::to_owned),
        tpu_quic: tpu_quic.map(str::to_owned),
    }
}

fn canned_reply(method: &str) -> RpcReply {
    match method {
        "getClusterNodes" => RpcReply::ok(String::from_utf8(fixture(FIXTURE)).unwrap()),
        "getGenesisHash" => RpcReply::ok(
            r#"{"jsonrpc":"2.0","result":"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG","id":1}"#,
        ),
        _ => RpcReply::ok(
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#,
        ),
    }
}

fn rate_limited(_method: &str) -> RpcReply {
    RpcReply {
        status: 429,
        body: "Too Many Requests".to_owned(),
    }
}

fn method_not_found(_method: &str) -> RpcReply {
    canned_reply("unsupported")
}

#[test]
fn cluster_nodes_fixture_parses_with_null_and_missing_fields() {
    let nodes: Vec<ClusterNode> = parse_result(&fixture(FIXTURE)).unwrap();

    assert_eq!(
        nodes,
        vec![
            node(
                "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V",
                Some("192.0.2.10:8001"),
                Some("192.0.2.10:8009"),
            ),
            node(
                "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC",
                Some("192.0.2.20:8001"),
                Some("198.51.100.7:11228"),
            ),
            node(
                "D1qNfH759x48mNvFoacjBzbso16tpyftzJGrFkvc52Uj",
                Some("192.0.2.30:8001"),
                None
            ),
            node(
                "3PCWRNyvSZnSi47hM9w88uF96jwiBAM9CoU5y6RA39em",
                Some("192.0.2.40:8001"),
                None
            ),
            node(
                "HAzUQNXVVQ6qrAXBBTVHkkFouc8rBJz5DGiCKFrDtSa9",
                None,
                Some("203.0.113.50:8009"),
            ),
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
    let mut rpc = CannedRpc::spawn(canned_reply).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let genesis_hash = client.genesis_hash().await.unwrap();
    let nodes = client.cluster_nodes().await.unwrap();

    assert_eq!(genesis_hash, "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG");
    assert_eq!(nodes.len(), 6);
    let first = rpc.requests.recv().await.unwrap();
    let second = rpc.requests.recv().await.unwrap();
    assert_eq!(first["jsonrpc"], "2.0");
    assert_eq!(first["method"], "getGenesisHash");
    assert_eq!(second["method"], "getClusterNodes");
}

#[tokio::test]
async fn client_surfaces_json_rpc_errors() {
    let rpc = CannedRpc::spawn(method_not_found).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let error = client.cluster_nodes().await.unwrap_err();

    assert!(matches!(error, RpcError::Remote { code: -32601, .. }));
}

#[tokio::test]
async fn client_surfaces_http_status_failures() {
    let rpc = CannedRpc::spawn(rate_limited).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();

    let error = client.genesis_hash().await.unwrap_err();

    assert!(matches!(
        error,
        RpcError::Transport(source) if source.status().map(|status| status.as_u16()) == Some(429)
    ));
}
