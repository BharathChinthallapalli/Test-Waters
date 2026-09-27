//! Command line and configuration (R1.1, R1.2).
//!
//! Owned by unit `daemon-proc`; the proxy flags by feature 03, task 8 (`wire`).
//! The whole command line is parsed, and the listen addresses and the upstream
//! checked, before anything touches the file system or the network: a
//! `--listen` or `--proxy-listen` other than `127.0.0.1:<port>`, or a
//! `--proxy-upstream` that `cs_proxy::Upstream::parse` refuses, ends the
//! process with status 2 ([`USAGE_EXIT_CODE`]) before any file or socket exists.
//!
//! ```text
//! cs-daemon [--listen 127.0.0.1:<port>] [--data-dir <path>]
//!           [--proxy-listen 127.0.0.1:<port>] [--proxy-upstream <url>]
//! cs-daemon --help
//! ```
//!
//! Each flag and its value are separate arguments (no `--flag=value`, which would
//! need the flag and a non-UTF-8 path in one argument). The parser is written by
//! hand; four flags don't justify a CLI crate.
//!
//! **Data directory.** Without `--data-dir`, the per-user data directory comes
//! from `etcetera::app_strategy::choose_native_strategy` with the app name
//! `Callsheet` and no domain or author (etcetera 0.11.0, `src/app_strategy.rs`
//! and `src/app_strategy/{xdg,apple,windows}.rs`):
//! - Linux and other Unix: `$XDG_DATA_HOME/callsheet`, by default
//!   `~/.local/share/callsheet` (XDG; the name is lower-cased);
//! - macOS: `~/Library/Application Support/Callsheet` (Apple conventions);
//! - Windows: `%APPDATA%\Callsheet\data`, by default
//!   `C:\Users\<user>\AppData\Roaming\Callsheet\data`.
//!
//! **Proxy.** Without `--proxy-listen` the proxy's port comes from the
//! `proxy-port` file in the data directory (`crate::proxy`); with it, that file
//! is neither read nor written. `--proxy-upstream` defaults to
//! [`Upstream::ANTHROPIC`]; `http` is accepted only to a loopback IP (tests).
//! The proxy always runs: there is no flag to turn it off, since every test gets
//! its own data directory and so its own port.
//!
//! There is no configuration file yet; everything comes from the command line.

use std::ffi::OsString;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use cs_proxy::{Upstream, UpstreamError};
use etcetera::app_strategy::{AppStrategy, AppStrategyArgs, choose_native_strategy};

/// The listen address when `--listen` isn't given: `127.0.0.1`, on a port the
/// operating system picks. Clients find the real port in `daemon.json`
/// ([`crate::instance`]).
///
/// Provisional: the default control port waits on an owner decision
/// (`.kiro/specs/02-daemon-and-store/tasks.md`, task 0.5, blocked on checking the
/// IANA registry). Until then the coordinator chose port 0.
pub const DEFAULT_LISTEN: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);

/// Exit status for a command line the daemon can't use, including a listen
/// address that isn't `127.0.0.1` (R1.2).
pub const USAGE_EXIT_CODE: u8 = 2;

/// The application name given to etcetera.
const APP_NAME: &str = "Callsheet";

/// Printed for `--help`.
pub const USAGE: &str = "\
Usage: cs-daemon [--listen 127.0.0.1:<port>] [--data-dir <path>]
                 [--proxy-listen 127.0.0.1:<port>] [--proxy-upstream <url>]

Runs the Callsheet daemon: the control API and the model-call proxy on
127.0.0.1, and the local store.

Options:
  --listen <ip:port>   Address of the control API; the IP must be 127.0.0.1.
                       Default 127.0.0.1:0 (the OS picks a free port; clients
                       read the address from daemon.json in the data directory).
  --data-dir <path>    Data directory. Default: the per-user data directory
                       (~/.local/share/callsheet, ~/Library/Application Support/
                       Callsheet or %APPDATA%\\Callsheet\\data).
  --proxy-listen <ip:port>
                       Address of the model-call proxy; the IP must be
                       127.0.0.1. Default: the port saved in proxy-port in the
                       data directory, picked by the OS on the first start and
                       reused after, so ANTHROPIC_BASE_URL stays valid.
  --proxy-upstream <url>
                       Where the proxy forwards calls: an https URL, or http to
                       a loopback IP (tests). Default https://api.anthropic.com.
  --help               Print this help.

