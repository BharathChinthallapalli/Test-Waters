//! The daemon's panic hook logs where a panic happened, never its message
//! (`cs_daemon::logging::install_panic_hook`).
//!
//! Its own test binary: the hook is process-wide.

use std::io::Write;
use std::sync::{Arc, Mutex};

use cs_daemon::logging;
use tracing::level_filters::LevelFilter;

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_panic_is_logged_with_its_location_and_without_its_message() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = logging::subscriber(LevelFilter::TRACE, move || writer.clone());

    let unwound = tracing::subscriber::with_default(subscriber, || {
        logging::install_panic_hook();
        std::panic::catch_unwind(|| panic!("quoted body text: {}", "SECRET-PANIC-PAYLOAD"))
    });
    let _ = std::panic::take_hook();

    assert!(unwound.is_err(), "the panic still unwinds");
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("a thread panicked"), "{logs}");
    assert!(logs.contains("panic_hook.rs:"), "{logs}");
    assert!(!logs.contains("SECRET-PANIC-PAYLOAD"), "{logs}");
    assert!(!logs.contains("quoted body text"), "{logs}");
}
