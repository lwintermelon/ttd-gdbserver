"""Client and server process management.

Two launch primitives cover every gate: `run_gdb` (a batch GDB script whose
transcript is saved next to the script) and `run_delve` (live `dlv exec` or
`dlv ttd` replay with `--init`). `Server` is a ttd-gdbserver process whose
stdout is drained on a reader thread.
"""

from __future__ import annotations

import os
import re
import subprocess
import threading
from pathlib import Path

from harness.common import (
    REPO_ROOT,
    SERVER,
    Case,
    child_env,
    no_window_kwargs,
)


class Server:
    """A `ttd-gdbserver` process serving one trace on an ephemeral port.

    stdout is drained on a reader thread rather than with `readline()` on the
    caller's thread: a blocking read has no deadline (a server that hangs
    before printing `Listening on` would hang the whole gate), and nobody
    draining it afterwards would deadlock a verbose server once the pipe
    buffer fills. `stop()` terminates and reaps.
    """

    def __init__(self, trace: Path, deadline: float = 120.0) -> None:
        self.proc = subprocess.Popen(
            [str(SERVER), str(trace), "--listen", "127.0.0.1:0"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            env=child_env(),
            **no_window_kwargs(),
        )
        self.lines: list[str] = []
        self.port: int | None = None
        listening = threading.Event()
        threading.Thread(target=self._drain, args=(listening,), daemon=True).start()
        if not listening.wait(deadline):
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


def run_gdb(script: str, path: Path, timeout: float) -> str:
    """Run a GDB batch script written to `path` (passed to GDB repo-relative).

    The path is stable rather than a temporary file, so a GDB diagnostic that
    quotes the script path is deterministic and belongs in the transcript
    instead of having to be normalized away.

    The raw output is also saved next to the script as `<stem>.out`, so a
    failing GDB case leaves the same artifacts a failing Delve case does
    (`out/<case>/<gate>/{replay.gdb,replay.out}`).
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(script, encoding="utf-8", newline="\n")
    relative = path.relative_to(REPO_ROOT).as_posix()
    proc = subprocess.run(
        ["gdb", "-q", "-batch", "-nx", "-x", relative],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        cwd=REPO_ROOT,
        env=child_env(),
        timeout=timeout,
        **no_window_kwargs(),
    )
    text = proc.stdout.decode("utf-8", "replace")
    path.with_suffix(".out").write_text(text, encoding="utf-8", errors="replace")
    return text


def render_gdb_script(case: Case, stem: str, **tokens: str) -> str:
    """The case's `<stem>.gdb` with `@TOKEN@` placeholders substituted."""
    path = case.script(stem)
    text = path.read_text(encoding="utf-8")
    for token, value in tokens.items():
        marker = f"@{token}@"
        if marker not in text:
            raise RuntimeError(f"{path} needs {marker}")
        text = text.replace(marker, value)
    return text


def run_dlv(args: list[str], out: Path, timeout: float) -> subprocess.CompletedProcess:
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("wb") as handle:
        try:
            return subprocess.run(
                args,
                stdout=handle,
                stderr=subprocess.STDOUT,
                env=child_env(),
                timeout=timeout,
                **no_window_kwargs(),
            )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(f"dlv timed out after {timeout:.0f}s") from exc


def run_delve(
    case: Case,
    mode: str,
    script: Path,
    out: Path,
    timeout: float,
    redirect: tuple[tuple[str, Path], ...] = (),
) -> str:
    """Run Delve on `case` with `--init script`; returns the raw transcript.

    `mode` is "live" (`dlv exec`) or "replay" (`dlv ttd`). `redirect` pairs a
    stream name with its target file (`--redirect stdout:…`), used by live
    runs to keep program output out of the transcript.
    """
    if not script.is_file():
        raise RuntimeError(f"missing {script}")
    dlv = os.environ.get("TTD_DLV", "dlv.exe")
    if mode == "live":
        args = [dlv, "exec", str(case.exe_path)]
    else:
        args = [dlv, "ttd", str(case.trace_path), str(case.exe_path)]
    args += ["--init", str(script), "--allow-non-terminal-interactive=true"]
    for stream, target in redirect:
        args += ["--redirect", f"{stream}:{target}"]
    proc = run_dlv(args, out, timeout)
    if proc.returncode != 0:
        raise RuntimeError(f"{mode} dlv exit {proc.returncode}")
    return out.read_text(encoding="utf-8", errors="replace")
