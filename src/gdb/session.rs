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
//! interrupt `T02`. A recorded process exit also stops at `T09` first; `W00`
//! is only answered when the client resumes forward again from that stop
//! (rr's `in_debuggee_end_state` → `handle_exited_state` sequence).

use std::marker::PhantomData;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::GdbStub;
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::stub::run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError};
use gdbstub::target::Target;

use crate::target::DebugTarget;

use super::checkpoint::CheckpointTable;
use super::target::GdbTarget;

/// Why the session layer could not start or keep serving.
///
/// Session I/O is fatal for the accept loop, so the error is `Send + Sync`: a
/// caller may move it to another thread (the integration tests do).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("listening socket: {0}")]
    Listener(#[source] std::io::Error),
    #[error("accepted connection: {0}")]
    Connection(#[source] std::io::Error),
    #[error("could not recover the debug target after the GDB session ended")]
    BackendRecovery,
}

/// Drive a `DebugTarget` over GDB RSP on `listen`.
///
/// Cancellation of in-flight replays (^C) is wired automatically through
/// [`DebugTarget::interrupt_handle`]. The traced executable is reported
/// through qXfer:exec-file from the trace's first module.
///
/// `persist` picks the mode: false serves exactly one session and returns
/// (the one-shot CLI default), true keeps accepting reconnects until
/// `shutdown` flips. When `shutdown` flips to true the accept loop stops
/// between connections and this function returns, letting the caller drop
/// the engine cleanly (releasing the trace files) and exit 0.
///
/// # Errors
/// [`Error::Listener`] when the listen socket cannot be bound or queried;
/// otherwise whatever [`serve_sessions`] reports.
pub fn run_gdb_server<T: DebugTarget + Send + 'static>(
    backend: T,
    listen: SocketAddr,
    shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    persist: bool,
) -> Result<(), Error> {
    let listener = TcpListener::bind(listen).map_err(Error::Listener)?;
    let local = listener.local_addr().map_err(Error::Listener)?;
    // Printed on stdout so integrations (Delve) can parse the actual port
    // when listening on port 0 — same pattern as `rr replay --dbgport=0`.
    // Informational lifecycle text otherwise stays out of stderr: a client
    // such as Delve forwards stderr, and only actual errors should reach its
    // debugger console.
    println!("Listening on {local}");
    log::info!("GDB RSP server listening on {local}");

    serve_sessions(listener, backend, persist, &|| {
        shutdown.load(std::sync::atomic::Ordering::Acquire)
    })?;
    log::info!("server shut down");
    Ok(())
}

/// Accept sequential connections, threading the backend through each session
/// so engine state (position, breakpoints) persists, and stop after the
/// first session unless `persist` is set (then: until `shutdown` flips).
/// Returns the backend when the loop ends, so a caller (and a test driving
/// the loop directly) can observe the state it carries.
///
/// The emulated rr checkpoint table is owned here and handed to each session
/// by value, so checkpoints persist across reconnects exactly like position
/// and breakpoints: `GdbTarget` gives the table back when the session ends.
///
/// `shutdown` is polled between connections (and while waiting for them);
/// when it returns true the loop stops and the backend is returned, letting
/// callers tear down cleanly.
///
/// # Errors
/// [`Error::Listener`] when the accept loop cannot observe or accept on the
/// socket, [`Error::Connection`] when an accepted stream cannot be configured
/// or a session cannot be started, and [`Error::BackendRecovery`] when a
/// session ends without handing the backend back.
// `listener` is taken by value on purpose: a serve loop owns the socket for
// its whole life, and `TcpListener` has no `Drop` side effect to preserve.
#[allow(clippy::needless_pass_by_value)]
pub fn serve_sessions<T: DebugTarget + Send + 'static>(
    listener: TcpListener,
    mut backend: T,
    persist: bool,
    shutdown: &(impl Fn() -> bool + Send),
) -> Result<T, Error> {
    // Nonblocking accept so the shutdown flag is observed promptly even
    // with no client in sight; accepted sockets go back to blocking mode
    // because the session code assumes blocking I/O.
    listener.set_nonblocking(true).map_err(Error::Listener)?;

    let mut checkpoints = CheckpointTable::new();
    loop {
        if shutdown() {
            break;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                stream.set_nonblocking(false).map_err(Error::Connection)?;
                stream.set_nodelay(true).map_err(Error::Connection)?;
                let peer = peer.to_string();
                log::info!("GDB client connected: {peer}");

                let (b, c) = run_gdb_session(backend, stream, checkpoints)?;
                backend = b;
                checkpoints = c;
                log::info!("GDB session closed");

                if !persist || shutdown() {
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
            Err(e) => return Err(Error::Connection(e)),
        }
    }
    Ok(backend)
}

/// Run one GDB session over an accepted TCP stream; returns the backend and
/// the checkpoint table so both persist into the next session.
///
/// A protocol-level failure ends only this session: it is logged, and the
/// recovered backend is returned so the server keeps serving. Abrupt client
/// disconnects and malformed packets are routine for a debugger backend, so
/// they must not take the whole server down with them.
///
/// # Errors
/// [`Error::BackendRecovery`] when the event loop leaves a lingering
/// reference to the backend, which means the replay worker never released it.
pub fn run_gdb_session<T: DebugTarget + Send + 'static>(
    backend: T,
    stream: TcpStream,
    checkpoints: CheckpointTable,
) -> Result<(T, CheckpointTable), Error> {
    let mut target = GdbTarget::new(backend, checkpoints);

    let gdb = GdbStub::new(stream);
    match gdb.run_blocking::<TtdEventLoop<T>>(&mut target) {
        Ok(disconnect_reason) => log::info!("GDB session ended: {disconnect_reason:?}"),
        Err(e) => log::error!("GDB session error: {e}"),
    }

    // Recover the backend so engine state (position, breakpoints) persists
    // into the next session. The event loop always waits for the replay worker
    // before leaving the Running state, so a lingering reference is a genuine
    // failure rather than a routine disconnect.
    let (backend, checkpoints) = target.into_parts();
    backend
        .map(|b| (b, checkpoints))
        .ok_or(Error::BackendRecovery)
}

/// `BlockingEventLoop` glue: polls the connection for ^C and the replay
/// worker for stop reasons. Only the associated `Target` type mentions `T`,
/// hence the `PhantomData` marker.
struct TtdEventLoop<T>(PhantomData<T>);

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
