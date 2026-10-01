// The run: played → now → next. Pure builder + FLIP helpers.

/**
 * Build the display list, left → right.
 * @param {{history: {track, played_at}[], now: object|null, queue: object[]}} input history newest-first
 * @returns {{key: string, role: 'past'|'now'|'next', offset: number, track: object}[]}
 */
export function buildRun({ history = [], now = null, queue = [] } = {}, { maxPast = 4, maxNext = 8 } = {}) {
  const tracks = (history || []).map((h) => h && h.track).filter(Boolean);

  const next = (queue || []).filter(Boolean).slice(0, maxNext);

  // 1. skip rows of the current track (it is on screen as now), 2. collapse consecutive
  //    duplicates, 3. take maxPast, reverse. Queued repeats keep their past plays.
  const past = [];
  for (const t of tracks) {
    if (past.length >= maxPast) break;
    if (now && t.uri === now.uri) continue;
    if (past.length && past[past.length - 1].uri === t.uri) continue;
    past.push(t);
  }
  past.reverse();

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

/**
 * recently-played (api) + plays the app saw itself (session), newest first.
 * A session play is the same play as an api row with the same uri within windowMs.
 * Matching is one-to-one, closest times first, so a replay stays until the API reports it.
 */
export function mergeHistory(api, session, windowMs) {
  const rows = (api || []).filter((r) => r && r.track);
  const time = (r) => Date.parse(r.played_at);
  const pairs = [];
  (session || []).forEach((s, i) =>
    rows.forEach((r, j) => {
      const d = Math.abs(time(r) - time(s));
      if (r.track.uri === s.track.uri && d < windowMs) pairs.push({ i, j, d });
    }),
  );
  const usedS = new Set();
  const usedA = new Set();
  for (const { i, j } of pairs.sort((a, b) => a.d - b.d)) {
    if (usedS.has(i) || usedA.has(j)) continue;
    usedS.add(i);
    usedA.add(j);
  }
  const extra = (session || []).filter((_, i) => !usedS.has(i));
  return [...rows, ...extra].sort((a, b) => time(b) - time(a));
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
