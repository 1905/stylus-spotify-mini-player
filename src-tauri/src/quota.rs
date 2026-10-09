//! The app's Spotify Web API quota: a guard that stops every Web API call while Spotify has
//! rate-limited the app (429 + Retry-After), and a counter of the calls made.
//!
//! Spotify blocked the app's client id once for ~15 h (`429 QUOTA_EXCEEDED, Retry-After: 53393`)
//! after a 1 s poll. While blocked, every call fails at once with
//! `RATE_LIMITED:<seconds left>: Spotify paused this app's library access`, without a request.
//! The block's end (unix seconds) is kept in the store (`apiBlockedUntil` in state.json), so a
//! relaunch respects it too. The in-app player (librespot) doesn't use the Web API and keeps working.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::nowplaying::lock;

const LOG: &str = "stylus::quota";
/// The store key of the block's end, unix seconds.
pub const STORE_KEY: &str = "apiBlockedUntil";
/// A 429 without a usable Retry-After blocks this long.
const DEFAULT_RETRY_SECS: u64 = 30;
/// A Retry-After above this is taken as this (a broken header must not block for years).
const MAX_RETRY_SECS: u64 = 2 * 24 * 3600;
/// The counter keeps this much history.
const WINDOW: Duration = Duration::from_secs(3600);
const MINUTE: Duration = Duration::from_secs(60);
/// The summary line's period.
pub const SUMMARY_EVERY: Duration = Duration::from_secs(600);
/// More requests than this in one minute logs a warning.
const BURST_WARN: usize = 300;

// ---- the guard ---------------------------------------------------------------

/// Seconds to wait from a Retry-After header: whole seconds, clamped to 1 s–2 days.
/// Missing or not a number (Spotify sends seconds, never an HTTP date) → 30 s.
pub fn retry_after_secs(header: Option<&str>) -> u64 {
    header
        .and_then(|h| h.trim().parse::<u64>().ok())
        .map_or(DEFAULT_RETRY_SECS, |s| s.clamp(1, MAX_RETRY_SECS))
}

/// The error every Web API call returns while blocked. The UI matches the `RATE_LIMITED` prefix.
pub fn rate_limited_error(secs_left: u64) -> String {
    format!("RATE_LIMITED:{secs_left}: Spotify paused this app's library access")
}

/// What a check found.
#[derive(Debug, PartialEq)]
pub enum Check {
    Open,
    /// Seconds left.
    Blocked(u64),
    /// The block just ran out: open again (log it, clear the stored value).
    Ended,
}

/// The block: until when (unix seconds) the Web API is off limits.
#[derive(Debug, Default, PartialEq)]
pub struct Block {
    until: Option<u64>,
}

impl Block {
    /// The block from its stored value; a past or broken value is no block.
    pub fn from_stored(v: Option<&Value>, now: u64) -> Block {
        Block { until: v.and_then(Value::as_u64).filter(|u| *u > now) }
    }

    /// The value to store: the end in unix seconds, or null (removes the key).
    pub fn stored(&self) -> Value {
        self.until.map_or(Value::Null, |u| json!(u))
    }

    pub fn left(&self, now: u64) -> u64 {
        self.until.map_or(0, |u| u.saturating_sub(now))
    }

    /// A 429 asking to wait `retry_secs`. A longer block already running is kept.
    /// True when this starts a block (none was running).
    pub fn hit(&mut self, retry_secs: u64, now: u64) -> bool {
        let started = self.left(now) == 0;
        let until = now.saturating_add(retry_secs);
        self.until = Some(self.until.filter(|u| *u > now).map_or(until, |u| u.max(until)));
        started
    }

    pub fn check(&mut self, now: u64) -> Check {
        match self.until {
            Some(u) if u > now => Check::Blocked(u - now),
            Some(_) => {
                self.until = None;
                Check::Ended
            }
            None => Check::Open,
        }
    }
}

/// The live block, read from the store on first use.
static BLOCK: Mutex<Option<Block>> = Mutex::new(None);

