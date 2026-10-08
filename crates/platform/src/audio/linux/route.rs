//! Exclusive output routing: making a machine that *sends* its sound go
//! quiet locally without losing the sound.
//!
//! # The problem
//!
//! Capturing a sink's `.monitor` is a *tap*, not a redirect: the sound
//! keeps going to the speakers as well as to us. So a machine sharing its
//! audio stayed audible on its own speakers, and with a peer playing the
//! same stream both machines made the same noise — exactly the state the
//! user calls "I cannot tell which one is playing".
//!
//! # The fix
//!
//! A **virtual output**. While sharing, kvmshare loads a null sink
//! (`module-null-sink`, which has no hardware and therefore makes no
//! sound), makes it the system default, and captures *its* monitor. Every
//! application now plays into the virtual output: the real speakers are
//! silent, and the stream carries the whole system's sound. When sharing
//! stops the real output is made default again and the module is unloaded.
//!
//! # Why this is done at the default sink rather than per stream
//!
//! Moving only the streams that exist right now would leave every *new*
//! sound (a notification, a track the user starts) back on the speakers.
//! Making the virtual output the default is what makes the behaviour
//! seamless: it holds for sound that has not been played yet.
//!
//! # Interruption
//!
//! Changing the default output is a real, visible side effect, so it is
//! always undone: on a clean stop, and on the next start after a crash
//! (the sink and the output it replaced are both recoverable — see
//! [`recover`]). A machine is never left silent without a running kvmshare
//! to explain why.

use std::path::PathBuf;

use kvmshare_log::{log_debug, log_warn};

use super::{default_sink, monitor_of, parse_pactl_names, run, PACTL};

/// The virtual output that carries a sending machine's sound while it is
/// sharing. Named, not anonymous, so a user can see it in their mixer and
/// find this code by searching for it.
pub const SEND_SINK: &str = "kvmshare_send";

/// The monitor source of [`SEND_SINK`] — the device capture must read while
/// the route is live. It is always the same name, because the sink always
/// has the same name.
pub fn capture_source() -> String {
    monitor_of(SEND_SINK)
}

/// A live route: the virtual output exists, is the default, and `module`
/// is the module that owns it.
pub struct Route {
    /// The sink that was default before, restored on release.
    previous: String,
    /// The `module-null-sink` instance id, unloaded on release.
    module: u32,
}

impl Route {
    /// Undo the route: put the real output back in charge and retire the
    /// virtual one. Best-effort by design — this runs on stop and on drop,
    /// where an error has nowhere useful to go — but never silent about a
    /// failure, so a machine left quiet is at least explained in the log.
    pub fn release(self) {
        // Point *new* sound at the real output first, so the instant the
        // virtual sink disappears applications have somewhere to go.
        if let Err(e) = run(PACTL, &["set-default-sink", &self.previous]) {
            log_warn!("audio: could not restore the default output to {} ({e})", self.previous);
        }
        // Streams still on the virtual sink are moved to the default as it
        // goes away, which is why the sink is unloaded rather than muted.
        if let Err(e) = run(PACTL, &["unload-module", &self.module.to_string()]) {
            log_warn!("audio: could not remove the share output ({e})");
        }
        forget_previous();
    }
}

/// The id of the module whose arguments create `sink_name=<sink>`, from the
/// `pactl list short modules` reply. Pure, so the parsing is pinned by a
/// test rather than depending on the machine's loaded modules.
fn parse_module_owner(list: &str, sink: &str) -> Option<u32> {
    let needle = format!("sink_name={sink}");
    for line in list.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(id) = parts.next().and_then(|s| s.trim().parse::<u32>().ok()) else {
            continue;
        };
        let _name = parts.next();
        let args = parts.next().unwrap_or("");
        // Match the argument exactly: `sink_name=kvmshare_send` must not
        // match `sink_name=kvmshare_send_backup`.
        if args.split_whitespace().any(|token| token == needle) {
            return Some(id);
        }
    }
    None
}

/// The ids of every sink-input, from `pactl list short sink-inputs`.
fn parse_sink_input_ids(list: &str) -> Vec<u32> {
    list.lines()
        .filter_map(|line| line.split('\t').next())
        .filter_map(|id| id.trim().parse::<u32>().ok())
        .collect()
}

/// Is the virtual output present right now?
fn send_sink_module() -> Option<u32> {
    let list = run(PACTL, &["list", "short", "modules"]).ok()?;
    parse_module_owner(&list, SEND_SINK)
}

/// The first output that is not our virtual one — the sensible fallback
/// when the recorded previous output is gone.
fn first_real_sink() -> Option<String> {
    parse_pactl_names(&run(PACTL, &["list", "short", "sinks"]).ok()?)
        .into_iter()
        .find(|name| name != SEND_SINK)
}

/// Where the previous default output is remembered, so an interrupted run
/// can be repaired. `XDG_RUNTIME_DIR` is the right home for it: it names
/// state for this login session, which is exactly the lifetime of the
/// PulseAudio server the record describes.
fn record_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("kvmshare-send-sink"))
}

fn remember_previous(sink: &str) {
    if let Some(path) = record_path() {
        let _ = std::fs::write(path, sink);
    }
}

