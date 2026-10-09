#!/usr/bin/env python3
"""MCP repro of the Spirc pause race (plans/2026-10-09-spirc-pause-race).

Each round sends resume, pause, a fast resume and a pause to Stylus through its MCP server, then
asks now_playing if the player still plays. After the rounds, the script reads the app log of the
run for pauses with no Paused event and for "Player::play called from invalid state" lines.

Usage:
    python3 dev/pause_race.py                          # 20 rounds on port 5590
    python3 dev/pause_race.py --rounds 40 --gap 0.15
    python3 dev/pause_race.py --no-log-check

Before you start: open Stylus, play a track on This Mac, turn on Settings -> MCP. The script pauses
and resumes the music. The key comes from ~/Library/Application Support/stylus/settings.json
(mcp_key). The script never prints the key.

Exit codes: 0 = no failed round and no log finding, 1 = a failed round or a log finding,
2 = setup error (no key, MCP off, connection refused, HTTP 401/403, precondition).
"""

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

APP_DIR = os.path.expanduser("~/Library/Application Support/stylus")
SETTINGS = os.path.join(APP_DIR, "settings.json")
LOG = os.path.join(APP_DIR, "logs", "stylus.log")
PROTOCOL = "2025-06-18"
LOG_TAIL_S = 3.0  # the log window ends this long after the last round

# "14:03:16.243Z INFO  ..." or "2026-10-09 14:03:16.243Z INFO  ..."
STAMP = re.compile(r"(?:\d{4}-\d{2}-\d{2} )?(\d{2}):(\d{2}):(\d{2})\.(\d{3})Z ")
CMD = "stylus::cmd: "
PAUSED = "stylus::player: Paused {"
INVALID = "Player::play called from invalid state"


class SetupError(Exception):
    """The run cannot start or continue: exit 2. The message never contains the key."""


class ToolError(Exception):
    """A tool call returned isError: true."""


# ---- pure logic (no I/O) ----------------------------------------------------------------------


def round_ok(state: dict) -> bool:
    """A round passes when now_playing says the player does not play after the last pause."""
    return not state.get("is_playing")


def _ms_of_day(t: datetime) -> int:
    """UTC time of day in ms. A naive datetime counts as UTC."""
    if t.tzinfo is not None:
        t = t.astimezone(timezone.utc)
    return ((t.hour * 60 + t.minute) * 60 + t.second) * 1000 + t.microsecond // 1000


def log_findings(lines: list[str], start: datetime, end: datetime) -> tuple[int, int]:
    """(P, I) for the log lines whose UTC time of day is from `start` to `end`, both included.

    P: pauses with no Paused event. A `stylus::cmd: pause` line opens a wait. A
    `stylus::player: Paused {` line closes it. The next `stylus::cmd:` line, or the window end,
    with the wait still open adds 1 to P.
    I: `Player::play called from invalid state` lines.
    `start` and `end` must be on the same UTC date: main() skips a run that crosses midnight.
    Lines with no stamp are ignored.
    """
    lo, hi = _ms_of_day(start), _ms_of_day(end)
    pauses = invalid = 0
    waiting = False
    for line in lines:
        m = STAMP.match(line)
        if not m:
            continue
        h, mi, s, ms = map(int, m.groups())
        if not lo <= ((h * 60 + mi) * 60 + s) * 1000 + ms <= hi:
            continue
        if INVALID in line:
            invalid += 1
        if PAUSED in line:
            waiting = False
        elif CMD in line:
            if waiting:
                pauses += 1
            waiting = line.split(CMD, 1)[1].strip() == "pause"
    if waiting:
        pauses += 1
    return pauses, invalid


# ---- MCP over HTTP ----------------------------------------------------------------------------

# main() sets url and key; mcp_call() does the handshake on its first call.
_mcp = {"url": "", "key": "", "headers": None, "next_id": 1}
# No proxy: a proxy from the environment must never get the bearer key.
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def _post(body: dict, headers: dict):
    """One POST to the MCP endpoint. Returns the parsed JSON body, or None for an empty body.
    Stores a session id from the server in `headers` (the server is stateless today)."""
    req = urllib.request.Request(_mcp["url"], data=json.dumps(body).encode(), headers=headers, method="POST")
    try:
        with _opener.open(req, timeout=15) as r:
            sid = r.headers.get("mcp-session-id")
            raw = r.read()
    except urllib.error.HTTPError as e:
        if e.code == 401:
            raise SetupError(f"MCP refused the key: HTTP 401. Make sure that mcp_key in {SETTINGS} is the key of the running app.") from None
        if e.code == 403:
            raise SetupError("MCP refused the request: HTTP 403 (bad Host or an Origin header). Use 127.0.0.1 and no proxy.") from None
        raise SetupError(f"MCP error: HTTP {e.code} for {body.get('method')}") from None
    except OSError as e:  # URLError, connection refused, timeout
        raise SetupError(f"Cannot reach MCP at {_mcp['url']}: {getattr(e, 'reason', e)}. Is Stylus open with Settings -> MCP on?") from None
    if sid:
        headers["mcp-session-id"] = sid
    if not raw.strip():
        return None
    try:
        return json.loads(raw)
    except ValueError:
        raise SetupError(f"MCP sent a reply that is not JSON for {body.get('method')}") from None


