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
    player::{Player, PlayerEvent},
};
use librespot_protocol::authentication::AuthenticationType;
use librespot_protocol::connect::ClusterUpdate;
use serde::{Deserialize, Serialize};
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter, State as Managed};
use tokio::sync::watch;

use crate::applog::AUTH;
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
/// How long the Premium check waits for the account type (ProductInfo can come after Spirc::new).
const ACCOUNT_TYPE_WAIT: Duration = Duration::from_secs(2);
/// A session that stayed up this long resets the reconnect backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// No Connect cluster this long after Connect came up (the dealer's connection id): the UI sees "nothing active" and This Mac
/// alone, not an endless ENGINE_NOT_READY (`cluster_or_quiet`).
const QUIET_VIEW_AFTER: Duration = Duration::from_secs(4);
/// No Connect cluster this long after Connect came up: the launch restore runs anyway (`restore_gate`).
const QUIET_RESTORE_AFTER: Duration = Duration::from_secs(5);
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

/// The start of `engine_login`'s error when the login works but its file can't be written.
/// The UI matches it (src/lib/engine.js `LOGIN_NOT_SAVED`).
pub const LOGIN_NOT_SAVED: &str = "logged in, but the login could not be saved";

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

/// `auth_status`'s answer: "ok" while the engine is ready or reconnecting (also when the
/// login's save failed and no file exists); "not_premium" after the Premium `Fatal`;
/// "login" without stored credentials or after Spotify refused them; else "ok".
pub fn auth_status_for(has_credentials: bool, state: &State) -> &'static str {
    match state {
        State::Ready | State::Reconnecting => "ok",
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

/// The event's variant name for a log line (a `Fatal` reason is not part of it).
fn event_name(event: &Event) -> &'static str {
    match event {
        Event::Start { .. } => "Start",
        Event::Connected => "Connected",
        Event::Dropped => "Dropped",
        Event::AuthRejected => "AuthRejected",
        Event::Fatal(_) => "Fatal",
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
        log::warn!(target: "stylus::player", "engine: could not store the player device id: {e}");
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
    /// Removes the stored credentials (logout). None stored is Ok.
    fn clear(&self) -> Result<(), String>;
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
        let text = match std::fs::read_to_string(Self::path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::warn!(target: AUTH, "credentials: unreadable: {e}");
                return None;
            }
        };
        // serde's error names a line and column, not the content
        serde_json::from_str(&text).map_err(|e| log::warn!(target: AUTH, "credentials: unreadable: {e}")).ok()
    }

    fn save(&self, creds: &Credentials) -> Result<(), String> {
        let json = serde_json::to_string(creds).map_err(|e| e.to_string())?;
        crate::paths::write_private(&Self::path(), &json).map_err(|e| e.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        let existed = crate::paths::remove_if_exists(&Self::path()).map_err(|e| format!("could not remove the credentials file: {e}"))?;
        log::info!(target: AUTH, "logout: credentials removed (file existed: {existed})");
        Ok(())
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

    fn clear(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
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
    /// Bumped by logout: a browser login that started before it must not log back in.
    login_gen: AtomicU64,
    /// Held by logout for its whole body and by a login from its `login_gen` check until it
    /// settles: a login can't restart the engine during a logout. Taken before `task`.
    auth_op: tokio::sync::Mutex<()>,
    /// The persisted Connect device id, read (or created) on the first run.
    device_id: OnceLock<String>,
    /// The playback session (session.rs).
    session: Arc<Tracker>,
    /// The saved session is loaded back on the first ready of the process only.
    restore_tried: AtomicBool,
    /// The connection dropped while this Mac was the active device: the next ready loads
    /// the session back (paused), so the UI doesn't fall to "no device".
    reload_after_drop: AtomicBool,
    /// A restore gave up only because no Connect state came in time: the first cluster
    /// update (or the quiet timer) runs it again, with the same `Restore` kind.
    restore_pending: Mutex<Option<Restore>>,
    /// What plays here, for the UI (`player-state` events, nowplaying.rs).
    now: Arc<NowPlaying>,
    /// The volume set while This Mac was inactive, for its next load (`PendingVolume`).
    pending_volume: Mutex<PendingVolume>,
    /// Why a save of the player login failed since the last `engine_login` began. It reports it.
    save_error: Mutex<Option<String>>,
    /// The credentials the current loop logs in with (`remember_live`). A restart without
    /// new credentials uses them before the store: they work even when their save failed.
    live_creds: Mutex<Option<Credentials>>,
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
            login_gen: AtomicU64::new(0),
            auth_op: tokio::sync::Mutex::new(()),
            device_id: OnceLock::new(),
            session,
            restore_tried: AtomicBool::new(false),
            reload_after_drop: AtomicBool::new(false),
            restore_pending: Mutex::new(None),
            now,
            pending_volume: Mutex::new(PendingVolume::default()),
            save_error: Mutex::new(None),
            live_creds: Mutex::new(None),
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
    /// device id and its volume %. None before the first cluster update of the session, for
    /// `QUIET_VIEW_AFTER`; then an empty cluster (no device active, no devices listed).
    pub fn connect_view(&self) -> Option<(Arc<librespot_protocol::connect::Cluster>, String, u8)> {
        let session = self.live_session()?;
        let cluster = cluster_or_quiet(self.0.now.cluster(), self.0.now.since_session())?;
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
        let name = event_name(&event);
        let mut prev = None;
        let changed = self.0.state.send_if_modified(|s| {
            if !self.is_current(generation) {
                return false;
            }
            let next = next_state(s, event);
            let changed = next != *s;
            if changed {
                prev = Some(Status::new(s, None).state);
            }
            *s = next;
            changed
        });
        let now = self.state();
        if let Some(prev) = prev {
            log::info!(target: AUTH, "engine: {prev} → {} ({name})", Status::new(&now, None).state);
        }
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

    /// Stops the running loop (if any) and starts a new one with `creds`, else the live
    /// credentials, else the stored ones (`start_creds`). No credentials → `needs_login`.
    pub async fn restart(&self, creds: Option<Credentials>) {
        let mut task = self.0.task.lock().await;
        let generation = self.end_loop(&mut task).await;
        let live = lock(&self.0.live_creds).clone();
        let stored = if creds.is_none() && live.is_none() {
            let store = self.0.store.clone();
            tokio::task::spawn_blocking(move || store.load()).await.ok().flatten()
        } else {
            None
        };
        let source = if creds.is_some() { "new login" } else if live.is_some() { "live login" } else { "stored" };
        let creds = start_creds(creds, live, stored);
        match creds {
            Some(_) => log::info!(target: AUTH, "credentials: found ({source})"),
            None => log::info!(target: AUTH, "credentials: none, login needed"),
        }
        self.apply(generation, Event::Start { has_credentials: creds.is_some() });
        if let Some(creds) = creds {
            *task = Some(tauri::async_runtime::spawn(run(self.clone(), generation, creds)));
        }
    }

    /// The live login's account, else the stored login's (no engine run needed). Non-empty only.
    pub async fn stored_account(&self) -> Option<String> {
        let live = lock(&self.0.live_creds).as_ref().and_then(|c| c.username.clone());
        if let Some(a) = live.filter(|a| !a.is_empty()) {
            return Some(a);
        }
        let store = self.0.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.load()).await.ok().flatten()?;
        stored.username.filter(|a| !a.is_empty())
    }

    /// `restart(None)` under `auth_op`: it can't run inside a logout and load the old account.
    pub async fn restart_stored(&self) {
        let _op = self.0.auth_op.lock().await;
        self.restart(None).await;
    }

    /// Retires the running loop (if any) and waits up to 3 s for it to end. Returns the new generation.
    async fn end_loop(&self, task: &mut Option<JoinHandle<()>>) -> u64 {
        let generation = self.retire();
        if let Some(mut old) = task.take() {
            // let Spirc say goodbye to Spotify; a loop asleep in its backoff is just cut
            if tokio::time::timeout(Duration::from_secs(3), &mut old).await.is_err() {
                old.abort();
            }
        }
        generation
    }

    /// Stops the loop and Spirc (playback stops). The engine then waits in `needs_login`.
    async fn stop(&self) {
        let mut task = self.0.task.lock().await;
        let generation = self.end_loop(&mut task).await;
        // after the bump: a late `remember_live` of the old loop sees it is stale
        lock(&self.0.live_creds).take();
        self.apply(generation, Event::Start { has_credentials: false });
    }

    /// Stops the engine, then removes the account's data: the player login, the list cache,
    /// the home-feed and known-mixes store keys, the saved session, the MCP key. A failed
    /// step is logged and the next steps still run; the first error is the result.
    pub async fn logout(&self) -> Result<(), String> {
        log::info!(target: AUTH, "logout: start");
        // bumped before the lock: a login that waits for it sees the logout
        self.0.login_gen.fetch_add(1, Ordering::SeqCst);
        let _op = self.0.auth_op.lock().await;
        self.stop().await;
        let store = self.0.store.clone();
        let session = self.0.session.clone();
        let results = vec![
            ("credentials", blocking(move || store.clear()).await),
            ("list cache", blocking(crate::cache::clear_all).await),
            ("saved state", blocking(|| crate::store::remove(&[crate::library::HOME_KEY, crate::library::KNOWN_KEY])).await),
            ("session", blocking(move || session.forget()).await),
            ("MCP", crate::mcp::disable_for_logout().await),
        ];
        // the next login is a new start: it restores that account's session
        self.0.restore_tried.store(false, Ordering::SeqCst);
        self.0.reload_after_drop.store(false, Ordering::SeqCst);
        *lock(&self.0.restore_pending) = None;
        let result = first_error(results);
        match &result {
            Ok(()) => log::info!(target: AUTH, "logged out"),
            Err(e) => log::warn!(target: AUTH, "logged out, with an error: {e}"),
        }
        result
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
            .map_err(|_| {
                log::warn!(target: AUTH, "login: no ready engine after {} s", READY_TIMEOUT.as_secs());
                format!("the player didn't connect in {} s", READY_TIMEOUT.as_secs())
            })?
            .map_err(|e| e.to_string())?
            .clone();
        match settled {
            State::Ready => Ok(()),
            State::NeedsLogin => {
                log::warn!(target: AUTH, "login: refused by Spotify");
                Err("Spotify refused the player login".into())
            }
            State::Failed(reason) => {
                log::warn!(target: AUTH, "login: engine failed: {reason}");
                Err(reason)
            }
            State::Starting | State::Reconnecting => unreachable!(),
        }
    }
}

/// The credentials a restart starts with: `given`, else `live`, else `stored`.
fn start_creds(given: Option<Credentials>, live: Option<Credentials>, stored: Option<Credentials>) -> Option<Credentials> {
    given.or(live).or(stored)
}

/// Keeps `creds` as the live credentials while `generation` is current. Checked under the
/// state lock, where `retire` bumps the generation: a retired loop never writes.
fn remember_live(engine: &Engine, generation: u64, creds: &Credentials) {
    engine.0.state.send_if_modified(|_| {
        if engine.is_current(generation) {
            *lock(&engine.0.live_creds) = Some(creds.clone());
        }
        false
    });
}

/// The account type from `get`, polled every 50 ms until it is there or `wait` is over.
/// librespot gives no signal when ProductInfo arrives: its attributes are a plain read.
async fn account_type(get: impl Fn() -> Option<String>, wait: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if let Some(t) = get() {
            return Some(t);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(50))).await;
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
            log::warn!(target: AUTH, "setup failed: no device id: {e}");
            engine.apply(generation, Event::Fatal(format!("no device id: {e}")));
            return;
        }
    };
    let mixer: Arc<dyn Mixer> = match SoftMixer::open(MixerConfig::default()) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            log::warn!(target: AUTH, "setup failed: no volume control: {e}");
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
    // a source that played to its end is loaded back, paused, on its first track
    let on_finished = {
        let engine = engine.clone();
        move || {
            let device_id = engine.device_id();
            tauri::async_runtime::spawn(restore(engine.clone(), device_id, Restore::Finished));
        }
    };
    tauri::async_runtime::spawn(session::listen(tracker.clone(), player.get_player_event_channel(), on_finished));
    let now_playing = engine.0.now.clone();
    now_playing.set_volume(tracker.volume());
    tauri::async_runtime::spawn(nowplaying::listen(now_playing.clone(), player.get_player_event_channel()));

    let mut attempt = 0;
    let mut type_logged = false;
    loop {
        // registered before Spirc starts the dealer and does its first PUT, so no update of
        // this session is missed. Dealer and PUT clusters both feed `on_cluster` and the
        // restore gate; only dealer clusters feed `follow_cluster` (see the select arm)
        let mut cluster = futures_util::stream::select(
            cluster_updates(&session).map(|u| (u, false)),
            put_clusters(&session).map(|u| (u, true)),
        );
        let mut connect_ups = connection_ids(&session);
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
                {
                    let (t, account) = (tracker.clone(), session.username());
                    let _ = tokio::task::spawn_blocking(move || t.use_account(&account)).await;
                }
                let waited = Instant::now();
                let account_type = account_type(|| session.get_user_attribute("type"), ACCOUNT_TYPE_WAIT).await;
                if !type_logged {
                    let ms = waited.elapsed().as_millis();
                    log::info!(target: AUTH, "account type: {} (after {ms} ms)", account_type.as_deref().unwrap_or("unknown"));
                    type_logged = true;
                }
                match premium_from_attr(account_type.as_deref()) {
                    Premium::No => {
                        engine.apply(generation, Event::Fatal(PREMIUM_REQUIRED.into()));
                        engine.stop_spirc();
                        spirc_task.await;
                        return;
                    }
                    Premium::Unknown => log::warn!(target: AUTH, "no account type from Spotify: the Premium check is skipped"),
                    Premium::Yes => {}
                }
                // after the Premium check: a Free account's login is never stored or kept
                let (kept, save_error) = keep_reusable(&engine, &session, creds).await;
                creds = kept;
                remember_live(&engine, generation, &creds);
                if save_error.is_some() {
                    *lock(&engine.0.save_error) = save_error;
                }
                match engine.apply(generation, Event::Connected) {
                    State::Ready if !engine.0.restore_tried.swap(true, Ordering::SeqCst) => {
                        engine.0.reload_after_drop.store(false, Ordering::SeqCst);
                        *lock(&engine.0.restore_pending) = None;
                        tauri::async_runtime::spawn(restore(engine.clone(), session.device_id().to_string(), Restore::Launch));
                    }
                    State::Ready if engine.0.reload_after_drop.swap(false, Ordering::SeqCst) => {
                        tauri::async_runtime::spawn(restore(engine.clone(), session.device_id().to_string(), Restore::Reconnect));
                    }
                    _ => {}
                }
                // a restore still waiting for a cluster runs anyway once this fires;
                // armed when the dealer has its connection id (`connect_ups`), not before
                let quiet = tokio::time::sleep(QUIET_RESTORE_AFTER);
                tokio::pin!(quiet);
                let (mut quiet_armed, mut quiet_done) = (false, false);
                // "This Mac was active", kept past Spirc's own disconnect at a drop (`active_after`);
                // the Spirc task has not run yet, so this channel sees all its events
                let mut events = player.get_player_event_channel();
                let mut was_here = now_playing.engine_active();
                // an interval, not a sleep per pass: frequent events must not keep pushing the checks back
                let mut health = tokio::time::interval(Duration::from_secs(5));
                health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                health.tick().await;
                tokio::pin!(spirc_task);
                let mut why = "the Spirc task stopped";
                // the Spirc task ends on shutdown or session loss; a dead player thread is fatal
                loop {
                    tokio::select! {
                        _ = &mut spirc_task => break,
                        Some(event) = events.recv() => was_here = active_after(was_here, &event, session.is_invalid()),
                        Some((update, from_put)) = cluster.next() => {
                            if let Ok(update) = update {
                                // not for a PUT cluster: a PUT response echoes This Mac's own state
                                // back, and `follow_cluster` would take Spirc's context for one
                                // another client loaded (e.g. our own track-list load's context);
                                // another client's load comes as a dealer update
                                if !from_put {
                                    follow_cluster(&tracker, &update, session.device_id());
                                }
                                now_playing.on_cluster(&update, session.device_id());
                                // the restore that found no Connect state runs once, now
                                if let Some(why) = lock(&engine.0.restore_pending).take() {
                                    tauri::async_runtime::spawn(restore(engine.clone(), session.device_id().to_string(), why));
                                }
                            }
                        }
                        Some(_) = connect_ups.next() => {
                            if now_playing.connect_up() {
                                log::info!(target: "stylus::session", "Connect is up (dealer connection id): the quiet clock starts");
                                quiet.as_mut().reset(tokio::time::Instant::now() + QUIET_RESTORE_AFTER);
                                quiet_armed = true;
                            }
                        }
                        _ = &mut quiet, if quiet_armed && !quiet_done => {
                            quiet_done = true;
                            if now_playing.cluster().is_none() {
                                log::info!(target: "stylus::session", "no Connect cluster {} s after Connect came up: no other device is active", QUIET_RESTORE_AFTER.as_secs());
                                if let Some(why) = lock(&engine.0.restore_pending).take() {
                                    tauri::async_runtime::spawn(restore(engine.clone(), session.device_id().to_string(), why));
                                }
                            }
                        }
                        _ = health.tick() => {
                            if player.is_invalid() {
                                log::warn!(target: AUTH, "audio player gone: Fatal");
                                engine.apply(generation, Event::Fatal("the audio player stopped".into()));
                                engine.stop_spirc();
                                return;
                            }
                            // a dead session doesn't always end the Spirc task (a paused player sends
                            // nothing, the dealer's token refresh can fail while offline): reconnect anyway
                            if session.is_invalid() {
                                log::warn!(target: AUTH, "session invalid: reconnecting");
                                why = "session invalid";
                                engine.stop_spirc();
                                let _ = tokio::time::timeout(Duration::from_secs(3), &mut spirc_task).await;
                                break;
                            }
                        }
                    }
                }
                log::info!(target: AUTH, "Spirc ended: {why}");
                engine.0.spirc.lock().unwrap().take();
                // the player outlives the Spirc: stop what it buffered, or the old track keeps
                // playing under a new Spirc that has no request id for it and can't pause it
                player.stop();
                if up_since.elapsed() >= STABLE_AFTER && attempt > 0 {
                    log::info!(target: AUTH, "reconnect backoff reset after {} s up", up_since.elapsed().as_secs());
                    attempt = 0;
                }
                // a drop, not a stop (restart, logout: the generation moved on)
                if was_here && engine.is_current(generation) {
                    engine.0.reload_after_drop.store(true, Ordering::SeqCst);
                }
                engine.apply(generation, Event::Dropped);
            }
            Err(e) => {
                // logged before `classify` drops the message (AuthRejected keeps none)
                let event = classify(e.kind, &e.to_string());
                match &event {
                    Event::Dropped => log::info!(target: AUTH, "connect failed, Dropped: {e}"),
                    other => log::warn!(target: AUTH, "connect failed, {}: {e}", event_name(other)),
                }
                if event == Event::Dropped {
                    engine.apply(generation, Event::Dropped);
                } else {
                    engine.apply(generation, event);
                    return;
                }
            }
        }
        if !engine.is_current(generation) {
            return;
        }
        log::info!(target: AUTH, "reconnect {attempt} in {} s", backoff(attempt).as_secs());
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

