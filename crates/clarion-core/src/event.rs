use base64::{Engine, prelude::BASE64_STANDARD};
use borsh::BorshDeserialize;
use clarion::ClarionEvent;
use solana_program::pubkey::Pubkey;
use solana_transaction::TransactionError;
use thiserror::Error;

const DATA_PREFIX: &str = "Program data: ";
const TRUNCATED: &str = "Log truncated";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EventError {
    #[error(
        "transaction failed ({0}), so it wrote no state and its events did not happen"
    )]
    TransactionFailed(TransactionError),
    #[error("log line {line} opens a frame at depth {declared}, expected {expected}")]
    FrameDepth {
        line: usize,
        declared: usize,
        expected: usize,
    },
    #[error("log line {line} closes {program}, which is not the executing program")]
    FrameMismatch { line: usize, program: String },
    #[error("log line {line} reports a failed program frame")]
    FrameFailed { line: usize },
    #[error("log line {line} carries program data outside any program frame")]
    DataOutsideFrame { line: usize },
    #[error("log line {line} carries {fields} data fields, expected 1")]
    FieldCount { line: usize, fields: usize },
    #[error("log line {line} carries data that is not base64")]
    Base64 { line: usize },
    #[error("log line {line} does not decode as a clarion event")]
    Undecodable { line: usize },
    #[error("log messages are truncated, so events may be missing")]
    Truncated,
    #[error("log messages end with {open} program frames still open")]
    Unclosed { open: usize },
}

enum Frame<'a> {
    Invoke { program: &'a str, depth: usize },
    Success { program: &'a str },
    Failed,
}

fn frame(line: &str) -> Option<Frame<'_>> {
    let (program, rest) = line.strip_prefix("Program ")?.split_once(' ')?;
    program.parse::<Pubkey>().ok()?;
    if let Some(depth) = rest
        .strip_prefix("invoke [")
        .and_then(|depth| depth.strip_suffix(']'))
    {
        return depth
            .parse()
            .ok()
            .map(|depth| Frame::Invoke { program, depth });
    }
    if rest == "success" {
        return Some(Frame::Success { program });
    }
    rest.starts_with("failed").then_some(Frame::Failed)
}

pub fn decode_events(
    program_id: &Pubkey,
    status: &Result<(), TransactionError>,
    logs: &[String],
) -> Result<Vec<ClarionEvent>, EventError> {
    if let Err(error) = status {
        return Err(EventError::TransactionFailed(error.clone()));
    }
    let program_id = program_id.to_string();
    let mut frames: Vec<&str> = Vec::new();
    let mut events = Vec::new();
    for (line, message) in logs.iter().enumerate() {
        if message.starts_with(TRUNCATED) {
            return Err(EventError::Truncated);
        }
        if let Some(payload) = message.strip_prefix(DATA_PREFIX) {
            let executing = frames.last().ok_or(EventError::DataOutsideFrame { line })?;
            if *executing == program_id {
                events.push(decode_payload(line, payload)?);
            }
            continue;
        }
        match frame(message) {
            Some(Frame::Invoke { program, depth }) => {
                let expected = frames.len() + 1;
                if depth != expected {
                    return Err(EventError::FrameDepth {
                        line,
                        declared: depth,
                        expected,
                    });
                }
                frames.push(program);
            }
            Some(Frame::Success { program }) => {
                if frames.pop() != Some(program) {
                    return Err(EventError::FrameMismatch {
                        line,
                        program: program.to_string(),
                    });
                }
            }
            Some(Frame::Failed) => return Err(EventError::FrameFailed { line }),
            None => {}
        }
    }
    if frames.is_empty() {
        Ok(events)
    } else {
        Err(EventError::Unclosed { open: frames.len() })
    }
}

fn decode_payload(line: usize, payload: &str) -> Result<ClarionEvent, EventError> {
    let fields: Vec<&str> = payload.split(' ').collect();
    let [field] = fields.as_slice() else {
        return Err(EventError::FieldCount {
            line,
            fields: fields.len(),
        });
    };
    let bytes = BASE64_STANDARD
        .decode(field)
        .map_err(|_| EventError::Base64 { line })?;
    ClarionEvent::try_from_slice(&bytes).map_err(|_| EventError::Undecodable { line })
}

