use std::{
    error::Error,
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use solana_program::pubkey::Pubkey;
use solana_transaction::Hash;
use thiserror::Error;

use crate::loader_buffer::{LoaderAccount, WriteTransaction};

pub const MAX_REQUESTS_PER_SECOND: f64 = 3.5;
pub const FIRST_BACKOFF: Duration = Duration::from_secs(2);
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);
pub const RATE_LIMIT_BUDGET: Duration = Duration::from_secs(300);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const TOO_MANY_REQUESTS: u16 = 429;
const BODY_EXCERPT_CHARS: usize = 200;

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpReply {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

pub type TransportError = Box<dyn Error + Send + Sync>;

pub trait Transport {
    fn post(&mut self, body: &str) -> Result<HttpReply, TransportError>;
}

#[derive(Debug)]
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    url: reqwest::Url,
}

impl HttpTransport {
    pub fn new(url: &str) -> Result<Self, RpcError> {
        let url = reqwest::Url::parse(url).map_err(|error| RpcError::Url {
            url: url.to_string(),
            detail: error.to_string(),
        })?;
        let client = reqwest::blocking::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .retry(reqwest::retry::never())
            .build()
            .map_err(|source| RpcError::Transport {
                method: "client".to_string(),
                source: source.into(),
            })?;
        Ok(Self { client, url })
    }
}

impl Transport for HttpTransport {
    fn post(&mut self, body: &str) -> Result<HttpReply, TransportError> {
        let response = self
            .client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.bytes()?.to_vec();
        Ok(HttpReply {
            status,
            retry_after,
            body,
        })
    }
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("invalid rpc url {url}: {detail}")]
    Url { url: String, detail: String },
    #[error("{method}: {source}")]
    Transport {
        method: String,
        #[source]
        source: TransportError,
    },
    #[error(
        "{method}: still rate limited (HTTP 429) after {attempts} retries over {}s, \
         the budget is {}s",
        .elapsed.as_secs(),
        .budget.as_secs()
    )]
    RateLimited {
        method: String,
        attempts: u32,
        elapsed: Duration,
        budget: Duration,
    },
    #[error("{method}: HTTP {status}: {excerpt}")]
    Http {
        method: String,
        status: u16,
        excerpt: String,
    },
    #[error("{method}: JSON-RPC error {code}: {message}")]
    Response {
        method: String,
        code: i64,
        message: String,
    },
    #[error("{method}: malformed reply: {detail}")]
    Malformed { method: String, detail: String },
}

#[derive(Clone, Copy, Debug)]
pub struct Pacer {
    interval: Duration,
    next_request: Option<Instant>,
}

impl Pacer {
    pub fn new(requests_per_second: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / requests_per_second),
            next_request: None,
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn wait_turn(&mut self, clock: &impl Clock) {
        if let Some(next_request) = self.next_request {
            let now = clock.now();
            if now < next_request {
                clock.sleep(next_request - now);
            }
        }
        self.next_request = Some(clock.now() + self.interval);
    }
}

pub fn backoff(retry_after: Option<&str>, retries: u32) -> Duration {
    retry_after
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds).max(FIRST_BACKOFF))
        .unwrap_or_else(|| {
            FIRST_BACKOFF
                .checked_mul(2u32.saturating_pow(retries))
                .map_or(MAX_BACKOFF, |delay| delay.min(MAX_BACKOFF))
        })
}

pub struct PacedRpc<T, C> {
    transport: T,
    clock: C,
    pacer: Pacer,
    next_id: u64,
    requests: u64,
}

impl<T: Transport, C: Clock> PacedRpc<T, C> {
    pub fn new(transport: T, clock: C, requests_per_second: f64) -> Self {
        Self {
            transport,
            clock,
            pacer: Pacer::new(requests_per_second),
            next_id: 1,
            requests: 0,
        }
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }

