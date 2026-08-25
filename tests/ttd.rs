mod common;

mod engine {
    use ttd_gdbserver::ttd::TtdEngine;

    fn create_engine() -> TtdEngine {
        super::common::create_engine()
    }

    #[test]
    fn test_engine_create_and_load() {
        let _engine = create_engine();
    }

    #[test]
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
    fn create_engine() -> ttd_gdbserver::ttd::TtdEngine {
        super::common::create_engine()
    }

    #[test]
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
    fn test_cursor_step_forward() {
        let engine = create_engine();
        let mut cursor = engine.create_cursor().expect("create cursor");
        let pos_before = cursor.position();
        let _ = cursor.step_forward(1).expect("step forward");
        let pos_after = cursor.position();
        println!("Step 1: {:?} -> {:?}", pos_before, pos_after);
        assert_ne!(pos_before, pos_after, "Position should change after step");
    }

    #[test]
    fn test_cursor_step_backward() {
        let engine = create_engine();
        let mut cursor = engine.create_cursor().expect("create cursor");
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
    fn test_cursor_watchpoint() {
        let engine = create_engine();
        let mut cursor = engine.create_cursor().expect("create cursor");
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

    #[test]
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
