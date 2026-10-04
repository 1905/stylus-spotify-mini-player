// Glow v2: a cover → an accent-weighted palette with regions → 1–3 soft blobs, a wash and a base tint.
//
// 1. sample: the cover drawn at 32×32, each pixel in OKLab with its position
// 2. cluster: k-means (k = 8, deterministic farthest-point seeds), near twins merged
// 3. score: population^0.7 × chroma × lightness, so a vivid patch (a blue sky) beats a big white wall;
//    greys, near-white and near-black barely count. Under 2.5 % chromatic pixels = monochrome: a neutral glow
// 4. place: each picked colour's blob sits where that colour sits on the cover (centroid, pushed outward)
// 5. tone-map for a dark UI: blob lightness clamped, chroma kept (gamut-cut only), alphas lowered
//    until --fg keeps 4.5:1 over the brightest point and 7:1 over the title/artist band

import { srgbToOklab, oklchToSrgb, toLch, contrast } from "./color.js";

export const SIZE = 32;
const K = 8;
const ITERS = 12;
const AB = 2; // a/b differences count double against lightness: hue matters more than shade here
const MERGE = 0.004; // squared distance under which two clusters are one colour
const NEUTRAL_C = 0.035; // OKLab chroma under this is a grey (white, black, beige paper)
const MONO_SHARE = 0.025; // less chromatic coverage than this: a monochrome cover
const TWIN = 0.008; // tone-mapped blob colours closer than this are one blob
const HUE_NEAR = (40 * Math.PI) / 180; // a hue this close to a picked one…
const NEAR_PENALTY = 0.45; // …counts this much of its score
const MIN_SHARE = 0.012; // a blob colour covers at least ~12 of 1024 pixels
const SPREAD_X = 2.8; // centroid offset → blob offset, in cover sizes: wider sideways, the stage is landscape
const SPREAD_Y = 2.2;
const GAP = 0.9; // blobs closer than this (cover sizes) blend into one colour: the later one moves away
const MAX_BLOBS = 3;
export const FG = [246, 244, 248]; // --fg
export const MIN_CONTRAST = 4.5; // --fg over the brightest glow point
export const TEXT_CONTRAST = 7; // --fg over the title/artist band: leaves --fg-2 (62 %) ≥ 4.5
export const BLOB_L = [0.58, 0.72]; // blob lightness range after tone-mapping
export const INK_L = 0.19;

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const triplet = (rgb) => rgb.join(" ");

/** rgba → [{lab, x, y}] with x, y the pixel centre in 0..1; transparent pixels skipped */
export function samplePixels(rgba, w, h) {
  const pts = [];
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      if (rgba[i + 3] < 128) continue;
      pts.push({ lab: srgbToOklab([rgba[i], rgba[i + 1], rgba[i + 2]]), x: (x + 0.5) / w, y: (y + 0.5) / h });
    }
  }
  return pts;
}

const dist = (p, q) => {
  const dL = p[0] - q[0], da = (p[1] - q[1]) * AB, db = (p[2] - q[2]) * AB;
  return dL * dL + da * da + db * db;
};

/** 3×3 cell of a 0..1 position: 0 top-left … 8 bottom-right */
export const cellOf = (x, y) => Math.min(2, Math.floor(y * 3)) * 3 + Math.min(2, Math.floor(x * 3));

/**
 * Deterministic k-means over sampled pixels.
 * Seeds: the mean, then each next seed is the pixel farthest from every seed so far,
 * so a small vivid patch gets its own cluster instead of being averaged into the wall.
 * → [{lab, n, x, y, cells: 9 counts}], largest first.
 */
