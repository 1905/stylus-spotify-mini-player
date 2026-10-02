//! The in-app player's playback session, owned by Rust: what plays on "This Mac" (the
//! source list or context, the track, the position, shuffle/repeat, the volume), kept in
//! `<app dir>/session.json` (0600, atomic writes) for the Spotify account it belongs to.
//! The engine feeds it from `local_load`, librespot's player events and Connect cluster
//! updates; at launch it loads the saved session back, paused. The UI only displays it.
//! A file that doesn't parse is moved aside to `session.json.bad` and the session starts empty.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const LOG: &str = "needle::session";
/// While playing, the position is written this often.
const SAVE_EVERY: Duration = Duration::from_secs(10);
/// A volume change is written once the volume stayed put this long.
const VOLUME_QUIET: Duration = Duration::from_secs(1);
/// librespot's own default volume (50 %), for an account with no session yet.
pub const DEFAULT_VOLUME: u16 = u16::MAX / 2;

// ---- the file --------------------------------------------------------------

/// What was loaded: a context (playlist, album…) or a plain track list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Context { context_uri: String },
    Uris { uris: Vec<String> },
}

impl Source {
    fn is_empty(&self) -> bool {
        match self {
            Source::Context { context_uri } => context_uri.is_empty(),
            Source::Uris { uris } => uris.is_empty(),
        }
    }

