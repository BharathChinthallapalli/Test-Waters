//! Callsheet daemon: the only binary in the workspace.
//!
//! Startup, in order: command line (exit 2 for a bad one, before any file or
//! socket exists) → logging → data directory → instance lock → token → signal
//! handlers → listener on `127.0.0.1` → server → `daemon.json` → wait for a signal
//! → graceful shutdown (`cs_daemon::shutdown`). A startup failure prints one line
//! naming the file or value involved, never a secret, and exits 1.

use std::error::Error;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use cs_core::control::{VersionResult, methods};
use cs_daemon::config::{self, Command, Config, USAGE, USAGE_EXIT_CODE};
use cs_daemon::http::{self, HttpConfig};
use cs_daemon::instance::{self, Instance};
use cs_daemon::logging;
use cs_daemon::rpc::{Handler, RpcError};
use cs_daemon::serve::{self, ServeConfig};
use cs_daemon::shutdown::{self, Signals};
use cs_daemon::token::ControlToken;
use serde_json::Value;
use tokio::net::TcpListener;

fn main() -> ExitCode {
    let config = match config::parse_args(std::env::args_os().skip(1)) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => return fail(&error, ExitCode::from(USAGE_EXIT_CODE)),
    };
    let level = match logging::level_from_env() {
        Ok(level) => level,
        Err(error) => return fail(&error, ExitCode::from(USAGE_EXIT_CODE)),
    };
    if let Err(error) = logging::init(level) {
        return fail(&error, ExitCode::FAILURE);
    }
    let (instance, token) = match prepare(&config) {
        Ok(prepared) => prepared,
        Err(error) => return fail(error.as_ref(), ExitCode::FAILURE),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&error, ExitCode::FAILURE),
    };
    match runtime.block_on(run(config, instance, token)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.as_ref(), ExitCode::FAILURE),
    }
}

/// Prints a one-line error to stderr and returns `code`.
fn fail(error: &dyn Error, code: ExitCode) -> ExitCode {
    eprintln!("cs-daemon: {error}");
    code
}

/// The data directory, the instance lock and the token: everything before the
/// runtime starts.
fn prepare(config: &Config) -> Result<(Instance, Arc<ControlToken>), Box<dyn Error>> {
    instance::prepare_data_dir(&config.data_dir)?;
    let instance = Instance::acquire(&config.data_dir)?;
    let token = ControlToken::load_or_create(&config.data_dir)?;
    Ok((instance, Arc::new(token)))
}

async fn run(
    config: Config,
    instance: Instance,
    token: Arc<ControlToken>,
) -> Result<(), Box<dyn Error>> {
    // Before `daemon.json` exists, so a signal sent as soon as a client can see the
    // daemon already shuts it down gracefully.
    let mut signals =
        Signals::install().map_err(|error| format!("cannot handle shutdown signals: {error}"))?;
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|error| format!("cannot listen on {}: {error}", config.listen))?;
    let SocketAddr::V4(address) = listener
        .local_addr()
        .map_err(|error| format!("cannot read the bound address: {error}"))?
    else {
        return Err("the listener is not bound to an IPv4 address".into());
    };
    // The bound port, never the configured one (which may be 0).
    let http_config = HttpConfig::new(address.port());
    let router = http::router(http_config, token, Arc::new(BootstrapHandler));
    let server = serve::spawn(listener, router, ServeConfig::default());

    let discovery = match instance.publish(address) {
        Ok(discovery) => discovery,
        Err(error) => {
            server.stop().await;
            // The error that matters is the publish failure.
            let _ = instance.close();
            return Err(error.into());
        }
    };
    tracing::info!(
        address = %discovery.address,
        pid = discovery.pid,
        data_dir = %config.data_dir.display(),
        "Callsheet daemon listening"
    );

    let signal = signals.recv().await;
    tracing::info!(signal = signal.name(), "shutting down");
    // Unit `wire` (task 11) passes the store writer's drain here.
    let drain = shutdown::nothing_to_drain();
    tokio::select! {
        stopped = shutdown::graceful(server, signal.drain_timeout(), drain, instance) => stopped?,
        second = signals.recv() => {
            return Err(format!("{second} during shutdown; exiting without finishing it").into());
        }
    }
    tracing::info!("Callsheet daemon stopped");
    Ok(())
}

/// Answers `version` and nothing else.
///
/// Temporary: unit `wire` (task 11) replaces it with the full method table in
/// `cs_daemon::methods`, backed by the store.
struct BootstrapHandler;

impl Handler for BootstrapHandler {
    async fn call(&self, method: &str, _params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            methods::VERSION => serde_json::to_value(VersionResult {
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            })
            .map_err(|_| RpcError::internal_error()),
            _ => Err(RpcError::method_not_found()),
        }
    }
}
