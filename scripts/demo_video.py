#!/usr/bin/env python3
"""Record benchmark metadata or render an Auto Paint demo from a TOML manifest."""

import argparse
from pathlib import Path
import subprocess
import sys

from demo.runs import load_manifest
from demo.record import from_source, record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    capture = commands.add_parser("record", help="run Auto Paint and save wall-clock time")
    executable = capture.add_mutually_exclusive_group()
    executable.add_argument("--from-source", action="store_true", help="build this checkout and record its resolved Paintrs version")
    executable.add_argument("--binary", default="auto-paint", help="path to the Auto Paint executable")
    capture.add_argument("--paintrs-version", help="record the version linked into this Auto Paint binary")
    capture.add_argument("args", nargs=argparse.REMAINDER, help="Auto Paint arguments after --")
    for name, help_text in (("build", "render the MP4 and timing JSON"), ("cards", "render only the PNG cards")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("manifest", type=Path)
        command.add_argument("output", type=Path)

    options = parser.parse_args()
    try:
        if options.command == "record":
            args = options.args[1:] if options.args[:1] == ["--"] else options.args
            binary, version = options.binary, options.paintrs_version
            if options.from_source:
                binary, resolved = from_source()
                if version and version != resolved:
                    raise ValueError(f"Cargo resolved Paintrs {resolved}, which differs from --paintrs-version {version}")
                version = resolved
            return record(binary, args, version)

        from demo.cards import install_card, run_card

        settings, runs = load_manifest(options.manifest)
        if options.command == "cards":
            options.output.mkdir(parents=True)
            install_card(settings.get("title", "Auto Paint demo"), settings.get("date", "")).save(
                options.output / "0000-install.png")
            for index, run in enumerate(runs, 1):
                run_card(run).save(options.output / f"{index:04d}-run.png")
        else:
            from demo.video import build

            build(runs, settings, options.output)
    except (ImportError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"demo_video: {error}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
