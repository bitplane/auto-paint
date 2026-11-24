"""Keep narration at its original pace and retime quiet sections of the picture."""

from dataclasses import asdict
from fractions import Fraction
import json
from pathlib import Path
import re
import subprocess
from tempfile import TemporaryDirectory

from .cards import HEIGHT, WIDTH, install_card, run_card

FPS = 30
CARD_FRAMES = 5 * FPS
SAMPLE_RATE = 48_000
BYTES_PER_FRAME = 2 * SAMPLE_RATE // FPS
SILENCE = re.compile(r"silence_(start|end):\s*(-?\d+(?:\.\d+)?)")
ENCODE = ["-c:v", "libx264", "-preset", "superfast", "-crf", 17, "-pix_fmt", "yuv420p",
          "-g", 60, "-threads", 2, "-video_track_timescale", 30000]


def ffmpeg(*args, capture=False):
    return subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "info" if capture else "error",
         "-nostdin", "-y", "-filter_threads", "1", *map(str, args)],
        check=True, capture_output=capture, text=capture,
    )


def probe(path):
    return json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_streams", "-show_format", "-show_chapters",
        "-of", "json", str(path),
    ]))


def timeline(source, speed):
    info = probe(source)
    picture = next((stream for stream in info["streams"] if stream["codec_type"] == "video"), None)
    if picture is None:
        raise ValueError(f"No video stream in {source}")
    audio = next((stream for stream in info["streams"] if stream["codec_type"] == "audio"), None)
    duration = Fraction(picture.get("duration", info["format"].get("duration", "0")))
    if duration <= 0:
        raise ValueError(f"No video duration in {source}")
    if not audio:
        return duration, False, [(Fraction(0), duration, speed > 1)]

    audio_start = Fraction(audio.get("start_time", "0"))
    if audio_start != Fraction(picture.get("start_time", "0")):
        raise ValueError(f"Audio and video have different start times in {source}")
    if speed == 1:
        return duration, True, [(Fraction(0), duration, False)]
    audio_end = min(duration, Fraction(audio.get("duration", str(duration))))
    result = ffmpeg("-nostats", "-xerror", "-i", source, "-map", "0:a:0", "-vn",
                    "-af", "silencedetect=noise=-50dB:duration=0.4", "-f", "null", "-", capture=True)
    silence = []
    start = None
    for boundary, value in SILENCE.findall(result.stderr):
        timestamp = min(audio_end, max(Fraction(0), Fraction(value) - audio_start))
        if boundary == "start":
            start = timestamp
        elif start is not None:
            silence.append((start, timestamp))
            start = None
    if start is not None:
        silence.append((start, audio_end))
    if audio_end < duration:
        if silence and silence[-1][1] == audio_end:
            silence[-1] = (silence[-1][0], duration)
        else:
            silence.append((audio_end, duration))

    padding = Fraction(1, 5)
    segments = []
    cursor = Fraction(0)
    for start, end in silence:
        start = max(cursor, start + (padding if start > 0 else 0))
        end -= padding if end < duration else 0
        if end - start < Fraction(1, 2):
            continue
        if start > cursor:
            segments.append((cursor, start, False))
        segments.append((start, end, True))
        cursor = end
    if cursor < duration:
        segments.append((cursor, duration, False))
    return duration, True, segments


def edited_duration(segments, speed):
    return sum((end - start) / (speed if fast else 1) for start, end, fast in segments)


def write_samples(dest, count, source=None):
    while count > 0:
        size = min(count, 1 << 20)
        data = source.read(size) if source is not None else b""
        dest.write(data.ljust(size, b"\0"))
        count -= size


def write_soundtrack(dest, video, raw, has_audio, segments, speed, frames):
    backstop = frames * BYTES_PER_FRAME
    if not has_audio:
        write_samples(dest, backstop)
        return
    ffmpeg("-xerror", "-i", video, "-map", "0:a:0", "-vn",
           "-ac", 1, "-ar", SAMPLE_RATE, "-f", "s16le", raw)
    cursor = Fraction(0)
    written = 0
    with raw.open("rb") as source:
        for start, end, fast in segments:
            cursor += (end - start) / (speed if fast else 1)
            next_pos = min(backstop, 2 * round(cursor * SAMPLE_RATE))
            size = next_pos - written
            if fast:
                write_samples(dest, size)
            else:
                source.seek(2 * round(start * SAMPLE_RATE))
                write_samples(dest, size, source)
            written = next_pos
    raw.unlink()
    write_samples(dest, backstop - written)


def encode_card(image, stage, name):
    png = stage / f"{name}.png"
    image.save(png)
    clip = stage / f"{name}.mp4"
    ffmpeg("-loop", 1, "-framerate", FPS, "-i", png, "-frames:v", CARD_FRAMES, "-an", *ENCODE, clip)
    return clip


def encode_drawing(source, output, segments, speed, frames):
    skipped = "+".join(
        f"clip(T-{float(start):.9f},0,{float(end-start):.9f})"
        for start, end, fast in segments if fast
    ) or "0"
    filters = (
        f"settb=AVTB,setpts=PTS-STARTPTS,setpts='(T-{(speed-1)/speed}*({skipped}))/TB',"
        f"fps={FPS}:round=near,scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease:"
        f"force_divisible_by=2,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2,setsar=1,"
        f"tpad=stop_mode=clone:stop_duration=1,trim=end_frame={frames}"
    )
    ffmpeg("-threads", 2, "-xerror", "-i", source, "-map", "0:v:0", "-an", "-vf", filters, *ENCODE, output)


