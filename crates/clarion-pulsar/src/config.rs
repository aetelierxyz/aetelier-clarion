use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
pub const DEFAULT_ROUNDS: u8 = 3;
pub const DEFAULT_ROUND_SPACING_MS: u64 = 30_000;
pub const MIN_ROUND_SPACING_MS: u64 = 30_000;
pub const DEFAULT_CONCURRENCY: u16 = 64;
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 2_000;
pub const DEFAULT_MAX_STARTS_PER_SECOND: u16 = 100;
pub const MAX_STARTS_PER_SECOND_LIMIT: u16 = 100;
pub const DEFAULT_OUTPUT_DIR: &str = "datasets/clarion-pulsar";
pub const FILE_KEYS: [&str; 8] = [
    "rpc_url",
    "vantage",
    "rounds",
    "round_spacing_ms",
    "concurrency",
    "connect_timeout_ms",
    "max_starts_per_second",
    "output_dir",
];

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse {path} at line {line}, column {column}: {message}")]
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    #[error("vantage is required: set it in the config file or pass --vantage")]
    MissingVantage,
    #[error("{field} must be at least 1")]
    Zero { field: &'static str },
    #[error("{field} must be at least {minimum}")]
    BelowMinimum { field: &'static str, minimum: u64 },
    #[error("{field} must be between {minimum} and {maximum}")]
    OutOfRange {
        field: &'static str,
        minimum: u64,
        maximum: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub rpc_url: String,
    pub vantage: Option<String>,
    pub rounds: u8,
    pub round_spacing_ms: u64,
    pub concurrency: u16,
    pub connect_timeout_ms: u64,
    pub max_starts_per_second: u16,
    pub output_dir: PathBuf,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            rpc_url: DEFAULT_RPC_URL.to_owned(),
            vantage: None,
            rounds: DEFAULT_ROUNDS,
            round_spacing_ms: DEFAULT_ROUND_SPACING_MS,
            concurrency: DEFAULT_CONCURRENCY,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            max_starts_per_second: DEFAULT_MAX_STARTS_PER_SECOND,
            output_dir: PathBuf::from(DEFAULT_OUTPUT_DIR),
        }
    }
}

