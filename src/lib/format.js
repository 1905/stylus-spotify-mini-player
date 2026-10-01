// Pure formatting helpers.

const pad = (n) => String(n).padStart(2, "0");

/** ms → "m:ss", or "h:mm:ss" when >= 1h. null/invalid → "0:00". */
export function fmtTime(ms) {
  const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

const ESC = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

/** HTML-escape a value for innerHTML. null/undefined → "". */
export function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ESC[c]);
}
