"""Draw the release video cards at the same size as the Paintrs recordings."""

from functools import lru_cache
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

WIDTH, HEIGHT = 1000, 700
HERE = Path(__file__).resolve().parent
FONTS = {
    "regular": HERE.parents[1] / "assets/LiberationSans-Regular.ttf",
    "bold": HERE / "fonts/LiberationSans-Bold.ttf",
    "mono": HERE / "fonts/LiberationMono-Regular.ttf",
}
INK = "#181d2d"
SECONDARY = "#4b5670"
RULE = "#c0c5d0"
LINK = "#0000ff"


@lru_cache
def font(face, size):
    return ImageFont.truetype(str(FONTS[face]), size)


def text(draw, x, y, value, size, *, face="regular", color=INK, right=864):
    fitted = font(face, size)
    while fitted.getlength(value) > right - x and size > 8:
        size -= 1
        fitted = font(face, size)
    draw.text((x, y), value, font=fitted, fill=color, anchor="lt")
    return fitted.getlength(value)


def window():
    image = Image.new("RGB", (WIDTH, HEIGHT), "#087b7f")
    draw = ImageDraw.Draw(image)
    draw.rectangle((84, 73, 932, 635), fill="#065a60")
    draw.rectangle((67, 56, 916, 620), fill="#d3d3d7")
    draw.line([(68, 618), (68, 57), (915, 57)], fill="white", width=2)
    draw.line([(915, 57), (915, 619), (68, 619)], fill="#373a48", width=2)
    start, stop = (31, 39, 98), (79, 98, 154)
    for x in range(73, 911):
        progress = (x - 73) / (910 - 73)
        color = tuple(round(a + (b - a) * progress) for a, b in zip(start, stop))
        draw.line((x, 62, x, 100), fill=color)
    draw.rectangle((86, 115, 896, 599), fill="#f8f8fb", outline="#82848e")
    text(draw, 88, 70, "Auto Paint", 24, face="bold", color="white")
    return image, draw


def install_card(title, date):
    image, draw = window()
    title_right = 864
    if date:
        width = font("regular", 24).getlength(date)
        date_x = max(595, round(864 - width))
        text(draw, date_x, 161, date, 24, color=SECONDARY)
        title_right = date_x - 18
    text(draw, 120, 147, title, 48, face="bold", right=title_right)
    draw.line((120, 213, 863, 213), fill=RULE, width=2)
    text(draw, 122, 250, "cargo install auto-paint", 32, face="mono")
    link(draw, 309, "https://github.com/bitplane/auto-paint")
    draw.line((120, 375, 863, 375), fill=RULE, width=2)
    text(draw, 122, 410, "For the paint program alone", 28, face="bold")
    text(draw, 122, 466, "cargo install paintrs", 32, face="mono")
    link(draw, 525, "https://github.com/bitplane/paintrs")
    return image


def link(draw, y, address):
    width = text(draw, 122, y, address, 26, color=LINK)
    draw.line((122, y + 27, 122 + round(width), y + 27), fill=LINK, width=2)


def run_card(run):
    image, draw = window()
    text(draw, 120, 144, run.model, 58, face="bold")
    if run.effort:
        effort = "XHigh" if run.effort == "xhigh" else run.effort.capitalize()
        text(draw, 124, 213, f"{effort} effort", 28, color=SECONDARY, right=494)
    text(draw, 508, 218, f"Paintrs {run.paintrs_version}", 23, color=SECONDARY)
    for y in (267, 534):
        draw.line((120, y, 863, y), fill=RULE, width=2)

    if run.elapsed is None:
        duration = "—"
    else:
        minutes, seconds = divmod(int(run.elapsed + 0.5), 60)
        duration = f"{minutes}m {seconds:02d}s"
    cost = "—" if run.cost is None else f"${run.cost:.3f}"
    def number(key):
        return "—" if key not in run.tokens else f"{run.tokens[key]:,}"

    for x, y, label, value, size, right in (
        (122, 290, "Wall-clock time", duration, 44, 493),
        (508, 290, run.cost_label, cost, 44, 864),
        (122, 407, "Input tokens", number("input"), 40, 493),
        (508, 407, "Output tokens", number("output"), 40, 864),
    ):
        text(draw, x, y, label, 22, color=SECONDARY, right=right)
        text(draw, x, y + (35 if y < 400 else 32), value, size, face="bold", right=right)
    notes = [f"{number(key)} {label}" for key, label in (("cached", "cached"), ("written", "written"))
             if run.tokens.get(key) is not None and (key != "written" or run.tokens[key])]
    if notes:
        text(draw, 122, 491, " · ".join(notes), 22, color=SECONDARY, right=503)
    if run.tokens.get("reasoning") is not None:
        text(draw, 508, 491, f'{number("reasoning")} reasoning', 22, color=SECONDARY)
    text(draw, 122, 552, run.command, 22, face="mono")
    return image
