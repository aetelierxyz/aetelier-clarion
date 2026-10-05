use std::{
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::time;

pub const MAINNET_BETA_GENESIS_HASH: &str =
    "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
pub const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const TESTNET_GENESIS_HASH: &str = "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY";

pub const USER_AGENT: &str = concat!("clarion-pulsar/", env!("CARGO_PKG_VERSION"));
pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_LIST_ENTRIES: usize = 20_000;
pub const MAX_SCHEDULE_SLOTS: u64 = 2_000_000;
pub const RETRYABLE_REMOTE_CODE: i64 = -32_005;

const PUBKEY_BYTES: usize = 32;
const MAX_PUBKEY_CHARS: usize = 44;
const MAX_VERSION_CHARS: usize = 64;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to build the http client: {}", describe_transport(.0))]
    Client(#[source] reqwest::Error),
    #[error("rpc transport failed: {}", describe_transport(.0))]
    Transport(#[source] reqwest::Error),
    #[error("rpc returned http status {0}")]
    Status(u16),
    #[error("rpc response body exceeds {limit} bytes")]
    BodyTooLarge { limit: u64 },
    #[error("rpc response could not be decoded: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("rpc returned error {code}: {}", escape_controls(.message))]
    Remote { code: i64, message: String },
    #[error("rpc response carries neither result nor error")]
    Empty,
    #[error("{method} failed after {attempts} of {limit} attempts: {source}")]
    Call {
        method: &'static str,
        attempts: u8,
        limit: u8,
        #[source]
        source: Box<RpcError>,
    },
    #[error("getGenesisHash returned a value that is not a 32-byte base58 hash")]
    InvalidGenesisHash,
    #[error(
        "getEpochInfo reports slot index {slot_index} beyond absolute slot {absolute_slot}"
    )]
    EpochInfo { absolute_slot: u64, slot_index: u64 },
}

impl RpcError {
    pub fn root(&self) -> &Self {
        match self {
            Self::Call { source, .. } => source.root(),
            other => other,
        }
    }
}

fn describe_transport(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "request timed out".to_owned()
    } else if error.is_connect() {
        "connection failed".to_owned()
    } else {
        error.to_string()
    }
}

pub fn escape_controls(text: &str) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut out, c| {
            if c.is_control() {
                out.extend(c.escape_default());
            } else if is_bidi_control(c) {
                out.extend(c.escape_unicode());
            } else {
                out.push(c);
            }
            out
        })
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

pub fn is_pubkey(text: &str) -> bool {
    text.len() <= MAX_PUBKEY_CHARS
        && bs58::decode(text)
            .into_vec()
            .is_ok_and(|bytes| bytes.len() == PUBKEY_BYTES)
}

pub fn is_version(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && text.len() <= MAX_VERSION_CHARS
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallPolicy {
    pub attempts: u8,
    pub first_backoff: Duration,
    pub retry_after_cap: Duration,
    pub request_timeout: Duration,
    pub max_body_bytes: u64,
}

impl Default for CallPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            first_backoff: Duration::from_secs(1),
            retry_after_cap: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
            max_body_bytes: MAX_BODY_BYTES,
        }
    }
}

impl CallPolicy {
    pub fn backoff(&self, retry: u8) -> Duration {
        let doublings = u32::from(retry.saturating_sub(1));
        let factor = 1_u32.checked_shl(doublings).unwrap_or(u32::MAX);
        self.first_backoff
            .checked_mul(factor)
            .unwrap_or(Duration::MAX)
    }

