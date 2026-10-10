"""Tests for scripts/release.py's pure helpers: python3 -m unittest discover -s scripts"""

import difflib
import json
import unittest

import release

CASK = '''cask "stylus" do
  version "0.2.0"
  sha256 "f658ad548b1cd629520076010f2a47e8664cd9d04c305bb78efcf007943c2cef"

  url "https://github.com/1905/stylus-spotify-mini-player/releases/download/v#{version}/Stylus.dmg"
end
'''
SHA = "a" * 64


def changed_lines(old, new):
    return [l for l in difflib.ndiff(old.splitlines(), new.splitlines()) if l.startswith("+ ")]


class Versions(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(release.parse_version("0.3.0"), (0, 3, 0))
        self.assertGreater(release.parse_version("0.10.0"), release.parse_version("0.9.9"))
        for bad in ["0.3", "v0.3.0", "0.3.0-rc1", ""]:
            with self.assertRaises(ValueError, msg=bad):
                release.parse_version(bad)

    def test_bump_the_real_files(self):
        # the repo's own files: a format change there fails here, not in the middle of a release
        current = json.loads((release.ROOT / "package.json").read_text())["version"]
        for name in release.VERSION_FILES:
            with self.subTest(name):
                old = (release.ROOT / name).read_text()
                new = release.bump_file(name, old, current, "99.0.0")
                lines = changed_lines(old, new)
                self.assertEqual(len(lines), 2 if name == "package-lock.json" else 1, lines)
                self.assertTrue(all("99.0.0" in l for l in lines), lines)

    def test_bump_refuses_a_wrong_old_version(self):
        for name in release.VERSION_FILES:
            with self.subTest(name), self.assertRaises(ValueError):
                release.bump_file(name, (release.ROOT / name).read_text(), "98.7.6", "99.0.0")

    def test_package_lock_leaves_a_dependency_with_the_same_version(self):
        lock = json.dumps({
            "name": "stylus", "version": "0.2.0", "lockfileVersion": 3,
            "packages": {"": {"name": "stylus", "version": "0.2.0"}, "node_modules/x": {"version": "0.2.0"}},
        }, indent=2)
        out = json.loads(release.bump_file("package-lock.json", lock, "0.2.0", "0.3.0"))
        self.assertEqual((out["version"], out["packages"][""]["version"]), ("0.3.0", "0.3.0"))
        self.assertEqual(out["packages"]["node_modules/x"]["version"], "0.2.0")

    def test_package_lock_refuses_when_a_dependency_comes_first(self):
        lock = '{\n  "name": "stylus",\n  "x": {"version": "0.2.0"},\n  "version": "0.2.0",\n  "packages": {"": {"version": "0.2.0"}}\n}'
        with self.assertRaises(ValueError):
            release.bump_file("package-lock.json", lock, "0.2.0", "0.3.0")


class Notes(unittest.TestCase):
    def test_highlights(self):
        self.assertEqual(release.highlights("- One.\n\n- Two,\n  more.\n"), "- One.\n- Two,\n  more.")
        for bad in ["", "Some prose.", "  - indented first", "- One.\nprose"]:
            with self.assertRaises(ValueError, msg=bad):
                release.highlights(bad)

    def test_body(self):
        body = release.release_body("0.3.0", "v0.2.0", "- Copy song link.\n")
        self.assertTrue(body.startswith("Stylus 0.3.0 for macOS (Apple Silicon).\n\nHighlights:\n- Copy song link.\n"))
        self.assertIn("brew install --cask 1905/tap/stylus", body)
        self.assertIn("xattr -dr com.apple.quarantine /Applications/Stylus.app", body)
        self.assertTrue(body.endswith("compare/v0.2.0...v0.3.0\n"))


class Cask(unittest.TestCase):
    def test_update(self):
        out = release.cask_text(CASK, "0.3.0", SHA)
        self.assertEqual(release.cask_state(out), ("0.3.0", SHA))
        self.assertEqual(len(changed_lines(CASK, out)), 2)
        self.assertIn("download/v#{version}/Stylus.dmg", out)

    def test_state(self):
        self.assertEqual(release.cask_state(CASK), ("0.2.0", "f658ad548b1cd629520076010f2a47e8664cd9d04c305bb78efcf007943c2cef"))

    def test_refuses_a_cask_without_the_lines(self):
        with self.assertRaises(ValueError):
            release.cask_text('cask "stylus" do\nend\n', "0.3.0", SHA)


if __name__ == "__main__":
    unittest.main()