export function kmeans(pts, k = K, iters = ITERS) {
  if (!pts.length) return [];
  const mean = [0, 0, 0];
  for (const p of pts) for (let j = 0; j < 3; j++) mean[j] += p.lab[j] / pts.length;
  const centers = [mean];
  const near = pts.map((p) => dist(p.lab, mean));
  while (centers.length < k) {
    let bi = -1, bd = 1e-6;
    for (let i = 0; i < pts.length; i++) if (near[i] > bd) { bd = near[i]; bi = i; }
    if (bi < 0) break; // every pixel already sits on a seed
    const c = [...pts[bi].lab];
    centers.push(c);
    for (let i = 0; i < pts.length; i++) near[i] = Math.min(near[i], dist(pts[i].lab, c));
  }

  const assign = new Int32Array(pts.length).fill(-1);
  for (let it = 0; it < iters; it++) {
    let moved = 0;
    for (let i = 0; i < pts.length; i++) {
      let bj = 0, bd = Infinity;
      for (let j = 0; j < centers.length; j++) {
        const d = dist(pts[i].lab, centers[j]);
        if (d < bd) { bd = d; bj = j; }
      }
      if (assign[i] !== bj) { assign[i] = bj; moved++; }
    }
    if (!moved) break;
    const sums = centers.map(() => [0, 0, 0, 0]);
    for (let i = 0; i < pts.length; i++) {
      const s = sums[assign[i]];
      s[0] += pts[i].lab[0]; s[1] += pts[i].lab[1]; s[2] += pts[i].lab[2]; s[3]++;
    }
    sums.forEach((s, j) => { if (s[3]) centers[j] = [s[0] / s[3], s[1] / s[3], s[2] / s[3]]; });
  }

  const out = centers.map((lab) => ({ lab, n: 0, x: 0, y: 0, cells: new Array(9).fill(0) }));
  pts.forEach((p, i) => {
    const c = out[assign[i]];
    c.n++; c.x += p.x; c.y += p.y; c.cells[cellOf(p.x, p.y)]++;
  });
  return merge(out.filter((c) => c.n).map((c) => ({ ...c, x: c.x / c.n, y: c.y / c.n })));
}

/** fold clusters closer than MERGE into the bigger one (population-weighted), largest first */
function merge(cl) {
  cl.sort((a, b) => b.n - a.n);
  const out = [];
  for (const c of cl) {
    const into = out.find((o) => dist(o.lab, c.lab) < MERGE);
    if (!into) { out.push({ ...c, cells: [...c.cells] }); continue; }
    const n = into.n + c.n;
    into.lab = into.lab.map((v, j) => (v * into.n + c.lab[j] * c.n) / n);
    into.x = (into.x * into.n + c.x * c.n) / n;
    into.y = (into.y * into.n + c.y * c.n) / n;
    into.cells = into.cells.map((v, j) => v + c.cells[j]);
    into.n = n;
  }
  return out;
}

/** how much a colour is worth as glow: 0 for greys/black, up to ~1 for a vivid colour filling the cover */
export function accentScore(share, L, C) {
  const vivid = C < NEUTRAL_C ? 0.03 : Math.min(C, 0.2) / 0.2;
  const light = L < 0.22 ? 0.08 : L < 0.4 ? 0.08 + (0.92 * (L - 0.22)) / 0.18 : 1;
  return share ** 0.7 * vivid * light;
}

const isChromatic = (c) => c.C >= NEUTRAL_C && c.L >= 0.22;

/**
 * rgba w×h → { colors (best accent first), regions (colour index per 3×3 cell), mono }.
 * colors: {rgb, L, C, h, share, x, y, score}; rgb is the cluster's own colour, untouched.
 */
