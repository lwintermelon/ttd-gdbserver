"""Record the source-level e2e programs with Microsoft TTD.

TTD's recorder requires an elevated token: run this command from an elevated
terminal.

    python tests/e2e/e2e.py record
    python tests/e2e/e2e.py record --only c-core go-core

Trace files are ignored by git.
"""

from __future__ import annotations

import argparse
import ctypes
import os
import subprocess
import sys
from pathlib import Path

from harness.common import E2E_DIR, Case, load_cases, no_window_kwargs

DEFAULT_TRACES = E2E_DIR / "traces"


def record_cases(cases: list[Case], ttd: Path, traces: Path) -> int:
    """Record the selected cases; returns a process-style status."""
    traces.mkdir(parents=True, exist_ok=True)
    for case in cases:
        exe = case.exe_path
        if not exe.is_file():
            print(f"missing {exe}; run `e2e.py build` first", file=sys.stderr)
            return 2
        out = traces / case.trace
        print(f"==> recording {case.name} -> {out}", flush=True)
        result = subprocess.run(
            [str(ttd), "-acceptEula", "-noUI", "-out", str(out), "-launch", str(exe)],
            **no_window_kwargs(),
        )
        if result.returncode != 0:
            print(f"recording {case.name} failed: exit {result.returncode}", file=sys.stderr)
            return 1
    return 0


def is_admin() -> bool:
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except (AttributeError, OSError):
        return False


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--ttd",
        default=os.environ.get("TTD_EXE", ""),
        help="path to TTD.exe (or set TTD_EXE)",
    )
    parser.add_argument("--traces", type=Path, default=DEFAULT_TRACES)
    parser.add_argument("--only", nargs="+", default=None)


def main(args: argparse.Namespace) -> int:
    ttd = Path(args.ttd) if args.ttd else None
    if ttd is None or not ttd.is_file():
        print("pass --ttd <TTD.exe> or set TTD_EXE", file=sys.stderr)
        return 2

    cases = load_cases()
    selected = set(args.only) if args.only else {case.name for case in cases}
    unknown = selected - {case.name for case in cases}
    if unknown:
        print(f"unknown program(s): {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2
    chosen = [case for case in cases if case.name in selected]

    if not is_admin():
        print(
            "TTD recording requires elevation: run this command from an\n"
            "elevated terminal.",
            file=sys.stderr,
        )
        return 2
    return record_cases(chosen, ttd, args.traces)
