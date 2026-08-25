"""Reverse-execution snapshot gate over the per-program e2e cases.

A live process cannot run backwards, so the oracle is a recorded transcript,
not a live comparison. Every reverse run replays the SAME trace, so the
transcript is deterministic: the snapshot diff is raw (addresses, positions
and thread ids are NOT masked — see `normalize_deterministic`), which makes
the gate sensitive to the smallest position or address regression.

    programs/c/core/reverse.gdb            # navigation + state dumps, no asserts
    programs/c/core/snapshots/reverse.snap

Scripts carry no in-band assertions (`FAIL` prints, `SCRIPT-DONE` markers, or
relation files): the full transcript IS the assertion. A regression changes
the printed state; a truncated run diffs against the longer snapshot.
"""

from __future__ import annotations

import argparse

from harness.clients import Server, render_gdb_script, run_delve, run_gdb
from harness.common import Case, case_out, load_cases, make_diff, require_built, select_cases
from harness.normalize import normalize_deterministic, script_errors
from harness.runner import add_gate_arguments, run_cases

SNAPSHOT = "snapshots/reverse.snap"


def check_or_record(case: Case, transcript: str, record: bool) -> tuple[str, str]:
    snapshot = case.directory / SNAPSHOT
    if record:
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        snapshot.write_text(transcript.rstrip() + "\n", encoding="utf-8")
        return "PASS", "recorded reverse snapshot"
    if not snapshot.is_file():
        return "FAIL", f"missing {snapshot}; run `e2e.py reverse --record`"
    expected = snapshot.read_text(encoding="utf-8").rstrip()
    if expected != transcript.rstrip():
        return "FAIL", "reverse output differs from snapshot\n" + make_diff(
            expected, transcript, "reverse snapshot"
        )
    return "PASS", "reverse output matches snapshot"


def delve_reverse(case: Case, timeout: float) -> str:
    gate = case_out(case, "reverse")
    text = run_delve(case, "replay", case.script("reverse"), gate / "replay.out", timeout)
    return normalize_deterministic(text)


def gdb_reverse(case: Case, timeout: float) -> str:
    gate = case_out(case, "reverse")
    server = Server(case.trace_path)
    try:
        body = render_gdb_script(
            case,
            "reverse",
            FILE=f"file {case.exe_path.as_posix()}",
            TARGET=f"target remote 127.0.0.1:{server.port}",
        )
        output = run_gdb(body + "\n", gate / "replay.gdb", timeout)
    finally:
        server.stop()
    return normalize_deterministic(output)


def run_case(case: Case, record: bool, timeout: float) -> tuple[str, str]:
    run_side = delve_reverse if case.client == "delve" else gdb_reverse
    transcript = run_side(case, timeout)
    errors = script_errors(transcript)
    if errors:
        return "FAIL", "the script failed: " + " | ".join(errors)
    return check_or_record(case, transcript, record)


def add_arguments(parser: argparse.ArgumentParser) -> None:
    add_gate_arguments(parser, record=True, timeout=300.0)


def main(args: argparse.Namespace) -> int:
    cases = select_cases(load_cases(), args.only, client=args.client)
    require_built(cases)
    return run_cases(
        cases,
        lambda case: run_case(case, args.record, args.timeout),
        jobs=args.jobs,
        serial=args.record,
    )
