//! The dispenser: admission control + serialized signing/submission.
//!
//! Concurrency model (the "single client at a time, queue up to two" rule):
//!
//! * `admission` is a [`Semaphore`] with `queue_capacity` permits. A claim must
//!   take a permit to enter the system; when none are free the request is
//!   rejected with `429` (mapped in the server) instead of waiting forever.
//! * `serialize` is an async [`Mutex`] that guarantees only **one** claim is
//!   actually signing/submitting at any moment. With the default capacity of 2,
//!   one claim dispenses while a second waits, and a third is turned away.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::eth::{FaucetSigner, LegacyTransfer};
use crate::rpc::RpcClient;

#[derive(Clone)]
pub struct Faucet {
    signer: FaucetSigner,
    rpc: RpcClient,
    chain_id: u64,
    drip_wei: u128,
    gas_limit: u64,
    gas_price_override: Option<u128>,
    receipt_timeout: Duration,
    admission: Arc<Semaphore>,
    serialize: Arc<Mutex<()>>,
    capacity: usize,
}

#[derive(Clone, Debug)]
pub struct DispenseReceipt {
    pub tx_hash: String,
    pub amount_wei: u128,
    /// The transaction was included in a block within the receipt timeout.
    pub mined: bool,
    /// The mined transaction succeeded (receipt status `0x1`). Only meaningful
    /// when `mined` is true; a plain value transfer to an EOA always succeeds,
    /// but a contract recipient can revert.
    pub transferred: bool,
    pub block_number: Option<u64>,
}

struct ReceiptOutcome {
    mined: bool,
    transferred: bool,
    block_number: Option<u64>,
}

#[derive(Debug)]
pub enum DispenseError {
    Rpc(String),
    InsufficientFunds { balance: u128, needed: u128 },
}

impl std::fmt::Display for DispenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispenseError::Rpc(message) => write!(f, "{message}"),
            DispenseError::InsufficientFunds { balance, needed } => write!(
                f,
                "faucet account is underfunded: balance {balance} wei, need {needed} wei"
            ),
        }
    }
}

impl Faucet {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signer: FaucetSigner,
        rpc: RpcClient,
        chain_id: u64,
        drip_wei: u128,
        gas_limit: u64,
        gas_price_override: Option<u128>,
        receipt_timeout: Duration,
        capacity: usize,
    ) -> Self {
        Self {
            signer,
            rpc,
            chain_id,
            drip_wei,
            gas_limit,
            gas_price_override,
            receipt_timeout,
            admission: Arc::new(Semaphore::new(capacity)),
            serialize: Arc::new(Mutex::new(())),
            capacity,
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn drip_wei(&self) -> u128 {
        self.drip_wei
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn faucet_address_hex(&self) -> String {
        self.signer.address_hex()
    }

    /// Number of claims currently occupying a slot (processing + waiting).
    pub fn in_flight(&self) -> usize {
        self.capacity.saturating_sub(self.admission.available_permits())
    }

    /// Try to enter the system. Returns `None` when the queue is full.
    pub fn try_admit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.admission).try_acquire_owned().ok()
    }

    /// Sign and submit the drip transaction. The caller must hold an admission
    /// permit (from [`Faucet::try_admit`]). The serialize lock and the permit
    /// are held only across nonce-fetch/sign/submit — which is all that nonce
    /// ordering requires, since `eth_getTransactionCount("pending")` reflects
    /// the submitted tx once `send_raw_transaction` returns — so the (possibly
    /// long) receipt wait does not block or starve other claims.
    pub async fn dispense(
        &self,
        permit: OwnedSemaphorePermit,
        to: [u8; 20],
    ) -> Result<DispenseReceipt, DispenseError> {
        let tx_hash = {
            let _serialize = self.serialize.lock().await;
            self.submit(to).await?
        };
        drop(permit);

        let outcome = self.await_receipt(&tx_hash).await;
        Ok(DispenseReceipt {
            tx_hash,
            amount_wei: self.drip_wei,
            mined: outcome.mined,
            transferred: outcome.transferred,
            block_number: outcome.block_number,
        })
    }

    /// Build, sign and broadcast the transfer, returning its hash. Runs entirely
    /// under the serialize lock so nonces stay sequential.
    async fn submit(&self, to: [u8; 20]) -> Result<String, DispenseError> {
        let from = self.signer.address_hex();

        let gas_price = match self.gas_price_override {
            Some(price) => price,
            None => self
                .rpc
                .gas_price()
                .await
                .map_err(DispenseError::Rpc)?
                .max(1),
        };

        let balance = self.rpc.balance(&from).await.map_err(DispenseError::Rpc)?;
        let needed = self
            .drip_wei
            .saturating_add(gas_price.saturating_mul(self.gas_limit as u128));
        if balance < needed {
            return Err(DispenseError::InsufficientFunds { balance, needed });
        }

        let nonce = self
            .rpc
            .transaction_count(&from)
            .await
            .map_err(DispenseError::Rpc)?;

        let tx = LegacyTransfer {
            chain_id: self.chain_id,
            nonce,
            gas_price,
            gas_limit: self.gas_limit,
            to,
            value: self.drip_wei,
        };
        let signed = self.signer.sign_legacy_transfer(&tx);

        self.rpc
            .send_raw_transaction(&signed.raw)
            .await
            .map_err(DispenseError::Rpc)
    }

    #[cfg(test)]
    fn test_instance(capacity: usize) -> Self {
        let signer = FaucetSigner::from_private_key_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        Self::new(
            signer,
            RpcClient::new("http://127.0.0.1:1".to_string()),
            1337,
            1,
            21_000,
            Some(1),
            Duration::from_secs(0),
            capacity,
        )
    }

    /// Poll for the transaction receipt until it appears or the timeout elapses.
    /// A timed-out wait is not an error: the transaction was accepted by the
    /// node and the hash is returned with `mined = false`.
    async fn await_receipt(&self, tx_hash: &str) -> ReceiptOutcome {
        if self.receipt_timeout.is_zero() {
            return ReceiptOutcome {
                mined: false,
                transferred: false,
                block_number: None,
            };
        }
        let deadline = Instant::now() + self.receipt_timeout;
        loop {
            if let Ok(Some(receipt)) = self.rpc.transaction_receipt(tx_hash).await {
                let block_number = receipt
                    .get("blockNumber")
                    .and_then(|value| value.as_str())
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());
                // A missing status field (pre-Byzantium) is treated as success
                // to avoid false negatives; "0x0" is an explicit revert.
                let transferred = receipt
                    .get("status")
                    .and_then(|value| value.as_str())
                    .map(|status| status != "0x0")
                    .unwrap_or(true);
                return ReceiptOutcome {
                    mined: true,
                    transferred,
                    block_number,
                };
            }
            if Instant::now() >= deadline {
                return ReceiptOutcome {
                    mined: false,
                    transferred: false,
                    block_number: None,
                };
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_caps_at_capacity_then_rejects() {
        let faucet = Faucet::test_instance(2);
        assert_eq!(faucet.in_flight(), 0);

        let p1 = faucet.try_admit();
        assert!(p1.is_some());
        assert_eq!(faucet.in_flight(), 1);

        let p2 = faucet.try_admit();
        assert!(p2.is_some());
        assert_eq!(faucet.in_flight(), 2);

        // Queue is full: a third claim is turned away.
        assert!(faucet.try_admit().is_none());

        // Releasing a permit frees a slot again.
        drop(p1);
        assert_eq!(faucet.in_flight(), 1);
        assert!(faucet.try_admit().is_some());
    }
}
