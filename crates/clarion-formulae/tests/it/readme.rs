use std::{path::Path, process::Command};

use crate::support::repo_root;

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

fn invocations(text: &str) -> Vec<(String, Vec<String>)> {
    fenced(text, "sh")
        .join("\n")
        .lines()
        .filter(|line| line.starts_with("target/release/clarion-formulae "))
        .map(|line| {
            let mut tokens = line.split_whitespace().skip(1);
            let subcommand = tokens.next().unwrap().to_owned();
            let flags = tokens
                .filter(|token| token.starts_with("--"))
                .map(str::to_owned)
                .collect();
            (subcommand, flags)
        })
        .collect()
}

fn help(subcommand: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_clarion-formulae"))
        .args([subcommand, "--help"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{subcommand}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn readme_has_at_most_one_hundred_fifty_words() {
    let count = words(&readme());

    assert!(count <= MAX_WORDS, "{count} words");
}

#[test]
fn build_command_names_this_package_and_runs_name_its_binary() {
    let commands = fenced(&readme(), "sh").join("\n");

    assert!(commands.contains(&format!("-p {}", env!("CARGO_PKG_NAME"))));
    assert!(
        commands
            .lines()
            .filter(|line| !line.starts_with("cargo "))
            .all(|line| line.starts_with("target/release/clarion-formulae "))
    );
    assert_eq!(
        Path::new(env!("CARGO_BIN_EXE_clarion-formulae"))
            .file_stem()
            .and_then(|stem| stem.to_str()),
        Some("clarion-formulae")
    );
}

#[test]
fn every_subcommand_and_flag_the_readme_names_exists_in_the_binary() {
    let invocations = invocations(&readme());
    let subcommands: Vec<&str> = invocations
        .iter()
        .map(|(subcommand, _)| subcommand.as_str())
        .collect();

    assert_eq!(subcommands, ["id", "build", "bind", "verify"]);
    for (subcommand, flags) in &invocations {
        let text = help(subcommand);
        for flag in flags {
            assert!(
                text.contains(&format!("{flag} ")) || text.contains(&format!("{flag}\n")),
                "{subcommand} {flag}"
            );
        }
    }
}

#[test]
fn files_the_readme_names_exist_in_the_repository() {
    let text = readme();

    for file in ["formulae/trace-completed.v1.json", "formulae/ids.txt"] {
        assert!(text.contains(file), "{file}");
        assert!(repo_root().join(file).is_file(), "{file}");
    }
}

#[test]
fn exit_codes_line_matches_the_binary_contract() {
    assert!(readme().contains("Exit codes: 0 success, 1 failure, 2 usage."));
}
