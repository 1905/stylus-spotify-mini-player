//! The app's own Spotify Connect speaker "This Mac" (librespot): Session + Player +
//! SoftMixer + Spirc, kept alive by a reconnect loop. For "This Mac" the UI calls the
//! `local_*` commands, which drive Spirc directly.
//!
//! The player logs in with Spotify's keymaster client id (a token of another client id logs
//! librespot in, but every audio fetch fails, P0 spike).
//! librespot's reusable credentials live in the credentials file. They are never logged.
//!
//! The playback session (what plays here, where, at what volume) is kept by session.rs:
//! fed from `local_load`, the player's events and Connect cluster updates, loaded back
//! (paused) on the first ready of each launch.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use librespot_connect::{ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack, Spirc};
use futures_util::StreamExt;
use librespot_core::dealer::{manager::BoxedStreamResult, protocol::Message};
use librespot_core::{authentication::Credentials, config::DeviceType, error::ErrorKind, Session, SessionConfig};
use librespot_playback::{
    config::PlayerConfig,
    mixer::{softmixer::SoftMixer, Mixer, MixerConfig, NoOpVolume},
    player::Player,
};
use librespot_protocol::authentication::AuthenticationType;
use librespot_protocol::connect::ClusterUpdate;
use serde::{Deserialize, Serialize};
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter, State as Managed};
use tokio::sync::watch;

use crate::nowplaying::{self, lock, volume_from_percent, Now, NowPlaying};
use crate::session::{self, Repeat, Source, Tracker};

/// The Connect device name other Spotify clients show. Renaming keeps the device id.
pub const DEVICE_NAME: &str = "This Mac";
/// librespot's default client id. Only its logins may fetch audio.
const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
/// The player login's redirect is `http://127.0.0.1:5588/login`, as in librespot's binary.
const LOGIN_PORT: u16 = 5588;
const LOGIN_PATH: &str = "/login";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// How long `engine_login` waits for the engine to settle after the browser part.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// One connect attempt (access point, login, Connect registration) may take this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A session that stayed up this long resets the reconnect backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// The scopes librespot's own binary asks for (librespot 0.8.0 src/main.rs `OAUTH_SCOPES`).
const OAUTH_SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

// ---- state machine ---------------------------------------------------------

/// The `Fatal` reason for an account without Premium. `auth_status` matches it.
pub const PREMIUM_REQUIRED: &str = "Spotify Premium is required to play on this Mac";

/// Whether the account has Premium, by the session attribute `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Premium {
    Yes,
    No,
    Unknown,
}

/// `Some("premium")` → Yes, any other value → No, no attribute → Unknown.
pub fn premium_from_attr(attr: Option<&str>) -> Premium {
    match attr {
        Some("premium") => Premium::Yes,
        Some(_) => Premium::No,
        None => Premium::Unknown,
    }
}

/// `auth_status`'s answer: "not_premium" after the Premium `Fatal`; "login" without stored
/// credentials or after Spotify refused them; else "ok".
pub fn auth_status_for(has_credentials: bool, state: &State) -> &'static str {
    match state {
        State::Failed(reason) if reason == PREMIUM_REQUIRED => "not_premium",
        State::NeedsLogin => "login",
        _ if !has_credentials => "login",
        _ => "ok",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    NeedsLogin,
    Starting,
    Ready,
    Reconnecting,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The engine (re)starts, with or without credentials to log in with.
    Start { has_credentials: bool },
    /// Spirc is up.
    Connected,
    /// A connect attempt failed for a passing reason, or a running session ended.
    Dropped,
    /// Spotify refused the player's credentials.
    AuthRejected,
    /// A failure no retry fixes.
    Fatal(String),
}

pub fn next_state(state: &State, event: Event) -> State {
    use State::*;
    match (state, event) {
        (_, Event::Start { has_credentials: true }) => Starting,
        (_, Event::Start { has_credentials: false }) => NeedsLogin,
        (Starting | Reconnecting, Event::Connected) => Ready,
        (Starting | Ready | Reconnecting, Event::Dropped) => Reconnecting,
        (Starting | Ready | Reconnecting, Event::AuthRejected) => NeedsLogin,
        (Starting | Ready | Reconnecting, Event::Fatal(reason)) => Failed(reason),
        // a stopped engine (needs_login, failed) only leaves through Start
        (s, _) => s.clone(),
    }
}

/// The `engine_status` command's answer and the `engine-status` event's payload.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    state: &'static str,
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The engine's Connect device id. Null until `ready`.
    device_id: Option<String>,
}

impl Status {
    fn new(s: &State, device_id: Option<&str>) -> Self {
        let (state, reason) = match s {
            State::NeedsLogin => ("needs_login", None),
            State::Starting => ("starting", None),
            State::Ready => ("ready", None),
            State::Reconnecting => ("reconnecting", None),
            State::Failed(r) => ("failed", Some(r.clone())),
        };
        let device_id = if *s == State::Ready { device_id.map(str::to_string) } else { None };
        Status { state, name: DEVICE_NAME, reason, device_id }
    }
}

/// What a librespot connect error means for the engine. librespot keeps its login
/// error type private, so this matches its message ("Login failed with reason: …").
fn classify(kind: ErrorKind, message: &str) -> Event {
    if kind == ErrorKind::Unauthenticated
        || message.contains("Bad credentials")
        || message.contains("Could not validate credentials")
    {
        Event::AuthRejected
    } else if message.contains("Premium account required") {
        Event::Fatal(PREMIUM_REQUIRED.into())
    } else if kind == ErrorKind::PermissionDenied {
        // other login refusals (banned, travel restriction…): retrying won't help
        Event::Fatal(message.into())
    } else {
        Event::Dropped
    }
}

/// Wait before reconnect attempt `attempt` (0-based): 1s, 2s, 4s… capped at 60s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64.checked_shl(attempt).unwrap_or(u64::MAX).min(60))
}

// ---- device id ---------------------------------------------------------------

/// `player-device-id` next to the credentials file. A stable id lets Spotify treat
/// the app as the same Connect device across launches.
fn device_id_path() -> std::path::PathBuf {
    crate::paths::app_dir().join("player-device-id")
}

/// The stored device id, or a new UUID v4 written to `path` (0600). A write failure
/// is logged: the new id still works for this launch.
fn load_or_create_device_id(path: &std::path::Path) -> String {
    if let Ok(stored) = std::fs::read_to_string(path) {
        let stored = stored.trim();
        if !stored.is_empty() {
            return stored.to_string();
        }
    }
    let id = new_device_id();
    if let Err(e) = crate::paths::write_private(path, &id) {
        eprintln!("engine: could not store the player device id: {e}");
    }
    id
}

/// A random UUID v4, hyphenated lowercase: librespot's own default device id.
fn new_device_id() -> String {
    SessionConfig::default().device_id
}

// ---- credential storage ----------------------------------------------------

/// Where librespot's reusable credentials live: a private file in the app, memory in tests.
pub trait CredStore: Send + Sync {
    fn load(&self) -> Option<Credentials>;
    fn save(&self, creds: &Credentials) -> Result<(), String>;
}