    fn call(
        &mut self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
    ) -> Result<Value, RpcError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        })
        .to_string();
        self.next_id += 1;
        let started = self.clock.now();
        let budget = deadline.map_or(RATE_LIMIT_BUDGET, |deadline| {
            deadline
                .saturating_duration_since(started)
                .min(RATE_LIMIT_BUDGET)
        });
        let mut retries = 0;
        loop {
            self.pacer.wait_turn(&self.clock);
            self.requests += 1;
            let reply =
                self.transport
                    .post(&body)
                    .map_err(|source| RpcError::Transport {
                        method: method.to_string(),
                        source,
                    })?;
            if reply.status == TOO_MANY_REQUESTS {
                let delay = backoff(reply.retry_after.as_deref(), retries);
                let elapsed = self.clock.now().duration_since(started);
                if elapsed + delay > budget {
                    return Err(RpcError::RateLimited {
                        method: method.to_string(),
                        attempts: retries,
                        elapsed,
                        budget,
                    });
                }
                self.clock.sleep(delay);
                retries += 1;
                continue;
            }
            return parse_reply(method, &reply);
        }
    }

    pub fn get_account_info(
        &mut self,
        address: &Pubkey,
    ) -> Result<Option<LoaderAccount>, RpcError> {
        let method = "getAccountInfo";
        let result = self.call(
            method,
            json!([address.to_string(), {"encoding": "base64", "commitment": "finalized"}]),
            None,
        )?;
        let value = result
            .get("value")
            .ok_or_else(|| malformed(method, "no value"))?;
        if value.is_null() {
            return Ok(None);
        }
        let owner = value
            .get("owner")
            .and_then(Value::as_str)
            .and_then(|owner| Pubkey::from_str(owner).ok())
            .ok_or_else(|| malformed(method, "owner is not a pubkey"))?;
        let data = match value.get("data") {
            Some(Value::Array(parts)) => match parts.as_slice() {
                [Value::String(encoded), Value::String(encoding)]
                    if encoding == "base64" =>
                {
                    STANDARD
                        .decode(encoded)
                        .map_err(|error| malformed(method, &error.to_string()))?
                }
                _ => return Err(malformed(method, "data is not [base64, \"base64\"]")),
            },
            _ => return Err(malformed(method, "data is not [base64, \"base64\"]")),
        };
        Ok(Some(LoaderAccount { owner, data }))
    }

    pub fn get_latest_blockhash(&mut self) -> Result<Hash, RpcError> {
        let method = "getLatestBlockhash";
        let result = self.call(method, json!([{"commitment": "confirmed"}]), None)?;
        result
            .pointer("/value/blockhash")
            .and_then(Value::as_str)
            .and_then(|blockhash| Hash::from_str(blockhash).ok())
            .ok_or_else(|| malformed(method, "value.blockhash is not a hash"))
    }

    pub fn send_write(
        &mut self,
        transaction: &WriteTransaction,
        deadline: Instant,
    ) -> Result<String, RpcError> {
        let method = "sendTransaction";
        let encoded = STANDARD.encode(transaction.wire_bytes());
        let result = self.call(
            method,
            json!([encoded, {"encoding": "base64", "skipPreflight": true}]),
            Some(deadline),
        )?;
        result
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| malformed(method, "result is not a signature"))
    }
}

fn malformed(method: &str, detail: &str) -> RpcError {
    RpcError::Malformed {
        method: method.to_string(),
        detail: detail.to_string(),
    }
}

