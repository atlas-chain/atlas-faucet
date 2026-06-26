# Deployment

How to deploy a released `atlas-faucet`. Every push to `main` and every
tag builds, tests, and publishes artifacts via the
[`Package`](.github/workflows/package.yml) workflow:

* a **Docker image** to the GitHub Container Registry, and
* a **release tarball** (statically-linked-ish Linux binary + docs) attached
  to the GitHub Release for tags.

For day-to-day operator notes (reaching the node, logs, health) see
[`instructions.md`](instructions.md); for the full env-var reference see
[`README.md`](README.md#configuration). This document is about getting a
release onto a server.

## Published artifacts

### Docker images (GHCR)

`ghcr.io/atlas-chain/atlas-faucet`, tagged by the workflow as:

| Tag | When | Use for |
| --- | --- | --- |
| `vX.Y.Z` (e.g. `v0.1.0`) | on each git tag | **pinned production deploys** |
| `latest` | on each git tag | newest tagged release |
| `main` | on each push to `main` | tracking the development tip |
| `sha-<short>` | every build | reproducing an exact commit |

Pin to a `vX.Y.Z` tag in production so a deploy is reproducible.

### Release tarball

Each git tag's GitHub Release carries
`atlas-faucet-<version>-x86_64-unknown-linux-gnu.tar.gz` and a matching
`.sha256`. The archive contains the `atlas-faucet` binary plus `README.md`,
`instructions.md`, `Dockerfile`, `docker-compose.yml`, and a `package.json`
recording the exact git ref/SHA it was built from.

---

## Option A — Docker (recommended)

```bash
docker pull ghcr.io/atlas-chain/atlas-faucet:v0.1.0

docker run -d --name atlas-faucet \
  -p 28884:28884 \
  --restart unless-stopped \
  --add-host host.docker.internal:host-gateway \
  -e RPC_URL="http://host.docker.internal:8545" \
  -e FAUCET_PRIVATE_KEY="0x<funded-key>" \
  -e POW_HMAC_SECRET="$(openssl rand -hex 32)" \
  -e FAUCET_DRIP_WEI="100000000000000000" \
  -e FAUCET_COOLDOWN_SECS="3600" \
  ghcr.io/atlas-chain/atlas-faucet:v0.1.0
```

* `--add-host host.docker.internal:host-gateway` lets the container reach a
  node running on the **host** at `:8545` (required on native Linux; Docker
  Desktop adds it automatically). If the node runs in the same Docker
  network, set `RPC_URL=http://<node-service>:8545` and drop `--add-host`.
* The image is private by default. On a server, authenticate first:
  `echo $GHCR_TOKEN | docker login ghcr.io -u <user> --password-stdin`
  (a PAT with `read:packages`), or have the org make the package public.

### Docker Compose

The repo's [`docker-compose.yml`](docker-compose.yml) builds from source. For
a released image, point `image:` at the registry and drop `build:`:

```yaml
services:
  faucet:
    image: ghcr.io/atlas-chain/atlas-faucet:v0.1.0
    container_name: atlas-faucet
    ports:
      - "28884:28884"
    environment:
      RPC_URL: "http://host.docker.internal:8545"
      FAUCET_PRIVATE_KEY: "0x<funded-key>"
      POW_HMAC_SECRET: "<32-random-bytes-hex>"
      FAUCET_DRIP_WEI: "100000000000000000"   # 0.1 ATL
      FAUCET_COOLDOWN_SECS: "3600"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    restart: unless-stopped
```

```bash
docker compose up -d && docker compose logs -f
```

---

## Option B — Prebuilt binary (release tarball)

For a host without Docker. Download from the Release, verify the checksum,
and install:

```bash
VERSION=0.1.0
BASE="https://github.com/atlas-chain/atlas-faucet/releases/download/v${VERSION}"
ARCHIVE="atlas-faucet-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"

curl -fsSLO "${BASE}/${ARCHIVE}"
curl -fsSLO "${BASE}/${ARCHIVE}.sha256"
sha256sum -c "${ARCHIVE}.sha256"          # must print: OK

tar -xzf "${ARCHIVE}"
sudo install -m 0755 \
  "atlas-faucet-${VERSION}-x86_64-unknown-linux-gnu/atlas-faucet" \
  /usr/local/bin/atlas-faucet
```

(`gh release download v0.1.0 -R atlas-chain/atlas-faucet` works too.)

### systemd unit

```ini
# /etc/systemd/system/atlas-faucet.service
[Unit]
Description=Atlas Faucet
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/atlas-faucet
EnvironmentFile=/etc/atlas-faucet.env
Restart=on-failure
RestartSec=5
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

```bash
# /etc/atlas-faucet.env  (chmod 600 — it holds the signing key)
LISTEN_HOST=127.0.0.1
LISTEN_PORT=28884
RPC_URL=http://127.0.0.1:8545
FAUCET_PRIVATE_KEY=0x<funded-key>
POW_HMAC_SECRET=<32-random-bytes-hex>
FAUCET_DRIP_WEI=100000000000000000
FAUCET_COOLDOWN_SECS=3600
```

```bash
sudo install -m 600 /dev/stdin /etc/atlas-faucet.env <<'EOF'
... contents above ...
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now atlas-faucet
sudo systemctl status atlas-faucet
journalctl -u atlas-faucet -f
```

---

## Option C — Build from source

```bash
git clone https://github.com/atlas-chain/atlas-faucet
cd atlas-faucet
cargo build --locked --release      # ./target/release/atlas-faucet
# or produce a distributable tarball like CI does:
scripts/package.sh                  # writes dist/*.tar.gz + .sha256
```

Requires the Rust toolchain (CI builds with stable; the project targets
edition 2024).

---

## TLS / reverse proxy

The faucet speaks plain HTTP. Terminate TLS in front of it and forward to
`127.0.0.1:28884`. Bind the faucet to localhost (`LISTEN_HOST=127.0.0.1`)
so only the proxy can reach it.

**Caddy** (automatic certificates):

```
faucet.example.com {
    reverse_proxy 127.0.0.1:28884
}
```

**nginx:**

```nginx
server {
    listen 443 ssl;
    server_name faucet.example.com;
    # ssl_certificate / ssl_certificate_key ...
    location / {
        proxy_pass http://127.0.0.1:28884;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
    }
}
```

---

## Production checklist

1. **Replace the signing key.** The default `FAUCET_PRIVATE_KEY` is the
   publicly-known Atlas dev key. Use a dedicated, modestly-funded account.
2. **Pin `POW_HMAC_SECRET`** (32 random bytes) so issued challenges survive a
   restart and can't be forged across instances.
3. **Size the payout.** Pair a small `FAUCET_DRIP_WEI` with
   `FAUCET_COOLDOWN_SECS` to bound the drain rate per address.
4. **Tune the proof-of-work.** Raise `POW_PUZZLES` to make each claim cost
   more browser CPU; watch real solve times for your audience.
5. **Front with TLS** and bind the faucet to localhost.
6. **Monitor funding.** A dry account returns `503 INSUFFICIENT_FUNDS`; watch
   `/status` and `journalctl`/`docker logs`, and top it up.

## Verify a running deployment

```bash
curl -fsS http://127.0.0.1:28884/healthz        # liveness + queue occupancy
curl -fsS http://127.0.0.1:28884/status | jq    # address, chain id, drip, PoW
```

Then open the UI in a browser, paste an address, and confirm a drip lands.

## Upgrading

* **Docker:** `docker pull …:vX.Y.Z` (or `:latest`), then
  `docker compose up -d` / re-run the container. State is in-memory only
  (cooldowns, consumed challenges), so a restart simply clears them.
* **Binary:** download the new tarball, replace `/usr/local/bin/atlas-faucet`,
  `sudo systemctl restart atlas-faucet`.
