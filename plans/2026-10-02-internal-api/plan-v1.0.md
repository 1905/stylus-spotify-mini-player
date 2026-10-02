# Internal API integration — plan v1.0 (option A: webview token factory)

Status: DRAFT, awaiting approval. Spec: ./spec.md (TL;DR there).

## Architecture

```
┌──────────────────────────── src-tauri ────────────────────────────┐
│                                                                    │
│  auth.rs (existing, public OAuth)    internal/                     │
│  spotify.rs (existing, public API)   ├── session.rs   SessionStore │
│                                      ├── factory.rs   TokenFactory │
│                                      ├── client.rs    InternalApi  │
│                                      ├── hashes.rs    HashRegistry │
│                                      └── model.rs     DTOs         │
│                                                                    │
│  WebviewWindow "login" (visible, once) ──► sp_dc                   │
│  WebviewWindow "token-factory" (hidden) ──► bearer, client-token   │
└────────────────────────────────────────────────────────────────────┘
```

Public client stays untouched. Internal client is additive. UI reads metadata
from internal, player/library actions stay on public.

## Components

### 1. `session.rs` — SessionStore
- Holds: `sp_dc`, `bearer` + `expires_at`, `client_token`, `client_id`.
- Persist in macOS Keychain via `keyring` crate (fallback: file with 0600).
- In-memory cache with expiry; `get_valid_tokens()` is the only public API.

### 2. Login flow (one-time)
- No `sp_dc` in store → open visible `WebviewWindow` at open.spotify.com.
- Poll `webview.cookies()` (Tauri 2 API — **verify exact name in spike**)
  every 2 s until `sp_dc` appears → persist → close window.
- Only reappears when `sp_dc` dies (logout/password change — rare).

### 3. `factory.rs` — TokenFactory (the core of option A)
- Hidden `WebviewWindow` (persistent data store — NOT incognito, else sp_dc
  won't be shared) loading open.spotify.com.
- `initialization_script` (runs at document start) hooks `fetch` + XHR:

```js
// watch for the page's own token calls, forward to Rust
const orig = window.fetch;
window.fetch = async (...a) => {
  const res = await orig(...a);
  const url = String(a[0]?.url ?? a[0]);
  if (url.includes("open.spotify.com/api/token") || url.includes("clienttoken.spotify.com")) {
    res.clone().json().then(j =>
      window.__TAURI__.event.emit("internal-token", { url, body: j })).catch(() => {});
  }
  return res;
};
```

- Rust listens for `internal-token` events → extracts `accessToken` +
  `accessTokenExpirationTimestampMs` from `/api/token`, `granted_token.token`
  from clienttoken → updates SessionStore.
- Force client-token refetch when needed: `webview.eval("localStorage.clear()")`
  + reload (cookies stay, verified technique).
- Spotify's JS computes the TOTP — we never reimplement it. When Spotify
  changes the anti-bot scheme, the page adapts and we keep working.

### 4. `hashes.rs` — HashRegistry
- Persisted-query hashes in `hashes.toml`, NOT compiled in. Load at runtime,
  reload on `PersistedQueryNotFound`.
- Seeded from `spotify-api-parser/docs/graphql-ops.md`.
- Refresh procedure: run `tour.py` + `gen_ops.py` in spotify-api-parser,
  copy the table. 2 minutes, no app release needed if hashes.toml is read
  from a writable path.

### 5. `client.rs` — InternalApi (reqwest)
- Sends the full verified header set (web-player fingerprint):
  `authorization`, `client-token`, `app-platform: WebPlayer`,
  `spotify-app-version: 1.3.5.4.g99326ded1016`, `accept: application/json`,
  `referer: https://open.spotify.com/`, Chrome UA.
- Methods (v1): `artist_overview(uri)` → top tracks + playcount + radio URI;
  `track(uri)` → playcount; `playlist_contents(uri, offset, limit)`;
  `radio_for_artist(id)` / `radio_for_track(id)` via inspiredby-mix REST +
  fetchPlaylistContents; `search_*`; `home()`.
- Error policy: 401 → one token refresh + retry; `PersistedQueryNotFound` →
  typed error "hashes stale"; 429 → backoff, surface.
- Budget behavior: few req/s max, cache responses in-memory, use observed
  page sizes (25–50).

### 6. `model.rs`
- `playcount`: string → `u64`. Playlist items: `itemV2.data`. Radio: URI chain
  result. Keep DTOs minimal — only fields the UI uses.

## Phases & gates

| # | Phase | Gate |
|---|-------|------|
| 0 | **Spike: WKWebView cookie persistence** — tiny Tauri window, load open.spotify.com, confirm `sp_dc` readable via `cookies()` and survives app restart | **P0, blocks everything.** If cookies don't persist/read → fall back to driving Brave via Playwright-like sidecar (ugh) |
| 1 | SessionStore + login window + sp_dc capture | login once, restart app, sp_dc still there |
| 2 | TokenFactory: hidden webview + JS interceptor → bearer + client-token in SessionStore | both tokens land, expiry parsed |
| 3 | InternalApi: single call `queryArtistOverview` end-to-end | 200 with playcount, from pure Rust |
| 4 | Full v1 method set + hashes.toml | all recipes return data |
| 5 | Refresh scheduler (re-mint at ~80% lifetime + on 401) | 2 h soak, zero auth failures |
| 6 | Wire into UI (playcount on tracks, radio button, mixes) | feature demo |

Phase 0 is half a day. 1–3 is the risky core (~2 days). 4–6 are mechanical.

## Risks

- **WKWebView ≠ Brave.** Tokens minted in a Safari-ish UA webview, consumed with
  Chrome UA headers. Token is almost certainly not UA-bound (verify in phase 3).
  If it is: send WKWebView-matching UA instead.
- **WebView2 on Windows** (later): same design, different cookie API. macOS first.
- **`sp_dc` expiry**: unknown, assume months. Handle its death gracefully
  (reopen login window) — never crash.
- **ToS**: account-level risk, personal use accepted already.
- **Hash rotation**: mitigated by hashes.toml being runtime-loaded.

## Open questions for the spike

1. Exact Tauri 2 cookie API (`cookies()` on WebviewWindow? filter args?).
2. Does the hidden webview share the cookie jar with the login window
   (same data store)? If not, factory can't reuse sp_dc → must implement
   /api/token call from Rust with sp_dc + intercepted TOTP params replay
   (params are time-limited — capture-and-replay within seconds works).
3. `initialization_script` timing vs Spotify's module loading (must win).
