//! What plays on "This Mac", straight from librespot. Rust emits
//! `player-state` on every player event that changes what the UI shows (track, play/pause,
//! seek, position correction, volume, shuffle/repeat, end of track, stop, the device going
//! active or inactive), with the shape `playback_state` gives the UI (spotify.rs) plus
//! `engine_active` and `queue`. Track names, artists and covers come from librespot's
//! metadata (the player's own session), cached per uri. The up-next list
//! comes from Spotify Connect cluster updates (`player_state.next_tracks` of this device).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use librespot_core::{Session, SpotifyUri};
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_metadata::image::ImageSize;
use librespot_metadata::{Metadata, Track};
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use librespot_protocol::connect::{Cluster, ClusterUpdate};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::session::{Repeat, Source, Tracker};

pub const EVENT: &str = "player-state";
const LOG: &str = "stylus::now";
/// Up-next tracks in the payload: as many as the cover row shows.
pub const QUEUE_MAX: usize = 20;
/// Track metadata kept; past this the cache starts over.
const META_CACHE_MAX: usize = 500;
/// Metadata requests in flight for one queue.
const META_CONCURRENCY: usize = 4;

// ---- a track as the UI shows it ----------------------------------------------

/// One cover image of a track's album.
#[derive(Debug, Clone, PartialEq)]
pub struct Cover {
    pub size: ImageSize,
    pub width: i32,
    pub url: String,
}

/// The cover the UI uses: the LARGE one (640 px, what the Web API lists first), else the widest.
pub fn pick_cover(covers: &[Cover]) -> Option<String> {
    let rank = |s: ImageSize| match s {
        ImageSize::SMALL => 0,
        ImageSize::DEFAULT => 1,
        ImageSize::LARGE => 2,
        ImageSize::XLARGE => 3,
    };
    covers
        .iter()
        .find(|c| c.size == ImageSize::LARGE)
        .or_else(|| covers.iter().max_by_key(|c| (c.width, rank(c.size))))
        .map(|c| c.url.clone())
}

/// A track with the fields the UI's `Track` has.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    pub uri: String,
    pub id: String,
    pub name: String,
    /// (id, name), in credit order.
    pub artists: Vec<(String, String)>,
    pub album: String,
    pub cover: Option<String>,
    pub duration_ms: u32,
}

impl TrackInfo {
    /// The UI's `Track`: `{id, uri, name, artists, artist_list:[{id,name}], album, cover, duration_ms}`,
    /// the shape of every track list in the app.
    pub fn payload(&self) -> Value {
        let names: Vec<&str> = self.artists.iter().map(|(_, n)| n.as_str()).collect();
        let list: Vec<Value> = self.artists.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
        json!({
            "id": self.id,
            "uri": self.uri,
            "name": self.name,
            "artists": names.join(", "),
            "artist_list": list,
            "album": self.album,
            "cover": self.cover,
            "duration_ms": self.duration_ms,
        })
    }
}

fn id_of(uri: &SpotifyUri) -> String {
    uri.to_id().unwrap_or_default()
}

/// The track librespot just started. None for an episode or a local file: the Web API's
/// `/me/player` has no item for those either (the UI shows "an ad or a podcast").
fn from_audio_item(item: &AudioItem) -> Option<TrackInfo> {
    let UniqueFields::Track { artists, album, .. } = &item.unique_fields else { return None };
    let covers: Vec<Cover> = item.covers.iter().map(|c| Cover { size: c.size, width: c.width, url: c.url.clone() }).collect();
    Some(TrackInfo {
        uri: item.uri.clone(),
        id: id_of(&item.track_id),
        name: item.name.clone(),
        artists: artists.iter().map(|a| (id_of(&a.id), a.name.clone())).collect(),
        album: album.clone(),
        cover: pick_cover(&covers),
        duration_ms: item.duration_ms,
    })
}