/// `player-credentials.json` in the app folder, readable by this user only
/// (0600). Not the Keychain: unsigned builds count as a new app after every rebuild, so
/// macOS asked for Keychain access again each time (user chose the file, 2026-10-02).
pub struct FileStore;

impl FileStore {
    fn path() -> std::path::PathBuf {
        crate::paths::app_dir().join("player-credentials.json")
    }
}

impl CredStore for FileStore {
    fn load(&self) -> Option<Credentials> {
        serde_json::from_str(&std::fs::read_to_string(Self::path()).ok()?).ok()
    }

    fn save(&self, creds: &Credentials) -> Result<(), String> {
        let json = serde_json::to_string(creds).map_err(|e| e.to_string())?;
        crate::paths::write_private(&Self::path(), &json).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

#[cfg(test)]
impl CredStore for MemoryStore {
    fn load(&self) -> Option<Credentials> {
        serde_json::from_str(self.0.lock().unwrap().as_deref()?).ok()
    }

    fn save(&self, creds: &Credentials) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(serde_json::to_string(creds).map_err(|e| e.to_string())?);
        Ok(())
    }
}

// ---- the engine ------------------------------------------------------------

/// Held in Tauri state. Cheap to clone: all clones share one engine.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

struct Inner {
    /// The current state. Its lock also makes "is this loop current?" + "apply" atomic.
    state: watch::Sender<State>,
    /// Bumped by every (re)start and by shutdown. A loop with a stale generation stops.
    generation: AtomicU64,
    app: OnceLock<AppHandle>,
    store: Arc<dyn CredStore>,
    /// The running Spirc, so restart and shutdown can stop it.
    spirc: Mutex<Option<Spirc>>,
    /// The connect loop. The async lock also serializes restarts.
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    login_busy: AtomicBool,
    /// The persisted Connect device id, read (or created) on the first run.
    device_id: OnceLock<String>,
    /// The playback session (session.rs).
    session: Arc<Tracker>,
    /// The saved session is loaded back on the first ready of the process only.
    restore_tried: AtomicBool,
    /// What plays here, for the UI (`player-state` events, nowplaying.rs).
    now: Arc<NowPlaying>,
    /// The volume set while This Mac was inactive, for its next load (`PendingVolume`).
    pending_volume: Mutex<PendingVolume>,
}

impl Engine {
    pub fn new(store: Arc<dyn CredStore>) -> Self {
        let session = Arc::new(Tracker::new(session::default_path()));
        let now = Arc::new(NowPlaying::new(session.clone()));
        Engine(Arc::new(Inner {
            state: watch::Sender::new(State::Starting),
            generation: AtomicU64::new(0),
            app: OnceLock::new(),
            store,
            spirc: Mutex::new(None),
            task: tokio::sync::Mutex::new(None),
            login_busy: AtomicBool::new(false),
            device_id: OnceLock::new(),
            session,
            restore_tried: AtomicBool::new(false),
            now,
            pending_volume: Mutex::new(PendingVolume::default()),
        }))
    }

    /// Where `engine-status` events go. Set once, in `setup`.
    pub fn attach(&self, app: AppHandle) {
        self.0.now.attach(app.clone());
        let _ = self.0.app.set(app);
    }

    fn state(&self) -> State {
        self.0.state.borrow().clone()
    }

    /// "ok", "login" or "not_premium" (`auth_status_for`).
    pub fn auth_status(&self) -> &'static str {
        auth_status_for(self.0.store.load().is_some(), &self.state())
    }

    fn status(&self, state: &State) -> Status {
        Status::new(state, self.0.device_id.get().map(String::as_str))
    }

    /// The persisted device id; reads or creates the file on the first call.
    fn device_id(&self) -> String {
        self.0.device_id.get_or_init(|| load_or_create_device_id(&device_id_path())).clone()
    }

    /// The player's session while the engine is ready and the session alive: internal.rs
    /// calls Spotify's internal endpoints with its tokens. None otherwise.
    pub fn live_session(&self) -> Option<Session> {
        if self.state() != State::Ready {
            return None;
        }
        self.0.now.session().filter(|s| !s.is_invalid())
    }

    /// What Spotify Connect looks like from here while ready: the latest cluster, this Mac's
    /// device id and its volume %. None before the first cluster update of the session.
    pub fn connect_view(&self) -> Option<(Arc<librespot_protocol::connect::Cluster>, String, u8)> {
        let session = self.live_session()?;
        let cluster = self.0.now.cluster()?;
        Some((cluster, session.device_id().to_string(), self.volume_percent()))
    }

    /// What plays here, from the player's own events (nowplaying.rs).
    pub fn now_playing(&self) -> Arc<NowPlaying> {
        self.0.now.clone()
    }

    /// This Mac's volume, 0–100 %: the level waiting for the next load while inactive
    /// (`PendingVolume`), else the player's.
    pub fn volume_percent(&self) -> u8 {
        let active = self.0.now.engine_active();
        lock(&self.0.pending_volume).read(active, self.0.now.volume_percent())
    }

    /// Runs `f` on the current Spirc. Err `ENGINE_NOT_READY` when the engine isn't
    /// ready or Spirc is gone (a send to a stopped Spirc fails too). Ok only means
    /// the command is queued.
    fn with_spirc(&self, f: impl FnOnce(&Spirc) -> Result<(), librespot_core::Error>) -> Result<(), String> {
        let state = self.state();
        if state != State::Ready {
            return Err(format!("ENGINE_NOT_READY: the player is {}", Status::new(&state, None).state));
        }
        let slot = self.0.spirc.lock().unwrap();
        let spirc = slot.as_ref().ok_or("ENGINE_NOT_READY: the player isn't connected")?;
        f(spirc).map_err(|e| format!("ENGINE_NOT_READY: {e}"))
    }

    /// Applies `event` from the loop of generation `generation`; a stale loop changes
    /// nothing. Emits `engine-status` when the state changes.
    fn apply(&self, generation: u64, event: Event) -> State {
        let changed = self.0.state.send_if_modified(|s| {
            if !self.is_current(generation) {
                return false;
            }
            let next = next_state(s, event);
            let changed = next != *s;
            *s = next;
            changed
        });
        let now = self.state();
        if changed {
            if let Some(app) = self.0.app.get() {
                let _ = app.emit("engine-status", self.status(&now));
            }
            if now != State::Ready {
                self.0.now.set_inactive();
            }
        }
        now
    }

    fn is_current(&self, generation: u64) -> bool {
        generation == self.0.generation.load(Ordering::SeqCst)
    }

    /// Bumps the generation (under the state lock, so no stale event lands after it)
    /// and asks the running Spirc to pause and disconnect.
    fn retire(&self) -> u64 {
        let mut generation = 0;
        self.0.state.send_if_modified(|_| {
            generation = self.0.generation.fetch_add(1, Ordering::SeqCst) + 1;
            false
        });
        self.stop_spirc();
        generation
    }

