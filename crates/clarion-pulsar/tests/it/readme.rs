use std::path::Path;

use clarion_pulsar::{
    config::{FILE_KEYS, FileConfig},
    leader_slots,
    output::{file_name, validators_file_name},
};
use tokio::process::Command;

use crate::support::STAMP;

const MAX_WORDS: usize = 150;

fn readme() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .unwrap()
}

fn words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}

fn fenced(text: &str, language: &str) -> Vec<String> {
    let opening = format!("```{language}");
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        match current.as_mut() {
            None if line.trim() == opening => current = Some(Vec::new()),
            Some(lines) if line.trim() == "```" => {
                blocks.push(lines.join("\n"));
                current = None;
            }
            Some(lines) => lines.push(line),
            None => {}
        }
    }
    blocks
}

fn invocation_flags(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace().map(|token| token.trim_matches('`'));
            let program = tokens.next()?;
            program.ends_with("clarion-pulsar").then(|| {
                tokens
                    .filter(|token| token.starts_with("--"))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect()
}

fn file_name_patterns(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '`' || c == '|')
        .filter(|token| token.starts_with("clarion-pulsar-") && token.ends_with(".csv"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn readme_has_at_most_one_hundred_fifty_words() {
    let count = words(&readme());

    assert!(count <= MAX_WORDS, "{count} words");
}

#[test]
fn config_example_names_every_key_with_the_code_defaults() {
    let blocks = fenced(&readme(), "toml");

    assert_eq!(blocks.len(), 1);
    let example: FileConfig = toml::from_str(&blocks[0]).unwrap();
    for key in FILE_KEYS {
        assert!(
            blocks[0]
                .lines()
                .any(|line| line.starts_with(&format!("{key} = "))),
            "{key}"
        );
    }
    assert!(example.vantage.is_some());
    assert_eq!(
        FileConfig {
            vantage: None,
            ..example
        },
        FileConfig::default()
    );
}

#[test]
fn output_file_names_match_the_names_the_code_writes() {
    let written = [
        file_name(STAMP),
        validators_file_name(STAMP),
        leader_slots::file_name("devnet", 7),
    ];

    let named: Vec<String> = file_name_patterns(&readme())
        .into_iter()
        .map(|pattern| {
            pattern
                .replace("<ts>", STAMP)
                .replace("<cluster>", "devnet")
                .replace("<epoch>", "7")
        })
        .collect();

    assert_eq!(named, written);
}

#[test]
fn output_table_describes_the_rows_and_the_cache_rule_the_code_applies() {
    let text = readme();

    assert!(text.contains("| one row per identity and vote account |"));
    assert!(text.contains("while present and readable, getLeaderSchedule is skipped"));
}

#[test]
fn build_command_names_this_package_and_runs_name_its_binary() {
    let text = readme();
    let commands = fenced(&text, "sh").join("\n");

    assert!(commands.contains(&format!("-p {}", env!("CARGO_PKG_NAME"))));
    assert!(
        commands
            .lines()
            .filter(|line| !line.starts_with("cargo "))
            .all(|line| line.starts_with("target/release/clarion-pulsar "))
    );
    assert_eq!(
        Path::new(env!("CARGO_BIN_EXE_clarion-pulsar"))
            .file_stem()
            .and_then(|stem| stem.to_str()),
        Some("clarion-pulsar")
    );
}

#[test]
fn keyed_url_is_placed_in_the_config_file() {
    let text = readme();

    assert!(text.contains("keyed provider URL goes in `rpc_url` of the config file"));
    assert!(!invocation_flags(&text).contains(&"--rpc-url".to_owned()));
}

#[tokio::test]
async fn every_flag_the_readme_names_exists_in_the_binary() {
    let flags = invocation_flags(&readme());
    let help = Command::new(env!("CARGO_BIN_EXE_clarion-pulsar"))
        .arg("--help")
        .output()
        .await
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();

    assert_eq!(flags, ["--vantage", "--config", "--help"]);
    for flag in flags {
        assert!(
            help.contains(&format!("{flag} ")) || help.contains(&format!("{flag}\n")),
            "{flag}"
        );
    }
}
