"""Run Auto Paint with its original arguments and capture the wall-clock duration."""

import json
from pathlib import Path
import subprocess
import time


def from_source():
    checkout = Path(__file__).resolve().parents[2]
    tree = subprocess.check_output(
        ["cargo", "tree", "--locked", "-p", "paintrs", "--depth", "0", "--prefix", "none"],
        cwd=checkout, text=True,
    ).strip().split()
    if len(tree) < 2 or tree[0] != "paintrs" or not tree[1].startswith("v"):
        raise ValueError("Cargo did not resolve a Paintrs version")
    build = subprocess.check_output(
        ["cargo", "build", "--locked", "--bin", "auto-paint", "--message-format=json-render-diagnostics"],
        cwd=checkout, text=True,
    )
    for line in build.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            return message["executable"], tree[1][1:]
    raise ValueError("Cargo did not emit an Auto Paint executable")


def record(binary, args, paintrs_version):
    options = {}
    for i, arg in enumerate(args):
        flag, equals, value = arg.partition("=")
        if flag in ("--harness", "--model", "--effort"):
            options[flag[2:]] = value if equals else args[i + 1] if i + 1 < len(args) else ""
        elif flag == "--smoke-test":
            options["smoke_test"] = True

    started = time.monotonic()
    output = None
    with subprocess.Popen([binary, *args], stdout=subprocess.PIPE, text=True,
                          encoding="utf-8", errors="replace") as process:
        for line in process.stdout:
            print(line, end="", flush=True)
            if output is None and line.startswith("Output: "):
                output = Path(line.removeprefix("Output: ").rstrip("\r\n"))
        if process.wait():
            return process.returncode
    if output is None or not (output / "drawing.mp4").is_file():
        if any(arg in ("--help", "-h", "--version", "-V") for arg in args):
            return 0
        raise ValueError("Auto Paint exited without a drawing.mp4 recording")
    metadata = {"elapsed_seconds": round(time.monotonic() - started, 3), "options": options}
    if paintrs_version:
        metadata["paintrs_version"] = paintrs_version
    path = output / "benchmark.json"
    with path.open("x", encoding="utf-8") as handle:
        json.dump(metadata, handle, indent=2)
        handle.write("\n")
    return 0