export function palette(rgba, w, h) {
  const pts = samplePixels(rgba, w, h);
  const total = pts.length || 1;
  const colors = kmeans(pts).map((c) => {
    const [L, C, hue] = toLch(c.lab);
    const share = c.n / total;
    return {
      rgb: oklchToSrgb([L, C, hue]), L, C, h: hue, share, x: c.x, y: c.y, cells: c.cells,
      score: accentScore(share, L, C),
    };
  });
  colors.sort((a, b) => b.score - a.score || b.share - a.share);
  const chromatic = colors.filter(isChromatic).reduce((s, c) => s + c.share, 0);
  // per cell: the colour that dominates it, accents weighted the same way as the global score
  const regions = Array.from({ length: 9 }, (_, cell) => {
    let bi = -1, bv = 0;
    colors.forEach((c, i) => {
      const v = c.cells[cell] * (isChromatic(c) ? Math.min(c.C, 0.2) / 0.2 : 0.03) + c.cells[cell] * 1e-6;
      if (v > bv) { bv = v; bi = i; }
    });
    return bi;
  });
  return { colors, regions, mono: chromatic < MONO_SHARE };
}

/**
 * blob colour for a dark stage: lightness into BLOB_L, chroma kept (cut only to fit sRGB).
 * A pastel pulled down to BLOB_L gets +30 % chroma, or a thin glow of it on black reads grey;
 * a dark colour lifted to BLOB_L keeps its chroma, or a brown turns into a loud orange.
 */
export function toneBlob({ L, C, h }) {
  return oklchToSrgb([clamp(L, BLOB_L[0], BLOB_L[1]), Math.min(C * (L > BLOB_L[1] ? 1.3 : 1), 0.26), h]);
}

/** base tint: the accent's hue, very dark, low chroma */
export const toneInk = ({ C, h }) => oklchToSrgb([INK_L, Math.min(C * 0.35, 0.045), h]);

const over = (under, top, a) => under.map((v, j) => Math.round(v * (1 - a) + top[j] * a));

/** a blob's alpha at d radii from its centre: 1 − smoothstep, as the CSS stops (0, 25, 50, 75, 100 %) draw it */
export const falloff = (d) => (d >= 1 ? 0 : 1 - d * d * (3 - 2 * d));

/** the stage colour at (x, y) in cover units: ink, the wash (taken at full strength), then each blob */
export function compositeAt(glow, x, y) {
  let c = over(glow.ink, glow.wash.rgb, glow.wash.a);
  for (const b of glow.blobs) {
    const d = Math.hypot(x - b.x, (y - b.y) / 0.85) / b.r;
    if (d < 1) c = over(c, b.rgb, b.a * falloff(d));
  }
  return c;
}

/** where the title and artist lines sit, in cover units from the cover's centre (both layouts, 700 px and up) */
export const TEXT_BAND = [];
for (let x = -1.5; x <= 2.5; x += 0.25) for (const y of [0.6, 0.85, 1.1]) TEXT_BAND.push([x, y]);

/** the darkest-for-text composite among spots: {rgb, contrast} of FG over it */
export function worst(glow, spots) {
  let rgb = null, k = Infinity;
  for (const [x, y] of spots) {
    const c = compositeAt(glow, x, y);
    const v = contrast(FG, c);
    if (v < k) { k = v; rgb = c; }
  }
  return { rgb, contrast: k };
}

/** the brightest spot of the glow: every blob centre and the cover's centre */
export const brightest = (glow) => worst(glow, [[0, 0], ...glow.blobs.map((b) => [b.x, b.y])]);

/** the text band's brightest spot */
export const textSpot = (glow) => worst(glow, TEXT_BAND);

/**
 * lower every alpha by 8 % steps until FG keeps MIN_CONTRAST (AA) over the brightest point
 * and TEXT_CONTRAST (AAA) wherever the title and artist lines sit
 */
export function fitContrast(glow) {
  for (let i = 0; i < 40 && (brightest(glow).contrast < MIN_CONTRAST || textSpot(glow).contrast < TEXT_CONTRAST); i++) {
    glow.wash.a *= 0.92;
    for (const b of glow.blobs) b.a *= 0.92;
  }
  glow.contrast = Math.min(brightest(glow).contrast, textSpot(glow).contrast);
  glow.textContrast = textSpot(glow).contrast;
  return glow;
}

