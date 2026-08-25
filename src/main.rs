use std::path::PathBuf;

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
    listen: String,
    /// Verbosity: -v enables debug logging for this crate,
    /// -vv also enables it for the gdbstub crate (very chatty).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbosity: u8,
}

fn init_logging(verbosity: u8) {
    // Default level: warnings and errors only, so the RSP wire stays quiet
    // for scripted clients (Delve pipes stderr through). -v turns on this
    // crate's debug logs (protocol handshakes, breakpoint sync, replays);
    // -vv additionally drops gdbstub's internal trace on stderr.
    let filter = match verbosity {
        0 => "warn",
        1 => "info,ttd_gdbserver=debug",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).init();
}

fn main() {
    let cli = Cli::parse();
    init_logging(cli.verbosity);

    // Graceful ^C: stop accepting, drop the engine (releases the trace
    // files), exit 0. The library exposes the flag; the binary owns the
    // handler because installing global console handlers is not a library's
    // business. A second ^C exits immediately.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = shutdown.clone();
    ctrlc::set_handler(move || {
        if !flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
            eprintln!("shutdown requested; finishing up (Ctrl+C again to force)");
        } else {
            std::process::exit(130);
        }
    })
    .expect("install ctrl+c handler");

    if let Err(e) = gdb_command(&cli.trace, &cli.listen, shutdown) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Start a GDB RSP server: open the trace, bind TCP, accept sessions.
fn gdb_command(
    trace: &std::path::Path,
    listen: &str,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Opening trace: {}", trace.display());
    let process = TtdProcess::open(trace)?;
    ttd_gdbserver::gdb::run_gdb_server(process, listen, shutdown)
}
