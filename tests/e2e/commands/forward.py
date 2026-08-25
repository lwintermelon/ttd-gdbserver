"""Forward live-vs-replay differential against a recorded live snapshot.

Each program directory owns its forward script and one snapshot:

    programs/go/core/forward.dlv
    programs/go/core/snapshots/live.snap

`live.snap` is the normalized transcript of ONE recorded live run. A normal
run executes only the replay, normalizes it identically, and diffs it against
that snapshot. `--record` runs live + replay once, refuses to write when they
disagree, and replaces the snapshot. Re-record when a script changes (mise
tracks the scripts as task sources, so the snapshot tasks rebuild then).
"""

from __future__ import annotations

import argparse

from harness.clients import Server, render_gdb_script, run_delve, run_gdb
from harness.common import Case, case_out, load_cases, make_diff, require_built, select_cases
from harness.normalize import BODY_MARKER, body_of, normalize_delve, normalize_gdb, script_errors
from harness.runner import add_gate_arguments, run_cases

SNAPSHOT = "snapshots/live.snap"


def delve_run(case: Case, mode: str, timeout: float) -> str:
    gate = case_out(case, "forward")
    redirect = ()
    if mode == "live":
        redirect = (("stdout", gate / "live.stdout"), ("stderr", gate / "live.stderr"))
    text = run_delve(
        case, mode, case.script("forward"), gate / f"{mode}.out", timeout, redirect
    )
    return normalize_delve(text)


def gdb_run(case: Case, mode: str, timeout: float) -> str:
    marker = f'printf "{BODY_MARKER}\\n"'
    # The injected live block has the same number of lines as the replay one,
    # so a GDB diagnostic quoting a line number reads identically in both runs.
    if mode == "live":
        run = f"run > NUL 2>&1\n# live\n{marker}"
        text = run_gdb(
            render_gdb_script(case, "forward", FILE=f"file {case.exe_path.as_posix()}", RUN=run)
            + "\n",
            case_out(case, "forward") / "live.gdb",
            timeout,
        )
        return normalize_gdb(body_of(text))
    server = Server(case.trace_path)
    try:
        run = f"target remote 127.0.0.1:{server.port}\ncontinue\n{marker}"
        text = run_gdb(
            render_gdb_script(case, "forward", FILE=f"file {case.exe_path.as_posix()}", RUN=run)
            + "\n",
            case_out(case, "forward") / "replay.gdb",
            timeout,
        )
    finally:
        server.stop()
    return normalize_gdb(body_of(text))


def run_case(case: Case, record: bool, timeout: float) -> tuple[str, str]:
    run_side = delve_run if case.client == "delve" else gdb_run
    replay = run_side(case, "replay", timeout)
    errors = script_errors(replay)
    if errors:
        return "FAIL", "the script failed on the replay side:\n" + "\n".join(errors)
    snapshot = case.directory / SNAPSHOT
    if record:
        live = run_side(case, "live", timeout)
        if live != replay:
            return "FAIL", "refusing to record: live != replay\n" + make_diff(
                live, replay, "live"
            )
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        snapshot.write_text(live.rstrip() + "\n", encoding="utf-8")
        return "PASS", "recorded live snapshot"
    if not snapshot.is_file():
        return "FAIL", f"missing {snapshot}; run `e2e.py forward --record`"
    expected = snapshot.read_text(encoding="utf-8").rstrip()
    if expected != replay.rstrip():
        return "FAIL", "replay differs from the live snapshot\n" + make_diff(
            expected, replay, "live snapshot"
        )
    return "PASS", "replay matches the live snapshot"


def add_arguments(parser: argparse.ArgumentParser) -> None:
    add_gate_arguments(parser, record=True)


def main(args: argparse.Namespace) -> int:
    cases = select_cases(load_cases(), args.only, client=args.client)
    require_built(cases)
    return run_cases(
        cases,
        lambda case: run_case(case, args.record, args.timeout),
        jobs=args.jobs,
        serial=args.record,
    )
