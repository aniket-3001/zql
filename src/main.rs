//! The `zql` command: parse arguments, then hand off to the server.
//!
//! Everything else lives in the library so that the corpora in `tests/` can
//! drive the engine directly.

use std::path::PathBuf;
use std::process::ExitCode;

use zql::server::{Config, Server};

/// Where the dashboard listens when `--dashboard` is given no port.
const DEFAULT_DASHBOARD_PORT: u16 = 8080;

const USAGE: &str = "\
zql — query SQLite files, CSVs and your filesystem with SQL

USAGE:
    zql [DIRECTORY] [OPTIONS]

ARGS:
    DIRECTORY          Directory the `files` source walks [default: .]

OPTIONS:
    -H, --host <HOST>  Address to bind [default: 127.0.0.1]
    -p, --port <PORT>  Port to listen on [default: 5432]
    -d, --dir <DIR>    Same as the positional DIRECTORY argument
        --no-cache     Re-walk the filesystem on every query
        --dashboard    Serve a live query log over HTTP [default port: 8080]
        --dashboard-port <PORT>
                       Port for the dashboard (implies --dashboard)
    -h, --help         Print this help
    -V, --version      Print version

Then point any Postgres client at it:

    psql -h 127.0.0.1 -p 5432

zql is read-only. It never writes to the files it reads.";

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)) {
        Ok(Invocation::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Invocation::Version) => {
            println!("zql {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Invocation::Serve(config)) => match Server::new(config).run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("zql: {err}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("zql: {message}");
            eprintln!("Try `zql --help`.");
            ExitCode::FAILURE
        }
    }
}

enum Invocation {
    Serve(Config),
    Help,
    Version,
}

/// Hand-rolled argument parsing, standing in for `clap`.
///
/// The surface is small enough that a loop over the arguments is clearer than
/// any abstraction over it, and an unknown flag is an error rather than
/// something silently ignored.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Invocation, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 5432;
    let mut dir: Option<PathBuf> = None;
    let mut cache = true;
    let mut dashboard: Option<u16> = None;

    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--no-cache" => cache = false,
            "--dashboard" => dashboard = dashboard.or(Some(DEFAULT_DASHBOARD_PORT)),
            "--dashboard-port" => {
                let raw = take_value(&mut args, &arg)?;
                dashboard = Some(
                    raw.parse()
                        .map_err(|_| format!("`{raw}` is not a valid port number"))?,
                );
            }

            "-H" | "--host" => host = take_value(&mut args, &arg)?,
            "-p" | "--port" => {
                let raw = take_value(&mut args, &arg)?;
                port = raw
                    .parse()
                    .map_err(|_| format!("`{raw}` is not a valid port number"))?;
            }
            "-d" | "--dir" => dir = Some(PathBuf::from(take_value(&mut args, &arg)?)),

            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option `{other}`"))
            }

            positional => {
                if dir.is_some() {
                    return Err(format!("unexpected argument `{positional}`"));
                }
                dir = Some(PathBuf::from(positional));
            }
        }
    }

    Ok(Invocation::Serve(Config {
        host,
        port,
        dir: dir.unwrap_or_else(|| PathBuf::from(".")),
        cache,
        dashboard,
    }))
}

fn take_value(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("`{flag}` needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    fn config(args: &[&str]) -> Config {
        match parse(args).unwrap() {
            Invocation::Serve(config) => config,
            _ => panic!("expected a server invocation"),
        }
    }

    #[test]
    fn defaults_bind_loopback_on_the_postgres_port() {
        let config = config(&[]);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 5432);
        assert_eq!(config.dir, PathBuf::from("."));
        assert!(config.cache);
        assert_eq!(config.dashboard, None, "the dashboard is opt-in");
    }

    #[test]
    fn the_dashboard_flags_agree_with_each_other() {
        assert_eq!(config(&["--dashboard"]).dashboard, Some(8080));
        assert_eq!(config(&["--dashboard-port", "9000"]).dashboard, Some(9000));
        // An explicit port wins whichever order the flags arrive in.
        assert_eq!(
            config(&["--dashboard", "--dashboard-port", "9000"]).dashboard,
            Some(9000)
        );
        assert_eq!(
            config(&["--dashboard-port", "9000", "--dashboard"]).dashboard,
            Some(9000)
        );
    }

    #[test]
    fn a_bare_path_is_the_directory() {
        assert_eq!(config(&["D:\\projects"]).dir, PathBuf::from("D:\\projects"));
    }

    #[test]
    fn flags_and_a_positional_path_combine() {
        let config = config(&["--port", "5433", "src", "--no-cache"]);
        assert_eq!(config.port, 5433);
        assert_eq!(config.dir, PathBuf::from("src"));
        assert!(!config.cache);
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(parse(&["--help"]).unwrap(), Invocation::Help));
        assert!(matches!(parse(&["-V"]).unwrap(), Invocation::Version));
    }

    #[test]
    fn bad_input_is_refused_with_a_message_rather_than_ignored() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--port"]).is_err());
        assert!(parse(&["--port", "99999"]).is_err());
        assert!(parse(&["one", "two"]).is_err());
    }
}
