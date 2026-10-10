#!/usr/bin/env python3
"""Release Stylus: version bump, tag, DMG, GitHub release notes, Homebrew cask.

    make release V=0.3.1 NOTES=/tmp/notes.md            # DMG from CI (the v* tag build)
    make release V=0.3.1 NOTES=/tmp/notes.md DMG=air    # DMG from the MacBook Air (make dmg)
    make release V=0.3.1 NOTES=/tmp/notes.md DRY=1      # print the steps, change nothing

NOTES is a markdown file of "- " highlight lines; the script wraps them in the release template.
Every step checks whether it is already done, so a second run continues after a failure.
Tests: python3 -m unittest discover -s scripts
"""

import argparse
import base64
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = "1905/stylus-spotify-mini-player"
TAP = "1905/homebrew-tap"
CASK = "Casks/stylus.rb"
GH_USER = "1905"
WORKFLOW = "build.yml"
# what the app is built from: on --dmg air the working tree must match the tag here
BUILD_PATHS = ["src", "src-tauri", "package.json", "package-lock.json"]
AIR_DMG = Path("/tmp/stylus-build/dmg/Stylus.dmg")  # where `make dmg` puts it

ROOT = Path(__file__).resolve().parent.parent
DRY = False


# ---------- pure helpers (scripts/test_release.py) ----------

def parse_version(v):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", v)
    if not m:
        raise ValueError(f"not a version (X.Y.Z): {v!r}")
    return tuple(int(x) for x in m.groups())


def replace_once(text, pattern, repl, what):
    new, n = re.subn(pattern, repl, text, flags=re.M)
    if n != 1:
        raise ValueError(f"{what}: expected 1 match of {pattern!r}, found {n}")
    return new


def bump_file(name, text, old, new):
    """The text of one version file with old replaced by new, the app's own version only."""
    o, n = re.escape(old), new
    if name in ("package.json", "src-tauri/tauri.conf.json"):
        return replace_once(text, rf'^(  "version": )"{o}"', rf'\1"{n}"', name)
    if name == "src-tauri/Cargo.toml":
        return replace_once(text, rf'^version = "{o}"$', f'version = "{n}"', name)
    if name == "src-tauri/Cargo.lock":
        return replace_once(text, rf'^(name = "stylus"\nversion = )"{o}"$', rf'\1"{n}"', name)
    if name == "package-lock.json":
        # the root "version" and packages[""].version come first; json.loads proves nothing else moved
        want = json.loads(text)
        if want["version"] != old or want["packages"][""]["version"] != old:
            raise ValueError(f"{name}: the app version is not {old}")
        want["version"] = want["packages"][""]["version"] = new
        out = re.sub(rf'("version": )"{o}"', rf'\1"{n}"', text, count=2)
        if json.loads(out) != want:
            raise ValueError(f"{name}: the first two versions are not the app's own")
        return out
    raise ValueError(f"unknown version file {name}")


VERSION_FILES = ["package.json", "package-lock.json", "src-tauri/tauri.conf.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock"]


def highlights(notes):
    """The "- " lines of a notes file, or ValueError when there are none."""
    lines = [l.rstrip() for l in notes.splitlines() if l.strip()]
    if not lines or not all(l.startswith("- ") or l.startswith("  ") for l in lines) or not lines[0].startswith("- "):
        raise ValueError('notes: write only "- " highlight lines (indented continuation lines are fine)')
    return "\n".join(lines)


def release_body(version, prev_tag, notes):
    return f"""Stylus {version} for macOS (Apple Silicon).

Highlights:
{highlights(notes)}

Install: `brew install --cask 1905/tap/stylus` or download Stylus.dmg below.
The app is not signed with an Apple Developer ID. On first launch run: `xattr -dr com.apple.quarantine /Applications/Stylus.app`

Requires Spotify Premium.

**Full Changelog**: https://github.com/{REPO}/compare/{prev_tag}...v{version}
"""


def cask_text(text, version, sha256):
    text = replace_once(text, r'^(  version )"[^"]*"$', rf'\1"{version}"', CASK)
    return replace_once(text, r'^(  sha256 )"[0-9a-f]{64}"$', rf'\1"{sha256}"', CASK)


