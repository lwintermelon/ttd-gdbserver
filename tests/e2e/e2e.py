#!/usr/bin/env python3
"""Source-level e2e test driver for ttd-gdbserver.

One entry point for every versioned task; each subcommand lives in
`commands/`, shared machinery in `harness/`:

    python tests/e2e/e2e.py build              # compile the programs
    python tests/e2e/e2e.py record             # record traces (elevated)
    python tests/e2e/e2e.py forward [--record] # replay vs live snapshot
    python tests/e2e/e2e.py reverse [--record] # replay vs reverse snapshot

The gates accept `--only <cases>` and `--client gdb|delve`. See
tests/e2e/README.md.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from commands import build, forward, record, reverse

COMMANDS = {
    "build": build,
    "record": record,
    "forward": forward,
    "reverse": reverse,
}


def main() -> int:
    parser = argparse.ArgumentParser(
        prog="e2e.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = parser.add_subparsers(dest="command", required=True)
    for name, module in COMMANDS.items():
        help_line = (module.__doc__ or "").strip().splitlines()[0]
        module.add_arguments(sub.add_parser(name, help=help_line))
    args = parser.parse_args()
    return COMMANDS[args.command].main(args)


if __name__ == "__main__":
    sys.exit(main())
