"""Parallel per-case execution with uniform PASS/FAIL/SKIP reporting.

A command implements one function `run(case) -> (status, detail)` where
status is "PASS", "FAIL" or "SKIP"; `run_cases` handles threading, timing,
exception containment and the summary line.
"""

from __future__ import annotations

import argparse
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor

from harness.common import Case


def add_gate_arguments(
    parser: argparse.ArgumentParser,
    *,
    record: bool = False,
    timeout: float = 120.0,
) -> None:
    """The CLI options every transcript gate shares."""
    parser.add_argument("--client", choices=["all", "delve", "gdb"], default="all")
    parser.add_argument("--only", nargs="+")
    if record:
        parser.add_argument("--record", action="store_true", help="refresh the snapshot")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=timeout)


def run_cases(
    cases: list[Case],
    run: Callable[[Case], tuple[str, str]],
    *,
    jobs: int = 4,
    serial: bool = False,
    max_detail_lines: int = 40,
) -> int:
    """Run `run` over `cases`; returns a process exit code."""
    started_all = time.perf_counter()

    def one(case: Case) -> tuple[Case, str, str, float]:
        started = time.perf_counter()
        try:
            status, detail = run(case)
        except Exception as exc:  # noqa: BLE001 - report one program, continue
            status, detail = "FAIL", str(exc)
        return case, status, detail, time.perf_counter() - started

    if serial or jobs <= 1:
        results = [one(case) for case in cases]
    else:
        with ThreadPoolExecutor(max_workers=jobs) as pool:
            results = list(pool.map(one, cases))

    failures = 0
    for case, status, detail, elapsed in results:
        if status == "FAIL":
            failures += 1
        line = f"{case.client:5} {case.name:22} {status} {elapsed:6.2f}s"
        if detail and "\n" not in detail and status != "FAIL":
            line += f"  {detail}"
            detail = ""
        print(line)
        if detail and status != "PASS":
            print("\n".join("    " + line for line in detail.splitlines()[:max_detail_lines]))
    print(f"done: {failures} failure(s) in {time.perf_counter() - started_all:.1f}s")
    return 1 if failures else 0
