//! End-to-end test of the full claim flow over real HTTP:
//!
//!   browser → GET /api/challenge → solve PoW → POST /api/claim → faucet signs
//!   → mock JSON-RPC node decodes & verifies the signed transaction.
//!
//! The mock node recovers the transaction sender and asserts the faucet sent
//! exactly the configured drip to the requested recipient, proving the signing
//! and submission path works without needing a real chain.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_faucet::eth::{FaucetSigner, recover_legacy_transfer};
use atlas_faucet::faucet::Faucet;
use atlas_faucet::pow::{PowKeeper, solve_puzzle};
use atlas_faucet::rpc::RpcClient;
use atlas_faucet::server::{AppState, build_router};
use atlas_faucet::util::{decode_fixed_hex, decode_flexible_hex, keccak256, parse_address, prefixed_hex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const DEV_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const FAUCET_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
const RECIPIENT: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const DRIP_WEI: u128 = 1_000_000_000_000_000_000;

#[derive(Clone)]
struct MockNode {
    sent: Arc<Mutex<Vec<String>>>,
}

async fn rpc_handler(State(node): State<MockNode>, Json(request): Json<Value>) -> Json<Value> {
    let method = request["method"].as_str().unwrap_or_default();
    let id = request["id"].clone();
    let result = match method {
        "eth_chainId" => json!("0x539"),                 // 1337
        "eth_gasPrice" => json!("0x3b9aca00"),           // 1 gwei
        "eth_getBalance" => json!("0x21e19e0c9bab2400000"), // 10000 ETH
        "eth_getTransactionCount" => json!("0x0"),
        "eth_sendRawTransaction" => {
            let raw = request["params"][0].as_str().unwrap().to_string();
            let hash = prefixed_hex(&keccak256(&decode_flexible_hex(&raw).unwrap()));
            node.sent.lock().unwrap().push(raw);
            json!(hash)
        }
        "eth_getTransactionReceipt" => json!({ "blockNumber": "0x1", "status": "0x1" }),
        _ => json!(null),
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

async fn spawn(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

fn faucet_state(rpc_url: String) -> (AppState, MockNode) {
    let node = MockNode {
        sent: Arc::new(Mutex::new(Vec::new())),
    };
    let signer = FaucetSigner::from_private_key_hex(DEV_KEY).unwrap();
    let faucet = Faucet::new(
        signer,
        RpcClient::new(rpc_url),
        1337,
        DRIP_WEI,
        21_000,
        Some(1_000_000_000),
        Duration::from_secs(2),
        2,
    );
    let state = AppState {
        faucet: Arc::new(faucet),
        keeper: Arc::new(PowKeeper::new(b"e2e-secret".to_vec())),
        html_title: Arc::new("Atlas Faucet".to_string()),
        pow_bits: 8,
        pow_puzzles: 6,
        pow_ttl_secs: 120,
        cooldown_secs: 0,
    };
    (state, node)
}

fn solve_from_challenge(challenge: &Value) -> Vec<u32> {
    let salt_vec = decode_fixed_hex(challenge["salt"].as_str().unwrap(), 16).unwrap();
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&salt_vec);
    let address = parse_address(challenge["address"].as_str().unwrap()).unwrap();
    let bits = challenge["bits"].as_u64().unwrap() as u32;
    let puzzles = challenge["puzzles"].as_u64().unwrap() as u32;
    (0..puzzles)
        .map(|k| solve_puzzle(&salt, &address, bits, k))
        .collect()
}

#[tokio::test]
async fn full_claim_flow_signs_and_submits_correct_transaction() {
    // Mock node.
    let node = MockNode {
        sent: Arc::new(Mutex::new(Vec::new())),
    };
    let rpc_url = spawn(
        Router::new()
            .route("/", post(rpc_handler))
            .with_state(node.clone()),
    )
    .await;

    // Faucet server wired to the mock node.
    let signer = FaucetSigner::from_private_key_hex(DEV_KEY).unwrap();
    let faucet = Faucet::new(
        signer,
        RpcClient::new(rpc_url),
        1337,
        DRIP_WEI,
        21_000,
        Some(1_000_000_000),
        Duration::from_secs(2),
        2,
    );
    let state = AppState {
        faucet: Arc::new(faucet),
        keeper: Arc::new(PowKeeper::new(b"e2e-secret".to_vec())),
        html_title: Arc::new("Atlas Faucet".to_string()),
        pow_bits: 8,
        pow_puzzles: 6,
        pow_ttl_secs: 120,
        cooldown_secs: 0,
    };
    let base = spawn(build_router(state)).await;

    let client = reqwest::Client::new();

    // 1. Get a challenge.
    let challenge_res = client
        .get(format!("{base}/api/challenge?address={RECIPIENT}"))
        .send()
        .await
        .unwrap();
    assert!(challenge_res.status().is_success());
    let challenge_body: Value = challenge_res.json().await.unwrap();
    let challenge = challenge_body["challenge"].clone();

    // 2. Solve the proof-of-work from the challenge alone.
    let nonces = solve_from_challenge(&challenge);

    // 3. Claim.
    let claim_res = client
        .post(format!("{base}/api/claim"))
        .json(&json!({ "challenge": challenge, "nonces": nonces }))
        .send()
        .await
        .unwrap();
    let status = claim_res.status();
    let claim_body: Value = claim_res.json().await.unwrap();
    assert_eq!(status, 200, "claim failed: {claim_body}");
    assert_eq!(claim_body["ok"], json!(true));
    assert_eq!(claim_body["mined"], json!(true));
    assert_eq!(claim_body["amountWei"], json!(DRIP_WEI.to_string()));
    let tx_hash = claim_body["txHash"].as_str().unwrap().to_string();

    // 4. The mock node received exactly one correctly-signed transaction.
    let sent = node.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "expected exactly one raw transaction");
    let decoded = recover_legacy_transfer(&sent[0]).unwrap();
    assert_eq!(prefixed_hex(&decoded.from), FAUCET_ADDRESS);
    assert_eq!(prefixed_hex(&decoded.to), RECIPIENT.to_lowercase());
    assert_eq!(decoded.value, DRIP_WEI);
    assert_eq!(decoded.nonce, 0);
    assert_eq!(decoded.chain_id, 1337);
    assert_eq!(
        tx_hash,
        prefixed_hex(&keccak256(&decode_flexible_hex(&sent[0]).unwrap()))
    );

    // 5. Replaying the same challenge is rejected and submits nothing more.
    let replay_res = client
        .post(format!("{base}/api/claim"))
        .json(&json!({ "challenge": challenge, "nonces": nonces }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay_res.status(), 409);
    let replay_body: Value = replay_res.json().await.unwrap();
    assert_eq!(replay_body["error"]["code"], "CHALLENGE_REUSED");
    assert_eq!(node.sent.lock().unwrap().len(), 1, "no new transaction on replay");
}

#[tokio::test]
async fn concurrent_same_address_claims_yield_one_drip() {
    // Two DISTINCT valid challenges for one recipient, claimed simultaneously,
    // must produce exactly one drip when a cooldown is configured.
    let node = MockNode {
        sent: Arc::new(Mutex::new(Vec::new())),
    };
    let rpc_url = spawn(
        Router::new()
            .route("/", post(rpc_handler))
            .with_state(node.clone()),
    )
    .await;

    let signer = FaucetSigner::from_private_key_hex(DEV_KEY).unwrap();
    let faucet = Faucet::new(
        signer,
        RpcClient::new(rpc_url),
        1337,
        DRIP_WEI,
        21_000,
        Some(1_000_000_000),
        Duration::from_secs(2),
        2,
    );
    let state = AppState {
        faucet: Arc::new(faucet),
        keeper: Arc::new(PowKeeper::new(b"e2e-secret".to_vec())),
        html_title: Arc::new("Atlas Faucet".to_string()),
        pow_bits: 8,
        pow_puzzles: 6,
        pow_ttl_secs: 120,
        cooldown_secs: 3600,
    };
    let base = spawn(build_router(state)).await;
    let client = reqwest::Client::new();

    let mut challenges = Vec::new();
    for _ in 0..2 {
        let body: Value = client
            .get(format!("{base}/api/challenge?address={RECIPIENT}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        challenges.push(body["challenge"].clone());
    }
    let solved: Vec<Vec<u32>> = challenges.iter().map(solve_from_challenge).collect();

    let claim_url = format!("{base}/api/claim");
    let (c0, u0, b0) = (client.clone(), claim_url.clone(), json!({ "challenge": challenges[0], "nonces": solved[0] }));
    let (c1, u1, b1) = (client.clone(), claim_url.clone(), json!({ "challenge": challenges[1], "nonces": solved[1] }));
    let f0 = async move { c0.post(u0).json(&b0).send().await.unwrap() };
    let f1 = async move { c1.post(u1).json(&b1).send().await.unwrap() };
    let (r0, r1) = tokio::join!(f0, f1);

    let mut statuses = [r0.status().as_u16(), r1.status().as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 429], "exactly one claim should succeed");
    assert_eq!(
        node.sent.lock().unwrap().len(),
        1,
        "only one transaction should be submitted for the address"
    );
}

#[tokio::test]
async fn tampered_challenge_is_rejected() {
    let (state, node) = faucet_state("http://127.0.0.1:1".to_string());
    let base = spawn(build_router(state)).await;
    let client = reqwest::Client::new();

    let challenge_body: Value = client
        .get(format!("{base}/api/challenge?address={RECIPIENT}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut challenge = challenge_body["challenge"].clone();
    let nonces = solve_from_challenge(&challenge);

    // Lower the difficulty after signing — the HMAC no longer matches.
    challenge["puzzles"] = json!(1);

    let res = client
        .post(format!("{base}/api/claim"))
        .json(&json!({ "challenge": challenge, "nonces": nonces }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "CHALLENGE_INVALID");
    assert!(node.sent.lock().unwrap().is_empty());
}
