"""Paths, case discovery, child environment, and small shared helpers."""

from __future__ import annotations

import difflib
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

E2E_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = E2E_DIR.parents[1]
PROGRAMS = E2E_DIR / "programs"
TRACES = E2E_DIR / "traces"
OUT = E2E_DIR / "out"
SERVER = REPO_ROOT / "target" / "debug" / "ttd-gdbserver.exe"

NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def no_window_kwargs() -> dict[str, int]:
    """Windows: keep child gdb/dlv/server processes headless."""
    return {"creationflags": NO_WINDOW} if os.name == "nt" else {}


@dataclass(frozen=True)
class Case:
    """One program directory under `programs/<lang>/<name>`."""

    name: str  # "c-core", "go-core"
    directory: Path
    client: str  # "gdb" | "delve"
    exe: str  # executable, relative to PROGRAMS
    trace: str  # trace file name, relative to TRACES

    @property
    def exe_path(self) -> Path:
        return PROGRAMS / self.exe

    @property
    def trace_path(self) -> Path:
        return TRACES / self.trace

    def script(self, stem: str) -> Path:
        """The case's `<stem>.dlv` or `<stem>.gdb`, whichever its client reads."""
        extension = "dlv" if self.client == "delve" else "gdb"
        return self.directory / f"{stem}.{extension}"


# (subdirectory, client, script extension, case-name prefix)
_LANGS = (("go", "delve", "dlv", "go-"), ("c", "gdb", "gdb", "c-"))


def load_cases(script_stem: str = "forward") -> list[Case]:
    """Discover program directories carrying a `<script_stem>` script."""
    cases: list[Case] = []
    for lang, client, extension, prefix in _LANGS:
        root = PROGRAMS / lang
        if not root.is_dir():
            continue
        for directory in sorted(p for p in root.iterdir() if p.is_dir()):
            if not (directory / f"{script_stem}.{extension}").is_file():
                continue
            name = prefix + directory.name.replace("_", "-")
            cases.append(
                Case(
                    name=name,
                    directory=directory,
                    client=client,
                    exe=f"{lang}/{directory.name}/{directory.name}.exe",
                    trace=f"{name}.run",
                )
            )
    return cases


def select_cases(
    cases: list[Case],
    only: list[str] | None,
    *,
    client: str = "all",
    skip: list[str] = (),
) -> list[Case]:
    """Filter by client/skip/only, refusing `--only` names that match nothing.

    A typo would otherwise select nothing and the gate would report
    "0 failure(s)" — a green run that tested nothing.
    """
    cases = [
        case
        for case in cases
        if (client == "all" or case.client == client) and case.name not in skip
    ]
    if not only:
        return cases
    unknown = sorted(set(only) - {case.name for case in cases})
    if unknown:
        raise SystemExit(
            f"unknown program(s): {', '.join(unknown)}\n"
            f"known: {', '.join(sorted(case.name for case in cases))}"
        )
    return [case for case in cases if case.name in set(only)]


def require_built(cases: list[Case]) -> None:
    """Every selected case needs its executable and its trace."""
    missing = [
        case.name
        for case in cases
        if not case.exe_path.is_file() or not case.trace_path.is_file()
    ]
    if missing:
        raise SystemExit(
            "not built/recorded, run `mise run build` and `mise run record`: "
            + ", ".join(missing)
        )


def child_env(dlv: Path | None = None) -> dict[str, str]:
    """Environment for client/server child processes.

    The program under test loads its runtime DLLs through PATH. Pin the
    toolchain that built it (the live run must load the same libgcc/
    libstdc++ the trace recorded; otherwise symbolization differs from the
    replay for reasons that have nothing to do with the server).
    """
    env = os.environ.copy()
    path_parts = []
    if dlv is not None:
        path_parts.append(str(dlv.parent))
    for key in ("DELVE_DIR", "TTD_DIR"):
        value = os.environ.get(key)
        if value:
            path_parts.append(value)
    cc = shutil.which(os.environ.get("CC", "gcc"))
    if cc:
        path_parts.append(str(Path(cc).parent))
    path_parts.append(env.get("PATH", ""))
    env["PATH"] = os.pathsep.join(path_parts)
    env["TERM"] = "dumb"
    env["DELVE_TTD_SERVER"] = str(SERVER)
    return env


def case_out(case: Case, gate: str) -> Path:
    """The directory holding one case's artifacts for one gate.

    Layout is per case, so everything for a failing program sits together:

        out/<case>/forward/{live,replay}.out  live.stdout  live.stderr
        out/<case>/forward/{live,replay}.gdb
        out/<case>/reverse/replay.{gdb,out}

    `out/` is gitignored.
    """
    path = OUT / case.name / gate
    path.mkdir(parents=True, exist_ok=True)
    return path


def make_diff(expected: str, actual: str, name: str) -> str:
    return "\n".join(
        difflib.unified_diff(
            expected.splitlines(),
            actual.splitlines(),
            fromfile=name,
            tofile="current replay",
            lineterm="",
        )
    )
