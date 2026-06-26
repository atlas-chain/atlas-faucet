//! HTTP surface: the browser UI plus the challenge / claim JSON API.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::faucet::{DispenseError, Faucet};
use crate::frontend::INDEX_HTML;
use crate::pow::{Challenge, ChallengeError, PowKeeper, verify_solution};
use crate::util::{checksum_address, parse_address};

#[derive(Clone)]
pub struct AppState {
    pub faucet: Arc<Faucet>,
    pub keeper: Arc<PowKeeper>,
    pub html_title: Arc<String>,
    pub pow_bits: u32,
    pub pow_puzzles: u32,
    pub pow_ttl_secs: u64,
    pub cooldown_secs: u64,
}

#[derive(Deserialize)]
struct ClaimRequest {
    challenge: Challenge,
    nonces: Vec<u32>,
}

pub async fn run_server(state: AppState, listen_host: String, listen_port: u16) {
    let bind_address = format!("{listen_host}:{listen_port}");

    let listener = match TcpListener::bind(&bind_address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("failed to bind HTTP server on {bind_address}: {error}");
            return;
        }
    };

    println!(
        "{}",
        json!({
            "message": "atlas faucet listening",
            "ui": format!("http://{bind_address}/"),
            "faucetAddress": state.faucet.faucet_address_hex(),
            "chainId": state.faucet.chain_id(),
            "dripWei": state.faucet.drip_wei().to_string(),
            "queueCapacity": state.faucet.capacity(),
            "powBits": state.pow_bits,
            "powPuzzles": state.pow_puzzles,
            "endpoints": ["/", "/status", "/healthz", "/api/challenge", "/api/claim"],
        })
    );

    if let Err(error) = axum::serve(listener, build_router(state)).await {
        eprintln!("HTTP server failed: {error}");
    }
}

/// Build the faucet's HTTP router. Exposed so integration tests can serve it on
/// an ephemeral port.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/healthz", get(health_handler))
        .route("/status", get(status_handler))
        .route("/api/challenge", get(challenge_handler))
        .route("/api/claim", post(claim_handler))
        .fallback(not_found_handler)
        .with_state(state)
}

async fn index_handler(State(state): State<AppState>) -> Response {
    let html = INDEX_HTML.replace("__HTML_TITLE__", &escape_html(state.html_title.as_str()));
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

async fn health_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "atlas-faucet",
        "inFlight": state.faucet.in_flight(),
        "queueCapacity": state.faucet.capacity(),
    }))
}

async fn status_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "atlas-faucet",
        "faucetAddress": state.faucet.faucet_address_hex(),
        "chainId": state.faucet.chain_id(),
        "dripWei": state.faucet.drip_wei().to_string(),
        "queueCapacity": state.faucet.capacity(),
        "inFlight": state.faucet.in_flight(),
        "cooldownSecs": state.cooldown_secs,
        "pow": {
            "algorithm": "sha256-leading-zeros",
            "bits": state.pow_bits,
            "puzzles": state.pow_puzzles,
            "ttlSecs": state.pow_ttl_secs,
        },
        "endpoints": ["/", "/status", "/healthz", "/api/challenge", "/api/claim"],
    }))
}

async fn challenge_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(address_raw) = params.get("address") else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "missing 'address' query parameter",
        );
    };

    let address = match parse_address(address_raw) {
        Ok(address) => address,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                format!("invalid address: {error}"),
            );
        }
    };

    let now = now_secs();
    let remaining = state
        .keeper
        .cooldown_remaining(&address, state.cooldown_secs, now);
    if remaining > 0 {
        return cooldown_response(remaining);
    }

    let challenge = state.keeper.issue(
        &address,
        state.pow_bits,
        state.pow_puzzles,
        state.pow_ttl_secs,
        now,
    );

    (StatusCode::OK, Json(json!({ "ok": true, "challenge": challenge }))).into_response()
}

