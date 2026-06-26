//! Atlas faucet library: configuration, proof-of-work, Ethereum signing/RPC,
//! the dispenser, and the HTTP server. The `atlas-faucet` binary
//! (`src/main.rs`) is a thin wrapper around these modules; integration tests
//! consume them directly.

pub mod config;
pub mod eth;
pub mod faucet;
pub mod frontend;
pub mod pow;
pub mod rpc;
pub mod server;
pub mod util;
