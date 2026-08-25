#!/usr/bin/env python3
"""Compare forward and reverse client transcripts with local baselines."""

import argparse
import difflib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent
CASES = ROOT / "cases"
SERVER = REPO / "target" / "debug" / "ttd-gdbserver.exe"
SCRIPTS = Path(tempfile.gettempdir()) / "ttd-regression"
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)
DIRECTIONS = ("forward", "reverse")
NOISE = (
    "Opening trace:",
    "[ttd]",
    "GDB RSP server",
    "GDB client connected",
    "Remote communication error",
)
SCRIPT_ERRORS = (
    "Error in sourced command file",
    'No symbol "',
    "could not find symbol",
    "command not available",
    "Undefined command",
    "not defined.",
    "illegal character U+",
    "not enough arguments",
    "Unable to evaluate expression",
    "unknown command",
)
LOCATION_ERROR = re.compile(r'location ".+" not found')


def no_window() -> dict[str, int]:
    return {"creationflags": NO_WINDOW} if os.name == "nt" else {}


@dataclass(frozen=True)
class Case:
    name: str
    directory: Path
    client: str

    @property
    def exe_path(self) -> Path:
        return self.directory / f"{self.name}.exe"

    @property
    def trace_path(self) -> Path:
        return self.directory / f"{self.name}.run"

    def script(self, direction: str) -> Path:
        extension = "dlv" if self.client == "delve" else "gdb"
        return self.directory / f"{direction}.{extension}"


def load_cases() -> list[Case]:
    cases = []
    for directory in sorted(path for path in CASES.iterdir() if path.is_dir()):
        for client, extension in (("delve", "dlv"), ("gdb", "gdb")):
            if (directory / f"forward.{extension}").is_file():
                cases.append(Case(directory.name, directory, client))
                break
    return cases


def select_cases(cases: list[Case], names: list[str]) -> list[Case]:
    unknown = sorted(set(names) - {case.name for case in cases})
    if unknown:
        raise SystemExit(
            f"unknown case(s): {', '.join(unknown)}\n"
            f"known: {', '.join(case.name for case in cases)}"
        )
    return [case for case in cases if not names or case.name in names]


def child_env() -> dict[str, str]:
    env = os.environ.copy()
    # Only the TTD folder joins PATH: its runtime DLLs are loaded by name.
    # Delve is addressed by DELVE_PATH, so its folder stays out of PATH.
    parts = [os.environ["TTD_DIR"]] if os.environ.get("TTD_DIR") else []
    cc = shutil.which(os.environ.get("CC", "gcc"))
    if cc:
        parts.append(str(Path(cc).parent))
    parts.append(env.get("PATH", ""))
    env["PATH"] = os.pathsep.join(parts)
    env["TERM"] = "dumb"
    env["DELVE_TTD_SERVER"] = str(SERVER)
    return env


class Server:
    def __init__(self, trace: Path, timeout: float = 600.0) -> None:
        self.proc = subprocess.Popen(
            [str(SERVER), str(trace), "--listen", "127.0.0.1:0"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            env=child_env(),
            **no_window(),
        )
        self.lines: list[str] = []
        self.port: int | None = None
        listening = threading.Event()
        threading.Thread(target=self._drain, args=(listening,), daemon=True).start()
        deadline = time.monotonic() + timeout
        while not listening.wait(0.1):
            if self.proc.poll() is not None:
                raise RuntimeError("server exited:\n" + "\n".join(self.lines[-10:]))
            if time.monotonic() >= deadline:
                self.stop()
                raise RuntimeError(f"server did not listen: {self.lines[-5:]}")

    def _drain(self, listening: threading.Event) -> None:
        for line in self.proc.stdout:
            self.lines.append(line.rstrip())
            match = re.search(r"Listening on \S+:(\d+)", line)
            if match and self.port is None:
                self.port = int(match.group(1))
                listening.set()

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)


def render_gdb(case: Case, direction: str, **tokens: str) -> str:
    path = case.script(direction)
    text = path.read_text(encoding="utf-8")
    for token, value in tokens.items():
        marker = f"@{token}@"
        if marker not in text:
            raise RuntimeError(f"{path} needs {marker}")
        text = text.replace(marker, value)
    return text


def log_path(case: Case, direction: str) -> Path:
    extension = "dlv.log" if case.client == "delve" else "gdb.log"
    return SCRIPTS / f"{case.name}-{direction}.{extension}"