    fn kind(&self) -> String {
        match self {
            Source::Context { context_uri } => format!("context {context_uri}"),
            Source::Uris { uris } => format!("{} tracks", uris.len()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn from_flags(context: bool, track: bool) -> Repeat {
        if track {
            Repeat::Track
        } else if context {
            Repeat::Context
        } else {
            Repeat::Off
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::Context => "context",
            Repeat::Track => "track",
        }
    }
}

/// `session.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub account: String,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub track_uri: Option<String>,
    #[serde(default)]
    pub position_ms: u32,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: Repeat,
    #[serde(default = "default_volume")]
    pub volume: u16,
    /// Unix time, ms.
    #[serde(default)]
    pub saved_at: u64,
}

fn default_volume() -> u16 {
    DEFAULT_VOLUME
}

impl Saved {
    /// The `session_get` answer and the `session-restored` payload.
    pub fn payload(&self) -> Value {
        let (context_uri, uris) = match &self.source {
            Some(Source::Context { context_uri }) => (Some(context_uri.clone()), None),
            Some(Source::Uris { uris }) => (None, Some(uris.clone())),
            None => (None, None),
        };
        json!({
            "contextUri": context_uri,
            "uris": uris,
            "trackUri": self.track_uri,
            "positionMs": self.position_ms,
            "shuffle": self.shuffle,
            "repeat": self.repeat.as_str(),
            "volume": self.volume,
        })
    }
}

/// The session in `json`; None when it doesn't parse or has no account.
fn parse(json: &str) -> Option<Saved> {
    let mut s: Saved = serde_json::from_str(json).ok()?;
    if s.account.is_empty() {
        return None;
    }
    s.source = s.source.filter(|src| !src.is_empty());
    Some(s)
}

/// The file at `path`. Missing → None. Broken → moved aside to `session.json.bad`, None.
fn read(path: &Path) -> Option<Saved> {
    let text = std::fs::read_to_string(path).ok()?;
    let saved = parse(&text);
    if saved.is_none() {
        log::warn!(target: LOG, "session.json doesn't parse: moved to session.json.bad");
        let _ = std::fs::rename(path, path.with_extension("json.bad"));
    }
    saved
}

/// The saved session of `account` (case-insensitive); another account's session counts as none.
fn read_for(path: &Path, account: &str) -> Option<Saved> {
    let saved = read(path)?;
    if saved.account.eq_ignore_ascii_case(account) {
        Some(saved)
    } else {
        log::info!(target: LOG, "the saved session belongs to another account: ignored");
        None
    }
}

fn write(path: &Path, saved: &Saved) {
    let result = serde_json::to_string(saved)
        .map_err(|e| e.to_string())
        .and_then(|json| crate::auth::write_private(path, &json).map_err(|e| e.to_string()));
    if let Err(e) = result {
        log::warn!(target: LOG, "could not save session.json: {e}");
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- pure rules ------------------------------------------------------------

/// The source after `track` starts. A context is kept: its tracks aren't known here (a
/// context loaded elsewhere arrives through the cluster update). A list is kept while the
/// track is in it; any other track (a load by another Spotify client) becomes a one-track list.
pub fn source_after_track(known: Option<&Source>, track: &str) -> Source {
    match known {
        Some(Source::Context { context_uri }) => Source::Context { context_uri: context_uri.clone() },
        Some(Source::Uris { uris }) if uris.iter().any(|u| u == track) => Source::Uris { uris: uris.clone() },
        _ => Source::Uris { uris: vec![track.to_string()] },
    }
}

/// A cluster context that names something loadable. Track-list loads show up as
/// `spotify:web-api` (librespot) and search/local files can't be loaded back.
pub fn loadable_context(uri: &str) -> Option<&str> {
    let bad = uri.is_empty()
        || !uri.starts_with("spotify:")
        || uri == "spotify:web-api"
        || uri.starts_with("spotify:search")
        || uri.starts_with("spotify:local-files")
        || uri.starts_with("spotify:track:")
        || uri.starts_with("spotify:episode:");
    (!bad).then_some(uri)
}

/// When the next write is due.
#[derive(Debug, Clone, Copy)]
pub struct WriteClock {
    /// Something changed that is written at once (track, pause, seek, modes, source).
    pub dirty: bool,
    /// The last volume change not yet written.
    pub volume_changed_at: Option<Instant>,
    pub last_write: Instant,
    pub playing: bool,
}

/// Immediate changes now; a volume change after `VOLUME_QUIET` of quiet (each change
/// restarts the wait); while playing, the position every `SAVE_EVERY`.
pub fn write_due(c: &WriteClock, now: Instant) -> bool {
    c.dirty
        || c.volume_changed_at.is_some_and(|t| now.saturating_duration_since(t) >= VOLUME_QUIET)
        || (c.playing && now.saturating_duration_since(c.last_write) >= SAVE_EVERY)
}

// ---- the live session ------------------------------------------------------

struct Live {
    account: Option<String>,
    source: Option<Source>,
    track_uri: Option<String>,
    position_ms: u32,
    /// Some while playing: when `position_ms` was true.
    playing_since: Option<Instant>,
    shuffle: bool,
    repeat: Repeat,
    volume: u16,
    /// A track event came in since Spirc (re)connected. Spirc announces its fresh
    /// shuffle/repeat (off) on connect; those are not the user's and are ignored.
    has_track: bool,
    clock: WriteClock,
    /// Set by the exit save: nothing is written after it.
    closed: bool,
}

impl Live {
    fn new(now: Instant) -> Live {
        Live {
            account: None,
            source: None,
            track_uri: None,
            position_ms: 0,
            playing_since: None,
            shuffle: false,
            repeat: Repeat::Off,
            volume: DEFAULT_VOLUME,
            has_track: false,
            clock: WriteClock { dirty: false, volume_changed_at: None, last_write: now, playing: false },
            closed: false,
        }
    }

    fn position_at(&self, now: Instant) -> u32 {
        let played = self.playing_since.map_or(0, |t| now.saturating_duration_since(t).as_millis());
        (u128::from(self.position_ms) + played).min(u128::from(u32::MAX)) as u32
    }

    fn set_position(&mut self, position_ms: u32, playing: Option<bool>, now: Instant) {
        self.position_ms = position_ms;
        let playing = playing.unwrap_or(self.playing_since.is_some());
        self.playing_since = playing.then_some(now);
        self.clock.playing = playing;
    }

    fn set_track(&mut self, uri: &str) {
        self.has_track = true;
        if uri.is_empty() || self.track_uri.as_deref() == Some(uri) {
            return;
        }
        self.source = Some(source_after_track(self.source.as_ref(), uri));
        self.track_uri = Some(uri.to_string());
        self.clock.dirty = true;
    }

    fn snapshot(&self, now: Instant) -> Option<Saved> {
        Some(Saved {
            account: self.account.clone()?,
            source: self.source.clone(),
            track_uri: self.track_uri.clone(),
            position_ms: self.position_at(now),
            shuffle: self.shuffle,
            repeat: self.repeat,
            volume: self.volume,
            saved_at: unix_ms(),
        })
    }

    /// The snapshot to write now, if one is due; resets the clock.
    fn take_due(&mut self, now: Instant) -> Option<Saved> {
        if self.closed || self.account.is_none() || !write_due(&self.clock, now) {
            return None;
        }
        self.clock.dirty = false;
        self.clock.volume_changed_at = None;
        self.clock.last_write = now;
        self.snapshot(now)
    }

    fn on_event(&mut self, event: &PlayerEvent, now: Instant) {
        let uri = |id: &librespot_core::SpotifyUri| id.to_uri().unwrap_or_default();
        log_event(event);
        match event {
            PlayerEvent::SessionConnected { .. } => {
                self.has_track = false;
                self.set_position(self.position_at(now), Some(false), now);
            }
            PlayerEvent::TrackChanged { audio_item } => self.set_track(&audio_item.uri),
            PlayerEvent::Loading { track_id, position_ms, .. } => {
                self.set_track(&uri(track_id));
                self.set_position(*position_ms, Some(false), now);
            }
            PlayerEvent::Playing { track_id, position_ms, .. } => {
                self.set_track(&uri(track_id));
                self.set_position(*position_ms, Some(true), now);
            }
            PlayerEvent::Paused { track_id, position_ms, .. } => {
                self.set_track(&uri(track_id));
                self.set_position(*position_ms, Some(false), now);
                self.clock.dirty = true;
            }
            PlayerEvent::Seeked { position_ms, .. } => {
                self.set_position(*position_ms, None, now);
                self.clock.dirty = true;
            }
            PlayerEvent::PositionCorrection { position_ms, .. } | PlayerEvent::PositionChanged { position_ms, .. } => {
                self.set_position(*position_ms, None, now)
            }
            PlayerEvent::Stopped { .. } => self.set_position(self.position_at(now), Some(false), now),
            PlayerEvent::VolumeChanged { volume } if *volume != self.volume => {
                self.volume = *volume;
                self.clock.volume_changed_at = Some(now);
            }
            PlayerEvent::ShuffleChanged { shuffle } if self.has_track && *shuffle != self.shuffle => {
                self.shuffle = *shuffle;
                self.clock.dirty = true;
            }
            PlayerEvent::RepeatChanged { context, track } if self.has_track => {
                let repeat = Repeat::from_flags(*context, *track);
                if repeat != self.repeat {
                    self.repeat = repeat;
                    self.clock.dirty = true;
                }
            }
            _ => {}
        }
    }
}

/// The live session. One per app, shared by the engine and its event listener.
pub struct Tracker {
    path: PathBuf,
    live: Mutex<Live>,
}

impl Tracker {
    pub fn new(path: PathBuf) -> Tracker {
        Tracker { path, live: Mutex::new(Live::new(Instant::now())) }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Scope the session to `account`: the first call (or a new account) reads its saved
    /// session from disk; another account's session is ignored and later overwritten.
    pub fn use_account(&self, account: &str) {
        if account.is_empty() {
            return;
        }
        let mut live = self.live();
        if live.account.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(account)) {
            return;
        }
        let saved = read_for(&self.path, account);
        let mut fresh = Live::new(Instant::now());
        fresh.account = Some(account.to_string());
        if let Some(s) = saved {
            fresh.source = s.source;
            fresh.track_uri = s.track_uri;
            fresh.position_ms = s.position_ms;
            fresh.shuffle = s.shuffle;
            fresh.repeat = s.repeat;
            fresh.volume = s.volume;
        }
        fresh.closed = live.closed;
        *live = fresh;
    }

    pub fn account(&self) -> Option<String> {
        self.live().account.clone()
    }

    /// The volume a new Spirc starts at: the live (or saved) one.
    pub fn volume(&self) -> u16 {
        self.live().volume
    }

    /// A track event came in since Spirc connected: the player already has something.
    pub fn has_track(&self) -> bool {
        self.live().has_track
    }

    /// What `local_load` sent. `track_uri` None: the first track of a list, unknown for a context.
    pub fn loaded(&self, source: Source, track_uri: Option<String>, position_ms: u32, shuffle: bool, repeat: Repeat) {
        let mut live = self.live();
        let now = Instant::now();
        live.track_uri = track_uri.or_else(|| match &source {
            Source::Uris { uris } => uris.first().cloned(),
            Source::Context { .. } => None,
        });
        live.source = Some(source);
        live.set_position(position_ms, Some(false), now);
        live.shuffle = shuffle;
        live.repeat = repeat;
        live.clock.dirty = true;
    }

    /// A Connect cluster update while this Mac is the active device. A new loadable context
    /// for the current track means another Spotify client loaded it here: take it, with its
    /// shuffle/repeat (loads don't raise librespot's shuffle/repeat events). A track that
    /// isn't the current one is a stale update and is ignored.
    pub fn on_cluster(&self, context_uri: &str, track_uri: &str, shuffle: bool, repeat: Repeat) {
        let Some(context_uri) = loadable_context(context_uri) else { return };
        let mut live = self.live();
        if live.track_uri.as_deref() != Some(track_uri) {
            return;
        }
        if matches!(&live.source, Some(Source::Context { context_uri: c }) if c == context_uri) {
            return;
        }
        log::info!(target: LOG, "source is now context {context_uri} (loaded by another client)");
        live.source = Some(Source::Context { context_uri: context_uri.to_string() });
        live.shuffle = shuffle;
        live.repeat = repeat;
        live.clock.dirty = true;
    }

    pub fn on_event(&self, event: &PlayerEvent) {
        self.live().on_event(event, Instant::now());
    }

    /// The session as it stands, None until the account is known or with nothing loaded.
    pub fn current(&self) -> Option<Saved> {
        self.live().snapshot(Instant::now()).filter(|s| s.source.is_some())
    }

    /// Writes the session if a write is due (see `write_due`).
    pub fn save_if_due(&self) {
        let due = self.live().take_due(Instant::now());
        if let Some(saved) = due {
            write(&self.path, &saved);
        }
    }

    /// On app exit: writes the current position now, then stops writing.
    pub fn save_and_close(&self) {
        let snapshot = {
            let mut live = self.live();
            if live.closed {
                return;
            }
            live.closed = true;
            live.snapshot(Instant::now())
        };
        if let Some(saved) = snapshot {
            write(&self.path, &saved);
            log::info!(target: LOG, "saved at exit: {} at {} ms", saved.track_uri.as_deref().unwrap_or("-"), saved.position_ms);
        }
    }
}

/// Feeds librespot's player events into `tracker` and writes when due, until the player
/// goes away (its channel closes).
pub async fn listen(tracker: std::sync::Arc<Tracker>, mut events: PlayerEventChannel) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => tracker.on_event(&event),
                None => break,
            },
            _ = tick.tick() => {}
        }
        let t = tracker.clone();
        let _ = tokio::task::spawn_blocking(move || t.save_if_due()).await;
    }
}