    /// Asks the running Spirc (if any) to pause and leave Spotify Connect.
    fn stop_spirc(&self) {
        let spirc = self.0.spirc.lock().unwrap().take();
        if let Some(spirc) = spirc {
            let _ = spirc.shutdown();
        }
    }

    /// Stops the running loop (if any) and starts a new one with `creds`, or with the
    /// stored credentials when None. No credentials → `needs_login`.
    pub async fn restart(&self, creds: Option<Credentials>) {
        let mut task = self.0.task.lock().await;
        let generation = self.retire();
        if let Some(mut old) = task.take() {
            // let Spirc say goodbye to Spotify; a loop asleep in its backoff is just cut
            if tokio::time::timeout(Duration::from_secs(3), &mut old).await.is_err() {
                old.abort();
            }
        }
        let creds = match creds {
            Some(c) => Some(c),
            None => {
                let store = self.0.store.clone();
                tokio::task::spawn_blocking(move || store.load()).await.ok().flatten()
            }
        };
        self.apply(generation, Event::Start { has_credentials: creds.is_some() });
        if let Some(creds) = creds {
            *task = Some(tauri::async_runtime::spawn(run(self.clone(), generation, creds)));
        }
    }

    /// On app exit: stop Spirc and give it up to 2s to disconnect. Not async: called
    /// from the event loop's Exit, outside the async runtime.
    /// The session's position is written first, synchronously.
    pub fn shutdown(&self) {
        self.0.session.save_and_close();
        self.retire();
        tauri::async_runtime::block_on(async {
            if let Some(mut task) = self.0.task.lock().await.take() {
                if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
                    task.abort();
                }
            }
        });
    }

    /// Waits until the engine leaves starting/reconnecting: Ok when ready.
    async fn settled(&self) -> Result<(), String> {
        let mut rx = self.0.state.subscribe();
        let settled = tokio::time::timeout(READY_TIMEOUT, rx.wait_for(|s| !matches!(s, State::Starting | State::Reconnecting)))
            .await
            .map_err(|_| format!("the player didn't connect in {} s", READY_TIMEOUT.as_secs()))?
            .map_err(|e| e.to_string())?
            .clone();
        match settled {
            State::Ready => Ok(()),
            State::NeedsLogin => Err("Spotify refused the player login".into()),
            State::Failed(reason) => Err(reason),
            State::Starting | State::Reconnecting => unreachable!(),
        }
    }
}

/// `initial_volume`: the session's volume, so launches and reconnects keep it.
fn connect_config(initial_volume: u16) -> ConnectConfig {
    ConnectConfig {
        name: DEVICE_NAME.into(),
        device_type: DeviceType::Computer,
        initial_volume,
        ..ConnectConfig::default()
    }
}

/// The connect loop of one engine generation: Session → Spirc → wait for it to end →
/// reconnect with backoff. Player and mixer live for the whole loop.
async fn run(engine: Engine, generation: u64, mut creds: Credentials) {
    // the persisted device id: the same Connect device across reconnects and launches
    let device_id = {
        let engine = engine.clone();
        tokio::task::spawn_blocking(move || engine.device_id()).await
    };
    let session_config = match device_id {
        Ok(device_id) => SessionConfig { device_id, ..SessionConfig::default() },
        Err(e) => {
            engine.apply(generation, Event::Fatal(format!("no device id: {e}")));
            return;
        }
    };
    let mixer: Arc<dyn Mixer> = match SoftMixer::open(MixerConfig::default()) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            engine.apply(generation, Event::Fatal(format!("no volume control: {e}")));
            return;
        }
    };
    let bitrate = tokio::task::spawn_blocking(|| crate::settings::load().bitrate).await.unwrap_or(crate::settings::DEFAULT_BITRATE);
    let player_config = PlayerConfig { bitrate: crate::settings::librespot_bitrate(bitrate), ..PlayerConfig::default() };
    let mut session = Session::new(session_config.clone(), None);
    // volume is applied by the output stage at playback time (audio_out.rs), not at decode time
    let sink_mixer = mixer.clone();
    let player = Player::new(player_config, session.clone(), Box::new(NoOpVolume), move || {
        Box::new(crate::audio_out::RampSink::new(sink_mixer))
    });
    // the session follows the player; the listener ends with the player (its channel closes)
    let tracker = engine.0.session.clone();
    if let Some(account) = creds.username.as_deref() {
        // stored credentials name the account: its saved volume is the first Spirc's volume
        let (t, account) = (tracker.clone(), account.to_string());
        let _ = tokio::task::spawn_blocking(move || t.use_account(&account)).await;
    }
    tauri::async_runtime::spawn(session::listen(tracker.clone(), player.get_player_event_channel()));
    let now_playing = engine.0.now.clone();
    now_playing.set_volume(tracker.volume());
    tauri::async_runtime::spawn(nowplaying::listen(now_playing.clone(), player.get_player_event_channel()));

    let mut attempt = 0;
    let mut type_logged = false;
    loop {
        let connect = Spirc::new(connect_config(tracker.volume()), session.clone(), creds.clone(), player.clone(), mixer.clone());
        // a stalled connect (half-open network after sleep) counts as a drop, not a hang in "starting"
        let connected = tokio::time::timeout(CONNECT_TIMEOUT, connect)
            .await
            .unwrap_or_else(|_| Err(librespot_core::Error::deadline_exceeded("connect timed out")));
        match connected {
            Ok((spirc, spirc_task)) => {
                // checked under the slot's lock: retire() bumps the generation before it
                // takes the slot, so either it sees this Spirc or this loop sees it's stale
                let stale = {
                    let mut slot = engine.0.spirc.lock().unwrap();
                    if engine.is_current(generation) {
                        *slot = Some(spirc);
                        None
                    } else {
                        Some(spirc)
                    }
                };
                if let Some(spirc) = stale {
                    // restarted while connecting: leave cleanly
                    let _ = spirc.shutdown();
                    spirc_task.await;
                    return;
                }
                let up_since = Instant::now();
                now_playing.set_session(session.clone());
                creds = keep_reusable(&engine, &session, creds).await;
                {
                    let (t, account) = (tracker.clone(), session.username());
                    let _ = tokio::task::spawn_blocking(move || t.use_account(&account)).await;
                }
                let account_type = session.get_user_attribute("type");
                if !type_logged {
                    log::info!(target: "stylus::player", "account type: {}", account_type.as_deref().unwrap_or("unknown"));
                    type_logged = true;
                }
                match premium_from_attr(account_type.as_deref()) {
                    Premium::No => {
                        engine.apply(generation, Event::Fatal(PREMIUM_REQUIRED.into()));
                        engine.stop_spirc();
                        spirc_task.await;
                        return;
                    }
                    Premium::Unknown => log::warn!(target: "stylus::player", "no account type from Spotify: the Premium check is skipped"),
                    Premium::Yes => {}
                }
                match engine.apply(generation, Event::Connected) {
                    State::Ready if !engine.0.restore_tried.swap(true, Ordering::SeqCst) => {
                        tauri::async_runtime::spawn(restore(engine.clone(), session.device_id().to_string()));
                    }
                    _ => {}
                }
                let mut cluster = cluster_updates(&session);
                tokio::pin!(spirc_task);
                // the Spirc task ends on shutdown or session loss; a dead player thread is fatal
                loop {
                    tokio::select! {
                        _ = &mut spirc_task => break,
                        Some(update) = cluster.next() => {
                            if let Ok(update) = update {
                                follow_cluster(&tracker, &update, session.device_id());
                                now_playing.on_cluster(&update, session.device_id());
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            if player.is_invalid() {
                                engine.apply(generation, Event::Fatal("the audio player stopped".into()));
                                engine.stop_spirc();
                                return;
                            }
                            // a dead session doesn't always end the Spirc task (a paused player sends
                            // nothing, the dealer's token refresh can fail while offline): reconnect anyway
                            if session.is_invalid() {
                                engine.stop_spirc();
                                let _ = tokio::time::timeout(Duration::from_secs(3), &mut spirc_task).await;
                                break;
                            }
                        }
                    }
                }
                engine.0.spirc.lock().unwrap().take();
                // the player outlives the Spirc: stop what it buffered, or the old track keeps
                // playing under a new Spirc that has no request id for it and can't pause it
                player.stop();
                if up_since.elapsed() >= STABLE_AFTER {
                    attempt = 0;
                }
                engine.apply(generation, Event::Dropped);
            }
            Err(e) => match classify(e.kind, &e.to_string()) {
                Event::Dropped => {
                    engine.apply(generation, Event::Dropped);
                }
                event => {
                    engine.apply(generation, event);
                    return;
                }
            },
        }
        if !engine.is_current(generation) {
            return;
        }
        tokio::time::sleep(backoff(attempt)).await;
        attempt += 1;
        if !engine.is_current(generation) {
            return;
        }
        // a Session can't be reused once it has connected or failed
        session = Session::new(session_config.clone(), None);
        player.set_session(session.clone());
    }
}

