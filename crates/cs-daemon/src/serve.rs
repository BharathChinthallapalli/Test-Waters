//! Serving the control API's router, and the model-call proxy, over TCP, with a
//! header-read timeout and graceful shutdown.
//!
//! Owned by unit `daemon-proc`; the proxy listener by feature 03, task 8
//! (`wire`). `axum::serve` isn't used: in axum 0.8.9 it builds a
//! `hyper_util::server::conn::auto::Builder` for every connection without a timer
//! and offers no way to configure it (`axum/src/serve/mod.rs`, `handle_connection`),
//! so hyper's header-read timeout never fires (`crate::http` module docs). This
//! module is the same accept loop with a configured connection builder, shared by
//! both listeners ([`spawn`] for the control API, [`spawn_proxy`] for the proxy):
//!
//! - **HTTP/1.1 only**, `hyper::server::conn::http1::Builder` (hyper 1.11.1
//!   `src/server/conn/http1.rs`); axum is built with only its `http1` feature
//!   anyway.
//! - **Header-read timeout**, [`HEADER_READ_TIMEOUT`] (10 s): the builder gets
//!   `.timer(hyper_util::rt::TokioTimer::new())` and `.header_read_timeout(..)`.
//!   hyper starts the deadline each time it begins reading a request head, idle
//!   keep-alive connections included, and closes the connection without a
//!   response when it passes (`src/proto/h1/conn.rs`, `poll_read_head`;
//!   `src/proto/h1/role.rs`, `on_error` sends nothing for a timeout). After the
//!   head, the control API's router bounds the body and the dispatch with
//!   `crate::http::REQUEST_TIMEOUT`. The proxy has **no** request timeout: a
//!   model response streams for as long as the upstream sends.
//! - **`half_close(false)`** (hyper's default, set explicitly): a client that
//!   shuts down its write side is gone, so the connection closes and a proxy
//!   call in flight is dropped, rather than kept half-open.
//! - **`TCP_NODELAY`** on every accepted socket (`TcpStream::set_nodelay`), so
//!   the small SSE chunks the proxy relays aren't held back by Nagle's
//!   algorithm. The control API sends one small response per request, so it
//!   gets the same.
//! - **Peer address** (control API): each request gets the extension
//!   `axum::extract::ConnectInfo<SocketAddr>`, which `crate::http` reads to name the
//!   peer in auth-failure logs, as `into_make_service_with_connect_info` would.
//! - The router becomes a hyper service with
//!   `hyper_util::service::TowerToHyperService` (hyper-util 0.1.21
//!   `src/service/glue.rs`); `axum::Router` is a tower service for any request
//!   body with `Bytes` data, `hyper::body::Incoming` included
//!   (`axum/src/routing/mod.rs`). The proxy is `cs_proxy::Proxy::handle` in a
//!   `hyper::service::service_fn`.
//! - **Connection errors** are logged at debug level with the error only, never
//!   request contents. A proxy response whose upstream body failed part-way is
//!   aborted on purpose (`cs_proxy::UpstreamBodyFailed`): hyper's
//!   `serve_connection` then returns an error, which is expected and logged the
//!   same way.
//! - **Graceful shutdown** with `hyper_util::server::graceful::GracefulShutdown`
//!   (`src/server/graceful.rs`): when the shutdown future completes the listener is
//!   dropped, so new connections are refused; every open connection is told to
//!   finish (HTTP/1: keep-alive off, so an idle one closes and a busy one closes
//!   after its response); then the loop waits at most [`DRAIN_TIMEOUT`] (or the
//!   timeout given to [`Server::stop_within`]) for them. Connection tasks still
//!   running after that are aborted, dropping their handler at its current
//!   `.await` (the cancellation contract of `crate::rpc::Handler`; a proxy call
//!   dropped this way is recorded as `clientCancelled`), and awaited, so no
//!   request handler runs once the server has stopped.
//! - Accept errors are handled like `axum::serve` (`axum/src/serve/listener.rs`,
//!   `handle_accept_error`): per-connection errors are skipped, anything else
//!   (for example too many open files) is logged and retried after a second.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use cs_proxy::{Proxy, ProxyBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

/// A client that hasn't sent a complete request head after this long is
/// disconnected.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long shutdown waits for open connections by default: a request already
/// being dispatched when shutdown starts is answered, or gets its 408 after
/// `crate::http::REQUEST_TIMEOUT` (10 s), within it. Proxy calls still streaming
/// after it are cut off.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(11);

/// How long to wait after an accept error that isn't about one connection.
const ACCEPT_ERROR_DELAY: Duration = Duration::from_secs(1);

/// The name of the control API's server, in logs.
pub const CONTROL_API: &str = "control API";

/// The name of the proxy's server, in logs.
pub const PROXY: &str = "proxy";

/// Timeouts of the server; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeConfig {
    /// [`HEADER_READ_TIMEOUT`] in the daemon.
    pub header_read_timeout: Duration,
    /// [`DRAIN_TIMEOUT`] in the daemon; [`Server::stop_within`] overrides it.
    pub drain_timeout: Duration,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            header_read_timeout: HEADER_READ_TIMEOUT,
            drain_timeout: DRAIN_TIMEOUT,
        }
    }
}

/// A server running on its own task.
#[derive(Debug)]
pub struct Server {
    name: &'static str,
    stop: oneshot::Sender<Duration>,
    drain_timeout: Duration,
    task: JoinHandle<()>,
}

impl Server {
    /// [`Server::stop_within`] the configured drain timeout.
    pub async fn stop(self) {
        let drain_timeout = self.drain_timeout;
        self.stop_within(drain_timeout).await;
    }