#[cfg(test)]
mod tests {
    use clarion::{Arm, ArmAggregates};
    use solana_transaction::InstructionError;

    use super::*;

    const CLARION: Pubkey = Pubkey::new_from_array([42; 32]);
    const ROUTER: Pubkey = Pubkey::new_from_array([43; 32]);
    const SYSTEM: &str = "11111111111111111111111111111111";
    const LANDED: Result<(), TransactionError> = Ok(());

    fn proposed() -> ClarionEvent {
        ClarionEvent::AuthorityProposed {
            authority: Pubkey::new_from_array([1; 32]),
            proposed_authority: Pubkey::new_from_array([2; 32]),
        }
    }

    fn revealed() -> ClarionEvent {
        ClarionEvent::Revealed {
            window_id: 4,
            reveal_slot: 1_911,
            aggregates: Arm::ALL.map(ArmAggregates::empty),
        }
    }

    fn data_line(event: &ClarionEvent) -> String {
        format!(
            "{DATA_PREFIX}{}",
            BASE64_STANDARD.encode(borsh::to_vec(event).unwrap())
        )
    }

    fn invoke(program: &str, depth: usize) -> String {
        format!("Program {program} invoke [{depth}]")
    }

    fn success(program: &str) -> String {
        format!("Program {program} success")
    }

    fn consumed(program: &str) -> String {
        format!("Program {program} consumed 6763 of 200000 compute units")
    }

    fn clarion_frame(event: &ClarionEvent) -> Vec<String> {
        let clarion = CLARION.to_string();
        vec![
            invoke(&clarion, 1),
            invoke(SYSTEM, 2),
            success(SYSTEM),
            data_line(event),
            consumed(&clarion),
            success(&clarion),
        ]
    }

    #[test]
    fn decodes_the_event_of_a_top_level_clarion_frame() {
        assert_eq!(
            decode_events(&CLARION, &LANDED, &clarion_frame(&proposed())),
            Ok(vec![proposed()])
        );
    }

    #[test]
    fn decodes_events_of_successive_frames_in_order() {
        let logs = [clarion_frame(&revealed()), clarion_frame(&proposed())].concat();

        assert_eq!(
            decode_events(&CLARION, &LANDED, &logs),
            Ok(vec![revealed(), proposed()])
        );
    }

    #[test]
    fn ignores_data_of_a_program_that_clarion_invokes() {
        let clarion = CLARION.to_string();
        let router = ROUTER.to_string();
        let logs = vec![
            invoke(&clarion, 1),
            invoke(&router, 2),
            data_line(&revealed()),
            success(&router),
            data_line(&proposed()),
            success(&clarion),
        ];

        assert_eq!(
            decode_events(&CLARION, &LANDED, &logs),
            Ok(vec![proposed()])
        );
    }

    #[test]
    fn attributes_data_to_clarion_when_another_program_invokes_it() {
        let clarion = CLARION.to_string();
        let router = ROUTER.to_string();
        let logs = vec![
            invoke(&router, 1),
            data_line(&revealed()),
            "Program log: Program data: AA==".to_string(),
            invoke(&clarion, 2),
            invoke(SYSTEM, 3),
            success(SYSTEM),
            data_line(&proposed()),
            consumed(&clarion),
            success(&clarion),
            format!("{DATA_PREFIX}not clarion data"),
            consumed(&router),
            success(&router),
        ];

        assert_eq!(
            decode_events(&CLARION, &LANDED, &logs),
            Ok(vec![proposed()])
        );
    }

    #[test]
    fn yields_nothing_for_logs_without_clarion_frames() {
        let router = ROUTER.to_string();
        let logs = vec![invoke(&router, 1), data_line(&proposed()), success(&router)];

        assert_eq!(decode_events(&CLARION, &LANDED, &logs), Ok(vec![]));
        assert_eq!(decode_events(&CLARION, &LANDED, &[]), Ok(vec![]));
    }

    #[test]
    fn rejects_logs_of_a_failed_transaction() {
        let clarion = CLARION.to_string();
        let mut logs = clarion_frame(&proposed());
        logs.extend([
            invoke(&clarion, 1),
            consumed(&clarion),
            format!("Program {clarion} failed: custom program error: 0x7b"),
        ]);

        assert_eq!(
            decode_events(&CLARION, &LANDED, &logs),
            Err(EventError::FrameFailed { line: 8 })
        );
    }