/// Connect cluster updates (the account's devices and the active one's player state).
/// Spirc listens too; the dealer hands each update to every listener.
fn cluster_updates(session: &Session) -> BoxedStreamResult<ClusterUpdate> {
    match session.dealer().listen_for("hm://connect-state/v1/cluster", Message::from_raw::<ClusterUpdate>) {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!(target: "stylus::session", "no cluster updates, loads by other clients keep their first track only: {e}");
            Box::pin(futures_util::stream::pending())
        }
    }
}

/// A cluster update while this Mac plays: tells the session which context another client loaded.
fn follow_cluster(tracker: &Tracker, update: &ClusterUpdate, device_id: &str) {
    let cluster = &update.cluster;
    if cluster.active_device_id != device_id {
        return;
    }
    let state = &cluster.player_state;
    let o = &state.options;
    tracker.on_cluster(&state.context_uri, &state.track.uri, o.shuffling_context, Repeat::from_flags(o.repeating_context, o.repeating_track));
}

/// Loads the saved session back, paused, on the first ready of the launch. Skipped when
/// there is none, when this player already has a track, or when another device is playing.
async fn restore(engine: Engine, device_id: String) {
    const LOG: &str = "stylus::session";
    let tracker = engine.0.session.clone();
    let Some(saved) = tracker.current() else {
        log::info!(target: LOG, "restore skipped: no saved session for this account");
        return;
    };
    if tracker.has_track() {
        log::info!(target: LOG, "restore skipped: the player already has a track");
        return;
    }
    // another device playing must not be interrupted: Spotify's own device state (the Connect
    // cluster) tells, and its first update arrives within seconds of connecting. Without it,
    // don't restore.
    let mut cluster = None;
    for _ in 0..12 {
        cluster = engine.0.now.cluster();
        if cluster.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if let Some(c) = cluster {
        let state = &c.player_state;
        if !c.active_device_id.is_empty() && c.active_device_id != device_id && state.is_playing && !state.is_paused {
            log::info!(target: LOG, "restore skipped: another device is playing (Connect state)");
            return;
        }
    } else {
        log::info!(target: LOG, "restore skipped: no Connect state, can't tell whether another device is playing");
        return;
    }
    if tracker.has_track() {
        log::info!(target: LOG, "restore skipped: the player got a track meanwhile");
        return;
    }
    let source = match saved.source.clone() {
        Some(Source::Context { context_uri }) => LoadSource::Context(context_uri),
        Some(Source::Uris { uris }) => LoadSource::Tracks(uris),
        None => return,
    };
    let modes = Modes { shuffle: saved.shuffle, repeat: saved.repeat == Repeat::Context, repeat_track: saved.repeat == Repeat::Track };
    let request = load_request(source, saved.track_uri.clone(), saved.position_ms, false, modes);
    let volume = saved.volume;
    // Spirc ignores everything while inactive: activate first, then volume and load, in order
    let sent = engine.with_spirc(|s| {
        s.activate()?;
        s.set_volume(volume)?;
        s.load(request)
    });
    match sent {
        Ok(()) => {
            log::info!(target: LOG, "restored {}", session::describe(&saved));
            if let Some(app) = engine.0.app.get() {
                let _ = app.emit("session-restored", saved.payload());
            }
        }
        Err(e) => log::warn!(target: LOG, "restore failed: {e}"),
    }
}

/// The reusable credentials a connected session got back from Spotify. After the
/// player login these replace the short-lived OAuth token, and they are stored.
/// librespot's own cache keeps exactly these fields; the type is always "stored
/// credentials" (checked against the P0 spike's cache file: auth_type 1).
async fn keep_reusable(engine: &Engine, session: &Session, creds: Credentials) -> Credentials {
    let reusable = Credentials {
        username: Some(session.username()),
        auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
        auth_data: session.auth_data(),
    };
    if reusable.auth_data.is_empty() || reusable == creds {
        return creds;
    }
    let store = engine.0.store.clone();
    let to_save = reusable.clone();
    let saved = tokio::task::spawn_blocking(move || store.save(&to_save)).await.map_err(|e| e.to_string()).and_then(|r| r);
    if let Err(e) = saved {
        eprintln!("engine: could not store the player login in the credentials file: {e}");
    }
    reusable
}

// ---- commands --------------------------------------------------------------

/// `{state, name: "This Mac", reason?, device_id}`; `device_id` is null until ready.
#[tauri::command]
pub fn engine_status(engine: Managed<'_, Engine>) -> Status {
    engine.status(&engine.state())
}

/// The player's own Spotify login in the browser (keymaster client id). Stores the
/// credentials, restarts the engine, and resolves once it is ready.
#[tauri::command]
pub async fn engine_login(engine: Managed<'_, Engine>) -> Result<(), String> {
    if engine.0.login_busy.swap(true, Ordering::SeqCst) {
        return Err("LOGIN_IN_PROGRESS".into());
    }
    let result = async {
        let token = crate::auth::oauth_login(KEYMASTER_CLIENT_ID, LOGIN_PORT, LOGIN_PATH, OAUTH_SCOPES, LOGIN_TIMEOUT).await?;
        engine.restart(Some(Credentials::with_access_token(token.access_token))).await;
        engine.settled().await
    }
    .await;
    engine.0.login_busy.store(false, Ordering::SeqCst);
    result
}

/// Restart with the stored credentials.
#[tauri::command]
pub async fn engine_restart(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.restart(None).await;
    Ok(())
}

/// The stream quality in kbps: 96, 160 or 320.
#[tauri::command]
pub fn engine_get_quality() -> u16 {
    crate::settings::load().bitrate
}

/// Stores the stream quality (96, 160 or 320 kbps) and restarts the engine with it.
/// Returns once the restart has begun; the UI waits for `engine-status` ready.
#[tauri::command]
pub async fn engine_set_quality(engine: Managed<'_, Engine>, kbps: u16) -> Result<(), String> {
    if !crate::settings::BITRATES.contains(&kbps) {
        return Err(format!("BAD_ARGS: quality must be 96, 160 or 320 kbps, got {kbps}"));
    }
    let saved = tokio::task::spawn_blocking(move || {
        crate::settings::update(|s| std::mem::replace(&mut s.bitrate, kbps)).map(|(old, _)| old)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    match saved {
        Ok(old) => log::info!("quality {old} → {kbps} kbps, restarting the engine"),
        Err(e) => {
            log::warn!("quality {kbps} kbps not saved: {e}");
            return Err(e);
        }
    }
    engine.restart(None).await;
    Ok(())
}

// ---- local transport (Spirc) --------------------------------------------

/// Most tracks one `local_load` takes.
const MAX_LOAD_URIS: usize = 200;

#[derive(Debug)]
enum LoadSource {
    Context(String),
    Tracks(Vec<String>),
}

/// Exactly one of a context uri or a track list (1–200). Empty values count as missing.
fn load_source(context_uri: Option<String>, uris: Option<Vec<String>>) -> Result<LoadSource, String> {
    let context_uri = context_uri.filter(|c| !c.is_empty());
    let uris = uris.filter(|u| !u.is_empty());
    match (context_uri, uris) {
        (Some(c), None) => Ok(LoadSource::Context(c)),
        (None, Some(u)) if u.len() > MAX_LOAD_URIS => {
            Err(format!("BAD_ARGS: {} uris, at most {MAX_LOAD_URIS}", u.len()))
        }
        (None, Some(u)) => Ok(LoadSource::Tracks(u)),
        (Some(_), Some(_)) => Err("BAD_ARGS: give contextUri or uris, not both".into()),
        (None, None) => Err("BAD_ARGS: give contextUri or uris".into()),
    }
}

/// Shuffle/repeat to keep across a load: librespot's handle_load resets them unless the
/// request carries them (Astra, 2026-10-02).
#[derive(Debug, Clone, Copy, Default)]
struct Modes {
    shuffle: bool,
    repeat: bool,
    repeat_track: bool,
}

fn modes(shuffle: Option<bool>, repeat: Option<String>) -> Modes {
    let repeat = repeat.unwrap_or_default();
    Modes { shuffle: shuffle.unwrap_or(false), repeat: repeat == "context", repeat_track: repeat == "track" }
}

fn load_request(source: LoadSource, track_uri: Option<String>, position_ms: u32, play: bool, m: Modes) -> LoadRequest {
    let options = LoadRequestOptions {
        start_playing: play,
        seek_to: position_ms,
        playing_track: track_uri.map(PlayingTrack::Uri),
        context_options: Some(LoadContextOptions::Options(Options { shuffle: m.shuffle, repeat: m.repeat, repeat_track: m.repeat_track })),
        ..LoadRequestOptions::default()
    };
    match source {
        LoadSource::Context(uri) => LoadRequest::from_context_uri(uri, options),
        LoadSource::Tracks(uris) => LoadRequest::from_tracks(uris, options),
    }
}

/// What `Engine::load` loads: a context or a track list (exactly one), from `track_uri` (None:
/// the first) at `position_ms`, playing or paused. `shuffle`/`repeat` ("off" | "context" |
/// "track") are kept across the load. The UI's `local_load` sends it camelCase.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSpec {
    pub context_uri: Option<String>,
    pub uris: Option<Vec<String>>,
    pub track_uri: Option<String>,
    #[serde(default)]
    pub position_ms: u32,
    #[serde(default)]
    pub play: bool,
    pub shuffle: Option<bool>,
    pub repeat: Option<String>,
}

/// This Mac's volume as last set while it was inactive. Spirc ignores volume then, and takes
/// its old level back when it activates: the level waits here, reads report it, and the next
/// load here sends it.
#[derive(Debug, Default, PartialEq)]
pub struct PendingVolume(Option<u8>);

impl PendingVolume {
    /// Sets `percent`: true when it goes to Spirc now (This Mac active), false when it waits.
    pub fn set(&mut self, active: bool, percent: u8) -> bool {
        self.0 = (!active).then_some(percent);
        active
    }

    /// The level to report: the waiting one while inactive, else the player's (`live`).
    pub fn read(&mut self, active: bool, live: u8) -> u8 {
        if active {
            // activated some other way (the UI, a phone): the player's level counts
            self.0 = None;
            return live;
        }
        self.0.unwrap_or(live)
    }

    pub fn take(&mut self) -> Option<u8> {
        self.0.take()
    }
}

pub const NOTHING_AFTER: &str = "Nothing after this track: next would stop playback";
pub const NOTHING_BEFORE: &str = "Nothing before this track: previous would stop playback";

/// Under this position `previous` goes to the track before, at or over it restarts the track:
/// librespot's own threshold (Spirc handle_prev, 3 s). Lower would let a paused press stop playback.
const PREV_RESTARTS_AFTER_MS: u32 = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Next,
    Previous,
}

/// Why a next/previous on This Mac would only stop playback (Spirc stops when there is no track
/// to go to), None when it may go. Only when the cluster's up-next is known for this track and
/// repeat is off; anything uncertain goes through.
pub fn skip_blocked(skip: Skip, n: &Now, now: Instant) -> Option<&'static str> {
    if !n.skips_fresh || n.repeat != Repeat::Off {
        return None;
    }
    match skip {
        Skip::Next if n.next.as_ref().is_some_and(Vec::is_empty) => Some(NOTHING_AFTER),
        Skip::Previous if n.prev == Some(false) && n.position(now) < PREV_RESTARTS_AFTER_MS => Some(NOTHING_BEFORE),
        _ => None,
    }
}

