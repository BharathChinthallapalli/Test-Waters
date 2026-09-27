//! The model-call proxy in the daemon (feature 03, task 8 `wire`; design,
//! "Daemon wiring" and requirements 3, 5, 6 and 7).
//!
//! **Address** (requirement 6). With `--proxy-listen` the proxy binds that
//! address and leaves the port file alone. Otherwise the port comes from
//! [`PORT_FILE_NAME`] in the data directory, so the `ANTHROPIC_BASE_URL` a user
//! exported keeps working across restarts:
//! - no file: pick the first free port of [`PORT_BAND`] in [`candidate_ports`]
//!   order and save it, owner-only and atomically
//!   (`cs_store::fsperm::write_owner_only_atomic`, like `daemon.json`);
//! - a file with a port: bind `127.0.0.1:<port>`. If that fails, startup fails
//!   with one line naming the port, the file, the cause and the fix
//!   ([`saved_port_error`]): "in use" for `AddrInUse`, "reserved by the system"
//!   for `PermissionDenied` (Windows' `WSAEACCES` for an excluded port range;
//!   Rust maps it to `PermissionDenied`, `library/std/src/sys/io/error/windows.rs`).
//!   A new port would silently break every client configured with the old one;
//! - a file that doesn't hold a port (not ASCII digits, 0, over 65535, over
//!   [`MAX_PORT_FILE_BYTES`], not UTF-8): startup fails, and the file is kept.
//!   It is not replaced with a new port, for the same reason: something other
//!   than Callsheet wrote it, and only the user knows which port their clients
//!   use. Deleting the file picks a new port; `--proxy-listen` sets one.
//!
//! The file holds the port in ASCII decimal and a newline; surrounding ASCII
//! whitespace is ignored when it is read, so a hand-written `echo 4200 >` works.
//!
//! **Why a fixed band, not port 0.** Port 0 gives a port from the OS's
//! ephemeral range, and a port saved from there can be taken later:
//! - Windows' dynamic range is 49152–65535 ("Troubleshoot port exhaustion
//!   issues", Microsoft Learn,
//!   <https://learn.microsoft.com/windows/client-management/troubleshoot-tcpip-port-exhaust>).
//!   Hyper-V, WSL2 and WinNAT reserve excluded port ranges from it, which can
//!   move after a reboot; a bind to an excluded port fails with `WSAEACCES`
//!   even though no program holds it. Microsoft's workaround is "a port that is
//!   not included in the default dynamic port range" (KB 3039044,
//!   <https://learn.microsoft.com/troubleshoot/windows-server/networking/error-10013-wsaeacces-is-returned>).
//! - Linux's `ip_local_port_range` defaults to 32768–60999: "the local port
//!   range that is used by TCP and UDP to choose the local port"
//!   (`Documentation/networking/ip-sysctl.rst`), so any outgoing connection,
//!   loopback ones included, can take a port there as its source port.
//! - macOS uses 49152–65535, the IANA dynamic range.
//!
//! [`PORT_BAND`], 20000–29999, is above the privileged ports and below all
//! three ranges. The order is a fixed permutation of the band that starts at a
//! point derived from the data directory's path, so one data directory always
//! tries the same ports first and two data directories rarely collide. A port
//! in use (or reserved) is skipped; if the whole band is taken, startup fails
//! and names `--proxy-listen`.
//!
//! **Serving** ([`start`]). `cs_proxy::Proxy::try_new` builds the proxy's HTTPS
//! client, so a client that can't be built (no usable root certificates, say)
//! stops startup instead of answering every call with 502. The proxy is served
//! by `crate::serve::spawn_proxy`: the control API's accept loop with the same
//! header-read timeout and no request timeout. Calls without a run header go to
//! the run `proxy-<startedAtMs>` (`startedAtMs` as in `daemon.json`). Content is
//! captured only while `settings.setCaptureContent` has it on
//! (`Store::capture_content`, read per call).
//!
//! **Recording.** One [`Recorder`], shared as `Arc<Recorder>`: the proxy's
//! `CallSink` and the source of `health.proxy`. At shutdown the servers stop
//! first (open calls finish within the drain timeout and are submitted), then
//! [`drain_recorder`] waits up to [`RECORDER_SHUTDOWN_TIMEOUT`] for the queue to
//! be written, then the store closes (`crate::shutdown`).
//!
//! **rustls provider.** `Proxy::try_new` installs ring as the process's rustls
//! provider with `CryptoProvider::install_default`, which is a
//! `std::sync::OnceLock::set` and returns `Err` when one is already installed
//! (rustls 0.23.45, `src/crypto/mod.rs`, `static_default`); cs-proxy ignores that
//! `Err`. So a second proxy in the same process, or several built at once by
//! parallel tests, can't panic there (tested below).

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cs_proxy::forward::ClientBuildError;
use cs_proxy::recorder::QUEUE_CAPACITY;
use cs_proxy::{Proxy, ProxyConfig, Recorder, Upstream};
use cs_store::{Store, fsperm};
use tokio::net::TcpListener;

