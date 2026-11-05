use std::time::{Duration, Instant};

use serde::Serialize;
use solana_keypair::{Keypair, Signer};
use solana_program::pubkey::Pubkey;
use solana_transaction::{Hash, SignerError};
use thiserror::Error;

use crate::{
    loader_buffer::{
        BufferError, BufferStatus, SignerLayout, WriteTransaction, buffer_program,
        buffer_status, check_writable, chunk_ranges, differing_chunks, max_chunk_bytes,
        sha256_hex,
    },
    paced_rpc::{Clock, PacedRpc, RATE_LIMIT_BUDGET, RpcError, Transport},
};

pub const BLOCKHASH_MAX_AGE: Duration = Duration::from_secs(20);
pub const BLOCKHASH_SEND_WINDOW: Duration = Duration::from_secs(60);
const FIRST_SERVER_ERROR: u16 = 500;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Buffer(#[from] BufferError),
    #[error("failed to sign a write: {0}")]
    Signer(#[from] SignerError),
    #[error("chunk offset {offset} does not fit the 32-bit loader write offset")]
    Offset { offset: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    pub max_passes: u32,
    pub settle: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassReport {
    pub pass: u32,
    pub max_passes: u32,
    pub chunks_differing: usize,
    pub chunks_total: usize,
    pub transactions_sent: usize,
    pub send_errors: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WriteSummary {
    pub passes: u32,
    pub transactions_sent: usize,
    pub chunks_total: usize,
    pub complete: bool,
    pub program_sha256: String,
    #[serde(skip)]
    pub chunks_missing: usize,
}

pub fn read_status<T: Transport, C: Clock>(
    rpc: &mut PacedRpc<T, C>,
    buffer: &Pubkey,
    program: &[u8],
) -> Result<BufferStatus, UploadError> {
    let account = rpc.get_account_info(buffer)?;
    Ok(buffer_status(
        buffer,
        account.as_ref(),
        program,
        max_chunk_bytes(SignerLayout::SeparateFeePayer),
    )?)
}

#[derive(Clone, Copy)]
struct Blockhash {
    hash: Hash,
    fetched_at: Instant,
}

fn fresh_blockhash<T: Transport, C: Clock>(
    rpc: &mut PacedRpc<T, C>,
    current: &mut Option<Blockhash>,
) -> Result<Blockhash, RpcError> {
    let now = rpc.clock().now();
    match *current {
        Some(blockhash)
            if now.duration_since(blockhash.fetched_at) < BLOCKHASH_MAX_AGE =>
        {
            Ok(blockhash)
        }
        _ => {
            let hash = rpc.get_latest_blockhash()?;
            let blockhash = Blockhash {
                hash,
                fetched_at: rpc.clock().now(),
            };
            *current = Some(blockhash);
            Ok(blockhash)
        }
    }
}

struct Signers<'a> {
    authority: &'a Keypair,
    fee_payer: &'a Keypair,
}

enum Sent {
    Accepted,
    Failed,
}

fn is_transient_send_failure(error: &RpcError) -> bool {
    match error {
        RpcError::Response { .. }
        | RpcError::Transport { .. }
        | RpcError::Malformed { .. } => true,
        RpcError::Http { status, .. } => *status >= FIRST_SERVER_ERROR,
        RpcError::Url { .. } | RpcError::RateLimited { .. } => false,
    }
}

fn send_chunk<T: Transport, C: Clock>(
    rpc: &mut PacedRpc<T, C>,
    blockhash: &mut Option<Blockhash>,
    buffer: &Pubkey,
    offset: u32,
    bytes: &[u8],
    signers: &Signers<'_>,
) -> Result<Sent, UploadError> {
    let started = rpc.clock().now();
    let mut posts = 0;
    loop {
        let current = fresh_blockhash(rpc, blockhash)?;
        let transaction = WriteTransaction::new(
            buffer,
            offset,
            bytes,
            signers.authority,
            signers.fee_payer,
            current.hash,
        )?;
        match rpc.send_write(&transaction, current.fetched_at + BLOCKHASH_SEND_WINDOW) {
            Ok(_) => return Ok(Sent::Accepted),
            Err(RpcError::RateLimited {
                method, attempts, ..
            }) => {
                posts += attempts + 1;
                let elapsed = rpc.clock().now().duration_since(started);
                if elapsed >= RATE_LIMIT_BUDGET {
                    return Err(RpcError::RateLimited {
                        method,
                        attempts: posts - 1,
                        elapsed,
                        budget: RATE_LIMIT_BUDGET,
                    }
                    .into());
                }
                *blockhash = None;
            }
            Err(error) if is_transient_send_failure(&error) => return Ok(Sent::Failed),
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn write_buffer<T: Transport, C: Clock>(
    rpc: &mut PacedRpc<T, C>,
    buffer: &Pubkey,
    program: &[u8],
    authority: &Keypair,
    fee_payer: &Keypair,
    options: WriteOptions,
    mut progress: impl FnMut(&PassReport),
) -> Result<WriteSummary, UploadError> {
    let chunk_bytes =
        max_chunk_bytes(SignerLayout::of(&authority.pubkey(), &fee_payer.pubkey()));
    let chunks_total = chunk_ranges(program.len(), chunk_bytes).count();
    let signers = Signers {
        authority,
        fee_payer,
    };
    let mut blockhash = None;
    let mut passes = 0;
    let mut transactions_sent = 0;
    loop {
        let account = rpc.get_account_info(buffer)?;
        check_writable(buffer, account.as_ref(), program.len(), &authority.pubkey())?;
        let stored = account
            .as_ref()
            .map(|account| buffer_program(&account.data))
            .unwrap_or_default();
        let differing = differing_chunks(program, stored, chunk_bytes);
        if differing.is_empty() || passes == options.max_passes {
            return Ok(WriteSummary {
                passes,
                transactions_sent,
                chunks_total,
                complete: differing.is_empty(),
                program_sha256: sha256_hex(program),
                chunks_missing: differing.len(),
            });
        }
        passes += 1;
        let mut report = PassReport {
            pass: passes,
            max_passes: options.max_passes,
            chunks_differing: differing.len(),
            chunks_total,
            transactions_sent: 0,
            send_errors: 0,
        };
        for range in differing {
            let offset = u32::try_from(range.start).map_err(|_| UploadError::Offset {
                offset: range.start,
            })?;
            let bytes = program.get(range).unwrap_or_default();
            match send_chunk(rpc, &mut blockhash, buffer, offset, bytes, &signers)? {
                Sent::Accepted => {}
                Sent::Failed => report.send_errors += 1,
            }
            report.transactions_sent += 1;
        }
        transactions_sent += report.transactions_sent;
        progress(&report);
        rpc.clock().sleep(options.settle);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap, rc::Rc};

    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde_json::{Value, json};
    use solana_loader_v3_interface::{
        instruction::UpgradeableLoaderInstruction, state::UpgradeableLoaderState,
    };
    use solana_sdk_ids::bpf_loader_upgradeable;
    use solana_transaction::Transaction;

    use super::*;
    use crate::{
        loader_buffer::{BUFFER_METADATA_SIZE, BufferError},
        paced_rpc::{
            HttpReply, MAX_REQUESTS_PER_SECOND, TransportError, fake_clock::FakeClock,
        },
    };

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        JsonRpc,
        Status(u16),
        Transport,
        TooMany(Option<&'static str>),
    }

    type FaultPlan = Box<dyn FnMut(&str, usize) -> Option<Fault>>;

    struct Chain {
        clock: FakeClock,
        data: Vec<u8>,
        owner: Pubkey,
        writes_seen: usize,
        drop_write: Box<dyn FnMut(usize) -> bool>,
        fault: FaultPlan,
        calls: HashMap<String, usize>,
        data_after_first_send: Option<Vec<u8>>,
        requests: Vec<(Duration, String)>,
        blockhashes: u8,
        signed_blockhashes: Vec<Hash>,
        posted_blockhashes: Vec<(Duration, Hash)>,
    }

    #[derive(Clone)]
    struct FakeChain(Rc<RefCell<Chain>>);

    impl FakeChain {
        fn methods(&self) -> Vec<String> {
            let chain = self.0.borrow();
            chain
                .requests
                .iter()
                .map(|(_, method)| method.clone())
                .collect()
        }

        fn times_of(&self, method: &str) -> Vec<Duration> {
            let chain = self.0.borrow();
            chain
                .requests
                .iter()
                .filter(|(_, called)| called == method)
                .map(|(time, _)| *time)
                .collect()
        }

        fn apply(chain: &mut Chain, transaction: &Transaction) {
            assert!(transaction.verify().is_ok());
            let [instruction] = transaction.message.instructions.as_slice() else {
                panic!("one instruction per write transaction");
            };
            let keys = &transaction.message.account_keys;
            assert_eq!(
                keys.get(usize::from(instruction.program_id_index)),
                Some(&bpf_loader_upgradeable::ID)
            );
            let UpgradeableLoaderInstruction::Write { offset, bytes } =
                bincode::deserialize(&instruction.data).unwrap()
            else {
                panic!("only loader Write instructions are sent");
            };
            chain
                .signed_blockhashes
                .push(transaction.message.recent_blockhash);
            let index = chain.writes_seen;
            chain.writes_seen += 1;
            if (chain.drop_write)(index) {
                return;
            }
            let start = BUFFER_METADATA_SIZE + offset as usize;
            chain
                .data
                .get_mut(start..start + bytes.len())
                .unwrap()
                .copy_from_slice(&bytes);
        }
    }

    impl Transport for FakeChain {
        fn post(&mut self, body: &str) -> Result<HttpReply, TransportError> {
            let request: Value = serde_json::from_str(body)?;
            let mut chain = self.0.borrow_mut();
            let method = request["method"].as_str().unwrap().to_string();
            let now = chain.clock.elapsed();
            chain.requests.push((now, method.clone()));
            let call = *chain
                .calls
                .entry(method.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1)
                - 1;
            if method == "sendTransaction" {
                let wire = STANDARD.decode(request["params"][0].as_str().unwrap())?;
                let transaction: Transaction = bincode::deserialize(&wire)?;
                chain
                    .posted_blockhashes
                    .push((now, transaction.message.recent_blockhash));
            }
            let reply = match (chain.fault)(&method, call) {
                None => None,
                Some(Fault::JsonRpc) => Some(HttpReply {
                    status: 200,
                    retry_after: None,
                    body: json!({"error": {"code": -32005, "message": "node is behind"}})
                        .to_string()
                        .into_bytes(),
                }),
                Some(Fault::Status(status)) => Some(HttpReply {
                    status,
                    retry_after: None,
                    body: b"bad gateway".to_vec(),
                }),
                Some(Fault::Transport) => return Err("connection reset".into()),
                Some(Fault::TooMany(retry_after)) => Some(HttpReply {
                    status: 429,
                    retry_after: retry_after.map(str::to_string),
                    body: Vec::new(),
                }),
            };
            if let Some(reply) = reply {
                return Ok(reply);
            }
            let reply = match method.as_str() {
                "getAccountInfo" => json!({"result": {"context": {"slot": 1}, "value": {
                    "lamports": 1, "owner": chain.owner.to_string(), "executable": false,
                    "rentEpoch": 0, "space": chain.data.len(),
                    "data": [STANDARD.encode(&chain.data), "base64"],
                }}}),
                "getLatestBlockhash" => {
                    chain.blockhashes += 1;
                    let blockhash = Hash::new_from_array([chain.blockhashes; 32]);
                    json!({"result": {"context": {"slot": 1}, "value": {
                        "blockhash": blockhash.to_string(), "lastValidBlockHeight": 1,
                    }}})
                }
                "sendTransaction" => {
                    let wire = STANDARD.decode(request["params"][0].as_str().unwrap())?;
                    let transaction: Transaction = bincode::deserialize(&wire)?;
                    Self::apply(&mut chain, &transaction);
                    if let Some(data) = chain.data_after_first_send.take() {
                        chain.data = data;
                    }
                    json!({"result": transaction.signatures[0].to_string()})
                }
                other => panic!("unexpected method {other}"),
            };
            Ok(HttpReply {
                status: 200,
                retry_after: None,
                body: reply.to_string().into_bytes(),
            })
        }
    }

    fn authority() -> Keypair {
        Keypair::new_from_array([1; 32])
    }

    fn fee_payer() -> Keypair {
        Keypair::new_from_array([2; 32])
    }

    fn buffer_key() -> Pubkey {
        Pubkey::new_from_array([7; 32])
    }

    fn program(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 253) as u8 + 1).collect()
    }

    fn buffer_data(authority: Option<Pubkey>, stored: &[u8]) -> Vec<u8> {
        let mut data = bincode::serialize(&UpgradeableLoaderState::Buffer {
            authority_address: authority,
        })
        .unwrap();
        data.resize(BUFFER_METADATA_SIZE, 0);
        data.extend_from_slice(stored);
        data
    }

    fn chain(
        data: Vec<u8>,
        drop_write: impl FnMut(usize) -> bool + 'static,
        rate: f64,
    ) -> (FakeChain, PacedRpc<FakeChain, FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        let chain = FakeChain(Rc::new(RefCell::new(Chain {
            clock: clock.clone(),
            data,
            owner: bpf_loader_upgradeable::ID,
            writes_seen: 0,
            drop_write: Box::new(drop_write),
            fault: Box::new(|_, _| None),
            calls: HashMap::new(),
            data_after_first_send: None,
            requests: Vec::new(),
            blockhashes: 0,
            signed_blockhashes: Vec::new(),
            posted_blockhashes: Vec::new(),
        })));
        let rpc = PacedRpc::new(chain.clone(), clock.clone(), rate);
        (chain, rpc, clock)
    }

    fn options(max_passes: u32) -> WriteOptions {
        WriteOptions {
            max_passes,
            settle: Duration::from_secs(20),
        }
    }

    fn separate_chunk() -> usize {
        max_chunk_bytes(SignerLayout::SeparateFeePayer)
    }

    #[test]
    fn loop_converges_to_an_exact_copy_despite_dropped_writes() {
        let program = program(separate_chunk() * 6 + 17);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) =
            chain(data, |index| index % 3 == 0, MAX_REQUESTS_PER_SECOND);
        let mut reports = Vec::new();

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |report| reports.push(*report),
        )
        .unwrap();

        assert_eq!(
            chain.0.borrow().data,
            buffer_data(Some(authority().pubkey()), &program)
        );
        assert!(summary.complete);
        assert_eq!(summary.chunks_total, 7);
        assert_eq!(summary.chunks_missing, 0);
        assert_eq!(summary.passes, reports.len() as u32);
        assert!(summary.passes >= 2);
        assert_eq!(
            summary.transactions_sent,
            chain.times_of("sendTransaction").len()
        );
        assert_eq!(reports[0].chunks_differing, 7);
        assert_eq!(reports[1].chunks_differing, 3);
        assert_eq!(summary.program_sha256, sha256_hex(&program));
    }

    #[test]
    fn only_differing_chunks_are_written() {
        let chunk = separate_chunk();
        let program = program(chunk * 4);
        let mut stored = program.clone();
        stored[chunk + 1] = 0;
        let data = buffer_data(Some(authority().pubkey()), &stored);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap();

        assert_eq!(summary.transactions_sent, 1);
        assert_eq!(summary.passes, 1);
        assert_eq!(
            chain.methods(),
            [
                "getAccountInfo",
                "getLatestBlockhash",
                "sendTransaction",
                "getAccountInfo"
            ]
        );
    }

    #[test]
    fn complete_buffer_sends_nothing() {
        let program = program(100);
        let data = buffer_data(Some(authority().pubkey()), &program);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &authority(),
            options(10),
            |_| {},
        )
        .unwrap();

        assert!(summary.complete);
        assert_eq!(summary.passes, 0);
        assert_eq!(chain.methods(), ["getAccountInfo"]);
    }

    #[test]
    fn exhausted_passes_report_the_missing_chunks() {
        let program = program(separate_chunk() * 2);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (_, mut rpc, _) = chain(data, |_| true, MAX_REQUESTS_PER_SECOND);
        let mut reports = Vec::new();

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(3),
            |report| reports.push(*report),
        )
        .unwrap();

        assert!(!summary.complete);
        assert_eq!(summary.passes, 3);
        assert_eq!(summary.chunks_missing, 2);
        assert_eq!(summary.transactions_sent, 6);
        assert_eq!(reports.len(), 3);
    }

    #[test]
    fn rejected_sends_are_counted_and_retried_next_pass() {
        let program = program(separate_chunk() * 2);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);
        chain.0.borrow_mut().fault = Box::new(|method, call| {
            (method == "sendTransaction" && call == 0).then_some(Fault::JsonRpc)
        });
        let mut reports = Vec::new();

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |report| reports.push(*report),
        )
        .unwrap();

        assert!(summary.complete);
        assert_eq!(reports[0].send_errors, 1);
        assert_eq!(reports[1].chunks_differing, 1);
        assert_eq!(summary.transactions_sent, 3);
    }

    #[test]
    fn server_and_transport_failures_on_send_are_counted_and_retried_next_pass() {
        let program = program(separate_chunk() * 4);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);
        chain.0.borrow_mut().fault = Box::new(|method, call| match (method, call) {
            ("sendTransaction", 0) => Some(Fault::Status(502)),
            ("sendTransaction", 1) => Some(Fault::Transport),
            ("sendTransaction", 2) => Some(Fault::Status(503)),
            _ => None,
        });
        let mut reports = Vec::new();

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |report| reports.push(*report),
        )
        .unwrap();

        assert!(summary.complete);
        assert_eq!(reports[0].send_errors, 3);
        assert_eq!(reports[0].transactions_sent, 4);
        assert_eq!(reports[1].chunks_differing, 3);
        assert_eq!(reports[1].send_errors, 0);
        assert_eq!(summary.transactions_sent, 7);
    }

    #[test]
    fn client_error_status_on_send_aborts_the_write() {
        let program = program(separate_chunk() * 2);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);
        chain.0.borrow_mut().fault = Box::new(|method, _| {
            (method == "sendTransaction").then_some(Fault::Status(403))
        });

        let error = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap_err();

        assert!(
            matches!(error, UploadError::Rpc(RpcError::Http { status: 403, .. })),
            "{error}"
        );
        assert_eq!(chain.times_of("sendTransaction").len(), 1);
    }

    #[test]
    fn rate_limited_send_is_re_signed_once_its_blockhash_window_closes() {
        let program = program(10);
        let data = buffer_data(Some(authority().pubkey()), &[0; 10]);
        let (chain, mut rpc, _) = chain(data, |_| false, 2.0);
        chain.0.borrow_mut().fault = Box::new(|method, call| {
            (method == "sendTransaction" && call < 5).then_some(Fault::TooMany(None))
        });

        let summary = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap();

        assert!(summary.complete);
        assert_eq!(summary.transactions_sent, 1);
        assert_eq!(
            chain.methods(),
            [
                "getAccountInfo",
                "getLatestBlockhash",
                "sendTransaction",
                "sendTransaction",
                "sendTransaction",
                "sendTransaction",
                "sendTransaction",
                "getLatestBlockhash",
                "sendTransaction",
                "getAccountInfo",
            ]
        );
        let fetches = chain.times_of("getLatestBlockhash");
        let posted = chain.0.borrow().posted_blockhashes.clone();
        for (sent_at, blockhash) in posted {
            let fetch = fetches[usize::from(blockhash.to_bytes()[0]) - 1];
            assert!(
                sent_at - fetch <= BLOCKHASH_SEND_WINDOW,
                "{sent_at:?} {fetch:?}"
            );
        }
    }

    #[test]
    fn persistently_rate_limited_send_gives_up_within_the_budget() {
        let program = program(10);
        let data = buffer_data(Some(authority().pubkey()), &[0; 10]);
        let (chain, mut rpc, clock) = chain(data, |_| false, 2.0);
        chain.0.borrow_mut().fault = Box::new(|method, _| {
            (method == "sendTransaction").then_some(Fault::TooMany(None))
        });

        let error = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap_err();

        assert!(
            matches!(
                error,
                UploadError::Rpc(RpcError::RateLimited { ref method, budget, .. })
                    if method == "sendTransaction" && budget == RATE_LIMIT_BUDGET
            ),
            "{error}"
        );
        assert!(clock.elapsed() <= RATE_LIMIT_BUDGET + BLOCKHASH_SEND_WINDOW);
        assert_eq!(chain.times_of("getAccountInfo").len(), 1);
    }

    #[test]
    fn blockhash_age_counts_from_the_reply_not_from_the_rate_limited_request() {
        let program = program(separate_chunk() * 2);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, 2.0);
        chain.0.borrow_mut().fault = Box::new(|method, call| {
            (method == "getLatestBlockhash" && call == 0)
                .then_some(Fault::TooMany(Some("30")))
        });

        write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap();

        assert_eq!(
            chain.methods(),
            [
                "getAccountInfo",
                "getLatestBlockhash",
                "getLatestBlockhash",
                "sendTransaction",
                "sendTransaction",
                "getAccountInfo",
            ]
        );
    }

    #[test]
    fn authority_change_between_passes_stops_the_writes() {
        let program = program(separate_chunk() * 2);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| true, MAX_REQUESTS_PER_SECOND);
        chain.0.borrow_mut().data_after_first_send = Some(buffer_data(
            Some(fee_payer().pubkey()),
            &vec![0; program.len()],
        ));

        let error = write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap_err();

        assert!(
            matches!(
                error,
                UploadError::Buffer(BufferError::WrongAuthority { stored, .. })
                    if stored == fee_payer().pubkey()
            ),
            "{error}"
        );
        assert_eq!(
            chain.methods(),
            [
                "getAccountInfo",
                "getLatestBlockhash",
                "sendTransaction",
                "sendTransaction",
                "getAccountInfo",
            ]
        );
    }

    #[test]
    fn settle_time_separates_the_passes() {
        let program = program(10);
        let data = buffer_data(Some(authority().pubkey()), &[0; 10]);
        let (chain, mut rpc, clock) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);

        write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &authority(),
            options(10),
            |_| {},
        )
        .unwrap();

        let reads = chain.times_of("getAccountInfo");
        assert_eq!(reads.len(), 2);
        assert!(reads[1] - reads[0] >= Duration::from_secs(20));
        assert!(clock.sleeps().contains(&Duration::from_secs(20)));
    }

    #[test]
    fn blockhash_is_refetched_only_once_it_is_twenty_seconds_old() {
        let program = program(separate_chunk() * 30);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, 0.5);

        write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap();

        let fetches = chain.times_of("getLatestBlockhash");
        assert!(fetches.len() >= 3, "{fetches:?}");
        assert!(
            fetches
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= BLOCKHASH_MAX_AGE),
            "{fetches:?}"
        );
        let signed = chain.0.borrow().signed_blockhashes.clone();
        let sends = chain.times_of("sendTransaction");
        for (sent_at, blockhash) in sends.iter().zip(signed) {
            let fetch = fetches
                .get(usize::from(blockhash.to_bytes()[0]) - 1)
                .unwrap();
            assert!(*sent_at - *fetch <= BLOCKHASH_MAX_AGE + Duration::from_secs(2));
        }
    }

    #[test]
    fn every_request_is_spaced_by_the_pacer() {
        let program = program(separate_chunk() * 3);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, 2.0);

        write_buffer(
            &mut rpc,
            &buffer_key(),
            &program,
            &authority(),
            &fee_payer(),
            options(10),
            |_| {},
        )
        .unwrap();

        let chain = chain.0.borrow();
        assert!(
            chain
                .requests
                .windows(2)
                .all(|pair| pair[1].0 - pair[0].0 >= Duration::from_millis(500))
        );
    }

    #[test]
    fn refusal_is_reported_before_any_send() {
        let program = program(10);
        let other = fee_payer().pubkey();
        for (data, owner) in [
            (
                buffer_data(Some(other), &[0; 10]),
                bpf_loader_upgradeable::ID,
            ),
            (buffer_data(None, &[0; 10]), bpf_loader_upgradeable::ID),
            (
                buffer_data(Some(authority().pubkey()), &[0; 11]),
                bpf_loader_upgradeable::ID,
            ),
            (buffer_data(Some(authority().pubkey()), &[0; 10]), other),
        ] {
            let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);
            chain.0.borrow_mut().owner = owner;

            let error = write_buffer(
                &mut rpc,
                &buffer_key(),
                &program,
                &authority(),
                &fee_payer(),
                options(10),
                |_| {},
            )
            .unwrap_err();

            assert!(matches!(error, UploadError::Buffer(_)), "{error}");
            assert_eq!(chain.methods(), ["getAccountInfo"]);
        }
    }

    #[test]
    fn status_uses_the_separate_fee_payer_chunk() {
        let program = program(separate_chunk() + 1);
        let data = buffer_data(Some(authority().pubkey()), &vec![0; program.len()]);
        let (chain, mut rpc, _) = chain(data, |_| false, MAX_REQUESTS_PER_SECOND);

        let status = read_status(&mut rpc, &buffer_key(), &program).unwrap();

        assert_eq!(status.chunk_bytes, separate_chunk());
        assert_eq!(status.chunks_total, 2);
        assert_eq!(status.chunks_missing, 2);
        assert_eq!(chain.methods(), ["getAccountInfo"]);
    }
}
