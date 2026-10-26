# ads: what Stylus sees of a Free-mode ad break

**Question.** In Free mode the Spotify web player plays and Stylus watches it over Connect. What
does Stylus see of an ad break: in the front-end's mode, in the cluster state, in its log?

**Data.** `~/Library/Application Support/stylus/logs/stylus.log` on the user's Mac, one Free
account, Stylus 0.4.1 (the version the log's start lines name). The file starts on 2026-10-25
06:25:42Z; this report reads it up to 2026-10-26 07:17Z. The log is INFO only
(`src-tauri/src/applog.rs:122`). The first break was looked at live on 2026-10-25 16:02Z (two ads,
the user's account); the others are read from the log afterwards.

## How a break reaches the log

The front-end has three modes (`src/app.js:54`, decided at `src/app.js:497`): `track` (a song
plays), `other` (the device is active but no track: an ad or a podcast), `idle` (nothing). Every
change of the playing item writes one `now:` line (`src/app.js:539`):

```
16:02:10.770Z now: spotify:track:1wz4… → other from no context on "Here" (e8a937c5…), playing
16:02:11.398Z next remote on "Here" (e8a937c5…): ok
16:02:49.859Z now: nothing → spotify:track:1KU5… "Black Velvet" from no context on "Here" (e8a937c5…), playing
```

- A break is logged once, as `<track> → other`. No ad uri, title or length is logged.
- While the mode is `other`, `state.now` is empty, so the line after the break names `nothing` as
  the previous item. The break's end is that line.
- The boundary between two ads in one break is not logged: the mode stays `other`.
- The only ad-specific code in Rust is `uri.starts_with("spotify:ad:")` in
  `src-tauri/src/internal.rs:759`; it only skips the metadata lookup for an ad item.

## Breaks in the log

| # | break starts (UTC) | context | `next` logged in the break | break ends (UTC) | length |
|---|---|---|---|---|---|
| 1 | 2026-10-25 08:27:08.942 | no context | no | 08:27:55.535 | 46.6 s |
| 2 | 2026-10-25 09:27:06.904 | playlist `37i9dQZEVXcLsItByI49dW` | no | 09:28:07.424 | 60.5 s |
| 3 | 2026-10-25 10:07:16.819 | the same playlist | no | not seen: Stylus restarted at 10:07:32 | – |
| 4 | 2026-10-25 10:43:33.338 | the same playlist | no | 10:44:24.498 | 51.2 s |
| 5 | 2026-10-25 11:09:51.370 | the same playlist | no | 11:10:51.754 | 60.4 s |
| 6 | 2026-10-25 16:02:10.770 | no context | yes, +0.6 s, `ok` | 16:02:49.859 | 39.1 s |
| 7 | 2026-10-26 07:16:39.854 | no context | yes, +0.2 s, `ok` | 07:17:39.318 | 59.5 s |

Notes:

- `next remote … ok` means Spotify accepted the command. The log does not tell a `next` sent by
  the code from one the user clicked.
- Break 7 also logs `WARN play list not confirmed after 8000ms: want spotify:track:1qDr…, now null`
  at 07:16:42.550: for 8 s after the `next`, the cluster still reported no track.
- Breaks with an accepted `next` (39.1 s, 59.5 s) are in the same range as breaks without one
  (46.6–60.5 s). The log has no ad boundaries, so it cannot show whether a `next` ended one ad and
  the next ad of the same break played.

## What the code on `master` does with a break (as of 1d9afd2, 2026-10-26)

- On the change into mode `other` in Free mode: a toast and a system notification
  "Реклама — пропускаем" (`notifyAd`, `src/app.js:139`), and one `next` (`skipAd`,
  `src/app.js:2574`). Added in `a1d3281` (2026-10-25).
- `1d9afd2` adds repeated skips inside one break: the front-end remembers the uri the break is
  followed by and sends `next` again for a poll with a different uri; 1.5 s without an ad
  (`AD_SETTLE_MS`) ends the break. It logs `ad break: skipping again (uri …)`. The commit message
  states that Spotify names only the first ad (`spotify:ad:*`) and that a following ad reports the
  uri of the track after the break. This report did not check that claim: no break after
  `1d9afd2` is in the log yet.

## Open questions

- Ad uris, ad types (audio, video) and the number of ads per break: not in the log. Seeing them
  needs a logging-only change (log the cluster's `track_uri` inside mode `other`).
- Whether the web player's own UI shows the break differently from the Connect cluster: not looked at.
- One account and one Mac; the break frequency and length can differ for other accounts and regions.
