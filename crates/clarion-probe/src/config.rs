use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
pub const DEFAULT_ROUNDS: u8 = 3;
pub const DEFAULT_ROUND_SPACING_MS: u64 = 200;
pub const DEFAULT_CONCURRENCY: u16 = 64;
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 2_000;
pub const DEFAULT_OUTPUT_DIR: &str = "datasets/probe";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("vantage is required: set it in the config file or pass --vantage")]
    MissingVantage,
    #[error("{field} must be at least 1")]
    Zero { field: &'static str },
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
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub rpc_url: Option<String>,
    pub vantage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeConfig {
    pub rpc_url: String,
    pub vantage: String,
    pub rounds: u8,
    pub round_spacing: Duration,
    pub concurrency: u16,
    pub connect_timeout: Duration,
    pub output_dir: PathBuf,
}

impl ProbeConfig {
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
        Ok(Self {
            rpc_url: file.rpc_url,
            vantage,
            rounds: file.rounds,
            round_spacing: Duration::from_millis(file.round_spacing_ms),
            concurrency: file.concurrency,
            connect_timeout: Duration::from_millis(file.connect_timeout_ms),
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
        assert_eq!(file.round_spacing_ms, 200);
        assert_eq!(file.concurrency, 64);
        assert_eq!(file.connect_timeout_ms, 2_000);
        assert_eq!(file.output_dir, PathBuf::from("datasets/probe"));
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
        let path = Path::new("/nonexistent/clarion-probe.toml");

        let error = FileConfig::load(Some(path)).unwrap_err();

        assert!(
            matches!(error, ConfigError::Read { path: reported, .. } if reported == path)
        );
    }

    #[test]
    fn resolved_defaults_carry_durations() {
        let config =
            ProbeConfig::resolve(FileConfig::default(), vantage("fra-1")).unwrap();

        assert_eq!(
            config,
            ProbeConfig {
                rpc_url: "https://api.devnet.solana.com".to_owned(),
                vantage: "fra-1".to_owned(),
                rounds: 3,
                round_spacing: Duration::from_millis(200),
                concurrency: 64,
                connect_timeout: Duration::from_secs(2),
                output_dir: PathBuf::from("datasets/probe"),
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

        let config = ProbeConfig::resolve(file, overrides).unwrap();

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

        let config = ProbeConfig::resolve(file, Overrides::default()).unwrap();

        assert_eq!(config.rpc_url, "http://file.example:8899");
        assert_eq!(config.vantage, "from-file");
    }

    #[test]
    fn missing_vantage_is_an_error() {
        let error = ProbeConfig::resolve(FileConfig::default(), Overrides::default())
            .unwrap_err();

        assert!(matches!(error, ConfigError::MissingVantage));
    }

    #[test]
    fn blank_vantage_is_an_error() {
        let error =
            ProbeConfig::resolve(FileConfig::default(), vantage("  ")).unwrap_err();

        assert!(matches!(error, ConfigError::MissingVantage));
    }

    #[test]
    fn zero_rounds_is_an_error() {
        let file = FileConfig {
            rounds: 0,
            ..FileConfig::default()
        };

        let error = ProbeConfig::resolve(file, vantage("fra-1")).unwrap_err();

        assert!(matches!(error, ConfigError::Zero { field: "rounds" }));
    }

    #[test]
    fn zero_concurrency_is_an_error() {
        let file = FileConfig {
            concurrency: 0,
            ..FileConfig::default()
        };

        let error = ProbeConfig::resolve(file, vantage("fra-1")).unwrap_err();

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

        let error = ProbeConfig::resolve(file, vantage("fra-1")).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Zero {
                field: "connect_timeout_ms"
            }
        ));
    }
}