/// A queued track from its metadata. Covers are file ids on Spotify's image host.
fn from_track(uri: &str, t: &Track) -> TrackInfo {
    let covers: Vec<Cover> = t
        .album
        .covers
        .iter()
        .map(|c| Cover { size: c.size, width: c.width, url: format!("https://i.scdn.co/image/{}", c.id) })
        .collect();
    TrackInfo {
        uri: uri.to_string(),
        id: id_of(&t.id),
        name: t.name.clone(),
        artists: t.artists.iter().map(|a| (id_of(&a.id), a.name.clone())).collect(),
        album: t.album.name.clone(),
        cover: pick_cover(&covers),
        duration_ms: t.duration.max(0) as u32,
    }
}

/// 0–65535 → 0–100 %, rounded.
pub(crate) fn volume_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + 32767) / 65535) as u8
}

// ---- the state ------------------------------------------------------------------

/// What the player's events say about now.
#[derive(Debug, Clone)]
pub struct Now {
    /// This Mac is the active Connect device (librespot's SessionConnected / SessionDisconnected).
    pub engine_active: bool,
    pub track_uri: Option<String>,
    pub playing: bool,
    pub position_ms: u32,
    /// When `position_ms` was true.
    pub position_at: Instant,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// From the cluster: what this device plays from. None = not known.
    pub context_uri: Option<String>,
    /// From the cluster: the next track uris. None = not known yet.
    pub next: Option<Vec<String>>,
    /// From the cluster: there is a track before this one. None = not known yet.
    pub prev: Option<bool>,
    /// `next` and `prev` came from a cluster update after the current track started loading:
    /// they describe this track, not the one before it.
    pub skips_fresh: bool,
}

impl Now {
    pub fn new(now: Instant) -> Now {
        Now {
            engine_active: false,
            track_uri: None,
            playing: false,
            position_ms: 0,
            position_at: now,
            volume: crate::session::DEFAULT_VOLUME,
            shuffle: false,
            repeat: Repeat::Off,
            context_uri: None,
            next: None,
            prev: None,
            skips_fresh: false,
        }
    }

    pub fn position(&self, now: Instant) -> u32 {
        let run = if self.playing { now.saturating_duration_since(self.position_at).as_millis() } else { 0 };
        (u128::from(self.position_ms) + run).min(u128::from(u32::MAX)) as u32
    }

    fn set_position(&mut self, position_ms: u32, playing: Option<bool>, now: Instant) {
        self.position_ms = position_ms;
        self.position_at = now;
        if let Some(p) = playing {
            self.playing = p;
        }
    }

    fn set_track(&mut self, uri: &SpotifyUri) {
        let uri = uri.to_uri().unwrap_or_default();
        if !uri.is_empty() {
            self.track_uri = Some(uri);
        }
    }

