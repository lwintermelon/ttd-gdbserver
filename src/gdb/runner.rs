//! `TargetRunner` — the concurrency core between the protocol event loop and
//! a [`DebugTarget`].
//!
//! Contract: queries (`query`) run inline against the backend; resume-style
//! operations (`start`) run on a worker thread so the event loop can keep
//! polling the connection while the target is running, and an in-flight
//! operation can be aborted through the backend's own interrupt handle
//! (`DebugTarget::interrupt_handle`) when the client sends ^C. All of the
//! mutex/channel/flag machinery lives here instead of being spread across
//! the protocol layer, and is unit-tested below without any TCP involved.
//!
//! A panicking backend does not wedge the runner: the worker catches the
//! panic, publishes a [`WorkerPanic`] through the same result channel as a
//! normal stop, and clears the in-flight flag before the protocol loop can
//! observe the result. The protocol layer maps that outcome to a terminal
//! SIGTRAP stop instead of waiting forever for a stop that will never come.

use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::target::DebugTarget;

/// Poison-tolerant lock: a panicking worker must not cascade into panics on
/// the event loop — the guarded data stays usable for the next operation.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A worker job panicked. Carries a printable payload for the log and the
/// protocol-layer diagnostic; `R` itself cannot be fabricated from thin air,
/// so the protocol layer maps this outcome to a target-error stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerPanic {
    message: String,
}

impl WorkerPanic {
    fn from_payload(payload: &(dyn Any + Send)) -> Self {
        let message = if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else {
            "non-string panic payload".to_string()
        };
        Self { message }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for WorkerPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Returned by [`TargetRunner::start`] when a replay worker is already
/// running. The protocol event loop must drain the in-flight result first;
/// this is an error rather than a panic so a malformed client cannot crash
/// the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlreadyRunning;

impl fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a replay is already in flight")
    }
}
/// Drives a `DebugTarget` from a single (protocol) thread.
///
/// `R` is whatever a started job returns — for the GDB frontend that is a
/// mapped `MultiThreadStopReason`; unit tests use plain values.
pub struct TargetRunner<T: DebugTarget + Send + 'static, R: Send + 'static> {
    backend: Arc<Mutex<T>>,
    /// Aborts an in-flight blocking replay inside the backend (^C).
    interrupt: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Set by `request_interrupt` before the worker finishes; makes the
    /// worker report `on_interrupt`'s result instead of the job's, so a ^C
    /// abort surfaces as an interrupt stop rather than the op's outcome.
    ///
    /// Cleared at the start of every job so a late interrupt observed after a
    /// previous worker passed its swap-check can never leak into the next job.
    interrupted: Arc<AtomicBool>,
    /// True between `start` and the worker publishing its result.
    replay_running: Arc<AtomicBool>,
    /// Receiving end of the most recent job. A `start` swaps in a fresh
    /// channel, so a result is never delivered twice; the one created in
    /// `new` has no sender and therefore yields `None` (nothing has run yet).
    rx: Mutex<Receiver<Result<R, WorkerPanic>>>,
}

impl<T: DebugTarget + Send + 'static, R: Send + 'static> TargetRunner<T, R> {
    pub fn new(backend: T) -> Self {
        // Take the interrupt factory up front: it must not require locking
        // the backend (the whole point is firing it *while* the replay
        // worker holds the lock).
        let interrupt = backend.interrupt_handle();
        let (_, rx) = mpsc::channel();
        Self {
            backend: Arc::new(Mutex::new(backend)),
            interrupt,
            interrupted: Arc::new(AtomicBool::new(false)),
            replay_running: Arc::new(AtomicBool::new(false)),
            rx: Mutex::new(rx),
        }
    }

    /// Run `f` against the backend right now. Blocks while a replay holds
    /// the lock (frontends only query while stopped, so this never contends
    /// in practice).
    pub fn query<Q, Qr>(&self, f: Q) -> Qr
    where
        Q: FnOnce(&mut T) -> Qr,
    {
        let mut backend = lock(&self.backend);
        f(&mut backend)
    }

