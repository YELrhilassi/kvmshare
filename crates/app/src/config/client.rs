//! The client's config file.
//!
//! A client obeys the *server's* layout — screens, positions, shortcuts and
//! the network policy all arrive on the wire, because they describe the
//! shared desktop rather than either machine. What the wire cannot carry is
//! this machine's own hardware and consent, which is exactly `[audio]`:
//! which output to capture, which device to play into, and whether to take
//! part at all.
//!
//! So the client's file is deliberately **only** that section. It is not a
//! [`super::Config`] with the layout left out: a client that could hold a
//! screen list would invite an operator to configure two contradictory
//! layouts on two machines, and the file that lost the argument would be
//! the one nobody could see.

use std::path::Path;

use super::{io, AudioConfig};

/// The client config file: the client role's machine-local settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ClientConfig {
    /// Audio streaming (`[audio]` section). Identical in shape to the
    /// server's — one machine's audio behaviour does not depend on which
    /// role it happens to be playing.
    #[serde(default)]
    pub audio: AudioConfig,
}

impl ClientConfig {
    /// Load the config at `path`, or the defaults when there is no file.
    ///
    /// A **missing** file is not an error: it means "this client takes no
    /// part in audio", which is what every machine that never opened the
    /// settings page wants and the state the feature ships in. A file that
    /// exists but cannot be read is an error the caller reports, so a
    /// typo is never mistaken for a deliberate silence.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Self = toml::from_str(&text)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// Persist the config atomically (temp file + rename) under the shared
    /// config lock, so a save racing a running client's read never leaves
    /// a half-written file. See [`super::Config::save`].
    pub fn save(&self, path: &Path) -> Result<(), String> {
        io::write_toml(path, self)
    }

    /// Reject values that cannot be honoured, so a broken setting is a
    /// load-time error rather than a stream that silently never starts.
    /// Runs on the same rules as the server's `[audio]` section, because it
    /// is the same section.
    pub fn validate(&self) -> Result<(), String> {
        self.audio.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No file at all is the inert default, not an error: this is the
    /// state of every machine that never opened the audio settings.
    #[test]
    fn a_missing_file_is_inert() {
        let cfg = ClientConfig::load(Path::new("/nonexistent/kvmshare-client.toml")).unwrap();
        assert!(!cfg.audio.is_active());
        assert!(!cfg.audio.send);
        assert!(!cfg.audio.receive);
    }

    /// A typo must be visible, not silently read as "audio off" — the
    /// quiet failure this whole feature is written to avoid.
    #[test]
    fn a_broken_file_is_an_error() {
        let dir = std::env::temp_dir().join("kvmshare-client-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.toml");
        std::fs::write(&path, "[audio\nsend = true\n").unwrap();
        assert!(ClientConfig::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// A saved config round-trips, so the settings page shows what the
    /// client will actually do.
    #[test]
    fn a_saved_config_reloads_unchanged() {
        let dir = std::env::temp_dir().join("kvmshare-client-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.toml");
        let cfg = ClientConfig {
            audio: AudioConfig {
                send: true,
                receive: true,
                capture_device: "alsa_output.pci".into(),
                playback_device: "default".into(),
                activity_floor_db: -40.0,
                peer: String::new(),
            },
        };
        cfg.save(&path).unwrap();
        let back = ClientConfig::load(&path).unwrap();
        assert_eq!(back.audio.send, cfg.audio.send);
        assert_eq!(back.audio.receive, cfg.audio.receive);
        assert_eq!(back.audio.capture_device, cfg.audio.capture_device);
        assert_eq!(back.audio.playback_device, cfg.audio.playback_device);
        assert_eq!(back.audio.activity_floor_db, cfg.audio.activity_floor_db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("toml.lock"));
    }

    /// A client file that carries a server section is a mistake, and the
    /// inert default (no audio) is the correct reading of "this file is
    /// not the client's" — but the fields the client *does* own must still
    /// be read, so the shape is checked rather than the whole file
    /// rejected.
    #[test]
    fn an_out_of_place_layout_section_does_not_break_the_client() {
        let dir = std::env::temp_dir().join("kvmshare-client-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stray.toml");
        std::fs::write(&path, "port = 24800\n[[screens]]\nname = \"x\"\n\n[audio]\nsend = true\n")
            .unwrap();
        let cfg = ClientConfig::load(&path).unwrap();
        assert!(cfg.audio.send, "the section the client owns is honoured");
        let _ = std::fs::remove_file(&path);
    }
}
