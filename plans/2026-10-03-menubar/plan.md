# Menu-bar mini player — plan v1.0

Status: implemented on feat/mcp (uncommitted), tests on the Air; tray placement and vibrancy need the real app.

## TL;DR

- What: a menu-bar icon (monochrome template glyph: a record with a stylus). Left click opens a small popover (320×140) under it: cover, title, artist, heart, prev/play/next, a thin progress bar, a volume slider. Right click opens a native menu: Play/Pause, Next, Previous, Show Stylus, Quit.
- Why: control the music without the main window (it stays hidden after Cmd+W).
- How: the main webview stays the only brain. It pushes a compact now-playing payload to Rust (`mini_push`); Rust relays it to the popover and the menu, and sets the menu-bar title. Popover and menu buttons go back to the main webview as `mini-command`, which runs the same functions as the main buttons (togglePlay, skip, setVolume, toggleSaved). Routing, pending spinners and quota guards are reused, not copied.
- Settings: "Show in menu bar" (default on), "Show song in menu bar" (default off, ~24 chars).
- Not: no separate Rust state machine for the tray, no seek in the popover, no shuffle/repeat, no device picker. Logged out: the popover shows the status line and "click the cover to open Stylus".

## Design

Rust `src-tauri/src/tray.rs`:
- Tray built in `setup` (`tray-icon` feature), `include_image!("icons/tray-template.png")`, `icon_as_template(true)`, menu on right click only.
- The popover window `mini` (`mini.html`) is built hidden at startup: no decorations, transparent, `Effect::HudWindow` vibrancy (always dark, like the app) with radius 12 (needs `macos-private-api` + `app.macOSPrivateApi`), always on top, all workspaces.
- Left click (button up): visible → hide; hidden less than 250 ms ago (the click itself blurred it) → stay hidden; else place under the icon (`anchor`: centred on the icon, 6 px gap, clamped to the monitor work area) and show + focus.
- Blur (`Focused(false)`) or Esc → hide. Rust emits `mini-visible {open}` to main: open = main polls at its visible rate and pushes a fresh payload.
- Commands: `mini_push(state)` (main → Rust), `mini_get()` (popover start), `mini_command(action, value)` (popover → main, allow-listed actions), `mini_hide()`, `tray_config(show, title)` (main settings). Initial visibility comes from `store::get("settings")` at setup.
- Menu "Quit" → `app.exit(0)` → `RunEvent::Exit` → `engine.shutdown()` (the Cmd+Q path). "Show Stylus" → show + focus main.
- Pure, tested: `anchor`, `menu_title` (truncate to 24 chars + "…"), `reopen_ok` (the blur/click debounce), `menu_labels` (Play/Pause text, enabled flags from the payload).

Frontend:
- `src/lib/mini.js` (pure, vitest): `miniPayload`, `miniChanged` (state change or a > 3 s position jump), `miniProgress`, `volumeIcon`.
- `app.js`: `syncMini()` at the end of renderNow / renderChrome / skip start-end; `mini-command` → the existing handlers; `mini-visible` → poll rate + push. Two switches in Settings.
- `src/mini.html` + `src/mini.js` + `src/mini.css`: the popover. Icons from `lib/icons.js`, Bricolage font, the app's colours; the cover blurred behind at low opacity.
- Harness: `dev/mini.html?s=<scenario>` (body identical to `src/mini.html`, vitest guards it) with `dev/mini-mock.js`; `dev/menubar.html?bar=light|dark&s=…` shows the popover under a fake menu bar with the template icon.
- `dev/tray_icon.py`: draws the template PNG (stdlib only), so the icon is reproducible.

## Tasks

1. heavy: tray.rs + lib.rs wiring + Cargo/conf/capability + icon.
2. heavy: lib/mini.js + tests; app.js wiring; settings switches (index.html + dev/index.html).
3. heavy: mini.html/js/css.
4. light: harness + screenshots 1x/2x light/dark → /tmp/qa-tray/.
5. gate: Air: vitest, cargo test, clippy.

## Real-app only

Tray placement on notch Macs and multi-monitor, vibrancy look, focus/blur behaviour (showing the popover activates the app: a visible main window comes forward too), the template icon at menu-bar size.
