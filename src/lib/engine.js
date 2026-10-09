// The in-app player (a librespot Connect device) as the device menu shows it. Spotify lists it as
// "This Mac"; our UI calls it "Here". It is known by the engine's device id, never by its name.

export const HERE = "Here";

/** Engine states that end by themselves: wait for them instead of acting. */
export const CONNECTING = new Set(["starting", "reconnecting"]);

/** States that a player login (engine_login) fixes. */
export const NEEDS_LOGIN = new Set(["needs_login"]);

/** The start of engine_login's error when the login works but can't be saved (Rust player.rs `LOGIN_NOT_SAVED`). */
export const LOGIN_NOT_SAVED = "logged in, but the login could not be saved";

/** Why engine_login couldn't save the login (the player is logged in for this launch), or null for another error. */
export function notSavedReason(e) {
  const s = String(e);
  if (!s.startsWith(LOGIN_NOT_SAVED)) return null;
  return s.slice(LOGIN_NOT_SAVED.length).replace(/^:\s*/, "") || "unknown error";
}

/** The engine stopped because the account has no Premium (Rust player.rs `PREMIUM_REQUIRED`). */
export const isPremiumRequired = (st) => Boolean(st && st.state === "failed" && /Premium is required/.test(st.reason || ""));

/** d is the in-app player: its id is the engine's device id (engine_status). */
export const isHere = (d, engine) => Boolean(d && d.id && engine && engine.device_id && d.id === engine.device_id);

/** A device's name in our UI: "Here" for the in-app player, else Spotify's name. */
export const deviceLabel = (d, engine) => (!d ? "" : isHere(d, engine) ? HERE : d.name || "");

/** Ready but still not listed after this long (menu open): the row offers a retry. */
export const THE_RUN_MISSING_MS = 20000;

/**
 * The "This Mac" row for the device menu, or null for no row.
 * engine: the last engine_status ({state, reason?}) or null while unknown.
 * busy: "login" | "connecting" while a click on the row is still working, else "".
 * missingMs: how long the menu has been open with the engine ready and the player not listed.
 * The row shows whenever the player isn't listed; once it is, it's a normal row.
 */
export function thisMacRow(engine, devices, busy = "", missingMs = 0) {
  if (!devices || devices.some((d) => isHere(d, engine))) return null;
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
  return { type: "Not available right now", title: engine.reason || "The player on this Mac isn't available right now" };
}

/**
 * The device to show and play on when nothing is playing: this Mac first (the in-app player, then
 * any other computer), then the last-active device, then the first. Spotify keeps calling a
 * speaker "active" long after it stopped, so active alone would keep picking the network player.
 */
export function preferredDevice(list, engine = null) {
  const all = (list || []).filter((d) => d && d.id && !d.is_restricted);
  return all.find((d) => isHere(d, engine)) || all.find((d) => /computer/i.test(d.type || "")) || all.find((d) => d.is_active) || all[0] || null;
}
