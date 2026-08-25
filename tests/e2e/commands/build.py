"""Build the Go/C/C++ programs used by the source-level e2e tests.

Build outputs are ignored by git; only the sources are versioned.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tomllib
from pathlib import Path

from harness.common import PROGRAMS, no_window_kwargs


def run(command: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> None:
    print("==>", " ".join(command))
    subprocess.run(command, cwd=cwd, env=env, check=True, **no_window_kwargs())


def build_go() -> None:
    for main in sorted((PROGRAMS / "go").glob("*/main.go")):
        name = main.parent.name
        env = os.environ.copy()
        if name in {"cgocrash"}:
            env["CGO_ENABLED"] = "1"
            env["CC"] = env.get("CC", "gcc")
        run(
            ["go", "build", "-gcflags=all=-N -l", "-o", f"{name}.exe", "."],
            cwd=main.parent,
            env=env,
        )


def build_c() -> None:
    for directory in sorted(p for p in (PROGRAMS / "c").iterdir() if p.is_dir()):
        sources = sorted(directory.glob("*.c")) + sorted(directory.glob("*.cpp"))
        if not sources:
            continue
        source = sources[0]
        compiler = "g++" if source.suffix == ".cpp" else "gcc"
        config = directory / "build.toml"
        flags = (
            tomllib.loads(config.read_text(encoding="utf-8")).get("flags", [])
            if config.is_file()
            else []
        )
        output = directory / f"{directory.name}.exe"
        run([compiler, "-g", "-O0", *flags, "-o", str(output), source.name], cwd=directory)


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--skip-go", action="store_true")
    parser.add_argument("--skip-c", action="store_true")


def main(args: argparse.Namespace) -> int:
    if not PROGRAMS.is_dir():
        print(f"missing programs directory: {PROGRAMS}", file=sys.stderr)
        return 2
    try:
        if not args.skip_go:
            build_go()
        if not args.skip_c:
            build_c()
    except subprocess.CalledProcessError as exc:
        print(f"build failed: {exc}", file=sys.stderr)
        return 1
    return 0
