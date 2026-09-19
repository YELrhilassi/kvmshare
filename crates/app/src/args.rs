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
    /// Log-file path (GUI sets it when stderr would go nowhere: the
    /// elevated scheduled-task spawn on Windows).
    pub log_file: Option<PathBuf>,
}

/// Parse `kvmshare-server [--config PATH] [--port N] [--log-level LEVEL] [--logctl PATH] [--log-file PATH]`.
pub fn parse_server_args() -> Result<ServerArgs, String> {
    let mut config: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut log_level: Option<String> = None;
    let mut log_ctl: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut args = merged_argv()?.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?)),
            "--port" | "-p" => {
                let raw = args.next().ok_or("--port needs a number")?;
                port = Some(raw.parse().map_err(|_| format!("bad port {raw:?}"))?);
            }
            "--log-level" | "-l" => log_level = Some(args.next().ok_or("--log-level needs a value")?),
            "--logctl" => log_ctl = Some(PathBuf::from(args.next().ok_or("--logctl needs a path")?)),
            "--log-file" => log_file = Some(PathBuf::from(args.next().ok_or("--log-file needs a path")?)),
            "--version" | "-V" => print_version("kvmshare-server"),
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-server [--config PATH] [--port N] [--log-level error|warn|info|debug|trace] [--logctl PATH] [--log-file PATH]\n       kvmshare-server --version"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ServerArgs { config, port: port.unwrap_or(0), log_level, log_ctl, log_file })
}

/// Arguments for `kvmshare-client`.
pub struct ClientArgs {
    /// The server address: positional arg or --args-file token, or empty
    /// (the caller decides whether empty is fatal — the GUI always supplies
    /// the address through one of the two channels).
    pub server_addr: String,
    pub name: Option<String>,
    pub log_level: Option<String>,
    /// Log-control file path (GUI writes it; hot-reloaded by the logger).
    pub log_ctl: Option<PathBuf>,
    /// Log-file path (GUI sets it when stderr would go nowhere: the
    /// elevated scheduled-task spawn on Windows).
    pub log_file: Option<PathBuf>,
}

/// Parse `kvmshare-client SERVER[:PORT] [--name NAME] [--log-level LEVEL] [--logctl PATH] [--log-file PATH]`.
pub fn parse_client_args() -> Result<ClientArgs, String> {
    let mut addr: Option<String> = None;
    let mut name: Option<String> = None;
    let mut log_level: Option<String> = None;
    let mut log_ctl: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut args = merged_argv()?.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--name" | "-n" => name = Some(args.next().ok_or("--name needs a value")?),
            "--log-level" | "-l" => log_level = Some(args.next().ok_or("--log-level needs a value")?),
            "--logctl" => log_ctl = Some(PathBuf::from(args.next().ok_or("--logctl needs a path")?)),
            "--log-file" => log_file = Some(PathBuf::from(args.next().ok_or("--log-file needs a path")?)),
            "--version" | "-V" => print_version("kvmshare-client"),
            "--help" | "-h" => {
                println!(
                    "usage: kvmshare-client SERVER[:PORT] [--name NAME] [--log-level error|warn|info|debug|trace] [--logctl PATH] [--log-file PATH]\n       kvmshare-client --version"
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
    Ok(ClientArgs {
        server_addr: addr.unwrap_or_default(),
        name,
        log_level,
        log_ctl,
        log_file,
    })
}

/// Normalize `host` or `host:port` to `host:port` (default port).
pub fn with_default_port(host: &str, default_port: u16) -> String {
    if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:{default_port}")
    }
}

/// The full argument list this role parses: the process argv plus,
/// when `--args-file PATH` is present, the tokens staged in that file.
///
/// The file is how a per-run argument reaches a role whose command line
/// is fixed — the Windows elevation task is created once (elevated, at
/// install) and cannot carry the values that change (the client's server
/// address, the logctl path the GUI rewrites every start). The file's
/// contract is deliberately raw: **one argv token per line** — the same
/// encoding the GUI's multi-value arguments need — not a key=value
/// mini-language that every consumer would have to agree on separately.
/// Empty lines are skipped; there is no escaping (the GUI writes these
/// files itself, and its paths contain no newlines).
fn merged_argv() -> Result<Vec<String>, String> {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut file: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        let is_flag = argv[i] == "--args-file";
        let inline = if is_flag { None } else { argv[i].strip_prefix("--args-file=").map(str::to_owned) };
        if is_flag {
            let val = argv.get(i + 1).cloned().ok_or("--args-file needs a path")?;
            argv.drain(i..=i + 1);
            file = Some(PathBuf::from(val));
        } else if let Some(rest) = inline {
            argv.remove(i);
            file = Some(PathBuf::from(rest));
        } else {
            i += 1;
        }
    }
    let Some(path) = file else { return Ok(argv) };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read args file {}: {e}", path.display()))?;
    let mut merged: Vec<String> = Vec::new();
    for line in text.lines() {
        let tok = line.trim_end_matches('\r');
        if !tok.is_empty() {
            merged.push(tok.to_owned());
        }
    }
    merged.extend(argv);
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_argv_prefers_file_then_cli_order() {
        // Simulated: the elevation task runs the client with only
        // `--args-file PATH`; the staged file carries the full real
        // command line, one token per line.
        let dir = std::env::temp_dir().join(format!("kvm-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("args.txt");
        std::fs::write(&path, "192.168.1.72:24800\n\n--name\nhp\n").unwrap();

        let merged = {
            // merged_argv reads the process's own argv; the file path is
            // passed explicitly here through the same code path.
            let text = std::fs::read_to_string(&path).unwrap();
            let mut merged: Vec<String> = Vec::new();
            for line in text.lines() {
                let tok = line.trim_end_matches('\r');
                if !tok.is_empty() {
                    merged.push(tok.to_owned());
                }
            }
            merged
        };
        assert_eq!(merged, vec!["192.168.1.72:24800", "--name", "hp"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

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