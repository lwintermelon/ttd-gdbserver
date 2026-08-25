"""Transcript normalization and client script-error detection.

The snapshot gates diff normalized transcripts, so these functions are part
of the gates' contract with the committed snapshots: change them and every
snapshot must be re-recorded.

**Differential direction matters.** Forward compares two *different* worlds
(live process vs recorded trace), so its normalizer drops what legitimately
differs (hex addresses, pids/tids, positions). Reverse compares two runs of
the *same* trace, so `normalize_deterministic` compares program state byte
for byte — addresses, positions, breakpoint ids and thread ids all stay in
the diff.
"""

from __future__ import annotations

import re

BODY_MARKER = "===BODY==="


def body_of(text: str) -> str:
    """The transcript after the injected BODY_MARKER, or all of it."""
    if BODY_MARKER not in text:
        return text
    return text.split(BODY_MARKER, 1)[1].lstrip("\r\n")


# A client that failed to execute a script line tells us nothing about the
# server, and recording it would freeze a blind spot into the snapshot: the
# gate would then "pass" while never checking the state that line wanted.
#
# Matching the message text rather than a `script:line:` prefix is deliberate:
# Delve prefixes *every* diagnostic with the init file and line, including the
# harmless "Process N has exited with status 0" it reports once the inferior
# is gone, so a prefix match would fail a script for finishing successfully.
SCRIPT_ERROR_MARKERS = (
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
# Delve quotes a bad `break` location; the quoting keeps ordinary program
# output from matching.
SCRIPT_LOCATION_ERROR = re.compile(r'location ".+" not found')


def script_errors(text: str) -> list[str]:
    return [
        line
        for line in text.splitlines()
        if SCRIPT_LOCATION_ERROR.search(line)
        or any(m in line for m in SCRIPT_ERROR_MARKERS)
    ]


ANSI = re.compile(r"\x1b\[[0-9;]*m")
HEX = re.compile(r"0x[0-9a-fA-F]+")
POS = re.compile(r"\bpos [0-9a-f]+:[0-9a-f]+\b")
THREAD = re.compile(r"Thread \d+(?:\.\d+)?", re.IGNORECASE)
PROCESS = re.compile(r"[Pp]rocess \d+")
GOROUTINE = re.compile(r"goroutine\(\d+\)")
EXIT_STATUS = re.compile(r"has exited with status \d+")
# An absolute file path (Windows drive, POSIX temporary/build root, UNC).
# Everything up to the final separator is replaced, keeping the basename: the
# paths are properties of the machine that recorded the trace (where its
# toolchain, its build directory and its sources lived), not of the program
# state being compared. Addresses, positions and thread ids are deliberately
# left alone — they are fixed by the trace.
ABSOLUTE_PATH = re.compile(
    r"(?:[A-Za-z]:[\\/]|\\\\[^\s\\/]+\\[^\s\\/]+\\|/tmp/|/home/|/usr/|/build/)"
    r"(?:[^\s\"'(),;]*[\\/])?"
)


def normalize_paths(text: str) -> str:
    return ABSOLUTE_PATH.sub("<PATH>/", text)


def normalize_delve(text: str) -> str:
    text = normalize_paths(ANSI.sub("", text))
    text = HEX.sub("0xADDR", text)
    text = POS.sub("pos POS", text)
    text = PROCESS.sub("process PID", text)
    text = THREAD.sub("Thread TID", text)
    text = GOROUTINE.sub("goroutine(N)", text)
    text = EXIT_STATUS.sub("has exited with status N", text)
    # Delve prints raw instruction bytes next to each disassembled line. A
    # RIP-relative displacement changes with the module load address, so the
    # bytes differ between live and replay even when the instruction is the
    # same. Keep the mnemonic, mask the encoding column.
    text = re.sub(r"\t[0-9a-fA-F]{4,}\t", "\tBYTES\t", text)
    lines = [line.rstrip() for line in text.splitlines()]
    skip = (
        "Opening trace:",
        "[ttd]",
        "GDB RSP server",
        "GDB client connected",
    )
    return "\n".join(
        line
        for line in lines
        if line.strip()
        and not line.startswith(skip)
        and line.strip() != "GDB session closed"
        and not re.fullmatch(r"[0-9a-fA-F]+:[0-9a-fA-F]+", line.strip())
        and not re.match(r"Breakpoint \d+ cleared at ", line)
    )


def normalize_gdb(text: str) -> str:
    text = normalize_paths(ANSI.sub("", text))
    text = HEX.sub("0xADDR", text)
    text = POS.sub("pos POS", text)
    text = PROCESS.sub("process PID", text)
    text = re.sub(
        r"^(Thread \w+ received signal|Program received signal)",
        "<SIGNAL> received signal",
        text,
        flags=re.MULTILINE,
    )
    text = THREAD.sub("Thread TID", text)
    text = re.sub(r"Thread \d+ hit", "Thread TID hit", text)
    text = re.sub(r"in \S*cxa_throw.*", "in CXA_THROW", text)
    text = re.sub(r" \(UTID [^)]+\)", "", text)
    # Replay resolves a module path for frames; live GDB often omits it.
    text = re.sub(r"(\bin [^\n]*? \(\)) from .*$", r"\1", text, flags=re.MULTILINE)
    # GDB names the script in command errors; the path is stable per case.
    # Trace end is the synthetic rr-style SIGKILL; it is not program behavior
    # and every client sees the same wire stop. Keep the transcript before it.
    synthetic = text.find("<SIGNAL> received signal SIGKILL")
    if synthetic >= 0:
        text = text[:synthetic]
    # TTD reports a breakpoint stop without the live "Thread N hit" prefix;
    # normalize the event itself, keep its location/args.
    text = re.sub(
        r"^(?:Thread \S+ hit )?Breakpoint \d+,",
        "Breakpoint hit,",
        text,
        flags=re.MULTILINE,
    )
    lines = [line.rstrip() for line in text.splitlines()]
    skip = ("[Inferior", "[New Thread", "[Thread", "[Switching", "GDB session closed")
    return "\n".join(
        line
        for line in lines
        if line.strip()
        and not line.startswith(skip)
        and not re.match(r"Breakpoint \d+ cleared at ", line)
    )


def normalize_deterministic(text: str) -> str:
    """Strip machine-specific artifacts from a same-trace transcript.

    A replay of the same trace reproduces the program byte for byte —
    addresses, positions, thread ids and breakpoint numbers included — so no
    program state is masked here: the diff stays sensitive to the smallest
    position or address regression.

    Two things are not program state and are removed anyway: ANSI colouring
    (the client emits it independently of the target) and absolute file paths
    (where the recording machine's toolchain, build directory and sources
    lived). The path basename stays in the comparison, so a frame resolving
    to the wrong module or source file is still a diff.
    """
    text = normalize_paths(ANSI.sub("", text))
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").splitlines()]
    # Server startup narration and the `W`/`X` teardown complaint are harness
    # noise, not program state (see `normalize_delve`/`normalize_gdb` for the
    # same list).
    skip = ("Opening trace:", "[ttd]", "GDB RSP server", "GDB client connected")
    return "\n".join(
        line
        for line in lines
        if line.strip()
        and not line.startswith(skip)
        and not line.startswith("Remote communication error")
    )