fn read_previous() -> Option<String> {
    let raw = std::fs::read_to_string(record_path()?).ok()?;
    let name = raw.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn forget_previous() {
    if let Some(path) = record_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Make the virtual output the default and move everything already playing
/// onto it. Public so the sequence is exercised as a whole where it can be,
/// and split from [`takeover`] so a failure can be undone in one place.
fn point_everything_at(sink: &str) -> Result<(), String> {
    run(PACTL, &["set-default-sink", sink])?;
    // Sound that was already playing would otherwise keep going to the
    // speakers while the link carried nothing.
    match run(PACTL, &["list", "short", "sink-inputs"]) {
        Ok(list) => {
            for id in parse_sink_input_ids(&list) {
                let _ = run(PACTL, &["move-sink-input", &id.to_string(), sink]);
            }
        }
        Err(e) => log_debug!("audio: could not list playing streams to move ({e})"),
    }
    Ok(())
}

/// Create the virtual output and make it the default. The caller captures
/// [`capture_source`] while the returned [`Route`] is alive.
pub fn takeover() -> Result<Route, String> {
    // A leftover from an interrupted run would otherwise stack a second
    // virtual sink on top of the first.
    recover();

    let previous = default_sink()?;
    if previous == SEND_SINK {
        // Recover should have handled this; if it did not, there is no
        // real output name to remember and routing would be a trap.
        return Err("the share output is already the default output".to_string());
    }

    let module = run(
        PACTL,
        &[
            "load-module",
            "module-null-sink",
            &format!("sink_name={SEND_SINK}"),
            &format!("sink_properties=device.description={SEND_SINK}"),
            "rate=48000",
            "channel_map=front-left,front-right",
        ],
    )?
    .trim()
    .parse::<u32>()
    .map_err(|e| format!("could not read the new output's module id: {e}"))?;

    // Recorded *before* the default changes, so a crash between these two
    // steps is still repairable.
    remember_previous(&previous);

    if let Err(e) = point_everything_at(SEND_SINK) {
        // Never leave the machine silent because routing half-worked.
        Route { previous, module }.release();
        return Err(e);
    }

    log_debug!("audio: sharing output routed through {SEND_SINK} (was {previous})");
    Ok(Route { previous, module })
}

/// Repair an interrupted route: if the virtual output is present (we did
/// not clean up, so a previous run died mid-share), put the real output
/// back and remove it.
pub fn recover() {
    let Some(module) = send_sink_module() else {
        // Nothing to undo; a record with no sink is stale.
        forget_previous();
        return;
    };
    let previous = read_previous()
        .or_else(first_real_sink)
        .unwrap_or_default();
    if !previous.is_empty() && previous != SEND_SINK {
        if let Err(e) = run(PACTL, &["set-default-sink", &previous]) {
            log_warn!("audio: could not restore {previous} as the default output ({e})");
        } else {
            log_warn!(
                "audio: an earlier share was interrupted; the default output is restored to {previous}"
            );
        }
    }
    if let Err(e) = run(PACTL, &["unload-module", &module.to_string()]) {
        log_warn!("audio: could not remove a leftover share output ({e})");
    }
    forget_previous();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module-owner lookup reads the third, tab-separated field and
    /// matches the argument exactly — a prefix match would unload an
    /// unrelated sink whose name happens to start the same way.
    #[test]
    fn the_share_module_is_found_by_its_exact_argument() {
        let list = "12\tmodule-null-sink\tsink_name=kvmshare_send sink_properties=device.description=kvmshare_send rate=48000\n\
                    13\tmodule-null-sink\tsink_name=kvmshare_send_backup rate=48000\n\
                    14\tmodule-alsa-card\tdevice_id=0\n";
        assert_eq!(parse_module_owner(list, SEND_SINK), Some(12));
        assert_eq!(parse_module_owner(list, "kvmshare_send_backup"), Some(13));
        assert_eq!(parse_module_owner(list, "not_loaded"), None);
    }

    /// A reply that is not a module list (a header, an error, a short line)
    /// yields nothing rather than a bogus id.
    #[test]
    fn a_module_list_that_is_not_one_yields_nothing() {
        assert_eq!(parse_module_owner("", SEND_SINK), None);
        assert_eq!(parse_module_owner("Failure: No such entity\n", SEND_SINK), None);
        assert_eq!(parse_module_owner("12\tmodule-null-sink\n", SEND_SINK), None);
    }

    /// The input ids are the first field of `pactl list short sink-inputs`.
    #[test]
    fn sink_input_ids_are_read_from_the_first_field() {
        let list = "538\t3\t0\ts16le 2ch 48000Hz\tRUNNING\n539\t3\t0\ts16le 2ch 48000Hz\tIDLE\n";
        assert_eq!(parse_sink_input_ids(list), vec![538, 539]);
        assert!(parse_sink_input_ids("").is_empty());
    }

    /// The capture source is the virtual sink's monitor, by name.
    #[test]
    fn the_capture_source_is_the_virtual_outputs_monitor() {
        assert_eq!(capture_source(), "kvmshare_send.monitor");
    }
}