/// A local command's result, logged (an error as a warning).
fn logged(what: &str, r: Result<(), String>) -> Result<(), String> {
    match &r {
        Ok(()) => log::info!(target: "stylus::cmd", "{what}"),
        Err(e) => log::warn!(target: "stylus::cmd", "{what} failed: {e}"),
    }
    r
}

/// The in-app player's transport, for the UI's `local_*` commands and for control.rs (MCP).
/// Each queues a Spirc command: Err `ENGINE_NOT_READY` when the engine isn't ready.
impl Engine {
    /// True while the engine is ready (Spirc running, session up).
    pub fn is_ready(&self) -> bool {
        self.state() == State::Ready
    }

    pub fn play(&self) -> Result<(), String> {
        logged("play", self.with_spirc(Spirc::play))
    }

    pub fn pause(&self) -> Result<(), String> {
        logged("pause", self.with_spirc(Spirc::pause))
    }

    /// Err `NOTHING_AFTER` when next would only stop playback (`skip_blocked`).
    pub fn next(&self) -> Result<(), String> {
        self.may_skip(Skip::Next)?;
        crate::audio_out::flush();
        logged("next", self.with_spirc(Spirc::next))
    }

    /// Err `NOTHING_BEFORE` when previous would only stop playback (`skip_blocked`).
    pub fn prev(&self) -> Result<(), String> {
        self.may_skip(Skip::Previous)?;
        crate::audio_out::flush();
        logged("prev", self.with_spirc(Spirc::prev))
    }

