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
        self.assertIn("0.1.0-alpha.5", self.command("--version").stdout)
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

    def test_a_session_name_targets_its_pane(self):
        _, frame = self.render("producer-z", HERDR_PANEL_ZOOM="2")
        sessions = {s["pane"] for s in self.data("--demo", "model")["sessions"]}
        x, y, target = next(cell for cell in frame["hit"] if cell[2].startswith(":pane:"))
        self.assertIn(target.removeprefix(":pane:"), sessions)
        self.assertEqual(self.data("--demo", "clicked", "producer-z", str(x), str(y), "26", "12"), target)

    def test_report_names_panes_after_their_sessions(self):
        tools = self.root / "bin"
        tools.mkdir()
        for tool in ["git", "cat"]:
            (tools / tool).symlink_to(shutil.which(tool))
        (tools / "gh").write_text("#!/bin/sh\nexit 1\n")
        (tools / "herdr").write_text('#!/bin/sh\ncase "$1 $2" in\n'
                                     '  "api snapshot") cat "$SNAPSHOT" ;;\n'
                                     '  "pane report-metadata") shift 2; echo "$*" >> "$LOG" ;;\n'
                                     '  *) exit 1 ;;\nesac\n')
        for tool in ["gh", "herdr"]:
            (tools / tool).chmod(0o755)
        subprocess.run(["git", "init", "--initial-branch=main", str(self.root / "checkout")],
                       check=True, capture_output=True)
        cfg = self.root / "config.toml"
        cfg.write_text('repo = "example/harbor"\nworkspace = "."\ncheckout = "checkout"\n')
        agent = {"agent": "claude", "agent_status": "working", "cwd": str(self.root),
                 "tab_id": "w1:t1", "workspace_id": "w1", "tokens": {}}
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(json.dumps({"result": {"snapshot": {
            "agents": [agent | {"pane_id": "w1:p1", "terminal_title_stripped": "search tuning"},
                       agent | {"pane_id": "w1:p2", "terminal_title_stripped": "index work",
                                "label": "set by hand"}],
            "tabs": [{"tab_id": "w1:t1", "label": "1"}],
            "workspaces": [{"workspace_id": "w1", "label": "harbor"}]}}}))
        log = self.root / "report.log"
        result = self.command("--config", str(cfg), "report", PATH=str(tools),
                              SNAPSHOT=str(snapshot), LOG=str(log))
        self.assertEqual(result.returncode, 0, result.stderr)
        # The session name becomes the pane title; a label set by hand keeps precedence.
        self.assertEqual(log.read_text().splitlines(), ["w1:p1 --source pr-map --title search tuning"])

    def test_agents_work_for_the_project_that_places_them(self):
        tools = self.root / "bin"
        tools.mkdir()
        for tool in ["git", "cat"]:
            (tools / tool).symlink_to(shutil.which(tool))
        (tools / "gh").write_text("#!/bin/sh\nexit 1\n")
        (tools / "herdr").write_text('#!/bin/sh\ncase "$1 $2" in\n'
                                     '  "api snapshot") cat "$SNAPSHOT" ;;\n'
                                     '  "pane report-metadata") shift 2; echo "$*" >> "$LOG" ;;\n'
                                     '  *) exit 1 ;;\nesac\n')
        for tool in ["gh", "herdr"]:
            (tools / tool).chmod(0o755)
        # example/harbor-ops is the workspace's own repository; example/harbor works from that
        # workspace with its checkout and a worktree inside it.
        ws = self.root / "ws"
        git = ["git", "-c", "user.name=test", "-c", "user.email=test@example.invalid"]
        for repo in [ws, ws / "harbor"]:
            subprocess.run(["git", "init", "--initial-branch=main", str(repo)], check=True, capture_output=True)
            subprocess.run([*git, "-C", str(repo), "commit", "--allow-empty", "-m", "init"],
                           check=True, capture_output=True)
        search = ws / "experiments" / "search"
        subprocess.run([*git, "-C", str(ws / "harbor"), "worktree", "add", "-b", "feat/search", str(search)],
                       check=True, capture_output=True)
        cfg = self.root / "config.toml"
        cfg.write_text('[[project]]\nrepo = "example/harbor"\nworkspace = "ws"\ncheckout = "ws/harbor"\n\n'
                       '[[project]]\nrepo = "example/harbor-ops"\ncheckout = "ws"\n')
        # The session in the workspace that edits harbor's worktree works for harbor.
        transcript = self.root / ".claude" / "projects" / "ws" / "s2.jsonl"
        transcript.parent.mkdir(parents=True)
        transcript.write_text("\n".join(json.dumps(r) for r in [
            {"type": "assistant", "timestamp": "2026-10-19T12:00:00Z", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": f"{search}/app.py"}}]}},
            {"type": "user", "timestamp": "2026-10-19T12:00:01Z", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1"}]}}]) + "\n")
        agent = {"agent": "claude", "agent_status": "idle", "tab_id": "w1:t1", "workspace_id": "w1", "tokens": {}}
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(json.dumps({"result": {"snapshot": {
            "agents": [agent | {"pane_id": "w1:p1", "cwd": str(search)},
                       agent | {"pane_id": "w1:p2", "cwd": str(ws), "agent_session": {"value": "s2"}},
                       agent | {"pane_id": "w1:p3", "cwd": str(ws), "terminal_title_stripped": "ops chores"},
                       agent | {"pane_id": "w1:p4", "cwd": "/nonexistent/tools"}],
            "tabs": [{"tab_id": "w1:t1", "label": "1"}],
            "workspaces": [{"workspace_id": "w1", "label": "harbor"}]}}}))
        env = {"PATH": str(tools), "SNAPSHOT": str(snapshot), "LOG": str(self.root / "report.log")}

        model = self.data("--config", str(cfg), "model", **env)
        self.assertEqual(model["repo"], "example/harbor")
        self.assertEqual({s["pane"]: s["current"] for s in model["sessions"]},
                         {"w1:p1": "feat/search", "w1:p2": "feat/search"})
        self.assertEqual(model["other_repos"], [
            {"repo": "example/harbor-ops", "agents": {"w1:p3": "ops chores"}, "open_prs": None}])
        self.assertEqual(list(model["elsewhere"]), ["tools"])
        ops = self.data("--config", str(cfg), "model", PRMAP_PROJECT="example/harbor-ops", **env)
        self.assertEqual([s["pane"] for s in ops["sessions"]], ["w1:p3"])
        self.assertEqual([(o["repo"], sorted(o["agents"])) for o in ops["other_repos"]],
                         [("example/harbor", ["w1:p1", "w1:p2"])])

        # The panel row of the other project opens its map; its agent's pane is marked there.
        result = self.command("--config", str(cfg), "minimap", HERDR_PANEL_FRAME_ID="multi-1",
                              HERDR_ACTIVE_PANE_ID="w1:p3", COLUMNS="26", LINES="12", **env)
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(next((self.root / "cache").glob("pr-map/*/frames/multi-1.json")).read_text())
        x, y, target = next(cell for cell in record["hit"] if cell[2].startswith(":project:"))
        self.assertEqual(target, ":project:example/harbor-ops")
        self.assertEqual(self.data("--config", str(cfg), "clicked", "multi-1", str(x), str(y), "26", "12", **env),
                         target)
        self.assertIn("harbor-ops", result.stdout.splitlines()[-1])
        # Herdr's Agents rows show that agent's repository and session name.
        self.assertEqual(self.command("--config", str(cfg), "report", **env).returncode, 0)
        self.assertIn("w1:p3 --source pr-map --token pr=◈ harbor-ops --title ops chores",
                      (self.root / "report.log").read_text().splitlines())

    def test_click_rejects_size_mismatch_and_invalid_ids(self):
        self.render("producer-1")
        self.assertIsNone(self.data("--demo", "clicked", "producer-1", "0", "11", "27", "12"))
        self.assertNotEqual(self.command("--demo", "model", "--frame", "../secret").returncode, 0)
        self.assertNotEqual(self.command("--demo", "model", "--frame", "missing-1").returncode, 0)

    def test_panel_sizes_and_unicode_labels(self):
        from rich.text import Text
        model = self.data("--demo", "model")
        model["nodes"]["feat/search"]["name"] = "文档搜索与索引"
        model["other_repos"] = [{"repo": "example/lighthouse-operations", "open_prs": 12,
                                 "agents": {f"w9:p{i}": None for i in range(5)}}]
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
