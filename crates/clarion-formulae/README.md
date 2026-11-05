# clarion-formulae

Formulae documents, attestation records and offline verification for the clarion program.

Build:

```sh
cargo build --release -p clarion-formulae
```

Formulae id:

```sh
target/release/clarion-formulae id formulae/trace-completed.v1.json
```

Record from the artifacts and the declared inputs:

```sh
target/release/clarion-formulae build --formulae formulae/trace-completed.v1.json --artifacts <dir> --input system.source_commit=<sha> --input results.subject_id=replay --out <dir>/attestation.json
```

Bind after `clarion-admin reveal`, with the `show` output saved to a file:

```sh
target/release/clarion-formulae bind --attestation <dir>/attestation.json --window <show.json> --cluster devnet --program-id <id> --commit-signature <sig> --reveal-signature <sig>
```

Verify; layers 1 and 2 always, layer 3 with `--artifacts`:

```sh
target/release/clarion-formulae verify --attestation <dir>/attestation.json --window <show.json> --artifacts <dir>
```

| Path | Content |
|---|---|
| `formulae/<name>.json` | immutable document; id = sha256 of its canonical JSON |
| `formulae/ids.txt` | golden `name id` lines, checked by the tests |
| `attestations/<cluster>/<program id>/<window id>/` | record beside its artifacts |

Exit codes: 0 success, 1 failure, 2 usage.
