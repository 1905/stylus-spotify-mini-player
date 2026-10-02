// Placeholder markup while a list loads: grey blocks shaped like the real rows and tiles.

const widths = [72, 54, 86, 63, 47, 78, 58, 68]; // title widths, %: varied so it reads as a list

/** n track-row placeholders (.row.is-skeleton): art, title and sub blocks. */
export function skeletonRows(n) {
  let html = "";
  for (let i = 0; i < n; i++) {
    const w = widths[i % widths.length];
    html +=
      `<div class="row is-skeleton" aria-hidden="true"><span class="sk sk-art"></span>` +
      `<span class="sk-text"><span class="sk sk-line" style="width: ${w}%"></span>` +
      `<span class="sk sk-line sk-sub" style="width: ${Math.round(w * 0.6)}%"></span></span></div>`;
  }
  return html;
}

/** n shelf-tile placeholders (.album.is-skeleton): cover, name and sub blocks. */
export function skeletonTiles(n) {
  let html = "";
  for (let i = 0; i < n; i++) {
    html +=
      `<span class="album is-skeleton" aria-hidden="true"><span class="art sk"></span>` +
      `<span class="sk sk-line"></span><span class="sk sk-line sk-sub"></span></span>`;
  }
  return html;
}