use crate::serve::{self, ServeConfig, Server};

/// The proxy's saved port, in the data directory.
pub const PORT_FILE_NAME: &str = "proxy-port";

/// A port file longer than this is not a port.
pub const MAX_PORT_FILE_BYTES: u64 = 32;

/// How long the shutdown waits for the recorder to write its queue.
pub const RECORDER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the proxy can't start. Every message is one line naming the file or
/// address involved, never a request, a header or the upstream's full URL.
#[derive(Debug)]
pub enum ProxyError {
    /// The port file exists but can't be read.
    PortFileRead { path: PathBuf, source: io::Error },
    /// The port file doesn't hold a port from 1 to 65535.
    PortFileCorrupt { path: PathBuf },
    /// The port picked on a first start can't be saved.
    PortFileWrite { path: PathBuf, source: io::Error },
    /// Another program is listening on the saved port (`AddrInUse`).
    SavedPortInUse { port: u16, path: PathBuf },
    /// The system reserves the saved port (`PermissionDenied`: Windows'
    /// `WSAEACCES` for an excluded port range, or a privileged port on Unix).
    SavedPortReserved { port: u16, path: PathBuf },
    /// The saved port can't be bound for another reason.
    SavedPortUnavailable {
        port: u16,
        path: PathBuf,
        source: io::Error,
    },
    /// No port of [`PORT_BAND`] is free on a first start.
    NoFreePort,
    /// `--proxy-listen`, or a port of the band, can't be bound.
    Bind {
        address: SocketAddrV4,
        source: io::Error,
    },
    /// The bound address can't be read, or isn't IPv4.
    BoundAddress(io::Error),
    /// The proxy's HTTPS client can't be built.
    Client(ClientBuildError),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortFileRead { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::PortFileCorrupt { path } => write!(
                f,
                "{} does not hold a port number from 1 to 65535; delete it to pick a new proxy \
                 port (then update ANTHROPIC_BASE_URL), or pass --proxy-listen 127.0.0.1:<port>",
                path.display()
            ),
            Self::PortFileWrite { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
            Self::SavedPortInUse { port, path } => write!(
                f,
                "cannot listen on 127.0.0.1:{port}, the proxy port saved in {}: port {port} is \
                 in use; stop the program using it, or pass --proxy-listen 127.0.0.1:<port> and \
                 point ANTHROPIC_BASE_URL at it",
                path.display()
            ),
            Self::SavedPortReserved { port, path } => write!(
                f,
                "cannot listen on 127.0.0.1:{port}, the proxy port saved in {}: port {port} is \
                 reserved by the system; pass --proxy-listen 127.0.0.1:<port> with another \
                 port, or delete {} to pick a new one (then update ANTHROPIC_BASE_URL)",
                path.display(),
                path.display()
            ),
            Self::SavedPortUnavailable { port, path, source } => write!(
                f,
                "cannot listen on 127.0.0.1:{port}, the proxy port saved in {}: {source}; pass \
                 --proxy-listen 127.0.0.1:<port> with another port, or delete {} to pick a new \
                 one (then update ANTHROPIC_BASE_URL)",
                path.display(),
                path.display()
            ),
            Self::NoFreePort => write!(
                f,
                "no port from {} to {} is free for the proxy; pass --proxy-listen \
                 127.0.0.1:<port>",
                PORT_BAND.start(),
                PORT_BAND.end()
            ),
            Self::Bind { address, source } => {
                write!(f, "cannot listen on {address} for the proxy: {source}")
            }
            Self::BoundAddress(source) => {
                write!(f, "cannot read the proxy's bound address: {source}")
            }
            Self::Client(source) => source.fmt(f),
        }
    }
}

