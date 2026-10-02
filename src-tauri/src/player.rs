//! The app's own Spotify Connect speaker "The Run" (librespot): Session + Player +
//! SoftMixer + Spirc, kept alive by a reconnect loop. The UI still controls playback
//! through the Web API (spotify.rs); to it, "The Run" is one more device id.
//!
//! The player needs its own login: Spotify's keymaster client id, not the app's
//! (the app's token logs librespot in, but every audio fetch fails, P0 spike).
//! librespot's reusable credentials live in the credentials file. They are never logged.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{authentication::Credentials, config::DeviceType, error::ErrorKind, Session, SessionConfig};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, PlayerConfig},
    mixer::{softmixer::SoftMixer, Mixer, MixerConfig},
    player::Player,
};
use librespot_protocol::authentication::AuthenticationType;
use serde::Serialize;
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter, State as Managed};
use tokio::sync::watch;

pub const DEVICE_NAME: &str = "The Run";
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

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    NeedsLogin,
    Starting,
    Ready,
    Reconnecting,
    Failed(String),
    /// The player and the app are logged in to different accounts. The engine stops.
    AccountMismatch(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The engine (re)starts, with or without credentials to log in with.
    Start { has_credentials: bool },
    /// Spirc is up as `player`. `app` is the Web API account; None = unknown, check skipped.
    Connected { player: String, app: Option<String> },
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
        (Starting | Reconnecting, Event::Connected { player, app }) => match app {
            Some(app) if !app.eq_ignore_ascii_case(&player) => {
                AccountMismatch(format!("the player is logged in as {player}, the app as {app}"))
            }
            _ => Ready,
        },
        (Starting | Ready | Reconnecting, Event::Dropped) => Reconnecting,
        (Starting | Ready | Reconnecting, Event::AuthRejected) => NeedsLogin,
        (Starting | Ready | Reconnecting, Event::Fatal(reason)) => Failed(reason),
        // a stopped engine (needs_login, failed, account_mismatch) only leaves through Start
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
}

