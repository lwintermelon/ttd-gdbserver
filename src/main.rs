//! `ttd-gdbserver` — serve a `WinDbg` TTD trace over the GDB Remote Serial
//! Protocol.
//!
//! The binary owns everything a library should not: the CLI, logging setup,
//! and the global console handler for graceful ^C shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use ttd_gdbserver::target::TtdProcess;

#[derive(Parser)]
#[command(
    name = "ttd-gdbserver",
    about = "Time-travel debugger for WinDbg TTD traces over GDB Remote Serial Protocol",
    version
)]
struct Cli {
    /// Path to the .run trace file
    trace: PathBuf,
    /// Listen address (host:port). Use port 0 to pick a free port;
    /// the chosen port is printed as "Listening on <addr>".
    #[arg(long, default_value = "127.0.0.1:1234")]
    listen: SocketAddr,
    /// Verbosity: -v enables debug logging for this crate,
    /// -vv also enables it for the gdbstub crate (very chatty).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbosity: u8,
    /// Keep serving after a session ends (default: exit after the first
    /// session). Like dlv's flag of the same name: a one-shot run gets a
    /// clean shutdown when the client disconnects, while this mode keeps the
    /// engine state alive for reconnects until ^C.
    #[arg(long)]
    accept_multiclient: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbosity);

    let shutdown = Arc::new(AtomicBool::new(false));
    install_interrupt_handler(Arc::clone(&shutdown));

    match serve(&cli.trace, cli.listen, &shutdown, cli.accept_multiclient) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Log only warnings and errors by default, so the RSP wire stays quiet for
/// scripted clients (Delve pipes stderr through). `-v` turns on this crate's
/// debug logs (protocol handshakes, breakpoint sync, replays); `-vv`
/// additionally drops gdbstub's internal trace on stderr.
fn init_logging(verbosity: u8) {
    let filter = match verbosity {
        0 => "warn",
        1 => "info,ttd_gdbserver=debug",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).init();
}

/// Graceful ^C: stop accepting, drop the engine (which releases the trace
/// files), and exit 0. The library exposes the flag; the binary owns the
/// handler because installing global console handlers is not a library's
/// business. A second ^C exits immediately.
fn install_interrupt_handler(shutdown: Arc<AtomicBool>) {
    ctrlc::set_handler(move || {
        if shutdown.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        eprintln!("shutdown requested; finishing up (Ctrl+C again to force)");
    })
    .expect("install ctrl+c handler");
}

/// Open the trace and serve GDB RSP sessions over TCP.
fn serve(
    trace: &std::path::Path,
    listen: SocketAddr,
    shutdown: &Arc<AtomicBool>,
    accept_multiclient: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unconditional, not `log::info!`: the first open of a trace builds its
    // memory index, which can take minutes, and the default log level would
    // leave the user staring at a silent terminal. One line before the work
    // starts is the only feedback available that early.
    eprintln!("Opening trace: {}", trace.display());
    log::info!("Opening trace: {}", trace.display());
    let process = TtdProcess::open(trace)?;
    ttd_gdbserver::gdb::run_gdb_server(process, listen, shutdown, accept_multiclient)?;
    Ok(())
}