fn with_block<T>(f: impl FnOnce(&mut Block) -> T) -> T {
    let mut guard = lock(&BLOCK);
    let block = guard.get_or_insert_with(|| {
        let b = Block::from_stored(crate::store::get(STORE_KEY).as_ref(), crate::paths::now());
        let left = b.left(crate::paths::now());
        if left > 0 {
            log::warn!(target: LOG, "Web API still blocked from the last run: {} left", human(left));
        }
        b
    });
    f(block)
}

fn persist(value: Value) {
    if let Err(e) = crate::store::store_set(STORE_KEY.to_string(), value) {
        log::warn!(target: LOG, "could not store the rate-limit block: {e}");
    }
}

/// Before every Web API request: Err(RATE_LIMITED…) while blocked, no request made.
pub fn check() -> Result<(), String> {
    match with_block(|b| b.check(crate::paths::now())) {
        Check::Open => Ok(()),
        Check::Blocked(left) => Err(rate_limited_error(left)),
        Check::Ended => {
            log::info!(target: LOG, "Web API block over: requests allowed again");
            persist(Value::Null);
            Ok(())
        }
    }
}

/// A 429 on `path`: block for Retry-After, store the end, and return the error for the caller.
pub fn on_429(retry_after: Option<&str>, path: &str) -> String {
    let secs = retry_after_secs(retry_after);
    let now = crate::paths::now();
    let (started, stored, left) = with_block(|b| {
        let started = b.hit(secs, now);
        (started, b.stored(), b.left(now))
    });
    if started {
        log::warn!(
            target: LOG,
            "Spotify rate limit (429) on {}: Web API blocked for {} (Retry-After {:?}); playback here keeps working",
            endpoint(path),
            human(secs),
            retry_after.unwrap_or("missing")
        );
    }
    persist(stored);
    rate_limited_error(left)
}

/// Seconds left on the block, 0 when open.
pub fn blocked_for() -> u64 {
    let _ = check(); // ends a block that ran out (logs it once)
    with_block(|b| b.left(crate::paths::now()))
}