async fn claim_handler(State(state): State<AppState>, body: Bytes) -> Response {
    // Parse explicitly so a malformed body returns our JSON error envelope
    // (axum's default Json rejection is a plain-text 422).
    let request: ClaimRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                format!("invalid request body: {error}"),
            );
        }
    };

    let now = now_secs();

    // 1. Authenticate the challenge envelope.
    let verified = match state.keeper.verify_challenge(&request.challenge, now) {
        Ok(verified) => verified,
        Err(ChallengeError::Expired) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "CHALLENGE_EXPIRED",
                "challenge expired; request a new one",
            );
        }
        Err(ChallengeError::Tampered) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "CHALLENGE_INVALID",
                "challenge signature is invalid",
            );
        }
        Err(error) => {
            return error_response(StatusCode::BAD_REQUEST, "BAD_REQUEST", error.to_string());
        }
    };

    // 2. Verify the proof-of-work.
    if let Err(error) = verify_solution(&verified, &request.nonces) {
        return error_response(StatusCode::BAD_REQUEST, "INVALID_POW", error.to_string());
    }

    // 3. Atomically reserve the per-address cooldown slot. This both enforces
    //    the cooldown and serializes concurrent claims for the same address, so
    //    one recipient cannot collect multiple drips at once. The reservation is
    //    held (as the cooldown record) on success and released on failure.
    if let Err(remaining) =
        state
            .keeper
            .try_begin_claim(&verified.address, state.cooldown_secs, now)
    {
        return cooldown_response(remaining);
    }

    // 4. Admission: only `queue_capacity` claims may be in the system at once.
    let Some(permit) = state.faucet.try_admit() else {
        state.keeper.release_claim(&verified.address, now);
        return busy_response(state.faucet.capacity());
    };

    // 5. Replay protection: burn the challenge now that it is admitted.
    if !state.keeper.consume_salt(&verified.salt, verified.expires_at, now) {
        drop(permit);
        state.keeper.release_claim(&verified.address, now);
        return error_response(
            StatusCode::CONFLICT,
            "CHALLENGE_REUSED",
            "challenge has already been redeemed",
        );
    }

    // 6. Dispense (serialized to one at a time inside the faucet).
    match state.faucet.dispense(permit, verified.address).await {
        Ok(receipt) => {
            // Keep the cooldown reservation (the address is now funded).
            let address = checksum_address(&verified.address);
            println!(
                "{}",
                json!({
                    "message": "faucet drip dispensed",
                    "address": address,
                    "amountWei": receipt.amount_wei.to_string(),
                    "txHash": receipt.tx_hash,
                    "mined": receipt.mined,
                    "transferred": receipt.transferred,
                    "blockNumber": receipt.block_number,
                })
            );
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "address": address,
                    "amountWei": receipt.amount_wei.to_string(),
                    "txHash": receipt.tx_hash,
                    "mined": receipt.mined,
                    "transferred": receipt.transferred,
                    "blockNumber": receipt.block_number,
                })),
            )
                .into_response()
        }
        // Both error variants occur before a transaction is broadcast, so we
        // release the address reservation and reclaim the salt for retry.
        Err(error @ DispenseError::InsufficientFunds { .. }) => {
            state.keeper.release_claim(&verified.address, now);
            state.keeper.release_salt(&verified.salt);
            eprintln!("{}", json!({ "message": "dispense failed", "error": error.to_string() }));
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "INSUFFICIENT_FUNDS",
                error.to_string(),
            )
        }
        Err(DispenseError::Rpc(message)) => {
            state.keeper.release_claim(&verified.address, now);
            state.keeper.release_salt(&verified.salt);
            eprintln!("{}", json!({ "message": "dispense failed", "error": message }));
            error_response(StatusCode::BAD_GATEWAY, "RPC_ERROR", message)
        }
    }
}

