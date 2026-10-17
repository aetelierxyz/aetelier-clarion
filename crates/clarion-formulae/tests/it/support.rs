use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use clarion_program_client::{
    Arm, ArmAggregates, WINDOW_DISCRIMINATOR, Window, parse_merkle_root, render_window,
    window_address,
};
use solana_keypair::Signature;
use solana_program::pubkey::Pubkey;

pub const FORMULAE_ID_HEX: &str =
    "d283858a6801c56d54fffa6ab4afdced38048313cf9c05e6cb8004ee503f2564";
pub const SOURCE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub const LEAVES: [&str; 4] = [
    "5c8792cb2a8c212e8c9cc8160f9f5610f7f232f1b7dcec6cd9a2955fedbc5d34",
    "c2e53cb32a3acfa42c4dd54f7e57a946fbc0b0307734a5bb173f1d23f129cb28",
    "95d128305e03d9dce43901fbd127d044c77822ee5602ac984cd1ec1255b5e5f1",
    "77c00a641b9255e9ae9d480c3362e542f1519d0310fa84e734946e4f1f31a72f",
];
pub const ROOT_HEX: &str =
    "85583a7ac2f218e83a96d8682a1c10655dacf0be2e831a00425540d7fbde793b";
pub const CORRECTED_LEDGER_SHA256: &str =
    "a7b30ddb64ceb33ebd8e28aaab5113b4e667e72fbfbf41f0c5a8a047d696ce6e";
pub const WINDOW_ID: u64 = 3;
pub const COMMIT_SLOT: u64 = 1_500;
pub const REVEAL_SLOT: u64 = 1_651;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

pub fn formulae_dir() -> PathBuf {
    repo_root().join("formulae")
}

pub fn formulae_file() -> PathBuf {
    formulae_dir().join("trace-completed.v1.json")
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/artifacts")
}

pub fn tmp(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("clarion-formulae")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn copy_fixtures(into: &Path) -> PathBuf {
    let dir = into.join("artifacts");
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
    }
    dir
}

pub fn formulae(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clarion-formulae"))
        .args(args)
        .output()
        .unwrap()
}

pub fn formulae_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clarion-formulae"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

pub fn program_id() -> Pubkey {
    Pubkey::new_from_array([7; 32])
}

pub fn signature(seed: u8) -> String {
    Signature::from([seed; 64]).to_string()
}

pub fn zero_arms() -> [ArmAggregates; 4] {
    Arm::ALL.map(ArmAggregates::empty)
}

pub fn window_json(
    program_id: &Pubkey,
    window_id: u64,
    root_hex: &str,
    reveal_slot: u64,
    aggregates: [ArmAggregates; 4],
) -> String {
    let (address, bump) = window_address(program_id, window_id);
    let window = Window {
        discriminator: WINDOW_DISCRIMINATOR,
        window_id,
        slot_start: 1_000 + window_id * 150,
        slot_end: 1_149 + window_id * 150,
        merkle_root: parse_merkle_root(root_hex).unwrap(),
        commit_slot: COMMIT_SLOT,
        reveal_slot,
        aggregates,
        bump,
    };
    render_window(&address, &window).unwrap()
}

pub fn revealed_window() -> String {
    window_json(&program_id(), WINDOW_ID, ROOT_HEX, REVEAL_SLOT, zero_arms())
}

pub fn write(path: &Path, text: &str) -> String {
    fs::write(path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

pub fn built(test: &str) -> (PathBuf, PathBuf) {
    built_with(test, &formulae_file())
}

pub fn built_with(test: &str, document: &Path) -> (PathBuf, PathBuf) {
    let dir = tmp(test);
    let record = dir.join("attestation.json");
    let output = formulae(&[
        "build",
        "--formulae",
        document.to_str().unwrap(),
        "--artifacts",
        fixtures().to_str().unwrap(),
        "--input",
        &format!("system.source_commit={SOURCE_COMMIT}"),
        "--input",
        "results.subject_id=replay",
        "--out",
        record.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    (dir, record)
}

pub fn bound(test: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (dir, record) = built(test);
    let window = dir.join("show.json");
    write(&window, &revealed_window());
    let output = formulae(&[
        "bind",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--cluster",
        "devnet",
        "--program-id",
        &program_id().to_string(),
        "--commit-signature",
        &signature(1),
        "--reveal-signature",
        &signature(2),
    ]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    (dir, record, window)
}

pub fn declaration(files: &[(&str, &str)]) -> String {
    let files: Vec<serde_json::Value> = files
        .iter()
        .map(|(filename, sha256)| serde_json::json!({"filename": filename, "sha256": sha256}))
        .collect();
    serde_json::json!({"schema": "formulae.withheld.v1", "files": files}).to_string()
}

pub fn edit_record(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    edit(&mut value);
    write(path, &serde_json::to_string_pretty(&value).unwrap());
}

pub fn published(dir: &Path, record: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    fs::copy(record, dir.join("attestation.json")).unwrap();
    for file in ["tracer_params.v1.json", "fillrates.json"] {
        fs::copy(fixtures().join(file), dir.join(file)).unwrap();
    }
    write(
        &dir.join("withheld.json"),
        &declaration(&[("corrected_ledger.json", CORRECTED_LEDGER_SHA256)]),
    );
    dir.join("attestation.json")
}