/// "14.8 h", "12 min", "30 s".
fn human(secs: u64) -> String {
    if secs >= 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else if secs >= 60 {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

// ---- the counter ---------------------------------------------------------------

/// An API path as the counter groups it: no query, ids replaced by `{id}`.
/// `/playlists/37i9dQZF1DXcBWIGoYBM5M/items?limit=50` → `/playlists/{id}/items`.
pub fn endpoint(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.split('/').map(|seg| if looks_like_id(seg) { "{id}" } else { seg }).collect::<Vec<_>>().join("/")
}

/// Spotify ids are 22 base62 characters; user ids and snapshot-like tokens are long too.
/// Endpoint words (`recently-played`, `currently-playing`) have hyphens or are short.
fn looks_like_id(seg: &str) -> bool {
    seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Web API requests in a rolling hour.
#[derive(Debug, Default)]
pub struct Counter {
    hits: VecDeque<(Instant, String)>,
    /// The burst warning fired and the minute's count hasn't dropped back under the limit.
    burst_warned: bool,
}

impl Counter {
    fn prune(&mut self, now: Instant) {
        while self.hits.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > WINDOW) {
            self.hits.pop_front();
        }
    }

    /// Counts one request. Some(count) when this pushes the last minute over the burst limit
    /// (once per burst: the next warning needs the count to drop back first).
    pub fn record(&mut self, endpoint: String, now: Instant) -> Option<usize> {
        self.prune(now);
        self.hits.push_back((now, endpoint));
        let minute = self.count_since(now, MINUTE);
        if minute <= BURST_WARN {
            self.burst_warned = false;
            None
        } else if self.burst_warned {
            None
        } else {
            self.burst_warned = true;
            Some(minute)
        }
    }

    /// Requests in the last `window` (at most an hour).
    pub fn count_since(&self, now: Instant, window: Duration) -> usize {
        self.hits.iter().rev().take_while(|(t, _)| now.saturating_duration_since(*t) <= window).count()
    }

    /// The `n` busiest endpoints of the last `window`, busiest first (ties by name).
    pub fn top(&self, now: Instant, window: Duration, n: usize) -> Vec<(String, usize)> {
        let mut by: HashMap<&str, usize> = HashMap::new();
        for (_, e) in self.hits.iter().rev().take_while(|(t, _)| now.saturating_duration_since(*t) <= window) {
            *by.entry(e.as_str()).or_default() += 1;
        }
        let mut top: Vec<(String, usize)> = by.into_iter().map(|(e, c)| (e.to_string(), c)).collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(n);
        top
    }

    /// The summary line: the last `SUMMARY_EVERY`, the hour, and the top 5 endpoints.
    pub fn summary(&self, now: Instant) -> String {
        let total = self.count_since(now, SUMMARY_EVERY);
        let top = self.top(now, SUMMARY_EVERY, 5).iter().map(|(e, c)| format!("{e} {c}")).collect::<Vec<_>>().join(", ");
        let top = if top.is_empty() { String::new() } else { format!("; top: {top}") };
        format!("Web API: {total} requests in {} min, {} in the last hour{top}", SUMMARY_EVERY.as_secs() / 60, self.count_since(now, WINDOW))
    }
}

static COUNTER: Mutex<Option<Counter>> = Mutex::new(None);

fn with_counter<T>(f: impl FnOnce(&mut Counter) -> T) -> T {
    let mut guard = lock(&COUNTER);
    f(guard.get_or_insert_with(Counter::default))
}

/// Counts a request to `path` (called for every request actually sent).
pub fn record(path: &str) {
    let burst = with_counter(|c| c.record(endpoint(path), Instant::now()));
    if let Some(n) = burst {
        let top = with_counter(|c| c.top(Instant::now(), MINUTE, 3));
        log::warn!(target: LOG, "Web API burst: {n} requests in the last minute (over {BURST_WARN}); top: {top:?}");
    }
}

/// Logs the summary line every `SUMMARY_EVERY`, for the life of the app.
pub async fn summaries() {
    let mut tick = tokio::time::interval(SUMMARY_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick is immediate: nothing to say yet
    loop {
        tick.tick().await;
        let line = with_counter(|c| c.summary(Instant::now()));
        log::info!(target: LOG, "{line}");
    }
}

/// `{blockedForSecs}`: 0 when the Web API is open.
#[tauri::command]
pub fn api_status() -> Value {
    json!({ "blockedForSecs": blocked_for() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_parsing() {
        assert_eq!(retry_after_secs(Some("53393")), 53393);
        assert_eq!(retry_after_secs(Some(" 12 ")), 12);
        assert_eq!(retry_after_secs(None), 30);
        assert_eq!(retry_after_secs(Some("")), 30);
        assert_eq!(retry_after_secs(Some("soon")), 30);
        assert_eq!(retry_after_secs(Some("-5")), 30);
        assert_eq!(retry_after_secs(Some("Wed, 21 Oct 2015 07:28:00 GMT")), 30);
        assert_eq!(retry_after_secs(Some("0")), 1, "0 still waits a second");
        assert_eq!(retry_after_secs(Some("999999999")), MAX_RETRY_SECS);
    }

    #[test]
    fn error_string() {
        assert_eq!(rate_limited_error(53393), "RATE_LIMITED:53393: Spotify paused this app's library access");
    }

    #[test]
    fn blocked_until_math() {
        let mut b = Block::default();
        assert_eq!(b.check(1000), Check::Open);
        assert!(b.hit(60, 1000), "the first 429 starts a block");
        assert_eq!(b.left(1000), 60);
        assert_eq!(b.check(1030), Check::Blocked(30));
        // a shorter 429 meanwhile doesn't shorten it; a longer one extends it
        assert!(!b.hit(5, 1030));
        assert_eq!(b.left(1030), 30);
        assert!(!b.hit(100, 1030));
        assert_eq!(b.left(1030), 100);
        assert_eq!(b.check(1129), Check::Blocked(1));
        assert_eq!(b.check(1130), Check::Ended, "ends exactly at the deadline");
        assert_eq!(b.check(1131), Check::Open, "Ended is reported once");
        // a 429 after a block ran out starts a new one
        let mut old = Block { until: Some(500) };
        assert!(old.hit(10, 1000));
        assert_eq!(old.left(1000), 10);
    }

    #[test]
    fn persisted_value() {
        let mut b = Block::default();
        assert_eq!(b.stored(), Value::Null);
        b.hit(53393, 1_000_000);
        assert_eq!(b.stored(), json!(1_053_393));
        // a relaunch reads it back: still blocked, same end
        let back = Block::from_stored(Some(&b.stored()), 1_000_100);
        assert_eq!(back.left(1_000_100), 53293);
        // a block that ended while the app was closed, and garbage, are no block
        assert_eq!(Block::from_stored(Some(&json!(999)), 1000), Block::default());
        for bad in [json!(null), json!("1053393"), json!(-1), json!(1.5)] {
            assert_eq!(Block::from_stored(Some(&bad), 1000), Block::default(), "{bad}");
        }
        assert_eq!(Block::from_stored(None, 1000), Block::default());
    }

    #[test]
    fn endpoints_drop_ids_and_query() {
        assert_eq!(endpoint("/me/player"), "/me/player");
        assert_eq!(endpoint("/me/player/queue"), "/me/player/queue");
        assert_eq!(endpoint("/me/player/recently-played?limit=30"), "/me/player/recently-played");
        assert_eq!(endpoint("/playlists/37i9dQZF1DXcBWIGoYBM5M/items?limit=50&offset=0"), "/playlists/{id}/items");
        assert_eq!(endpoint("/albums/4aawyAB9vmqN3uQ7FjRGTy"), "/albums/{id}");
        assert_eq!(endpoint("/artists/3iOvXCl6edW5Um0fXEBRXy/albums?include_groups=album"), "/artists/{id}/albums");
        assert_eq!(endpoint("/me/player/play?device_id=abc"), "/me/player/play");
        assert_eq!(endpoint("/me/top/tracks?time_range=short_term"), "/me/top/tracks");
    }

    #[test]
    fn counter_rolling_window() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut c = Counter::default();
        c.record("/me/player".into(), t0);
        c.record("/me/player".into(), t0 + s(30));
        c.record("/me/player/queue".into(), t0 + s(50));
        assert_eq!(c.count_since(t0 + s(50), MINUTE), 3);
        assert_eq!(c.count_since(t0 + s(80), MINUTE), 2, "the first fell out of the minute");
        assert_eq!(c.count_since(t0 + s(100), WINDOW), 3);
        assert_eq!(c.top(t0 + s(60), WINDOW, 5), vec![("/me/player".to_string(), 2), ("/me/player/queue".to_string(), 1)]);
        // an hour later only the newest is left; recording prunes the old ones
        c.record("/search".into(), t0 + s(3640));
        assert_eq!(c.count_since(t0 + s(3640), WINDOW), 2);
        assert_eq!(c.hits.len(), 2);
        assert!(c.summary(t0 + s(3640)).starts_with("Web API: 1 requests in 10 min, 2 in the last hour; top: /search 1"), "{}", c.summary(t0 + s(3640)));
    }

    #[test]
    fn burst_warns_once_per_burst() {
        let t0 = Instant::now();
        let mut c = Counter::default();
        let mut warnings = 0;
        for i in 0..400 {
            if c.record("/me/player".into(), t0 + Duration::from_millis(i * 100)).is_some() {
                warnings += 1;
            }
        }
        assert_eq!(warnings, 1);
        // the minute empties out, then a new burst warns again
        let t1 = t0 + Duration::from_secs(200);
        assert!(c.record("/me/player".into(), t1).is_none());
        let fired = (0..400).filter(|i| c.record("/x".into(), t1 + Duration::from_millis(i * 10)).is_some()).count();
        assert_eq!(fired, 1);
    }
}
