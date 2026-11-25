"""Helpers for Edict plugins (standard library only).

Edict runs a plugin as ``python plugin.py run <request.json>`` in a
terminal, with its working directory set to a scratch folder for the run. What
the plugin prints is shown to the user as is (colours and progress bars work).
Messages for the app itself go to a separate file, ``$EDICT_EVENTS``, one JSON
object per line; the functions here write them.

    import edict

    req = edict.request()          # {"action", "settings", "input", ...}
    edict.progress("Folding")
    ...
    edict.result(kind="structure", file="pred.cif", format="mmcif", chains=["A"])

A run succeeds when the plugin exits 0 after sending a result. Exiting with
``edict.MEMORY_GUARD_EXIT`` tells the app a memory guard stopped it.

The app puts this file on PYTHONPATH; it is written fresh for every run, so
plugins always get the copy matching the app's protocol.
"""

from __future__ import annotations

import json
import os
import sys
import threading
import time

API = 1
#: Exit status meaning "a memory guard stopped this run on purpose".
MEMORY_GUARD_EXIT = 75
GIB = 1 << 30

_EVENTS = os.environ.get("EDICT_EVENTS")


def _emit(obj: dict) -> None:
    if not _EVENTS:
        return
    line = json.dumps(obj, separators=(",", ":")) + "\n"
    # One write per event, appended, so the app never reads half a line twice.
    with open(_EVENTS, "a", encoding="utf-8") as fh:
        fh.write(line)
        fh.flush()


def request() -> dict:
    """The run's request: action, settings (typed), input, workdir."""
    path = os.environ.get("EDICT_REQUEST") or (sys.argv[2] if len(sys.argv) > 2 else None)
    if not path:
        raise SystemExit("no request: run me from Edict, or pass a request file")
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def progress(text: str, fraction: float | None = None) -> None:
    """The one-line status shown above the terminal and in the task list."""
    _emit({"event": "progress", "text": text, "fraction": fraction})


def warn(text: str) -> None:
    """Something the user should notice; also kept as the reason if the run fails."""
    print(f"warning: {text}", file=sys.stderr, flush=True)
    _emit({"event": "warn", "text": text})


def fail(message: str, code: int = 1) -> "NoReturn":  # noqa: F821
    """Stop with an error the app shows as the reason."""
    print(f"error: {message}", file=sys.stderr, flush=True)
    _emit({"event": "error", "text": message})
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(code)


def result(kind: str, **fields) -> None:
    """The run's result. kinds: "structure" (file, format, chains, ...) or "features"
    (features=[{type, label, strand, segments, qualifiers, detail, selected}, ...])."""
    _emit({"event": "result", "kind": kind, **fields})


def in_app() -> bool:
    return bool(_EVENTS)


# ----- memory --------------------------------------------------------------------


def mem_available() -> int | None:
    """Bytes the kernel thinks can be allocated without swapping (Linux), else None."""
    try:
        with open("/proc/meminfo", encoding="ascii") as fh:
            for line in fh:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    return None


def rss() -> int | None:
    """This process's resident memory in bytes (Linux), else None."""
    try:
        with open("/proc/self/statm", encoding="ascii") as fh:
            return int(fh.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
    except (OSError, ValueError):
        return None


def first_to_be_killed() -> None:
    """Ask the kernel to pick this process first if memory runs out, rather than the
    app or the desktop (Linux; raising one's own score needs no privileges)."""
    try:
        with open("/proc/self/oom_score_adj", "w", encoding="ascii") as fh:
            fh.write("1000")
    except OSError:
        pass


class RamGuard:
    """Watch free host memory from a thread; if it drops below ``floor`` bytes, stop
    the whole process at once with MEMORY_GUARD_EXIT, before the kernel's OOM killer
    starts choosing victims (which on a desktop can be the compositor).

    Use as a context manager around the memory-hungry part, or call start()/stop().
    Does nothing where free memory can't be read.
    """

    def __init__(self, floor: int, interval: float = 0.25, what: str = "this run"):
        self.floor = floor
        self.interval = interval
        self.what = what
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.lowest: int | None = None

    def start(self) -> "RamGuard":
        if mem_available() is None or self._thread:
            return self
        self._thread = threading.Thread(target=self._watch, name="ram-guard", daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2)
            self._thread = None

    def __enter__(self) -> "RamGuard":
        return self.start()

    def __exit__(self, *exc) -> None:
        self.stop()

    def _watch(self) -> None:
        while not self._stop.wait(self.interval):
            free = mem_available()
            if free is None:
                continue
            self.lowest = free if self.lowest is None else min(self.lowest, free)
            if free < self.floor:
                mine = rss()
                fail(
                    f"memory guard: only {free / GIB:.1f} GiB of RAM left (floor {self.floor / GIB:.1f} GiB)"
                    + (f", {self.what} was using {mine / GIB:.1f} GiB" if mine else "")
                    + ". Stopped before the system ran out. Close other programs, or lower the floor "
                    "in the plugin's settings if you have swap to spare.",
                    MEMORY_GUARD_EXIT,
                )


class Timer:
    """``with Timer() as t: ...; t.seconds``"""

    def __enter__(self) -> "Timer":
        self.start = time.time()
        self.seconds = 0.0
        return self

    def __exit__(self, *exc) -> None:
        self.seconds = time.time() - self.start