/// Whether This Mac is still the active device after `event`. Spirc's cleanup after a lost
/// session (dead session) also sends SessionDisconnected: that one is a drop, the device was
/// active, so it doesn't count.
fn active_after(active: bool, event: &PlayerEvent, session_dead: bool) -> bool {
    match event {
        PlayerEvent::SessionConnected { .. } => true,
        PlayerEvent::SessionDisconnected { .. } => active && session_dead,
        _ => active,
    }
}

/// Connect cluster updates (the account's devices and the active one's player state).
/// Spirc listens too; the dealer hands each update to every listener. The cluster Spotify
/// sends back for each connect-state PUT is that PUT's HTTP response, not a dealer message:
/// `put_clusters` gives those. With no other device active, the first dealer update can come
/// late or never (`cluster_or_quiet`, `restore_gate`).
fn cluster_updates(session: &Session) -> BoxedStreamResult<ClusterUpdate> {
    match session.dealer().listen_for("hm://connect-state/v1/cluster", Message::from_raw::<ClusterUpdate>) {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!(target: "stylus::session", "no cluster updates, loads by other clients keep their first track only: {e}");
            Box::pin(futures_util::stream::pending())
        }
    }
}

/// The cluster of each connect-state PUT Spirc makes (its HTTP response; vendored
/// librespot-core patch 4, `SpClient::connect_state_responses`). Spirc's first PUT comes right
/// after the dealer's connection id, so this is usually the session's first cluster. Subscribed
/// before Spirc starts: a new Session has a new SpClient, and the receiver never gives a body
/// sent before it subscribed. A body that is not a cluster is skipped (logged once per session).
fn put_clusters(session: &Session) -> BoxedStreamResult<ClusterUpdate> {
    let bodies = session.spclient().connect_state_responses();
    Box::pin(futures_util::stream::unfold((bodies, false), |(mut bodies, mut logged)| async move {
        loop {
            // an error: the SpClient (the session) is gone
            bodies.changed().await.ok()?;
            let Some(body) = bodies.borrow_and_update().clone() else { continue };
            match cluster_from_put(&body) {
                Some(update) => return Some((Ok(update), (bodies, logged))),
                None if !logged => {
                    logged = true;
                    log::info!(target: "stylus::session", "a connect-state PUT response ({} bytes) is not a cluster: skipped", body.len());
                }
                None => {}
            }
        }
    }))
}