/// `<app dir>/session.json`.
pub fn default_path() -> PathBuf {
    crate::auth::app_dir().join("session.json")
}

/// Log line for a restore.
pub fn describe(s: &Saved) -> String {
    format!(
        "{}, track {}, at {} ms",
        s.source.as_ref().map_or("nothing".into(), Source::kind),
        s.track_uri.as_deref().unwrap_or("(first)"),
        s.position_ms
    )
}

/// One log line per player event that matters for debugging (not the position ticks).
fn log_event(event: &PlayerEvent) {
    match event {
        PlayerEvent::PositionCorrection { .. } | PlayerEvent::PositionChanged { .. } => {}
        PlayerEvent::TrackChanged { audio_item } => {
            log::info!(target: "needle::player", "track {} \"{}\" ({} ms)", audio_item.uri, audio_item.name, audio_item.duration_ms)
        }
        PlayerEvent::Unavailable { track_id, .. } => log::warn!(target: "needle::player", "unavailable: {track_id:?}"),
        other => log::info!(target: "needle::player", "{}", event_summary(other)),
    }
}

/// The event's name and its small fields, without the debug dump of whole audio items.
fn event_summary(event: &PlayerEvent) -> String {
    let s = format!("{event:?}");
    if s.len() > 240 { format!("{}…", &s[..s.char_indices().nth(240).map_or(s.len(), |(i, _)| i)]) } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("needle-session-{}-{name}-{}", std::process::id(), unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.json")
    }

    fn sample() -> Saved {
        Saved {
            account: "alice".into(),
            source: Some(Source::Uris { uris: vec!["spotify:track:a".into(), "spotify:track:b".into()] }),
            track_uri: Some("spotify:track:b".into()),
            position_ms: 61_000,
            shuffle: true,
            repeat: Repeat::Context,
            volume: 40_000,
            saved_at: 1,
        }
    }

    #[test]
    fn file_round_trip_and_shape() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(parse(&json), Some(s.clone()));
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["source"], json!({"uris": ["spotify:track:a", "spotify:track:b"]}));
        assert_eq!(v["repeat"], "context");
        let ctx = Saved { source: Some(Source::Context { context_uri: "spotify:playlist:p".into() }), ..s };
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains(r#""source":{"context_uri":"spotify:playlist:p"}"#), "{json}");
        assert_eq!(parse(&json), Some(ctx));
    }

    #[test]
    fn payload_is_camel_case() {
        assert_eq!(
            sample().payload(),
            json!({"contextUri": null, "uris": ["spotify:track:a", "spotify:track:b"], "trackUri": "spotify:track:b",
                   "positionMs": 61000, "shuffle": true, "repeat": "context", "volume": 40000})
        );
    }

    #[test]
    fn parse_rejects_garbage_and_drops_empty_sources() {
        for bad in ["", "{", "[]", "null", r#"{"source":{"uris":[]}}"#, r#"{"account":""}"#] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        let s = parse(r#"{"account":"a","source":{"uris":[]}}"#).unwrap();
        assert_eq!(s.source, None);
        assert_eq!(s.volume, DEFAULT_VOLUME);
        assert_eq!(s.repeat, Repeat::Off);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let path = temp_file("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert!(read(&path).is_none());
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(path.with_extension("json.bad")).unwrap(), "{not json");
        // missing file: nothing, nothing moved
        assert!(read(&path).is_none());
    }

    #[test]
    fn other_account_is_ignored() {
        let path = temp_file("account");
        write(&path, &sample());
        assert!(read_for(&path, "bob").is_none());
        assert_eq!(read_for(&path, "ALICE").map(|s| s.track_uri), Some(Some("spotify:track:b".into())));
        let t = Tracker::new(path.clone());
        t.use_account("bob");
        assert!(t.current().is_none());
        assert_eq!(t.volume(), DEFAULT_VOLUME);
        let t = Tracker::new(path);
        t.use_account("alice");
        assert_eq!(t.current().unwrap().position_ms, 61_000);
        assert_eq!(t.volume(), 40_000);
    }

    #[test]
    fn written_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_file("mode");
        write(&path, &sample());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn track_outside_the_source_falls_back_to_itself() {
        let list = Source::Uris { uris: vec!["spotify:track:a".into(), "spotify:track:b".into()] };
        assert_eq!(source_after_track(Some(&list), "spotify:track:b"), list);
        assert_eq!(source_after_track(Some(&list), "spotify:track:z"), Source::Uris { uris: vec!["spotify:track:z".into()] });
        let ctx = Source::Context { context_uri: "spotify:album:x".into() };
        assert_eq!(source_after_track(Some(&ctx), "spotify:track:z"), ctx);
        assert_eq!(source_after_track(None, "spotify:track:z"), Source::Uris { uris: vec!["spotify:track:z".into()] });
    }

    #[test]
    fn only_real_contexts_are_loadable() {
        assert_eq!(loadable_context("spotify:playlist:p"), Some("spotify:playlist:p"));
        assert_eq!(loadable_context("spotify:user:u:collection"), Some("spotify:user:u:collection"));
        for bad in ["", "spotify:web-api", "spotify:search:abc", "spotify:local-files", "spotify:track:t", "context://x"] {
            assert_eq!(loadable_context(bad), None, "{bad}");
        }
    }

    #[test]
    fn write_timing() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let clock = WriteClock { dirty: false, volume_changed_at: None, last_write: t0, playing: false };
        assert!(!write_due(&clock, t0 + s(60)), "idle and paused: nothing to write");
        assert!(write_due(&WriteClock { dirty: true, ..clock }, t0), "a track change, pause or seek: at once");
        // volume: debounced
        let vol = WriteClock { volume_changed_at: Some(t0 + s(5)), ..clock };
        assert!(!write_due(&vol, t0 + s(5) + Duration::from_millis(500)));
        assert!(write_due(&vol, t0 + s(6)));
        // playing: every 10 s
        let playing = WriteClock { playing: true, ..clock };
        assert!(!write_due(&playing, t0 + s(9)));
        assert!(write_due(&playing, t0 + s(10)));
    }

    #[test]
    fn tracker_follows_loads_and_clusters() {
        let path = temp_file("tracker");
        let t = Tracker::new(path.clone());
        t.loaded(Source::Uris { uris: vec!["spotify:track:a".into()] }, None, 0, false, Repeat::Off);
        // no account yet: nothing written
        t.save_if_due();
        assert!(!path.exists());
        t.use_account("alice");
        t.loaded(Source::Uris { uris: vec!["spotify:track:a".into()] }, None, 500, true, Repeat::Track);
        t.save_if_due();
        let saved = read(&path).unwrap();
        assert_eq!(saved.track_uri.as_deref(), Some("spotify:track:a"));
        assert_eq!((saved.position_ms, saved.shuffle, saved.repeat), (500, true, Repeat::Track));
        // a stale cluster (other track) is ignored; one for the current track switches to its context
        t.on_cluster("spotify:playlist:p", "spotify:track:x", false, Repeat::Off);
        assert!(matches!(t.current().unwrap().source, Some(Source::Uris { .. })));
        t.on_cluster("spotify:web-api", "spotify:track:a", false, Repeat::Off);
        assert!(matches!(t.current().unwrap().source, Some(Source::Uris { .. })));
        t.on_cluster("spotify:playlist:p", "spotify:track:a", false, Repeat::Context);
        let cur = t.current().unwrap();
        assert_eq!(cur.source, Some(Source::Context { context_uri: "spotify:playlist:p".into() }));
        assert_eq!(cur.repeat, Repeat::Context);
        // exit: written, then closed
        t.save_and_close();
        assert_eq!(read(&path).unwrap().source, cur.source);
        t.loaded(Source::Uris { uris: vec!["spotify:track:q".into()] }, None, 0, false, Repeat::Off);
        t.save_if_due();
        assert_eq!(read(&path).unwrap().source, cur.source);
    }

    #[test]
    fn connect_time_modes_are_ignored_until_a_track() {
        let mut live = Live::new(Instant::now());
        live.shuffle = true;
        live.on_event(&PlayerEvent::ShuffleChanged { shuffle: false }, Instant::now());
        live.on_event(&PlayerEvent::RepeatChanged { context: true, track: false }, Instant::now());
        assert!(live.shuffle);
        assert_eq!(live.repeat, Repeat::Off);
        live.has_track = true;
        live.on_event(&PlayerEvent::ShuffleChanged { shuffle: false }, Instant::now());
        assert!(!live.shuffle && live.clock.dirty);
    }

    #[test]
    fn position_runs_while_playing() {
        let t0 = Instant::now();
        let mut live = Live::new(t0);
        live.set_position(1_000, Some(true), t0);
        assert_eq!(live.position_at(t0 + Duration::from_millis(2_500)), 3_500);
        live.set_position(live.position_at(t0 + Duration::from_secs(3)), Some(false), t0 + Duration::from_secs(3));
        assert_eq!(live.position_at(t0 + Duration::from_secs(60)), 4_000);
    }
}