    pub fn retry_delay(
        &self,
        retry: u8,
        retry_after: Option<Duration>,
        rng: &mut fastrand::Rng,
    ) -> Duration {
        if let Some(retry_after) = retry_after {
            return retry_after.min(self.retry_after_cap);
        }
        let backoff = self.backoff(retry);
        let quarter = u64::try_from(backoff.as_micros() / 4).unwrap_or(u64::MAX);
        backoff.saturating_add(Duration::from_micros(rng.u64(0..=quarter)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screened<T> {
    pub kept: T,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterNode {
    pub pubkey: String,
    pub gossip: Option<String>,
    pub tpu_quic: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochInfo {
    pub epoch: u64,
    pub absolute_slot: u64,
    pub slot_index: u64,
}

impl EpochInfo {
    pub fn first_slot(&self) -> Option<u64> {
        self.absolute_slot.checked_sub(self.slot_index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "(u64, u64, u64)")]
pub struct EpochCredits {
    pub epoch: u64,
    pub credits: u64,
    pub previous_credits: u64,
}

impl From<(u64, u64, u64)> for EpochCredits {
    fn from((epoch, credits, previous_credits): (u64, u64, u64)) -> Self {
        Self {
            epoch,
            credits,
            previous_credits,
        }
    }
}

impl EpochCredits {
    pub fn earned(&self) -> u64 {
        self.credits.saturating_sub(self.previous_credits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteAccount {
    pub vote_pubkey: String,
    pub node_pubkey: String,
    pub activated_stake: u64,
    pub commission: u8,
    pub last_vote: u64,
    pub root_slot: u64,
    #[serde(deserialize_with = "last_entry")]
    pub epoch_credits: Option<EpochCredits>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct VoteAccounts {
    #[serde(deserialize_with = "bounded_list")]
    pub current: Vec<VoteAccount>,
    #[serde(deserialize_with = "bounded_list")]
    pub delinquent: Vec<VoteAccount>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Production {
    pub leader_slots: u64,
    pub blocks_produced: u64,
}

pub type BlockProduction = BTreeMap<String, Production>;

pub type LeaderSlots = BTreeMap<String, u64>;

pub type IdentityCounts = Vec<(String, (u64, u64))>;

#[derive(Deserialize)]
struct NodeList(#[serde(deserialize_with = "bounded_list")] Vec<ClusterNode>);

#[derive(Debug, Deserialize)]
struct Contextual<T> {
    value: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockProductionValue {
    #[serde(deserialize_with = "bounded_identity_map")]
    by_identity: IdentityCounts,
}

#[derive(Debug)]
struct ScheduleCounts(Vec<(String, u64)>);

impl<'de> Deserialize<'de> for ScheduleCounts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ScheduleVisitor).map(Self)
    }
}

fn bounded_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_seq(BoundedList(PhantomData))
}

struct BoundedList<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for BoundedList<T> {
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a list of at most {MAX_LIST_ENTRIES} entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = seq.next_element()? {
            if entries.len() >= MAX_LIST_ENTRIES {
                return Err(de::Error::custom(format_args!(
                    "list has more than {MAX_LIST_ENTRIES} entries"
                )));
            }
            entries.push(entry);
        }
        Ok(entries)
    }
}

fn bounded_identity_map<'de, D>(deserializer: D) -> Result<IdentityCounts, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(BoundedIdentityMap)
}

struct BoundedIdentityMap;

impl<'de> Visitor<'de> for BoundedIdentityMap {
    type Value = IdentityCounts;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a map of at most {MAX_LIST_ENTRIES} identities")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry()? {
            if entries.len() >= MAX_LIST_ENTRIES {
                return Err(de::Error::custom(format_args!(
                    "map has more than {MAX_LIST_ENTRIES} identities"
                )));
            }
            entries.push(entry);
        }
        Ok(entries)
    }
}

struct ScheduleVisitor;

impl<'de> Visitor<'de> for ScheduleVisitor {
    type Value = Vec<(String, u64)>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "a leader schedule of at most {MAX_SCHEDULE_SLOTS} slot indices"
        )
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut remaining = MAX_SCHEDULE_SLOTS;
        let mut counts = Vec::new();
        while let Some(identity) = map.next_key::<String>()? {
            if counts.len() >= MAX_LIST_ENTRIES {
                return Err(de::Error::custom(format_args!(
                    "leader schedule has more than {MAX_LIST_ENTRIES} identities"
                )));
            }
            let count = map.next_value_seed(SlotCount {
                remaining: &mut remaining,
            })?;
            counts.push((identity, count));
        }
        Ok(counts)
    }
}

struct SlotCount<'a> {
    remaining: &'a mut u64,
}

impl<'de> DeserializeSeed<'de> for SlotCount<'_> {
    type Value = u64;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<u64, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for SlotCount<'_> {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a list of slot indices")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<u64, A::Error> {
        let mut count = 0_u64;
        while seq.next_element::<u64>()?.is_some() {
            *self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
                de::Error::custom(format_args!(
                    "leader schedule has more than {MAX_SCHEDULE_SLOTS} slot indices"
                ))
            })?;
            count = count.saturating_add(1);
        }
        Ok(count)
    }
}