impl std::error::Error for ProxyError {}

/// The port in a port file's contents: ASCII digits, surrounded by nothing but
/// ASCII whitespace, from 1 to 65535.
pub fn parse_port(contents: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(contents).ok()?.trim_ascii();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port != 0)
}

/// The port saved in `data_dir`, `None` when there is no port file.
pub fn saved_port(data_dir: &Path) -> Result<Option<u16>, ProxyError> {
    let path = data_dir.join(PORT_FILE_NAME);
    let read_error = |source| ProxyError::PortFileRead {
        path: path.clone(),
        source,
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(read_error(error)),
    };
    let mut contents = Vec::new();
    file.take(MAX_PORT_FILE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(read_error)?;
    if contents.len() as u64 > MAX_PORT_FILE_BYTES {
        return Err(ProxyError::PortFileCorrupt { path });
    }
    parse_port(&contents)
        .map(Some)
        .ok_or(ProxyError::PortFileCorrupt { path })
}

/// Saves `port` in `data_dir`, owner-only and atomically.
pub fn save_port(data_dir: &Path, port: u16) -> Result<(), ProxyError> {
    let path = data_dir.join(PORT_FILE_NAME);
    fsperm::write_owner_only_atomic(&path, format!("{port}\n").as_bytes())
        .map_err(|source| ProxyError::PortFileWrite { path, source })
}

/// Binds the proxy's listener (see the module docs for where the port comes
/// from) and returns it with its bound address.
pub async fn bind(
    data_dir: &Path,
    listen: Option<SocketAddrV4>,
) -> Result<(TcpListener, SocketAddrV4), ProxyError> {
    if let Some(address) = listen {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|source| ProxyError::Bind { address, source })?;
        let bound = bound_address(&listener)?;
        return Ok((listener, bound));
    }
    if let Some(port) = saved_port(data_dir)? {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|source| saved_port_error(port, data_dir.join(PORT_FILE_NAME), source))?;
        let bound = bound_address(&listener)?;
        return Ok((listener, bound));
    }
    for port in candidate_ports(data_dir) {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let listener = match TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) if port_taken(&error) => continue,
            Err(source) => return Err(ProxyError::Bind { address, source }),
        };
        let bound = bound_address(&listener)?;
        save_port(data_dir, bound.port())?;
        tracing::info!(port = bound.port(), "proxy port picked and saved");
        return Ok((listener, bound));
    }
    Err(ProxyError::NoFreePort)
}

/// Where a first start picks the proxy's port (see the module docs).
pub const PORT_BAND: RangeInclusive<u16> = 20000..=29999;

/// A prime that doesn't divide the band's size, so stepping by it visits every
/// port of the band once.
const PORT_STRIDE: u32 = 7919;

/// Every port of [`PORT_BAND`] once, in a fixed order that starts at a point
/// derived from `data_dir` (FNV-1a over its path's bytes).
pub fn candidate_ports(data_dir: &Path) -> impl Iterator<Item = u16> {
    let size = u32::from(PORT_BAND.end() - PORT_BAND.start()) + 1;
    let hash = data_dir
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
        });
    let start = hash % size;
    (0..size).map(move |step| {
        let offset = (start + step * PORT_STRIDE) % size;
        // `offset < size`, so this stays within the band.
        PORT_BAND.start() + offset as u16
    })
}