    /// Applies a player event. True when the UI should hear about it.
    pub fn on_event(&mut self, event: &PlayerEvent, now: Instant) -> bool {
        match event {
            PlayerEvent::SessionConnected { .. } => self.engine_active = true,
            PlayerEvent::SessionDisconnected { .. } => {
                self.engine_active = false;
                self.set_position(self.position(now), Some(false), now);
            }
            // the position comes with the Loading/Playing/Paused around it (a load can start mid-song)
            PlayerEvent::TrackChanged { audio_item } => self.track_uri = Some(audio_item.uri.clone()),
            PlayerEvent::Loading { track_id, position_ms, .. } => {
                self.skips_fresh = false;
                self.set_track(track_id);
                self.set_position(*position_ms, Some(false), now);
            }
            PlayerEvent::Playing { track_id, position_ms, .. } => {
                self.set_track(track_id);
                self.set_position(*position_ms, Some(true), now);
            }
            PlayerEvent::Paused { track_id, position_ms, .. } => {
                self.set_track(track_id);
                self.set_position(*position_ms, Some(false), now);
            }
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. }
            | PlayerEvent::PositionChanged { position_ms, .. } => self.set_position(*position_ms, None, now),
            PlayerEvent::Stopped { .. } => self.set_position(self.position(now), Some(false), now),
            PlayerEvent::EndOfTrack { .. } => {}
            PlayerEvent::VolumeChanged { volume } => self.volume = *volume,
            PlayerEvent::ShuffleChanged { shuffle } => self.shuffle = *shuffle,
            PlayerEvent::RepeatChanged { context, track } => self.repeat = Repeat::from_flags(*context, *track),
            _ => return false,
        }
        true
    }

    /// The payload: `playback_state`'s shape, plus `engine_active` and `queue`.
    /// `track`: the current track's metadata (None: an episode, or not known yet).
    /// `queue`: the next tracks with known metadata, None while the next uris aren't known.
    /// Not active (another device plays, or nothing is loaded here): `{active:false, engine_active}`.
    pub fn payload(&self, device_id: &str, device_name: &str, now: Instant, track: Option<&TrackInfo>, queue: Option<Vec<Value>>) -> Value {
        if !self.engine_active || self.track_uri.is_none() {
            return json!({ "active": false, "engine_active": self.engine_active });
        }
        let duration = track.map_or(u32::MAX, |t| if t.duration_ms > 0 { t.duration_ms } else { u32::MAX });
        json!({
            "active": true,
            "engine_active": true,
            "is_playing": self.playing,
            "progress_ms": self.position(now).min(duration),
            "device_id": device_id,
            "device_name": device_name,
            "track": track.map_or(Value::Null, TrackInfo::payload),
            "shuffle": self.shuffle,
            "repeat": self.repeat.as_str(),
            "volume_percent": volume_percent(self.volume),
            "supports_volume": true,
            "context_uri": self.context_uri,
            "queue": queue,
        })
    }
}

/// The next playable track uris of a cluster's player state (delimiters, pages and
/// episodes dropped), at most `QUEUE_MAX`.
pub fn next_uris<'a>(uris: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    uris.into_iter().filter(|u| u.starts_with("spotify:track:")).take(QUEUE_MAX).map(str::to_string).collect()
}

/// What the current track's metadata is.
#[derive(Debug, Clone)]
enum Meta {
    /// Not known yet: nothing is sent until it is (TrackChanged follows every load).
    Unknown,
    Track(TrackInfo),
    /// An episode or a local file: sent with `track: null`.
    NotATrack,
}

// ---- the live holder ----------------------------------------------------------------

/// One per engine. Fed by the player's events and the Connect cluster; emits `player-state`.
pub struct NowPlaying {
    now: Mutex<Now>,
    /// The current track's metadata, for `now.track_uri`.
    current: Mutex<(Option<String>, Meta)>,
    meta: Mutex<HashMap<String, TrackInfo>>,
    fetching: Mutex<HashSet<String>>,
    session: Mutex<Option<Session>>,
    /// The latest Connect cluster (all devices, the active one's player state), for
    /// `list_devices` / `get_queue`. None until the first update of a session.
    cluster: Mutex<Option<Arc<Cluster>>>,
    /// When the current session connected: how long it has gone without a cluster.
    session_since: Mutex<Option<Instant>>,
    app: OnceLock<AppHandle>,
    tracker: Arc<Tracker>,
    /// An event was seen: before that, `local_state` is null.
    seen: Mutex<bool>,
}