def cask_state(text):
    """(version, sha256) of the cask."""
    v = re.search(r'^  version "([^"]*)"$', text, re.M)
    s = re.search(r'^  sha256 "([0-9a-f]{64})"$', text, re.M)
    return (v and v.group(1), s and s.group(1))


# ---------- shell ----------

def run(cmd, mutate=False, check=True, capture=True):
    """Run a command (a list); mutate=True ones only print on --dry-run. Returns stdout, stripped."""
    shown = " ".join(cmd)
    if mutate and DRY:
        print(f"  [dry-run] {shown}")
        return ""
    if mutate:
        print(f"  $ {shown}")
    r = subprocess.run(cmd, cwd=ROOT, text=True, capture_output=capture)
    if check and r.returncode != 0:
        sys.exit(f"✗ {shown}\n{(r.stderr or r.stdout or '').strip()}")
    return (r.stdout or "").strip() if capture else ""


def ok(cmd):
    return subprocess.run(cmd, cwd=ROOT, capture_output=True).returncode == 0


def step(title):
    print(f"\n→ {title}")


def gh_json(args):
    out = run(["gh"] + args)
    return json.loads(out) if out else None


# ---------- steps ----------

def preflight(version, notes_path, dmg):
    step("checks")
    parse_version(version)
    highlights(Path(notes_path).read_text())
    if run(["gh", "api", "user", "--jq", ".login"]) != GH_USER:
        sys.exit(f"✗ gh is not logged in as {GH_USER}")
    branch = run(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    if run(["git", "status", "--porcelain"]):
        msg = "the working tree is not clean"
        if not DRY:
            sys.exit(f"✗ {msg}")
        print(f"  ⚠ {msg} (dry-run goes on)")
    if branch != "master":
        if not DRY:
            sys.exit(f"✗ on {branch}: releases start from master")
        print(f"  ⚠ on {branch}, not master (dry-run goes on)")
    run(["git", "fetch", "-q", "origin", "master", "--tags"])
    if not ok(["git", "merge-base", "--is-ancestor", "origin/master", "HEAD"]):
        sys.exit("✗ origin/master has commits this branch lacks: pull first")
    if dmg == "air" and not ok(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "air", "true"]):
        sys.exit("✗ ssh air does not answer")
    print("  ✓ notes, gh login, git state" + (", the Air" if dmg == "air" else ""))


def bump(version):
    step(f"version {version}")
    current = json.loads((ROOT / "package.json").read_text())["version"]
    if current == version:
        print("  ✓ already bumped")
        return
    if parse_version(version) <= parse_version(current):
        sys.exit(f"✗ {version} is not above the current {current}")
    texts = {}
    for name in VERSION_FILES:
        texts[name] = bump_file(name, (ROOT / name).read_text(), current, version)
        print(f"  {name}: {current} → {version}")
    if DRY:
        print("  [dry-run] files not written, nothing committed")
        return
    rel = f"chore/release-{version}"
    run(["git", "checkout", "-q", "-b", rel], mutate=True)
    for name, text in texts.items():
        (ROOT / name).write_text(text)
    run(["git", "add"] + VERSION_FILES, mutate=True)
    run(["git", "commit", "-q", "-m", f"Release {version}: version bump"], mutate=True)
    run(["git", "checkout", "-q", "master"], mutate=True)
    run(["git", "merge", "--no-ff", "-q", rel, "-m", f"Merge {rel}"], mutate=True)
    run(["git", "branch", "-q", "-d", rel], mutate=True)


def push_master():
    step("push master")
    if run(["git", "rev-parse", "HEAD"]) == run(["git", "rev-parse", "origin/master"]):
        print("  ✓ origin/master is up to date")
        return
    run(["git", "push", "-q", "origin", "HEAD:master"], mutate=True)


