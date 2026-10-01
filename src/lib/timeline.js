// The run: played → now → next. Pure builder + FLIP helpers.

/**
 * Build the display list, left → right.
 * @param {{history: {track, played_at}[], now: object|null, queue: object[]}} input history newest-first
 * @returns {{key: string, role: 'past'|'now'|'next', offset: number, track: object}[]}
 */
export function buildRun({ history = [], now = null, queue = [] } = {}, { maxPast = 4, maxNext = 8 } = {}) {
  const tracks = (history || []).map((h) => h && h.track).filter(Boolean);

  // 1. drop head rows that equal now
  let i = 0;
  if (now) while (i < tracks.length && tracks[i].uri === now.uri) i++;

  // 2. collapse consecutive duplicates, 3. take maxPast, reverse
  const past = [];
  for (; i < tracks.length && past.length < maxPast; i++) {
    if (past.length && past[past.length - 1].uri === tracks[i].uri) continue;
    past.push(tracks[i]);
  }
  past.reverse();

  const next = (queue || []).filter(Boolean).slice(0, maxNext);

  const rows = [
    ...past.map((track, j) => ({ role: "past", offset: j - past.length, track })),
    ...(now ? [{ role: "now", offset: 0, track: now }] : []),
    ...next.map((track, j) => ({ role: "next", offset: j + 1, track })),
  ];

  const seen = new Map();
  return rows.map((r) => {
    const k = seen.get(r.track.uri) || 0;
    seen.set(r.track.uri, k + 1);
    return { key: `${r.track.uri}~${k}`, ...r };
  });
}

const DURATION = 600;
const EASE = "cubic-bezier(.2,.7,.2,1)";

const reducedMotion = () =>
  typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Snapshot child rects by data-key. */
export function measure(container) {
  const rects = new Map();
  if (!container) return rects;
  for (const el of container.children) {
    if (el.dataset && el.dataset.key) rects.set(el.dataset.key, el.getBoundingClientRect());
  }
  return rects;
}

/** FLIP: animate children from prevRects to their current layout. New children fade in. */
export function flip(container, prevRects) {
  if (!container || !prevRects || reducedMotion()) return;
  for (const el of container.children) {
    const key = el.dataset && el.dataset.key;
    if (!key) continue;
    const prev = prevRects.get(key);
    const cur = el.getBoundingClientRect();
    if (!prev) {
      el.animate([{ opacity: 0 }, { opacity: getComputedStyle(el).opacity }], { duration: DURATION, easing: EASE });
      continue;
    }
    const dx = prev.left - cur.left;
    const dy = prev.top - cur.top;
    const sx = cur.width ? prev.width / cur.width : 1;
    const sy = cur.height ? prev.height / cur.height : 1;
    if (!dx && !dy && sx === 1 && sy === 1) continue;
    el.animate(
      [
        { transformOrigin: "top left", transform: `translate(${dx}px, ${dy}px) scale(${sx}, ${sy})` },
        { transformOrigin: "top left", transform: "none" },
      ],
      { duration: DURATION, easing: EASE },
    );
  }
}
