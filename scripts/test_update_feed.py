"""Tests for scripts/update_feed.py: python3 -m unittest discover -s scripts"""

import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import update_feed

URL = "https://github.com/1905/stylus-spotify-mini-player/releases/download/v0.4.0/Stylus.app.tar.gz"
SIG = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZQ==\n"
NOW = datetime(2026, 10, 24, 16, 30, 5, tzinfo=timezone(timedelta(hours=8)))


class Feed(unittest.TestCase):
    def test_shape(self):
        f = update_feed.feed("0.4.0", "Stylus 0.4.0\n", SIG, URL, NOW)
        self.assertEqual(f, {
            "version": "0.4.0",
            "notes": "Stylus 0.4.0",
            "pub_date": "2026-10-24T08:30:05Z",
            "platforms": {"darwin-aarch64": {"signature": SIG.strip(), "url": URL}},
        })

    def test_refuses_bad_input(self):
        for args in [
            ("v0.4.0", "", SIG, URL),
            ("0.4", "", SIG, URL),
            ("0.4.0", "", "  \n", URL),
            ("0.4.0", "", SIG, "ftp://x/Stylus.app.tar.gz"),
            ("0.4.0", "", SIG, "https://x/Stylus.dmg"),
            ("0.4.0", "", SIG, "Stylus.app.tar.gz"),
        ]:
            with self.subTest(args), self.assertRaises(ValueError):
                update_feed.feed(*args)

    def test_local_http_feed_for_the_e2e_test(self):
        url = "http://127.0.0.1:8798/Stylus.app.tar.gz"
        self.assertEqual(update_feed.feed("0.4.1", "", SIG, url)["platforms"]["darwin-aarch64"]["url"], url)

    def test_main_writes_the_file(self):
        with tempfile.TemporaryDirectory() as d:
            sig, notes, out = Path(d) / "a.sig", Path(d) / "notes.md", Path(d) / "latest.json"
            sig.write_text(SIG)
            notes.write_text("- One.\n- Two.\n")
            update_feed.main(["0.4.0", "--notes-file", str(notes), "--sig", str(sig), "--url", URL, "--out", str(out)])
            f = json.loads(out.read_text())
            self.assertEqual(f["notes"], "- One.\n- Two.")
            self.assertEqual(f["platforms"]["darwin-aarch64"]["signature"], SIG.strip())
            self.assertRegex(f["pub_date"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")


if __name__ == "__main__":
    unittest.main()
