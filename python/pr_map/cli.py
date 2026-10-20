"""The installed pr-map command: Rust panel/report commands and the Textual map."""
import argparse
import os

from . import prmap


def main():
    parser = argparse.ArgumentParser(description="A PR stack map with live coding-agent positions")
    parser.add_argument("--demo", action="store_true", help="use bundled fictional data, fully offline")
    parser.add_argument("--config", help="configuration TOML; otherwise PRMAP_CONFIG or XDG config")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--minimap", action="store_true", help="print the Herdr sidebar panel")
    mode.add_argument("--report", action="store_true", help="report agent PR metadata to Herdr")
    mode.add_argument("--json", action="store_true", help="print the model JSON")
    parser.add_argument("--popup", action="store_true", help="close after jumping to a Herdr agent")
    args = parser.parse_args()
    if args.config:
        os.environ["PRMAP_CONFIG"] = os.path.abspath(args.config)
    prmap.DEMO = args.demo
    if args.minimap or args.report or args.json:
        command = [prmap.BIN, *(["--demo"] if args.demo else []), "minimap" if args.minimap else "report" if args.report else "model"]
        os.execvpe(command[0], command, os.environ)
    target = prmap.clicked() if args.popup else None
    # A click on a session's name goes straight to its pane, before the map would start.
    if target and target.startswith(prmap.PANE) and prmap.focus_pane(target[len(prmap.PANE):]):
        return
    # A click on another project's row opens that project's map.
    if target and target.startswith(prmap.PROJECT):
        prmap.REPO, target = target[len(prmap.PROJECT):], None
    from .app import PRMapApp
    PRMapApp(popup=args.popup, clicked=target).run()


if __name__ == "__main__":
    main()