fn last_entry<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_seq(LastEntry(PhantomData))
}

struct LastEntry<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for LastEntry<T> {
    type Value = Option<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a list")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut last = None;
        while let Some(entry) = seq.next_element()? {
            last = Some(entry);
        }
        Ok(last)
    }
}

pub fn screen_nodes(nodes: Vec<ClusterNode>) -> Screened<Vec<ClusterNode>> {
    let total = nodes.len();
    let kept: Vec<ClusterNode> = nodes
        .into_iter()
        .filter(|node| is_pubkey(&node.pubkey))
        .map(|node| ClusterNode {
            version: node.version.filter(|version| is_version(version)),
            ..node
        })
        .collect();
    let dropped = dropped_count(total, kept.len());
    Screened { kept, dropped }
}

pub fn screen_vote_accounts(accounts: VoteAccounts) -> Screened<VoteAccounts> {
    let total = accounts
        .current
        .len()
        .saturating_add(accounts.delinquent.len());
    let valid = |account: &VoteAccount| {
        is_pubkey(&account.vote_pubkey) && is_pubkey(&account.node_pubkey)
    };
    let kept = VoteAccounts {
        current: accounts.current.into_iter().filter(valid).collect(),
        delinquent: accounts.delinquent.into_iter().filter(valid).collect(),
    };
    let dropped = dropped_count(
        total,
        kept.current.len().saturating_add(kept.delinquent.len()),
    );
    Screened { kept, dropped }
}

pub fn screen_block_production(entries: IdentityCounts) -> Screened<BlockProduction> {
    let mut kept = BlockProduction::new();
    let mut dropped = 0_u64;
    for (identity, (leader_slots, blocks_produced)) in entries {
        if is_pubkey(&identity) {
            kept.entry(identity).or_insert(Production {
                leader_slots,
                blocks_produced,
            });
        } else {
            dropped = dropped.saturating_add(1);
        }
    }
    Screened { kept, dropped }
}

pub fn screen_leader_schedule(counts: Vec<(String, u64)>) -> Screened<LeaderSlots> {
    let mut kept = LeaderSlots::new();
    let mut dropped = 0_u64;
    for (identity, count) in counts {
        if is_pubkey(&identity) {
            kept.entry(identity).or_insert(count);
        } else {
            dropped = dropped.saturating_add(1);
        }
    }
    Screened { kept, dropped }
}

fn dropped_count(total: usize, kept: usize) -> u64 {
    u64::try_from(total.saturating_sub(kept)).unwrap_or(u64::MAX)
}

enum Field<T> {
    Absent,
    Present(T),
}

