"""Client of the Rust backend; immutable frame ids bind popup selection to displayed output."""
import json
import os
import shutil
import subprocess
from datetime import datetime

BIN = os.environ.get("PRMAP_BIN") or shutil.which("prmap") or "prmap"
DEMO = False
# A clicked cell's target names a station, or a session's pane after this prefix.
PANE = ":pane:"


def _backend(*args):
    command = [BIN, *(["--demo"] if DEMO else []), *args]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        if result.returncode:
            return None
        return json.loads(result.stdout)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return None


def load(force=False, frame=False):
    """Load the accepted frame initially, then refresh live data on subsequent calls."""
    frame_id = os.environ.get("HERDR_PANEL_FRAME_ID") if frame else None
    if frame_id:
        return _backend("model", "--frame", frame_id)
    return _backend("model", *(["--force"] if force else []))


def commits(branch):
    return _backend("commits", branch) or []


def clicked():
    """The target of the panel cell a click opened us on: a station, PANE + pane id, or None."""
    frame_id = os.environ.get("HERDR_PANEL_FRAME_ID")
    cell = [os.environ.get(f"HERDR_PANEL_{k}") for k in ("COLUMN", "LINE", "WIDTH", "HEIGHT")]
    if not frame_id or not all(cell):
        return None
    return _backend("clicked", frame_id, *cell)


def focus_pane(pane_id):
    if DEMO:
        return False
    try:
        command = os.environ.get("HERDR_BIN_PATH", "herdr")
        return subprocess.run([command, "agent", "focus", pane_id], capture_output=True, timeout=5).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def is_closed(node):
    return bool(node["pr"]) and node["pr"]["state"] == "CLOSED"


def epoch(stamp):
    return datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()
