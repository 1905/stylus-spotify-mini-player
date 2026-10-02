// The in-app player ("The Run", a librespot Connect device) as the device menu shows it.

export const THE_RUN = "The Run";

/** Engine states that end by themselves: wait for them instead of acting. */
export const CONNECTING = new Set(["starting", "reconnecting"]);

/** States that a player login (engine_login) fixes. */
export const NEEDS_LOGIN = new Set(["needs_login", "account_mismatch"]);

export const isTheRun = (d) => Boolean(d) && d.name === THE_RUN;

/** Ready but still not listed after this long (menu open): the row offers a retry. */
export const THE_RUN_MISSING_MS = 20000;

/**
 * The "This Mac" row for the device menu, or null for no row.
 * engine: the last engine_status ({state, reason?}) or null while unknown.
 * busy: "login" | "connecting" while a click on the row is still working, else "".
 * missingMs: how long the menu has been open with the engine ready and "The Run" not listed.
 * The row shows whenever "The Run" isn't listed; once it is, it's a normal row.
 */
export function thisMacRow(engine, devices, busy = "", missingMs = 0) {
  if (!devices || devices.some(isTheRun)) return null;
  if (busy === "login") return { type: "Waiting for login…", title: "Finish the player login in your browser" };
  const connecting = { type: "Connecting…", title: "The player on this Mac is connecting to Spotify" };
  if (busy) return connecting;
  const st = engine && engine.state;
  if (!st || CONNECTING.has(st)) return connecting;
  if (st === "ready") {
    if (missingMs < THE_RUN_MISSING_MS) return { type: "Connecting…", title: "Waiting for Spotify to list the player on this Mac" };
    return { type: "Not showing up — retry", title: "Spotify hasn't listed the player on this Mac yet" };
  }
  if (st === "needs_login") return { type: "Log in to play here", title: "Opens your browser once to log in the player on this Mac" };
  if (st === "account_mismatch")
    return { type: "Log in to play here", title: engine.reason || "The player on this Mac uses another Spotify account" };
  return { type: "Not available right now", title: engine.reason || "The player on this Mac isn't available right now" };
}

/**
 * The device to show and play on when nothing is playing: this Mac first (The Run, then any
 * other computer), then the last-active device, then the first. Spotify keeps calling a
 * speaker "active" long after it stopped, so active alone would keep picking the network player.
 */
export function preferredDevice(list) {
  const all = (list || []).filter((d) => d && d.id && !d.is_restricted);
  return all.find(isTheRun) || all.find((d) => /computer/i.test(d.type || "")) || all.find((d) => d.is_active) || all[0] || null;
}