Environment:
  CALLSHEET_LOG        Log level: off, error, warn, info (default), debug or trace.
";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon.
    Run(Config),
    /// Print [`USAGE`] and exit 0.
    Help,
}

/// Where the daemon listens and keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Always `127.0.0.1`; the port may be 0.
    pub listen: SocketAddrV4,
    pub data_dir: PathBuf,
    /// `--proxy-listen`: always `127.0.0.1`; the port may be 0. `None` means the
    /// port saved in the data directory (`crate::proxy`).
    pub proxy_listen: Option<SocketAddrV4>,
    /// `--proxy-upstream`, [`Upstream::ANTHROPIC`] by default.
    pub proxy_upstream: Upstream,
}

/// A command line the daemon can't use. Every variant exits with
/// [`USAGE_EXIT_CODE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A flag without its value.
    MissingValue { flag: &'static str },
    /// The same flag twice.
    Repeated { flag: &'static str },
    /// An argument that isn't a known flag.
    UnknownArgument(String),
    /// `--listen` or `--proxy-listen` isn't `<ip>:<port>`.
    InvalidListen { flag: &'static str, value: String },
    /// `--listen` or `--proxy-listen` is a valid address, but not `127.0.0.1`
    /// (R1.2; feature 03, requirement 5).
    NotLoopback {
        flag: &'static str,
        address: SocketAddr,
    },
    /// `--proxy-upstream` was refused. The message never repeats the URL, which
    /// could hold a credential.
    InvalidUpstream(UpstreamError),
    /// `--data-dir` is empty.
    EmptyDataDir,
    /// No `--data-dir`, and etcetera found no home directory.
    NoDataDir,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue { flag } => write!(f, "{flag} needs a value"),
            Self::Repeated { flag } => write!(f, "{flag} was given more than once"),
            Self::UnknownArgument(argument) => {
                write!(f, "unknown argument {argument:?} (see --help)")
            }
            Self::InvalidListen { flag, value } => {
                write!(
                    f,
                    "{flag} {value:?} is not an address like 127.0.0.1:<port>"
                )
            }
            Self::NotLoopback { flag, address } => {
                let what = if *flag == PROXY_LISTEN {
                    "the proxy"
                } else {
                    "the control API"
                };
                write!(f, "{flag} {address}: {what} only listens on 127.0.0.1")
            }
            Self::InvalidUpstream(error) => write!(f, "{PROXY_UPSTREAM}: {error}"),
            Self::EmptyDataDir => f.write_str("--data-dir is empty"),
            Self::NoDataDir => f.write_str(
                "no home directory to put the data directory in; pass --data-dir <path>",
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parses the arguments after the program name. Touches neither the file system
/// nor the network; without `--data-dir` it only reads the environment variables
/// etcetera uses to find the home directory.
pub fn parse_args<I>(args: I) -> Result<Command, ConfigError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut listen: Option<SocketAddrV4> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut proxy_listen: Option<SocketAddrV4> = None;
    let mut proxy_upstream: Option<Upstream> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(Command::Help);
        }
        match arg.to_str() {
            Some(LISTEN) => {
                let value = value_of(LISTEN, &mut args)?;
                set_once(&mut listen, LISTEN, parse_listen(LISTEN, &value)?)?;
            }
            Some(DATA_DIR) => {
                let value = value_of(DATA_DIR, &mut args)?;
                if value.is_empty() {
                    return Err(ConfigError::EmptyDataDir);
                }
                set_once(&mut data_dir, DATA_DIR, PathBuf::from(value))?;
            }
            Some(PROXY_LISTEN) => {
                let value = value_of(PROXY_LISTEN, &mut args)?;
                let address = parse_listen(PROXY_LISTEN, &value)?;
                set_once(&mut proxy_listen, PROXY_LISTEN, address)?;
            }
            Some(PROXY_UPSTREAM) => {
                let value = value_of(PROXY_UPSTREAM, &mut args)?;
                let upstream = Upstream::parse(&value.to_string_lossy())
                    .map_err(ConfigError::InvalidUpstream)?;
                set_once(&mut proxy_upstream, PROXY_UPSTREAM, upstream)?;
            }
            _ => {
                return Err(ConfigError::UnknownArgument(
                    arg.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    let proxy_upstream = match proxy_upstream {
        Some(upstream) => upstream,
        None => Upstream::parse(Upstream::ANTHROPIC).map_err(ConfigError::InvalidUpstream)?,
    };
    Ok(Command::Run(Config {
        listen: listen.unwrap_or(DEFAULT_LISTEN),
        data_dir,
        proxy_listen,
        proxy_upstream,
    }))
}

const LISTEN: &str = "--listen";
const DATA_DIR: &str = "--data-dir";
const PROXY_LISTEN: &str = "--proxy-listen";
const PROXY_UPSTREAM: &str = "--proxy-upstream";

fn value_of(
    flag: &'static str,
    rest: &mut impl Iterator<Item = OsString>,
) -> Result<OsString, ConfigError> {
    rest.next().ok_or(ConfigError::MissingValue { flag })
}

fn set_once<T>(slot: &mut Option<T>, flag: &'static str, value: T) -> Result<(), ConfigError> {
    if slot.is_some() {
        return Err(ConfigError::Repeated { flag });
    }
    *slot = Some(value);
    Ok(())
}

/// Accepts only `127.0.0.1:<port>` as the value of `flag`. `localhost`, `::1`,
/// other `127.x` addresses and every non-loopback address are refused: clients
/// connect to one address, `127.0.0.1` (design, "Configuration and binding").
pub fn parse_listen(flag: &'static str, value: &OsString) -> Result<SocketAddrV4, ConfigError> {
    let text = value.to_string_lossy();
    let address: SocketAddr = text.parse().map_err(|_| ConfigError::InvalidListen {
        flag,
        value: text.clone().into_owned(),
    })?;
    match address {
        SocketAddr::V4(v4) if *v4.ip() == Ipv4Addr::LOCALHOST => Ok(v4),
        address => Err(ConfigError::NotLoopback { flag, address }),
    }
}

/// The per-user data directory (see the module docs for each platform).
pub fn default_data_dir() -> Result<PathBuf, ConfigError> {
    let strategy = choose_native_strategy(AppStrategyArgs {
        top_level_domain: String::new(),
        author: String::new(),
        app_name: APP_NAME.to_owned(),
    })
    .map_err(|_| ConfigError::NoDataDir)?;
    Ok(strategy.data_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, ConfigError> {
        parse_args(args.iter().map(OsString::from))
    }

    fn run(args: &[&str]) -> Config {
        match parse(args).unwrap() {
            Command::Run(config) => config,
            Command::Help => panic!("expected a config"),
        }
    }

    #[test]
    fn defaults_to_loopback_port_zero() {
        let config = run(&["--data-dir", "d"]);

        assert_eq!(config.listen, "127.0.0.1:0".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("d"));
        assert_eq!(config.proxy_listen, None);
        assert_eq!(config.proxy_upstream.as_str(), Upstream::ANTHROPIC);
    }

    #[test]
    fn accepts_127_0_0_1() {
        assert_eq!(
            run(&["--listen", "127.0.0.1:4100", "--data-dir", "d"]).listen,
            "127.0.0.1:4100".parse().unwrap()
        );
    }

    #[test]
    fn flag_and_value_are_separate_arguments() {
        assert_eq!(
            parse(&["--listen=127.0.0.1:4101"]),
            Err(ConfigError::UnknownArgument(
                "--listen=127.0.0.1:4101".to_owned()
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_may_be_any_path() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"d\xff".to_vec());

        let command = parse_args([OsString::from("--data-dir"), path.clone()]).unwrap();

        assert_eq!(
            command,
            Command::Run(Config {
                listen: DEFAULT_LISTEN,
                data_dir: PathBuf::from(path),
                proxy_listen: None,
                proxy_upstream: Upstream::parse(Upstream::ANTHROPIC).unwrap(),
            })
        );
    }

    #[test]
    fn refuses_every_other_address() {
        for flag in [LISTEN, PROXY_LISTEN] {
            for value in [
                "0.0.0.0:1",
                "127.0.0.2:1",
                "10.0.0.1:1",
                "[::1]:1",
                "[::]:1",
            ] {
                assert!(
                    matches!(
                        parse(&[flag, value, "--data-dir", "d"]),
                        Err(ConfigError::NotLoopback { flag: refused, .. }) if refused == flag
                    ),
                    "{flag} {value}"
                );
            }
            for value in [
                "localhost:1",
                "::1",
                "127.0.0.1",
                "garbage",
                "",
                "127.0.0.1:99999",
            ] {
                assert_eq!(
                    parse(&[flag, value, "--data-dir", "d"]),
                    Err(ConfigError::InvalidListen {
                        flag,
                        value: value.to_owned()
                    }),
                    "{flag} {value}"
                );
            }
        }
    }

    #[test]
    fn refuses_unknown_missing_repeated_and_empty() {
        assert_eq!(
            parse(&["--port", "1"]),
            Err(ConfigError::UnknownArgument("--port".to_owned()))
        );
        assert_eq!(
            parse(&["--listen"]),
            Err(ConfigError::MissingValue { flag: LISTEN })
        );
        assert_eq!(
            parse(&["--data-dir", "a", "--data-dir", "b"]),
            Err(ConfigError::Repeated { flag: DATA_DIR })
        );
        assert_eq!(parse(&["--data-dir", ""]), Err(ConfigError::EmptyDataDir));
        assert_eq!(
            parse(&["--proxy-upstream"]),
            Err(ConfigError::MissingValue {
                flag: PROXY_UPSTREAM
            })
        );
        assert_eq!(
            parse(&[
                "--proxy-listen",
                "127.0.0.1:1",
                "--proxy-listen",
                "127.0.0.1:2"
            ]),
            Err(ConfigError::Repeated { flag: PROXY_LISTEN })
        );
    }

    #[test]
    fn arguments_are_read_in_order_up_to_help() {
        assert_eq!(
            parse(&["--listen", "0.0.0.0:1", "--help"]),
            Err(ConfigError::NotLoopback {
                flag: LISTEN,
                address: "0.0.0.0:1".parse().unwrap()
            })
        );
        assert_eq!(
            parse(&["--help", "--listen", "0.0.0.0:1"]),
            Ok(Command::Help)
        );
    }

    #[test]
    fn proxy_flags_are_read() {
        let config = run(&[
            "--proxy-listen",
            "127.0.0.1:4200",
            "--proxy-upstream",
            "http://127.0.0.1:9/base",
            "--data-dir",
            "d",
        ]);

        assert_eq!(config.proxy_listen, Some("127.0.0.1:4200".parse().unwrap()));
        assert_eq!(config.proxy_upstream.as_str(), "http://127.0.0.1:9/base");
        assert_eq!(
            run(&["--proxy-listen", "127.0.0.1:0", "--data-dir", "d"]).proxy_listen,
            Some("127.0.0.1:0".parse().unwrap())
        );
    }

    #[test]
    fn a_refused_upstream_is_a_usage_error_that_does_not_repeat_it() {
        for value in [
            "http://example.com",
            "ftp://127.0.0.1",
            "https://user:hunter2@api.example.com",
            "https://api.example.com/?key=hunter2",
            "not a url hunter2",
        ] {
            let error = parse(&["--proxy-upstream", value, "--data-dir", "d"]).unwrap_err();

            assert!(
                matches!(error, ConfigError::InvalidUpstream(_)),
                "{value}: {error}"
            );
            let message = error.to_string();
            assert!(message.starts_with("--proxy-upstream: "), "{message}");
            assert!(!message.contains("hunter2"), "{message}");
        }
    }

    #[test]
    fn messages_name_the_flag_and_what_listens() {
        assert_eq!(
            parse(&["--proxy-listen", "0.0.0.0:1"])
                .unwrap_err()
                .to_string(),
            "--proxy-listen 0.0.0.0:1: the proxy only listens on 127.0.0.1"
        );
        assert_eq!(
            parse(&["--listen", "0.0.0.0:1"]).unwrap_err().to_string(),
            "--listen 0.0.0.0:1: the control API only listens on 127.0.0.1"
        );
    }

    #[test]
    fn default_data_dir_ends_with_the_app_name() {
        // Only when a home directory exists (it does on CI and dev machines).
        let Ok(dir) = default_data_dir() else { return };
        let text = dir.to_string_lossy().to_lowercase();
        assert!(text.contains("callsheet"), "{}", dir.display());
    }
}
