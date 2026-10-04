// Which path a player command takes: the in-app player ("Here") directly, or the Web API.

/** True when deviceId is the in-app player and it's ready (connected; maybe not active). */
export const isEngineDevice = (engine, deviceId) =>
  Boolean(engine && engine.state === "ready" && engine.device_id && deviceId && deviceId === engine.device_id);

/**
 * Send play/pause/seek/next/prev/volume to the in-app player directly? Only when deviceId (the
 * device the command was made for) is the in-app player, it's ready, and the last poll showed it
 * active: an inactive player ignores those commands, so they go through the Web API instead.
 */
export const isLocal = (engine, deviceId, activeId) => isEngineDevice(engine, deviceId) && activeId === deviceId;

/** Volume: how long a drag must pause before a send, and how long polls keep their hands off after. */
export const volumeTiming = (local) => (local ? { quietMs: 30, lagMs: 1000 } : { quietMs: 200, lagMs: 2500 });