def run_gdb(case: Case, direction: str, script: str, timeout: float) -> str:
    """Run a GDB script, letting GDB write its own transcript.

    GDB has a native output file (`set logging`), so the script itself opens it;
    stdout only carries what GDB prints before logging starts, which is kept as
    a fallback.
    """
    SCRIPTS.mkdir(parents=True, exist_ok=True)
    log = log_path(case, direction)
    log.unlink(missing_ok=True)
    path = SCRIPTS / f"{case.name}-{direction}.gdb"
    path.write_text(
        f"set logging file {log.as_posix()}\n"
        "set logging redirect on\n"
        "set logging enabled on\n"
        f"{script}",
        encoding="utf-8",
        newline="\n",
    )
    early = SCRIPTS / f"{case.name}-{direction}.stdout"
    with early.open("wb") as handle:
        subprocess.run(
            ["gdb", "-q", "-batch", "-nx", "-x", str(path)],
            stdout=handle,
            stderr=subprocess.STDOUT,
            cwd=REPO,
            env=child_env(),
            timeout=timeout,
            **no_window(),
        )
    return (log if log.is_file() else early).read_text(encoding="utf-8", errors="replace")


def run_delve(case: Case, direction: str, timeout: float) -> str:
    """Run Delve with its transcript attached to a file.

    `dlv` has no option for its own output (`--redirect` targets the inferior's
    stdio), so the child's stdout is attached to the log file directly. The
    executable comes from `DELVE_PATH`, which keeps a stock `dlv.exe` on PATH
    from being picked instead.
    """
    SCRIPTS.mkdir(parents=True, exist_ok=True)
    log = log_path(case, direction)
    with log.open("wb") as handle:
        proc = subprocess.run(
            [
                os.environ.get("DELVE_PATH", "dlv.exe"),
                "ttd",
                str(case.trace_path),
                str(case.exe_path),
                "--init",
                str(case.script(direction)),
                "--allow-non-terminal-interactive=true",
            ],
            stdout=handle,
            stderr=subprocess.STDOUT,
            env=child_env(),
            timeout=timeout,
            **no_window(),
        )
    if proc.returncode:
        raise RuntimeError(f"dlv exited with {proc.returncode}; log: {log}")
    return log.read_text(encoding="utf-8", errors="replace")


def transcript(text: str) -> str:
    return "\n".join(
        line
        for line in text.replace("\r\n", "\n").splitlines()
        if not line.startswith(NOISE)
    ).rstrip()


def replay(case: Case, direction: str, timeout: float) -> str:
    if case.client == "delve":
        return transcript(run_delve(case, direction, timeout))
    server = Server(case.trace_path)
    try:
        tokens = {"FILE": f"file {case.exe_path.as_posix()}"}
        target = f"target remote 127.0.0.1:{server.port}"
        if direction == "forward":
            tokens["RUN"] = f"{target}\ncontinue"
        else:
            tokens["TARGET"] = target
        return transcript(run_gdb(case, direction, render_gdb(case, direction, **tokens), timeout))
    finally:
        server.stop()


def check(case: Case, direction: str, update: bool, timeout: float) -> tuple[str, str]:
    actual = replay(case, direction, timeout)
    errors = [
        line
        for line in actual.splitlines()
        if LOCATION_ERROR.search(line) or any(marker in line for marker in SCRIPT_ERRORS)
    ]
    if errors:
        return "FAIL", "client script failed:\n" + "\n".join(errors)
    baseline = case.directory / f"{direction}.base"
    if update:
        baseline.write_text(actual + "\n", encoding="utf-8")
        return "PASS", "baseline updated"
    if not baseline.is_file():
        return "FAIL", f"missing {baseline}; run `mise run baseline -- {case.name}`"
    expected = baseline.read_text(encoding="utf-8").rstrip()
    if actual != expected:
        return "FAIL", "\n".join(
            difflib.unified_diff(
                expected.splitlines(),
                actual.splitlines(),
                fromfile=str(baseline),
                tofile="current",
                lineterm="",
            )
        ) + f"\n(raw transcript: {log_path(case, direction)})"
    return "PASS", ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--update", action="store_true")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=300.0)
    args = parser.parse_args()
    cases = select_cases(load_cases(), args.cases)
    missing = [
        case.name for case in cases if not case.exe_path.is_file() or not case.trace_path.is_file()
    ]
    if missing:
        raise SystemExit("build and record first: " + ", ".join(missing))
    work = [(case, direction) for case in cases for direction in DIRECTIONS]
    started = time.perf_counter()

    def one(item: tuple[Case, str]) -> tuple[Case, str, str, str, float]:
        case, direction = item
        before = time.perf_counter()
        try:
            status, detail = check(case, direction, args.update, args.timeout)
        except Exception as exc:
            status, detail = "FAIL", str(exc)
        return case, direction, status, detail, time.perf_counter() - before

    if args.update or args.jobs == 1:
        results = [one(item) for item in work]
    else:
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            results = list(pool.map(one, work))
    failures = 0
    for case, direction, status, detail, elapsed in results:
        failures += status == "FAIL"
        print(f"{case.client:5} {case.name:22} {direction:7} {status} {elapsed:6.2f}s")
        if detail:
            print("\n".join("    " + line for line in detail.splitlines()[:60]))
    print(f"done: {failures} failure(s) in {time.perf_counter() - started:.1f}s")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
