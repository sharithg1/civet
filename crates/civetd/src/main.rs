//! `civetd`: the Civet daemon. See `docs/INTERFACES.md` section 5.1.

use std::net::IpAddr;
use std::path::PathBuf;

use civetd::bootstrap::exit_code;
use civetd::daemon::{self, DaemonOptions, ExitError};
use civetd::paths::{StatePaths, create_private_dir, state_dir_or_default};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "civetd", version, about = "Civet daemon")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the daemon (QUIC + unix socket) until SIGTERM/SIGINT.
    Serve {
        /// State directory (default $CIVET_STATE_DIR or ~/.civet).
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// QUIC bind address.
        #[arg(long, default_value = "0.0.0.0")]
        bind: IpAddr,
        /// UDP ports to try, e.g. 60001-60999.
        #[arg(long, env = "CIVET_PORT_RANGE", default_value = "60001-60999")]
        port_range: String,
        /// Fork, start a new session, detach stdio, log to <state>/logs.
        #[arg(long)]
        detach: bool,
    },
    /// Start or reattach to the daemon and print its bootstrap JSON line.
    Bootstrap {
        /// State directory (default $CIVET_STATE_DIR or ~/.civet).
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Stop a running daemon even if it has live panes.
        #[arg(long)]
        replace: bool,
        /// QUIC bind address for a daemon this call starts (default
        /// 0.0.0.0; local clients pass 127.0.0.1).
        #[arg(long)]
        bind: Option<IpAddr>,
    },
    /// Pipe stdin/stdout to the running daemon's unix socket.
    StdioTunnel {
        /// State directory (default $CIVET_STATE_DIR or ~/.civet).
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Print the version.
    Version {
        /// Machine-readable.
        #[arg(long)]
        json: bool,
    },
}

/// Log to stderr (stdout is reserved for the bootstrap line and the tunnel).
fn init_stderr_logging(default: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CIVET_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn init_file_logging(state_dir: &std::path::Path) -> std::io::Result<()> {
    let paths = StatePaths::new(state_dir);
    create_private_dir(&paths.logs_dir())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    let filter = tracing_subscriber::EnvFilter::try_from_env("CIVET_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
    Ok(())
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Cmd::Version { json } => {
            if json {
                println!("{}", civetd::version_json());
            } else {
                println!("{}", civetd::version_line());
            }
            Ok(())
        }
        Cmd::Serve {
            state_dir,
            bind,
            port_range,
            detach,
        } => {
            let state_dir = state_dir_or_default(state_dir.as_deref());
            let port_range = daemon::parse_port_range(&port_range)?;
            // Absolute, because daemonizing does not chdir but callers may
            // pass relative paths and we log/refer to it later.
            let state_dir = std::path::absolute(&state_dir).unwrap_or(state_dir);
            if detach {
                // Single-threaded here: fork before any runtime exists.
                daemon::daemonize()?;
                init_file_logging(&state_dir)?;
            } else {
                init_stderr_logging("info");
            }
            let mut opts = DaemonOptions::new(state_dir);
            opts.bind_ip = bind;
            opts.port_range = port_range;
            let r = runtime()?.block_on(daemon::serve(opts));
            if let Err(e) = &r {
                tracing::error!(error = %format!("{e:#}"), "civetd serve failed");
            }
            r
        }
        Cmd::Bootstrap {
            state_dir,
            replace,
            bind,
        } => {
            init_stderr_logging("warn");
            let state_dir = state_dir_or_default(state_dir.as_deref());
            let state_dir = std::path::absolute(&state_dir).unwrap_or(state_dir);
            let info = runtime()?.block_on(daemon::bootstrap(&state_dir, replace, bind))?;
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(info.to_json_line().as_bytes())?;
            out.flush()?;
            Ok(())
        }
        Cmd::StdioTunnel { state_dir } => {
            init_stderr_logging("warn");
            let state_dir = state_dir_or_default(state_dir.as_deref());
            let rt = runtime()?;
            let r = rt.block_on(daemon::stdio_tunnel(&state_dir));
            // tokio's stdin reader thread may still be blocked in read(2),
            // and dropping the runtime would wait for it: exit right here.
            match r {
                Ok(()) => std::process::exit(exit_code::OK),
                Err(e) => exit_with(e),
            }
        }
    }
}

fn exit_with(e: anyhow::Error) -> ! {
    let code = e
        .downcast_ref::<ExitError>()
        .map(|x| x.code)
        .unwrap_or(exit_code::ERROR);
    eprintln!("civetd: {e:#}");
    std::process::exit(code);
}

/// `\0CIVETD_VERSION=<version>\0`, kept in the binary so a client can check
/// a `civetd-<triple>` file's version before uploading it (see
/// `civetd::bootstrap::binary_version`).
#[used]
static VERSION_MARKER: [u8; civetd::bootstrap::VERSION_MARKER.len()] =
    civetd::bootstrap::version_marker_bytes();

fn main() {
    // Keep the marker even under aggressive dead-code elimination.
    std::hint::black_box(&VERSION_MARKER);
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        exit_with(e);
    }
}
