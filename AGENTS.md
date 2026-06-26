# atlas-faucet — agent notes

Proof-of-work native-token faucet for the Atlas chain. Rust + `axum`/`tokio`,
following the build/deploy conventions of `atlas-payload-provider`
(env-only config, minimal deps, multi-stage Docker, `scripts/package.sh`, the
`Package` workflow).

## Layout

| File | Role |
| --- | --- |
| `src/config.rs` | `envy` env config + defaults. |
| `src/pow.rs` | HMAC-signed challenges, multi-puzzle sha256 PoW, replay/cooldown state. |
| `src/eth.rs` | Hand-rolled RLP + legacy EIP-155 signing + sender recovery. |
| `src/rpc.rs` | Minimal async JSON-RPC client (`reqwest`). |
| `src/faucet.rs` | Dispenser: admission semaphore + serialize mutex, sign/submit. |
| `src/server.rs` | axum router + handlers (`/`, `/healthz`, `/status`, `/api/challenge`, `/api/claim`). |
| `src/index.html` | Single-file UI; inline Web Worker pool solves the PoW with a progress bar. |
| `tests/e2e_claim.rs` | Full HTTP claim flow against a mock JSON-RPC node. |

## Invariants (do not break)

* **PoW parity.** The browser solver in `index.html` and `pow::puzzle_hash`
  must compute the identical preimage and hash:
  `sha256(salt[16] || address[20] || index_u32_le || nonce_u32_le)` with the
  leading-zero-bit predicate. `scripts/pow-parity.mjs` checks this; the Rust
  test `pow::tests::browser_parity_vector` locks a known vector.
* **One drip at a time.** `Faucet` serializes dispensing so the faucet account
  nonce stays sequential; admission caps in-flight claims at
  `FAUCET_QUEUE_CAPACITY`.
* **Challenge integrity.** Challenges are HMAC-signed and address-bound; the
  server is stateless about *issued* challenges and only records *consumed*
  salts to stop replay.
* **Signing is verified.** `eth::tests::signs_eip155_official_vector` pins the
  signing path to the canonical EIP-155 test vector. Keep it green.

## Test / run

```bash
cargo test
cargo run                 # needs an Atlas node on RPC_URL (default :8545)
node scripts/pow-parity.mjs   # JS↔Rust proof-of-work parity check
docker compose up --build
```
