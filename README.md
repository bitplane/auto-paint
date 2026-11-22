# Auto Paint

Install a [release binary](https://github.com/bitplane/auto-paint/releases), put [Codex](https://developers.openai.com/codex/cli/) on your PATH, and run `codex login`.
Run `auto-paint --prompt "A sailboat at sunset"` to watch Codex draw and save a PNG and MP4.
Use `--harness claude` after `claude auth login` for Haiku, or `--harness agy` after signing in with `agy` for Gemini 3.8 Flash. Use `--model` to select a model or family.
Run `auto-paint` for a random subject. The category and idea picks are saved in `selection.json`.
Voiceover and subtitles are on by default in the live window and MP4. Use `--no-narrate` for silence, `--reference photo.png` for a reference, or `--no-watch` to run headless.
Use `--output my-take` to name the output directory and `--effort low` to set reasoning effort. Run `auto-paint --help` for all options.

H.264 recording uses OpenH264 (BSD-2-Clause). Cisco’s AVC patent coverage applies only to the codec binary distributed by Cisco.
