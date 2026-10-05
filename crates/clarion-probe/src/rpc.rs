use std::time::Duration;

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use thiserror::Error;

pub const MAINNET_BETA_GENESIS_HASH: &str =
    "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
pub const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const TESTNET_GENESIS_HASH: &str = "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("rpc transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("rpc response is not a json-rpc envelope: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("rpc returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("rpc response carries neither result nor error")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterNode {
    pub pubkey: String,
    pub gossip: Option<String>,
    pub tpu_quic: Option<String>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: Option<T>,
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: i64,
    message: String,
}

pub fn parse_result<T: DeserializeOwned>(body: &[u8]) -> Result<T, RpcError> {
    let envelope: Envelope<T> = serde_json::from_slice(body)?;
    match (envelope.result, envelope.error) {
        (_, Some(error)) => Err(RpcError::Remote {
            code: error.code,
            message: error.message,
        }),
        (Some(result), None) => Ok(result),
        (None, None) => Err(RpcError::Empty),
    }
}

pub fn cluster_label(genesis_hash: &str) -> &str {
    match genesis_hash {
        MAINNET_BETA_GENESIS_HASH => "mainnet-beta",
        DEVNET_GENESIS_HASH => "devnet",
        TESTNET_GENESIS_HASH => "testnet",
        unknown => unknown,
    }
}

#[derive(Debug, Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
}

impl RpcClient {
    pub fn new(url: impl Into<String>) -> Result<Self, RpcError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            url: url.into(),
        })
    }

    pub async fn cluster_nodes(&self) -> Result<Vec<ClusterNode>, RpcError> {
        self.call("getClusterNodes").await
    }

    pub async fn genesis_hash(&self) -> Result<String, RpcError> {
        self.call("getGenesisHash").await
    }

    async fn call<T: DeserializeOwned>(&self, method: &str) -> Result<T, RpcError> {
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method });
        let body = self
            .http
            .post(&self.url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        parse_result(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_genesis_hashes_map_to_cluster_names() {
        assert_eq!(
            cluster_label("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
            "mainnet-beta"
        );
        assert_eq!(
            cluster_label("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"),
            "devnet"
        );
        assert_eq!(
            cluster_label("4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY"),
            "testnet"
        );
    }

    #[test]
    fn unknown_genesis_hash_labels_itself() {
        let hash = "GH7ome3EiwEr7tu9JuTh2dpYWBJK3z69Xm1ZE3MEE6JC";

        assert_eq!(cluster_label(hash), hash);
    }

    #[test]
    fn result_is_extracted_from_the_envelope() {
        let body = br#"{"jsonrpc":"2.0","result":"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG","id":1}"#;

        let hash: String = parse_result(body).unwrap();

        assert_eq!(hash, DEVNET_GENESIS_HASH);
    }

    #[test]
    fn remote_error_is_surfaced_with_code_and_message() {
        let body =
            br#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;

        let error = parse_result::<String>(body).unwrap_err();

        assert!(matches!(
            error,
            RpcError::Remote { code: -32601, message } if message == "Method not found"
        ));
    }

    #[test]
    fn envelope_without_result_or_error_is_rejected() {
        let error = parse_result::<String>(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();

        assert!(matches!(error, RpcError::Empty));
    }

    #[test]
    fn non_json_body_is_a_decode_error() {
        let error = parse_result::<String>(b"<html>502</html>").unwrap_err();

        assert!(matches!(error, RpcError::Decode(_)));
    }

    #[test]
    fn node_fields_are_read_from_camel_case_and_default_to_none() {
        let node: ClusterNode = serde_json::from_str(
            r#"{"pubkey":"A","gossip":null,"tpuQuic":"192.0.2.1:8009","version":"3.1.13"}"#,
        )
        .unwrap();
        let bare: ClusterNode = serde_json::from_str(r#"{"pubkey":"B"}"#).unwrap();

        assert_eq!(
            node,
            ClusterNode {
                pubkey: "A".to_owned(),
                gossip: None,
                tpu_quic: Some("192.0.2.1:8009".to_owned()),
            }
        );
        assert_eq!(
            bare,
            ClusterNode {
                pubkey: "B".to_owned(),
                gossip: None,
                tpu_quic: None
            }
        );
    }
}
