//! Reverse-debugging command handling (`qRRCmd`, `vRun`).
//!
//! `qRRCmd` is rr's reverse-debugging command vocabulary (`when`,
//! checkpoints), also spoken by Delve's gdbserial client; `vRun` is the
//! standard extended-remote restart form. These commands need only a small
//! slice of the debug target: the current position, the trace lifetime, and
//! `goto`. Defining `CommandTarget` keeps the command logic out of
//! `GdbTarget` and makes it unit-testable with a plain recording stub
//! instead of a real `TtdProcess`.

use crate::target::{DebugError, DebugTarget};
use crate::ttd::types::TtdPosition;

use super::checkpoint::{CheckpointTable, format_position};
use super::packet::parse_qrrcmd_args;

/// The command-visible slice of a debug target.
pub(super) trait CommandTarget {
    fn position(&self) -> TtdPosition;
    fn lifetime(&self) -> (TtdPosition, TtdPosition);
    fn goto(&mut self, pos: TtdPosition) -> Result<(), DebugError>;
}

impl<T: DebugTarget> CommandTarget for T {
    fn position(&self) -> TtdPosition {
        DebugTarget::position(self)
    }

    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        DebugTarget::lifetime(self)
    }

    fn goto(&mut self, pos: TtdPosition) -> Result<(), DebugError> {
        // The stop reason is irrelevant for a command-level jump; the
        // destination is observable through `position()`. The error, however,
        // must survive: acknowledging a restart that did not happen would
        // leave the client's view of the trace wrong.
        DebugTarget::goto(self, pos).map(|_| ())
    }
}

/// Answer rr's `qRRCmd` (`when`, `checkpoint`, `info checkpoints`,
/// `delete checkpoint`). Everything else is an empty reply, which is what rr
/// does for commands it doesn't know.
pub(super) fn handle_qrrcmd(
    backend: &dyn CommandTarget,
    checkpoints: &mut CheckpointTable,
    args: &str,
) -> String {
    let args = parse_qrrcmd_args(args);
    if args.is_empty() {
        return String::new();
    }
    match args[0].as_str() {
        "when" => format_position(backend.position()),
        "checkpoint" => {
            let where_ = args.get(1).cloned().unwrap_or_default();
            let pos = backend.position();
            checkpoints.insert(pos, &where_).1
        }
        "info checkpoints" => checkpoints.render(),
        "delete checkpoint" => match args.get(1).and_then(|s| s.parse::<u64>().ok()) {
            Some(id) if checkpoints.remove(id) => format!("Deleted checkpoint {id}"),
            _ => "No such checkpoint".to_string(),
        },
        _ => String::new(),
    }
}

