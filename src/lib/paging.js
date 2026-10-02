// Paged search results for the full results page: Spotify serves 10 per page, up to 1000.

export const PAGE_SIZE = 10;
export const PAGE_CAP = 1000; // Spotify's offset + limit limit on /search

/** The offsets of up to n pages from `from`, inside the cap. */
export function pageOffsets(from, n) {
  const out = [];
  for (let o = from; out.length < n && o + PAGE_SIZE <= PAGE_CAP; o += PAGE_SIZE) out.push(o);
  return out;
}

/**
 * Fold settled page loads (Promise.allSettled results for `offsets`, in order) into a list:
 * pages land in order until the first failure or the last page. keyOf names an item: a repeat
 * Spotify serves on a later page is dropped. Returns {added, next, hasMore, error}.
 */
export function foldPages(items, offsets, results, keyOf) {
  const seen = new Set(items.map(keyOf));
  const added = [];
  let next = offsets.length ? offsets[0] : 0;
  let hasMore = true;
  let error = null;
  for (let i = 0; i < results.length; i++) {
    const r = results[i];
    if (r.status === "rejected") {
      error = r.reason;
      break; // a later page that landed would leave a gap: Load more retries from here
    }
    const page = r.value || {};
    for (const it of page.items || []) {
      const k = it && keyOf(it);
      if (!k || seen.has(k)) continue;
      seen.add(k);
      added.push(it);
    }
    next = offsets[i] + PAGE_SIZE;
    hasMore = Boolean(page.has_more) && next + PAGE_SIZE <= PAGE_CAP;
    if (!hasMore) break;
  }
  return { added, next, hasMore, error };
}