/// A bind error that means this port can't be used, and the next one may be.
fn port_taken(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
    )
}

/// The startup error for a saved `port` that can't be bound.
pub fn saved_port_error(port: u16, path: PathBuf, source: io::Error) -> ProxyError {
    match source.kind() {
        io::ErrorKind::AddrInUse => ProxyError::SavedPortInUse { port, path },
        io::ErrorKind::PermissionDenied => ProxyError::SavedPortReserved { port, path },
        _ => ProxyError::SavedPortUnavailable { port, path, source },
    }
}

fn bound_address(listener: &TcpListener) -> Result<SocketAddrV4, ProxyError> {
    match listener.local_addr().map_err(ProxyError::BoundAddress)? {
        SocketAddr::V4(address) => Ok(address),
        SocketAddr::V6(_) => Err(ProxyError::BoundAddress(io::Error::new(
            io::ErrorKind::InvalidData,
            "the proxy listener is not bound to an IPv4 address",
        ))),
    }
}

/// The run for calls that name none: `proxy-<startedAtMs>`.
pub fn default_run_id(started_at_ms: u64) -> String {
    format!("proxy-{started_at_ms}")
}

/// A proxy serving on its own task.
#[derive(Debug)]
pub struct RunningProxy {
    /// `127.0.0.1:<bound port>`.
    pub address: SocketAddrV4,
    pub recorder: Arc<Recorder>,
    pub server: Server,
}

/// Builds the proxy for `listener` (bound to `address`), starts its recorder on
/// the current runtime and serves it (see the module docs). Needs a tokio
/// runtime.
pub fn start(
    listener: TcpListener,
    address: SocketAddrV4,
    upstream: Upstream,
    started_at_ms: u64,
    store: Arc<Store>,
    serve_config: ServeConfig,
) -> Result<RunningProxy, ProxyError> {
    let config = ProxyConfig {
        upstream,
        default_run_id: default_run_id(started_at_ms),
        listen_port: address.port(),
    };
    let recorder = Arc::new(Recorder::start(Arc::clone(&store), QUEUE_CAPACITY));
    let capture = Arc::new(move || store.capture_content());
    let proxy = match Proxy::try_new(config, recorder.clone(), capture) {
        Ok(proxy) => proxy,
        Err(error) => {
            // Nothing was queued; the drain task ends once its sender is gone.
            drop(recorder);
            return Err(ProxyError::Client(error));
        }
    };
    let server = serve::spawn_proxy(listener, proxy, serve_config);
    Ok(RunningProxy {
        address,
        recorder,
        server,
    })
}

