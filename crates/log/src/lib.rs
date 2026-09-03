//! Leveled logging shared by every kvmshare crate.
//!
//! One tiny logger, no framework. Every line is
//! `HH:MM:SS LEVEL component: message` on stderr — the GUI spawns the
//! binaries with stderr pointed at the role's log file, so that is
//! exactly what gets tailed.
//!
//! Levels, quietest first: `error`, `warn`, `info` (default), `debug`,
//! `trace` (very verbose, per-event).
//!
//! ## Live control (hot reload)
//!
//! The level and the enabled flag can change *without restarting*: a
//! small control file (written by the GUI) is polled and applied live.
//! The file is plain `key=value`, one per line:
//!
//! ```text
//! level=debug
//! enabled=1
//! ```
//!
//! A missing file, or a file with no `level`/`enabled` keys, leaves the
//! current settings untouched — so the GUI only ever writes what it
//! wants to change. `enabled=0` silences everything (used as an
//! operator override); `level=` accepts any of the level names above.
//!
//! The control file also wins at startup: [`init`] applies it
//! immediately, so a level chosen in the GUI survives process restarts.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Severity of a log line. Ordering is `Error < Warn < Info < Debug <
/// Trace`; a line is written when its level is <= the configured level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
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
            "trace" | "verbose" | "veryverbose" => Ok(Level::Trace),
            other => Err(format!(
                "unknown log level {other:?} (use error, warn, info, debug or trace)"
            )),
        }
    }

    /// All levels, quietest first — the GUI's selector options.
    pub const ALL: [Level; 5] = [Level::Error, Level::Warn, Level::Info, Level::Debug, Level::Trace];
}

static LEVEL: Mutex<Level> = Mutex::new(Level::Info);
static ENABLED: Mutex<bool> = Mutex::new(true);
static COMPONENT: OnceLock<String> = OnceLock::new();
/// The control file currently being watched (re-targetable: a later
/// [`init`] with a different path points the watcher at it).
static CONTROL: Mutex<Option<PathBuf>> = Mutex::new(None);

/// How often the control file is re-read. Cheap (one tiny file), and
/// 400 ms keeps level changes feeling instant.
const CONTROL_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// Configure the process-wide logger.
///
/// `level` is the startup level (from `--log-level` or `KVMSHARE_LOG`);
/// when `control` is given it is applied immediately and then watched
/// for live changes. The component name comes from the executable's own
/// file name (`kvmshare-server`, `kvmshare-client`, …), so one set of
/// macros serves every binary. Safe to call more than once; later calls
/// win.
pub fn init(level: &str, control: Option<PathBuf>) -> Result<(), String> {
    let lvl = Level::parse(level)?;
    *LEVEL.lock().unwrap() = lvl;
    let component = std::env::args()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "kvmshare".into());
    let _ = COMPONENT.set(component);
    start_watcher(control)
}

/// The level an operator asked for, or `info` when unset — feed this to
/// [`init`]. `KVMSHARE_LOG` is the single env knob.
pub fn level_from_env_or_default() -> String {
    std::env::var("KVMSHARE_LOG").unwrap_or_else(|_| "info".into())
}

/// The active level (defaults to `Info` before [`init`]).
pub fn level() -> Level {
    *LEVEL.lock().unwrap()
}

/// Whether logging is currently enabled (defaults to true).
pub fn enabled() -> bool {
    *ENABLED.lock().unwrap()
}

/// Change the level at runtime (the control-file watcher calls this;
/// exposed for tests).
pub fn set_level(lvl: Level) {
    *LEVEL.lock().unwrap() = lvl;
}

/// Enable or disable all logging at runtime.
pub fn set_enabled(on: bool) {
    *ENABLED.lock().unwrap() = on;
}

/// Emit one line if logging is enabled and `lvl` is within the current
/// level. Exposed for the macros below.
pub fn write_line(lvl: Level, args: std::fmt::Arguments<'_>) {
    if !enabled() || lvl > level() {
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

/// `log_error!`, `log_warn!`, `log_info!`, `log_debug!`, `log_trace!` —
/// usable as `kvmshare_log::log_*!` from every crate in the workspace.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::write_line($crate::Level::Error, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::write_line($crate::Level::Warn, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::write_line($crate::Level::Info, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::write_line($crate::Level::Debug, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => { $crate::write_line($crate::Level::Trace, format_args!($($arg)*)) };
}

// ---------------------------------------------------------------------------
// Control-file watcher
// ---------------------------------------------------------------------------

fn start_watcher(control: Option<PathBuf>) -> Result<(), String> {
    *CONTROL.lock().unwrap() = control;
    // Apply the file immediately if present, so a configured level is in
    // effect from the very first line.
    apply_control_file();
    // One background thread for the process lifetime; it re-reads the
    // (possibly re-targeted) path every tick.
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(CONTROL_POLL);
            apply_control_file();
        });
    });
    Ok(())
}

/// Read the current control file (if any) and apply `level=`/`enabled=`.
/// Missing file or malformed lines leave the matching setting untouched.
fn apply_control_file() {
    let path = match CONTROL.lock().unwrap().clone() {
        Some(p) => p,
        None => return,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return, // not written yet / deleted — keep current settings
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "level" => {
                if let Ok(lvl) = Level::parse(value) {
                    set_level(lvl);
                }
            }
            "enabled" => match value.trim() {
                "0" | "false" | "off" => set_enabled(false),
                "1" | "true" | "on" => set_enabled(true),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parsing() {
        assert_eq!(Level::parse("info").unwrap(), Level::Info);
        assert_eq!(Level::parse("DEBUG").unwrap(), Level::Debug);
        assert_eq!(Level::parse(" warning ").unwrap(), Level::Warn);
        assert_eq!(Level::parse("trace").unwrap(), Level::Trace);
        assert_eq!(Level::parse("verbose").unwrap(), Level::Trace);
        assert!(Level::parse("loud").is_err());
    }

    #[test]
    fn ordering() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    // The tests below mutate process-global logger state, so they must
    // run one at a time — cargo runs tests in parallel threads otherwise.
    #[test]
    fn control_file_and_enabled_state() {
        let path = std::env::temp_dir().join(format!("kvmshare-logctl-test-{}", std::process::id()));
        set_enabled(true);
        set_level(Level::Info);

        std::fs::write(&path, "level=debug\nenabled=0\n").unwrap();
        *CONTROL.lock().unwrap() = Some(path.clone());
        apply_control_file();
        assert_eq!(level(), Level::Debug);
        assert!(!enabled(), "enabled=0 must disable logging");

        // Re-enable and quieten — hot reload both ways.
        std::fs::write(&path, "level=warn\nenabled=1\n").unwrap();
        apply_control_file();
        assert_eq!(level(), Level::Warn);
        assert!(enabled());

        // A missing file keeps the current settings.
        std::fs::remove_file(&path).ok();
        apply_control_file();
        assert_eq!(level(), Level::Warn);

        // Garbage lines are ignored.
        std::fs::write(&path, "nonsense\nlevel=not-a-level\nenabled=maybe\n").unwrap();
        apply_control_file();
        assert_eq!(level(), Level::Warn);
        assert!(enabled());

        // Disabled logger: no panic, state stays put (write_line no-ops).
        set_enabled(false);
        set_level(Level::Trace);
        write_line(Level::Error, format_args!("boom"));
        assert!(!enabled());
        set_enabled(true);
        assert!(enabled());
    }
}