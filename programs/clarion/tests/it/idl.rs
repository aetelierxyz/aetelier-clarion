use std::{fs, path::Path};

use clarion::idl::{DEVNET_INSTANCES, idl_path, render};
use clarion::{
    ARM_AGGREGATES_SIZE, ARM_COUNT, Arm, ClarionError, ClarionEvent, ClarionInstruction,
    PROPOSAL_DISCRIMINATOR, PROPOSAL_SEED, PROPOSAL_SIZE, STATE_DISCRIMINATOR,
    STATE_SEED, STATE_SIZE, WINDOW_DISCRIMINATOR, WINDOW_SEED, WINDOW_SIZE,
    proposal_address, state_address, window_address,
};
use serde_json::Value;
use solana_program::{instruction::Instruction, pubkey::Pubkey};
use solana_system_interface::program as system_program;

use crate::support::{
    GENESIS_SLOT, MIN_REVEAL_LAG_SLOTS, PROGRAM_ID, WINDOW_LEN, accept_instruction,
    aggregates, cancel_instruction, grid_commit_instruction, init_instruction,
    propose_instruction, reveal_instruction,
};

const WINDOW_ID: u64 = 3;

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn committed_idl(program_id: &str) -> String {
    fs::read_to_string(idl_path(crate_dir(), program_id)).unwrap()
}

fn root(program_id: &str) -> Value {
    serde_json::from_str(&committed_idl(program_id)).unwrap()
}

fn program() -> Value {
    let root = root(DEVNET_INSTANCES[0]);
    assert_eq!(root["kind"], "rootNode");
    assert_eq!(root["standard"], "codama");
    root["program"].clone()
}

fn entries<'a>(node: &'a Value, key: &str) -> &'a Vec<Value> {
    node[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is not an array"))
}

fn named<'a>(node: &'a Value, key: &str, name: &str) -> &'a Value {
    entries(node, key)
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("{key} holds no {name}"))
}

fn names(nodes: &[Value]) -> Vec<&str> {
    nodes
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect()
}

fn camel_case(pascal_case: &str) -> String {
    let mut characters = pascal_case.chars();
    let first = characters.next().unwrap().to_ascii_lowercase();
    std::iter::once(first).chain(characters).collect()
}

