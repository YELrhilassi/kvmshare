//! The application layer: config, argument parsing, clipboard polling,
//! session construction, and the role guard — everything above the
//! core/platform crates that the `kvmshare-server` and `kvmshare-client`
//! binaries share.

pub mod guard;

mod args;
mod audio_devices;
mod audio_state;
mod audio_test;
mod client_state;
mod clipboard;
mod config;
mod hostname;
mod machine_id;
mod session;
mod trust;

pub use args::{
    parse_client_args, parse_server_args, with_default_port, AudioTestRequest, BUILD_ID, ClientArgs,
    DEFAULT_TEST_SECONDS, PKG_VERSION, ServerArgs,
};
pub use audio_devices::audio_devices_json;
pub use audio_state::{audio_status_sink, clear_audio_state};
pub use audio_test::audio_test_tone_json;
pub use client_state::{
    write_client_state, write_client_state_connected, write_client_state_refused,
    write_client_state_stopped,
};
pub use trust::{policy_from_revoked_list, revoked_policy, revoked_policy_from_env, REVOKED_ENV, REVOKED_FILE};
pub use clipboard::spawn_server_clipboard;
pub use config::{
    client_config_path, default_config_path, set_id, ClientConfig, Config, NetworkConfig,
    ScreenConfig, DEFAULT_PORT, DEFAULT_SCREEN_H, DEFAULT_SCREEN_W,
};
pub use guard::state_dir;
pub use hostname::hostname;
pub use machine_id::machine_id;
pub use session::session_from_config;