impl From<&State> for Status {
    fn from(s: &State) -> Self {
        let (state, reason) = match s {
            State::NeedsLogin => ("needs_login", None),
            State::Starting => ("starting", None),
            State::Ready => ("ready", None),
            State::Reconnecting => ("reconnecting", None),
            State::Failed(r) => ("failed", Some(r.clone())),
            State::AccountMismatch(r) => ("account_mismatch", Some(r.clone())),
        };
        Status { state, name: DEVICE_NAME, reason }
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
        Event::Fatal("Spotify Premium required".into())
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

// ---- credential storage ----------------------------------------------------

/// Where librespot's reusable credentials live: a private file in the app, memory in tests.
pub trait CredStore: Send + Sync {
    fn load(&self) -> Option<Credentials>;
    fn save(&self, creds: &Credentials) -> Result<(), String>;
}

/// `player-credentials.json` next to the app's `tokens.json`, readable by this user only
/// (0600). Not the Keychain: unsigned builds count as a new app after every rebuild, so
/// macOS asked for Keychain access again each time (user chose the file, 2026-10-02).
pub struct FileStore;

impl FileStore {
    fn path() -> std::path::PathBuf {
        crate::auth::app_dir().join("player-credentials.json")
    }
}

impl CredStore for FileStore {
    fn load(&self) -> Option<Credentials> {
        serde_json::from_str(&std::fs::read_to_string(Self::path()).ok()?).ok()
    }

    fn save(&self, creds: &Credentials) -> Result<(), String> {
        let json = serde_json::to_string(creds).map_err(|e| e.to_string())?;
        crate::auth::write_private(&Self::path(), &json).map_err(|e| e.to_string())
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
}

impl Engine {
    pub fn new(store: Arc<dyn CredStore>) -> Self {
        Engine(Arc::new(Inner {
            state: watch::Sender::new(State::Starting),
            generation: AtomicU64::new(0),
            app: OnceLock::new(),
            store,
            spirc: Mutex::new(None),
            task: tokio::sync::Mutex::new(None),
            login_busy: AtomicBool::new(false),
        }))
    }

    /// Where `engine-status` events go. Set once, in `setup`.
    pub fn attach(&self, app: AppHandle) {
        let _ = self.0.app.set(app);
    }

    fn state(&self) -> State {
        self.0.state.borrow().clone()
    }

    /// Applies `event` from the loop of generation `generation`; a stale loop changes
    /// nothing. Emits `engine-status` when the state changes.
    fn apply(&self, generation: u64, event: Event) -> State {
        let changed = self.0.state.send_if_modified(|s| {
            if generation != self.0.generation.load(Ordering::SeqCst) {
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
                let _ = app.emit("engine-status", Status::from(&now));
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
    pub fn shutdown(&self) {
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
            State::Failed(reason) | State::AccountMismatch(reason) => Err(reason),
            State::Starting | State::Reconnecting => unreachable!(),
        }
    }
}

fn connect_config() -> ConnectConfig {
    ConnectConfig {
        name: DEVICE_NAME.into(),
        device_type: DeviceType::Computer,
        initial_volume: u16::MAX / 2,
        ..ConnectConfig::default()
    }
}

/// The Web API account (`/me`), with the app's token. None when the app isn't
/// logged in or the call fails: the account and Premium checks are then skipped.
async fn app_account() -> Option<serde_json::Value> {
    let token = crate::auth::valid_access_token().await.ok()?;
    let res = crate::auth::http().get("https://api.spotify.com/v1/me").bearer_auth(token).send().await.ok()?;
    res.error_for_status().ok()?.json().await.ok()
}

async fn app_user_id() -> Option<String> {
    app_account().await?["id"].as_str().map(str::to_string)
}

/// A known non-Premium account. librespot refuses those (upstream even exits the process;
/// our vendored copy only logs), so the engine must not start for them.
fn premium_missing(me: &serde_json::Value) -> bool {
    matches!(me["product"].as_str(), Some(p) if p != "premium")
}

/// The connect loop of one engine generation: Session → Spirc → wait for it to end →
/// reconnect with backoff. Player and mixer live for the whole loop.
async fn run(engine: Engine, generation: u64, mut creds: Credentials) {
    if app_account().await.is_some_and(|me| premium_missing(&me)) {
        engine.apply(generation, Event::Fatal("Spotify Premium is required to play on this Mac".into()));
        return;
    }
    // one device id for this loop, so a reconnect keeps the same Connect device
    let session_config = SessionConfig::default();
    let mixer: Arc<dyn Mixer> = match SoftMixer::open(MixerConfig::default()) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            engine.apply(generation, Event::Fatal(format!("no volume control: {e}")));
            return;
        }
    };
    let Some(backend) = audio_backend::find(None) else {
        engine.apply(generation, Event::Fatal("no audio output".into()));
        return;
    };
    let mut session = Session::new(session_config.clone(), None);
    let player = Player::new(PlayerConfig::default(), session.clone(), mixer.get_soft_volume(), move || {
        backend(None, AudioFormat::default())
    });

    let mut attempt = 0;
    loop {
        let connect = Spirc::new(connect_config(), session.clone(), creds.clone(), player.clone(), mixer.clone());
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
                creds = keep_reusable(&engine, &session, creds).await;
                let player_user = session.username();
                let event = Event::Connected { player: player_user, app: app_user_id().await };
                if let State::AccountMismatch(_) = engine.apply(generation, event) {
                    engine.stop_spirc();
                    spirc_task.await;
                    return;
                }
                tokio::pin!(spirc_task);
                // the Spirc task ends on shutdown or session loss; a dead player thread is fatal
                loop {
                    tokio::select! {
                        _ = &mut spirc_task => break,
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
    match tokio::task::spawn_blocking(move || store.save(&to_save)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("engine: could not store the player login in the credentials file: {e}"),
        Err(e) => eprintln!("engine: could not store the player login in the credentials file: {e}"),
    }
    reusable
}

// ---- commands --------------------------------------------------------------

/// `{state, name: "The Run", reason?}`.
#[tauri::command]
pub fn engine_status(engine: Managed<'_, Engine>) -> Status {
    Status::from(&engine.state())
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

/// Restart with the stored credentials. The UI calls it after the app's own login
/// or logout, so the account check runs again.
#[tauri::command]
pub async fn engine_restart(engine: Managed<'_, Engine>) -> Result<(), String> {
    engine.restart(None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use State::*;

    fn connected(player: &str, app: Option<&str>) -> Event {
        Event::Connected { player: player.into(), app: app.map(Into::into) }
    }

    #[test]
    fn start_depends_on_credentials() {
        for s in [NeedsLogin, Ready, Failed("x".into()), AccountMismatch("x".into())] {
            assert_eq!(next_state(&s, Event::Start { has_credentials: true }), Starting);
            assert_eq!(next_state(&s, Event::Start { has_credentials: false }), NeedsLogin);
        }
    }

    #[test]
    fn premium_check() {
        assert!(premium_missing(&serde_json::json!({"product": "free"})));
        assert!(!premium_missing(&serde_json::json!({"product": "premium"})));
        // the field can be missing (Feb 2026 docs say it was removed): don't block on unknown
        assert!(!premium_missing(&serde_json::json!({"id": "x"})));
    }

    #[test]
    fn connected_same_account_is_ready() {
        assert_eq!(next_state(&Starting, connected("alice", Some("alice"))), Ready);
        assert_eq!(next_state(&Reconnecting, connected("alice", Some("alice"))), Ready);
        // the app isn't logged in or /me failed: nothing to compare
        assert_eq!(next_state(&Starting, connected("alice", None)), Ready);
        // usernames and ids differ only in case for some old accounts
        assert_eq!(next_state(&Starting, connected("Alice", Some("alice"))), Ready);
    }

    #[test]
    fn connected_other_account_is_mismatch_naming_both() {
        let AccountMismatch(reason) = next_state(&Starting, connected("alice", Some("bob"))) else {
            panic!("expected account_mismatch")
        };
        assert!(reason.contains("alice") && reason.contains("bob"), "{reason}");
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
        for s in [NeedsLogin, Failed("x".into()), AccountMismatch("x".into())] {
            assert_eq!(next_state(&s, Event::Dropped), s);
            assert_eq!(next_state(&s, connected("a", Some("a"))), s);
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
            Event::Fatal("Spotify Premium required".into())
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
    fn device_is_the_run_at_half_volume() {
        let c = connect_config();
        assert_eq!(c.name, "The Run");
        assert_eq!(c.device_type, DeviceType::Computer);
        assert_eq!(c.initial_volume, u16::MAX / 2);
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
        let v = serde_json::to_value(Status::from(&Ready)).unwrap();
        assert_eq!(v, serde_json::json!({"state": "ready", "name": "The Run"}));
        let v = serde_json::to_value(Status::from(&Failed("Premium required".into()))).unwrap();
        assert_eq!(v, serde_json::json!({"state": "failed", "name": "The Run", "reason": "Premium required"}));
        let names: Vec<&str> = [NeedsLogin, Starting, Reconnecting, AccountMismatch("m".into())]
            .iter()
            .map(|s| Status::from(s).state)
            .collect();
        assert_eq!(names, ["needs_login", "starting", "reconnecting", "account_mismatch"]);
    }
}
