use std::ffi::{OsStr, OsString};
use std::sync::Arc;
use std::time::Duration;

use atlas_faucet::config::create_config;
use atlas_faucet::eth::FaucetSigner;
use atlas_faucet::faucet::Faucet;
use atlas_faucet::pow::{self, PowKeeper};
use atlas_faucet::rpc::RpcClient;
use atlas_faucet::server;

fn main() {
    install_process_panic_handler();

    match startup_action(std::env::args_os().skip(1)) {
        StartupAction::Run => {}
        StartupAction::PrintVersion => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            return;
        }
        StartupAction::Error(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }

    let config = create_config();
    config
        .validate()
        .unwrap_or_else(|error| panic!("invalid config: {error}"));

    let signer = FaucetSigner::from_private_key_hex(&config.private_key())
        .unwrap_or_else(|error| panic!("invalid FAUCET_PRIVATE_KEY: {error}"));

    // The dev genesis funds the well-known Anvil account #0. Warn loudly if the
    // faucet is signing with that publicly-known key, since funds are not safe.
    const DEV_SIGNER_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    if signer.address_hex() == DEV_SIGNER_ADDRESS {
        eprintln!(
            "{}",
            serde_json::json!({
                "level": "warning",
                "message": "faucet is signing with the PUBLICLY KNOWN Atlas dev key; set FAUCET_PRIVATE_KEY for any shared deployment",
                "faucetAddress": DEV_SIGNER_ADDRESS,
            })
        );
    }
    let drip_wei = config
        .drip_wei()
        .unwrap_or_else(|error| panic!("{error}"));
    let gas_price_override = config
        .gas_price_wei()
        .unwrap_or_else(|error| panic!("{error}"));

    let secret = config
        .pow_hmac_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_else(pow::random_secret);
    let keeper = Arc::new(PowKeeper::new(secret));

    let rpc = RpcClient::new(config.rpc_url.clone());

    let worker_threads = config.web_workers.get();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_io()
        .enable_time()
        .build()
        .expect("failed to build tokio runtime");

    runtime.block_on(async move {
        let chain_id = match config.chain_id {
            Some(id) => id,
            None => match rpc.chain_id().await {
                Ok(id) => id,
                Err(error) => {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "message": "could not discover chain id from RPC; falling back to 1337. Set CHAIN_ID to override.",
                            "rpcUrl": rpc.url(),
                            "error": error,
                        })
                    );
                    1337
                }
            },
        };

        let faucet = Arc::new(Faucet::new(
            signer,
            rpc.clone(),
            chain_id,
            drip_wei,
            config.faucet_gas_limit,
            gas_price_override,
            Duration::from_secs(config.faucet_receipt_timeout_secs),
            config.faucet_queue_capacity.get(),
        ));

        println!(
            "{}",
            serde_json::json!({
                "message": "atlas faucet configured",
                "faucetAddress": faucet.faucet_address_hex(),
                "rpcUrl": rpc.url(),
                "chainId": chain_id,
                "dripWei": drip_wei.to_string(),
                "powBits": config.pow_bits,
                "powPuzzles": config.pow_puzzles,
                "queueCapacity": config.faucet_queue_capacity.get(),
                "cooldownSecs": config.faucet_cooldown_secs,
            })
        );

        let state = server::AppState {
            faucet,
            keeper,
            html_title: Arc::new(config.html_title.clone()),
            pow_bits: config.pow_bits,
            pow_puzzles: config.pow_puzzles,
            pow_ttl_secs: config.pow_ttl_secs,
            cooldown_secs: config.faucet_cooldown_secs,
        };

        server::run_server(state, config.listen_host.clone(), config.listen_port.get()).await;
    });
}

fn install_process_panic_handler() {
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("uncaught panic: {panic_info}");
    }));
}

#[derive(Debug, PartialEq, Eq)]
enum StartupAction {
    Run,
    PrintVersion,
    Error(String),
}

fn startup_action<I>(args: I) -> StartupAction
where
    I: IntoIterator<Item = OsString>,
{
    let mut saw_version = false;

    for arg in args {
        if arg == OsStr::new("-v") || arg == OsStr::new("--version") {
            saw_version = true;
            continue;
        }

        return StartupAction::Error(format!(
            "unsupported command-line argument: {}. Use environment variables to configure atlas-faucet; command-line arguments are not supported.",
            arg.to_string_lossy()
        ));
    }

    if saw_version {
        StartupAction::PrintVersion
    } else {
        StartupAction::Run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_runs_service() {
        assert_eq!(startup_action(args(&[])), StartupAction::Run);
    }

    #[test]
    fn version_arguments_print_version() {
        assert_eq!(startup_action(args(&["-v"])), StartupAction::PrintVersion);
        assert_eq!(
            startup_action(args(&["--version"])),
            StartupAction::PrintVersion
        );
    }

    #[test]
    fn invalid_argument_returns_error() {
        match startup_action(args(&["--rpc-url", "http://localhost"])) {
            StartupAction::Error(message) => {
                assert!(message.contains("--rpc-url"));
                assert!(message.contains("environment variables"));
            }
            action => panic!("expected error action, got {action:?}"),
        }
    }
}