    /// Raw access to the backend guard (poison-tolerant). Prefer [`Self::query`].
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        lock(&self.backend)
    }

    /// Start `job` on a worker thread; returns immediately. If the client
    /// requested an interrupt before the job finishes, `on_interrupt`'s
    /// result is delivered instead of the job's. A panicking job delivers
    /// [`WorkerPanic`] through the same channel.
    pub fn start<Job, OnInterrupt>(
        &mut self,
        job: Job,
        on_interrupt: OnInterrupt,
    ) -> Result<(), AlreadyRunning>
    where
        Job: FnOnce(&mut T) -> R + Send + 'static,
        OnInterrupt: FnOnce(&mut T) -> R + Send + 'static,
    {
        self.replay_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AlreadyRunning)?;
        // A late interrupt from the previous job must not affect this one.
        self.interrupted.store(false, Ordering::SeqCst);

        let (tx, rx) = mpsc::channel();
        *lock(&self.rx) = rx;

        let backend = self.backend.clone();
        let interrupted = self.interrupted.clone();
        let replay_running = self.replay_running.clone();

        std::thread::spawn(move || {
            let outcome = match catch_unwind(AssertUnwindSafe(|| {
                let mut b = lock(&backend);
                let result = job(&mut b);
                if interrupted.swap(false, Ordering::SeqCst) {
                    on_interrupt(&mut b)
                } else {
                    result
                }
            })) {
                Ok(result) => Ok(result),
                Err(payload) => Err(WorkerPanic::from_payload(&*payload)),
            };

            // Drop the worker's backend handle before publishing the result:
            // `into_backend` (session teardown) uses `Arc::try_unwrap`, and
            // without this an immediate handoff after the last stop could
            // nondeterministically fail while this thread is still winding
            // down.
            drop(backend);

            // Publish idle *before* sending: `in_flight() == false` means the
            // backend lock has been released, so the protocol event loop can
            // immediately service monitor/qRRCmd/replay packets without
            // contending with the dead worker.
            replay_running.store(false, Ordering::SeqCst);
            let _ = tx.send(outcome);
        });

        Ok(())
    }

    /// Poll for a finished job's result without blocking.
    pub fn take_result(&self) -> Option<Result<R, WorkerPanic>> {
        lock(&self.rx).try_recv().ok()
    }

    /// Block until the worker reports its result.
    pub fn wait_result(&self) -> Option<Result<R, WorkerPanic>> {
        lock(&self.rx).recv().ok()
    }

    /// Whether a replay worker is currently in flight.
    pub fn in_flight(&self) -> bool {
        self.replay_running.load(Ordering::SeqCst)
    }

    /// Request that the in-flight job be reported as interrupted. Fires the
    /// backend's interrupt handle (aborting the blocking replay); does
    /// nothing when no replay is running or the target has no handle.
    pub fn request_interrupt(&self) {
        if self.replay_running.load(Ordering::SeqCst) {
            self.interrupted.store(true, Ordering::SeqCst);
            if let Some(f) = &self.interrupt {
                f();
            }
        }
    }

    /// Recover the backend once all work has drained (session end).
    ///
    /// If a replay is still in flight when the client disconnects, fire the
    /// backend interrupt handle and wait (bounded) for the worker to release
    /// the backend. Without this, an abrupt disconnect during `vCont;c`
    /// would make `Arc::try_unwrap` fail and turn a recoverable session end
    /// into a server-level error.
    pub fn into_backend(self) -> Option<T> {
        if self.in_flight() && self.interrupt.is_some() {
            self.request_interrupt();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.in_flight() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if self.in_flight() {
                log::warn!("replay worker did not stop within 5s of the session-end interrupt");
            }
        }

        Arc::try_unwrap(self.backend).ok().map(|m| {
            m.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{
        BreakpointKind, DebugError, DebugTarget, ModuleInfo, StopReason, TargetDiagnostics,
        ThreadExtraInfoData,
    };
    use crate::ttd::types::{TtdPosition, TtdX64Regs};
    use std::sync::mpsc;

    #[derive(Default)]
    struct MockTarget {
        position: TtdPosition,
        /// When set, `continue_forward` blocks until the sender fires.
        gate: Option<mpsc::Receiver<()>>,
        /// Fired by `interrupt_handle`; test targets use it to release a
        /// blocked `continue_forward` the way TTD's real interrupt does.
        interrupt: Option<mpsc::Sender<()>>,
        panics: bool,
    }

    impl DebugTarget for MockTarget {
        fn step(&mut self) -> Result<StopReason, DebugError> {
            self.position.steps += 1;
            Ok(StopReason::StepComplete)
        }
        fn step_back(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::StepComplete)
        }
        fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
            if self.panics {
                panic!("mock backend panic");
            }
            if let Some(rx) = self.gate.take() {
                let _ = rx.recv();
            }
            Ok(StopReason::Breakpoint {
                bp_id: 1,
                addr: 0xdead,
            })
        }
        fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::TraceStart)
        }
        fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
            self.position = pos;
            Ok(StopReason::PositionReached)
        }
        fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
            buf.fill(addr as u8);
            buf.len()
        }
        fn set_breakpoint(&mut self, _addr: u64, _t: Option<u64>) -> Result<u64, DebugError> {
            Ok(1)
        }
        fn set_data_breakpoint(
            &mut self,
            _a: u64,
            _s: u64,
            _k: BreakpointKind,
        ) -> Result<u64, DebugError> {
            Ok(1)
        }
        fn remove_breakpoint(&mut self, _id: u64) -> bool {
            true
        }
        fn active_thread_ids(&self) -> Vec<u64> {
            vec![100]
        }
        fn thread_state(&self, _id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
            Some((TtdX64Regs::default(), 0x7ffe_0000))
        }
        fn thread_info(&self, _id: u64) -> Option<ThreadExtraInfoData> {
            None
        }
        fn current_thread_id(&self) -> Option<u64> {
            Some(100)
        }
        fn interrupt_handle(&self) -> Option<std::sync::Arc<dyn Fn() + Send + Sync>> {
            let tx = self.interrupt.clone()?;
            Some(std::sync::Arc::new(move || {
                let _ = tx.send(());
            }))
        }
        fn set_current_thread(&mut self, _id: u64) {}
        fn modules(&self) -> Vec<ModuleInfo> {
            vec![]
        }
        fn diagnostics(&self) -> TargetDiagnostics {
            unimplemented!("unused in runner tests")
        }
        fn position(&self) -> TtdPosition {
            self.position
        }
        fn lifetime(&self) -> (TtdPosition, TtdPosition) {
            (
                TtdPosition {
                    sequence: 0,
                    steps: 0,
                },
                TtdPosition {
                    sequence: 9,
                    steps: 1000,
                },
            )
        }
    }

    type TestRunner = TargetRunner<MockTarget, &'static str>;

    #[test]
    fn job_result_is_delivered_and_in_flight_tracks_worker() {
        let mut runner = TestRunner::new(MockTarget::default());
        assert!(!runner.in_flight());
        runner.start(|_| "done", |_| "interrupted").unwrap();
        assert!(runner.in_flight());
        assert_eq!(runner.wait_result(), Some(Ok("done")));
        assert!(!runner.in_flight());
    }

    #[test]
    fn start_while_running_is_an_error_and_does_not_disturb_the_first_job() {
        let (tx, rx) = mpsc::channel();
        let target = MockTarget {
            gate: Some(rx),
            ..Default::default()
        };
        let mut runner = TestRunner::new(target);

        runner
            .start(
                |b| b.continue_forward().map(|_| "first").unwrap_or("err"),
                |_| "interrupted",
            )
            .unwrap();
        assert_eq!(
            runner.start(|_| "second", |_| "interrupted"),
            Err(AlreadyRunning),
            "a second start while the first job is in flight must be rejected"
        );

        tx.send(()).unwrap();
        assert_eq!(runner.wait_result(), Some(Ok("first")));
        assert!(!runner.in_flight());
    }
    #[test]
    fn queries_run_inline_against_the_backend() {
        let runner = TestRunner::new(MockTarget::default());
        let n = runner.query(|b| {
            b.step().unwrap();
            b.position().steps
        });
        assert_eq!(n, 1);
    }

    /// A session ending while a replay is still in flight must fire the
    /// backend interrupt handle and recover the backend once the worker
    /// drains, instead of leaking a live `Arc<Mutex<T>>`.
    #[test]
    fn into_backend_interrupts_running_worker_and_recovers_backend() {
        let (tx, rx) = mpsc::channel();
        let target = MockTarget {
            gate: Some(rx),
            interrupt: Some(tx),
            ..Default::default()
        };
        let mut runner = TestRunner::new(target);
        runner
            .start(
                |b| b.continue_forward().map(|_| "job").unwrap_or("err"),
                |_| "interrupted",
            )
            .unwrap();
        assert!(runner.in_flight());

        let backend = runner
            .into_backend()
            .expect("interrupting the worker must let the backend be recovered");
        assert!(
            backend.gate.is_none(),
            "the worker must have consumed the gate"
        );
    }
    #[test]
    fn late_interrupt_overrides_job_result() {
        let (tx, rx) = mpsc::channel();
        // The mock blocks in continue_forward like a real blocking replay;
        // this test simulates the backend's abort by releasing the gate.
        let target = MockTarget {
            gate: Some(rx),
            ..Default::default()
        };

        let mut runner = TestRunner::new(target);
        runner
            .start(
                |b| b.continue_forward().map(|_| "job").unwrap_or("err"),
                |b| {
                    let _ = b.current_thread_id();
                    "interrupted"
                },
            )
            .unwrap();
        // Fire ^C mid-flight (sets the flag; no handle to fire), then let
        // the blocked replay finish. The worker must report on_interrupt's
        // result, not the job's.
        runner.request_interrupt();
        tx.send(()).unwrap();
        assert_eq!(runner.wait_result(), Some(Ok("interrupted")));
        assert!(!runner.in_flight());

        // The flag is consumed: a subsequent normal job reports normally.
        runner.start(|_| "second", |_| "should-not-fire").unwrap();
        assert_eq!(runner.wait_result(), Some(Ok("second")));
    }

    #[test]
    fn interrupt_without_in_flight_replay_is_a_noop() {
        let runner = TestRunner::new(MockTarget::default());
        runner.request_interrupt(); // must not panic or set anything
        assert!(runner.take_result().is_none());
    }

    #[test]
    fn backend_panic_is_reported_and_the_runner_stays_usable() {
        let mut runner = TestRunner::new(MockTarget {
            panics: true,
            ..Default::default()
        });
        runner
            .start(
                |b| {
                    let _ = b.continue_forward();
                    "unreachable"
                },
                |_| "interrupted",
            )
            .unwrap();

        match runner.wait_result() {
            Some(Err(panic)) => {
                assert!(
                    panic.message().contains("mock backend panic"),
                    "panic message must be preserved: {panic}"
                );
            }
            other => panic!("a worker panic must be reported, got {other:?}"),
        }
        assert!(
            !runner.in_flight(),
            "a panicking worker must clear its in-flight flag"
        );

        // The poisoned mutex must not wedge later operations.
        let n = runner.query(|b| {
            b.step().unwrap();
            b.position().steps
        });
        assert_eq!(n, 1);
    }

    /// A late interrupt observed after a worker has already passed its own
    /// interrupt check must not leak into the next job.
    #[test]
    fn late_interrupt_does_not_leak_into_the_next_job() {
        let (tx, rx) = mpsc::channel();
        let target = MockTarget {
            gate: Some(rx),
            ..Default::default()
        };
        let mut runner = TestRunner::new(target);
        runner
            .start(
                |b| b.continue_forward().map(|_| "job").unwrap_or("err"),
                |_| "interrupted",
            )
            .unwrap();
        tx.send(()).unwrap();
        assert_eq!(runner.wait_result(), Some(Ok("job")));

        // Request after the worker has sent its result but before another
        // catch-all protocol packet reads it: must be a no-op.
        runner.request_interrupt();

        runner.start(|_| "next-job", |_| "should-not-fire").unwrap();
        assert_eq!(runner.wait_result(), Some(Ok("next-job")));
    }
}
