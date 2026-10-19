"""Drive the real Textual UI with the offline example through its public test pilot."""
import asyncio
import os
from pathlib import Path
import unittest
from unittest.mock import patch

from pr_map import prmap
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


if __name__ == "__main__":
    unittest.main()