impl FileConfig {
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| {
            let offset = source.span().map_or(0, |span| span.start);
            let before = text.get(..offset).unwrap_or(&text);
            let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
            ConfigError::Parse {
                path: path.to_path_buf(),
                line: before.matches('\n').count() + 1,
                column: before[line_start..].chars().count() + 1,
                message: source.message().to_owned(),
            }
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub rpc_url: Option<String>,
    pub vantage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulsarConfig {
    pub rpc_url: String,
    pub vantage: String,
    pub rounds: u8,
    pub round_spacing: Duration,
    pub concurrency: u16,
    pub connect_timeout: Duration,
    pub max_starts_per_second: u16,
    pub output_dir: PathBuf,
}

impl PulsarConfig {
    pub fn resolve(
        mut file: FileConfig,
        overrides: Overrides,
    ) -> Result<Self, ConfigError> {
        if let Some(rpc_url) = overrides.rpc_url {
            file.rpc_url = rpc_url;
        }
        let vantage = overrides
            .vantage
            .or(file.vantage)
            .filter(|label| !label.trim().is_empty())
            .ok_or(ConfigError::MissingVantage)?;
        if file.rounds == 0 {
            return Err(ConfigError::Zero { field: "rounds" });
        }
        if file.concurrency == 0 {
            return Err(ConfigError::Zero {
                field: "concurrency",
            });
        }
        if file.connect_timeout_ms == 0 {
            return Err(ConfigError::Zero {
                field: "connect_timeout_ms",
            });
        }
        if file.round_spacing_ms < MIN_ROUND_SPACING_MS {
            return Err(ConfigError::BelowMinimum {
                field: "round_spacing_ms",
                minimum: MIN_ROUND_SPACING_MS,
            });
        }
        if !(1..=MAX_STARTS_PER_SECOND_LIMIT).contains(&file.max_starts_per_second) {
            return Err(ConfigError::OutOfRange {
                field: "max_starts_per_second",
                minimum: 1,
                maximum: u64::from(MAX_STARTS_PER_SECOND_LIMIT),
            });
        }
        Ok(Self {
            rpc_url: file.rpc_url,
            vantage,
            rounds: file.rounds,
            round_spacing: Duration::from_millis(file.round_spacing_ms),
            concurrency: file.concurrency,
            connect_timeout: Duration::from_millis(file.connect_timeout_ms),
            max_starts_per_second: file.max_starts_per_second,
            output_dir: file.output_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vantage(label: &str) -> Overrides {
        Overrides {
            rpc_url: None,
            vantage: Some(label.to_owned()),
        }
    }

    #[test]
    fn absent_file_yields_defaults() {
        let file = FileConfig::load(None).unwrap();

        assert_eq!(file.rpc_url, "https://api.devnet.solana.com");
        assert_eq!(file.vantage, None);
        assert_eq!(file.rounds, 3);
        assert_eq!(file.round_spacing_ms, 30_000);
        assert_eq!(file.concurrency, 64);
        assert_eq!(file.connect_timeout_ms, 2_000);
        assert_eq!(file.max_starts_per_second, 100);
        assert_eq!(file.output_dir, PathBuf::from("datasets/clarion-pulsar"));
    }

    #[test]
    fn empty_document_equals_defaults() {
        assert_eq!(
            toml::from_str::<FileConfig>("").unwrap(),
            FileConfig::default()
        );
    }

    #[test]
    fn file_fields_replace_defaults_one_by_one() {
        let file: FileConfig = toml::from_str(
            r#"
            vantage = "fra-1"
            rounds = 5
            connect_timeout_ms = 750
            "#,
        )
        .unwrap();

        assert_eq!(
            file,
            FileConfig {
                vantage: Some("fra-1".to_owned()),
                rounds: 5,
                connect_timeout_ms: 750,
                ..FileConfig::default()
            }
        );
    }

    #[test]
    fn unknown_file_field_is_rejected() {
        assert!(toml::from_str::<FileConfig>("cluster = \"devnet\"").is_err());
    }

    #[test]
    fn unreadable_file_reports_its_path() {
        let path = Path::new("/nonexistent/clarion-pulsar.toml");

        let error = FileConfig::load(Some(path)).unwrap_err();

        assert!(
            matches!(error, ConfigError::Read { path: reported, .. } if reported == path)
        );
    }

    #[test]
    fn resolved_defaults_carry_durations() {
        let config =
            PulsarConfig::resolve(FileConfig::default(), vantage("fra-1")).unwrap();

        assert_eq!(
            config,
            PulsarConfig {
                rpc_url: "https://api.devnet.solana.com".to_owned(),
                vantage: "fra-1".to_owned(),
                rounds: 3,
                round_spacing: Duration::from_secs(30),
                concurrency: 64,
                connect_timeout: Duration::from_secs(2),
                max_starts_per_second: 100,
                output_dir: PathBuf::from("datasets/clarion-pulsar"),
            }
        );
    }

    #[test]
    fn flags_override_the_file() {
        let file = FileConfig {
            rpc_url: "http://file.example:8899".to_owned(),
            vantage: Some("from-file".to_owned()),
            ..FileConfig::default()
        };
        let overrides = Overrides {
            rpc_url: Some("http://flag.example:8899".to_owned()),
            vantage: Some("from-flag".to_owned()),
        };

        let config = PulsarConfig::resolve(file, overrides).unwrap();

        assert_eq!(config.rpc_url, "http://flag.example:8899");
        assert_eq!(config.vantage, "from-flag");
    }

    #[test]
    fn file_values_stand_without_flags() {
        let file = FileConfig {
            rpc_url: "http://file.example:8899".to_owned(),
            vantage: Some("from-file".to_owned()),
            ..FileConfig::default()
        };

        let config = PulsarConfig::resolve(file, Overrides::default()).unwrap();

        assert_eq!(config.rpc_url, "http://file.example:8899");
        assert_eq!(config.vantage, "from-file");
    }

    #[test]
    fn missing_vantage_is_an_error() {
        let error = PulsarConfig::resolve(FileConfig::default(), Overrides::default())
            .unwrap_err();

        assert!(matches!(error, ConfigError::MissingVantage));
    }

    #[test]
    fn blank_vantage_is_an_error() {
        let error =
            PulsarConfig::resolve(FileConfig::default(), vantage("  ")).unwrap_err();

        assert!(matches!(error, ConfigError::MissingVantage));
    }

    #[test]
    fn zero_rounds_is_an_error() {
        let file = FileConfig {
            rounds: 0,
            ..FileConfig::default()
        };

        let error = PulsarConfig::resolve(file, vantage("fra-1")).unwrap_err();

        assert!(matches!(error, ConfigError::Zero { field: "rounds" }));
    }

    #[test]
    fn zero_concurrency_is_an_error() {
        let file = FileConfig {
            concurrency: 0,
            ..FileConfig::default()
        };

        let error = PulsarConfig::resolve(file, vantage("fra-1")).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Zero {
                field: "concurrency"
            }
        ));
    }

    #[test]
    fn zero_connect_timeout_is_an_error() {
        let file = FileConfig {
            connect_timeout_ms: 0,
            ..FileConfig::default()
        };

        let error = PulsarConfig::resolve(file, vantage("fra-1")).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Zero {
                field: "connect_timeout_ms"
            }
        ));
    }

    #[test]
    fn round_spacing_of_thirty_seconds_is_accepted() {
        let file = FileConfig {
            round_spacing_ms: 30_000,
            ..FileConfig::default()
        };

        let config = PulsarConfig::resolve(file, vantage("fra-1")).unwrap();

        assert_eq!(config.round_spacing, Duration::from_secs(30));
    }

    #[test]
    fn round_spacing_below_thirty_seconds_is_rejected() {
        for round_spacing_ms in [0, 200, 29_999] {
            let file = FileConfig {
                round_spacing_ms,
                ..FileConfig::default()
            };

            let error = PulsarConfig::resolve(file, vantage("fra-1")).unwrap_err();

            assert!(matches!(
                error,
                ConfigError::BelowMinimum {
                    field: "round_spacing_ms",
                    minimum: 30_000
                }
            ));
        }
    }

    #[test]
    fn start_rate_between_one_and_one_hundred_is_accepted() {
        for max_starts_per_second in [1, 37, 100] {
            let file = FileConfig {
                max_starts_per_second,
                ..FileConfig::default()
            };

            let config = PulsarConfig::resolve(file, vantage("fra-1")).unwrap();

            assert_eq!(config.max_starts_per_second, max_starts_per_second);
        }
    }

    #[test]
    fn start_rate_outside_one_to_one_hundred_is_rejected() {
        for max_starts_per_second in [0, 101, u16::MAX] {
            let file = FileConfig {
                max_starts_per_second,
                ..FileConfig::default()
            };

            let error = PulsarConfig::resolve(file, vantage("fra-1")).unwrap_err();

            assert!(matches!(
                error,
                ConfigError::OutOfRange {
                    field: "max_starts_per_second",
                    minimum: 1,
                    maximum: 100
                }
            ));
        }
    }

    #[test]
    fn every_listed_key_is_a_file_field_and_every_field_is_listed() {
        let document = FILE_KEYS
            .iter()
            .map(|key| match *key {
                "rpc_url" | "vantage" | "output_dir" => format!("{key} = \"x\""),
                _ => format!("{key} = 1"),
            })
            .collect::<Vec<_>>()
            .join("\n");

        let file: FileConfig = toml::from_str(&document).unwrap();

        assert_eq!(
            file,
            FileConfig {
                rpc_url: "x".to_owned(),
                vantage: Some("x".to_owned()),
                rounds: 1,
                round_spacing_ms: 1,
                concurrency: 1,
                connect_timeout_ms: 1,
                max_starts_per_second: 1,
                output_dir: PathBuf::from("x"),
            }
        );
    }
}
