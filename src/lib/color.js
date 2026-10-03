// Cover-art colours: pure picker + DOM extractor.

export const FALLBACK = { vivid: [139, 124, 240], ink: [21, 19, 27] };

const MIN_SAT = 0.15;
const MIN_L = 0.2;
const MAX_L = 0.8;
const INK_L = 0.08;
const INK_MAX_S = 0.4;

/** [r,g,b] 0..255 → [h 0..1, s 0..1, l 0..1] */
function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return [0, 0, l];
  const s = d / (1 - Math.abs(2 * l - 1));
  let h;
  if (max === r) h = ((g - b) / d) % 6;
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  h /= 6;
  if (h < 0) h += 1;
  return [h, s, l];
}

/** [h,s,l] 0..1 → [r,g,b] 0..255 ints */
function hslToRgb(h, s, l) {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = h * 6;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  const [r1, g1, b1] =
    hp < 1 ? [c, x, 0] : hp < 2 ? [x, c, 0] : hp < 3 ? [0, c, x] :
    hp < 4 ? [0, x, c] : hp < 5 ? [x, 0, c] : [c, 0, x];
  const m = l - c / 2;
  return [r1, g1, b1].map((v) => Math.round((v + m) * 255));
}

/**
 * rgba pixel data → {vivid, ink}.
 * vivid = most saturated pixel with HSL lightness in [0.2, 0.8]; FALLBACK when max saturation < 0.15.
 * ink = vivid's hue at lightness 8%, saturation min(s, 40%).
 */
export function pickColors(rgba) {
  let best = null;
  let bestS = -1;
  for (let i = 0; i + 3 < rgba.length; i += 4) {
    if (rgba[i + 3] < 128) continue;
    const r = rgba[i], g = rgba[i + 1], b = rgba[i + 2];
    const [h, s, l] = rgbToHsl(r, g, b);
    if (l < MIN_L || l > MAX_L) continue;
    if (s > bestS) { bestS = s; best = { rgb: [r, g, b], h, s }; }
  }
  if (!best || bestS < MIN_SAT) return { vivid: [...FALLBACK.vivid], ink: [...FALLBACK.ink] };
  return { vivid: best.rgb, ink: hslToRgb(best.h, Math.min(best.s, INK_MAX_S), INK_L) };
}

/** Load a cover into a 16×16 canvas and pick colours. Resolves null on any failure (CORS, 404). */
export function extractColors(url) {
  return new Promise((resolve) => {
    if (!url) return resolve(null);
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => {
      try {
        const c = document.createElement("canvas");
        c.width = c.height = 16;
        const ctx = c.getContext("2d", { willReadFrequently: true });
        ctx.drawImage(img, 0, 0, 16, 16);
        resolve(pickColors(ctx.getImageData(0, 0, 16, 16).data));
      } catch {
        resolve(null);
      }
    };
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

// ---------- OKLab (Björn Ottosson) + WCAG contrast: the math under the v2 glow (glow.js) ----------

/** sRGB channel 0..255 → linear 0..1, as a lookup table */
const LIN = Float64Array.from({ length: 256 }, (_, i) => {
  const c = i / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
});

const toByte = (c) => {
  const v = c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055;
  return Math.round(Math.min(1, Math.max(0, v)) * 255);
};

/** [r,g,b] 0..255 → [L 0..1, a, b] */
export function srgbToOklab([r, g, b]) {
  const lr = LIN[r], lg = LIN[g], lb = LIN[b];
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** OKLab → linear sRGB, unclamped (out of gamut when a channel leaves 0..1) */
function oklabToLinear([L, a, b]) {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

const inGamut = (lin) => lin.every((c) => c >= -1e-4 && c <= 1 + 1e-4);

/** [L, C, h radians] → [r,g,b] 0..255; chroma is cut (hue and lightness kept) until the colour fits sRGB */
export function oklchToSrgb([L, C, h]) {
  const at = (c) => oklabToLinear([L, c * Math.cos(h), c * Math.sin(h)]);
  let lin = at(C);
  if (!inGamut(lin)) {
    let lo = 0, hi = C;
    for (let i = 0; i < 14; i++) {
      const mid = (lo + hi) / 2;
      if (inGamut(at(mid))) lo = mid;
      else hi = mid;
    }
    lin = at(lo);
  }
  return lin.map(toByte);
}

/** [L, a, b] → [L, C, h radians] */
export const toLch = ([L, a, b]) => [L, Math.hypot(a, b), Math.atan2(b, a)];

/** WCAG relative luminance of [r,g,b] 0..255 */
export const luminance = ([r, g, b]) => 0.2126 * LIN[r] + 0.7152 * LIN[g] + 0.0722 * LIN[b];

/** WCAG contrast ratio, 1..21 */
export function contrast(x, y) {
  const a = luminance(x), b = luminance(y);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}
