//! GDB RSP session: TCP server + gdbstub `run_blocking` event loop.
//!
//! Replay operations run on a worker thread so the event loop can keep
//! watching the connection for a ^C interrupt while the target is running.
//!
//! ## Reversible debugging mapping (Delve gdbserial client)
//!
//! | Delve command          | RSP on the wire          | backend op            |
//! |------------------------|--------------------------|-----------------------|
//! | continue               | `vCont;c`                | `continue_forward`    |
//! | step / stepi           | `vCont;s:<tid>`          | `step`                |
//! | reverse continue       | `Hc<tid>` + `bc`         | `continue_backward`   |
//! | reverse step / stepi   | `Hc<tid>` + `bs`         | `step_back`           |
//! | `restart [pos]`        | `vRun;;<hex pos>`        | `goto`                |
//! | when                   | `qRRCmd when`            | `position`            |
//! | checkpoint(s)          | `qRRCmd checkpoint ...`  | (emulated)            |
//!
//! Stop-reply signals: breakpoint/step `T05`, trace end (forward) `T09`
//! (Delve's "almost exited", rewindable), trace start (reverse) `T00`,
//! interrupt `T02`.

use std::marker::PhantomData;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::GdbStub;
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::stub::run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError};
use gdbstub::target::Target;

use crate::target::DebugTarget;

use super::target::GdbTarget;

/// Drive a `DebugTarget` over GDB RSP on `listen` (e.g. "127.0.0.1:1234").
///
/// Cancellation of in-flight replays (^C) is wired automatically through
/// [`DebugTarget::interrupt_handle`]. The traced executable is reported
/// through qXfer:exec-file from the trace's first module.
///
/// When `shutdown` flips to true the accept loop stops between connections
/// and this function returns, letting the caller drop the engine cleanly
/// (releasing the trace files) and exit 0.
pub fn run_gdb_server<T: DebugTarget + Send + 'static>(
    backend: T,
    listen: &str,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(listen)?;
    let local = listener.local_addr()?;
    // Printed on stdout so integrations (Delve) can parse the actual port
    // when listening on port 0 — same pattern as `rr replay --dbgport=0`.
    println!("Listening on {}", local);
    // Companion stderr lines are part of the observable CLI surface
    // (scripts/diffs key off them); the log crate gets the same info.
    eprintln!("GDB RSP server listening on {}", local);
    log::info!("GDB RSP server listening on {}", local);

    serve_sessions(listener, backend, None, || {
        shutdown.load(std::sync::atomic::Ordering::Acquire)
    })?;
    log::info!("server shut down");
    Ok(())
}

/// Accept up to `max_sessions` sequential connections (`None` = until
/// killed), threading the backend through each session so engine state
/// (position, breakpoints) persists. Returns the backend when the session
/// bound is reached — the bound is what makes multi-session behavior
/// testable without process management.
///
/// `shutdown` is polled between connections (and while waiting for them);
/// when it returns true the loop stops and the backend is returned, letting
/// callers tear down cleanly.
pub fn serve_sessions<T: DebugTarget + Send + 'static>(
    listener: TcpListener,
    mut backend: T,
    max_sessions: Option<usize>,
    shutdown: impl Fn() -> bool + Send,
) -> Result<T, Box<dyn std::error::Error>> {
    // A zero session bound means "do not accept anything": return the
    // backend untouched instead of accepting a connection the caller did not
    // ask for.
    if max_sessions == Some(0) {
        return Ok(backend);
    }

    // Nonblocking accept so the shutdown flag is observed promptly even
    // with no client in sight; accepted sockets go back to blocking mode
    // because the session code assumes blocking I/O.
    listener.set_nonblocking(true)?;

    let mut count = 0usize;
    loop {
        if shutdown() {
            break;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                let peer = peer.to_string();
                eprintln!("GDB client connected: {}", peer);
                log::info!("GDB client connected: {}", peer);

                backend = run_gdb_session(backend, stream)?;
                eprintln!("GDB session closed");
                log::info!("GDB session closed");

                count += 1;
                if max_sessions.is_some_and(|max| count >= max) || shutdown() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Poll the shutdown flag while waiting for a client too, so
                // ^C with no session in sight ends the loop promptly instead
                // of relying on the next 100 ms tick.
                if shutdown() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(backend)
}

/// Run one GDB session over an accepted TCP stream; returns the backend so
/// engine state persists across sessions.
pub fn run_gdb_session<T: DebugTarget + Send + 'static>(
    backend: T,
    stream: TcpStream,
) -> Result<T, Box<dyn std::error::Error>> {
    let mut target = GdbTarget::new(backend);

    let gdb = GdbStub::new(stream);
    match gdb.run_blocking::<TtdEventLoop<T>>(&mut target) {
        Ok(disconnect_reason) => {
            log::info!("GDB session ended: {:?}", disconnect_reason);
        }
        Err(e) => {
            log::error!("GDB session error: {}", e);
        }
    }

    // Recover the backend so engine state (position, breakpoints)
    // persists into the next session.
    match target.into_backend() {
        Some(b) => Ok(b),
        None => {
            // The event loop always waits for the replay worker before
            // leaving the Running state, so a lingering reference should
            // be impossible; treat it as a fatal session error.
            Err("could not recover backend after session".into())
        }
    }
}

/// `BlockingEventLoop` glue: polls the connection for ^C and the replay
/// worker for stop reasons.
struct TtdEventLoop<T: DebugTarget + Send + 'static>(PhantomData<T>);

impl<T: DebugTarget + Send + 'static> BlockingEventLoop for TtdEventLoop<T> {
    type Target = GdbTarget<T>;
    type Connection = TcpStream;
    type StopReason = MultiThreadStopReason<u64>;

    fn wait_for_stop_reason(
        target: &mut Self::Target,
        conn: &mut Self::Connection,
    ) -> Result<
        Event<Self::StopReason>,
        WaitForStopReasonError<
            <Self::Target as Target>::Error,
            <Self::Connection as Connection>::Error,
        >,
    > {
        loop {
            // ^C / any incoming data from the client while the target runs.
            if let Ok(Some(_)) = conn.peek() {
                let byte = conn.read().map_err(WaitForStopReasonError::Connection)?;
                return Ok(Event::IncomingData(byte));
            }
            if let Some(reason) = target.take_stop_reason() {
                return Ok(Event::TargetStopped(reason));
            }
            // 1 ms keeps scripted stepping snappy; the cost is negligible
            // idle wakeups on a loop that only spins while the target runs.
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn on_interrupt(
        target: &mut Self::Target,
    ) -> Result<Option<Self::StopReason>, <Self::Target as Target>::Error> {
        // The worker thread sets `replay_running = false` BEFORE it pushes
        // the stop reason onto the channel, so a ^C that races past the
        // worker's last `take_stop_reason` call may arrive when the worker
        // is no longer "in flight" but has already delivered its reason.
        // Always drain the channel first so we report a real stop reason
        // when one is buffered; only ask the worker to abort when the
        // replay is actually still running.
        if let Some(reason) = target.take_stop_reason() {
            return Ok(Some(reason));
        }
        target.interrupt_replay();
        if target.replay_in_flight() {
            Ok(target.wait_stop_reason())
        } else {
            Ok(None)
        }
    }
}
