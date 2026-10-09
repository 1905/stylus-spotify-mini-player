//! P1 spike (plans/2026-10-09-drop-web-api): which connect-state commands does a real Spirc
//! device obey when another session of the same account sends them over HTTP?
//!
//!   cargo run --release      (CREDS=<path> overrides the credentials file)
//!
//! Session T: a Spirc device "dwa-probe-target" with a silent sink that keeps real time (a pipe
//! to /dev/null decodes as fast as it can: tracks end in seconds and fake the results).
//! Session C: same account, other device id, no Spirc, HTTP only.
//! T's state comes from the player events that T's Spirc and player emit. (T's dealer got no
//! cluster update for its own changes in the first run, so the cluster is not used.)
//! Tokens and credentials are never printed.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{
    authentication::Credentials,
    config::DeviceType,
    dealer::protocol::TransferOptions,
    spclient::TransferRequest,
    Session, SessionConfig, SpotifyUri,
};
use librespot_metadata::{Metadata, Track};
use librespot_playback::{
    audio_backend::{Sink, SinkResult},
    config::PlayerConfig,
    convert::Converter,
    decoder::AudioPacket,
    mixer::{softmixer::SoftMixer, Mixer, MixerConfig, NoOpVolume},
    player::{Player, PlayerEvent},
};
use serde_json::{json, Value};

const KNOWN_TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
const DEFAULT_CREDS: &str = "/tmp/dwa-spike-creds/player-credentials.json";
const TARGET_NAME: &str = "dwa-probe-target";
const INITIAL_VOLUME: u16 = 13107; // 20 %
const SET_VOLUME: u32 = 32768; // 50 %

/// T's state from its player events.
#[derive(Clone, Debug, Default)]
struct View {
    active: bool,
    track: String,
    playing: bool,
    position: i64,
    shuffle: bool,
    repeat_context: bool,
    repeat_track: bool,
    volume: u32,
}

/// The last player events of T; `at` = when `position_ms` was true.
#[derive(Default)]
struct Live {
    track: String,
    playing: bool,
    position_ms: u32,
    at: Option<Instant>,
    shuffle: bool,
    repeat_context: bool,
    repeat_track: bool,
    volume: u32,
}

impl Live {
    fn on(&mut self, ev: PlayerEvent) {
        let at = |me: &mut Live, track: &librespot_core::SpotifyUri, pos: u32, playing: bool| {
            me.track = track.to_uri().unwrap_or_default();
            me.position_ms = pos;
            me.at = Some(Instant::now());
            me.playing = playing;
        };
        match ev {
            PlayerEvent::Playing { track_id, position_ms, .. } => at(self, &track_id, position_ms, true),
            PlayerEvent::PositionChanged { track_id, position_ms, .. } | PlayerEvent::PositionCorrection { track_id, position_ms, .. } => {
                let playing = self.playing;
                at(self, &track_id, position_ms, playing)
            }
            PlayerEvent::Seeked { track_id, position_ms, .. } => {
                let playing = self.playing;
                at(self, &track_id, position_ms, playing)
            }
            PlayerEvent::Paused { track_id, position_ms, .. } => at(self, &track_id, position_ms, false),
            PlayerEvent::Loading { track_id, position_ms, .. } => {
                let playing = self.playing;
                at(self, &track_id, position_ms, playing)
            }
            PlayerEvent::Stopped { .. } => self.playing = false,
            PlayerEvent::ShuffleChanged { shuffle } => self.shuffle = shuffle,
            PlayerEvent::RepeatChanged { context, track } => (self.repeat_context, self.repeat_track) = (context, track),
            PlayerEvent::VolumeChanged { volume } => self.volume = volume.into(),
            _ => {}
        }
    }

    fn view(&self) -> View {
        let elapsed = match (self.playing, self.at) {
            (true, Some(at)) => at.elapsed().as_millis() as i64,
            _ => 0,
        };
        View {
            active: !self.track.is_empty(),
            track: self.track.clone(),
            playing: self.playing,
            position: self.position_ms as i64 + elapsed,
            shuffle: self.shuffle,
            repeat_context: self.repeat_context,
            repeat_track: self.repeat_track,
            volume: self.volume,
        }
    }
}

