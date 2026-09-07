//! Command-line argument parsing for the two binaries (deliberately
//! small; see the README for the GUI).

use std::path::PathBuf;

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
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-server [--config PATH] [--port N] [--log-level error|warn|info|debug|trace] [--logctl PATH]"
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
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-client SERVER[:PORT] [--name NAME] [--log-level error|warn|info|debug|trace] [--logctl PATH]"
                );
                std::process::exit(0);
            }
            other if addr.is_none() => addr = Some(other.to_owned()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ClientArgs { server_addr: addr.ok_or("missing server address")?, name, log_level, log_ctl })
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
}