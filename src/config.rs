//! Environment-variable configuration for the faucet.
//!
//! Every knob is read from the environment with [`envy`], matching the
//! configuration style of `atlas-payload-provider`. Command-line arguments
//! are intentionally not supported (see `main.rs`).

use std::num::{NonZeroU16, NonZeroUsize};

use serde::Deserialize;

const DEFAULT_LISTEN_HOST: &str = "0.0.0.0";
const DEFAULT_LISTEN_PORT: NonZeroU16 = NonZeroU16::new(28884).unwrap();
const DEFAULT_WEB_WORKERS: NonZeroUsize = NonZeroUsize::new(4).unwrap();
const DEFAULT_HTML_TITLE: &str = "Atlas Faucet";
const DEFAULT_RPC_URL: &str = "http://127.0.0.1:8545";

/// Well-known Hardhat/Foundry/Anvil account #0, pre-funded with 10,000 ATL in
/// the Atlas dev genesis (`atlas-reth` `ARKIV_DEV_MNEMONIC`). Override with a
/// real key in any shared deployment.
const DEFAULT_FAUCET_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// 1 ATL (18 decimals) handed out per successful claim by default.
const DEFAULT_DRIP_WEI: &str = "1000000000000000000";

const DEFAULT_POW_BITS: u32 = 16;
const DEFAULT_POW_PUZZLES: u32 = 480;
const DEFAULT_POW_TTL_SECS: u64 = 180;
const DEFAULT_QUEUE_CAPACITY: NonZeroUsize = NonZeroUsize::new(2).unwrap();
const DEFAULT_COOLDOWN_SECS: u64 = 60;
const DEFAULT_GAS_LIMIT: u64 = 21_000;
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 30;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen_host")]
    pub listen_host: String,
    #[serde(default = "default_listen_port")]
    pub listen_port: NonZeroU16,
    #[serde(default = "default_web_workers")]
    pub web_workers: NonZeroUsize,
    #[serde(default = "default_html_title")]
    pub html_title: String,

    /// JSON-RPC endpoint of the Atlas execution node.
    #[serde(default = "default_rpc_url")]
    pub rpc_url: String,
    /// Optional explicit chain id. When unset it is discovered via
    /// `eth_chainId` at startup.
    #[serde(default)]
    pub chain_id: Option<u64>,

    /// secp256k1 private key (0x-prefixed) of the funded faucet account.
    #[serde(default = "default_faucet_private_key")]
    pub faucet_private_key: String,
    /// Amount handed out per successful claim, in wei (decimal string).
    #[serde(default = "default_drip_wei")]
    pub faucet_drip_wei: String,

    /// Leading zero *bits* required per proof-of-work sub-puzzle.
    #[serde(default = "default_pow_bits")]
    pub pow_bits: u32,
    /// Number of independent sub-puzzles the client must solve. Total work is
    /// roughly `pow_puzzles * 2^pow_bits` hashes; the count gives the browser a
    /// smooth, deterministic progress bar.
    #[serde(default = "default_pow_puzzles")]
    pub pow_puzzles: u32,
    /// Challenge lifetime in seconds.
    #[serde(default = "default_pow_ttl_secs")]
    pub pow_ttl_secs: u64,
    /// Optional fixed HMAC secret for signing challenges. A random per-process
    /// secret is generated when unset (challenges then do not survive restarts).
    #[serde(default)]
    pub pow_hmac_secret: Option<String>,

    /// Maximum number of claims allowed in the system at once (one is dispensed
    /// at a time; the remainder wait). Further claims receive `429`.
    #[serde(default = "default_queue_capacity")]
    pub faucet_queue_capacity: NonZeroUsize,
    /// Per-address cooldown in seconds between successful claims (0 disables).
    #[serde(default = "default_cooldown_secs")]
    pub faucet_cooldown_secs: u64,

    /// Gas limit used for the value-transfer transaction.
    #[serde(default = "default_gas_limit")]
    pub faucet_gas_limit: u64,
    /// Optional fixed gas price in wei. When unset, `eth_gasPrice` is queried.
    #[serde(default)]
    pub faucet_gas_price_wei: Option<String>,
    /// How long to wait for the dispense transaction to be mined before
    /// returning the (still pending) transaction hash.
    #[serde(default = "default_receipt_timeout_secs")]
    pub faucet_receipt_timeout_secs: u64,
}

