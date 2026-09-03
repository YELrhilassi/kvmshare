//! Leveled logging for both binaries.
//!
//! One tiny logger, no framework: each line is
//! `HH:MM:SS LEVEL kvmshare-server: message` on stderr. Levels are
//! `error`, `warn`, `info` (default) and `debug`. The level comes from
//! the `--log-level` argument (or `KVMSHARE_LOG` env var), so operators
//! choose how chatty the log tail is. The GUI's notification watcher and
//! log tail just read these lines.
//!
//! The component name (kvmshare-server / kvmshare-client) is taken from
//! the binary's own file name at [`init`], so one set of macros serves
//! both processes.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Severity of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    /// Parse a level name (case-insensitive). Unknown names are an error
    /// so a typo surfaces instead of silently defaulting.
    pub fn parse(s: &str) -> Result<Level, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Ok(Level::Error),
            "warn" | "warning" => Ok(Level::Warn),
            "info" => Ok(Level::Info),
            "debug" => Ok(Level::Debug),
            other => Err(format!("unknown log level {other:?} (use error, warn, info or debug)")),
        }
    }
}

static LEVEL: Mutex<Level> = Mutex::new(Level::Info);
static COMPONENT: OnceLock<String> = OnceLock::new();

/// Set the process-wide log level and component (from argv[0]'s file
/// name). Called once at startup with the resolved level; safe to call
/// again (later calls win). Returns an error for bad level names.
pub fn init(level: &str) -> Result<(), String> {
    let lvl = Level::parse(level)?;
    *LEVEL.lock().unwrap() = lvl;
    let component = std::env::args()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "kvmshare".into());
    let _ = COMPONENT.set(component);
    Ok(())
}

/// The active level (defaults to `Info` before [`init`]).
pub fn level() -> Level {
    *LEVEL.lock().unwrap()
}

/// The level an operator asked for, or `info` when unset — feed this to
/// [`init`]. `KVMSHARE_LOG` is the single env knob.
pub fn level_from_env_or_default() -> String {
    std::env::var("KVMSHARE_LOG").unwrap_or_else(|_| "info".into())
}

/// Emit one line if `lvl` is enabled. Exposed for the macros below.
pub fn write_line(lvl: Level, args: std::fmt::Arguments<'_>) {
    if lvl > level() {
        return;
    }
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let component = COMPONENT.get().map(String::as_str).unwrap_or("kvmshare");
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "{h:02}:{m:02}:{s:02} {} {component}: {}", lvl.label(), args);
}

/// `log_error!`, `log_warn!`, `log_info!`, `log_debug!` — available as
/// `kvmshare_app::log_*` in both binaries.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::write_line($crate::log::Level::Error, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::write_line($crate::log::Level::Warn, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::write_line($crate::log::Level::Info, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::log::write_line($crate::log::Level::Debug, format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parsing() {
        assert_eq!(Level::parse("info").unwrap(), Level::Info);
        assert_eq!(Level::parse("DEBUG").unwrap(), Level::Debug);
        assert_eq!(Level::parse(" warning ").unwrap(), Level::Warn);
        assert!(Level::parse("verbose").is_err());
    }

    #[test]
    fn ordering() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
    }
}