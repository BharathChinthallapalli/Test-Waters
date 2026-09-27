//! Callsheet daemon: the only binary in the workspace.
//!
//! Startup, in order: command line (exit 2 for a bad one, before any file or
//! socket exists) → logging → data directory → instance lock → token → signal
//! handlers → store (opened and migrated on a blocking thread) → migration
//! backup check → listener on `127.0.0.1` → server with every control-API
//! method (`cs_daemon::methods::Methods`) → `daemon.json` → wait for a signal →
//! graceful shutdown (`cs_daemon::shutdown`), whose drain step closes the store
//! after its writer has carried out every queued write. A startup failure prints
//! one line naming the file or value involved, never a secret, and exits 1.
//!
//! A signal while the store opens or the backup check runs stops the daemon
//! before it listens (exit 0). At exit, blocking tasks still running (a keychain
//! call waiting on an unlock prompt) get [`BLOCKING_TASKS_GRACE`] and are then
//! abandoned, so the process always ends.
//!
//! **The store** uses the OS keychain (`cs_store::secrets::os_keychain`), which is
//! contacted only when content capture needs the key, never at startup.
//!
//! **Migration backups** (feature 02 design, "Store", Migrations). A backup
//! (`backup-v*.db`) exists only to recover from a migration that went wrong, so it
//! is deleted once the migrated database has passed `PRAGMA integrity_check` and a
//! full verify. The check runs whenever a backup is present at startup: after
//! this start's migration made one, or when an earlier start failed before
//! deleting its own, so the next successful startup deletes it. Outcomes:
//! - both pass: every backup is deleted before the listener opens (a failed
//!   deletion is logged and retried at the next start; erasure also removes
//!   backups);
//! - `integrity_check` reports a problem, or either check can't run: the daemon
//!   **refuses to start** and keeps the backups. The database is damaged or
//!   unreadable, and the backup is what the user recovers from;
//! - the verify runs and finds a problem in the event log: the daemon **starts
//!   with a warning** and keeps the backups. The database is sound, and the
//!   problem is tamper evidence the user inspects through `events.verify`, which
//!   needs a running daemon; the backup holds the log as it was before migrating,
//!   for comparison. The check, and the warning, repeat at every start until
//!   the log verifies or an erasure removes the backups. Deliberately: the
//!   warning stays visible for as long as the problem exists, and a full verify
//!   only costs startup time while a backup is present.

use std::error::Error;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_daemon::config::{self, Command, Config, USAGE, USAGE_EXIT_CODE};
use cs_daemon::http::{self, HttpConfig};
use cs_daemon::instance::{self, Instance};
use cs_daemon::logging;
use cs_daemon::methods::Methods;
use cs_daemon::serve::{self, ServeConfig, Server};
use cs_daemon::shutdown::{self, DrainHook, Signals};
use cs_daemon::token::ControlToken;
use cs_store::migrate::{count_migration_backups, remove_migration_backups};
use cs_store::{Store, secrets};
use tokio::net::TcpListener;

fn main() -> ExitCode {
    let started = Instant::now();
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
    let (config, instance, token) = match prepare(config) {
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
    let result = runtime.block_on(run(config, instance, token, started));
    // Dropping a runtime waits for every `spawn_blocking` task, with no limit. A
    // keychain call can sit in one for as long as an unlock prompt stays open
    // (`cs_store::secrets::SecretStore::content_key`), and would then keep the
    // process alive after a finished shutdown, or after the forced exit on a
    // repeated signal. The shutdown steps are done by now, so give such tasks a
    // moment and then abandon them.
    runtime.shutdown_timeout(BLOCKING_TASKS_GRACE);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.as_ref(), ExitCode::FAILURE),
    }
}

/// How long blocking tasks still running at exit get before they are abandoned.
const BLOCKING_TASKS_GRACE: Duration = Duration::from_secs(1);

/// After this long, a shutdown still waiting for the store's writer logs that
/// it is still waiting.
const SLOW_DRAIN: Duration = Duration::from_secs(5);

/// Prints a one-line error to stderr and returns `code`.
fn fail(error: &dyn Error, code: ExitCode) -> ExitCode {
    eprintln!("cs-daemon: {error}");
    code
}

