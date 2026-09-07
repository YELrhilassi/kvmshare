//! The application layer: config, argument parsing, clipboard polling,
//! session construction, and the role guard — everything above the
//! core/platform crates that the `kvmshare-server` and `kvmshare-client`
//! binaries share.

pub mod guard;

mod args;
mod clipboard;
mod config;
mod hostname;
mod machine_id;
mod session;

pub use args::{parse_client_args, parse_server_args, with_default_port, ClientArgs, ServerArgs};
pub use clipboard::spawn_server_clipboard;
pub use config::{
    default_config_path, Config, NetworkConfig, ScreenConfig, DEFAULT_PORT, DEFAULT_SCREEN_H,
    DEFAULT_SCREEN_W,
};
pub use guard::state_dir;
pub use hostname::hostname;
pub use machine_id::machine_id;
pub use session::session_from_config;