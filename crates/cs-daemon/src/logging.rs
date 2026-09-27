//! Structured logs (R1.6).
//!
//! Owned by unit `daemon-proc`: `tracing-subscriber` writes one JSON object per
//! event to stderr. The level comes from the `CALLSHEET_LOG` environment variable
//! (`off`, `error`, `warn`, `info`, `debug`, `trace` or `0`–`5`, parsed by
//! `tracing::level_filters::LevelFilter`'s `FromStr`, tracing-core 0.1.36
//! `src/metadata.rs`), `info` when unset or empty. There are no per-target
//! filters: the `env-filter` feature isn't enabled.
//!
//! No request or response bodies and no header values are ever logged, at any
//! level: the HTTP layer logs only the fact of a rejection and the peer
//! (`crate::http`), the server only connection errors (`crate::serve`), and
//! `hyper` is built without its `tracing` feature. The daemon's tests capture the
//! output while requests carry a known token and assert it never appears.

use std::fmt;

use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;

/// The environment variable that sets the log level.
pub const LOG_ENV: &str = "CALLSHEET_LOG";

/// The level when [`LOG_ENV`] is unset or empty.
pub const DEFAULT_LEVEL: LevelFilter = LevelFilter::INFO;

/// [`LOG_ENV`] holds something that isn't a level. The value isn't repeated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLevel;

impl fmt::Display for InvalidLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{LOG_ENV} must be one of off, error, warn, info, debug or trace"
        )
    }
}

impl std::error::Error for InvalidLevel {}

/// The level named by [`LOG_ENV`] in this process's environment.
pub fn level_from_env() -> Result<LevelFilter, InvalidLevel> {
    match std::env::var_os(LOG_ENV) {
        None => parse_level(None),
        // A value that isn't UTF-8 can't name a level.
        Some(value) => parse_level(Some(value.to_str().ok_or(InvalidLevel)?)),
    }
}

/// `None` or an empty string is [`DEFAULT_LEVEL`].
pub fn parse_level(value: Option<&str>) -> Result<LevelFilter, InvalidLevel> {
    match value.map(str::trim) {
        None | Some("") => Ok(DEFAULT_LEVEL),
        Some(value) => value.parse().map_err(|_| InvalidLevel),
    }
}

/// The daemon's subscriber: JSON events up to `level`, written to `writer`.
/// Tests pass a buffer; [`init`] passes stderr.
pub fn subscriber<W>(level: LevelFilter, writer: W) -> impl Subscriber + Send + Sync + 'static
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .with_max_level(level)
        .with_writer(writer)
        .finish()
}

/// Installs the JSON-to-stderr subscriber for the whole process. Fails if one is
/// already installed.
pub fn init(level: LevelFilter) -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
    tracing::subscriber::set_global_default(subscriber(level, std::io::stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_is_info() {
        assert_eq!(parse_level(None), Ok(LevelFilter::INFO));
        assert_eq!(parse_level(Some("")), Ok(LevelFilter::INFO));
        assert_eq!(parse_level(Some("  ")), Ok(LevelFilter::INFO));
    }

    #[test]
    fn level_names_are_case_insensitive() {
        assert_eq!(parse_level(Some("debug")), Ok(LevelFilter::DEBUG));
        assert_eq!(parse_level(Some("WARN")), Ok(LevelFilter::WARN));
        assert_eq!(parse_level(Some("off")), Ok(LevelFilter::OFF));
    }

    #[test]
    fn anything_else_is_an_error_that_does_not_repeat_it() {
        let error = parse_level(Some("verbose-secret")).unwrap_err();

        assert!(!error.to_string().contains("verbose-secret"));
        assert_eq!(parse_level(Some("6")), Err(InvalidLevel));
    }
}
