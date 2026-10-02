// The in-app player ("The Run", a librespot Connect device) as the device menu shows it.

export const THE_RUN = "The Run";

/** Engine states that end by themselves: wait for them instead of acting. */
export const CONNECTING = new Set(["starting", "reconnecting"]);

/** States that a player login (engine_login) fixes. */
export const NEEDS_LOGIN = new Set(["needs_login", "account_mismatch"]);

export const isTheRun = (d) => Boolean(d) && d.name === THE_RUN;

/**
 * The "This Mac" row for the device menu, or null for no row.
 * engine: the last engine_status ({state, reason?}) or null while unknown.
 * busy: "login" | "connecting" while a click on the row is still working, else "".
 * No row once "The Run" is listed (it's a normal row then), or when the engine is ready.
 */
export function thisMacRow(engine, devices, busy = "") {
  if (!devices || devices.some(isTheRun)) return null;
  if (busy === "login") return { type: "Waiting for login…", title: "Finish the player login in your browser" };
  if (busy) return { type: "Connecting…", title: "The player on this Mac is connecting to Spotify" };
  const st = engine && engine.state;
  if (!st || st === "ready") return null;
  if (CONNECTING.has(st)) return { type: "Connecting…", title: "The player on this Mac is connecting to Spotify" };
  if (st === "needs_login") return { type: "Log in to play here", title: "Opens your browser once to log in the player on this Mac" };
  if (st === "account_mismatch")
    return { type: "Log in to play here", title: engine.reason || "The player on this Mac uses another Spotify account" };
  return { type: "Not available right now", title: engine.reason || "The player on this Mac isn't available right now" };
}