impl View {
    fn short(&self) -> String {
        let id = self.track.rsplit(':').next().unwrap_or("");
        let repeat = if self.repeat_track { "track" } else if self.repeat_context { "context" } else { "off" };
        format!(
            "active={} {} pos={}s track={} shuffle={} repeat={} vol={}",
            self.active,
            if self.playing { "playing" } else { "paused" },
            self.position / 1000,
            &id[..id.len().min(8)],
            self.shuffle,
            repeat,
            self.volume
        )
    }
}

/// Drops the audio, but takes as long as playing it would.
#[derive(Default)]
struct SilentSink {
    since: Option<Instant>,
    samples: u64,
}

impl Sink for SilentSink {
    fn start(&mut self) -> SinkResult<()> {
        (self.since, self.samples) = (Some(Instant::now()), 0);
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let since = *self.since.get_or_insert_with(Instant::now);
        self.samples += packet.samples().map(|s| s.len() as u64).unwrap_or(0);
        let due = Duration::from_secs_f64(self.samples as f64 / librespot_playback::SAMPLES_PER_SECOND as f64);
        if let Some(wait) = due.checked_sub(since.elapsed()) {
            std::thread::sleep(wait);
        }
        Ok(())
    }
}

struct Row {
    name: String,
    endpoint: String,
    status: String,
    body_start: String,
    expected: String,
    seen: String,
    pass: bool,
    body: Option<Value>,
}

struct Probe {
    c: Session,
    http: reqwest::Client,
    c_id: String,
    t_id: String,
    live: Arc<Mutex<Live>>,
    rows: Vec<Row>,
}

fn first_bytes(b: &[u8], n: usize) -> String {
    let s = String::from_utf8_lossy(&b[..b.len().min(n)]).replace(['\n', '\r', '|'], " ");
    if s.is_empty() { "(empty)".into() } else { s }
}

fn command_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

impl Probe {
    fn view(&self) -> View {
        self.live.lock().unwrap().view()
    }

    /// One HTTP request from C with the session's tokens (same headers as internal.rs `send`).
    async fn send(&self, method: reqwest::Method, path: &str, body: &Value) -> (String, String) {
        let token = match self.c.login5().auth_token().await {
            Ok(t) => t,
            Err(e) => return ("login5 error".into(), first_bytes(e.to_string().as_bytes(), 120)),
        };
        let base = match self.c.spclient().base_url().await {
            Ok(b) => b,
            Err(e) => return ("base url error".into(), first_bytes(e.to_string().as_bytes(), 120)),
        };
        let mut rb = self
            .http
            .request(method, format!("{base}{path}"))
            .header("authorization", format!("Bearer {}", token.access_token))
            .header("app-platform", "OSX")
            .header("spotify-app-version", "1.2.52.442")
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Ok(ct) = self.c.spclient().client_token().await {
            rb = rb.header("client-token", ct);
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16().to_string();
                let bytes = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                (status, first_bytes(&bytes, 120))
            }
            Err(e) => ("transport error".into(), first_bytes(e.to_string().as_bytes(), 120)),
        }
    }

    /// Sends each body in turn until `check` holds 1.5 s later (a second look at 3.5 s marks
    /// a late change). Records one row with the body that worked, or the last body tried.
    async fn run(&mut self, name: &str, method: reqwest::Method, path_kind: &str, expected: &str, bodies: Vec<Value>, check: impl Fn(&View, &View) -> bool) {
        let path = format!("/connect-state/v1/{path_kind}/from/{}/to/{}", self.c_id, self.t_id);
        let endpoint = format!("{method} /connect-state/v1/{path_kind}/from/{{C}}/to/{{T}}");
        let mut row = None;
        for (i, body) in bodies.iter().enumerate() {
            let before = self.view();
            let (status, body_start) = self.send(method.clone(), &path, body).await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let mut after = self.view();
            let mut pass = check(&before, &after);
            let mut late = false;
            if !pass {
                tokio::time::sleep(Duration::from_secs(2)).await;
                after = self.view();
                pass = check(&before, &after);
                late = pass;
            }
            let variant = if bodies.len() > 1 { format!(" [body {}]", i + 1) } else { String::new() };
            println!("{name}{variant}: HTTP {status} | before {} | after {}{}", before.short(), after.short(), if late { " (late)" } else { "" });
            let seen = format!("{}{}{}", after.short(), if late { " (after 3.5 s)" } else { "" }, variant);
            row = Some(Row { name: name.into(), endpoint: endpoint.clone(), status, body_start, expected: expected.into(), seen, pass, body: Some(body.clone()) });
            if pass {
                break;
            }
        }
        self.rows.push(row.expect("at least one body"));
    }

    /// The `command` envelope: `minimal` alone, and with the fields librespot's `Request`
    /// types need (`logging_params`, plus `extra`). The minimal one goes first.
    fn variants(cmd: Value, extra: Value) -> Vec<Value> {
        let mut full = cmd.clone();
        full["logging_params"] = json!({"command_id": command_id()});
        if let (Some(f), Some(e)) = (full.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                f.insert(k.clone(), v.clone());
            }
        }
        vec![json!({"command": cmd}), json!({"command": full})]
    }
}

