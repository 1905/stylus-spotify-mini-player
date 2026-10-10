// Covers from Spotify's image CDN load through the app's cover:// scheme (src-tauri/src/covers.rs),
// which keeps each image on disk. Anything else (data:, other hosts, empty) stays as it is.

const CDN_HOST = /(^|\.)(scdn\.co|spotifycdn\.com)$/;

// only the app serves cover:// (its pages load from tauri://); the browser mock (dev/) keeps CDN URLs
const IN_APP = typeof location !== "undefined" && location.protocol === "tauri:";

/** The src for a cover: a CDN https URL → cover://localhost/<encoded URL>; anything else unchanged. */
export function coverSrc(url, inApp = IN_APP) {
  if (!inApp || typeof url !== "string" || !url.startsWith("https://")) return url;
  let u;
  try {
    u = new URL(url);
  } catch {
    return url;
  }
  // the same rule as covers.rs `allowed`: no port, a CDN host
  return u.port === "" && CDN_HOST.test(u.hostname) ? `cover://localhost/${encodeURIComponent(url)}` : url;
}