fn parse_reply(method: &str, reply: &HttpReply) -> Result<Value, RpcError> {
    if !(200..300).contains(&reply.status) {
        return Err(RpcError::Http {
            method: method.to_string(),
            status: reply.status,
            excerpt: String::from_utf8_lossy(&reply.body)
                .chars()
                .take(BODY_EXCERPT_CHARS)
                .collect(),
        });
    }
    let mut body: Value = serde_json::from_slice(&reply.body)
        .map_err(|error| malformed(method, &error.to_string()))?;
    if let Some(error) = body.get("error").filter(|error| !error.is_null()) {
        return Err(RpcError::Response {
            method: method.to_string(),
            code: error
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    body.get_mut("result")
        .map(Value::take)
        .ok_or_else(|| malformed(method, "no result"))
}

#[cfg(test)]
pub(crate) mod fake_clock {
    use std::{cell::Cell, cell::RefCell, rc::Rc};

    use super::*;

    #[derive(Clone)]
    pub(crate) struct FakeClock {
        start: Instant,
        elapsed: Rc<Cell<Duration>>,
        sleeps: Rc<RefCell<Vec<Duration>>>,
    }

    impl FakeClock {
        pub(crate) fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: Rc::new(Cell::new(Duration::ZERO)),
                sleeps: Rc::new(RefCell::new(Vec::new())),
            }
        }

        pub(crate) fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }

        pub(crate) fn sleeps(&self) -> Vec<Duration> {
            self.sleeps.borrow().clone()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }

        fn sleep(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
            self.sleeps.borrow_mut().push(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use solana_keypair::Keypair;

    use super::{fake_clock::FakeClock, *};

    struct ScriptedTransport {
        clock: FakeClock,
        replies: VecDeque<HttpReply>,
        posts: Rc<RefCell<Vec<(Duration, Value)>>>,
    }

    impl Transport for ScriptedTransport {
        fn post(&mut self, body: &str) -> Result<HttpReply, TransportError> {
            self.posts
                .borrow_mut()
                .push((self.clock.elapsed(), serde_json::from_str(body)?));
            self.replies
                .pop_front()
                .ok_or_else(|| "script exhausted".into())
        }
    }

    fn ok(result: Value) -> HttpReply {
        HttpReply {
            status: 200,
            retry_after: None,
            body: json!({"jsonrpc": "2.0", "id": 1, "result": result})
                .to_string()
                .into_bytes(),
        }
    }

    fn too_many(retry_after: Option<&str>) -> HttpReply {
        HttpReply {
            status: 429,
            retry_after: retry_after.map(str::to_string),
            body: b"Too many requests".to_vec(),
        }
    }

    fn blockhash_reply() -> HttpReply {
        ok(json!({"context": {"slot": 1}, "value": {
            "blockhash": Hash::new_from_array([4; 32]).to_string(),
            "lastValidBlockHeight": 100,
        }}))
    }

    type Posts = Rc<RefCell<Vec<(Duration, Value)>>>;

    fn rpc(
        replies: Vec<HttpReply>,
        rate: f64,
    ) -> (PacedRpc<ScriptedTransport, FakeClock>, FakeClock, Posts) {
        let clock = FakeClock::new();
        let posts = Rc::new(RefCell::new(Vec::new()));
        let transport = ScriptedTransport {
            clock: clock.clone(),
            replies: replies.into(),
            posts: posts.clone(),
        };
        (PacedRpc::new(transport, clock.clone(), rate), clock, posts)
    }

    fn times(posts: &Posts) -> Vec<Duration> {
        posts.borrow().iter().map(|(time, _)| *time).collect()
    }

    #[test]
    fn pacer_spaces_every_request_by_the_interval() {
        let (mut rpc, clock, posts) = rpc(vec![blockhash_reply(); 4], 2.0);

        for _ in 0..4 {
            rpc.get_latest_blockhash().unwrap();
        }

        assert_eq!(
            times(&posts),
            [0, 500, 1_000, 1_500].map(Duration::from_millis)
        );
        assert_eq!(clock.sleeps(), vec![Duration::from_millis(500); 3]);
        assert_eq!(rpc.requests(), 4);
    }

    #[test]
    fn pacer_does_not_sleep_when_the_interval_already_passed() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(0.5);

        pacer.wait_turn(&clock);
        clock.sleep(Duration::from_secs(3));
        pacer.wait_turn(&clock);
        pacer.wait_turn(&clock);

        assert_eq!(pacer.interval(), Duration::from_secs(2));
        assert_eq!(
            clock.sleeps(),
            vec![Duration::from_secs(3), Duration::from_secs(2)]
        );
    }

    #[test]
    fn rate_limited_request_honours_retry_after_and_is_retried_unchanged() {
        let (mut rpc, _, posts) = rpc(vec![too_many(Some("7")), blockhash_reply()], 2.0);

        assert_eq!(
            rpc.get_latest_blockhash().unwrap(),
            Hash::new_from_array([4; 32])
        );

        let posts = posts.borrow();
        assert_eq!(
            posts.iter().map(|(time, _)| *time).collect::<Vec<_>>(),
            vec![Duration::ZERO, Duration::from_secs(7)]
        );
        assert_eq!(posts[0].1, posts[1].1);
        assert_eq!(rpc.requests(), 2);
    }

    #[test]
    fn rate_limited_request_without_retry_after_backs_off_exponentially_to_sixty() {
        let mut replies = vec![too_many(None); 7];
        replies.push(blockhash_reply());
        let (mut rpc, clock, _) = rpc(replies, MAX_REQUESTS_PER_SECOND);

        rpc.get_latest_blockhash().unwrap();

        assert_eq!(
            clock.sleeps(),
            [2, 4, 8, 16, 32, 60, 60].map(Duration::from_secs)
        );
    }

    #[test]
    fn retry_after_that_is_not_seconds_falls_back_to_the_backoff() {
        assert_eq!(
            backoff(Some("Wed, 21 Oct 2026 07:28:00 GMT"), 1),
            Duration::from_secs(4)
        );
        assert_eq!(backoff(Some(" 7 "), 0), Duration::from_secs(7));
        assert_eq!(backoff(None, 40), MAX_BACKOFF);
    }

    #[test]
    fn retry_after_below_the_first_backoff_is_raised_to_it() {
        assert_eq!(backoff(Some("0"), 5), FIRST_BACKOFF);
        assert_eq!(backoff(Some("1"), 0), FIRST_BACKOFF);
    }

    #[test]
    fn zero_retry_after_waits_the_first_backoff() {
        let (mut rpc, _, posts) = rpc(vec![too_many(Some("0")), blockhash_reply()], 2.0);

        rpc.get_latest_blockhash().unwrap();

        assert_eq!(times(&posts), vec![Duration::ZERO, FIRST_BACKOFF]);
    }

    #[test]
    fn sustained_zero_retry_after_gives_up_within_the_budget() {
        let (mut rpc, clock, posts) = rpc(vec![too_many(Some("0")); 1_000], 2.0);

        let error = rpc.get_latest_blockhash().unwrap_err();

        assert!(
            matches!(error, RpcError::RateLimited { budget, .. } if budget == RATE_LIMIT_BUDGET),
            "{error}"
        );
        assert!(clock.elapsed() <= RATE_LIMIT_BUDGET);
        assert_eq!(posts.borrow().len(), 151);
    }

    #[test]
    fn rate_limiting_beyond_the_budget_is_a_clear_error() {
        let (mut rpc, clock, posts) =
            rpc(vec![too_many(None); 20], MAX_REQUESTS_PER_SECOND);

        let error = rpc.get_latest_blockhash().unwrap_err();

        assert!(matches!(
            error,
            RpcError::RateLimited { attempts: 8, elapsed, .. }
                if elapsed == Duration::from_secs(242)
        ));
        assert!(clock.elapsed() <= RATE_LIMIT_BUDGET);
        assert_eq!(posts.borrow().len(), 9);
        assert_eq!(
            error.to_string(),
            "getLatestBlockhash: still rate limited (HTTP 429) after 8 retries over 242s, \
             the budget is 300s"
        );
    }

    #[test]
    fn rate_limited_send_stops_retrying_at_its_deadline() {
        let authority = Keypair::new_from_array([1; 32]);
        let transaction = WriteTransaction::new(
            &Pubkey::new_from_array([7; 32]),
            0,
            &[1, 2],
            &authority,
            &authority,
            Hash::new_from_array([4; 32]),
        )
        .unwrap();
        let (mut rpc, clock, posts) = rpc(vec![too_many(None); 10], 2.0);
        let deadline = clock.now() + Duration::from_secs(10);

        let error = rpc.send_write(&transaction, deadline).unwrap_err();

        assert!(
            matches!(
                error,
                RpcError::RateLimited { attempts: 2, budget, .. }
                    if budget == Duration::from_secs(10)
            ),
            "{error}"
        );
        assert_eq!(times(&posts), [0, 2, 6].map(Duration::from_secs));
        assert!(clock.now() <= deadline);
    }

    #[test]
    fn other_http_status_is_not_retried() {
        let reply = HttpReply {
            status: 503,
            retry_after: Some("1".to_string()),
            body: b"unavailable".to_vec(),
        };
        let (mut rpc, _, posts) = rpc(vec![reply], 2.0);

        let error = rpc.get_latest_blockhash().unwrap_err();

        assert_eq!(
            error.to_string(),
            "getLatestBlockhash: HTTP 503: unavailable"
        );
        assert_eq!(posts.borrow().len(), 1);
    }

    #[test]
    fn json_rpc_error_is_reported_with_its_code() {
        let reply = HttpReply {
            status: 200,
            retry_after: None,
            body: json!({"jsonrpc": "2.0", "id": 1, "error": {
                "code": -32002, "message": "Blockhash not found",
            }})
            .to_string()
            .into_bytes(),
        };
        let (mut rpc, _, _) = rpc(vec![reply], 2.0);

        let error = rpc.get_latest_blockhash().unwrap_err();

        assert!(matches!(error, RpcError::Response { code: -32002, .. }));
    }

    #[test]
    fn account_info_requests_base64_and_decodes_owner_and_data() {
        let owner = Pubkey::new_from_array([8; 32]);
        let address = Pubkey::new_from_array([9; 32]);
        let reply = ok(json!({"context": {"slot": 1}, "value": {
            "lamports": 1, "owner": owner.to_string(), "executable": false,
            "rentEpoch": 0, "space": 3, "data": [STANDARD.encode([1, 2, 3]), "base64"],
        }}));
        let (mut rpc, _, posts) = rpc(vec![reply, ok(json!({"value": null}))], 2.0);

        let present = rpc.get_account_info(&address).unwrap();
        let absent = rpc.get_account_info(&address).unwrap();

        assert_eq!(
            present,
            Some(LoaderAccount {
                owner,
                data: vec![1, 2, 3]
            })
        );
        assert_eq!(absent, None);
        let posts = posts.borrow();
        assert_eq!(posts[0].1["method"], "getAccountInfo");
        assert_eq!(
            posts[0].1["params"],
            json!([address.to_string(), {"encoding": "base64", "commitment": "finalized"}])
        );
        assert_ne!(posts[0].1["id"], posts[1].1["id"]);
    }

    #[test]
    fn send_write_posts_the_signed_wire_bytes_without_preflight() {
        let authority = Keypair::new_from_array([1; 32]);
        let transaction = WriteTransaction::new(
            &Pubkey::new_from_array([7; 32]),
            0,
            &[1, 2],
            &authority,
            &authority,
            Hash::new_from_array([4; 32]),
        )
        .unwrap();
        let (mut rpc, clock, posts) = rpc(vec![ok(json!("signature"))], 2.0);
        let deadline = clock.now() + RATE_LIMIT_BUDGET;

        assert_eq!(rpc.send_write(&transaction, deadline).unwrap(), "signature");

        let posts = posts.borrow();
        assert_eq!(posts[0].1["method"], "sendTransaction");
        assert_eq!(
            posts[0].1["params"],
            json!([
                STANDARD.encode(transaction.wire_bytes()),
                {"encoding": "base64", "skipPreflight": true},
            ])
        );
    }
}
