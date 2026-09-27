//! Test harness for the proxy: a mock upstream (hyper server on
//! `127.0.0.1:0`), the proxy served on `127.0.0.1:0`, a raw HTTP/1.1 client
//! over a `TcpStream`, and a [`CallSink`] that keeps every submitted call.
//!
//! The client is raw on purpose: it sends exactly the bytes a test writes
//! (wrong `Host`, `Origin`, chunked bodies) and reads the response bytes as
//! they arrive, so tests see what a real client would.

#![allow(dead_code)]

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use cs_proxy::{CallSink, PendingCall, Proxy, ProxyConfig, Upstream};
use http::{HeaderMap, Request, Response};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// How long any single wait in a test may take.
pub const WAIT: Duration = Duration::from_secs(10);

/// The default run the test proxy uses.
pub const DEFAULT_RUN: &str = "proxy-test";

// ---------------------------------------------------------------- sink

/// Keeps every submitted call.
#[derive(Default)]
pub struct RecordingSink {
    calls: Mutex<Vec<PendingCall>>,
}

impl CallSink for RecordingSink {
    fn submit(&self, call: PendingCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl RecordingSink {
    pub fn calls(&self) -> Vec<PendingCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Waits until at least `count` calls were submitted, then returns them all.
    pub async fn wait_for(&self, count: usize) -> Vec<PendingCall> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let calls = self.calls();
            if calls.len() >= count {
                return calls;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of {count} calls recorded",
                calls.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

// ---------------------------------------------------------------- upstream

/// The mock upstream's response body.
pub type MockBody = BoxBody<Bytes, io::Error>;

/// A request as the mock upstream received it.
#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    /// Path and query.
    pub target: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

type Handler = Arc<dyn Fn(&Received) -> Response<MockBody> + Send + Sync>;

/// A hyper HTTP/1.1 server on `127.0.0.1:0` that answers with `handler`.
pub struct MockUpstream {
    pub addr: SocketAddr,
    received: Arc<Mutex<Vec<Received>>>,
}

impl MockUpstream {
    pub async fn start(
        handler: impl Fn(&Received) -> Response<MockBody> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let log = Arc::clone(&received);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let handler = Arc::clone(&handler);
                let log = Arc::clone(&log);
                let service = service_fn(move |request: Request<Incoming>| {
                    let handler = Arc::clone(&handler);
                    let log = Arc::clone(&log);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body
                            .collect()
                            .await
                            .map(|b| b.to_bytes())
                            .unwrap_or_default();
                        let received = Received {
                            method: parts.method.to_string(),
                            target: parts
                                .uri
                                .path_and_query()
                                .map(|p| p.as_str().to_owned())
                                .unwrap_or_default(),
                            headers: parts.headers,
                            body,
                        };
                        let response = handler(&received);
                        log.lock().unwrap().push(received);
                        Ok::<_, Infallible>(response)
                    }
                });
                tokio::spawn(async move {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self { addr, received }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

/// A raw TCP upstream, for framings hyper's server won't produce: for each
/// connection it reads one request, writes `response` exactly as given, then
/// closes the connection (a FIN, since the request was read in full).
pub struct RawUpstream {
    pub addr: SocketAddr,
}

impl RawUpstream {
    pub async fn start(response: impl Into<Bytes>) -> Self {
        let response = response.into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let response = response.clone();
                tokio::spawn(async move {
                    if read_request(&stream).await.is_ok() {
                        let _ = write_all(&stream, &response).await;
                    }
                });
            }
        });
        Self { addr }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Reads one request with a `content-length` body (or none).
async fn read_request(stream: &TcpStream) -> io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return Ok(());
            }
        }
        stream.readable().await?;
        match stream.try_read(&mut chunk) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => request.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
}

async fn write_all(stream: &TcpStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.writable().await?;
        match stream.try_write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A complete response body.
pub fn full(body: impl Into<Bytes>) -> MockBody {
    Full::new(body.into())
        .map_err(|never| match never {})
        .boxed()
}

/// A response body fed from a channel: each `Ok` is a chunk, an `Err` aborts
/// the body (hyper then closes the connection without finishing it).
pub fn channel_body() -> (mpsc::Sender<io::Result<Bytes>>, MockBody) {
    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(16);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        let item = rx.recv().await?;
        Some((item.map(Frame::data), rx))
    });
    (tx, StreamBody::new(stream).boxed())
}

/// Builds a mock response from `(name, value)` pairs, in order.
pub fn response(status: u16, headers: &[(&str, &str)], body: MockBody) -> Response<MockBody> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(body).unwrap()
}

// ---------------------------------------------------------------- proxy

/// The proxy served on `127.0.0.1:0`.
pub struct TestProxy {
    pub addr: SocketAddr,
    pub sink: Arc<RecordingSink>,
}

impl TestProxy {
    pub async fn start(upstream: &str, capture: bool) -> Self {
        Self::start_on("127.0.0.1:0", upstream, capture).await
    }

