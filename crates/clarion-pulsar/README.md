# clarion-pulsar

Solana TPU QUIC handshake latency sampler.

Build:

```sh
cargo build --release -p clarion-pulsar
```

Devnet run:

```sh
target/release/clarion-pulsar --vantage fra-1
```

Mainnet-beta run; a keyed provider URL goes in `rpc_url` of the config file, never on the command line:

```sh
target/release/clarion-pulsar --config clarion-pulsar-mainnet-beta.toml
```

Config file; `vantage` is required, every other value is its default:

```toml
rpc_url = "https://api.devnet.solana.com"
vantage = "fra-1"
rounds = 3
round_spacing_ms = 30000
concurrency = 64
connect_timeout_ms = 2000
max_starts_per_second = 100
output_dir = "datasets/clarion-pulsar"
```

| Output file | Content |
|---|---|
| `clarion-pulsar-<ts>.csv` | one handshake row per identity and sweep |
| `clarion-pulsar-validators-<ts>.csv` | one row per identity and vote account |
| `clarion-pulsar-leader-slots-<cluster>-<epoch>.csv` | leader slot counts; while present and readable, getLeaderSchedule is skipped |

`target/release/clarion-pulsar --help` lists every key, column, pacing rule and exit code.