async fn session(creds: &Credentials) -> Session {
    let s = Session::new(SessionConfig::default(), None);
    s.connect(creds.clone(), false).await.expect("controller login refused");
    s
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let path = std::env::var("CREDS").unwrap_or_else(|_| DEFAULT_CREDS.into());
    let creds: Credentials = serde_json::from_str(&std::fs::read_to_string(&path).expect("no credentials file")).expect("bad credentials file");

    // ---- T: a real Spirc device with a null sink
    let t = Session::new(SessionConfig::default(), None);
    let t_id = t.device_id().to_string();
    let player_config = PlayerConfig { position_update_interval: Some(Duration::from_millis(500)), ..PlayerConfig::default() };
    let player = Player::new(player_config, t.clone(), Box::new(NoOpVolume), move || Box::new(SilentSink::default()) as Box<dyn Sink>);
    let live = Arc::new(Mutex::new(Live::default()));
    {
        let mut events = player.get_player_event_channel();
        let live = live.clone();
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                live.lock().unwrap().on(ev);
            }
        });
    }
    let mixer: Arc<dyn Mixer> = Arc::new(SoftMixer::open(MixerConfig::default()).expect("no mixer"));
    let config = ConnectConfig { name: TARGET_NAME.into(), device_type: DeviceType::Speaker, initial_volume: INITIAL_VOLUME, ..ConnectConfig::default() };
    let (spirc, spirc_task) = Spirc::new(config, t.clone(), creds.clone(), player, mixer).await.expect("Spirc did not start");
    tokio::spawn(spirc_task);
    println!("T up as {TARGET_NAME}; waiting 5 s for it to register");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // ---- C: HTTP only
    let c = session(&creds).await;
    let c_id = c.device_id().to_string();
    assert_ne!(c_id, t_id);
    let album = Track::get(&c, &SpotifyUri::from_uri(KNOWN_TRACK).unwrap()).await.expect("track metadata").album.id.to_uri().unwrap();
    println!("C up; known track's album {album}");
    let mut p = Probe { c, http: reqwest::Client::new(), c_id, t_id, live, rows: vec![] };

    // ---- transfer through SpClient::transfer (what P3 calls)
    {
        let before = p.view();
        let req = TransferRequest { transfer_options: TransferOptions { restore_paused: Some("restore".into()), ..Default::default() } };
        let res = p.c.spclient().transfer(&p.c_id, &p.t_id, Some(&req)).await;
        let (status, body_start) = match &res {
            Ok(b) => ("2xx".to_string(), first_bytes(b, 120)),
            Err(e) => ("error".to_string(), first_bytes(e.to_string().as_bytes(), 120)),
        };
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut after = p.view();
        if !after.active {
            tokio::time::sleep(Duration::from_secs(2)).await;
            after = p.view();
        }
        println!("transfer: {status} | before {} | after {}", before.short(), after.short());
        p.rows.push(Row {
            name: "transfer".into(),
            endpoint: "POST /connect-state/v1/connect/transfer/from/{C}/to/{T} (SpClient::transfer)".into(),
            status,
            body_start,
            expected: "T loads a track (it was idle)".into(),
            seen: after.short(),
            pass: after.active,
            body: Some(serde_json::to_value(json!({"transfer_options": {"restore_paused": "restore"}})).unwrap()),
        });
    }

    // ---- play context + skip_to
    let play = json!({"endpoint": "play", "context": {"uri": album, "url": format!("context://{album}")}, "options": {"skip_to": {"track_uri": KNOWN_TRACK}}});
    let k = KNOWN_TRACK;
    p.run("play context", reqwest::Method::POST, "player/command", "plays the known track (context not visible in player events)", Probe::variants(play, json!({"play_origin": {"feature_identifier": "stylus"}})), move |_, v| v.playing && v.track == k).await;

    p.run("pause", reqwest::Method::POST, "player/command", "paused", Probe::variants(json!({"endpoint": "pause"}), json!({})), |b, v| b.playing && !v.playing && !v.track.is_empty()).await;
    p.run("resume", reqwest::Method::POST, "player/command", "playing", Probe::variants(json!({"endpoint": "resume"}), json!({})), |b, v| !b.playing && v.playing).await;
    p.run("seek_to", reqwest::Method::POST, "player/command", "position 60-64 s", Probe::variants(json!({"endpoint": "seek_to", "value": 60000}), json!({"position": 60000})), |_, v| (59_000..=64_500).contains(&v.position)).await;
    p.run("skip_next", reqwest::Method::POST, "player/command", "another track", Probe::variants(json!({"endpoint": "skip_next"}), json!({})), |b, v| !v.track.is_empty() && v.track != b.track).await;
    let next_track = p.view().track;
    p.run("skip_prev", reqwest::Method::POST, "player/command", "previous track, or restart when >3 s in", Probe::variants(json!({"endpoint": "skip_prev"}), json!({})), |b, v| !v.track.is_empty() && (v.track != b.track || (b.position > 3000 && v.position < 2500))).await;
    for (endpoint, val) in [("set_shuffling_context", true), ("set_shuffling_context", false), ("set_repeating_context", true), ("set_repeating_track", true), ("set_repeating_track", false), ("set_repeating_context", false)] {
        let check = move |b: &View, v: &View| match endpoint {
            "set_shuffling_context" => b.shuffle != val && v.shuffle == val,
            "set_repeating_context" => b.repeat_context != val && v.repeat_context == val,
            _ => b.repeat_track != val && v.repeat_track == val,
        };
        let name = format!("{endpoint} {val}");
        p.run(&name, reqwest::Method::POST, "player/command", &format!("{} = {val}", endpoint.trim_start_matches("set_")), Probe::variants(json!({"endpoint": endpoint, "value": val}), json!({})), check).await;
    }
    p.run("volume", reqwest::Method::PUT, "connect/volume", "T volume 32768 (50 %)", vec![json!({"volume": SET_VOLUME})], |_, v| v.volume.abs_diff(SET_VOLUME) <= 1100).await;

    // ---- play uris: a list of tracks as context pages
    if !next_track.is_empty() && next_track != KNOWN_TRACK {
        let nt = next_track.clone();
        let play = json!({"endpoint": "play", "context": {"pages": [{"tracks": [{"uri": KNOWN_TRACK}, {"uri": next_track}]}]}, "options": {"skip_to": {"track_index": 1}}});
        p.run("play uris", reqwest::Method::POST, "player/command", "plays the 2nd uri of the list", Probe::variants(play, json!({"play_origin": {"feature_identifier": "stylus"}})), move |_, v| v.playing && v.track == nt).await;
    }

    // ---- play context again with the minimal body only: in one run it needed >3.5 s to start
    let play = json!({"endpoint": "play", "context": {"uri": album, "url": format!("context://{album}")}, "options": {"skip_to": {"track_uri": KNOWN_TRACK}}});
    p.run("play context (minimal body, 2nd time)", reqwest::Method::POST, "player/command", "plays the known track", vec![json!({"command": play})], move |_, v| v.playing && v.track == k).await;

    let _ = spirc.pause();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = spirc.shutdown();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // ---- the table
    let mut out = String::from("# connect-state spike: commands from C (HTTP only) to T (Spirc)\n\n| command | endpoint | HTTP | body start | expected | seen | result |\n|---|---|---|---|---|---|---|\n");
    for r in &p.rows {
        out += &format!("| {} | `{}` | {} | {} | {} | {} | {} |\n", r.name, r.endpoint, r.status, r.body_start, r.expected, r.seen, if r.pass { "PASS" } else { "FAIL" });
    }
    out += "\n## Bodies (the one that passed, else the last one tried)\n\n";
    for r in &p.rows {
        if let Some(b) = &r.body {
            out += &format!("- {} ({}): `{}`\n", r.name, if r.pass { "PASS" } else { "FAIL" }, b);
        }
    }
    println!("\n{out}");
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/REPORT.md"), &out).expect("write REPORT.md");
}
