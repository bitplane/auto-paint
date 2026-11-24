# Benchmark videos

Put your chosen harness on PATH. The editor records each run to `drawing.mp4`. To assemble the videos, use Python 3.11 or newer, Pillow, and FFmpeg with `libx264`, AAC, and `ffprobe` on PATH.

From the Auto Paint checkout, try the complete pipeline without a model.

```sh
python -m pip install Pillow
mkdir -p target/demo
cp scripts/demo/example.toml target/demo/demo.toml
python scripts/demo_video.py record --from-source -- --smoke-test --no-watch --output target/demo/runs/smoke
python scripts/demo_video.py build target/demo/demo.toml target/demo/demo.mp4
```

On Windows PowerShell, use `New-Item -ItemType Directory -Force target/demo` and `Copy-Item scripts/demo/example.toml target/demo/demo.toml` for the setup.

## Make a comparison

Everything after `--` goes straight to Auto Paint. Leaving out `--output`, `--prompt`, and `--reference` preserves Auto Paint's random subject choice. Use `--prompt` for a repeatable brief or `--reference` for a shared image. When recording from a scratch directory, invoke the script by its absolute path.

```sh
python scripts/demo_video.py record --from-source -- --harness opencode --model provider/model-id --effort high --no-watch
```

Use the model selector your harness supports. For local or Chinese models via OpenCode, configure the provider in OpenCode first and use its model ID. Other supported Auto Paint harnesses work the same way. The wrapper writes `benchmark.json` with the elapsed wall time and the public command options. A run is portable as a folder containing `drawing.mp4`, `benchmark.json`, and `usage.json` when available.

Copy the example manifest and list the run folders in playback order. Paths are relative to the manifest. Set `title`, `date`, and any per-run display labels. For older recordings, supply `harness`, `model_arg`, `effort`, and `elapsed_seconds` as needed. Existing `usage.json` files supply token counts. A manifest can supply its own `[runs.tokens]` for an external recorder. Set `cost_usd` for a known estimate or `[runs.pricing]` for flat USD rates per million tokens. Use the rates that applied to your runs, and enter the final estimate directly for tiered pricing. Missing measurements appear as dashes.

Every run card shows the version of Paintrs linked into Auto Paint. `record --from-source` builds the checkout with Cargo's lockfile and saves the resolved version in each run. To use an official release instead, pass `--binary path/to/auto-paint --paintrs-version VERSION` before `--`. Find the version in that release tag's `Cargo.lock`. For archived recordings, set `paintrs_version` in a specific `[[runs]]` entry or at the top of the manifest as a default. Per-run manifest values take precedence.

```sh
python scripts/demo_video.py cards demo.toml preview-cards
python scripts/demo_video.py build demo.toml demo.mp4
```

The install card and run cards last five seconds each. The script draws the window frame, typography, and blue underlined links with Pillow and the bundled Liberation fonts. Narration and its subtitles play at original speed. FFmpeg detects silence with a −50 dB threshold, keeps 200 ms around speech, and speeds up quiet stretches at least 500 ms long. Use `speed = 1` for an unsped cut. The build writes model chapters into the MP4 and saves the source timestamps and edit segments alongside it as `demo.timing.json`. Temporary render files use the output directory's filesystem.
