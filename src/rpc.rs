//! Tiny async Ethereum JSON-RPC client built on `reqwest`.
//!
//! Only the handful of methods the faucet needs are implemented:
//! `eth_chainId`, `eth_getTransactionCount`, `eth_gasPrice`, `eth_getBalance`,
//! `eth_sendRawTransaction`, and `eth_getTransactionReceipt`.

use reqwest::Client;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct RpcClient {
    client: Client,
    url: String,
}

impl RpcClient {
    pub fn new(url: String) -> Self {
        Self {
            client: Client::new(),
            url,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("{method} request to {} failed: {error}", self.url))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("{method} response body read failed: {error}"))?;

        let body: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{method} returned invalid JSON (HTTP {status}): {error}"))?;

        if let Some(error) = body.get("error").filter(|error| !error.is_null()) {
            return Err(format!("{method} rpc error: {error}"));
        }

        body.get("result")
            .cloned()
            .ok_or_else(|| format!("{method} response missing result: {text}"))
    }

    pub async fn chain_id(&self) -> Result<u64, String> {
        let result = self.call("eth_chainId", json!([])).await?;
        parse_hex_u64(&result, "eth_chainId")
    }

    pub async fn transaction_count(&self, address: &str) -> Result<u64, String> {
        let result = self
            .call(
                "eth_getTransactionCount",
                json!([address, "pending"]),
            )
            .await?;
        parse_hex_u64(&result, "eth_getTransactionCount")
    }

    pub async fn gas_price(&self) -> Result<u128, String> {
        let result = self.call("eth_gasPrice", json!([])).await?;
        parse_hex_u128(&result, "eth_gasPrice")
    }

    pub async fn balance(&self, address: &str) -> Result<u128, String> {
        // Use the "pending" tag to match `transaction_count`, so an unmined
        // outgoing drip is already reflected in the balance we check against.
        let result = self
            .call("eth_getBalance", json!([address, "pending"]))
            .await?;
        parse_hex_u128(&result, "eth_getBalance")
    }

    pub async fn send_raw_transaction(&self, raw: &str) -> Result<String, String> {
        let result = self
            .call("eth_sendRawTransaction", json!([raw]))
            .await?;
        result
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "eth_sendRawTransaction did not return a tx hash".to_string())
    }

    pub async fn transaction_receipt(&self, tx_hash: &str) -> Result<Option<Value>, String> {
        let result = self
            .call("eth_getTransactionReceipt", json!([tx_hash]))
            .await?;
        if result.is_null() {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }
}

fn parse_hex_u64(value: &Value, method: &str) -> Result<u64, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{method} returned a non-string quantity"))?;
    let body = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(body, 16)
        .map_err(|error| format!("{method} returned an unparseable quantity {text}: {error}"))
}

fn parse_hex_u128(value: &Value, method: &str) -> Result<u128, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{method} returned a non-string quantity"))?;
    let body = text.strip_prefix("0x").unwrap_or(text);
    u128::from_str_radix(body, 16)
        .map_err(|error| format!("{method} returned an unparseable quantity {text}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_quantities() {
        assert_eq!(parse_hex_u64(&json!("0x539"), "m").unwrap(), 1337);
        assert_eq!(parse_hex_u64(&json!("0x0"), "m").unwrap(), 0);
        assert_eq!(
            parse_hex_u128(&json!("0xde0b6b3a7640000"), "m").unwrap(),
            1_000_000_000_000_000_000
        );
        assert!(parse_hex_u64(&json!(1234), "m").is_err());
        assert!(parse_hex_u64(&json!("0xzz"), "m").is_err());
    }
}