    fn may_skip(&self, skip: Skip) -> Result<(), String> {
        let n = self.0.now.now();
        match skip_blocked(skip, &n, Instant::now()).filter(|_| n.engine_active) {
            Some(why) => Err(why.into()),
            None => Ok(()),
        }
    }

    pub fn seek(&self, position_ms: u32) -> Result<(), String> {
        crate::audio_out::flush();
        logged(&format!("seek {position_ms}"), self.with_spirc(|s| s.set_position_ms(position_ms)))
    }

    /// `percent` 0–100. True: sent to Spirc now; false: This Mac is inactive, so the level waits
    /// for its next load (`PendingVolume`).
    pub fn set_volume(&self, percent: u8) -> Result<bool, String> {
        let percent = percent.min(100);
        if !lock(&self.0.pending_volume).set(self.0.now.engine_active(), percent) {
            log::info!(target: "stylus::cmd", "volume {percent} waits for the next load here");
            return Ok(false);
        }
        logged(&format!("volume {percent}"), self.with_spirc(|s| s.set_volume(volume_from_percent(percent))))?;
        // the player's VolumeChanged follows in a moment: reads right after see the level already
        self.0.now.set_volume(volume_from_percent(percent));
        Ok(true)
    }

    pub fn set_shuffle(&self, on: bool) -> Result<(), String> {
        logged(&format!("shuffle {on}"), self.with_spirc(|s| s.shuffle(on)))
    }

    /// `mode` is "off", "context" or "track".
    pub fn set_repeat(&self, mode: &str) -> Result<(), String> {
        let (context, track) = match mode {
            "off" => (false, false),
            "context" => (true, false),
            "track" => (true, true),
            _ => return Err(format!("BAD_ARGS: bad repeat mode: {mode}")),
        };
        logged(&format!("repeat {mode}"), self.with_spirc(|s| {
            s.repeat(context)?;
            s.repeat_track(track)
        }))
    }

    /// What plays here now (the `player-state` payload), None before the player's first event.
    pub fn now_state(&self) -> Option<serde_json::Value> {
        self.0.now.snapshot()
    }

    /// Loads a context or a track list here. Activates the device first: Spirc ignores every
    /// command, Load included, while inactive. Then the volume set while inactive, if any.
    /// Ok only means queued.
    pub fn load(&self, spec: LoadSpec) -> Result<(), String> {
        let LoadSpec { context_uri, uris, track_uri, position_ms, play, shuffle, repeat } = spec;
        let source = load_source(context_uri, uris)?;
        let m = modes(shuffle, repeat);
        let saved = match &source {
            LoadSource::Context(c) => Source::Context { context_uri: c.clone() },
            LoadSource::Tracks(u) => Source::Uris { uris: u.clone() },
        };
        log::info!(target: "stylus::cmd", "load {source:?} at {track_uri:?} {position_ms} ms play={play} {m:?}");
        let request = load_request(source, track_uri.clone(), position_ms, play, m);
        crate::audio_out::flush();
        self.with_spirc(|s| {
            s.activate()?;
            s.load(request)
        })?;
        self.0.session.loaded(saved, track_uri, position_ms, m.shuffle, Repeat::from_flags(m.repeat, m.repeat_track));
        let pending = lock(&self.0.pending_volume).take();
        if let Some(p) = pending {
            // best effort: the load went through, a lost level isn't worth failing it
            let _ = logged(&format!("pending volume {p}"), self.with_spirc(|s| s.set_volume(volume_from_percent(p))));
        }
        Ok(())
    }
}

#[tauri::command]
pub fn local_play(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.play()
}

#[tauri::command]
pub fn local_pause(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.pause()
}

#[tauri::command]
pub fn local_next(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.next()
}

#[tauri::command]
pub fn local_prev(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.prev()
}

#[tauri::command]
pub fn local_seek(engine: Managed<'_, Engine>, position_ms: u32) -> Result<(), String> {
    engine.seek(position_ms)
}

/// `percent` 0–100.
#[tauri::command]
pub fn local_volume(engine: Managed<'_, Engine>, percent: u8) -> Result<(), String> {
    engine.set_volume(percent).map(|_| ())
}

#[tauri::command]
pub fn local_shuffle(engine: Managed<'_, Engine>, on: bool) -> Result<(), String> {
    engine.set_shuffle(on)
}

/// `mode` is "off", "context" or "track".
#[tauri::command]
pub fn local_repeat(engine: Managed<'_, Engine>, mode: String) -> Result<(), String> {
    engine.set_repeat(&mode)
}

/// What plays on this Mac now, from librespot: the latest `player-state` payload (see
/// nowplaying.rs), for the UI's first paint. Null before the player's first event.
#[tauri::command]
pub fn local_state(engine: Managed<'_, Engine>) -> Option<serde_json::Value> {
    engine.now_state()
}

/// Loads a context or a track list on this Mac's speaker (`Engine::load`). Ok only means
/// queued; the UI confirms the track from the player-state events.
#[tauri::command]
pub fn local_load(engine: Managed<'_, Engine>, spec: LoadSpec) -> Result<(), String> {
    engine.load(spec)
}

