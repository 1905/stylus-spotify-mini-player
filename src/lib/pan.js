// The cover row's pan: input scaling and the rubber band past either end. Pure.

/** A trackpad / wheel delta moves the row this much: 1:1 felt too quick. */
export const WHEEL_SCALE = 0.55;
/** Past an end, the row moves this fraction of the input at first… */
export const RUBBER_K = 0.3;
/** …and never more than this far past it, px. */
export const RUBBER_CAP = 110;

const stretch = (d) => RUBBER_CAP * (1 - 1 / (1 + (RUBBER_K * d) / RUBBER_CAP));
const unstretch = (r) => (RUBBER_CAP * r) / (RUBBER_K * (RUBBER_CAP - r));

/**
 * The shown offset for an input offset raw, with min ≤ max the pan limits: inside them 1:1,
 * past them a resisting stretch (slope RUBBER_K at the edge, approaching RUBBER_CAP).
 */
export function rubberBand(raw, min, max) {
  if (raw > max) return max + stretch(raw - max);
  if (raw < min) return min - stretch(min - raw);
  return raw;
}

/** The input offset that shows as shown (rubberBand's inverse); a shown offset past the cap pins to it. */
export function rubberRaw(shown, min, max) {
  const cap = RUBBER_CAP - 0.01;
  if (shown > max) return max + unstretch(Math.min(cap, shown - max));
  if (shown < min) return min - unstretch(Math.min(cap, min - shown));
  return shown;
}