/// The data directory, the instance lock and the token: everything before the
/// runtime starts. The returned config holds the data directory as resolved by
/// `prepare_data_dir`, used for everything after.
fn prepare(mut config: Config) -> Result<(Config, Instance, Arc<ControlToken>), Box<dyn Error>> {
    config.data_dir = instance::prepare_data_dir(&config.data_dir)?;
    let instance = Instance::acquire(&config.data_dir)?;
    let token = ControlToken::load_or_create(&config.data_dir)?;
    Ok((config, instance, Arc::new(token)))
}

async fn run(
    config: Config,
    instance: Instance,
    token: Arc<ControlToken>,
    started: Instant,
) -> Result<(), Box<dyn Error>> {
    // Before `daemon.json` exists, so a signal sent as soon as a client can see the
    // daemon already shuts it down gracefully.
    let mut signals =
        Signals::install().map_err(|error| format!("cannot handle shutdown signals: {error}"))?;
    // Opening (and migrating) the store and checking a migration backup can take
    // a while; a signal meanwhile stops the daemon without starting it. Work
    // left on a blocking thread is abandoned at exit (`BLOCKING_TASKS_GRACE`):
    // each migration is its own transaction, and a half-written backup is
    // checked and deleted by the next start.
    let opening = async {
        let store = open_store(&config.data_dir).await?;
        if let Err(error) = check_migration_backups(&store, &config.data_dir).await {
            let _ = store.close().await;
            return Err(error);
        }
        Ok::<_, Box<dyn Error>>(store)
    };
    let store = tokio::select! {
        opened = opening => match opened {
            Ok(store) => store,
            Err(error) => {
                // The error that matters is the startup failure.
                let _ = instance.close();
                return Err(error);
            }
        },
        signal = signals.recv() => {
            tracing::info!(signal = signal.name(), "stopped while starting");
            // The store may still be opening or migrating on a blocking thread
            // that is only abandoned, not stopped, so `daemon.lock` must stay
            // held until the process ends: unlocking now would let a restarted
            // daemon migrate the same database alongside it. Leaking the handle
            // leaves the lock to the operating system, which releases it at
            // exit. `daemon.json` was never written by this process.
            std::mem::forget(instance);
            return Ok(());
        }
    };
    let server = match start(&config, &instance, token, Arc::clone(&store), started).await {
        Ok(server) => server,
        Err(error) => {
            // The error that matters is the startup failure.
            let _ = store.close().await;
            let _ = instance.close();
            return Err(error);
        }
    };

    let signal = signals.recv().await;
    tracing::info!(signal = signal.name(), "shutting down");
    let stopping = shutdown::graceful(server, signal.drain_timeout(), close_store(store), instance);
    tokio::pin!(stopping);
    let mut received = 1;
    loop {
        tokio::select! {
            stopped = &mut stopping => {
                stopped?;
                break;
            }
            again = signals.recv() => {
                received += 1;
                if received >= shutdown::SIGNALS_TO_FORCE_EXIT {
                    return Err(format!(
                        "{again} received {received} times; exiting without finishing the shutdown"
                    )
                    .into());
                }
                tracing::warn!(
                    signal = again.name(),
                    "already shutting down; the next signal exits without finishing"
                );
            }
        }
    }
    tracing::info!("Callsheet daemon stopped");
    Ok(())
}

/// Opens and migrates the store on a blocking thread (`Store::open` blocks on
/// file I/O).
async fn open_store(data_dir: &Path) -> Result<Arc<Store>, Box<dyn Error>> {
    let dir = data_dir.to_path_buf();
    let opened =
        tokio::task::spawn_blocking(move || Store::open(&dir, Arc::new(secrets::os_keychain())))
            .await
            .map_err(|_| "opening the store did not finish")?;
    let store = opened.map_err(|error| format!("cannot open the store: {error}"))?;
    let migrated = store.migrated();
    if migrated.from != migrated.to {
        tracing::info!(
            from = migrated.from,
            to = migrated.to,
            "database schema migrated"
        );
    }
    Ok(Arc::new(store))
}

