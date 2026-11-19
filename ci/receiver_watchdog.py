#!/usr/bin/env python3
"""Bound a native receiver test process and report its stacks if it stalls."""
import ctypes
import os
import platform
import re
import selectors
import signal
import subprocess
import sys
import time

BEGIN = b"receiver-watchdog: reconnect case started"
END = b"receiver-watchdog: reconnect case finished"


def allow_debugger(pid):
    # Linux Yama allows the nominated watchdog and its debugger child to attach.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(0x59616D61, ctypes.c_ulong(pid), 0, 0, 0) != 0:  # PR_SET_PTRACER
        os.write(2, b"receiver-watchdog: could not opt the test into debugger attachment\n")


def descendants(pid):
    rows = subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True, timeout=3)
    parents = {}
    for child, parent in re.findall(r"^\s*(\d+)\s+(\d+)\s*$", rows, re.M):
        parents.setdefault(int(parent), []).append(int(child))
    seen = [pid]
    for parent in seen:
        seen.extend(parents.get(parent, []))
    return seen


def dump_threads(pid, system):
    try:
        processes = descendants(pid)
    except (OSError, subprocess.SubprocessError):
        processes = [pid]
    captured = False
    for child in processes[:4]:
        print(f"receiver-watchdog: thread stacks for process {child}", flush=True)
        if system == "Darwin":
            command = ["/usr/bin/sample", str(child), "1", "10", "-file", "/dev/stdout"]
        else:
            command = ["gdb", "-nx", "--batch", "-ex", "set debuginfod enabled off",
                       "-ex", "set print frame-arguments none", "-p", str(child),
                       "-ex", "thread apply all bt 20"]
        try:
            subprocess.run(command, stdin=subprocess.DEVNULL, timeout=6, check=True)
            captured = True
        except (OSError, subprocess.SubprocessError) as error:
            print(f"receiver-watchdog: debugger unavailable or incomplete: {error}", flush=True)
    return captured


def stop_group(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def supervise(command, *, system, suite_timeout=None, reconnect_timeout=45, require_stacks=False):
    parent = os.getpid()
    setup = (lambda: allow_debugger(parent)) if system == "Linux" else None
    environment = os.environ.copy()
    if system == "Linux" and suite_timeout is not None:
        environment["GRAFTON_NDI_WATCHDOG_SCRIPT"] = os.path.abspath(__file__)
        environment["GRAFTON_NDI_WATCHDOG_PID"] = str(parent)
    with subprocess.Popen(command, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          start_new_session=True, preexec_fn=setup) as child:
        started = time.monotonic()
        suite_end = started + suite_timeout if suite_timeout is not None else float("inf")
        reconnect_end = float("inf")
        tail = b""
        try:
            with selectors.DefaultSelector() as output:
                output.register(child.stdout, selectors.EVENT_READ)
                while output.get_map() or child.poll() is None:
                    now = time.monotonic()
                    deadline = min(suite_end, reconnect_end)
                    if child.poll() is None and now >= deadline:
                        phase = "receiver overlap suite" if suite_end <= reconnect_end else "reconnect case"
                        print(f"receiver-watchdog: {phase} exceeded its deadline", flush=True)
                        captured = dump_threads(child.pid, system)
                        return 125 if require_stacks and not captured else 124
                    if not output.get_map():
                        time.sleep(0.05)
                        continue
                    ready = output.select(0.1)
                    if not ready:
                        if child.poll() is not None:
                            break
                        continue
                    data = os.read(child.stdout.fileno(), 65536)
                    if not data:
                        output.unregister(child.stdout)
                        continue
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                    lines = (tail + data).split(b"\n")
                    for line in lines[:-1]:
                        if BEGIN in line:
                            reconnect_end = time.monotonic() + reconnect_timeout
                        if END in line:
                            reconnect_end = float("inf")
                    tail = lines[-1][-1024:]
            status = child.wait()
            return status if status >= 0 else 128 - status
        finally:
            if child.poll() is None:
                stop_group(child)


def main(args):
    if args == ["--check-debugger"]:
        focus = [sys.executable, "-c", "import time; time.sleep(30)"]
        result = supervise(focus, system=platform.system(), suite_timeout=1, require_stacks=True)
        return 0 if result == 124 else 1
    if args[0] == "--exec-child":
        allow_debugger(int(os.environ["GRAFTON_NDI_WATCHDOG_PID"]))
        os.execvp(args[1], args[1:])
    executable, *options = args
    stem = os.path.basename(executable)
    overlap = stem.startswith("receiver_connect_overlap_sdk-")
    unit = stem.startswith("grafton_ndi-")
    if not (overlap or unit) or any(flag in options for flag in ("--list", "--help", "-h")):
        os.execvp(executable, args)
    if "--nocapture" not in options and "--no-capture" not in options:
        options.append("--nocapture")
    timeout = None
    if overlap:
        # Custom stress workloads can run each case for minutes. Retain their
        # requested run time while still bounding the whole executable.
        duration = int(os.environ.get("GRAFTON_NDI_OVERLAP_SECONDS", "2"))
        timeout = max(90, 60 + 12 * duration)
    return supervise([executable, *options], system=platform.system(), suite_timeout=timeout)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