/** place blobs: centroid offset × SPREAD_X/Y; one that lands on an earlier blob moves away from it */
function place(c, placed) {
  let x = (c.x - 0.5) * SPREAD_X, y = (c.y - 0.5) * SPREAD_Y;
  for (const p of placed) {
    const dx = x - p.x, dy = y - p.y, d = Math.hypot(dx, dy);
    if (d >= GAP) continue;
    // straight away from the earlier blob; dead centre → toward the cover's far side
    const ux = d > 1e-3 ? dx / d : p.x <= 0 ? 1 : -1, uy = d > 1e-3 ? dy / d : 0.35;
    x = p.x + ux * GAP; y = p.y + uy * GAP;
  }
  return { x, y };
}

/** hue distance in radians, 0..π */
const hueGap = (a, b) => {
  const d = Math.abs(a - b) % (2 * Math.PI);
  return d > Math.PI ? 2 * Math.PI - d : d;
};

/**
 * Up to MAX_BLOBS accents, greedy: each round takes the best score, where a colour within HUE_NEAR of
 * one already taken counts NEAR_PENALTY of its score (an orange next to an orange loses to the teal);
 * tone-mapped twins (two reds, two lavenders) are skipped; nothing under 28 % of the best score.
 */
export function pickAccents(cands) {
  const picks = [];
  if (!cands.length) return picks;
  const floor = cands[0].score * 0.28;
  const left = cands.filter((c) => c.score >= floor);
  while (picks.length < MAX_BLOBS) {
    let best = null, bv = 0;
    for (const c of left) {
      if (picks.includes(c)) continue;
      const lab = srgbToOklab(toneBlob(c));
      if (picks.some((p) => dist(srgbToOklab(toneBlob(p)), lab) < TWIN)) continue;
      const v = c.score * (picks.some((p) => hueGap(p.h, c.h) < HUE_NEAR) ? NEAR_PENALTY : 1);
      if (v > bv) { bv = v; best = c; }
    }
    if (!best || bv < floor) break;
    picks.push(best);
  }
  return picks;
}

/** palette → glow: {ink, vivid, wash {rgb,a}, blobs [{rgb,a,x,y,r}], mono, contrast} */
export function composeGlow(pal) {
  const cands = pal.colors.filter((c) => isChromatic(c) && c.share >= MIN_SHARE);
  if (pal.mono || !cands.length) return composeMono(pal);
  const top = cands[0];
  const picks = pickAccents(cands);
  const blobs = [];
  picks.forEach((c, i) => {
    c.picked = true; // dev/glow.html marks these swatches
    const { x, y } = place(c, blobs);
    blobs.push({
      rgb: toneBlob(c),
      a: Math.max(0.3, 0.62 * Math.sqrt(c.score / top.score)),
      x, y,
      r: clamp(1.6 + c.share * 2.5, 1.6, 2.6) * (i ? 0.9 : 1),
    });
  });
  // one colour only: its light also spills to the far side, fainter, so the stage isn't lopsided
  if (blobs.length === 1) {
    const b = blobs[0];
    const { x, y } = place({ x: 1 - (b.x / SPREAD_X + 0.5), y: 0.62 }, blobs);
    blobs.push({ ...b, a: b.a * 0.55, x, y, r: b.r * 0.9 });
  }
  const glow = {
    ink: toneInk(top),
    vivid: oklchToSrgb([clamp(top.L, 0.58, 0.75), top.C, top.h]),
    wash: { rgb: toneBlob(top), a: 0.16 },
    blobs,
    mono: false,
  };
  fitContrast(glow);
  return glow;
}