/// The saved playback session of the player's account, for the UI to show before its first
/// poll: `{contextUri, uris, trackUri, positionMs, shuffle, repeat, volume}` (one of
/// contextUri/uris is null; volume 0–65535). Null when there is none.
#[tauri::command]
pub async fn session_get(engine: Managed<'_, Engine>) -> Result<Option<serde_json::Value>, String> {
    let engine = engine.inner().clone();
    tokio::task::spawn_blocking(move || {
        let tracker = &engine.0.session;
        if tracker.account().is_none() {
            // before the engine connects: the account of the stored player login
            if let Some(account) = engine.0.store.load().and_then(|c| c.username) {
                tracker.use_account(&account);
            }
        }
        tracker.current().map(|s| s.payload())
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use State::*;

    #[test]
    fn start_depends_on_credentials() {
        for s in [NeedsLogin, Ready, Failed("x".into())] {
            assert_eq!(next_state(&s, Event::Start { has_credentials: true }), Starting);
            assert_eq!(next_state(&s, Event::Start { has_credentials: false }), NeedsLogin);
        }
    }

    #[test]
    fn premium_from_the_session_attribute() {
        assert_eq!(premium_from_attr(Some("premium")), Premium::Yes);
        assert_eq!(premium_from_attr(Some("free")), Premium::No);
        assert_eq!(premium_from_attr(Some("open")), Premium::No);
        assert_eq!(premium_from_attr(None), Premium::Unknown);
    }

    #[test]
    fn auth_status_from_the_engine() {
        assert_eq!(auth_status_for(true, &Starting), "ok");
        assert_eq!(auth_status_for(true, &Ready), "ok");
        assert_eq!(auth_status_for(true, &Failed("no audio".into())), "ok");
        assert_eq!(auth_status_for(false, &Starting), "login");
        assert_eq!(auth_status_for(true, &NeedsLogin), "login");
        assert_eq!(auth_status_for(true, &Failed(PREMIUM_REQUIRED.into())), "not_premium");
        assert_eq!(auth_status_for(false, &Failed(PREMIUM_REQUIRED.into())), "not_premium");
    }

    #[test]
    fn connected_is_ready() {
        assert_eq!(next_state(&Starting, Event::Connected), Ready);
        assert_eq!(next_state(&Reconnecting, Event::Connected), Ready);
    }

    #[test]
    fn drops_reconnect_while_running() {
        for s in [Starting, Ready, Reconnecting] {
            assert_eq!(next_state(&s, Event::Dropped), Reconnecting);
        }
    }

    #[test]
    fn auth_rejected_needs_login() {
        assert_eq!(next_state(&Starting, Event::AuthRejected), NeedsLogin);
        assert_eq!(next_state(&Reconnecting, Event::AuthRejected), NeedsLogin);
    }

    #[test]
    fn fatal_fails_with_reason() {
        assert_eq!(next_state(&Reconnecting, Event::Fatal("no audio".into())), Failed("no audio".into()));
    }

    #[test]
    fn a_stopped_engine_ignores_its_old_loop() {
        for s in [NeedsLogin, Failed("x".into())] {
            assert_eq!(next_state(&s, Event::Dropped), s);
            assert_eq!(next_state(&s, Event::Connected), s);
            assert_eq!(next_state(&s, Event::Fatal("y".into())), s);
        }
    }

    #[test]
    fn classify_login_failures() {
        let msg = |reason: &str| format!("Permission denied {{ Login failed with reason: {reason} }}");
        assert_eq!(classify(ErrorKind::PermissionDenied, &msg("Bad credentials")), Event::AuthRejected);
        assert_eq!(
            classify(ErrorKind::PermissionDenied, &msg("Could not validate credentials")),
            Event::AuthRejected
        );
        assert_eq!(classify(ErrorKind::Unauthenticated, "login5 refused"), Event::AuthRejected);
        assert_eq!(
            classify(ErrorKind::PermissionDenied, &msg("Premium account required")),
            Event::Fatal(PREMIUM_REQUIRED.into())
        );
        assert!(matches!(classify(ErrorKind::PermissionDenied, &msg("Application banned")), Event::Fatal(_)));
    }

    #[test]
    fn classify_network_errors_retry() {
        assert_eq!(classify(ErrorKind::Unavailable, "Service unavailable { connection refused }"), Event::Dropped);
        assert_eq!(classify(ErrorKind::DeadlineExceeded, "timeout"), Event::Dropped);
    }

    #[test]
    fn backoff_doubles_to_a_minute() {
        let secs: Vec<u64> = (0..9).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff(200).as_secs(), 60);
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::default();
        assert!(store.load().is_none());
        let creds = Credentials {
            username: Some("alice".into()),
            auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: vec![0, 1, 2, 255],
        };
        store.save(&creds).unwrap();
        assert_eq!(store.load(), Some(creds));
    }

    #[test]
    fn device_is_this_mac_at_half_volume() {
        let c = connect_config(session::DEFAULT_VOLUME);
        assert_eq!(c.name, "This Mac");
        assert_eq!(c.device_type, DeviceType::Computer);
        assert_eq!(c.initial_volume, u16::MAX / 2);
        assert_eq!(connect_config(40_000).initial_volume, 40_000);
        assert!(!c.disable_volume);
    }

    #[tokio::test]
    async fn engine_without_credentials_needs_login() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        engine.restart(None).await;
        assert_eq!(engine.state(), NeedsLogin);
        assert!(engine.0.task.lock().await.is_none());
    }

    #[tokio::test]
    async fn stale_loop_events_are_dropped() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        engine.restart(None).await; // generation 1, needs_login
        let old = engine.0.generation.load(Ordering::SeqCst);
        engine.retire();
        let now = engine.0.generation.load(Ordering::SeqCst);
        engine.apply(now, Event::Start { has_credentials: true });
        assert_eq!(engine.apply(old, Event::Fatal("old loop".into())), Starting);
        assert_eq!(engine.apply(now, Event::Dropped), Reconnecting);
    }

    #[test]
    fn status_payload() {
        let v = serde_json::to_value(Status::new(&Ready, Some("dev-1"))).unwrap();
        assert_eq!(v, serde_json::json!({"state": "ready", "name": "This Mac", "device_id": "dev-1"}));
        let v = serde_json::to_value(Status::new(&Failed("Premium required".into()), Some("dev-1"))).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"state": "failed", "name": "This Mac", "reason": "Premium required", "device_id": null})
        );
        let names: Vec<&str> = [NeedsLogin, Starting, Reconnecting].iter().map(|s| Status::new(s, Some("dev-1")).state).collect();
        assert_eq!(names, ["needs_login", "starting", "reconnecting"]);
    }

    #[test]
    fn status_device_id_only_when_ready() {
        for s in [NeedsLogin, Starting, Reconnecting, Failed("x".into())] {
            assert_eq!(Status::new(&s, Some("dev-1")).device_id, None, "{s:?}");
        }
        assert_eq!(Status::new(&Ready, Some("dev-1")).device_id.as_deref(), Some("dev-1"));
        // ready, but the id isn't known yet: null, not a made-up one
        assert_eq!(Status::new(&Ready, None).device_id, None);
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("stylus-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("player-device-id")
    }

    #[test]
    fn device_id_created_once_then_reused() {
        let path = temp_path("devid");
        let _ = std::fs::remove_file(&path);
        let first = load_or_create_device_id(&path);
        assert!(is_uuid_v4(&first), "{first}");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), first);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_or_create_device_id(&path), first);
        // a stored id survives surrounding whitespace
        std::fs::write(&path, "  abc-123\n").unwrap();
        assert_eq!(load_or_create_device_id(&path), "abc-123");
        // an empty or garbage file gets a fresh id
        std::fs::write(&path, " \n").unwrap();
        let fresh = load_or_create_device_id(&path);
        assert!(is_uuid_v4(&fresh) && fresh != first, "{fresh}");
    }

    #[test]
    fn new_device_ids_differ() {
        let (a, b) = (new_device_id(), new_device_id());
        assert!(is_uuid_v4(&a) && is_uuid_v4(&b));
        assert_ne!(a, b);
    }

    fn is_uuid_v4(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
            && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && parts[2].starts_with('4')
            && matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b'))
    }

    fn uris(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("spotify:track:{i}")).collect()
    }

    #[test]
    fn load_source_needs_exactly_one() {
        let ctx = Some("spotify:playlist:p".to_string());
        assert!(matches!(load_source(ctx.clone(), None), Ok(LoadSource::Context(c)) if c == "spotify:playlist:p"));
        assert!(matches!(load_source(None, Some(uris(2))), Ok(LoadSource::Tracks(t)) if t.len() == 2));
        for bad in [load_source(ctx, Some(uris(1))), load_source(None, None)] {
            assert!(bad.unwrap_err().starts_with("BAD_ARGS: "));
        }
        // empty values count as missing
        assert!(load_source(Some(String::new()), None).unwrap_err().starts_with("BAD_ARGS: "));
        assert!(load_source(None, Some(vec![])).unwrap_err().starts_with("BAD_ARGS: "));
    }

    #[test]
    fn load_source_caps_uris_at_200() {
        assert!(load_source(None, Some(uris(MAX_LOAD_URIS))).is_ok());
        assert!(load_source(None, Some(uris(MAX_LOAD_URIS + 1))).unwrap_err().starts_with("BAD_ARGS: "));
    }

    #[test]
    fn load_request_carries_options() {
        let req = load_request(LoadSource::Tracks(uris(2)), Some("spotify:track:1".into()), 4200, false, Modes::default());
        let dbg = format!("{req:?}");
        assert!(dbg.contains("start_playing: false"), "{dbg}");
        assert!(dbg.contains("seek_to: 4200"), "{dbg}");
        assert!(dbg.contains("Uri(\"spotify:track:1\")"), "{dbg}");
        let req = load_request(LoadSource::Context("spotify:album:a".into()), None, 0, true, Modes::default());
        let dbg = format!("{req:?}");
        assert!(dbg.contains("spotify:album:a") && dbg.contains("start_playing: true"), "{dbg}");
        assert!(dbg.contains("playing_track: None"), "{dbg}");
    }

    #[test]
    fn load_keeps_shuffle_and_repeat() {
        let req = load_request(LoadSource::Context("spotify:album:a".into()), None, 0, true, modes(Some(true), Some("context".into())));
        let dbg = format!("{req:?}");
        assert!(dbg.contains("shuffle: true") && dbg.contains("repeat: true") && dbg.contains("repeat_track: false"), "{dbg}");
        let m = modes(None, Some("track".into()));
        assert!(!m.shuffle && !m.repeat && m.repeat_track);
    }

    #[test]
    fn volume_set_while_inactive_waits_for_the_load() {
        // the observed bug: set_volume 35 while inactive was dropped by Spirc; the read said 73
        let mut p = PendingVolume::default();
        assert!(!p.set(false, 35), "inactive: not sent now");
        assert_eq!(p.read(false, 73), 35, "reads report the level set");
        assert_eq!(p.read(false, 73) as i64 + 10, 45, "volume_step +10 starts from it");
        assert_eq!(p.take(), Some(35), "the load sends it");
        assert_eq!(p.read(false, 73), 73);
        // active: straight to Spirc, nothing waits
        assert!(p.set(true, 30));
        assert_eq!(p.read(true, 30), 30);
        // activated some other way: the player's level counts, the old one is dropped
        p.set(false, 20);
        assert_eq!(p.read(true, 60), 60);
        assert_eq!(p.take(), None);
    }

    #[test]
    fn inactive_engine_volume_waits_for_the_load() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        assert_eq!(engine.set_volume(35), Ok(false), "not the active device: queued, no Spirc needed");
        assert_eq!(engine.volume_percent(), 35);
        assert!(engine.load(LoadSpec::default()).unwrap_err().starts_with("BAD_ARGS"));
        assert_eq!(engine.volume_percent(), 35, "a refused load keeps it waiting");
    }

    #[test]
    fn next_and_previous_refuse_only_when_they_would_stop() {
        let t0 = Instant::now();
        let at = |next: Option<Vec<&str>>, prev: Option<bool>, fresh: bool, pos: u32| {
            let mut n = Now::new(t0);
            n.track_uri = Some("spotify:track:kerala".into());
            n.next = next.map(|l| l.into_iter().map(String::from).collect());
            n.prev = prev;
            n.skips_fresh = fresh;
            n.position_ms = pos;
            n
        };
        // the observed bug: Kerala played on its own (uris:[track]), next stopped it silently
        assert_eq!(skip_blocked(Skip::Next, &at(Some(vec![]), Some(false), true, 6_000), t0), Some(NOTHING_AFTER));
        assert_eq!(skip_blocked(Skip::Next, &at(Some(vec!["spotify:track:b"]), Some(false), true, 6_000), t0), None);
        // up-next not known, or from before this track loaded: Spirc decides
        assert_eq!(skip_blocked(Skip::Next, &at(None, None, true, 6_000), t0), None);
        assert_eq!(skip_blocked(Skip::Next, &at(Some(vec![]), Some(false), false, 6_000), t0), None);
        // repeat on: Spirc wraps or repeats
        let mut rep = at(Some(vec![]), Some(false), true, 0);
        rep.repeat = Repeat::Context;
        assert_eq!(skip_blocked(Skip::Next, &rep, t0), None);
        // previous: no track before and under 3 s stops; later it restarts the track
        assert_eq!(skip_blocked(Skip::Previous, &at(Some(vec![]), Some(false), true, 1_000), t0), Some(NOTHING_BEFORE));
        assert_eq!(skip_blocked(Skip::Previous, &at(Some(vec![]), Some(false), true, 6_000), t0), None);
        // Astra round 4: the boundary matches librespot's 3 s (2.5–3 s used to slip through and stop)
        for (pos, want) in [(2_500, Some(NOTHING_BEFORE)), (2_700, Some(NOTHING_BEFORE)), (2_999, Some(NOTHING_BEFORE)), (3_000, None)] {
            assert_eq!(skip_blocked(Skip::Previous, &at(Some(vec![]), Some(false), true, pos), t0), want, "{pos}");
        }
        assert_eq!(skip_blocked(Skip::Previous, &at(Some(vec![]), Some(true), true, 1_000), t0), None);
    }

    #[test]
    fn a_load_makes_up_next_stale_until_the_cluster_speaks() {
        let t0 = Instant::now();
        let mut n = Now::new(t0);
        n.next = Some(vec![]);
        n.skips_fresh = true;
        let id = librespot_core::SpotifyUri::from_uri("spotify:track:5DAjrJqXqYtgr67pVhmUeR").unwrap();
        n.on_event(&librespot_playback::player::PlayerEvent::Loading { play_request_id: 2, track_id: id, position_ms: 0 }, t0);
        assert!(!n.skips_fresh, "the old track's up-next must not block the new one");
        assert_eq!(skip_blocked(Skip::Next, &n, t0), None);
    }

    #[test]
    fn commands_without_spirc_are_not_ready() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        let err = engine.with_spirc(|s| s.play()).unwrap_err();
        assert!(err.starts_with("ENGINE_NOT_READY: "), "{err}");
    }
}
