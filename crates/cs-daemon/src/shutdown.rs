//! Graceful shutdown (R1.3).
//!
//! Owned by unit `daemon-proc`. [`Signals`] waits for the first of:
//! - Unix: `SIGINT` (Ctrl+C), `SIGTERM` and `SIGHUP` (terminal closed), through
//!   `tokio::signal::unix::signal(SignalKind::interrupt() / terminate() /
//!   hangup())` (tokio 1.53.1 `src/signal/unix.rs`). `tokio::signal::ctrl_c` is the
//!   same `SIGINT` listener on Unix, but it registers only when first polled; these
//!   register when [`Signals::install`] runs.
//! - Windows: `tokio::signal::windows::ctrl_c`, `ctrl_break` and `ctrl_close`
//!   (console closed; `src/signal/windows.rs`). After a close event Windows ends
//!   the process when the handler returns or after `SPI_GETHUNGAPPTIMEOUT`, 5 s by
//!   default (Microsoft Learn, "HandlerRoutine callback function", Timeouts).
//!   tokio's handler waits (`src/signal/windows/sys.rs`), so the whole shutdown
//!   gets those 5 s: connections are given [`CLOSE_EVENT_DRAIN_TIMEOUT`] instead
//!   of `crate::serve::DRAIN_TIMEOUT`.
//!
//! [`graceful`] then runs, in this order:
//! 1. the server stops accepting and lets open connections finish, within
//!    [`Signal::drain_timeout`], aborting the rest;
//! 2. the caller's [`DrainHook`] runs: the store's writer is drained and closed
//!    (unit `writer`, wired in by unit `wire`);
//! 3. `daemon.json` is removed and `daemon.lock` released
//!    ([`crate::instance::Instance::close`]);
//! 4. `main` exits with status 0, or 1 if a step failed.
//!
//! Every step runs even when an earlier one failed. A second signal during the
//! shutdown makes `main` stop waiting and exit 1 at once: the lock is released
//! with the process, and the next start removes the `daemon.json` left behind.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use crate::instance::Instance;
use crate::serve::{DRAIN_TIMEOUT, Server};

/// Drains pending work before the daemon exits (step 2 in the module docs).
pub type DrainHook = Box<dyn FnOnce() -> DrainFuture + Send>;

/// What a [`DrainHook`] returns.
pub type DrainFuture =
    Pin<Box<dyn Future<Output = Result<(), Box<dyn Error + Send + Sync>>> + Send>>;

/// A [`DrainHook`] with nothing to drain.
pub fn nothing_to_drain() -> DrainHook {
    Box::new(|| Box::pin(async { Ok(()) }))
}

/// How long connections get after a Windows console close event, leaving the
/// rest of the 5 s Windows allows to the drain hook and the cleanup.
pub const CLOSE_EVENT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// A signal that shuts the daemon down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Unix `SIGINT`.
    Interrupt,
    /// Unix `SIGTERM`.
    Terminate,
    /// Unix `SIGHUP`.
    Hangup,
    /// Windows `CTRL_C_EVENT`.
    CtrlC,
    /// Windows `CTRL_BREAK_EVENT`.
    CtrlBreak,
    /// Windows `CTRL_CLOSE_EVENT`.
    CtrlClose,
}

impl Signal {
    /// The signal's usual name, for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Hangup => "SIGHUP",
            Self::CtrlC => "CTRL_C_EVENT",
            Self::CtrlBreak => "CTRL_BREAK_EVENT",
            Self::CtrlClose => "CTRL_CLOSE_EVENT",
        }
    }

    /// How long open connections get to finish after this signal.
    pub fn drain_timeout(self) -> Duration {
        match self {
            Self::CtrlClose => CLOSE_EVENT_DRAIN_TIMEOUT,
            _ => DRAIN_TIMEOUT,
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The shutdown signals, registered with the operating system.
#[derive(Debug)]
pub struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hangup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl Signals {
    /// Registers the listeners. From here on these signals no longer end the
    /// process at once; [`Signals::recv`] reports them. Needs a tokio runtime.
    pub fn install() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
                hangup: signal(SignalKind::hangup())?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
            Ok(Self {
                ctrl_c: ctrl_c()?,
                ctrl_break: ctrl_break()?,
                ctrl_close: ctrl_close()?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "shutdown signals are not implemented on this platform",
            ))
        }
    }

    /// Waits for the next signal.
    pub async fn recv(&mut self) -> Signal {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => Signal::Interrupt,
                _ = self.terminate.recv() => Signal::Terminate,
                _ = self.hangup.recv() => Signal::Hangup,
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => Signal::CtrlC,
                _ = self.ctrl_break.recv() => Signal::CtrlBreak,
                _ = self.ctrl_close.recv() => Signal::CtrlClose,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            std::future::pending().await
        }
    }
}

/// A step of [`graceful`] that failed; the later steps still ran.
#[derive(Debug)]
pub enum ShutdownError {
    /// The drain hook failed: pending writes may be lost.
    Drain(Box<dyn Error + Send + Sync>),
    /// Removing `daemon.json` or releasing the lock failed.
    Instance(crate::instance::InstanceError),
}

impl fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drain(source) => write!(f, "draining pending work failed: {source}"),
            Self::Instance(source) => source.fmt(f),
        }
    }
}

impl Error for ShutdownError {}

/// Runs the shutdown steps in order (see the module docs), giving connections
/// `drain_timeout`. Returns the first failure, after running every step.
pub async fn graceful(
    server: Server,
    drain_timeout: Duration,
    drain: DrainHook,
    instance: Instance,
) -> Result<(), ShutdownError> {
    server.stop_within(drain_timeout).await;
    tracing::info!("control API stopped");

    let drained = drain().await.map_err(ShutdownError::Drain);
    if let Err(error) = &drained {
        tracing::error!(%error, "shutdown: drain failed");
    }

    let closed = instance.close().map_err(ShutdownError::Instance);
    if let Err(error) = &closed {
        tracing::error!(%error, "shutdown: cannot clean up the data directory");
    }
    drained.and(closed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_event_leaves_time_for_the_rest_of_the_shutdown() {
        assert!(Signal::CtrlClose.drain_timeout() < Duration::from_secs(5));
        assert_eq!(Signal::Terminate.drain_timeout(), DRAIN_TIMEOUT);
    }
}