/** monochrome cover: a cool-neutral glow from where its light part sits; the faint cast of the cover kept */
function composeMono(pal) {
  const avg = pal.colors.reduce((s, c) => [s[0] + c.C * Math.cos(c.h) * c.share, s[1] + c.C * Math.sin(c.h) * c.share], [0, 0]);
  const h = Math.atan2(avg[1], avg[0]);
  const C = Math.min(Math.hypot(avg[0], avg[1]), 0.025); // a muted forest stays faintly green; mixed greys cancel out
  const light = [...pal.colors].sort((a, b) => b.L * b.share ** 0.5 - a.L * a.share ** 0.5)[0] || { x: 0.5, y: 0.5 };
  const grey = oklchToSrgb([0.68, C, h]);
  const glow = {
    ink: oklchToSrgb([0.18, Math.min(C, 0.012), h]),
    vivid: oklchToSrgb([0.72, C, h]),
    wash: { rgb: grey, a: 0.08 },
    blobs: [{ rgb: grey, a: 0.24, ...place(light, []), r: 2 }],
    mono: true,
  };
  fitContrast(glow);
  return glow;
}

/** glow → the CSS custom properties .bg reads (triplets, alphas, offsets and radii in cover sizes) */
export function glowVars(glow) {
  const v = {
    "--i": triplet(glow.ink),
    "--v": triplet(glow.vivid),
    "--w": triplet(glow.wash.rgb),
    "--wa": glow.wash.a.toFixed(3),
  };
  for (let i = 0; i < MAX_BLOBS; i++) {
    const b = glow.blobs[i], n = `--g${i + 1}`;
    v[n] = b ? triplet(b.rgb) : triplet(glow.vivid);
    v[`${n}a`] = b ? b.a.toFixed(3) : "0";
    v[`${n}x`] = b ? b.x.toFixed(3) : "0";
    v[`${n}y`] = b ? b.y.toFixed(3) : "0";
    v[`${n}r`] = b ? b.r.toFixed(3) : "1";
  }
  return v;
}

/** The colours before any cover (and when one can't be read). */
export const FALLBACK = { vivid: [139, 124, 240], ink: [21, 19, 27] };

/** the look before any cover: FALLBACK colours, one wash and no blobs */
export const fallbackVars = ({ vivid, ink }) => glowVars({ ink, vivid, wash: { rgb: vivid, a: 0.22 }, blobs: [] });

// ---------- DOM: load, sample, cache ----------

const cache = new Map(); // url → Promise<glow|null>, most recent last
const CACHE_MAX = 120;

async function loadImage(url) {
  const img = new Image();
  img.crossOrigin = "anonymous";
  img.decoding = "async";
  img.src = url;
  await img.decode();
  return img;
}

/** raw rgba of an image drawn at SIZE×SIZE (throws on a tainted canvas) */
function drawSample(img) {
  const c = typeof OffscreenCanvas === "function" ? new OffscreenCanvas(SIZE, SIZE) : Object.assign(document.createElement("canvas"), { width: SIZE, height: SIZE });
  const ctx = c.getContext("2d", { willReadFrequently: true });
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(img, 0, 0, SIZE, SIZE);
  return ctx.getImageData(0, 0, SIZE, SIZE).data;
}

/** cover url → glow (cached per url); resolves null on failure (CORS, 404). glow.ms = draw + analyse time on the main thread */
export function extractGlow(url) {
  if (!url) return Promise.resolve(null);
  const hit = cache.get(url);
  if (hit) {
    cache.delete(url);
    cache.set(url, hit);
    return hit;
  }
  const job = (async () => {
    try {
      const img = await loadImage(url);
      const t0 = performance.now();
      const glow = composeGlow(palette(drawSample(img), SIZE, SIZE));
      glow.ms = performance.now() - t0;
      return glow;
    } catch {
      cache.delete(url); // a failed load may work next time
      return null;
    }
  })();
  cache.set(url, job);
  if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value);
  return job;
}

/** for dev/glow.html: palette + glow + main-thread timings for one url, uncached */
export async function inspectCover(url) {
  const img = await loadImage(url);
  const t0 = performance.now();
  const data = drawSample(img);
  const t1 = performance.now();
  const pal = palette(data, SIZE, SIZE);
  const glow = composeGlow(pal);
  const t2 = performance.now();
  return { data, pal, glow, drawMs: t1 - t0, analyseMs: t2 - t1 };
}
