"""Exercise the installed process boundary with fictional fixtures and isolated caches."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = os.environ.get("PRMAP_TEST_BIN", str(ROOT / "target/debug/prmap"))


class CLITests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pr-map-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "PRMAP_"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_CACHE_HOME=str(self.root / "cache"), PATH="/nonexistent")

    def command(self, *args, **env):
        return subprocess.run([BIN, *args], env=self.env | env, capture_output=True, text=True, timeout=15)

    def data(self, *args, **env):
        result = self.command(*args, **env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def render(self, frame, pane="w1:p1", **view):
        result = self.command("--demo", "minimap", HERDR_PANEL_FRAME_ID=frame,
                              HERDR_ACTIVE_PANE_ID=pane, COLUMNS="26", LINES="12", **view)
        self.assertEqual(result.returncode, 0, result.stderr)
        record = next((self.root / "cache").glob(f"pr-map/*/frames/{frame}.json"))
        return result.stdout, json.loads(record.read_text())

    def test_demo_is_offline_and_fictional(self):
        model = self.data("--demo", "model")
        self.assertEqual(model["repo"], "example/harbor")
        self.assertEqual(len(model["sessions"]), 4)
        self.assertEqual(model["git"], "/demo/harbor")
        self.assertIn("feat/search", model["nodes"])
        self.assertEqual(self.command("--demo", "report").stdout, "#102 Search")
        self.assertIn("search", self.data("--demo", "commits", "feat/search")[0][3])

    def test_demo_uses_fictional_position_inside_an_existing_session(self):
        from rich.text import Text
        result = self.command("--demo", "minimap", HERDR_ACTIVE_PANE_ID="w9:p9")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("#102", Text.from_ansi(result.stdout).plain.splitlines()[-1])

    def test_help_and_version_need_no_configuration(self):
        self.assertEqual(self.command("--help").returncode, 0)
        self.assertIn("0.1.0-alpha.2", self.command("--version").stdout)
        self.assertFalse((self.root / "cache").exists())

    def test_frames_survive_other_views(self):
        _, a = self.render("producer-1", "w1:p1")
        _, b = self.render("producer-2", "w1:p3")
        ah = {(x, y): branch for x, y, branch in a["hit"]}
        bh = {(x, y): branch for x, y, branch in b["hit"]}
        cell = next(cell for cell in ah.keys() & bh.keys() if ah[cell] != bh[cell])
        first = self.data("--demo", "clicked", "producer-1", *map(str, (*cell, 26, 12)))
        second = self.data("--demo", "clicked", "producer-2", *map(str, (*cell, 26, 12)))
        self.assertEqual(first, ah[cell])
        self.assertEqual(second, bh[cell])
        self.assertNotEqual(first, second)
        shown = self.data("--demo", "model", "--frame", "producer-1")
        self.assertIn(first, shown["nodes"])

    def test_click_rejects_size_mismatch_and_invalid_ids(self):
        self.render("producer-1")
        self.assertIsNone(self.data("--demo", "clicked", "producer-1", "0", "11", "27", "12"))
        self.assertNotEqual(self.command("--demo", "model", "--frame", "../secret").returncode, 0)
        self.assertNotEqual(self.command("--demo", "model", "--frame", "missing-1").returncode, 0)

    def test_panel_sizes_and_unicode_labels(self):
        from rich.text import Text
        model = self.data("--demo", "model")
        model["nodes"]["feat/search"]["name"] = "文档搜索与索引"
        fixture = self.root / "model.json"
        fixture.write_text(json.dumps(model))
        for width in [1, 12, 26, 40]:
            for zoom in range(3):
                result = self.command("minimap", "--model", str(fixture), COLUMNS=str(width),
                                      LINES="12", HERDR_PANEL_ZOOM=str(zoom), HERDR_PANEL_FRAME_ID=f"fixture-{width}-{zoom}")
                self.assertEqual(result.returncode, 0, result.stderr)
                rows = result.stdout.splitlines()
                self.assertLessEqual(len(rows), 12)
                for row in rows:
                    self.assertLessEqual(Text.from_ansi(row).cell_len, width)

    def test_configuration_outside_workspace(self):
        git = shutil.which("git")
        checkout = self.root / "checkout"
        subprocess.run([git, "init", "--initial-branch=main", str(checkout)], check=True, capture_output=True)
        tools = self.root / "bin"
        tools.mkdir()
        (tools / "git").symlink_to(git)
        for name in ["gh", "herdr"]:
            tool = tools / name
            tool.write_text("#!/bin/sh\nexit 1\n")
            tool.chmod(0o755)
        cfg = self.root / "config.toml"
        cfg.write_text(f'repo = "example/harbor"\nworkspace = "."\ncheckout = "checkout"\n')
        model = self.data("--config", str(cfg), "model", PATH=str(tools))
        self.assertEqual(model["repo"], "example/harbor")
        self.assertEqual(model["git"], str(checkout))
        self.assertFalse(model["herdr"])


if __name__ == "__main__":
    unittest.main()