pub fn create_config() -> Config {
    // Treat a set-but-empty variable (e.g. `CHAIN_ID=""` from a k8s manifest or
    // a blank `.env` line) as unset, so optional/defaulted fields fall back
    // instead of failing to parse "" as an integer.
    envy::from_iter::<_, Config>(std::env::vars().filter(|(_, value)| !value.trim().is_empty()))
        .unwrap_or_else(|err| panic!("invalid config: {err}"))
}

/// Largest sensible per-puzzle difficulty. The browser solver searches a u32
/// nonce space; above this a puzzle becomes effectively unsolvable and would
/// hang the visitor's browser, so we refuse it at startup.
const MAX_POW_BITS: u32 = 28;
const MAX_POW_PUZZLES: u32 = 100_000;

fn default_listen_host() -> String {
    DEFAULT_LISTEN_HOST.to_string()
}
fn default_listen_port() -> NonZeroU16 {
    DEFAULT_LISTEN_PORT
}
fn default_web_workers() -> NonZeroUsize {
    DEFAULT_WEB_WORKERS
}
fn default_html_title() -> String {
    DEFAULT_HTML_TITLE.to_string()
}
fn default_rpc_url() -> String {
    DEFAULT_RPC_URL.to_string()
}
fn default_faucet_private_key() -> String {
    DEFAULT_FAUCET_PRIVATE_KEY.to_string()
}
fn default_drip_wei() -> String {
    DEFAULT_DRIP_WEI.to_string()
}
fn default_pow_bits() -> u32 {
    DEFAULT_POW_BITS
}
fn default_pow_puzzles() -> u32 {
    DEFAULT_POW_PUZZLES
}
fn default_pow_ttl_secs() -> u64 {
    DEFAULT_POW_TTL_SECS
}
fn default_queue_capacity() -> NonZeroUsize {
    DEFAULT_QUEUE_CAPACITY
}
fn default_cooldown_secs() -> u64 {
    DEFAULT_COOLDOWN_SECS
}
fn default_gas_limit() -> u64 {
    DEFAULT_GAS_LIMIT
}
fn default_receipt_timeout_secs() -> u64 {
    DEFAULT_RECEIPT_TIMEOUT_SECS
}