def tag(version):
    t = f"v{version}"
    step(f"tag {t}")
    if not ok(["git", "rev-parse", "-q", "--verify", f"refs/tags/{t}"]):
        run(["git", "tag", "-a", t, "-m", f"Stylus {version}"], mutate=True)
    elif json.loads(run(["git", "show", f"{t}:package.json"]))["version"] != version:
        sys.exit(f"✗ {t} exists but its package.json is not {version}")
    if run(["git", "ls-remote", "--tags", "origin", f"refs/tags/{t}"]):
        print(f"  ✓ {t} is on origin")
    else:
        run(["git", "push", "-q", "origin", t], mutate=True)


def prev_tag(t):
    """The release before t: the newest tag under t, or under HEAD while t is not made yet (dry-run)."""
    base = f"{t}^" if ok(["git", "rev-parse", "-q", "--verify", f"refs/tags/{t}"]) else "HEAD"
    return run(["git", "describe", "--tags", "--abbrev=0", base])


def release_assets(t):
    """Asset names of the release, or None when there is no release."""
    if not ok(["gh", "release", "view", t, "-R", REPO]):
        return None
    return [a["name"] for a in gh_json(["release", "view", t, "-R", REPO, "--json", "assets"])["assets"]]


def tag_run(t, wait_s):
    """The newest build run of the tag, waiting up to wait_s for it to show."""
    for _ in range(max(1, wait_s // 5)):
        runs = gh_json(["run", "list", "-R", REPO, "--workflow", WORKFLOW, "--branch", t, "-L", "1",
                        "--json", "databaseId,status,conclusion"])
        if runs:
            return runs[0]
        if DRY:
            return None
        time.sleep(5)
    return None


def dmg_ci(t):
    step("DMG from CI")
    if DRY:
        print(f"  [dry-run] gh run watch <the {t} build> --exit-status")
        return
    r = tag_run(t, 60)
    if not r:
        sys.exit("✗ no build run for the tag after 60 s")
    rid = str(r["databaseId"])
    if subprocess.run(["gh", "run", "watch", rid, "-R", REPO, "--exit-status"], cwd=ROOT).returncode == 0:
        return
    # a job GitHub never started (billing lock, no runner) has no log, only an annotation
    sha = run(["git", "rev-list", "-n", "1", t])
    notes = []
    for cr in gh_json(["api", f"repos/{REPO}/commits/{sha}/check-runs"])["check_runs"]:
        for a in gh_json(["api", f"repos/{REPO}/check-runs/{cr['id']}/annotations"]) or []:
            notes.append(a["message"])
    print("\n".join(f"  ! {n}" for n in notes))
    if any("billing" in n for n in notes):
        sys.exit("✗ GitHub did not start the job (billing). Fix it, or run again with DMG=air")
    print(run(["gh", "run", "view", rid, "-R", REPO, "--log-failed"], check=False)[-6000:])
    sys.exit(f"✗ build run {rid} failed: fix it, push, then run this again")


def dmg_air(t, version, body):
    step("DMG from the Air")
    # make dmg ships the working tree: it must be the tag's source
    changed = run(["git", "diff", "--name-only", t, "--"] + BUILD_PATHS)
    untracked = run(["git", "ls-files", "--others", "--exclude-standard", "--"] + BUILD_PATHS)
    if changed or untracked:
        sys.exit(f"✗ the build sources differ from {t}:\n{changed}\n{untracked}".rstrip())
    # a CI build of the tag would upload its own DMG over this one: stop it before the upload.
    # GitHub makes the run some seconds after the tag push, so wait for it to show
    if not DRY:
        r = tag_run(t, 90)
        if not r:
            sys.exit(f"✗ no build run for {t} after 90 s: a late one could replace the Air DMG")
        if r["status"] != "completed":
            run(["gh", "run", "cancel", str(r["databaseId"]), "-R", REPO], mutate=True)
            while tag_run(t, 5)["status"] != "completed":
                time.sleep(5)
    run(["make", "dmg"], mutate=True, capture=False)
    if DRY:
        print(f"  [dry-run] gh release create {t} Stylus.dmg Stylus.dmg.sha256")
        return
    digest = hashlib.sha256(AIR_DMG.read_bytes()).hexdigest()
    sha_file = AIR_DMG.with_name("Stylus.dmg.sha256")
    sha_file.write_text(f"{digest}  Stylus.dmg\n")
    files = [str(AIR_DMG), str(sha_file)]
    if release_assets(t) is None:
        with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as f:
            f.write(body)
        run(["gh", "release", "create", t, "-R", REPO, "--title", f"Stylus {version}", "--notes-file", f.name] + files, mutate=True)
    else:
        run(["gh", "release", "upload", t, "-R", REPO, "--clobber"] + files, mutate=True)


def notes_step(t, version, body):
    step("release notes")
    if DRY:
        print("  [dry-run] gh release edit, body:\n")
        print("    " + body.replace("\n", "\n    "))
        return
    with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as f:
        f.write(body)
    run(["gh", "release", "edit", t, "-R", REPO, "--title", f"Stylus {version}", "--notes-file", f.name, "--draft=false"], mutate=True)


def dmg_digest(t):
    """sha256 of the released DMG, from the file itself (not the .sha256 asset)."""
    with tempfile.TemporaryDirectory() as d:
        run(["gh", "release", "download", t, "-R", REPO, "-p", "Stylus.dmg", "-D", d])
        return hashlib.sha256((Path(d) / "Stylus.dmg").read_bytes()).hexdigest()


def cask(t, version):
    step(f"Homebrew cask ({TAP})")
    if DRY:
        print(f"  [dry-run] {CASK}: version {version}, sha256 of the released Stylus.dmg")
        return
    digest = dmg_digest(t)
    meta = gh_json(["api", f"repos/{TAP}/contents/{CASK}"])
    text = base64.b64decode(meta["content"]).decode()
    if cask_state(text) == (version, digest):
        print("  ✓ already current")
        return
    payload = json.dumps({
        "message": f"stylus {version}",
        "content": base64.b64encode(cask_text(text, version, digest).encode()).decode(),
        "sha": meta["sha"],
    })
    print(f"  $ gh api -X PUT repos/{TAP}/contents/{CASK} (stylus {version})")
    r = subprocess.run(["gh", "api", "-X", "PUT", f"repos/{TAP}/contents/{CASK}", "--input", "-"],
                       input=payload, text=True, capture_output=True)
    if r.returncode != 0:
        sys.exit(f"✗ cask update failed: {r.stderr.strip()}")


def verify(t, version):
    step("verify")
    if DRY:
        print("  [dry-run] download URL answers 200, cask shows the new version")
        return
    url = f"https://github.com/{REPO}/releases/download/{t}/Stylus.dmg"
    code = run(["curl", "-sIL", "-o", "/dev/null", "-w", "%{http_code}", url])
    if code != "200":
        sys.exit(f"✗ {url} answered {code}")
    text = base64.b64decode(gh_json(["api", f"repos/{TAP}/contents/{CASK}"])["content"]).decode()
    if cask_state(text)[0] != version:
        sys.exit(f"✗ the cask still shows {cask_state(text)[0]}")
    print(f"  ✓ {url}\n  ✓ cask {version}")


def main():
    global DRY
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("version", help="X.Y.Z")
    p.add_argument("--notes", required=True, help='markdown file of "- " highlight lines')
    p.add_argument("--dmg", choices=["ci", "air"], default="ci", help="who builds the DMG (default: ci)")
    p.add_argument("--dry-run", action="store_true", help="print the steps, change nothing")
    a = p.parse_args()
    DRY = a.dry_run
    t = f"v{a.version}"

    preflight(a.version, a.notes, a.dmg)
    bump(a.version)
    push_master()
    tag(a.version)
    body = release_body(a.version, prev_tag(t), Path(a.notes).read_text())
    assets = release_assets(t)
    if assets and "Stylus.dmg" in assets:
        step("DMG")
        print("  ✓ the release has Stylus.dmg")
    elif a.dmg == "air":
        dmg_air(t, a.version, body)
    else:
        dmg_ci(t)
    notes_step(t, a.version, body)
    cask(t, a.version)
    verify(t, a.version)
    print(f"\n✓ Stylus {a.version} released" if not DRY else "\n✓ dry-run done, nothing changed")


if __name__ == "__main__":
    main()
