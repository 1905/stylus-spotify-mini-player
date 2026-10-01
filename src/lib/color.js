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