fn as_usize(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

fn type_size(program: &Value, node: &Value) -> usize {
    match node["kind"].as_str().unwrap() {
        "numberTypeNode" => match node["format"].as_str().unwrap() {
            "u8" | "i8" => 1,
            "u16" | "i16" => 2,
            "u32" | "i32" => 4,
            "u64" | "i64" => 8,
            "u128" | "i128" => 16,
            format => panic!("unsized number format {format}"),
        },
        "publicKeyTypeNode" => 32,
        "arrayTypeNode" => {
            assert_eq!(node["count"]["kind"], "fixedCountNode");
            as_usize(&node["count"]["value"]) * type_size(program, &node["item"])
        }
        "definedTypeLinkNode" => {
            let name = node["name"].as_str().unwrap();
            type_size(program, &named(program, "definedTypes", name)["type"])
        }
        "structTypeNode" => entries(node, "fields")
            .iter()
            .map(|field| type_size(program, &field["type"]))
            .sum(),
        "enumTypeNode" => {
            assert!(
                entries(node, "variants")
                    .iter()
                    .all(|variant| variant["kind"] == "enumEmptyVariantTypeNode")
            );
            type_size(program, &node["size"])
        }
        kind => panic!("unsized type node {kind}"),
    }
}

fn discriminator(node: &Value, fields: &[Value]) -> u64 {
    assert_eq!(
        node["discriminators"],
        serde_json::json!([{
            "kind": "fieldDiscriminatorNode",
            "name": "discriminator",
            "offset": 0
        }])
    );
    let field = &fields[0];
    assert_eq!(field["name"], "discriminator");
    assert_eq!(
        field["type"],
        serde_json::json!({"kind": "numberTypeNode", "format": "u8", "endian": "le"})
    );
    field["defaultValue"]["number"].as_u64().unwrap()
}

fn sample_instructions() -> [ClarionInstruction; 6] {
    [
        ClarionInstruction::Init {
            genesis_slot: GENESIS_SLOT,
            window_len: WINDOW_LEN,
            min_reveal_lag_slots: MIN_REVEAL_LAG_SLOTS,
        },
        ClarionInstruction::Commit {
            window_id: WINDOW_ID,
            slot_start: 0,
            slot_end: 0,
            merkle_root: [0; 32],
        },
        ClarionInstruction::Reveal {
            window_id: WINDOW_ID,
            aggregates: aggregates(),
        },
        ClarionInstruction::ProposeAuthority {
            new_authority: Pubkey::new_from_array([6; 32]),
        },
        ClarionInstruction::AcceptAuthority,
        ClarionInstruction::CancelAuthorityProposal,
    ]
}

fn sample_events() -> [ClarionEvent; 6] {
    let key = Pubkey::new_from_array([6; 32]);
    [
        ClarionEvent::Initialized {
            authority: key,
            genesis_slot: 0,
            window_len: 0,
            min_reveal_lag_slots: 0,
        },
        ClarionEvent::Committed {
            window_id: 0,
            slot_start: 0,
            slot_end: 0,
            merkle_root: [0; 32],
            commit_slot: 0,
        },
        ClarionEvent::Revealed {
            window_id: 0,
            reveal_slot: 0,
            aggregates: aggregates(),
        },
        ClarionEvent::AuthorityProposed {
            authority: key,
            proposed_authority: key,
        },
        ClarionEvent::AuthorityAccepted {
            previous_authority: key,
            authority: key,
        },
        ClarionEvent::AuthorityProposalCancelled {
            authority: key,
            proposed_authority: key,
        },
    ]
}

fn built_instructions() -> [(&'static str, Instruction); 7] {
    let authority = Pubkey::new_from_array([1; 32]);
    let successor = Pubkey::new_from_array([3; 32]);
    [
        (
            "init",
            init_instruction(&authority, GENESIS_SLOT, WINDOW_LEN, MIN_REVEAL_LAG_SLOTS),
        ),
        ("commit", grid_commit_instruction(&authority, 0)),
        ("commit", grid_commit_instruction(&authority, WINDOW_ID)),
        (
            "reveal",
            reveal_instruction(&authority, WINDOW_ID, aggregates()),
        ),
        (
            "proposeAuthority",
            propose_instruction(&authority, &successor),
        ),
        ("acceptAuthority", accept_instruction(&successor)),
        ("cancelAuthorityProposal", cancel_instruction(&authority)),
    ]
}

fn window_id_argument(instruction: &Instruction) -> u64 {
    match ClarionInstruction::unpack(&instruction.data).unwrap() {
        ClarionInstruction::Commit { window_id, .. }
        | ClarionInstruction::Reveal { window_id, .. } => window_id,
        other => panic!("{other:?} has no window_id"),
    }
}

fn resolve_pda(program: &Value, default: &Value, instruction: &Instruction) -> Pubkey {
    let pda = named(program, "pdas", default["pda"]["name"].as_str().unwrap());
    let seed_values = default["seeds"].as_array().cloned().unwrap_or_default();
    let seeds: Vec<Vec<u8>> = entries(pda, "seeds")
        .iter()
        .map(|seed| match seed["kind"].as_str().unwrap() {
            "constantPdaSeedNode" => {
                assert_eq!(seed["type"]["encoding"], "utf8");
                seed["value"]["string"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            }
            "variablePdaSeedNode" => {
                assert_eq!(seed["name"], "windowId");
                assert_eq!(seed["type"]["format"], "u64");
                assert_eq!(seed["type"]["endian"], "le");
                let value = seed_values
                    .iter()
                    .find(|value| value["name"] == seed["name"])
                    .unwrap();
                assert_eq!(value["value"]["kind"], "argumentValueNode");
                assert_eq!(value["value"]["name"], "windowId");
                window_id_argument(instruction).to_le_bytes().to_vec()
            }
            kind => panic!("unexpected seed {kind}"),
        })
        .collect();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    Pubkey::find_program_address(&seeds, &PROGRAM_ID).0
}

fn resolve_default(
    program: &Value,
    default: &Value,
    instruction: &Instruction,
) -> Pubkey {
    match default["kind"].as_str().unwrap() {
        "publicKeyValueNode" => default["publicKey"].as_str().unwrap().parse().unwrap(),
        "pdaValueNode" => resolve_pda(program, default, instruction),
        kind => panic!("unexpected default {kind}"),
    }
}

#[test]
fn committed_idls_match_the_source() {
    for program_id in DEVNET_INSTANCES {
        assert!(
            render(crate_dir(), program_id).unwrap() == committed_idl(program_id),
            "{} is stale; regenerate with `cargo run --example idl --features idl`",
            idl_path(crate_dir(), program_id).display()
        );
    }
}

#[test]
fn instance_idls_differ_only_in_the_program_address() {
    let without_address = |program_id: &str| {
        let mut root = root(program_id);
        root["program"]["publicKey"] = Value::Null;
        root
    };
    let idl_files = fs::read_dir(crate_dir().join("idl")).unwrap().count();

    assert_eq!(idl_files, DEVNET_INSTANCES.len());
    for program_id in DEVNET_INSTANCES {
        let program = &root(program_id)["program"];

        assert!(program_id.parse::<Pubkey>().is_ok());
        assert_eq!(program["publicKey"], program_id);
        assert_eq!(program["name"], "clarion");
        assert_eq!(program["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            without_address(program_id),
            without_address(DEVNET_INSTANCES[0])
        );
    }
}

#[test]
fn init_leaves_the_program_account_to_the_caller() {
    let program = program();
    let init = named(&program, "instructions", "init");

    assert!(
        named(init, "accounts", "program")
            .get("defaultValue")
            .is_none()
    );
}

#[test]
fn instructions_follow_the_borsh_tags_and_argument_sizes() {
    let program = program();
    let instructions = entries(&program, "instructions");

    assert_eq!(
        names(instructions),
        [
            "init",
            "commit",
            "reveal",
            "proposeAuthority",
            "acceptAuthority",
            "cancelAuthorityProposal"
        ]
    );
    for (node, sample) in instructions.iter().zip(sample_instructions()) {
        let bytes = borsh::to_vec(&sample).unwrap();
        let arguments = entries(node, "arguments");

        assert_eq!(discriminator(node, arguments), u64::from(bytes[0]));
        assert_eq!(arguments[0]["defaultValueStrategy"], "omitted");
        let argument_size: usize = arguments[1..]
            .iter()
            .map(|argument| type_size(&program, &argument["type"]))
            .sum();
        assert_eq!(argument_size, bytes.len() - 1, "{}", node["name"]);
    }
    assert_eq!(
        instructions
            .iter()
            .map(|node| names(&entries(node, "arguments")[1..]))
            .collect::<Vec<_>>(),
        [
            vec!["genesisSlot", "windowLen", "minRevealLagSlots"],
            vec!["windowId", "slotStart", "slotEnd", "merkleRoot"],
            vec!["windowId", "aggregates"],
            vec!["newAuthority"],
            vec![],
            vec![],
        ]
    );
}

#[test]
fn instruction_accounts_follow_the_processor_order_and_flags() {
    let program = program();
    let account_names: Vec<Vec<&str>> = entries(&program, "instructions")
        .iter()
        .map(|node| names(entries(node, "accounts")))
        .collect();

    assert_eq!(
        account_names,
        [
            vec!["authority", "state", "systemProgram", "program"],
            vec![
                "authority",
                "state",
                "window",
                "systemProgram",
                "previousWindow"
            ],
            vec!["authority", "state", "window"],
            vec!["authority", "state", "authorityProposal", "systemProgram"],
            vec!["newAuthority", "state", "authorityProposal"],
            vec!["authority", "state", "authorityProposal"],
        ]
    );
    for (name, instruction) in built_instructions() {
        let node = named(&program, "instructions", name);
        let declared = entries(node, "accounts");
        let passed: Vec<&Value> = declared
            .iter()
            .filter(|account| {
                account["isOptional"] != true || window_id_argument(&instruction) > 0
            })
            .collect();

        assert_eq!(passed.len(), instruction.accounts.len(), "{name}");
        for (account, meta) in passed.into_iter().zip(&instruction.accounts) {
            assert_eq!(account["isSigner"], meta.is_signer, "{name}");
            assert_eq!(account["isWritable"], meta.is_writable, "{name}");
            if !account["defaultValue"].is_null() {
                assert_eq!(
                    resolve_default(&program, &account["defaultValue"], &instruction),
                    meta.pubkey,
                    "{name} {}",
                    account["name"]
                );
            }
        }
    }
    let commit = named(&program, "instructions", "commit");
    assert_eq!(commit["optionalAccountStrategy"], "omitted");
    assert_eq!(
        named(commit, "accounts", "previousWindow")["isOptional"],
        true
    );
    let init = named(&program, "instructions", "init");
    assert_eq!(
        named(init, "accounts", "systemProgram")["defaultValue"]["publicKey"],
        system_program::ID.to_string()
    );
}

#[test]
fn accounts_carry_the_discriminators_and_sizes() {
    let program = program();
    let accounts = entries(&program, "accounts");

    assert_eq!(names(accounts), ["state", "window", "authorityProposal"]);
    for (node, (discriminator_value, size, pda)) in accounts.iter().zip([
        (STATE_DISCRIMINATOR, STATE_SIZE, "state"),
        (WINDOW_DISCRIMINATOR, WINDOW_SIZE, "window"),
        (PROPOSAL_DISCRIMINATOR, PROPOSAL_SIZE, "authorityProposal"),
    ]) {
        let fields = entries(&node["data"], "fields");

        assert_eq!(discriminator(node, fields), u64::from(discriminator_value));
        assert_eq!(type_size(&program, &node["data"]), size);
        assert_eq!(node["pda"]["name"], pda);
    }
    assert_eq!(
        [
            STATE_DISCRIMINATOR,
            WINDOW_DISCRIMINATOR,
            PROPOSAL_DISCRIMINATOR
        ],
        [1, 2, 3]
    );
    assert_eq!([STATE_SIZE, WINDOW_SIZE, PROPOSAL_SIZE], [66, 254, 34]);
}

#[test]
fn pdas_derive_the_program_addresses() {
    let program = program();
    let pdas = entries(&program, "pdas");

    assert_eq!(names(pdas), ["state", "window", "authorityProposal"]);
    let constant_seeds: Vec<&str> = pdas
        .iter()
        .map(|pda| {
            entries(pda, "seeds")[0]["value"]["string"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(
        constant_seeds
            .iter()
            .map(|seed| seed.as_bytes())
            .collect::<Vec<_>>(),
        [STATE_SEED, WINDOW_SEED, PROPOSAL_SEED]
    );
    assert_eq!(entries(&pdas[0], "seeds").len(), 1);
    assert_eq!(entries(&pdas[1], "seeds").len(), 2);
    assert_eq!(entries(&pdas[2], "seeds").len(), 1);

    let authority = Pubkey::new_from_array([1; 32]);
    let reveal = reveal_instruction(&authority, WINDOW_ID, aggregates());
    let reveal_node = named(&program, "instructions", "reveal");
    let propose = propose_instruction(&authority, &authority);
    let propose_node = named(&program, "instructions", "proposeAuthority");
    let resolve = |node: &Value, account: &str, instruction: &Instruction| {
        resolve_default(
            &program,
            &named(node, "accounts", account)["defaultValue"],
            instruction,
        )
    };
    assert_eq!(
        resolve(reveal_node, "state", &reveal),
        state_address(&PROGRAM_ID).0
    );
    assert_eq!(
        resolve(reveal_node, "window", &reveal),
        window_address(&PROGRAM_ID, WINDOW_ID).0
    );
    assert_eq!(
        resolve(propose_node, "authorityProposal", &propose),
        proposal_address(&PROGRAM_ID).0
    );
}

#[test]
fn errors_match_codes_names_and_messages() {
    let program = program();
    let errors = entries(&program, "errors");
    let expected: Vec<ClarionError> = (0..=u32::from(u16::MAX))
        .filter_map(ClarionError::from_code)
        .collect();

    assert_eq!(errors.len(), expected.len());
    assert_eq!(errors.len(), 38);
    for (node, error) in errors.iter().zip(expected) {
        assert_eq!(node["kind"], "errorNode");
        assert_eq!(node["code"], error.code());
        assert_eq!(node["name"], camel_case(&format!("{error:?}")));
        assert_eq!(node["message"], error.to_string());
    }
    assert!(errors.iter().all(|node| node["code"] != 122));
}

#[test]
fn events_follow_the_borsh_tags_and_sizes() {
    let program = program();
    let events = entries(&program, "events");

    assert_eq!(
        names(events),
        [
            "initialized",
            "committed",
            "revealed",
            "authorityProposed",
            "authorityAccepted",
            "authorityProposalCancelled"
        ]
    );
    for (node, sample) in events.iter().zip(sample_events()) {
        let bytes = borsh::to_vec(&sample).unwrap();
        let fields = entries(&node["data"], "fields");

        assert_eq!(discriminator(node, fields), u64::from(bytes[0]));
        assert_eq!(type_size(&program, &node["data"]), bytes.len());
    }
}

#[test]
fn defined_types_match_the_arm_layout() {
    let program = program();
    let arm = named(&program, "definedTypes", "arm");
    let arm_aggregates = named(&program, "definedTypes", "armAggregates");

    assert_eq!(
        names(entries(&arm["type"], "variants")),
        Arm::ALL
            .map(|arm| camel_case(&format!("{arm:?}")))
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(type_size(&program, &arm["type"]), 1);
    assert_eq!(
        type_size(&program, &arm_aggregates["type"]),
        ARM_AGGREGATES_SIZE
    );
    let window = named(&program, "accounts", "window");
    let reveal = named(&program, "instructions", "reveal");
    let revealed = named(&program, "events", "revealed");
    for aggregates_type in [
        &named(&window["data"], "fields", "aggregates")["type"],
        &named(reveal, "arguments", "aggregates")["type"],
        &named(&revealed["data"], "fields", "aggregates")["type"],
    ] {
        assert_eq!(aggregates_type["count"]["value"], ARM_COUNT);
        assert_eq!(aggregates_type["item"]["name"], "armAggregates");
    }
}
