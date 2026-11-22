"""Exercise the real drawing UI and check the video and PNG."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def subtitle_region(video, seek):
    return subprocess.check_output(
        ["ffmpeg", "-v", "error", *seek, "-i", video, "-vf", "crop=480:96:260:586",
         "-frames:v", "1", "-pix_fmt", "gray", "-f", "rawvideo", "-"],
        timeout=30,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--watch", action="store_true")
    parser.add_argument("--no-narrate", action="store_true")
    parser.add_argument("--ffprobe", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="auto-paint-smoke-") as folder:
        output = Path(folder) / "take"
        command = [str(args.binary.resolve()), "--smoke-test", "--output", str(output)]
        if args.no_narrate:
            command.append("--no-narrate")
        if not args.watch:
            command.append("--no-watch")
        env = os.environ.copy()
        if args.watch and sys.platform.startswith("linux"):
            env.pop("WAYLAND_DISPLAY", None)
            env.pop("WAYLAND_SOCKET", None)
            command = ["xvfb-run", "-a", *command]
        subprocess.run(command, env=env, check=True, timeout=90)
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
            expected = {"h264", "aac"} if not args.no_narrate else {"h264"}
            assert {stream["codec_name"] for stream in streams} == expected
            picture = next(stream for stream in streams if stream["codec_name"] == "h264")
            assert (picture["width"], picture["height"]) == (1000, 700)
            if not args.no_narrate:
                before = subtitle_region(video, ["-ss", "0"])
                speaking = subtitle_region(video, ["-ss", "1"])
                finished = subtitle_region(video, ["-sseof", "-0.25"])
                assert len(before) == len(speaking) == len(finished) == 480 * 96
                white = sum(now > 220 and now - old > 20 for old, now in zip(before, speaking))
                outline = sum(now < 50 and old > 80 for old, now in zip(before, speaking))
                assert white > 50, "White subtitle text missing during speech"
                assert outline > 50, "Dark subtitle outline missing during speech"
                changed = sum(abs(old - new) > 30 for old, new in zip(before, finished))
                assert changed < len(before) // 100, "Subtitle remained after speech"
            subprocess.run(
                ["ffmpeg", "-v", "error", "-xerror", "-i", video, "-f", "null", "-"],
                check=True,
                timeout=30,
            )


if __name__ == "__main__":
    main()