    #[test]
    fn rejects_truncated_logs() {
        let mut logs = clarion_frame(&proposed());
        logs.push("Log truncated".to_string());

        assert_eq!(
            decode_events(&CLARION, &LANDED, &logs),
            Err(EventError::Truncated)
        );
    }

    #[test]
    fn rejects_unbalanced_frames() {
        let clarion = CLARION.to_string();
        let router = ROUTER.to_string();

        assert_eq!(
            decode_events(&CLARION, &LANDED, &[invoke(&clarion, 2)]),
            Err(EventError::FrameDepth {
                line: 0,
                declared: 2,
                expected: 1
            })
        );
        assert_eq!(
            decode_events(&CLARION, &LANDED, &[invoke(&clarion, 1), success(&router)]),
            Err(EventError::FrameMismatch {
                line: 1,
                program: router.clone()
            })
        );
        assert_eq!(
            decode_events(&CLARION, &LANDED, &[success(&clarion)]),
            Err(EventError::FrameMismatch {
                line: 0,
                program: clarion.clone()
            })
        );
        assert_eq!(
            decode_events(&CLARION, &LANDED, &[invoke(&clarion, 1)]),
            Err(EventError::Unclosed { open: 1 })
        );
        assert_eq!(
            decode_events(&CLARION, &LANDED, &[data_line(&proposed())]),
            Err(EventError::DataOutsideFrame { line: 0 })
        );
    }

    #[test]
    fn rejects_malformed_clarion_data() {
        let clarion = CLARION.to_string();
        let frame_with =
            |data: String| vec![invoke(&clarion, 1), data, success(&clarion)];
        let encoded = BASE64_STANDARD.encode(borsh::to_vec(&proposed()).unwrap());
        let mut unknown_tag = borsh::to_vec(&proposed()).unwrap();
        unknown_tag[0] = 6;
        let mut trailing = borsh::to_vec(&proposed()).unwrap();
        trailing.push(0);

        assert_eq!(
            decode_events(
                &CLARION,
                &LANDED,
                &frame_with(format!("{DATA_PREFIX}{encoded} {encoded}"))
            ),
            Err(EventError::FieldCount { line: 1, fields: 2 })
        );
        assert_eq!(
            decode_events(&CLARION, &LANDED, &frame_with(format!("{DATA_PREFIX}*"))),
            Err(EventError::Base64 { line: 1 })
        );
        for bytes in [unknown_tag, trailing, Vec::new()] {
            assert_eq!(
                decode_events(
                    &CLARION,
                    &LANDED,
                    &frame_with(format!(
                        "{DATA_PREFIX}{}",
                        BASE64_STANDARD.encode(bytes)
                    ))
                ),
                Err(EventError::Undecodable { line: 1 })
            );
        }
    }

    #[test]
    fn rejects_a_failed_transaction_whose_frames_all_succeeded() {
        let clarion = CLARION.to_string();
        let logs = vec![
            invoke(&clarion, 1),
            data_line(&proposed()),
            consumed(&clarion),
            success(&clarion),
            invoke(SYSTEM, 1),
            success(SYSTEM),
        ];
        let rent = TransactionError::InsufficientFundsForRent { account_index: 2 };

        assert_eq!(
            decode_events(&CLARION, &Err(rent.clone()), &logs),
            Err(EventError::TransactionFailed(rent))
        );
    }

    #[test]
    fn rejects_a_transaction_failed_by_a_silent_precompile() {
        let precompile =
            TransactionError::InstructionError(1, InstructionError::Custom(2));

        assert_eq!(
            decode_events(
                &CLARION,
                &Err(precompile.clone()),
                &clarion_frame(&proposed())
            ),
            Err(EventError::TransactionFailed(precompile))
        );
    }

    #[test]
    fn reports_the_transaction_status_before_reading_any_log() {
        let failed = TransactionError::AccountInUse;

        assert_eq!(
            decode_events(
                &CLARION,
                &Err(failed.clone()),
                &["Log truncated".to_string()]
            ),
            Err(EventError::TransactionFailed(failed))
        );
    }
}