def _rpc_error(v) -> str:
    err = (v or {}).get("error") or {}
    return err.get("message", "no result")


def _connect() -> dict:
    """initialize, then notifications/initialized. Returns the headers for the next requests."""
    headers = {
        "content-type": "application/json",
        "accept": "application/json, text/event-stream",
        "authorization": f"Bearer {_mcp['key']}",
    }
    init = {"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "pause_race", "version": "1"}}
    v = _post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": init}, headers)
    if not v or "result" not in v:
        raise SetupError(f"MCP initialize failed: {_rpc_error(v)}")
    headers["mcp-protocol-version"] = PROTOCOL
    _post({"jsonrpc": "2.0", "method": "notifications/initialized"}, headers)
    return headers


def mcp_call(name: str, args: dict) -> dict:
    """Calls the tool `name`. Returns the parsed `content[0].text`. Raises ToolError on isError."""
    if _mcp["headers"] is None:
        _mcp["headers"] = _connect()
    _mcp["next_id"] += 1
    params = {"name": name, "arguments": args}
    v = _post({"jsonrpc": "2.0", "id": _mcp["next_id"], "method": "tools/call", "params": params}, _mcp["headers"])
    if not v or "result" not in v:
        raise SetupError(f"MCP {name} failed: {_rpc_error(v)}")
    result = v["result"]
    text = result["content"][0]["text"]
    if result.get("isError"):
        raise ToolError(f"{name} failed: {text}")
    return json.loads(text)


# ---- the run ----------------------------------------------------------------------------------


def _read_key(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            settings = json.load(f)
    except FileNotFoundError:
        raise SetupError(f"No settings file at {path}. Start Stylus once and turn on Settings -> MCP.") from None
    except (OSError, ValueError) as e:
        raise SetupError(f"Cannot read {path}: {type(e).__name__}") from None
    key = settings.get("mcp_key") if isinstance(settings, dict) else None
    if not isinstance(key, str) or not key:
        raise SetupError(f"No mcp_key in {path}. Turn on Settings -> MCP in Stylus.")
    return key


def _read_new_lines(path: str, offset: int) -> list[str]:
    """The log lines written after `offset`. The full file if it rolled over (got smaller)."""
    try:
        with open(path, "rb") as f:
            if os.fstat(f.fileno()).st_size >= offset:
                f.seek(offset)
            return f.read().decode("utf-8", "replace").splitlines()
    except OSError as e:
        raise SetupError(f"Cannot read the log {path}: {type(e).__name__}") from None


def _parse_args(argv=None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description="MCP repro of the Spirc pause race. Pauses and resumes the music on This Mac.")
    p.add_argument("--rounds", type=int, default=20, metavar="N", help="rounds to run (default %(default)s)")
    p.add_argument("--gap", type=float, default=0.3, metavar="S", help="seconds from the first pause to the fast resume (default %(default)s)")
    p.add_argument("--settle", type=float, default=1.5, metavar="S", help="seconds from the last pause to now_playing (default %(default)s)")
    p.add_argument("--port", type=int, default=5590, metavar="P", help="MCP port of Stylus (default %(default)s)")
    p.add_argument("--log", default=LOG, metavar="PATH", help="the app log (default %(default)s)")
    p.add_argument("--no-log-check", action="store_true", help="do not read the app log after the rounds")
    return p.parse_args(argv)


def _run(a: argparse.Namespace) -> int:
    _mcp["url"] = f"http://127.0.0.1:{a.port}/mcp"
    _mcp["key"] = _read_key(SETTINGS)
    log_offset = None
    if not a.no_log_check:
        try:
            log_offset = os.path.getsize(a.log)
        except OSError:
            raise SetupError(f"No log at {a.log}. Use --log PATH or --no-log-check.") from None

    now = mcp_call("now_playing", {})
    if (now.get("device") or {}).get("name") != "This Mac" or not now.get("track"):
        raise SetupError("Start a track on This Mac in Stylus first.")

    start = datetime.now(timezone.utc)
    failed = 0
    for i in range(1, a.rounds + 1):
        for tool, wait in (("resume", 2.0), ("pause", a.gap), ("resume", 1.0), ("pause", a.settle)):
            mcp_call(tool, {})
            time.sleep(wait)
        if round_ok(mcp_call("now_playing", {})):
            print(f"round {i:02d} ok", flush=True)
        else:
            failed += 1
            print(f"round {i:02d} FAIL: still playing {a.settle:g} s after pause", flush=True)
    end = datetime.now(timezone.utc) + timedelta(seconds=LOG_TAIL_S)
    print(f"failed {failed} of {a.rounds}", flush=True)

    pauses = invalid = 0
    if log_offset is not None:
        if start.date() != end.date():
            print("log check skipped: run crossed midnight UTC")
        else:
            time.sleep(LOG_TAIL_S)  # the window ends LOG_TAIL_S after the run: wait for those lines
            pauses, invalid = log_findings(_read_new_lines(a.log, log_offset), start, end)
            print(f"log: {pauses} pauses with no Paused event, {invalid} invalid-state plays")
    return 1 if failed or pauses or invalid else 0


def main(argv=None) -> int:
    a = _parse_args(argv)
    try:
        return _run(a)
    except (SetupError, ToolError) as e:
        print(str(e), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