    pub async fn start_on(listen: &str, upstream: &str, capture: bool) -> Self {
        let listener = TcpListener::bind(listen).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sink = Arc::new(RecordingSink::default());
        let config = ProxyConfig {
            upstream: Upstream::parse(upstream).unwrap(),
            default_run_id: DEFAULT_RUN.into(),
            listen_port: addr.port(),
        };
        let proxy = Proxy::try_new(config, sink.clone(), Arc::new(move || capture)).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let proxy = proxy.clone();
                let service = service_fn(move |request: Request<Incoming>| {
                    let proxy = proxy.clone();
                    async move { Ok::<_, Infallible>(proxy.handle(request).await) }
                });
                tokio::spawn(async move {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self { addr, sink }
    }

    /// The `Host` value the proxy accepts.
    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

// ---------------------------------------------------------------- raw client

/// An HTTP/1.1 request as bytes. Adds `content-length` when there is a body
/// and none of the given headers sets it.
pub fn request_bytes(
    method: &str,
    target: &str,
    host: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!("{method} {target} HTTP/1.1\r\nhost: {host}\r\n").into_bytes();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    let has_length = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
    if !body.is_empty() && !has_length {
        out.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// A response head as it came off the wire.
#[derive(Debug, Clone)]
pub struct Head {
    pub status: u16,
    /// Lower-cased names, raw values, in wire order.
    pub headers: Vec<(String, Vec<u8>)>,
}

impl Head {
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    pub fn is_chunked(&self) -> bool {
        self.get("transfer-encoding") == Some(b"chunked")
    }

    pub fn content_length(&self) -> Option<usize> {
        std::str::from_utf8(self.get("content-length")?)
            .ok()?
            .parse()
            .ok()
    }

    /// Headers sorted by name (values of one name keep their order).
    pub fn sorted(&self) -> Vec<(String, Vec<u8>)> {
        let mut headers = self.headers.clone();
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        headers
    }
}

/// A raw HTTP/1.1 client connection.
pub struct RawClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl RawClient {
    pub async fn connect(addr: SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
        }
    }

    pub async fn send(&self, bytes: &[u8]) -> io::Result<()> {
        write_all(&self.stream, bytes).await
    }

    /// Bytes read but not yet parsed.
    pub fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Reads until at least `len` bytes are buffered past what was parsed.
    pub async fn fill_to(&mut self, len: usize) {
        while self.buf.len() < len {
            self.fill_or_eof().await;
        }
    }

    /// Reads more bytes into the buffer; `false` at end of stream.
    async fn fill(&mut self) -> io::Result<bool> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            self.stream.readable().await?;
            match self.stream.try_read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(true);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn fill_or_eof(&mut self) {
        let more = tokio::time::timeout(WAIT, self.fill())
            .await
            .expect("timed out reading from the proxy")
            .expect("reading from the proxy failed");
        assert!(more, "the proxy closed the connection early");
    }

    /// Reads up to and including the next `\r\n`.
    async fn line(&mut self) -> Vec<u8> {
        loop {
            if let Some(end) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line = self.buf[..end].to_vec();
                self.buf.drain(..end + 2);
                return line;
            }
            self.fill_or_eof().await;
        }
    }

    async fn exact(&mut self, len: usize) -> Vec<u8> {
        while self.buf.len() < len {
            self.fill_or_eof().await;
        }
        self.buf.drain(..len).collect()
    }

    pub async fn read_head(&mut self) -> Head {
        let status_line = self.line().await;
        let status_line = String::from_utf8(status_line).unwrap();
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("bad status line {status_line:?}"));
        let mut headers = Vec::new();
        loop {
            let line = self.line().await;
            if line.is_empty() {
                break;
            }
            let colon = line.iter().position(|b| *b == b':').unwrap();
            let name = String::from_utf8(line[..colon].to_vec())
                .unwrap()
                .to_ascii_lowercase();
            let value = line[colon + 1..].trim_ascii().to_vec();
            headers.push((name, value));
        }
        Head { status, headers }
    }

    /// The next chunk of a chunked body, `None` after the last one.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        let size_line = self.line().await;
        let size = std::str::from_utf8(&size_line).unwrap();
        let size = size.split(';').next().unwrap().trim();
        let size = usize::from_str_radix(size, 16).unwrap();
        if size == 0 {
            // Trailers, then the empty line.
            while !self.line().await.is_empty() {}
            return None;
        }
        let data = self.exact(size).await;
        assert_eq!(self.exact(2).await, b"\r\n");
        Some(data)
    }

    /// The whole body of a response to a request that wasn't `HEAD`.
    pub async fn read_body(&mut self, head: &Head) -> Vec<u8> {
        if head.is_chunked() {
            let mut body = Vec::new();
            while let Some(chunk) = self.next_chunk().await {
                body.extend_from_slice(&chunk);
            }
            body
        } else if let Some(length) = head.content_length() {
            self.exact(length).await
        } else {
            while tokio::time::timeout(WAIT, self.fill())
                .await
                .unwrap()
                .unwrap()
            {}
            std::mem::take(&mut self.buf)
        }
    }

    /// Sends one request and reads the whole response.
    pub async fn exchange(addr: SocketAddr, request: &[u8]) -> (Head, Vec<u8>) {
        let mut client = Self::connect(addr).await;
        client.send(request).await.unwrap();
        let head = client.read_head().await;
        let body = client.read_body(&head).await;
        (head, body)
    }

    /// Whether the peer closed the connection (reads until end of stream).
    pub async fn closed_by_peer(&mut self) -> bool {
        matches!(
            tokio::time::timeout(WAIT, async {
                loop {
                    match self.fill().await {
                        Ok(true) => continue,
                        Ok(false) | Err(_) => return,
                    }
                }
            })
            .await,
            Ok(())
        )
    }
}

/// `(name, value)` pairs sorted by name, for comparing header sets.
pub fn sorted_pairs(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = pairs
        .iter()
        .map(|(n, v)| (n.to_ascii_lowercase(), v.as_bytes().to_vec()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A header map as sorted `(name, value)` pairs.
pub fn sorted_map(headers: &HeaderMap) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = headers
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_vec()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
