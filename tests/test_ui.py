"""Drive the real Textual UI with the offline example through its public test pilot."""
import asyncio
import os
from pathlib import Path
import unittest
from unittest.mock import patch

from pr_map import cli, prmap
from pr_map.app import PRMapApp, MetroMap


class UITests(unittest.IsolatedAsyncioTestCase):
    async def test_demo_navigation_search_and_resize(self):
        old_demo, old_bin = prmap.DEMO, prmap.BIN
        prmap.DEMO = True
        prmap.BIN = os.environ["PRMAP_TEST_BIN"]
        env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "PRMAP_CONFIG"))}
        try:
            with patch.dict(os.environ, env, clear=True):
                app = PRMapApp()
                async with app.run_test(size=(140, 42)) as pilot:
                    for _ in range(30):
                        await pilot.pause(0.05)
                        if app.model:
                            break
                    self.assertEqual(app.model["repo"], "example/harbor")
                    metro = app.query_one(MetroMap)
                    self.assertEqual(metro.selected, "feat/search")
                    await pilot.press("a", "slash")
                    await pilot.press(*list("payments"), "enter")
                    await pilot.pause()
                    self.assertEqual(metro.selected, "feat/payments")
                    await pilot.resize_terminal(85, 26)
                    await pilot.pause()
                    await pilot.resize_terminal(160, 48)
                    await pilot.pause()
                    self.assertEqual(metro.selected, "feat/payments")
        finally:
            prmap.DEMO, prmap.BIN = old_demo, old_bin

    def test_a_clicked_session_is_focused_without_opening_the_map(self):
        focused = []
        with patch.object(prmap, "clicked", return_value=prmap.PANE + "w1:p2"), \
             patch.object(prmap, "focus_pane", side_effect=lambda pane: focused.append(pane) or True), \
             patch.object(PRMapApp, "run", side_effect=AssertionError("the map opened")), \
             patch("sys.argv", ["pr-map", "--popup"]):
            cli.main()
        self.assertEqual(focused, ["w1:p2"])

    async def test_a_clicked_session_it_cannot_focus_opens_its_station(self):
        old_demo, old_bin = prmap.DEMO, prmap.BIN
        prmap.DEMO = True
        prmap.BIN = os.environ["PRMAP_TEST_BIN"]
        env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "PRMAP_CONFIG"))}
        try:
            with patch.dict(os.environ, env, clear=True):
                # The demo never focuses a real pane, so the map opens on the session instead.
                app = PRMapApp(popup=True, clicked=prmap.PANE + "w1:p3")
                async with app.run_test(size=(140, 42)) as pilot:
                    for _ in range(30):
                        await pilot.pause(0.05)
                        if app.model:
                            break
                    session = next(s for s in app.model["sessions"] if s["pane"] == "w1:p3")
                    self.assertEqual(app.query_one(MetroMap).selected, session["current"])
        finally:
            prmap.DEMO, prmap.BIN = old_demo, old_bin


if __name__ == "__main__":
    unittest.main()
