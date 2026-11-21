"""Exercise the real drawing UI and check the video and PNG."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--watch", action="store_true")
    parser.add_argument("--narrate", action="store_true")
    parser.add_argument("--ffprobe", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="auto-paint-smoke-") as folder:
        output = Path(folder) / "take"
        command = [str(args.binary.resolve()), "--smoke-test", "--output", str(output)]
        if args.narrate:
            command.append("--narrate")
        if not args.watch:
            command.append("--no-watch")
        subprocess.run(command, check=True, timeout=90)
        png = output / "drawing.png"
        with png.open("rb") as drawing:
            assert drawing.read(8) == b"\x89PNG\r\n\x1a\n", "Invalid drawing PNG"
        video = output / "drawing.mp4"
        with video.open("rb") as recording:
            assert recording.read(8)[4:] == b"ftyp", "Invalid recording MP4"
        if args.ffprobe:
            streams = json.loads(subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_streams", "-of", "json", video],
                text=True,
            ))["streams"]
            expected = {"h264", "aac"} if args.narrate else {"h264"}
            assert {stream["codec_name"] for stream in streams} == expected
            subprocess.run(
                ["ffmpeg", "-v", "error", "-xerror", "-i", video, "-f", "null", "-"],
                check=True,
                timeout=30,
            )


if __name__ == "__main__":
    main()
