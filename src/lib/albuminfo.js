// The back of the current cover's sleeve: the album's details from `get_album_info`
// (Rust parse::album_info). Pure text and markup; app.js flips the cover and fills it.

import { esc } from "./format.js";

const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/** "12 March 2021" (day), "March 2021" (month), "2021" (year); "" when unknown. */
export function releaseText(date, precision) {
  const m = /^(\d{4})(?:-(\d{2}))?(?:-(\d{2}))?/.exec(String(date || ""));
  if (!m) return "";
  const [, y, mo, d] = m;
  const month = mo ? MONTHS[Number(mo) - 1] : null;
  if (precision === "year" || !month) return y;
  if (precision === "month" || !d) return `${month} ${y}`;
  return `${Number(d)} ${month} ${y}`;
}

/** "53 min", "1 hr 12 min", "45 sec" for a short single; "" when unknown. */
export function lengthText(ms) {
  const s = Math.round(Number(ms) / 1000);
  if (!Number.isFinite(s) || s <= 0) return "";
  if (s < 60) return `${s} sec`;
  const min = Math.round(s / 60);
  const h = Math.floor(min / 60);
  return h ? `${h} hr ${min % 60} min` : `${min} min`;
}

/** "12 songs" / "1 song"; "" when unknown. */
export const songsText = (n) => (Number(n) > 0 ? `${n} ${Number(n) === 1 ? "song" : "songs"}` : "");

const TYPES = { album: "Album", single: "Single", ep: "EP", compilation: "Compilation" };

/** Album / Single / EP / Compilation ("Album" for anything else). */
export const typeLabel = (type) => TYPES[String(type || "").toLowerCase()] || "Album";

/**
 * The copyright lines with their symbol: © for "C", ℗ for "P" (the sound recording). A text that
 * already starts with a symbol, or "(C)" / "(P)", keeps one symbol, not two. Repeats are dropped.
 */
export function copyrightLines(list) {
  const out = [];
  for (const c of list || []) {
    const text = String((c && c.text) || "").trim();
    if (!text) continue;
    const sym = String(c.type).toUpperCase() === "P" ? "℗" : "©";
    const lead = /^(?:\((c|p)\)|(©|℗))\s*/i.exec(text);
    const own = lead ? (lead[2] || (lead[1].toLowerCase() === "p" ? "℗" : "©")) : sym;
    const line = `${own} ${text.slice(lead ? lead[0].length : 0)}`;
    if (!out.includes(line)) out.push(line);
  }
  return out;
}

/** A record-shop catalogue number from the album id: flavour, stable per album. */
export const catalogueNo = (id) => (id ? `Cat. no. ${String(id).slice(0, 6).toUpperCase()}` : "");

/** The back of the sleeve, filled. */
export function sleeveBackHtml(info) {
  const facts = [
    ["Released", releaseText(info.release_date, info.release_precision)],
    ["Length", [songsText(info.total_tracks), lengthText(info.duration_ms)].filter(Boolean).join(" · ")],
    ["Label", info.label || ""],
  ].filter(([, v]) => v);
  const legal = copyrightLines(info.copyrights);
  return (
    `<div class="sb-head"><span class="sb-type">${esc(typeLabel(info.type))}</span><span class="sb-cat">${esc(catalogueNo(info.id))}</span></div>` +
    `<div class="sb-title"><p class="sb-name">${esc(info.name || "")}</p><p class="sb-artist">${esc(info.artists || "")}</p></div>` +
    (facts.length ? `<dl class="sb-facts">${facts.map(([k, v]) => `<div><dt>${k}</dt><dd>${esc(v)}</dd></div>`).join("")}</dl>` : "") +
    (legal.length ? `<div class="sb-legal">${legal.map((l) => `<p>${esc(l)}</p>`).join("")}</div>` : "")
  );
}

/** Loading: skeleton lines in the shape of the filled back (styles.css sets their widths). */
export const SLEEVE_LOADING =
  '<div class="sb-head"><span class="sb-line"></span></div>' +
  '<div class="sb-title"><span class="sb-line is-big"></span><span class="sb-line"></span></div>' +
  '<div class="sb-facts"><span class="sb-line"></span><span class="sb-line"></span><span class="sb-line"></span></div>';

export const SLEEVE_ERROR = `<p class="sb-error">Album details aren't available</p>`;