/// A mutex's guard, poisoned or not: a panic elsewhere must not take the state down with it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl NowPlaying {
    pub fn new(tracker: Arc<Tracker>) -> NowPlaying {
        NowPlaying {
            now: Mutex::new(Now::new(Instant::now())),
            current: Mutex::new((None, Meta::Unknown)),
            meta: Mutex::new(HashMap::new()),
            fetching: Mutex::new(HashSet::new()),
            session: Mutex::new(None),
            cluster: Mutex::new(None),
            session_since: Mutex::new(None),
            app: OnceLock::new(),
            tracker,
            seen: Mutex::new(false),
        }
    }

    pub fn attach(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }

    /// The player's session (a new one after every reconnect): metadata is fetched with it.
    pub fn set_session(&self, session: Session) {
        *lock(&self.session) = Some(session);
        *lock(&self.cluster) = None;
        *lock(&self.session_since) = Some(Instant::now());
    }

    /// The time since the current session connected. None before the first session.
    pub fn since_session(&self) -> Option<std::time::Duration> {
        lock(&self.session_since).map(|t| t.elapsed())
    }

    /// The latest Connect cluster of the current session.
    pub fn cluster(&self) -> Option<Arc<Cluster>> {
        lock(&self.cluster).clone()
    }

    /// This Mac is the active Connect device, by its own events.
    pub fn engine_active(&self) -> bool {
        lock(&self.now).engine_active
    }

    /// This Mac's volume, 0–100 %.
    pub fn volume_percent(&self) -> u8 {
        volume_percent(lock(&self.now).volume)
    }

    /// The player's current session, if it ever connected.
    pub fn session(&self) -> Option<Session> {
        lock(&self.session).clone()
    }

    pub fn set_volume(&self, volume: u16) {
        lock(&self.now).volume = volume;
    }

    fn device_id(&self) -> Option<String> {
        lock(&self.session).as_ref().map(|s| s.device_id().to_string())
    }

    fn cached(&self, uri: &str) -> Option<TrackInfo> {
        lock(&self.meta).get(uri).cloned()
    }

    fn remember(&self, t: TrackInfo) {
        let mut meta = lock(&self.meta);
        if meta.len() >= META_CACHE_MAX {
            meta.clear();
        }
        meta.insert(t.uri.clone(), t);
    }

    /// The current track's metadata, from the cache when the track changed without a
    /// TrackChanged (yet).
    fn current_meta(&self, track_uri: Option<&str>) -> Meta {
        let mut cur = lock(&self.current);
        if cur.0.as_deref() != track_uri {
            let meta = track_uri.and_then(|u| self.cached(u)).map_or(Meta::Unknown, Meta::Track);
            *cur = (track_uri.map(str::to_string), meta);
        }
        cur.1.clone()
    }

    /// The payload as of now; None while the current track's metadata isn't known.
    fn payload(&self) -> Option<Value> {
        let now = lock(&self.now).clone();
        let meta = self.current_meta(now.track_uri.as_deref());
        if now.engine_active && now.track_uri.is_some() && matches!(meta, Meta::Unknown) {
            return None;
        }
        let track = match &meta {
            Meta::Track(t) => Some(t.clone()),
            _ => None,
        };
        let queue = now.next.as_ref().map(|uris| uris.iter().filter_map(|u| self.cached(u)).map(|t| t.payload()).collect());
        let mut now = now;
        // before the first cluster update: the context this player loaded, if it's this track's
        if now.context_uri.is_none() {
            now.context_uri = self.tracker.current().and_then(|s| match s.source {
                Some(Source::Context { context_uri }) if s.track_uri == now.track_uri => Some(context_uri),
                _ => None,
            });
        }
        let device_id = self.device_id().unwrap_or_default();
        Some(now.payload(&device_id, crate::player::DEVICE_NAME, Instant::now(), track.as_ref(), queue))
    }

    fn emit(&self) {
        let Some(app) = self.app.get() else { return };
        if let Some(p) = self.payload() {
            let _ = app.emit(EVENT, p);
        }
    }

    /// The `local_state` answer: the latest state, or None before the first event.
    pub fn snapshot(&self) -> Option<Value> {
        if !*lock(&self.seen) {
            return None;
        }
        let now = lock(&self.now).clone();
        // metadata still loading: the inactive shape would be wrong, so say nothing yet
        self.payload().or_else(|| (!now.engine_active).then(|| json!({ "active": false, "engine_active": false })))
    }

    pub fn on_event(&self, event: &PlayerEvent) {
        if let PlayerEvent::TrackChanged { audio_item } = event {
            let meta = match from_audio_item(audio_item) {
                Some(t) => {
                    self.remember(t.clone());
                    Meta::Track(t)
                }
                None => Meta::NotATrack,
            };
            *lock(&self.current) = (Some(audio_item.uri.clone()), meta);
        }
        let changed = lock(&self.now).on_event(event, Instant::now());
        if changed {
            *lock(&self.seen) = true;
            self.emit();
        }
    }

    /// The engine stopped (dropped, restarting, logged out): nothing plays here.
    pub fn set_inactive(&self) {
        let was = {
            let mut now = lock(&self.now);
            let was = now.engine_active;
            *lock(&self.cluster) = None;
            now.engine_active = false;
            now.playing = false;
            was
        };
        if was {
            log::info!(target: LOG, "This Mac is no longer active: the engine stopped");
            self.emit();
        }
    }

    /// A Connect cluster update. While this Mac is the active device: its context and up-next.
    pub fn on_cluster(self: &Arc<Self>, update: &ClusterUpdate, device_id: &str) {
        let cluster = &update.cluster;
        let first = lock(&self.cluster).replace(Arc::new(cluster.clone().unwrap_or_default())).is_none();
        if first {
            let active = if cluster.active_device_id.is_empty() { "none" } else { cluster.active_device_id.as_str() };
            log::info!(target: LOG, "first Connect cluster: {} devices, active device {active}", cluster.device.len());
        }
        if cluster.active_device_id != device_id {
            return;
        }
        let state = &cluster.player_state;
        let context = crate::session::loadable_context(&state.context_uri).map(str::to_string);
        let next = next_uris(state.next_tracks.iter().map(|t| t.uri.as_str()));
        let prev = !next_uris(state.prev_tracks.iter().map(|t| t.uri.as_str())).is_empty();
        // a load applies shuffle/repeat without emitting player events (librespot handle_load):
        // the cluster's options are the only report of them, e.g. after a restore
        let o = &state.options;
        let (shuffle, repeat) = (o.shuffling_context, Repeat::from_flags(o.repeating_context, o.repeating_track));
        let changed = {
            let mut now = lock(&self.now);
            let changed = now.context_uri != context || now.next.as_ref() != Some(&next) || now.shuffle != shuffle || now.repeat != repeat;
            now.shuffle = shuffle;
            now.repeat = repeat;
            now.context_uri = context;
            now.next = Some(next.clone());
            now.prev = Some(prev);
            now.skips_fresh = true;
            changed
        };
        if !changed {
            return;
        }
        self.emit();
        self.fetch_missing(next);
    }

    /// Fetches the metadata of the uris not cached yet (librespot), then emits.
    fn fetch_missing(self: &Arc<Self>, uris: Vec<String>) {
        let Some(session) = lock(&self.session).clone() else { return };
        let missing: Vec<String> = {
            let meta = lock(&self.meta);
            let mut fetching = lock(&self.fetching);
            uris.into_iter().filter(|u| !meta.contains_key(u) && fetching.insert(u.clone())).collect()
        };
        if missing.is_empty() {
            return;
        }
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            use futures_util::stream::{self, StreamExt};
            let got: Vec<(String, Option<TrackInfo>)> = stream::iter(missing)
                .map(|uri| {
                    let session = session.clone();
                    async move {
                        let info = match SpotifyUri::from_uri(&uri) {
                            Ok(id) => match Track::get(&session, &id).await {
                                Ok(t) => Some(from_track(&uri, &t)),
                                Err(e) => {
                                    log::info!(target: LOG, "no metadata for {uri}: {e}");
                                    None
                                }
                            },
                            Err(_) => None,
                        };
                        (uri, info)
                    }
                })
                .buffered(META_CONCURRENCY)
                .collect()
                .await;
            // one lock at a time: fetch_missing takes meta then fetching, so holding fetching
            // while remember() takes meta could deadlock against it
            {
                let mut fetching = lock(&this.fetching);
                for (uri, _) in &got {
                    fetching.remove(uri);
                }
            }
            for (_, info) in got {
                if let Some(t) = info {
                    this.remember(t);
                }
            }
            this.emit();
        });
    }
}