    /// Stops accepting, lets open connections finish for at most `drain_timeout`,
    /// aborts the rest, and returns when the server and every connection task have
    /// ended.
    pub async fn stop_within(self, drain_timeout: Duration) {
        // The task may already be gone (it panicked); then there's nothing to stop.
        let _ = self.stop.send(drain_timeout);
        if let Err(error) = self.task.await {
            tracing::error!(server = self.name, %error, "server task failed");
        }
    }

    /// [`CONTROL_API`] or [`PROXY`].
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// Serves the control API's `router` on `listener` on a new task until
/// [`Server::stop`].
pub fn spawn(listener: TcpListener, router: Router, config: ServeConfig) -> Server {
    spawn_named(CONTROL_API, config, |shutdown| {
        serve(listener, router, config, shutdown)
    })
}

/// Serves `proxy` on `listener` on a new task until [`Server::stop`]: the same
/// accept loop as the control API, without a request timeout.
pub fn spawn_proxy(listener: TcpListener, proxy: Proxy, config: ServeConfig) -> Server {
    spawn_named(PROXY, config, |shutdown| {
        accept_loop(PROXY, listener, config, shutdown, move |_peer| {
            let proxy = proxy.clone();
            service_fn(move |request: hyper::Request<Incoming>| {
                let proxy = proxy.clone();
                async move { Ok::<hyper::Response<ProxyBody>, Infallible>(proxy.handle(request).await) }
            })
        })
    })
}

fn spawn_named<L, Fut>(name: &'static str, config: ServeConfig, serve_until: L) -> Server
where
    L: FnOnce(std::pin::Pin<Box<dyn Future<Output = Duration> + Send>>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (stop, stopped) = oneshot::channel::<Duration>();
    let drain_timeout = config.drain_timeout;
    let task = tokio::spawn(serve_until(Box::pin(async move {
        // A dropped sender stops the server too, with the default timeout.
        stopped.await.unwrap_or(drain_timeout)
    })));
    Server {
        name,
        stop,
        drain_timeout,
        task,
    }
}

/// Serves `router` on `listener` until `shutdown` completes, then shuts down
/// gracefully, waiting for open connections at most as long as `shutdown`
/// returns (see the module docs). Returns once every connection task has ended.
pub async fn serve<F>(listener: TcpListener, router: Router, config: ServeConfig, shutdown: F)
where
    F: Future<Output = Duration>,
{
    accept_loop(CONTROL_API, listener, config, shutdown, |peer| {
        with_peer(&router, peer)
    })
    .await;
}

/// The accept loop both servers share (see the module docs). `make_service`
/// builds the service for one connection from its peer's address.
async fn accept_loop<F, M, S, B>(
    name: &'static str,
    listener: TcpListener,
    config: ServeConfig,
    shutdown: F,
    make_service: M,
) where
    F: Future<Output = Duration>,
    M: Fn(SocketAddr) -> S,
    S: hyper::service::Service<
            hyper::Request<Incoming>,
            Response = hyper::Response<B>,
            Error = Infallible,
        > + Send
        + 'static,
    S::Future: Send + 'static,
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(config.header_read_timeout)
        .half_close(false);
    let graceful = GracefulShutdown::new();
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);

    let drain_timeout = loop {
        let (stream, peer) = tokio::select! {
            drain_timeout = &mut shutdown => break drain_timeout,
            // Reap finished connection tasks so the set doesn't grow.
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => match accepted {
                Ok(connection) => connection,
                Err(error) if is_connection_error(&error) => continue,
                Err(error) => {
                    tracing::error!(server = name, %error, "cannot accept connections");
                    tokio::select! {
                        drain_timeout = &mut shutdown => break drain_timeout,
                        () = tokio::time::sleep(ACCEPT_ERROR_DELAY) => continue,
                    }
                }
            },
        };
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(server = name, %peer, %error, "cannot set TCP_NODELAY");
        }
        let connection = builder.serve_connection(TokioIo::new(stream), make_service(peer));
        let connection = graceful.watch(connection);
        connections.spawn(async move {
            if let Err(error) = connection.await {
                // hyper's messages name the failure (for example "read header from
                // client timeout", or a proxy response aborted because its
                // upstream failed), never request contents.
                tracing::debug!(server = name, %peer, %error, "connection ended with an error");
            }
        });
    };

    drop(listener);
    tracing::info!(
        server = name,
        open_connections = connections.len(),
        "stopped accepting connections"
    );
    if tokio::time::timeout(drain_timeout, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!(
            server = name,
            timeout_ms = u64::try_from(drain_timeout.as_millis()).unwrap_or(u64::MAX),
            open_connections = connections.len(),
            "connections still open after the drain timeout; aborting them"
        );
        connections.abort_all();
    }
    // Wait for aborted tasks too, so no handler runs after this returns.
    while connections.join_next().await.is_some() {}
}

/// The router as a hyper service for one connection, adding the peer's address to
/// every request.
fn with_peer(
    router: &Router,
    peer: SocketAddr,
) -> impl hyper::service::Service<
    hyper::Request<Incoming>,
    Response = axum::response::Response,
    Error = Infallible,
    Future: Send + 'static,
> + Clone
+ Send
+ 'static {
    let service = TowerToHyperService::new(router.clone());
    service_fn(move |mut request: hyper::Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        hyper::service::Service::call(&service, request)
    })
}

/// Errors about one connection, which say nothing about the listener.
fn is_connection_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}
