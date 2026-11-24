"""Read the ordered manifest and normalize Auto Paint's usage reports."""

from dataclasses import dataclass
import json
import math
from pathlib import Path
import shlex
import tomllib


@dataclass
class Run:
    video: Path
    model: str
    paintrs_version: str
    effort: str | None
    command: str
    elapsed: float | None
    tokens: dict[str, int]
    cost: float | None
    cost_label: str


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}


def tokens_for(harness, usage):
    if harness == "muse":
        models = usage.get("models", [])
        return {
            key: sum(model[name] for model in models)
            for key, name in (("input", "input_tokens"), ("cached", "cached_tokens"),
                              ("output", "output_tokens"), ("reasoning", "reasoning_tokens"))
            if models and all(name in model for model in models)
        }
    cached = usage.get("cached_input_tokens", usage.get("cache_read_input_tokens", usage.get("cache_read_tokens")))
    written = usage.get("cache_write_input_tokens", usage.get("cache_creation_input_tokens", usage.get("cache_write_tokens")))
    inputs = usage.get("input_tokens")
    if inputs is not None and harness in ("claude", "grok", "agy"):
        inputs += (cached or 0) + (written or 0)
    reasoning = usage.get("reasoning_tokens", usage.get("reasoning_output_tokens", usage.get("thinking_tokens")))
    if reasoning is None:
        reasoning = usage.get("output_tokens_details", {}).get("thinking_tokens")
    return {key: value for key, value in {
        "input": inputs, "cached": cached, "written": written,
        "output": usage.get("output_tokens"), "reasoning": reasoning,
    }.items() if value is not None}


def cost_for(row, usage, tokens):
    if "cost_usd" in row:
        return row["cost_usd"], row.get("cost_label", "Estimated API cost")
    if "pricing" in row:
        if "input" not in tokens or "output" not in tokens:
            raise ValueError("pricing requires input and output token counts")
        prices = row["pricing"]
        cached, written = tokens.get("cached", 0), tokens.get("written", 0)
        uncached = tokens["input"] - cached - written
        if uncached < 0:
            raise ValueError("cached and written tokens exceed total input")
        regular = prices["input_per_million"]
        return ((
            uncached * regular
            + cached * prices.get("cached_input_per_million", regular)
            + written * prices.get("cache_write_per_million", regular)
            + tokens["output"] * prices["output_per_million"]
        ) / 1_000_000, row.get("cost_label", "Estimated API cost"))
    if "estimated_cost_usd" in usage:
        return usage["estimated_cost_usd"], row.get("cost_label", "Estimated API cost")
    return usage.get("cost_usd"), row.get("cost_label", "Harness-reported cost")


def read_run(row, base, default_paintrs_version):
    directory = (base / row["directory"]).resolve()
    video = directory / "drawing.mp4"
    if not video.is_file():
        raise ValueError(f"Missing recording {video}")
    report = read_json(directory / "usage.json")
    metadata = read_json(directory / "benchmark.json")
    paintrs_version = row.get("paintrs_version") or metadata.get("paintrs_version") or default_paintrs_version
    if not isinstance(paintrs_version, str) or not paintrs_version.strip():
        raise ValueError(f"Set paintrs_version in the manifest for {directory}")
    usage = report.get("usage", {})
    metadata_args = metadata.get("options", {})
    harness = row.get("harness") or report.get("backend") or metadata_args.get("harness")
    resolved_model = report.get("model")
    if harness == "claude" and len(usage.get("model_usage", {})) == 1:
        resolved_model = next(iter(usage["model_usage"]))
    model_arg = row.get("model_arg") or metadata_args.get("model") or report.get("model")
    model = row.get("title") or resolved_model or model_arg or directory.name
    effort = row.get("effort") or metadata_args.get("effort")
    argv = ["auto-paint"]
    for name, value in (("harness", harness), ("model", model_arg), ("effort", effort)):
        if value:
            argv += [f"--{name}", value]
    if metadata_args.get("smoke_test"):
        argv.append("--smoke-test")
    command = row.get("command", shlex.join(argv))
    elapsed = row.get("elapsed_seconds", metadata.get("elapsed_seconds"))

    tokens = tokens_for(harness, usage) | row.get("tokens", {})
    if any(type(count) is not int or count < 0 for count in tokens.values()):
        raise ValueError(f"Token counts must be nonnegative integers in {directory}")
    cost, label = cost_for(row, usage, tokens)
    for name, value in (("elapsed_seconds", elapsed), ("cost_usd", cost)):
        if value is not None and (not math.isfinite(value) or value < 0):
            raise ValueError(f"{name} must be finite and nonnegative in {directory}")
    return Run(video, model, paintrs_version, effort, command, elapsed, tokens, cost, label)


def load_manifest(path):
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    rows = manifest.get("runs", [])
    if not rows:
        raise ValueError("The manifest needs at least one [[runs]] entry")
    return manifest, [read_run(row, path.parent, manifest.get("paintrs_version")) for row in rows]
