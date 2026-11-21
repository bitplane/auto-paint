# Auto Paint

Install a [release binary](https://github.com/bitplane/auto-paint/releases), put [Codex](https://developers.openai.com/codex/cli/) on your PATH, and run `codex login`.
Run `auto-paint --prompt "A sailboat at sunset"` to watch Codex draw and save a PNG and MP4.
Use `--reference photo.png` for a reference, `--narrate` for voiceover, or `--no-watch` to run headless.
Use `--output my-take` to name the output directory. Run `auto-paint --help` for all options.

H.264 recording uses OpenH264 (BSD-2-Clause). Cisco’s AVC patent coverage applies only to the codec binary distributed by Cisco.
