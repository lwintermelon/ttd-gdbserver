use std::path::PathBuf;

use clap::Parser;

use ttd_gdbserver::target::{DebugTarget, TtdProcess};

#[derive(Parser)]
#[command(
    name = "ttd-gdbserver",
    about = "Time-travel debugger for WinDbg TTD traces over GDB Remote Serial Protocol"
)]
struct Cli {
    /// Path to the .run trace file
    trace: PathBuf,
    /// Listen address (host:port). Use port 0 to pick a free port;
    /// the chosen port is printed as "Listening on <addr>".
    #[arg(long, default_value = "127.0.0.1:1234")]
    listen: String,
    /// Path to the traced executable (served via qXfer:exec-file so the
    /// client can load DWARF). Defaults to the first module in the trace.
    #[arg(long)]
    exe: Option<PathBuf>,
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    if let Err(e) = gdb_command(&cli.trace, &cli.listen, cli.exe.as_deref()) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Start a GDB RSP server: open the trace, bind TCP, accept sessions.
fn gdb_command(
    trace: &std::path::Path,
    listen: &str,
    exe: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Opening trace: {}", trace.display());
    let process = TtdProcess::open(trace)?;
    // Resolve the executable path: explicit --exe wins, else the first
    // module in the trace (usually the main executable).
    let exe_path = exe
        .map(|p| p.display().to_string())
        .or_else(|| process.modules().first().map(|m| m.name.clone()));
    let interrupt = process.interrupt();
    ttd_gdbserver::gdb::run_gdb_server(process, listen, Some(interrupt), exe_path)
}
