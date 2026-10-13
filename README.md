# aetelier-clarion

Solana commit-reveal program for per-window transaction landing aggregates, its admin CLI, and a TPU QUIC handshake latency sampler.

## Devnet

| Item | Value |
|---|---|
| Program id | `6YX9anqQRmUywdM4R8x2oqmZo2W9WWyHKaWxjhpJtKtp` |
| Upgrade authority | `E9RmLD7Rv5CFbv8qjT3MVN8PMpcVnoFukNDvoZoJgEey` |
| State account | `9PpHf7k2Zcy78LU9AWDGUVqE4DKfhc5cBh7KAx7vVe8g` |
| Genesis slot | 510501377 |
| Window length | 9,000 slots |
| Minimum reveal lag | 150 slots |

## Verifiable build

`programs/clarion` builds in the digest-pinned `solanafoundation/solana-verifiable-build:3.1.13` image, selected by `[workspace.metadata.cli]` in its `Cargo.toml`:

```sh
solana-verify build "$PWD/programs/clarion"
solana-verify get-executable-hash programs/clarion/target/deploy/clarion.so
solana-verify --url https://api.devnet.solana.com get-program-hash <program id>
```

Equal hashes mean the deployed program was built from the checked-out commit.