/// The listener, the server and `daemon.json`. On failure nothing is left
/// serving; the caller closes the store and the instance.
async fn start(
    config: &Config,
    instance: &Instance,
    token: Arc<ControlToken>,
    store: Arc<Store>,
    started: Instant,
) -> Result<Server, Box<dyn Error>> {
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
    let methods = Methods::new(store, token.clone(), started);
    let router = http::router(http_config, token, Arc::new(methods));
    let server = serve::spawn(listener, router, ServeConfig::default());

    let discovery = match instance.publish(address) {
        Ok(discovery) => discovery,
        Err(error) => {
            server.stop().await;
            return Err(error.into());
        }
    };
    tracing::info!(
        address = %discovery.address,
        pid = discovery.pid,
        data_dir = %config.data_dir.display(),
        "Callsheet daemon listening"
    );
    Ok(server)
}

/// Deletes the migration backups once the database has passed
/// `PRAGMA integrity_check` and a full verify; see the module docs for what
/// happens otherwise.
async fn check_migration_backups(store: &Store, data_dir: &Path) -> Result<(), Box<dyn Error>> {
    let present = store.migrated().backup.is_some() || {
        let dir = data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || count_migration_backups(&dir))
            .await
            .map_err(|_| "listing the migration backups did not finish")?
            .map_err(|error| {
                format!(
                    "cannot list migration backups in {}: {error}",
                    data_dir.display()
                )
            })?
            > 0
    };
    if !present {
        return Ok(());
    }
    let kept = |why: String| -> Box<dyn Error> {
        format!(
            "{why}; the migration backups (backup-v*.db) in {} are kept for recovery",
            data_dir.display()
        )
        .into()
    };
    tracing::info!("migration backup present: checking the database before deleting it");

    // `PRAGMA integrity_check` returns the single row `ok` for a sound database,
    // otherwise its problems. They name tables, indexes and pages, never row
    // contents.
    let problems = store
        .read(|conn| {
            let mut problems = Vec::new();
            conn.pragma_query(None, "integrity_check", |row| {
                let row: String = row.get(0)?;
                if row != "ok" {
                    problems.push(row);
                }
                Ok(())
            })?;
            Ok(problems)
        })
        .await
        .map_err(|error| kept(format!("cannot run PRAGMA integrity_check: {error}")))?;
    if let Some(first) = problems.first() {
        return Err(kept(format!(
            "the database failed PRAGMA integrity_check ({} problems, first: {first})",
            problems.len()
        )));
    }

    let verified = cs_store::verify::verify(store)
        .await
        .map_err(|error| kept(format!("cannot verify the event log: {error}")))?;
    if let Some(problem) = verified.first_problem {
        tracing::warn!(
            kind = ?problem.kind,
            global_pos = problem.global_pos,
            data_dir = %data_dir.display(),
            "the event log does not verify: the migration backups (backup-v*.db) are kept \
             for comparison and the check runs again at the next start; see events.verify"
        );
        return Ok(());
    }

    let data_dir = data_dir.to_path_buf();
    let removed = tokio::task::spawn_blocking(move || remove_migration_backups(&data_dir)).await;
    match removed {
        Ok(Ok(removed)) => {
            tracing::info!(removed, "database checked; migration backups deleted");
        }
        // Not fatal: the next start tries again, and erasure removes backups too.
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot delete the migration backups; retrying at the next start");
        }
        Err(_) => tracing::warn!("deleting the migration backups did not finish"),
    }
    Ok(())
}

/// The drain step of the shutdown: closes the store once its writer has
/// carried out every queued write, logging if that takes longer than
/// [`SLOW_DRAIN`]. `Store::close` may be called again while a call waits.
fn close_store(store: Arc<Store>) -> DrainHook {
    Box::new(move || {
        Box::pin(async move {
            let closed = match tokio::time::timeout(SLOW_DRAIN, store.close()).await {
                Ok(closed) => closed,
                Err(_) => {
                    tracing::warn!(
                        waited_ms = u64::try_from(SLOW_DRAIN.as_millis()).unwrap_or(u64::MAX),
                        "shutdown: still waiting for the store to finish its queued writes"
                    );
                    store.close().await
                }
            };
            closed.map_err(|error| Box::new(error) as Box<dyn Error + Send + Sync>)
        })
    })
}