async fn not_found_handler() -> Response {
    error_response(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn busy_response(capacity: usize) -> Response {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "FAUCET_BUSY",
        format!("faucet is busy (queue capacity {capacity}); please retry shortly"),
    );
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

fn cooldown_response(remaining: u64) -> Response {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "COOLDOWN",
        format!("address recently funded; try again in {remaining}s"),
    );
    if let Ok(value) = HeaderValue::from_str(&remaining.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

fn error_response<S>(status: StatusCode, code: &str, message: S) -> Response
where
    S: AsRef<str>,
{
    (
        status,
        Json(json!({
            "ok": false,
            "error": { "code": code, "message": message.as_ref() },
        })),
    )
        .into_response()
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::FaucetSigner;
    use crate::pow::{PowKeeper, solve_puzzle};
    use crate::rpc::RpcClient;
    use std::time::Duration;

    fn test_state(rpc_url: String, cooldown: u64) -> AppState {
        let signer = FaucetSigner::from_private_key_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        let faucet = Faucet::new(
            signer,
            RpcClient::new(rpc_url),
            1337,
            1_000_000_000_000_000_000,
            21_000,
            Some(1_000_000_000),
            Duration::from_secs(0),
            2,
        );
        AppState {
            faucet: Arc::new(faucet),
            keeper: Arc::new(PowKeeper::new(b"unit-secret".to_vec())),
            html_title: Arc::new("Atlas Faucet".to_string()),
            pow_bits: 6,
            pow_puzzles: 4,
            pow_ttl_secs: 120,
            cooldown_secs: cooldown,
        }
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }

    fn solved_nonces(challenge: &Challenge, keeper: &PowKeeper) -> Vec<u32> {
        let verified = keeper.verify_challenge(challenge, challenge.issued_at + 1).unwrap();
        (0..verified.puzzles)
            .map(|k| solve_puzzle(&verified.salt, &verified.address, verified.bits, k))
            .collect()
    }

    #[tokio::test]
    async fn challenge_requires_valid_address() {
        let state = test_state("http://127.0.0.1:1".to_string(), 0);
        let response = challenge_handler(
            State(state),
            Query(HashMap::from([("address".to_string(), "0xnothex".to_string())])),
        )
        .await;
        let (status, body) = body_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "BAD_REQUEST");
    }

    #[tokio::test]
    async fn claim_rejects_bad_pow() {
        let state = test_state("http://127.0.0.1:1".to_string(), 0);
        let challenge = state.keeper.issue(
            &parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap(),
            state.pow_bits,
            state.pow_puzzles,
            state.pow_ttl_secs,
            now_secs(),
        );
        let raw = serde_json::to_vec(&json!({ "challenge": challenge, "nonces": [0, 0, 0, 0] }))
            .unwrap();
        let response = claim_handler(State(state), Bytes::from(raw)).await;
        let (status, body) = body_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_POW");
    }

    #[tokio::test]
    async fn claim_rejects_replayed_challenge() {
        // RPC points nowhere, but replay is rejected before any RPC call.
        let state = test_state("http://127.0.0.1:1".to_string(), 0);
        let address = parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap();
        let challenge = state.keeper.issue(
            &address,
            state.pow_bits,
            state.pow_puzzles,
            state.pow_ttl_secs,
            now_secs(),
        );
        let nonces = solved_nonces(&challenge, &state.keeper);

        // Pre-consume the salt to simulate a prior successful redemption.
        let verified = state
            .keeper
            .verify_challenge(&challenge, now_secs())
            .unwrap();
        assert!(state
            .keeper
            .consume_salt(&verified.salt, verified.expires_at, now_secs()));

        let raw = serde_json::to_vec(&json!({ "challenge": challenge, "nonces": nonces })).unwrap();
        let response = claim_handler(State(state), Bytes::from(raw)).await;
        let (status, body) = body_json(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "CHALLENGE_REUSED");
    }
}