impl<T> Field<T> {
    fn absent() -> Self {
        Self::Absent
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Envelope<T> {
    #[serde(default = "Field::absent")]
    result: Field<T>,
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
        (Field::Present(result), None) => Ok(result),
        (Field::Absent, None) => Err(RpcError::Empty),
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

enum Failure {
    Retry {
        error: RpcError,
        retry_after: Option<Duration>,
    },
    Stop(RpcError),
}

fn transport_failure(error: reqwest::Error) -> Failure {
    let retryable =
        error.is_timeout() || error.is_connect() || error.is_request() || error.is_body();
    let error = RpcError::Transport(error.without_url());
    if retryable {
        Failure::Retry {
            error,
            retry_after: None,
        }
    } else {
        Failure::Stop(error)
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

async fn read_body(
    response: &mut reqwest::Response,
    limit: u64,
) -> Result<Vec<u8>, Failure> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(Failure::Stop(RpcError::BodyTooLarge { limit }));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
        let total =
            u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if total > limit {
            return Err(Failure::Stop(RpcError::BodyTooLarge { limit }));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
    policy: CallPolicy,
    rng: Mutex<fastrand::Rng>,
}

impl RpcClient {
    pub fn new(url: impl Into<String>) -> Result<Self, RpcError> {
        Self::with_policy(url, CallPolicy::default(), fastrand::Rng::new())
    }

    pub fn with_policy(
        url: impl Into<String>,
        policy: CallPolicy,
        rng: fastrand::Rng,
    ) -> Result<Self, RpcError> {
        let http = reqwest::Client::builder()
            .timeout(policy.request_timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| RpcError::Client(error.without_url()))?;
        Ok(Self {
            http,
            url: url.into(),
            policy,
            rng: Mutex::new(rng),
        })
    }

    pub async fn genesis_hash(&self) -> Result<String, RpcError> {
        let hash: String = self.call("getGenesisHash", None).await?;
        if is_pubkey(&hash) {
            Ok(hash)
        } else {
            Err(RpcError::InvalidGenesisHash)
        }
    }

    pub async fn epoch_info(&self) -> Result<EpochInfo, RpcError> {
        let info: EpochInfo = self.call("getEpochInfo", None).await?;
        match info.first_slot() {
            Some(_) => Ok(info),
            None => Err(RpcError::EpochInfo {
                absolute_slot: info.absolute_slot,
                slot_index: info.slot_index,
            }),
        }
    }

    pub async fn cluster_nodes(&self) -> Result<Screened<Vec<ClusterNode>>, RpcError> {
        let NodeList(nodes) = self.call("getClusterNodes", None).await?;
        Ok(screen_nodes(nodes))
    }

    pub async fn vote_accounts(&self) -> Result<Screened<VoteAccounts>, RpcError> {
        let accounts: VoteAccounts = self.call("getVoteAccounts", None).await?;
        Ok(screen_vote_accounts(accounts))
    }

    pub async fn block_production(&self) -> Result<Screened<BlockProduction>, RpcError> {
        let production: Contextual<BlockProductionValue> =
            self.call("getBlockProduction", None).await?;
        Ok(screen_block_production(production.value.by_identity))
    }

    pub async fn leader_schedule(
        &self,
        first_slot: u64,
    ) -> Result<Option<Screened<LeaderSlots>>, RpcError> {
        let schedule: Option<ScheduleCounts> = self
            .call("getLeaderSchedule", Some(json!([first_slot])))
            .await?;
        Ok(schedule.map(|ScheduleCounts(counts)| screen_leader_schedule(counts)))
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Option<Value>,
    ) -> Result<T, RpcError> {
        let request = match params {
            Some(params) => {
                json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
            }
            None => json!({ "jsonrpc": "2.0", "id": 1, "method": method }),
        };
        let limit = self.policy.attempts.max(1);
        let mut attempts = 0_u8;
        loop {
            attempts = attempts.saturating_add(1);
            let failure = match self.attempt(&request).await {
                Ok(result) => return Ok(result),
                Err(failure) => failure,
            };
            let retry_after = match failure {
                Failure::Retry { retry_after, .. } if attempts < limit => retry_after,
                Failure::Retry { error, .. } | Failure::Stop(error) => {
                    return Err(RpcError::Call {
                        method,
                        attempts,
                        limit,
                        source: Box::new(error),
                    });
                }
            };
            time::sleep(self.retry_delay(attempts, retry_after)).await;
        }
    }

    fn retry_delay(&self, retry: u8, retry_after: Option<Duration>) -> Duration {
        let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
        self.policy.retry_delay(retry, retry_after, &mut rng)
    }

    async fn attempt<T: DeserializeOwned>(&self, request: &Value) -> Result<T, Failure> {
        let mut response = self
            .http
            .post(&self.url)
            .json(request)
            .send()
            .await
            .map_err(transport_failure)?;
        let status = response.status();
        let announced_retry_after = retry_after(response.headers());
        if !status.is_success() {
            let error = RpcError::Status(status.as_u16());
            return Err(
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    Failure::Retry {
                        error,
                        retry_after: announced_retry_after,
                    }
                } else {
                    Failure::Stop(error)
                },
            );
        }
        let body = read_body(&mut response, self.policy.max_body_bytes).await?;
        parse_result(&body).map_err(|error| match error {
            RpcError::Remote {
                code: RETRYABLE_REMOTE_CODE,
                ..
            } => Failure::Retry {
                error,
                retry_after: announced_retry_after,
            },
            error => Failure::Stop(error),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V";
    const KEY_B: &str = "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC";

    fn node(pubkey: &str, version: Option<&str>) -> ClusterNode {
        ClusterNode {
            pubkey: pubkey.to_owned(),
            gossip: None,
            tpu_quic: None,
            version: version.map(str::to_owned),
        }
    }

    fn vote(vote_pubkey: &str, node_pubkey: &str) -> VoteAccount {
        VoteAccount {
            vote_pubkey: vote_pubkey.to_owned(),
            node_pubkey: node_pubkey.to_owned(),
            activated_stake: 1,
            commission: 5,
            last_vote: 10,
            root_slot: 9,
            epoch_credits: None,
        }
    }

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
    fn null_result_is_kept_apart_from_an_absent_result() {
        let null: Option<u64> =
            parse_result(br#"{"jsonrpc":"2.0","result":null,"id":1}"#).unwrap();
        let absent =
            parse_result::<Option<u64>>(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();

        assert_eq!(null, None);
        assert!(matches!(absent, RpcError::Empty));
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
                version: Some("3.1.13".to_owned()),
            }
        );
        assert_eq!(
            bare,
            ClusterNode {
                pubkey: "B".to_owned(),
                gossip: None,
                tpu_quic: None,
                version: None,
            }
        );
    }

    #[test]
    fn pubkey_must_decode_to_exactly_thirty_two_bytes() {
        let thirty_one = bs58::encode([7_u8; 31]).into_string();
        let thirty_three = bs58::encode([7_u8; 33]).into_string();
        let zeros = bs58::encode([0_u8; 32]).into_string();

        assert!(is_pubkey(KEY_A));
        assert!(is_pubkey(&zeros));
        assert!(!is_pubkey(&thirty_one));
        assert!(!is_pubkey(&thirty_three));
        assert!(!is_pubkey(&"1".repeat(33)));
        assert!(!is_pubkey(""));
        assert!(!is_pubkey("0OIl"));
        assert!(!is_pubkey(
            "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2\u{1b}"
        ));
        assert!(!is_pubkey(&format!("{KEY_A}{KEY_A}")));
    }

    #[test]
    fn version_pattern_accepts_release_strings_only() {
        let longest = "9".repeat(64);
        let too_long = "9".repeat(65);

        for good in ["3.1.13", "2.2.0-rc.1+build_7", "a", longest.as_str()] {
            assert!(is_version(good), "{good}");
        }
        for bad in [
            "",
            "-3.1",
            ".3",
            "3.1 13",
            "3.1\u{1b}[31m",
            "3,1",
            "3.1/13",
            "é1",
            too_long.as_str(),
        ] {
            assert!(!is_version(bad), "{bad:?}");
        }
    }

    #[test]
    fn nodes_with_invalid_pubkeys_are_dropped_and_counted() {
        let screened = screen_nodes(vec![
            node(KEY_A, Some("3.1.13")),
            node("not-a-key", Some("3.1.13")),
            node(KEY_B, Some("3.1.13\u{1b}[2J")),
        ]);

        assert_eq!(screened.dropped, 1);
        assert_eq!(
            screened.kept,
            vec![node(KEY_A, Some("3.1.13")), node(KEY_B, None)]
        );
    }

    #[test]
    fn vote_accounts_need_both_keys_valid() {
        let screened = screen_vote_accounts(VoteAccounts {
            current: vec![vote(KEY_A, KEY_B), vote("bad", KEY_B)],
            delinquent: vec![vote(KEY_B, "bad"), vote(KEY_B, KEY_A)],
        });

        assert_eq!(screened.dropped, 2);
        assert_eq!(screened.kept.current, vec![vote(KEY_A, KEY_B)]);
        assert_eq!(screened.kept.delinquent, vec![vote(KEY_B, KEY_A)]);
    }

    #[test]
    fn block_production_and_schedule_identities_are_screened() {
        let production = screen_block_production(vec![
            (KEY_A.to_owned(), (4, 3)),
            ("short".to_owned(), (1, 1)),
        ]);
        let schedule =
            screen_leader_schedule(vec![(KEY_B.to_owned(), 8), ("\u{7}".to_owned(), 2)]);

        assert_eq!(production.dropped, 1);
        assert_eq!(
            production.kept,
            BlockProduction::from([(
                KEY_A.to_owned(),
                Production {
                    leader_slots: 4,
                    blocks_produced: 3
                }
            )])
        );
        assert_eq!(schedule.dropped, 1);
        assert_eq!(schedule.kept, LeaderSlots::from([(KEY_B.to_owned(), 8)]));
    }

    #[test]
    fn remote_message_is_displayed_with_control_characters_escaped() {
        let error = RpcError::Remote {
            code: -32000,
            message: "busy\u{1b}[2J\nnext\u{7}".to_owned(),
        };

        let text = error.to_string();

        assert_eq!(
            text,
            "rpc returned error -32000: busy\\u{1b}[2J\\nnext\\u{7}"
        );
        assert!(!text.chars().any(char::is_control));
    }

    #[test]
    fn bidirectional_overrides_are_escaped() {
        assert_eq!(escape_controls("a\u{202E}b"), "a\\u{202e}b");
    }

    #[test]
    fn escaping_keeps_printable_text_unchanged() {
        assert_eq!(
            escape_controls("getVoteAccounts: ok é"),
            "getVoteAccounts: ok é"
        );
    }

    #[test]
    fn list_above_twenty_thousand_entries_is_rejected() {
        let at_limit = format!("[{}]", vec!["1"; MAX_LIST_ENTRIES].join(","));
        let above = format!("[{}]", vec!["1"; MAX_LIST_ENTRIES + 1].join(","));

        let mut accepted = serde_json::Deserializer::from_str(&at_limit);
        let mut rejected = serde_json::Deserializer::from_str(&above);

        assert_eq!(
            bounded_list::<_, u8>(&mut accepted).unwrap().len(),
            MAX_LIST_ENTRIES
        );
        assert!(
            bounded_list::<_, u8>(&mut rejected)
                .unwrap_err()
                .to_string()
                .contains("more than 20000 entries")
        );
    }

    #[test]
    fn block_production_above_twenty_thousand_identities_is_rejected() {
        let entries = |count: usize| {
            let pairs: Vec<String> = (0..count)
                .map(|index| format!("\"{index}\":[1,1]"))
                .collect();
            format!(
                r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":{{"byIdentity":{{{}}},"range":{{"firstSlot":0,"lastSlot":1}}}}}},"id":1}}"#,
                pairs.join(",")
            )
        };

        let accepted: Contextual<BlockProductionValue> =
            parse_result(entries(MAX_LIST_ENTRIES).as_bytes()).unwrap();
        let rejected = parse_result::<Contextual<BlockProductionValue>>(
            entries(MAX_LIST_ENTRIES + 1).as_bytes(),
        )
        .unwrap_err();

        assert_eq!(accepted.value.by_identity.len(), MAX_LIST_ENTRIES);
        assert!(rejected.to_string().contains("more than 20000 identities"));
    }

    #[test]
    fn schedule_counts_slot_indices_per_identity_without_keeping_them() {
        let schedule: Option<ScheduleCounts> = parse_result(
            format!(r#"{{"jsonrpc":"2.0","result":{{"{KEY_A}":[0,1,2],"{KEY_B}":[]}},"id":1}}"#)
                .as_bytes(),
        )
        .unwrap();

        let counts = schedule.map(|ScheduleCounts(counts)| counts);

        assert_eq!(
            counts,
            Some(vec![(KEY_A.to_owned(), 3), (KEY_B.to_owned(), 0)])
        );
    }

    #[test]
    fn schedule_above_two_million_slot_indices_is_rejected() {
        let half = vec!["7"; 1_000_000].join(",");
        let at_limit = format!(r#"{{"{KEY_A}":[{half}],"{KEY_B}":[{half}]}}"#);
        let above = format!(r#"{{"{KEY_A}":[{half}],"{KEY_B}":[{half},7]}}"#);

        let accepted: ScheduleCounts = serde_json::from_str(&at_limit).unwrap();
        let rejected = serde_json::from_str::<ScheduleCounts>(&above).unwrap_err();

        assert_eq!(
            accepted.0,
            vec![(KEY_A.to_owned(), 1_000_000), (KEY_B.to_owned(), 1_000_000)]
        );
        assert!(
            rejected
                .to_string()
                .contains("more than 2000000 slot indices")
        );
    }

    #[test]
    fn schedule_above_twenty_thousand_identities_is_rejected() {
        let entries = |count: usize| {
            let pairs: Vec<String> =
                (0..count).map(|index| format!("\"{index}\":[]")).collect();
            format!("{{{}}}", pairs.join(","))
        };

        let accepted: ScheduleCounts =
            serde_json::from_str(&entries(MAX_LIST_ENTRIES)).unwrap();
        let rejected =
            serde_json::from_str::<ScheduleCounts>(&entries(MAX_LIST_ENTRIES + 1))
                .unwrap_err();

        assert_eq!(accepted.0.len(), MAX_LIST_ENTRIES);
        assert!(rejected.to_string().contains("more than 20000 identities"));
    }

    #[test]
    fn only_the_last_epoch_credits_entry_is_kept() {
        let account: VoteAccount = serde_json::from_str(&format!(
            r#"{{"votePubkey":"{KEY_A}","nodePubkey":"{KEY_B}","activatedStake":42,
                "epochVoteAccount":true,"commission":7,"lastVote":100,"rootSlot":68,
                "epochCredits":[[5,100,40],[6,250,100]]}}"#
        ))
        .unwrap();
        let empty: VoteAccount = serde_json::from_str(&format!(
            r#"{{"votePubkey":"{KEY_A}","nodePubkey":"{KEY_B}","activatedStake":0,
                "commission":0,"lastVote":0,"rootSlot":0,"epochCredits":[]}}"#
        ))
        .unwrap();

        assert_eq!(
            account.epoch_credits,
            Some(EpochCredits {
                epoch: 6,
                credits: 250,
                previous_credits: 100
            })
        );
        assert_eq!(empty.epoch_credits, None);
    }

    #[test]
    fn earned_credits_saturate_at_zero() {
        let regressed = EpochCredits {
            epoch: 6,
            credits: 90,
            previous_credits: 100,
        };

        assert_eq!(regressed.earned(), 0);
    }

    #[test]
    fn first_slot_of_the_epoch_is_absolute_slot_minus_slot_index() {
        let info = EpochInfo {
            epoch: 7,
            absolute_slot: 1_000,
            slot_index: 200,
        };
        let inconsistent = EpochInfo {
            slot_index: 1_001,
            ..info
        };

        assert_eq!(info.first_slot(), Some(800));
        assert_eq!(inconsistent.first_slot(), None);
    }

    #[test]
    fn backoff_doubles_from_one_second() {
        let policy = CallPolicy::default();

        let backoffs: Vec<Duration> =
            (1..=4).map(|retry| policy.backoff(retry)).collect();

        assert_eq!(backoffs, [1, 2, 4, 8].map(Duration::from_secs).to_vec());
    }

    #[test]
    fn jitter_adds_at_most_a_quarter_of_the_backoff() {
        let policy = CallPolicy::default();
        let mut rng = fastrand::Rng::with_seed(11);

        for retry in 1..=4 {
            let backoff = policy.backoff(retry);
            for _ in 0..200 {
                let delay = policy.retry_delay(retry, None, &mut rng);
                assert!(delay >= backoff && delay <= backoff + backoff / 4);
            }
        }
    }

    #[test]
    fn retry_after_replaces_the_backoff_and_is_capped_at_thirty_seconds() {
        let policy = CallPolicy::default();
        let mut rng = fastrand::Rng::with_seed(11);

        assert_eq!(
            policy.retry_delay(1, Some(Duration::from_secs(3)), &mut rng),
            Duration::from_secs(3)
        );
        assert_eq!(
            policy.retry_delay(4, Some(Duration::from_secs(120)), &mut rng),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn retry_after_is_read_as_whole_seconds() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);

        headers.insert(RETRY_AFTER, " 7 ".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(7)));

        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn default_policy_makes_five_attempts_and_caps_bodies_at_sixty_four_mebibytes() {
        let policy = CallPolicy::default();

        assert_eq!(policy.attempts, 5);
        assert_eq!(policy.max_body_bytes, 67_108_864);
        assert_eq!(policy.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn user_agent_names_the_tool_and_its_version() {
        assert_eq!(
            USER_AGENT,
            format!("clarion-pulsar/{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn root_unwraps_the_call_context() {
        let error = RpcError::Call {
            method: "getVoteAccounts",
            attempts: 1,
            limit: 5,
            source: Box::new(RpcError::Status(403)),
        };

        assert!(matches!(error.root(), RpcError::Status(403)));
        assert_eq!(
            error.to_string(),
            "getVoteAccounts failed after 1 of 5 attempts: rpc returned http status 403"
        );
    }
}