/// A connect-state PUT response body as a cluster update. None when it doesn't parse, or
/// when it is empty: an empty body parses as an empty Cluster, which would hide the real one.
fn cluster_from_put(body: &[u8]) -> Option<ClusterUpdate> {
    use protobuf::Message as _;
    if body.is_empty() {
        return None;
    }
    let cluster = librespot_protocol::connect::Cluster::parse_from_bytes(body).ok()?;
    Some(ClusterUpdate { cluster: Some(cluster).into(), ..Default::default() })
}

/// The dealer's connection id messages: Spirc does its first connect-state PUT on the first
/// one, so Connect is up from then on (the quiet clock, `NowPlaying::connect_up`). The dealer
/// hands each message to every listener: Spirc still gets its own. With no listener, the
/// stream fires at once: the clock starts at the session, as a fallback.
fn connection_ids(session: &Session) -> BoxedStreamResult<()> {
    match session.dealer().listen_for("hm://pusher/v1/connections/", |_| Ok(())) {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!(target: "stylus::session", "no connection id updates, the quiet clock starts at the session: {e}");
            Box::pin(futures_util::stream::once(async { Ok(()) }))
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

/// Why `restore` runs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Restore {
    /// The first ready of the launch (or of a new login).
    Launch,
    /// The source played to its end (session.rs `finish`): load it back on its first track.
    Finished,
    /// The connection dropped while this Mac was the active device.
    Reconnect,
}

/// Loads the session back, paused (`Restore` says why). Skipped when there is none, when
/// another device is playing, and at launch when this player already has a track. At launch
/// with no saved session it loads Liked Songs (`load_default`). It never starts sound.
async fn restore(engine: Engine, device_id: String, why: Restore) {
    const LOG: &str = "stylus::session";
    let tracker = engine.0.session.clone();
    let saved = tracker.current();
    if saved.is_none() && why != Restore::Launch {
        log::info!(target: LOG, "reload ({why:?}) skipped: nothing loaded");
        return;
    }
    if why == Restore::Launch && tracker.has_track() {
        log::info!(target: LOG, "restore skipped: the player already has a track");
        return;
    }
    // another device playing must not be interrupted: Spotify's own device state (the Connect
    // cluster) tells, and its first update usually arrives within seconds of connecting.
    // No wait once the gate can tell (a retry from `restore_pending` comes with a cluster or quiet).
    let gate = || restore_gate(engine.0.now.cluster().as_deref(), &device_id, engine.0.now.since_session());
    for _ in 0..12 {
        if gate() != Gate::NoState {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if gate() == Gate::NoState {
        *lock(&engine.0.restore_pending) = Some(why);
        // a cluster that came in after the last look has already passed the loop's check
        if engine.0.now.cluster().is_none() || lock(&engine.0.restore_pending).take().is_none() {
            log::info!(target: LOG, "restore ({why:?}) waits for the first Connect state");
            return;
        }
    }
    if !may_load(&engine, &device_id, why) {
        return;
    }
    if why == Restore::Launch && tracker.has_track() {
        log::info!(target: LOG, "restore skipped: the player got a track meanwhile");
        return;
    }
    // the user can load or play something while this waited: that wins over the old snapshot
    if superseded(saved.as_ref(), tracker.current().as_ref(), engine.0.now.now().playing) {
        log::info!(target: LOG, "restore ({why:?}): skipped, superseded by a newer load");
        return;
    }
    let Some(saved) = saved else {
        load_default(&engine, &device_id).await;
        return;
    };
    let Some(source) = saved.source.clone() else { return };
    match send_load(&engine, source, saved.track_uri.clone(), saved.position_ms, saved.shuffle, saved.repeat, saved.volume) {
        Ok(()) => {
            log::info!(target: LOG, "restored ({why:?}) {}{}", session::describe(&saved), if saved.finished { ", finished: from the top" } else { "" });
            emit_restored(&engine, &saved);
        }
        Err(e) => log::warn!(target: LOG, "restore ({why:?}) failed: {e}"),
    }
}

/// A restore's snapshot (`before`, read before its wait) is out of date: the session `now` has
/// another source or track, or the player plays.
fn superseded(before: Option<&session::Saved>, now: Option<&session::Saved>, playing: bool) -> bool {
    let key = |s: Option<&session::Saved>| s.map(|s| (s.source.clone(), s.track_uri.clone()));
    playing || key(before) != key(now)
}

/// What the Connect cluster says about a restore load now.
#[derive(Debug, PartialEq)]
enum Gate {
    Load,
    /// Another device plays: a load here would take its music away.
    OtherDevicePlaying,
    /// No cluster yet (or a new session's): can't tell whether another device plays.
    NoState,
    /// No cluster `QUIET_RESTORE_AFTER` after Connect came up: a load may go.
    Quiet,
}

/// The cluster for the UI: the latest one; else, `QUIET_VIEW_AFTER` after Connect came up with none,
/// an empty one (no device active, This Mac alone in the list); else None (ENGINE_NOT_READY).
fn cluster_or_quiet(cluster: Option<Arc<librespot_protocol::connect::Cluster>>, since_session: Option<Duration>) -> Option<Arc<librespot_protocol::connect::Cluster>> {
    match cluster {
        Some(c) => Some(c),
        None if since_session.is_some_and(|d| d >= QUIET_VIEW_AFTER) => Some(Arc::default()),
        None => None,
    }
}

/// `since_session`: the time since Connect came up (`NowPlaying::since_session`); None before.
/// No cluster for `QUIET_RESTORE_AFTER` counts as "no other device is active": Spotify pushes a
/// cluster update to the devices of the account when one of them plays or changes state, so
/// another active device would have sent one by then. Before that, no cluster means "wait":
/// a load could take a phone's music away.
fn restore_gate(cluster: Option<&librespot_protocol::connect::Cluster>, device_id: &str, since_session: Option<Duration>) -> Gate {
    let Some(c) = cluster else {
        return if since_session.is_some_and(|d| d >= QUIET_RESTORE_AFTER) { Gate::Quiet } else { Gate::NoState };
    };
    let state = &c.player_state;
    if !c.active_device_id.is_empty() && c.active_device_id != device_id && state.is_playing && !state.is_paused {
        Gate::OtherDevicePlaying
    } else {
        Gate::Load
    }
}

/// Checks the current cluster right before a restore load (the user can start music on
/// another device while a restore waits). No state: the next cluster update retries, same kind.
fn may_load(engine: &Engine, device_id: &str, why: Restore) -> bool {
    const LOG: &str = "stylus::session";
    match restore_gate(engine.0.now.cluster().as_deref(), device_id, engine.0.now.since_session()) {
        Gate::Load => true,
        Gate::Quiet => {
            log::info!(target: LOG, "restore ({why:?}): no Connect state after {} s, no other device is active", QUIET_RESTORE_AFTER.as_secs());
            true
        }
        Gate::OtherDevicePlaying => {
            log::info!(target: LOG, "restore ({why:?}) skipped: another device is playing");
            false
        }
        Gate::NoState => {
            log::info!(target: LOG, "restore ({why:?}) skipped: no Connect state, can't tell whether another device is playing");
            *lock(&engine.0.restore_pending) = Some(why);
            false
        }
    }
}

/// Sends a paused load to Spirc: activate first (Spirc ignores everything while inactive),
/// then the volume, then the load. Ok only means queued.
fn send_load(engine: &Engine, source: Source, track_uri: Option<String>, position_ms: u32, shuffle: bool, repeat: Repeat, volume: u16) -> Result<(), String> {
    let source = match source {
        Source::Context { context_uri } => LoadSource::Context(context_uri),
        Source::Uris { uris } => LoadSource::Tracks(uris),
    };
    let modes = Modes { shuffle, repeat: repeat == Repeat::Context, repeat_track: repeat == Repeat::Track };
    let request = load_request(source, restore_start(track_uri, shuffle), position_ms, false, modes);
    engine.with_spirc(|s| {
        s.activate()?;
        s.set_volume(volume)?;
        s.load(request)
    })
}

fn emit_restored(engine: &Engine, saved: &session::Saved) {
    if let Some(app) = engine.0.app.get() {
        let _ = app.emit("session-restored", saved.payload());
    }
}

/// How long a Liked Songs context load gets to show its first track before the list is tried.
const DEFAULT_CONTEXT_WAIT: Duration = Duration::from_secs(10);

const DEFAULT_LOG: &str = "stylus::session";

/// No saved session (first launch, after logout): load Liked Songs, paused, on its first
/// track (`session::default_sources`). An empty Liked Songs (by its count, one request): nothing.
/// The context first; the first 200 liked uris are fetched and loaded as a list only when the
/// context shows no track in time (or the account is unknown).
async fn load_default(engine: &Engine, device_id: &str) {
    let tracker = engine.0.session.clone();
    let username = tracker.account().unwrap_or_default();
    match crate::spotify::liked_count().await {
        Ok(0) => {
            log::info!(target: DEFAULT_LOG, "default session skipped: Liked Songs is empty");
            return;
        }
        Ok(_) => {}
        Err(e) => log::warn!(target: DEFAULT_LOG, "default session: Liked Songs count not read ({e}): trying the context"),
    }
    for source in session::default_sources(&username, None) {
        if load_default_source(engine, device_id, &tracker, source).await {
            return;
        }
    }
    let liked = match crate::spotify::saved_tracks(session::DEFAULT_LIST_MAX, None).await {
        Ok(v) => v["tracks"].as_array().into_iter().flatten().filter_map(|t| t["uri"].as_str().map(str::to_string)).collect::<Vec<_>>(),
        Err(e) => {
            log::warn!(target: DEFAULT_LOG, "default session: Liked Songs not read ({e}): no list to load");
            return;
        }
    };
    let list = session::default_sources("", Some(liked));
    if list.is_empty() {
        log::info!(target: DEFAULT_LOG, "default session skipped: Liked Songs is empty");
    }
    for source in list {
        load_default_source(engine, device_id, &tracker, source).await;
    }
}

/// Loads one default `source`, paused. false only when it was a context that showed no track
/// in time (the list is next); true when done (loaded, or nothing more to try).
async fn load_default_source(engine: &Engine, device_id: &str, tracker: &Tracker, source: Source) -> bool {
    if tracker.has_track() {
        log::info!(target: DEFAULT_LOG, "default session skipped: the player got a track meanwhile");
        return true;
    }
    let is_context = matches!(source, Source::Context { .. });
    let what = if is_context { "context" } else { "list" };
    if !may_load(engine, device_id, Restore::Launch) {
        return true;
    }
    if let Err(e) = send_load(engine, source.clone(), None, 0, false, Repeat::Off, tracker.volume()) {
        log::warn!(target: DEFAULT_LOG, "default session: Liked Songs {what} not sent: {e}");
        return true;
    }
    // only what was sent (`Engine::load` records after Ok too)
    tracker.loaded(source, None, 0, false, Repeat::Off);
    if is_context && !track_within(tracker, DEFAULT_CONTEXT_WAIT).await {
        log::warn!(target: DEFAULT_LOG, "default session: the Liked Songs context showed no track in {} s", DEFAULT_CONTEXT_WAIT.as_secs());
        return false;
    }
    log::info!(target: DEFAULT_LOG, "default session: Liked Songs loaded as a {what}, paused");
    if let Some(saved) = tracker.current() {
        emit_restored(engine, &saved);
    }
    true
}

/// Waits up to `wait` for the player's first track event.
async fn track_within(tracker: &Tracker, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        if tracker.has_track() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    tracker.has_track()
}

/// The reusable credentials a connected session got back from Spotify. After the
/// player login these replace the short-lived OAuth token, and they are stored.
/// librespot's own cache keeps exactly these fields; the type is always "stored
/// credentials" (checked against the P0 spike's cache file: auth_type 1).
/// Also gives the save error, if the save failed: the session still works for this launch.
async fn keep_reusable(engine: &Engine, session: &Session, creds: Credentials) -> (Credentials, Option<String>) {
    let reusable = Credentials {
        username: Some(session.username()),
        auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
        auth_data: session.auth_data(),
    };
    store_reusable(engine.0.store.clone(), reusable, creds).await
}

/// Saves `reusable` unless it is empty or the same as `creds`. The credentials to keep using,
/// and the save error.
async fn store_reusable(store: Arc<dyn CredStore>, reusable: Credentials, creds: Credentials) -> (Credentials, Option<String>) {
    if reusable.auth_data.is_empty() {
        log::warn!(target: AUTH, "credentials: not saved, empty auth data");
        return (creds, None);
    }
    if reusable == creds {
        log::info!(target: AUTH, "credentials: unchanged, not written");
        return (creds, None);
    }
    let to_save = reusable.clone();
    let saved = blocking(move || store.save(&to_save)).await;
    match &saved {
        Ok(()) => log::info!(target: AUTH, "credentials: saved"),
        Err(e) => log::warn!(target: AUTH, "could not store the player login in the credentials file: {e}"),
    }
    (reusable, saved.err())
}

/// `engine_login`'s answer: the settle result, then a failed save of the login as an Err
/// (the engine stays logged in for this launch).
fn login_result(settled: Result<(), String>, save_error: Option<String>) -> Result<(), String> {
    settled?;
    match save_error {
        Some(e) => Err(format!("{LOGIN_NOT_SAVED}: {e}")),
        None => Ok(()),
    }
}

/// `f` on the blocking pool; a panic is an Err.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string()).and_then(|r| r)
}

/// Logs each failed step; Err with the first failure (`<step>: <error>`), else Ok.
fn first_error(results: Vec<(&str, Result<(), String>)>) -> Result<(), String> {
    let mut first = None;
    for (step, r) in results {
        if let Err(e) = r {
            log::warn!(target: AUTH, "logout: {step}: {e}");
            first.get_or_insert(format!("{step}: {e}"));
        }
    }
    first.map_or(Ok(()), Err)
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
        log::info!(target: AUTH, "login: refused, LOGIN_IN_PROGRESS");
        return Err("LOGIN_IN_PROGRESS".into());
    }
    log::info!(target: AUTH, "login: start");
    let gen = engine.0.login_gen.load(Ordering::SeqCst);
    let result = async {
        // the browser wait is outside the lock: an open browser login never blocks a logout
        // a logout bumps login_gen: the browser wait stops at once and frees the port
        let e = engine.inner().clone();
        let cancelled = move || e.0.login_gen.load(Ordering::SeqCst) != gen;
        let token = crate::auth::oauth_login(KEYMASTER_CLIENT_ID, LOGIN_PORT, LOGIN_PATH, OAUTH_SCOPES, LOGIN_TIMEOUT, cancelled).await?;
        let _op = engine.0.auth_op.lock().await;
        if engine.0.login_gen.load(Ordering::SeqCst) != gen {
            return Err(crate::auth::LOGIN_CANCELLED.to_string());
        }
        lock(&engine.0.save_error).take();
        engine.restart(Some(Credentials::with_access_token(token.access_token))).await;
        let settled = engine.settled().await;
        login_result(settled, lock(&engine.0.save_error).clone())
    }
    .await;
    engine.0.login_busy.store(false, Ordering::SeqCst);
    match &result {
        Ok(()) => log::info!(target: AUTH, "login: ok"),
        Err(e) if e.starts_with("LOGIN_CANCELLED") => log::info!(target: AUTH, "login: cancelled by logout"),
        Err(e) => log::warn!(target: AUTH, "login: failed: {e}"),
    }
    result
}

/// Stops the player and removes the account's data (`Engine::logout`). The UI then shows the
/// login screen.
#[tauri::command]
pub async fn logout(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.logout().await
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
    let saved = blocking(move || crate::settings::update(|s| std::mem::replace(&mut s.bitrate, kbps)).map(|(old, _)| old)).await;
    match saved {
        Ok(old) => log::info!("quality {old} → {kbps} kbps, restarting the engine"),
        Err(e) => {
            log::warn!("quality {kbps} kbps not saved: {e}");
            return Err(e);
        }
    }
    engine.restart_stored().await;
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

/// The start track of a restore load. No saved track (a finished source) with shuffle on:
/// track 1 by index, as Spirc starts a shuffled load with no start track on a random one.
fn restore_start(track_uri: Option<String>, shuffle: bool) -> Option<PlayingTrack> {
    match track_uri {
        Some(uri) => Some(PlayingTrack::Uri(uri)),
        None if shuffle => Some(PlayingTrack::Index(0)),
        None => None,
    }
}

fn load_request(source: LoadSource, playing_track: Option<PlayingTrack>, position_ms: u32, play: bool, m: Modes) -> LoadRequest {
    let options = LoadRequestOptions {
        start_playing: play,
        seek_to: position_ms,
        playing_track,
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
        let request = load_request(source, track_uri.clone().map(PlayingTrack::Uri), position_ms, play, m);
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
        // logged in, but the save failed: no file, yet the engine runs
        assert_eq!(auth_status_for(false, &Ready), "ok");
        assert_eq!(auth_status_for(false, &Reconnecting), "ok");
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

    /// A store whose save always fails (a full disk, a read-only folder).
    struct FailStore;

    impl CredStore for FailStore {
        fn load(&self) -> Option<Credentials> {
            None
        }
        fn save(&self, _: &Credentials) -> Result<(), String> {
            Err("disk full".into())
        }
        fn clear(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn stored(data: &[u8]) -> Credentials {
        Credentials {
            username: Some("alice".into()),
            auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: data.to_vec(),
        }
    }

    #[tokio::test]
    async fn a_failed_save_keeps_the_login_and_gives_the_error() {
        let token = Credentials::with_access_token("t");
        let (kept, err) = store_reusable(Arc::new(FailStore), stored(&[1]), token.clone()).await;
        assert_eq!(kept, stored(&[1]), "the session's reusable login stays in use");
        assert_eq!(err.as_deref(), Some("disk full"));
        let mem = Arc::new(MemoryStore::default());
        let (kept, err) = store_reusable(mem.clone(), stored(&[1]), token.clone()).await;
        assert_eq!((kept, err), (stored(&[1]), None));
        assert_eq!(mem.load(), Some(stored(&[1])));
        // nothing new to save: no save, no error
        assert_eq!(store_reusable(Arc::new(FailStore), stored(&[]), token.clone()).await, (token, None));
        assert_eq!(store_reusable(Arc::new(FailStore), stored(&[1]), stored(&[1])).await, (stored(&[1]), None));
    }

    // real time, short waits: tokio's `test-util` (time::pause) is not enabled in Cargo.toml
    #[tokio::test]
    async fn account_type_waits_for_product_info() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let get = || (calls.fetch_add(1, Ordering::SeqCst) >= 3).then(|| "free".to_string());
        assert_eq!(account_type(get, ACCOUNT_TYPE_WAIT).await.as_deref(), Some("free"));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn account_type_gives_up_after_the_wait() {
        let wait = Duration::from_millis(300);
        let start = Instant::now();
        assert_eq!(account_type(|| None, wait).await, None);
        let waited = start.elapsed();
        assert!(waited >= wait && waited < wait + Duration::from_millis(500), "{waited:?}");
    }

    #[test]
    fn start_creds_prefers_given_then_live_then_stored() {
        let (g, l, s) = (stored(&[1]), stored(&[2]), stored(&[3]));
        assert_eq!(start_creds(Some(g.clone()), Some(l.clone()), Some(s.clone())), Some(g));
        assert_eq!(start_creds(None, Some(l.clone()), Some(s.clone())), Some(l));
        assert_eq!(start_creds(None, None, Some(s.clone())), Some(s));
        assert_eq!(start_creds(None, None, None), None);
    }

    #[tokio::test]
    async fn a_stored_restart_waits_for_the_auth_lock() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        let before = engine.0.generation.load(Ordering::SeqCst);
        let op = engine.0.auth_op.lock().await;
        let restart = tokio::spawn({
            let engine = engine.clone();
            async move { engine.restart_stored().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(engine.0.generation.load(Ordering::SeqCst), before, "no restart while auth_op is held");
        drop(op);
        restart.await.unwrap();
        assert_eq!(engine.0.generation.load(Ordering::SeqCst), before + 1);
        assert_eq!(engine.state(), NeedsLogin);
    }

    #[test]
    fn login_reports_a_failed_save() {
        assert_eq!(login_result(Ok(()), None), Ok(()));
        assert_eq!(login_result(Ok(()), Some("disk full".into())), Err("logged in, but the login could not be saved: disk full".into()));
        assert_eq!(login_result(Err("refused".into()), Some("disk full".into())), Err("refused".into()));
    }

    #[test]
    fn first_error_keeps_the_first_failure() {
        assert_eq!(first_error(vec![("a", Ok(())), ("b", Ok(()))]), Ok(()));
        assert_eq!(first_error(vec![("a", Ok(())), ("b", Err("x".into())), ("c", Err("y".into()))]), Err("b: x".into()));
    }

    /// In the test app folder (`paths::app_dir`), never the real one.
    #[tokio::test]
    async fn logout_removes_the_account_data() {
        let _t = crate::mcp::SETTINGS_TEST.lock().await;
        let dir = crate::paths::app_dir();
        assert!(dir.to_string_lossy().contains("stylus-test-appdir-"), "{}", dir.display());
        let creds = Credentials {
            username: Some("alice".into()),
            auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: vec![1, 2, 3],
        };
        FileStore.save(&creds).unwrap();
        crate::cache::lists().put("alice", "liked", &serde_json::json!([1]));
        crate::store::store_set(crate::library::HOME_KEY.into(), serde_json::json!({"account": "alice"})).unwrap();
        crate::store::store_set(crate::library::KNOWN_KEY.into(), serde_json::json!(["spotify:playlist:x"])).unwrap();
        crate::settings::update(|s| {
            s.mcp_enabled = true;
            s.mcp_key = Some("old".into());
        })
        .unwrap();
        let engine = Engine::new(Arc::new(FileStore));
        assert_eq!(engine.stored_account().await.as_deref(), Some("alice"), "from the file, before the player connects");
        engine.0.session.use_account("alice");
        engine.0.session.loaded(Source::Uris { uris: vec!["spotify:track:a".into()] }, None, 0, false, Repeat::Off);
        engine.0.session.save_if_due();
        assert!(dir.join("player-credentials.json").exists() && dir.join("cache").exists() && dir.join("session.json").exists());
        engine.0.restore_tried.store(true, Ordering::SeqCst);
        engine.0.reload_after_drop.store(true, Ordering::SeqCst);
        *lock(&engine.0.restore_pending) = Some(Restore::Launch);
        *lock(&engine.0.live_creds) = Some(creds.clone());
        assert_eq!(engine.stored_account().await.as_deref(), Some("alice"));

        engine.logout().await.unwrap();

        assert!(!dir.join("player-credentials.json").exists());
        assert!(!dir.join("cache").exists());
        assert!(!dir.join("session.json").exists());
        assert_eq!(crate::store::get(crate::library::HOME_KEY), None);
        assert_eq!(crate::store::get(crate::library::KNOWN_KEY), None);
        let s = crate::settings::load();
        assert_eq!((s.mcp_enabled, s.mcp_key), (false, None));
        assert_eq!(engine.state(), NeedsLogin);
        assert_eq!(engine.auth_status(), "login");
        assert!(!engine.0.restore_tried.load(Ordering::SeqCst), "the next login restores again");
        assert!(!engine.0.reload_after_drop.load(Ordering::SeqCst), "no reload of the old session after a logout");
        assert!(lock(&engine.0.restore_pending).is_none(), "no late restore of the old account");
        assert!(lock(&engine.0.live_creds).is_none(), "no restart with the old account's login");
        assert_eq!(engine.stored_account().await, None, "me_id has no account after a logout");
        // a second logout finds nothing to remove: still Ok
        engine.logout().await.unwrap();
    }

    #[test]
    fn restore_loads_only_when_no_other_device_plays() {
        use librespot_protocol::connect::Cluster;
        let cluster = |active: &str, playing: bool, paused: bool| {
            let mut c = Cluster::new();
            c.active_device_id = active.into();
            let ps = c.player_state.mut_or_insert_default();
            ps.is_playing = playing;
            ps.is_paused = paused;
            c
        };
        let s = |secs: u64| Some(Duration::from_secs(secs));
        let just_before = Some(QUIET_RESTORE_AFTER - Duration::from_millis(1));
        assert_eq!(restore_gate(None, "mac", s(0)), Gate::NoState, "no cluster yet: wait, never guess");
        assert_eq!(restore_gate(None, "mac", None), Gate::NoState, "Connect not up yet (no connection id)");
        assert_eq!(restore_gate(None, "mac", just_before), Gate::NoState, "still waiting for a cluster");
        assert_eq!(restore_gate(None, "mac", Some(QUIET_RESTORE_AFTER)), Gate::Quiet, "no cluster for 5 s: nothing else is active");
        assert_eq!(restore_gate(Some(&cluster("phone", true, false)), "mac", s(30)), Gate::OtherDevicePlaying, "a cluster always wins");
        assert_eq!(restore_gate(Some(&cluster("", false, false)), "mac", s(1)), Gate::Load, "no active device");
        assert_eq!(restore_gate(Some(&cluster("mac", true, false)), "mac", s(1)), Gate::Load, "this Mac is the active one");
        assert_eq!(restore_gate(Some(&cluster("phone", true, false)), "mac", s(1)), Gate::OtherDevicePlaying);
        assert_eq!(restore_gate(Some(&cluster("phone", true, true)), "mac", s(1)), Gate::Load, "the phone is paused");
        assert_eq!(restore_gate(Some(&cluster("phone", false, false)), "mac", s(1)), Gate::Load, "the phone is stopped");
    }

    fn put_body(active: &str, playing: bool) -> Vec<u8> {
        use librespot_protocol::connect::{Cluster, DeviceInfo};
        use protobuf::Message as _;
        let mut c = Cluster::new();
        c.active_device_id = active.into();
        c.device.insert(active.into(), DeviceInfo { device_id: active.into(), name: "Phone".into(), ..Default::default() });
        c.player_state.mut_or_insert_default().is_playing = playing;
        c.write_to_bytes().unwrap()
    }

    #[test]
    fn cluster_from_put_parses_a_cluster() {
        let update = cluster_from_put(&put_body("phone", true)).expect("a cluster");
        assert_eq!(update.cluster.active_device_id, "phone");
        assert_eq!(update.cluster.device["phone"].name, "Phone");
        assert!(update.cluster.player_state.is_playing);
    }

    #[test]
    fn put_clusters_skips_a_bad_body() {
        // field 15 with wire type 7: no such wire type
        assert!(cluster_from_put(&[0xff, 0x00]).is_none());
        // an empty body parses as an empty Cluster: it says nothing, so it is skipped
        assert!(cluster_from_put(&[]).is_none());
    }

    #[test]
    fn put_cluster_blocks_restore() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        let update = cluster_from_put(&put_body("phone", true)).unwrap();
        engine.0.now.on_cluster(&update, "mac");
        assert_eq!(restore_gate(engine.0.now.cluster().as_deref(), "mac", None), Gate::OtherDevicePlaying);
        assert!(!may_load(&engine, "mac", Restore::Launch), "the phone keeps its music");
    }

    #[test]
    fn restore_skips_a_load_made_meanwhile() {
        let saved = |context: &str, track: &str, position_ms: u32| session::Saved {
            account: "alice".into(),
            source: Some(Source::Context { context_uri: context.into() }),
            track_uri: Some(track.into()),
            position_ms,
            shuffle: false,
            repeat: Repeat::Off,
            volume: session::DEFAULT_VOLUME,
            saved_at: 0,
            finished: false,
        };
        let before = saved("spotify:album:a", "spotify:track:1", 1000);
        let other = saved("spotify:album:b", "spotify:track:1", 1000);
        let next = saved("spotify:album:a", "spotify:track:2", 0);
        let moved = saved("spotify:album:a", "spotify:track:1", 9000);
        assert!(superseded(Some(&before), Some(&other), false), "a new source");
        assert!(superseded(Some(&before), Some(&next), false), "a new track");
        assert!(superseded(Some(&before), Some(&before), true), "same source, playing");
        assert!(!superseded(Some(&before), Some(&moved), false), "same source paused: only the position moved");
        assert!(superseded(None, Some(&before), false), "something loaded where there was nothing");
        assert!(!superseded(None, None, false));
    }

    #[test]
    fn no_state_keeps_the_reconnect() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        assert!(!may_load(&engine, "mac", Restore::Reconnect), "no cluster, Connect not up: can't tell");
        assert_eq!(*lock(&engine.0.restore_pending), Some(Restore::Reconnect));
        assert!(!may_load(&engine, "mac", Restore::Finished));
        assert_eq!(*lock(&engine.0.restore_pending), Some(Restore::Finished));
    }

    #[tokio::test]
    async fn default_source_not_recorded_when_send_fails() {
        let engine = Engine::new(Arc::new(MemoryStore::default()));
        let tracker = engine.0.session.clone();
        tracker.use_account("default-source-test-account");
        // a cluster with no active device: the gate lets the load go
        engine.0.now.on_cluster(&ClusterUpdate::new(), "mac");
        assert!(may_load(&engine, "mac", Restore::Launch));
        assert_eq!(engine.state(), Starting, "no Spirc: send_load fails");
        let liked = Source::Context { context_uri: "spotify:user:x:collection".into() };
        assert!(load_default_source(&engine, "mac", &tracker, liked).await);
        assert!(tracker.current().is_none(), "a load that was not sent is not the session");
        assert!(!tracker.is_due(), "nothing to save");
    }

    #[test]
    fn ui_sees_nothing_active_when_no_cluster_comes() {
        use librespot_protocol::connect::Cluster;
        let s = |secs: u64| Some(Duration::from_secs(secs));
        assert!(cluster_or_quiet(None, None).is_none(), "Connect not up yet (no connection id)");
        assert!(cluster_or_quiet(None, Some(QUIET_VIEW_AFTER - Duration::from_millis(1))).is_none(), "the first cluster may still come");
        let quiet = cluster_or_quiet(None, Some(QUIET_VIEW_AFTER)).expect("an empty cluster after 4 s");
        assert!(quiet.active_device_id.is_empty() && quiet.device.is_empty());
        assert_eq!(crate::pb::cluster_state(&quiet, 0), None, "nothing active");
        let own = crate::internal::with_own_device(crate::pb::devices(&quiet), "mac", &quiet.active_device_id, 50);
        assert_eq!(own.len(), 1, "This Mac alone");
        assert_eq!(own[0]["id"], "mac");
        assert_eq!(own[0]["is_active"], false);
        let mut real = Cluster::new();
        real.active_device_id = "phone".into();
        assert_eq!(cluster_or_quiet(Some(Arc::new(real)), s(0)).unwrap().active_device_id, "phone", "a real cluster wins");
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
        let req = load_request(LoadSource::Tracks(uris(2)), Some(PlayingTrack::Uri("spotify:track:1".into())), 4200, false, Modes::default());
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
    fn a_finished_shuffled_restore_starts_on_track_1() {
        let req = load_request(LoadSource::Context("spotify:album:a".into()), restore_start(None, true), 0, false, modes(Some(true), None));
        let dbg = format!("{req:?}");
        assert!(dbg.contains("Index(0)") && dbg.contains("shuffle: true"), "{dbg}");
        assert!(matches!(restore_start(None, false), None), "no shuffle: Spirc starts on track 1 by itself");
        assert!(matches!(restore_start(Some("spotify:track:9".into()), true), Some(PlayingTrack::Uri(u)) if u == "spotify:track:9"));
    }

    #[test]
    fn a_drop_keeps_this_mac_active() {
        let connected = PlayerEvent::SessionConnected { connection_id: "c".into(), user_name: "u".into() };
        let disconnected = PlayerEvent::SessionDisconnected { connection_id: "c".into(), user_name: "u".into() };
        assert!(active_after(false, &connected, false));
        // Spirc's cleanup after a lost session: still "was active", the next ready reloads
        assert!(active_after(true, &disconnected, true));
        // another device took over (live session): no longer active
        assert!(!active_after(true, &disconnected, false));
        assert!(!active_after(false, &disconnected, true));
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
