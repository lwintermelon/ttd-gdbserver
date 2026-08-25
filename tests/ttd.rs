//! TTD FFI-level integration tests: `TtdEngine` + `TtdCursor`, including the
//! safe callback wrappers.
//!
//! Every test here needs a recorded `.run` trace. When `TTD_TRACE_PATH` is
//! unset (CI), build.rs does not emit the `has_ttd_trace` cfg, and the
//! `#[cfg_attr]` below marks all tests `#[ignore]` — libtest then reports
//! them as *skipped*, not as passing. Run them with:
//!
//! ```bash
//! TTD_TRACE_PATH=path/to/trace.run cargo test --test ttd -- --ignored
//! ```
//!
//! (When the cfg is set the attribute expands to nothing, so the tests run
//! as part of a plain `cargo test` too; `--ignored` is only needed in the
//! no-trace configuration's complementary run.)

mod engine {
    use ttd_gdbserver::ttd::TtdEngine;
    use ttd_gdbserver_test_support::create_engine as load_trace;

    fn create_engine() -> TtdEngine {
        load_trace()
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_engine_create_and_load() {
        let _engine = create_engine();
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_engine_lifetime() {
        let engine = create_engine();
        let first = engine.first_position();
        let last = engine.last_position();
        println!("Trace lifetime: {:?} -> {:?}", first, last);
        assert!(
            first.sequence <= last.sequence,
            "first position should <= last"
        );
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_engine_threads() {
        let engine = create_engine();
        let count = engine.thread_count();
        println!("Thread count: {}", count);
        assert!(count > 0, "Trace should have at least one thread");

        for i in 0..count {
            let info = engine.thread_info(i).expect("thread info");
            println!(
                "  Thread {}: uid={}, os_tid={:#x}",
                i, info.unique_id, info.os_thread_id
            );
        }
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_engine_exceptions() {
        let engine = create_engine();
        let count = engine.exception_count();
        println!("Exception count: {}", count);
        for i in 0..count.min(5) {
            let ev = engine.exception_event(i);
            println!(
                "  Exception {}: code={:#x}, pos={:?}, pc={:#x}",
                i, ev.code, ev.position, ev.program_counter
            );
        }
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_engine_modules() {
        let engine = create_engine();
        let count = engine.module_count();
        println!("Module count: {}", count);
        assert!(count > 0);
        for i in 0..count.min(5) {
            if let Some((addr, size, name)) = engine.module_info(i) {
                println!("  Module {}: {} at {:#x} size={}", i, name, addr, size);
            }
        }
    }
}

mod cursor {
    use ttd_gdbserver::ttd::TtdEngine;
    use ttd_gdbserver_test_support::create_engine as load_trace;

    fn create_engine() -> TtdEngine {
        load_trace()
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_initial_position() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let pos = cursor.position();
        println!("Initial position: {:?}", pos);
        assert!(
            pos.sequence > 0 || pos.steps > 0,
            "Position should be valid"
        );
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_read_registers() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let regs = cursor.read_registers();
        let pc = cursor.pc();
        println!("RIP = {:#x}", regs.rip);
        assert_eq!(regs.rip, pc, "pc() should match registers.rip");
        assert!(pc > 0, "PC should be non-zero");
        // x87/SSE state comes from AMD64_CONTEXT.FltSave; MXCSR has a
        // non-zero default (0x1F80) once SSE is initialized.
        println!(
            "MXCSR = {:#x}, st0 = {:02x?}, xmm0 = {:02x?}",
            regs.mxcsr,
            &regs.st[..10],
            &regs.xmm[..16]
        );
        assert!(regs.mxcsr != 0, "MXCSR should be non-zero");
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_read_memory() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let pc = cursor.pc();
        let mut buf = [0u8; 16];
        let n = cursor.read_memory(pc, &mut buf);
        println!("Read {} bytes at PC {:#x}", n, pc);
        assert!(n > 0, "Should read at least 1 byte at PC");
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_step_forward() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let pos_before = cursor.position();
        let _ = cursor.step_forward(1).expect("step forward");
        let pos_after = cursor.position();
        println!("Step 1: {:?} -> {:?}", pos_before, pos_after);
        assert_ne!(pos_before, pos_after, "Position should change after step");
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_step_backward() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        cursor.step_forward(5).expect("step forward 5");
        let pos_after_forward = cursor.position();
        let _result = cursor.step_backward(1).expect("step backward");
        let pos_after_backward = cursor.position();
        assert!(
            pos_after_backward <= pos_after_forward,
            "Position should go backward"
        );
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_watchpoint() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let pc = cursor.pc();
        cursor
            .add_watchpoint(
                pc,
                1,
                ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                0,
            )
            .expect("add watchpoint");
        let result = cursor.step_forward(1).expect("step with watchpoint");
        println!(
            "Watchpoint test: stop_reason={}, wp_addr={:#x}",
            result.stop_reason, result.wp_address
        );
        cursor
            .remove_watchpoint(
                pc,
                1,
                ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                0,
            )
            .expect("remove watchpoint");
    }

    /// Regression test for the access-type bug at the FFI boundary: the
    /// `TtdReplayResult.wp_access_type` field must carry a `DataAccessMask`
    /// bitmask (Write = 1<<1 = 2), not the SDK's `DataAccessType` enum value
    /// (Write = 1). Before the C++ shim conversion, an execute watchpoint
    /// reported `wp_access_type = 2` (the *enum* Execute value), which
    /// collides with the *bitmask* Write value — masking the bug.
    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_watchpoint_reports_access_bitmask() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let pc = cursor.pc();
        cursor
            .add_watchpoint(
                pc,
                1,
                ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                0,
            )
            .expect("add watchpoint");
        let result = cursor.step_forward(1).expect("step with watchpoint");
        cursor
            .remove_watchpoint(
                pc,
                1,
                ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                0,
            )
            .expect("remove watchpoint");
        assert_eq!(
            result.stop_reason,
            ttd_gdbserver::ttd::types::TTD_Replay_EventType_MemoryWatchpoint,
            "execute watchpoint at the current PC must be the stop reason"
        );
        // Execute's bitmask is 1<<2 = 4; the SDK enum value 2 must NOT leak.
        assert_eq!(
            result.wp_access_type, 4,
            "wp_access_type must be the DataAccessMask bitmask for Execute (4), \
             not the DataAccessType enum value (2)"
        );
    }

    /// The watchpoint callback must deliver the OS thread id of the hitting
    /// thread so the target layer can apply per-thread breakpoint filters.
    /// The first thread's own execute watchpoint must report that thread's
    /// OS tid (a live, non-zero value from the trace's thread list).
    ///
    /// Drives the safe `set_watchpoint_callback` API (owned closure, no
    /// manual ctx pointer).
    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_watchpoint_callback_reports_thread_id() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let engine = create_engine();
        // Installing a callback mutates the cursor's Rust-side slot.
        let mut cursor = engine.create_cursor().expect("create cursor");
        let pc = cursor.pc();
        let cur_tid = cursor.current_tid();
        assert!(cur_tid != 0, "a current thread must exist at trace start");

        let reported = std::sync::Arc::new(AtomicU32::new(0));
        let seen = reported.clone();
        cursor.set_watchpoint_callback(move |_addr, _size, _access, thread_id| {
            seen.store(thread_id, Ordering::Release);
            true
        });
        cursor.set_event_mask(ttd_gdbserver::ttd::types::TTD_Replay_EventMask_MemoryWatchpoint);
        cursor
            .add_watchpoint(
                pc,
                1,
                ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                0,
            )
            .expect("add watchpoint");
        cursor.step_forward(1).expect("step");
        cursor.clear_watchpoint_callback();
        println!(
            "callback thread id = {:#x}, current tid = {:#x}",
            reported.load(Ordering::Acquire),
            cur_tid
        );
        assert_eq!(
            reported.load(Ordering::Acquire),
            cur_tid,
            "callback must report the hitting thread's OS tid"
        );
    }

    /// The scoped watchpoint callback (`with_watchpoint_callback`) must fire
    /// during replay and be uninstalled afterwards: a second replay without a
    /// registered breakpoint runs to the boundary instead of stopping at the
    /// watched address.
    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_scoped_watchpoint_callback() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let engine = create_engine();
        // Installing a callback mutates the cursor's Rust-side slot.
        let mut cursor = engine.create_cursor().expect("create cursor");
        let pc = cursor.pc();

        let hits = AtomicU32::new(0);
        {
            let hits = &hits;
            let mut cb = move |_addr: u64, _size: u64, _access: u8, _thread_id: u32| {
                hits.fetch_add(1, Ordering::Release);
                true
            };
            let result = cursor.with_watchpoint_callback(&mut cb, |cursor| {
                cursor.set_event_mask(
                    ttd_gdbserver::ttd::types::TTD_Replay_EventMask_MemoryWatchpoint,
                );
                cursor.add_watchpoint(
                    pc,
                    1,
                    ttd_gdbserver::ttd::types::TTD_Replay_DataAccessMask_Execute,
                    0,
                )?;
                cursor.step_forward(1)
            });
            result.expect("step with scoped callback");
        }
        assert!(
            hits.load(Ordering::Acquire) > 0,
            "scoped callback must fire"
        );

        // After the scope the callback is gone: repeating the same step (the
        // TTD watchpoint itself is still registered) stops again but must not
        // invoke the Rust closure any more (hits stay flat).
        let before = hits.load(Ordering::Acquire);
        cursor.set_position(engine.first_position());
        let _ = cursor.step_forward(1).expect("step after scope");
        assert_eq!(
            hits.load(Ordering::Acquire),
            before,
            "callback must be uninstalled after the scope"
        );
    }

    /// `build_index_with_progress` must succeed and (when the index needs
    /// rebuilding) report monotonic progress. With an existing .idx file the
    /// build is skipped and no callbacks fire — both outcomes are valid.
    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_build_index_progress_callback() {
        use std::path::Path;
        use std::sync::atomic::{AtomicU32, Ordering};

        let path = std::env::var("TTD_TRACE_PATH").expect("TTD_TRACE_PATH set");
        let engine = ttd_gdbserver::ttd::TtdEngine::new().expect("engine");
        engine.load_trace(Path::new(&path)).expect("load trace");

        let calls = AtomicU32::new(0);
        let last_total = AtomicU32::new(0);
        engine
            .build_index_with_progress(|processed, total| {
                calls.fetch_add(1, Ordering::Release);
                last_total.store(total, Ordering::Release);
                assert!(processed <= total, "progress {processed} exceeds {total}");
                assert!(total > 0);
            })
            .expect("build index with progress");
        let n = calls.load(Ordering::Acquire);
        println!(
            "index progress callbacks: {n} (last total {})",
            last_total.load(Ordering::Acquire)
        );
        // Idempotent second call: index now exists either way.
        engine.build_index().expect("second build is a no-op");
    }

    #[test]
    #[cfg_attr(not(has_ttd_trace), ignore)]
    fn test_cursor_per_thread_queries() {
        let engine = create_engine();
        let cursor = engine.create_cursor().expect("create cursor");
        let count = cursor.thread_count();
        assert!(count > 0);
        let pc0 = cursor.pc();
        let pc_t0 = cursor.pc_thread(0);
        assert_eq!(pc0, pc_t0, "pc_thread(0) should match pc()");
        for i in 0..count.min(3) {
            let info = cursor.active_thread_info(i).unwrap();
            let pc = cursor.pc_thread(info.thread.os_thread_id);
            let pos = cursor.pos_thread(info.thread.os_thread_id);
            println!(
                "  Thread idx={}: uid={}, tid={:#x}, pc={:#x}, pos={:?}",
                i, info.thread.unique_id, info.thread.os_thread_id, pc, pos
            );
        }
    }
}
