"""Render documentation from bundled fictional data using the real UI and renderer."""
import argparse
import asyncio
import re
import os
from pathlib import Path
import subprocess

from rich.console import Console
from rich.text import Text
from rich.terminal_theme import TerminalTheme

from pr_map import prmap
from pr_map.app import PRMapApp

ROOT = Path(__file__).resolve().parents[1]
THEME = TerminalTheme((26, 27, 38), (192, 202, 245),
    [(21, 22, 30), (247, 118, 142), (158, 206, 106), (224, 175, 104),
     (122, 162, 247), (187, 154, 247), (125, 207, 255), (192, 202, 245)])

async def main():
    os.environ.pop("NO_COLOR", None)
    prmap.DEMO = True
    prmap.BIN = os.environ["PRMAP_BIN"]
    for key in list(os.environ):
        if key.startswith("HERDR_"):
            del os.environ[key]
    app = PRMapApp()
    async with app.run_test(size=(140, 35)) as pilot:
        for _ in range(30):
            await pilot.pause(0.1)
            if app.model:
                break
        assert app.model and app.model["repo"] == "example/harbor"
        await pilot.press("a")
        await pilot.pause()
        svg = app.export_screenshot(title="PR Map · offline example")
        (ROOT / "docs/images/full-map.svg").write_text(svg)
    env = dict(os.environ, COLUMNS="36", LINES="16", HERDR_ACTIVE_PANE_ID="w1:p1", HERDR_PANEL_ZOOM="2")
    panel = subprocess.check_output([prmap.BIN, "--demo", "minimap"], env=env, text=True)
    console = Console(record=True, width=36, color_system="truecolor")
    console.print(Text.from_ansi(panel), end="")
    console.save_svg(str(ROOT / "docs/images/minimap.svg"), title="PR Map · offline example", theme=THEME)
    for name in ["full-map", "minimap"]:
        path = ROOT / "docs/images" / f"{name}.svg"
        svg = re.sub(r"    @font-face \{.*?\n    \}", "", path.read_text(), flags=re.S)
        svg = svg.replace("font-family: Fira Code, monospace;", 'font-family: "DejaVu Sans Mono", monospace;')
        svg = "\n".join(line.rstrip() for line in svg.splitlines()).rstrip() + "\n"
        path.write_text(svg)
        if ARGS.png:
            import cairosvg
            cairosvg.svg2png(bytestring=svg.encode(), write_to=str(path.with_suffix(".png")))

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--png", action="store_true", help="also render PNGs; requires cairosvg")
    ARGS = parser.parse_args()
    asyncio.run(main())
