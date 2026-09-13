//! Command-line argument parsing for the two binaries (deliberately
//! small; see the README for the GUI).

use std::path::PathBuf;

/// This binary's build id (see build.rs): a 64-bit hash of the build
/// environment, printed by `--version` and compared by the GUI's
/// install check — the binaries of one build all carry the same id.
pub const BUILD_ID: &str = env!("KVMSHARE_BUILD_ID");

/// The binary's package version, for `--version` output.
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Print the version banner and exit 0: the GUI's install check and a
/// human diagnosing an install both need a one-line, machine-parsable
/// answer to "what is this binary?".
fn print_version(bin: &str) -> ! {
    println!("{bin} {PKG_VERSION} (build {BUILD_ID})");
    std::process::exit(0);
}

/// Arguments for `kvmshare-server`.
pub struct ServerArgs {
    pub config: Option<PathBuf>,
    pub port: u16,
    pub log_level: Option<String>,
    /// Log-control file path (GUI writes it; hot-reloaded by the logger).
    pub log_ctl: Option<PathBuf>,
}

/// Parse `kvmshare-server [--config PATH] [--port N] [--log-level LEVEL] [--logctl PATH]`.
pub fn parse_server_args() -> Result<ServerArgs, String> {
    let mut config: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut log_level: Option<String> = None;
    let mut log_ctl: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?)),
            "--port" | "-p" => {
                let raw = args.next().ok_or("--port needs a number")?;
                port = Some(raw.parse().map_err(|_| format!("bad port {raw:?}"))?);
            }
            "--log-level" | "-l" => log_level = Some(args.next().ok_or("--log-level needs a value")?),
            "--logctl" => log_ctl = Some(PathBuf::from(args.next().ok_or("--logctl needs a path")?)),
            "--version" | "-V" => print_version("kvmshare-server"),
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-server [--config PATH] [--port N] [--log-level error|warn|info|debug|trace] [--logctl PATH]\n       kvmshare-server --version"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ServerArgs { config, port: port.unwrap_or(0), log_level, log_ctl })
}

/// Arguments for `kvmshare-client`.
pub struct ClientArgs {
    pub server_addr: String,
    pub name: Option<String>,
    pub log_level: Option<String>,
    /// Log-control file path (GUI writes it; hot-reloaded by the logger).
    pub log_ctl: Option<PathBuf>,
}

/// Parse `kvmshare-client SERVER[:PORT] [--name NAME] [--log-level LEVEL] [--logctl PATH]`.
pub fn parse_client_args() -> Result<ClientArgs, String> {
    let mut addr: Option<String> = None;
    let mut name: Option<String> = None;
    let mut log_level: Option<String> = None;
    let mut log_ctl: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--name" | "-n" => name = Some(args.next().ok_or("--name needs a value")?),
            "--log-level" | "-l" => log_level = Some(args.next().ok_or("--log-level needs a value")?),
            "--logctl" => log_ctl = Some(PathBuf::from(args.next().ok_or("--logctl needs a path")?)),
            "--version" | "-V" => print_version("kvmshare-client"),
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-client SERVER[:PORT] [--name NAME] [--log-level error|warn|info|debug|trace] [--logctl PATH]\n       kvmshare-client --version"
                );
                std::process::exit(0);
            }
            // `--server` / `--connect` are accepted as aliases for the
            // positional address, so `kvmshare-client --server HOST`
            // works like the documented `kvmshare-client HOST` (the
            // GUI passes the position, but a human typing the flag
            // name should not be told the argument is unknown).
            "--server" | "--connect" if addr.is_none() => {
                addr = Some(args.next().ok_or("--server needs an address")?);
            }
            // A flag-looking token may never be swallowed as the
            // positional address: the first flag-shaped argument after
            // the address used to become it (connect-for-ever to a
            // host literally named "--name"), and the error surfaced
            // only as a DNS failure three seconds later.
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown argument {other:?}"))
            }
            other if addr.is_none() => addr = Some(other.to_owned()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ClientArgs { server_addr: addr.ok_or("missing server address (use `kvmshare-client HOST[:PORT]`)")?, name, log_level, log_ctl })
}

/// Normalize `host` or `host:port` to `host:port` (default port).
pub fn with_default_port(host: &str, default_port: u16) -> String {
    if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:{default_port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_addr_normalization() {
        assert_eq!(with_default_port("pc", 24800), "pc:24800");
        assert_eq!(with_default_port("pc:1234", 24800), "pc:1234");
        assert_eq!(with_default_port("192.168.1.69", 24800), "192.168.1.69:24800");
    }

    #[test]
    fn build_id_is_stamped() {
        // build.rs must have stamped a 16-hex-digit id; `--version` and
        // the GUI's install check both read it.
        assert_eq!(BUILD_ID.len(), 16);
        assert!(BUILD_ID.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}