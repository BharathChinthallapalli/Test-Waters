//! Serves the control API's HTTP layer on `127.0.0.1:<random port>` with a fixed
//! demo token and a toy handler that answers only `version`, for trying the layer
//! with `curl`. Not the daemon: no store, no token file, no discovery file. Served
//! with [`cs_daemon::serve`] (header-read timeout), like the daemon; Ctrl+C stops it.
//!
//! ```sh
//! cargo run -p cs-daemon --example rpc_http_demo
//! curl -i -H 'Authorization: Bearer demo-token' \
//!   --data '{"jsonrpc":"2.0","method":"version","id":1}' http://127.0.0.1:<port>/rpc
//! ```

use std::sync::Arc;

use cs_daemon::http::{HttpConfig, StaticToken, router};
use cs_daemon::rpc::{Handler, RpcError};
use cs_daemon::serve::{self, DRAIN_TIMEOUT, ServeConfig};
use serde_json::{Value, json};

/// A demo token; the daemon generates a random one per install.
const DEMO_TOKEN: &str = "demo-token";

struct DemoHandler;

impl Handler for DemoHandler {
    async fn call(&self, method: &str, _params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            "version" => Ok(json!({ "daemonVersion": env!("CARGO_PKG_VERSION") })),
            _ => Err(RpcError::method_not_found()),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .init();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = HttpConfig::for_listener(&listener)?;
    let port = config.port;
    let app = router(
        config,
        Arc::new(StaticToken::new(DEMO_TOKEN)),
        Arc::new(DemoHandler),
    );
    println!("listening on 127.0.0.1:{port}");
    serve::serve(listener, app, ServeConfig::default(), async {
        // An error means Ctrl+C can't be caught; stop at once rather than never.
        let _ = tokio::signal::ctrl_c().await;
        DRAIN_TIMEOUT
    })
    .await;
    Ok(())
}