/// Handle `vRun[;arg...]` (Delve restart forms: position, checkpoint, empty).
///
/// Returns the failure instead of logging and swallowing it: the RSP caller
/// turns this into an error reply, so the client never believes a restart
/// happened when it did not.
///
/// # Errors
/// [`DebugError::InvalidCommand`] for a malformed position or an unknown
/// checkpoint, and whatever [`CommandTarget::goto`] reports when the target
/// cannot move.
pub(super) fn handle_vrun(
    backend: &mut dyn CommandTarget,
    checkpoints: &CheckpointTable,
    args: &mut dyn Iterator<Item = &[u8]>,
) -> Result<(), DebugError> {
    let pos_str = match args.next() {
        Some(s) => String::from_utf8_lossy(s).trim().to_string(),
        None => String::new(),
    };
    log::debug!("vRun pos_str={pos_str:?}");

    // Positions are always "SEQ:STEPS" (hex, colon-separated); checkpoint ids
    // are bare "c<N>". The position branch must come first: a sequence id may
    // itself start with the hex digit 'c' ("cafe:12"), which the checkpoint
    // prefix check would otherwise swallow.
    if let Some((seq, steps)) = pos_str.split_once(':') {
        let (Ok(sequence), Ok(steps)) =
            (u64::from_str_radix(seq, 16), u64::from_str_radix(steps, 16))
        else {
            return Err(DebugError::InvalidCommand(format!(
                "vRun: malformed position '{pos_str}'"
            )));
        };
        backend.goto(TtdPosition::new(sequence, steps))?;
        log::debug!("vRun -> position {:?}", backend.position());
        return Ok(());
    }

    let target = if pos_str.is_empty() {
        backend.lifetime().0
    } else if let Some(id_str) = pos_str
        .strip_prefix('c')
        .or_else(|| pos_str.strip_prefix('C'))
    {
        let id = id_str.parse::<u64>().map_err(|_| {
            DebugError::InvalidCommand(format!("vRun: unrecognized restart target '{pos_str}'"))
        })?;
        checkpoints
            .position(id)
            .ok_or_else(|| DebugError::InvalidCommand(format!("vRun: no such checkpoint {id}")))?
    } else {
        return Err(DebugError::InvalidCommand(format!(
            "vRun: unrecognized restart target '{pos_str}'"
        )));
    };
    backend.goto(target)?;
    log::debug!("vRun -> position {:?}", backend.position());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition::new(sequence, steps)
    }

    #[derive(Default)]
    struct Stub {
        position: TtdPosition,
        lifetime: (TtdPosition, TtdPosition),
        gotos: Vec<TtdPosition>,
        fail_goto: bool,
    }

    impl CommandTarget for Stub {
        fn position(&self) -> TtdPosition {
            self.position
        }

        fn lifetime(&self) -> (TtdPosition, TtdPosition) {
            self.lifetime
        }

        fn goto(&mut self, pos: TtdPosition) -> Result<(), DebugError> {
            if self.fail_goto {
                return Err(DebugError::InvalidCommand("stub goto failed".to_string()));
            }
            self.gotos.push(pos);
            self.position = pos;
            Ok(())
        }
    }

    #[test]
    fn qrrcmd_when_formats_position() {
        let stub = Stub {
            position: pos(0xA, 0x1F),
            ..Default::default()
        };
        assert_eq!(
            "A:1F",
            handle_qrrcmd(&stub, &mut CheckpointTable::new(), ":when:-1")
        );
    }

    #[test]
    fn qrrcmd_checkpoint_info_and_delete() {
        let mut backend = Stub {
            position: pos(1, 2),
            ..Default::default()
        };
        let mut checkpoints = CheckpointTable::new();

        let ack = handle_qrrcmd(&backend, &mut checkpoints, ":checkpoint:-1:61");
        assert_eq!("Checkpoint 1 at 1:2 a", ack);
        assert_eq!(
            handle_qrrcmd(&backend, &mut checkpoints, ":info checkpoints:-1"),
            "Num\tWhen\tWhere\n1\t1:2\ta\n"
        );

        // Moving the backend does not move the saved checkpoint.
        backend.position = pos(9, 9);
        let ack = handle_qrrcmd(&backend, &mut checkpoints, ":checkpoint:-1");
        assert_eq!("Checkpoint 2 at 9:9", ack);
        assert_eq!(
            "Deleted checkpoint 1",
            handle_qrrcmd(&backend, &mut checkpoints, ":delete checkpoint:-1:31")
        );
        assert_eq!(
            "No such checkpoint",
            handle_qrrcmd(&backend, &mut checkpoints, ":delete checkpoint:-1:31")
        );
    }

    #[test]
    fn qrrcmd_unknown_is_empty() {
        let backend = Stub::default();
        assert_eq!(
            String::new(),
            handle_qrrcmd(&backend, &mut CheckpointTable::new(), ":bogus:-1")
        );
    }

    #[test]
    fn vrun_position_start_and_checkpoint() {
        let mut backend = Stub {
            position: pos(5, 5),
            lifetime: (pos(1, 0), pos(9, 9)),
            ..Default::default()
        };
        let mut checkpoints = CheckpointTable::new();
        checkpoints.insert(pos(3, 4), "saved");

        // Explicit position.
        let args: Vec<&[u8]> = vec![b"a:0"];
        handle_vrun(&mut backend, &checkpoints, &mut args.into_iter()).unwrap();
        assert_eq!(backend.position(), pos(0xA, 0));

        // No argument -> trace start.
        handle_vrun(&mut backend, &checkpoints, &mut std::iter::empty()).unwrap();
        assert_eq!(backend.position(), pos(1, 0));

        // Checkpoint id (case-insensitive prefix).
        let args: Vec<&[u8]> = vec![b"C1"];
        handle_vrun(&mut backend, &checkpoints, &mut args.into_iter()).unwrap();
        assert_eq!(backend.position(), pos(3, 4));
    }

    /// A restart the target cannot perform must be reported, not acked: the
    /// old handler logged and returned success, so a failed `vRun` looked like
    /// a successful jump to the client.
    #[test]
    fn vrun_rejects_unknown_checkpoints_and_malformed_positions() {
        let mut backend = Stub {
            position: pos(5, 5),
            lifetime: (pos(1, 0), pos(9, 9)),
            ..Default::default()
        };
        let checkpoints = CheckpointTable::new();

        for arg in ["c99", "C99", "not-a-position", "a:zz"] {
            let args: Vec<&[u8]> = vec![arg.as_bytes()];
            let err = handle_vrun(&mut backend, &checkpoints, &mut args.into_iter())
                .expect_err("unknown restart targets must fail");
            assert!(
                matches!(err, DebugError::InvalidCommand(_)),
                "{arg}: {err:?}"
            );
        }
        assert_eq!(
            backend.position(),
            pos(5, 5),
            "a rejected restart must not move the target"
        );
    }

    #[test]
    fn vrun_propagates_a_failed_goto() {
        let mut backend = Stub {
            position: pos(5, 5),
            lifetime: (pos(1, 0), pos(9, 9)),
            fail_goto: true,
            ..Default::default()
        };
        let args: Vec<&[u8]> = vec![b"1:0"];
        let err = handle_vrun(&mut backend, &CheckpointTable::new(), &mut args.into_iter())
            .expect_err("a failed goto must not be acknowledged");
        assert!(matches!(err, DebugError::InvalidCommand(_)), "{err:?}");
        assert_eq!(backend.position(), pos(5, 5));
    }
}