impl Config {
    /// The faucet signing key, falling back to the built-in dev key when the
    /// variable is unset or blank (e.g. `FAUCET_PRIVATE_KEY=""` from Compose).
    pub fn private_key(&self) -> String {
        let trimmed = self.faucet_private_key.trim();
        if trimmed.is_empty() {
            DEFAULT_FAUCET_PRIVATE_KEY.to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Validate cross-field invariants that envy's per-field parsing cannot
    /// express. Called once at startup so misconfiguration fails fast.
    pub fn validate(&self) -> Result<(), String> {
        if self.pow_bits == 0 || self.pow_bits > MAX_POW_BITS {
            return Err(format!(
                "POW_BITS must be between 1 and {MAX_POW_BITS} (got {}); raise POW_PUZZLES for more total work instead",
                self.pow_bits
            ));
        }
        if self.pow_puzzles == 0 || self.pow_puzzles > MAX_POW_PUZZLES {
            return Err(format!(
                "POW_PUZZLES must be between 1 and {MAX_POW_PUZZLES} (got {})",
                self.pow_puzzles
            ));
        }
        self.drip_wei()?;
        self.gas_price_wei()?;
        Ok(())
    }

    /// Parse the configured drip amount into wei.
    pub fn drip_wei(&self) -> Result<u128, String> {
        self.faucet_drip_wei
            .trim()
            .parse::<u128>()
            .map_err(|error| format!("FAUCET_DRIP_WEI is not a valid wei amount: {error}"))
    }

    /// Parse the optional fixed gas price into wei.
    pub fn gas_price_wei(&self) -> Result<Option<u128>, String> {
        match self.faucet_gas_price_wei.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(value) => value
                .parse::<u128>()
                .map(Some)
                .map_err(|error| format!("FAUCET_GAS_PRICE_WEI is not a valid wei amount: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_pairs<const N: usize>(pairs: [(&str, &str); N]) -> Result<Config, envy::Error> {
        envy::from_iter(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        )
    }

    #[test]
    fn defaults_apply_when_env_is_empty() {
        let config = from_pairs([]).unwrap();
        assert_eq!(config.listen_host, DEFAULT_LISTEN_HOST);
        assert_eq!(config.listen_port, DEFAULT_LISTEN_PORT);
        assert_eq!(config.html_title, DEFAULT_HTML_TITLE);
        assert_eq!(config.rpc_url, DEFAULT_RPC_URL);
        assert_eq!(config.chain_id, None);
        assert_eq!(config.faucet_private_key, DEFAULT_FAUCET_PRIVATE_KEY);
        assert_eq!(config.drip_wei().unwrap(), 1_000_000_000_000_000_000);
        assert_eq!(config.pow_bits, DEFAULT_POW_BITS);
        assert_eq!(config.pow_puzzles, DEFAULT_POW_PUZZLES);
        assert_eq!(config.faucet_queue_capacity, DEFAULT_QUEUE_CAPACITY);
        assert_eq!(config.faucet_cooldown_secs, DEFAULT_COOLDOWN_SECS);
        assert_eq!(config.gas_price_wei().unwrap(), None);
    }

    #[test]
    fn parses_valid_overrides() {
        let config = from_pairs([
            ("LISTEN_PORT", "9000"),
            ("RPC_URL", "https://rpc.example.net"),
            ("CHAIN_ID", "1337"),
            ("FAUCET_DRIP_WEI", "250000000000000000"),
            ("POW_BITS", "18"),
            ("POW_PUZZLES", "1000"),
            ("FAUCET_QUEUE_CAPACITY", "3"),
            ("FAUCET_COOLDOWN_SECS", "0"),
            ("FAUCET_GAS_PRICE_WEI", "1000000000"),
        ])
        .unwrap();
        assert_eq!(config.listen_port.get(), 9000);
        assert_eq!(config.rpc_url, "https://rpc.example.net");
        assert_eq!(config.chain_id, Some(1337));
        assert_eq!(config.drip_wei().unwrap(), 250_000_000_000_000_000);
        assert_eq!(config.pow_bits, 18);
        assert_eq!(config.pow_puzzles, 1000);
        assert_eq!(config.faucet_queue_capacity.get(), 3);
        assert_eq!(config.faucet_cooldown_secs, 0);
        assert_eq!(config.gas_price_wei().unwrap(), Some(1_000_000_000));
    }

    #[test]
    fn rejects_zero_queue_capacity() {
        assert!(from_pairs([("FAUCET_QUEUE_CAPACITY", "0")]).is_err());
    }

    #[test]
    fn empty_numeric_env_is_treated_as_unset() {
        // Mirrors create_config()'s empty-value filtering: a blank CHAIN_ID must
        // not crash parsing, and should fall back to the auto-discovery default.
        let config = envy::from_iter::<_, Config>(
            [("CHAIN_ID", ""), ("LISTEN_PORT", ""), ("POW_BITS", "")]
                .into_iter()
                .filter(|(_, v): &(&str, &str)| !v.trim().is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string())),
        )
        .unwrap();
        assert_eq!(config.chain_id, None);
        assert_eq!(config.listen_port, DEFAULT_LISTEN_PORT);
        assert_eq!(config.pow_bits, DEFAULT_POW_BITS);
    }

    #[test]
    fn validate_enforces_pow_bounds() {
        let mut config = from_pairs([]).unwrap();
        assert!(config.validate().is_ok());

        config.pow_bits = 0;
        assert!(config.validate().is_err());
        config.pow_bits = 64;
        assert!(config.validate().is_err());
        config.pow_bits = 16;

        config.pow_puzzles = 0;
        assert!(config.validate().is_err());
        config.pow_puzzles = 480;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_drip() {
        let config = from_pairs([("FAUCET_DRIP_WEI", "not-a-number")]).unwrap();
        assert!(config.drip_wei().is_err());
    }
}
