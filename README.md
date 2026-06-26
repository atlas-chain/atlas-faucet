# Atlas Faucet

A small, self-contained proof-of-work faucet for the **Atlas** chain. A user
enters their address, the browser solves a short multicore proof-of-work
(~5 s), and the faucet sends them native test funds. The proof-of-work gates
abuse without captchas or accounts, and a single-flight queue keeps the funding
account's nonce sequential.

Written in Rust over `axum` + `tokio`, mirroring the build/deploy conventions of
[`atlas-payload-provider`](https://github.com/atlas-chain/atlas-payload-provider).

## Quick start

```bash
# 1. Run an Atlas dev node (chain id 1337, RPC on :8545). For example:
#    just node-dev      # from atlas-reth
# 2. Run the faucet:
cargo run
```

Open `http://127.0.0.1:28884/`, paste an address, and click **Request funds**.

By default the faucet signs with the well-known Atlas dev account
(`0xf39F…2266`, Anvil account #0), which is pre-funded in the dev genesis.

## How it works

```
browser                              faucet                      atlas node
   │  GET /api/challenge?address=0x…    │                            │
   │ ◀───────── signed challenge ───────┤  (HMAC, no server state)   │
   │                                    │                            │
   │  solve POW across all CPU cores    │                            │
   │  (progress bar = solved / puzzles) │                            │
   │                                    │                            │
   │  POST /api/claim {challenge,nonces}│                            │
   │ ──────────────────────────────────▶│ verify HMAC + POW          │
   │                                    │ admit (queue ≤ N) + dispense│
   │                                    │ ── eth_sendRawTransaction ─▶│
   │ ◀──────── { txHash, amount } ──────┤ ◀──── receipt ─────────────┤
```

### Proof-of-work

A challenge asks the client to solve `POW_PUZZLES` independent sub-puzzles, each
requiring `POW_BITS` leading zero bits of `sha256(salt || address || index ||
nonce)`. Many small puzzles (instead of one big target) buy three things:

* **Multicore by construction** — workers each take a slice of the puzzle
  indices, so the work scales across every core.
* **An honest progress bar** — progress is `solved / total`, deterministic, not
  a guess.
* **Cheap verification** — the server checks one hash per puzzle.

The challenge is bound to the recipient address (so a solution can't be reused
for someone else) and HMAC-signed (so the server stays stateless about issued
challenges — it only records *consumed* ones to stop replay).

### Single-flight queue

At most `FAUCET_QUEUE_CAPACITY` claims (default **2**) may be in the system at
once. Exactly **one** is dispensed at a time; the rest wait. A claim that
arrives when the queue is full gets `429 Too Many Requests` with a `Retry-After`
header. Serializing the dispense keeps the faucet account's nonce correct.

## Configuration

All configuration is via environment variables (no command-line flags).

| Variable | Default | Description |
| --- | --- | --- |
| `LISTEN_HOST` | `0.0.0.0` | HTTP bind host. |
| `LISTEN_PORT` | `28884` | HTTP bind port. |
| `WEB_WORKERS` | `4` | Tokio worker thread count. |
| `HTML_TITLE` | `Atlas Faucet` | Browser UI title. |
| `RPC_URL` | `http://127.0.0.1:8545` | Atlas execution node JSON-RPC. |
| `CHAIN_ID` | _auto_ | Chain id; discovered via `eth_chainId` when unset. |
| `FAUCET_PRIVATE_KEY` | _dev key_ | 0x secp256k1 key of the funded faucet account. **Override in production.** |
| `FAUCET_DRIP_WEI` | `1000000000000000000` | Amount sent per claim, in wei (1 ATL). |
| `POW_BITS` | `16` | Leading zero bits per sub-puzzle. |
| `POW_PUZZLES` | `480` | Number of sub-puzzles (controls total work / solve time). |
| `POW_TTL_SECS` | `180` | Challenge lifetime. |
| `POW_HMAC_SECRET` | _random_ | Fixed challenge-signing secret (challenges survive restarts when set). |
| `FAUCET_QUEUE_CAPACITY` | `2` | Max concurrent claims in the system (one dispensed at a time). |
| `FAUCET_COOLDOWN_SECS` | `60` | Per-address cooldown between claims (0 disables). |
| `FAUCET_GAS_LIMIT` | `21000` | Gas limit for the transfer. |
| `FAUCET_GAS_PRICE_WEI` | _auto_ | Fixed gas price; otherwise `eth_gasPrice`. |
| `FAUCET_RECEIPT_TIMEOUT_SECS` | `30` | How long to wait for the tx to be mined before returning a pending hash. |

### Tuning the proof-of-work

Total client work is roughly `POW_PUZZLES × 2^POW_BITS` sha256 hashes, spread
across the visitor's CPU cores. The defaults (`16` bits × `480` puzzles)
target about 5 seconds on a typical multicore laptop. Prefer raising
`POW_PUZZLES` for a longer/heavier challenge — it scales the work linearly and
keeps the progress bar smooth. `POW_BITS` scales work exponentially per puzzle
and is capped at **28** (the browser searches a 32-bit nonce space per puzzle);
the faucet refuses to start above that.

## API

### `GET /api/challenge?address=0x…`

Issues a signed challenge for the address. Returns `429` if the address is in
cooldown (so the client doesn't waste effort solving).

```json
{ "ok": true, "challenge": { "version": 1, "algorithm": "sha256-leading-zeros",
  "address": "0x…", "salt": "0x…", "bits": 16, "puzzles": 480,
  "issuedAt": 1700000000, "expiresAt": 1700000180, "hmac": "0x…" } }
```

### `POST /api/claim`

```json
{ "challenge": { …the challenge above… }, "nonces": [12, 4, 88, …] }
```

On success returns `200`:

```json
{ "ok": true, "address": "0x…", "amountWei": "1000000000000000000",
  "txHash": "0x…", "mined": true, "blockNumber": 1234 }
```

Error responses use `{ "ok": false, "error": { "code": "…", "message": "…" } }`:

| Code | Status | Meaning |
| --- | --- | --- |
| `BAD_REQUEST` | 400 | Missing/invalid address or body. |
| `CHALLENGE_INVALID` | 400 | HMAC mismatch (tampered challenge). |
| `CHALLENGE_EXPIRED` | 400 | Challenge past its TTL. |
| `INVALID_POW` | 400 | A sub-puzzle nonce does not meet the difficulty. |
| `CHALLENGE_REUSED` | 409 | Challenge already redeemed. |
| `COOLDOWN` | 429 | Address recently funded (see `Retry-After`). |
| `FAUCET_BUSY` | 429 | Queue full; retry shortly (see `Retry-After`). |
| `INSUFFICIENT_FUNDS` | 503 | Faucet account is out of funds. |
| `RPC_ERROR` | 502 | The execution node rejected or failed the transaction. |

### `GET /status` and `GET /healthz`

`/status` reports the faucet address, chain id, drip amount, PoW parameters, and
current queue occupancy. `/healthz` is a minimal liveness probe.

## Packaging

```bash
scripts/package.sh
```

Builds with `cargo build --locked --profile release` and writes
`dist/atlas-faucet-<version>-<target>.tar.gz` plus a `.sha256`. GitHub Actions
runs the same script on pushes to `main`, on tags, and via manual dispatch.

## Docker

```bash
docker compose up --build
```

The Compose file points `RPC_URL` at `host.docker.internal:8545` so the
container can reach a node running on the host. Pushes to `main` publish images
to GitHub Packages:

```bash
docker pull ghcr.io/atlas-chain/atlas-faucet:main

docker run -p 28884:28884 \
  --add-host host.docker.internal:host-gateway \
  -e FAUCET_PRIVATE_KEY=0x<funded-key> \
  ghcr.io/atlas-chain/atlas-faucet:main
```

On native Linux the `--add-host host.docker.internal:host-gateway` flag is
required for the default `RPC_URL` to resolve the host node; on Docker Desktop it
is optional, and `docker compose up` adds it automatically.

See [`instructions.md`](instructions.md) for operator notes.

## Security notes

* The default `FAUCET_PRIVATE_KEY` is the **publicly known** Atlas dev key. It is
  fine for a throwaway dev chain and **must** be replaced for anything shared.
* Proof-of-work is rate-limiting, not Sybil-proofing. Pair it with
  `FAUCET_COOLDOWN_SECS` and a modest `FAUCET_DRIP_WEI` for public faucets.
* Set `POW_HMAC_SECRET` in production so issued challenges survive a restart and
  are not forgeable across instances.

## Development

```bash
cargo test     # unit + integration tests (incl. EIP-155 signing vector)
cargo run
```
