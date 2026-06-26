# Operator notes

This document covers running `atlas-faucet` in a real deployment.

## Prerequisites

* An Atlas execution node reachable over JSON-RPC (default `:8545`).
* A funded account on that chain whose private key the faucet will sign with.

## Docker Compose

```yaml
services:
  faucet:
    image: ghcr.io/atlas-chain/atlas-faucet:main   # or build: .
    container_name: atlas-faucet
    ports:
      - "28884:28884"
    environment:
      RPC_URL: "http://host.docker.internal:8545"
      FAUCET_PRIVATE_KEY: "0x<funded-key>"
      FAUCET_DRIP_WEI: "100000000000000000"   # 0.1 ATL
      POW_BITS: "16"
      POW_PUZZLES: "600"
      FAUCET_QUEUE_CAPACITY: "2"
      FAUCET_COOLDOWN_SECS: "3600"             # one drip per address per hour
      POW_HMAC_SECRET: "<random-32-bytes-base64-or-hex>"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    restart: unless-stopped
```

```bash
docker compose up --build -d
docker compose logs -f
```

### Reaching the node

* **Node on the same host:** keep `RPC_URL=http://host.docker.internal:8545`
  (the bundled `extra_hosts` entry makes this resolve on Linux too).
* **Node in the same Compose project:** use the service name, e.g.
  `RPC_URL=http://atlas-node:8545`, and drop `extra_hosts`.

## Production checklist

1. **Replace the signing key.** The default key is public. Set
   `FAUCET_PRIVATE_KEY` to a dedicated, modestly-funded account — never a key
   that holds anything you care about.
2. **Pin a challenge secret.** Set `POW_HMAC_SECRET` so challenges survive
   restarts and cannot be forged. Use 32 random bytes.
3. **Size the drip.** `FAUCET_DRIP_WEI` is the per-claim payout. Combine a small
   payout with `FAUCET_COOLDOWN_SECS` to bound drain rate.
4. **Tune the proof-of-work.** Raise `POW_PUZZLES` to make each claim cost more
   browser CPU. Watch real solve times in your audience's browsers.
5. **Front it with TLS.** Terminate HTTPS at a reverse proxy (nginx/Caddy) and
   forward to `:28884`. The faucet speaks plain HTTP.
6. **Monitor funding.** When the account runs dry, claims return
   `503 INSUFFICIENT_FUNDS`. Watch logs / `/status` and top it up.

## Logs

The service logs newline-delimited JSON to stdout, e.g.:

```json
{"message":"faucet drip dispensed","address":"0x…","amountWei":"1000000000000000000","txHash":"0x…","mined":true,"blockNumber":42}
```

## Health checks

* `GET /healthz` — liveness, plus current queue occupancy.
* `GET /status` — full configuration snapshot (no secrets).