impl NowPlaying {
    /// The raw state as the player's events left it: live even while a track's metadata loads,
    /// when `snapshot` says nothing (the MCP routing, control.rs and mcp_app.rs, needs it).
    pub fn now(&self) -> Now {
        lock(&self.now).clone()
    }

    /// What was last loaded here (the session), None with nothing loaded.
    pub fn last_session(&self) -> Option<crate::session::Saved> {
        self.tracker.current()
    }
}

/// 0–100 % → 0–65535, the inverse of `volume_percent`. Above 100 counts as 100.
pub(crate) fn volume_from_percent(percent: u8) -> u16 {
    ((u32::from(percent.min(100)) * 65535 + 50) / 100) as u16
}

/// Feeds the player's events into `np` until the player goes away (its channel closes).
pub async fn listen(np: Arc<NowPlaying>, mut events: PlayerEventChannel) {
    while let Some(event) = events.recv().await {
        np.on_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn info() -> TrackInfo {
        TrackInfo {
            uri: URI.into(),
            id: "4uLU6hMCjMI75M1A2tKUQC".into(),
            name: "Never Gonna Give You Up".into(),
            artists: vec![("0gxyHStUsqpMadRV0Di1Qt".into(), "Rick Astley".into()), ("x".into(), "Someone".into())],
            album: "Whenever You Need Somebody".into(),
            cover: Some("https://i.scdn.co/image/ab67616d0000b273".into()),
            duration_ms: 213_000,
        }
    }

    fn track_id() -> SpotifyUri {
        SpotifyUri::from_uri(URI).unwrap()
    }

    #[test]
    fn track_payload_shape() {
        assert_eq!(
            info().payload(),
            json!({
                "id": "4uLU6hMCjMI75M1A2tKUQC", "uri": URI, "name": "Never Gonna Give You Up",
                "artists": "Rick Astley, Someone",
                "artist_list": [{"id": "0gxyHStUsqpMadRV0Di1Qt", "name": "Rick Astley"}, {"id": "x", "name": "Someone"}],
                "album": "Whenever You Need Somebody", "cover": "https://i.scdn.co/image/ab67616d0000b273", "duration_ms": 213000,
            })
        );
    }

    #[test]
    fn cover_prefers_large_then_widest() {
        let c = |size, width, url: &str| Cover { size, width, url: url.into() };
        let all = [c(ImageSize::SMALL, 64, "s"), c(ImageSize::LARGE, 640, "l"), c(ImageSize::DEFAULT, 300, "d")];
        assert_eq!(pick_cover(&all).as_deref(), Some("l"));
        assert_eq!(pick_cover(&all[..1]).as_deref(), Some("s"));
        // no widths (track metadata): the bigger size wins
        assert_eq!(pick_cover(&[c(ImageSize::SMALL, 0, "s"), c(ImageSize::DEFAULT, 0, "d")]).as_deref(), Some("d"));
        assert_eq!(pick_cover(&[]), None);
    }

    #[test]
    fn state_payload_shape() {
        let t0 = Instant::now();
        let mut now = Now::new(t0);
        now.engine_active = true;
        now.on_event(&PlayerEvent::Playing { play_request_id: 1, track_id: track_id(), position_ms: 10_000 }, t0);
        now.on_event(&PlayerEvent::VolumeChanged { volume: 65535 }, t0);
        now.on_event(&PlayerEvent::ShuffleChanged { shuffle: true }, t0);
        now.on_event(&PlayerEvent::RepeatChanged { context: true, track: false }, t0);
        now.context_uri = Some("spotify:playlist:p".into());
        now.next = Some(vec![]);
        let p = now.payload("dev1", "This Mac", t0 + Duration::from_millis(2_500), Some(&info()), Some(vec![info().payload()]));
        assert_eq!(
            p,
            json!({
                "active": true, "engine_active": true, "is_playing": true, "progress_ms": 12500,
                "device_id": "dev1", "device_name": "This Mac", "track": info().payload(),
                "shuffle": true, "repeat": "context", "volume_percent": 100, "supports_volume": true,
                "context_uri": "spotify:playlist:p", "queue": [info().payload()],
            })
        );
        // paused: the position stops; past the end it is capped at the duration
        now.on_event(&PlayerEvent::Paused { play_request_id: 1, track_id: track_id(), position_ms: 300_000 }, t0);
        let p = now.payload("dev1", "This Mac", t0 + Duration::from_secs(60), Some(&info()), None);
        assert_eq!((p["is_playing"].clone(), p["progress_ms"].clone(), p["queue"].clone()), (json!(false), json!(213000), Value::Null));
        // an episode: active, no track
        assert_eq!(now.payload("dev1", "This Mac", t0, None, None)["track"], Value::Null);
    }

    #[test]
    fn inactive_and_empty_say_so() {
        let t0 = Instant::now();
        let mut now = Now::new(t0);
        assert_eq!(now.payload("d", "This Mac", t0, None, None), json!({"active": false, "engine_active": false}));
        assert!(now.on_event(&PlayerEvent::SessionConnected { connection_id: "c".into(), user_name: "u".into() }, t0));
        // active but nothing loaded yet
        assert_eq!(now.payload("d", "This Mac", t0, None, None), json!({"active": false, "engine_active": true}));
        now.on_event(&PlayerEvent::Playing { play_request_id: 1, track_id: track_id(), position_ms: 0 }, t0);
        assert_eq!(now.payload("d", "This Mac", t0, Some(&info()), None)["active"], true);
        // another device took over
        now.on_event(&PlayerEvent::SessionDisconnected { connection_id: "c".into(), user_name: "u".into() }, t0 + Duration::from_secs(5));
        assert!(!now.playing);
        assert_eq!(now.position_ms, 5_000, "the position stops where it was");
        assert_eq!(now.payload("d", "This Mac", t0, Some(&info()), None), json!({"active": false, "engine_active": false}));
    }

    #[test]
    fn events_that_change_nothing_are_not_sent() {
        let mut now = Now::new(Instant::now());
        assert!(!now.on_event(&PlayerEvent::AutoPlayChanged { auto_play: true }, Instant::now()));
        assert!(!now.on_event(&PlayerEvent::PlayRequestIdChanged { play_request_id: 3 }, Instant::now()));
        assert!(now.on_event(&PlayerEvent::Seeked { play_request_id: 1, track_id: track_id(), position_ms: 42 }, Instant::now()));
        assert_eq!(now.position_ms, 42);
    }

    #[test]
    fn next_uris_keep_tracks_only() {
        let raw = ["spotify:track:a", "spotify:delimiter", "spotify:episode:e", "spotify:meta:page:1", "spotify:track:b"];
        assert_eq!(next_uris(raw), vec!["spotify:track:a".to_string(), "spotify:track:b".to_string()]);
        let many: Vec<String> = (0..50).map(|i| format!("spotify:track:{i}")).collect();
        assert_eq!(next_uris(many.iter().map(String::as_str)).len(), QUEUE_MAX);
    }

    #[test]
    fn volume_percent_rounds() {
        assert_eq!(volume_percent(0), 0);
        assert_eq!(volume_percent(65535), 100);
        assert_eq!(volume_percent(32768), 50);
        for p in [0, 1, 30, 35, 40, 73, 99, 100] {
            assert_eq!(volume_percent(volume_from_percent(p)), p, "{p}");
        }
        assert_eq!(volume_from_percent(50), 32768);
        assert_eq!(volume_from_percent(1), 655);
        assert_eq!(volume_from_percent(255), 65535, "above 100 counts as 100");
    }
}