def add_clip(playlist, path, frames):
    playlist.write(f"file '{path.name}'\nduration {frames/FPS:.9f}\n")


def chapter_file(file, chapters, end):
    def escape(text):
        return re.sub(r"([\\=;#\n])", r"\\\1", text)

    with file.open("w", encoding="utf-8", newline="\n") as meta:
        meta.write(";FFMETADATA1\n")
        ends = [start for _, start in chapters[1:]] + [end]
        for (title, start), stop in zip(chapters, ends, strict=True):
            meta.write(f"\n[CHAPTER]\nTIMEBASE=1/{FPS}\nSTART={start}\nEND={stop}\ntitle={escape(title)}\n")


def verify(output, frames, chapters):
    result = probe(output)
    streams = {stream["codec_type"]: stream for stream in result["streams"]}
    picture, audio = streams["video"], streams["audio"]
    if int(picture["nb_frames"]) != frames:
        raise ValueError(f"Expected {frames} video frames, got {picture['nb_frames']}")
    for stream in (picture, audio):
        if abs(float(stream["duration"]) - frames / FPS) > 0.005 or float(stream["start_time"]):
            raise ValueError(f"Unexpected {stream['codec_type']} timing in {output}")
    actual = result.get("chapters", [])
    if len(actual) != len(chapters) or any(
        tag["tags"]["title"] != title or abs(float(tag["start_time"]) - start / FPS) > 0.005
        for tag, (title, start) in zip(actual, chapters)
    ):
        raise ValueError(f"Unexpected chapter timing in {output}")


def build(runs, settings, output):
    speed = settings.get("speed", 8)
    if type(speed) is not int or speed < 1:
        raise ValueError("speed must be a positive integer")
    output = output.resolve()
    if output.suffix.lower() != ".mp4":
        raise ValueError("Output must have an .mp4 extension")
    timings = output.with_suffix(".timing.json")
    if output.exists() or timings.exists():
        raise ValueError(f"Choose an unused output filename ({output})")
    output.parent.mkdir(parents=True, exist_ok=True)

    title = settings.get("title", "Auto Paint demo")
    date = settings.get("date", "")
    with TemporaryDirectory(prefix="auto-paint-demo-", dir=output.parent) as scratch:
        stage = Path(scratch)
        playlist = stage / "playlist.txt"
        soundtrack = stage / "soundtrack.s16le"
        chapters = [("Install", 0)]
        total_frames = CARD_FRAMES
        report = []
        with playlist.open("w", encoding="utf-8", newline="\n") as clips, soundtrack.open("wb") as voice:
            clips.write("ffconcat version 1.0\n")
            add_clip(clips, encode_card(install_card(title, date), stage, "install"), CARD_FRAMES)
            write_samples(voice, CARD_FRAMES * BYTES_PER_FRAME)
            for index, run in enumerate(runs, 1):
                duration, has_audio, segments = timeline(run.video, speed)
                frames = max(1, round(edited_duration(segments, speed) * FPS))
                if chapters[-1][0] != run.model:
                    chapters.append((run.model, total_frames))
                add_clip(clips, encode_card(run_card(run), stage, f"{index:04d}-card"), CARD_FRAMES)
                write_samples(voice, CARD_FRAMES * BYTES_PER_FRAME)
                drawing = stage / f"{index:04d}-drawing.mp4"
                encode_drawing(run.video, drawing, segments, speed, frames)
                add_clip(clips, drawing, frames)
                write_soundtrack(voice, run.video, stage / "source.s16le", has_audio, segments, speed, frames)
                report.append(asdict(run) | {
                    "video": str(run.video),
                    "source_seconds": float(duration),
                    "drawing_start_seconds": (total_frames + CARD_FRAMES) / FPS,
                    "drawing_seconds": frames / FPS,
                    "edit_segments": [
                        {"start": float(start), "end": float(end), "speed_up": fast}
                        for start, end, fast in segments
                    ],
                })
                total_frames += CARD_FRAMES + frames
                print(f"{run.model}: {float(duration):.1f}s → {frames/FPS:.1f}s", flush=True)

        chapter_data = stage / "chapters.ffmeta"
        chapter_file(chapter_data, chapters, total_frames)
        staged_output = stage / "demo.mp4"
        ffmpeg("-f", "concat", "-safe", 0, "-i", playlist,
               "-f", "s16le", "-ar", SAMPLE_RATE, "-ac", 1, "-i", soundtrack,
               "-f", "ffmetadata", "-i", chapter_data,
               "-map", "0:v:0", "-map", "1:a:0", "-map_metadata", 2, "-map_chapters", 2,
               "-c:v", "copy", "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart", staged_output)
        verify(staged_output, total_frames, chapters)
        staged_timings = stage / "timing.json"
        staged_timings.write_text(json.dumps({
            "speed": speed, "card_seconds": CARD_FRAMES / FPS, "duration_seconds": total_frames / FPS,
            "runs": report,
        }, indent=2) + "\n", encoding="utf-8")
        staged_output.replace(output)
        staged_timings.replace(timings)
    print(output)
