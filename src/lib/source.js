// Play sources: which contexts a detail view names, and which take a start track.

const ORIGIN_KINDS = new Set(["playlist", "album"]);

/** The context uri of a detail view ({kind, id}), or null: playlists and albums only. */
export const originUri = (o) => (o && ORIGIN_KINDS.has(o.kind) && o.id ? `spotify:${o.kind}:${o.id}` : null);

/** Only playlists and albums take a start track (offset); any other context would start from the top. */
export const offsettable = (uri) => /^spotify:(playlist|album):/.test(String(uri || ""));