/// Waits for `recorder` to write every queued call, at most `timeout`; logs if
/// it takes longer, and moves on. Calls it writes after that are refused by
/// the closed store and counted as dropped.
pub async fn drain_recorder(recorder: &Recorder, timeout: Duration) {
    match tokio::time::timeout(timeout, recorder.shutdown()).await {
        Ok(()) => {
            let stats = recorder.stats();
            tracing::info!(
                calls_recorded = stats.calls_recorded,
                records_dropped = stats.records_dropped,
                "shutdown: call recorder drained"
            );
        }
        Err(_) => {
            let stats = recorder.stats();
            tracing::warn!(
                waited_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                calls_recorded = stats.calls_recorded,
                records_dropped = stats.records_dropped,
                "shutdown: the call recorder did not finish writing in time; calls still \
                 queued may be lost"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_store::secrets::InMemorySecretStore;

    fn data_dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = crate::instance::prepare_data_dir(&root.path().join("data")).unwrap();
        (root, dir)
    }

    #[test]
    fn parse_port_takes_1_to_65535_in_decimal_only() {
        for (contents, port) in [
            (&b"1"[..], 1),
            (b"4200\n", 4200),
            (b"  4200 \r\n", 4200),
            (b"65535", 65535),
            (b"004200", 4200),
        ] {
            assert_eq!(parse_port(contents), Some(port), "{contents:?}");
        }
        for contents in [
            &b""[..],
            b"\n",
            b"0",
            b"00",
            b"65536",
            b"99999999999999999999",
            b"-1",
            b"+4200",
            b"42 00",
            b"4200x",
            b"0x10",
            b"port=4200",
            b"\xff4200",
            b"\xef\xbc\x94", // FULLWIDTH DIGIT FOUR
        ] {
            assert_eq!(parse_port(contents), None, "{contents:?}");
        }
    }

    #[test]
    fn port_file_round_trips_owner_only() {
        let (_root, dir) = data_dir();

        assert!(saved_port(&dir).unwrap().is_none());
        save_port(&dir, 4321).unwrap();

        assert_eq!(saved_port(&dir).unwrap(), Some(4321));
        assert_eq!(
            std::fs::read_to_string(dir.join(PORT_FILE_NAME)).unwrap(),
            "4321\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(PORT_FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn the_first_start_saves_the_port_and_the_next_reuses_it() {
        let (_root, dir) = data_dir();

        let (first, address) = bind(&dir, None).await.unwrap();
        assert_eq!(*address.ip(), Ipv4Addr::LOCALHOST);
        assert!(PORT_BAND.contains(&address.port()), "{address}");
        assert_eq!(saved_port(&dir).unwrap(), Some(address.port()));
        drop(first);

        let (_second, again) = bind(&dir, None).await.unwrap();
        assert_eq!(again, address);
    }

    #[test]
    fn candidate_ports_visit_the_whole_band_once_in_a_fixed_order() {
        let a = Path::new("/data/a");

        let order: Vec<u16> = candidate_ports(a).collect();

        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, PORT_BAND.collect::<Vec<_>>());
        assert_eq!(order, candidate_ports(a).collect::<Vec<_>>());
        assert_ne!(
            candidate_ports(a).next(),
            candidate_ports(Path::new("/data/b")).next(),
            "two data directories start at different ports"
        );
        // Outside Linux's (32768-60999) and Windows' and macOS's (49152-65535)
        // ephemeral ranges, above the privileged ports.
        assert!(*PORT_BAND.start() > 1024 && *PORT_BAND.end() < 32768);
    }

    #[tokio::test]
    async fn a_first_start_skips_a_taken_port_of_the_band() {
        let (_root, dir) = data_dir();
        let mut order = candidate_ports(&dir);
        let first = order.next().unwrap();
        // Held for the test; if something else holds it already, it's taken all
        // the same.
        let _held = std::net::TcpListener::bind(("127.0.0.1", first));

        let (_listener, address) = bind(&dir, None).await.unwrap();

        assert_ne!(address.port(), first);
        assert!(PORT_BAND.contains(&address.port()), "{address}");
        assert_eq!(saved_port(&dir).unwrap(), Some(address.port()));
    }

    #[tokio::test]
    async fn a_taken_saved_port_is_an_error_naming_the_port_and_the_fix() {
        let (_root, dir) = data_dir();
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        save_port(&dir, port).unwrap();

        let error = bind(&dir, None).await.unwrap_err();

        assert!(
            matches!(error, ProxyError::SavedPortInUse { port: p, .. } if p == port),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains(&format!("127.0.0.1:{port}")), "{message}");
        assert!(message.contains("is in use"), "{message}");
        assert!(message.contains(PORT_FILE_NAME), "{message}");
        assert!(message.contains("--proxy-listen"), "{message}");
        assert!(!message.contains('\n'), "{message}");
        // The saved port is kept for when the port is free again.
        assert_eq!(saved_port(&dir).unwrap(), Some(port));
    }

    #[test]
    fn bind_errors_on_the_saved_port_name_their_cause() {
        let path = PathBuf::from("data").join(PORT_FILE_NAME);
        let error = |kind: io::ErrorKind| saved_port_error(4242, path.clone(), kind.into());

        let reserved = error(io::ErrorKind::PermissionDenied);
        assert!(matches!(
            reserved,
            ProxyError::SavedPortReserved { port: 4242, .. }
        ));
        let message = reserved.to_string();
        assert!(
            message.contains("port 4242 is reserved by the system")
                && message.contains("--proxy-listen")
                && message.contains(&format!("delete {}", path.display()))
                && !message.contains("in use")
                && !message.contains('\n'),
            "{message}"
        );

        let in_use = error(io::ErrorKind::AddrInUse);
        assert!(matches!(
            in_use,
            ProxyError::SavedPortInUse { port: 4242, .. }
        ));
        let message = in_use.to_string();
        assert!(
            message.contains("port 4242 is in use") && !message.contains("reserved"),
            "{message}"
        );

        let other = error(io::ErrorKind::AddrNotAvailable);
        assert!(matches!(other, ProxyError::SavedPortUnavailable { .. }));
        assert!(other.to_string().contains("--proxy-listen"));
    }

    #[tokio::test]
    async fn a_corrupt_port_file_is_refused_and_kept() {
        for contents in [&b"not a port"[..], b"0", b"65536", &[b'1'; 40]] {
            let (_root, dir) = data_dir();
            std::fs::write(dir.join(PORT_FILE_NAME), contents).unwrap();

            let error = bind(&dir, None).await.unwrap_err();

            assert!(
                matches!(error, ProxyError::PortFileCorrupt { .. }),
                "{contents:?}: {error}"
            );
            let message = error.to_string();
            assert!(message.contains(PORT_FILE_NAME), "{message}");
            assert!(message.contains("--proxy-listen"), "{message}");
            assert!(!message.contains('\n'), "{message}");
            assert_eq!(
                std::fs::read(dir.join(PORT_FILE_NAME)).unwrap(),
                contents,
                "the file was replaced"
            );
        }
    }

    #[tokio::test]
    async fn proxy_listen_neither_reads_nor_writes_the_port_file() {
        let (_root, dir) = data_dir();
        std::fs::write(dir.join(PORT_FILE_NAME), b"garbage").unwrap();

        let (_listener, address) = bind(&dir, Some("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();

        assert_ne!(address.port(), 0);
        assert_eq!(std::fs::read(dir.join(PORT_FILE_NAME)).unwrap(), b"garbage");

        let (_root, empty) = data_dir();
        bind(&empty, Some("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
        assert!(!empty.join(PORT_FILE_NAME).exists());
    }

    #[test]
    fn the_default_run_is_proxy_and_the_start_time() {
        assert_eq!(default_run_id(1_790_000_000_000), "proxy-1790000000000");
        assert!(cs_proxy::headers::valid_run_id(&default_run_id(u64::MAX)));
    }

    /// Several proxies started at once in one process, then again: installing
    /// the rustls provider never panics (see the module docs).
    #[test]
    fn starting_proxies_in_parallel_and_twice_does_not_panic() {
        let (_root, dir) = data_dir();
        let store =
            Arc::new(Store::open(&dir, Arc::new(InMemorySecretStore::new([3; 32]))).unwrap());
        for _round in 0..2 {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let store = Arc::clone(&store);
                    std::thread::spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        runtime.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let address = bound_address(&listener).unwrap();
                            let running = start(
                                listener,
                                address,
                                Upstream::parse(Upstream::ANTHROPIC).unwrap(),
                                1,
                                store,
                                ServeConfig::default(),
                            )
                            .unwrap();
                            running.server.stop().await;
                            drain_recorder(&running.recorder, RECORDER_SHUTDOWN_TIMEOUT).await;
                        });
                    })
                })
                .collect();
            for thread in threads {
                thread.join().expect("starting a proxy panicked");
            }
        }
    }
}
