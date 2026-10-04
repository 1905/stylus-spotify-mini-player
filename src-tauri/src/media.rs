//! OS media controls: Now Playing + media keys (souvlaki → MPNowPlayingInfoCenter /
//! MPRemoteCommandCenter on macOS). The JS poll feeds `media_update` / `media_clear`;
//! a key press or a Now Playing control is emitted to JS as `media-command`.
//!
//! Threading: the controls live in a main-thread `thread_local`, and commands reach them
//! through `AppHandle::run_on_main_thread`. Not a `Mutex` in managed state: managed state
//! must be `Send + Sync`, and `MediaControls` is `!Send` on Windows (COM objects). macOS also
//! wants MediaPlayer calls on the thread that runs the app's run loop.
//! If the controls can't be created, every command is a no-op.

use std::cell::RefCell;
use std::time::Duration;

use serde::Serialize;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tauri::{AppHandle, Emitter};

pub const COMMAND_EVENT: &str = "media-command";

thread_local! {
    /// Main thread only. None: not created (yet), or creation failed.
    static CONTROLS: RefCell<Option<Controls>> = const { RefCell::new(None) };
}

struct Controls {
    os: MediaControls,
    /// What Now Playing shows. Same metadata again → skip set_metadata: it would reset the
    /// elapsed time and reload the cover from the network.
    shown: Option<Track>,
}

// ---- pure parts ------------------------------------------------------------

/// The `media-command` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Command {
    pub action: &'static str,
    #[serde(rename = "positionMs", skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
}

impl Command {
    fn new(action: &'static str) -> Self {
        Self { action, position_ms: None }
    }
}

/// An OS event as a `media-command`, or None for events the app doesn't handle
/// (stop, relative seeks, volume, open URI, raise, quit; macOS sends none of them).
pub fn command_for(event: &MediaControlEvent) -> Option<Command> {
    use MediaControlEvent::*;
    Some(match event {
        Play => Command::new("play"),
        Pause => Command::new("pause"),
        Toggle => Command::new("toggle"),
        Next => Command::new("next"),
        Previous => Command::new("previous"),
        SetPosition(MediaPosition(at)) => Command {
            action: "seek",
            position_ms: Some(u64::try_from(at.as_millis()).unwrap_or(u64::MAX)),
        },
        _ => return None,
    })
}

/// The song part of a `media_update`, owned. Empty strings count as missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover: Option<String>,
    pub duration: Option<Duration>,
}

impl Track {
    pub fn new(
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        cover: Option<String>,
        duration_ms: Option<f64>,
    ) -> Self {
        let text = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        Self {
            title: text(title),
            artist: text(artist),
            album: text(album),
            cover: text(cover),
            duration: duration_ms.and_then(ms).filter(|d| !d.is_zero()),
        }
    }

    pub fn metadata(&self) -> MediaMetadata<'_> {
        MediaMetadata {
            title: self.title.as_deref(),
            artist: self.artist.as_deref(),
            album: self.album.as_deref(),
            cover_url: self.cover.as_deref(),
            duration: self.duration,
        }
    }
}

/// JS milliseconds (maybe fractional, negative or NaN) as a Duration.
fn ms(v: f64) -> Option<Duration> {
    (v.is_finite() && v >= 0.0).then(|| Duration::from_secs_f64(v / 1000.0))
}

/// The play state of a `media_update`. A missing `playing` counts as paused.
pub fn playback(playing: Option<bool>, position_ms: Option<f64>) -> MediaPlayback {
    let progress = position_ms.and_then(ms).map(MediaPosition);
    if playing.unwrap_or(false) {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    }
}

// ---- the OS side (main thread) ---------------------------------------------

/// Create the controls and attach the event handler. Call on the main thread (Tauri's setup).
/// A failure is logged; the app runs on without media keys.
pub fn init(app: &AppHandle) {
    let config = PlatformConfig { display_name: "Stylus", dbus_name: "stylus", hwnd: None };
    let mut os = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => return eprintln!("media: could not create OS media controls: {e:?}"),
    };
    let emitter = app.clone();
    let attached = os.attach(move |event| {
        if let Some(cmd) = command_for(&event) {
            if let Err(e) = emitter.emit(COMMAND_EVENT, cmd) {
                eprintln!("media: could not emit {COMMAND_EVENT}: {e}");
            }
        }
    });
    if let Err(e) = attached {
        return eprintln!("media: could not attach the media key handler: {e:?}");
    }
    CONTROLS.with(|c| *c.borrow_mut() = Some(Controls { os, shown: None }));
}

/// Run `f` on the controls, on the main thread. No controls → nothing happens.
fn with_controls(app: &AppHandle, f: impl FnOnce(&mut Controls) + Send + 'static) {
    let queued = app.run_on_main_thread(move || {
        CONTROLS.with(|c| {
            if let Some(controls) = c.borrow_mut().as_mut() {
                f(controls);
            }
        })
    });
    if let Err(e) = queued {
        eprintln!("media: could not reach the main thread: {e}");
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // the locked interface has the keys at the top level
pub fn media_update(
    app: AppHandle,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    cover: Option<String>,
    duration_ms: Option<f64>,
    position_ms: Option<f64>,
    playing: Option<bool>,
) {
    let track = Track::new(title, artist, album, cover, duration_ms);
    let state = playback(playing, position_ms);
    with_controls(&app, move |c| {
        // metadata first: on macOS set_metadata replaces the whole Now Playing info,
        // elapsed time included, and set_playback then writes the position back
        if c.shown.as_ref() != Some(&track) {
            if let Err(e) = c.os.set_metadata(track.metadata()) {
                eprintln!("media: set_metadata failed: {e:?}");
            }
            c.shown = Some(track);
        }
        if let Err(e) = c.os.set_playback(state) {
            eprintln!("media: set_playback failed: {e:?}");
        }
    });
}

#[tauri::command]
pub fn media_clear(app: AppHandle) {
    with_controls(&app, |c| {
        if let Err(e) = c.os.set_playback(MediaPlayback::Stopped) {
            eprintln!("media: set_playback failed: {e:?}");
        }
        if let Err(e) = c.os.set_metadata(MediaMetadata::default()) {
            eprintln!("media: set_metadata failed: {e:?}");
        }
        c.shown = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use souvlaki::SeekDirection;

    #[test]
    fn transport_events_map_to_actions() {
        use MediaControlEvent::*;
        for (event, action) in
            [(Play, "play"), (Pause, "pause"), (Toggle, "toggle"), (Next, "next"), (Previous, "previous")]
        {
            assert_eq!(command_for(&event), Some(Command { action, position_ms: None }));
        }
    }

    #[test]
    fn set_position_is_an_absolute_seek_in_ms() {
        let event = MediaControlEvent::SetPosition(MediaPosition(Duration::from_millis(61_500)));
        assert_eq!(command_for(&event), Some(Command { action: "seek", position_ms: Some(61_500) }));
    }

    #[test]
    fn unsupported_events_are_ignored() {
        use MediaControlEvent::*;
        for event in [
            Stop,
            Seek(SeekDirection::Forward),
            SeekBy(SeekDirection::Backward, Duration::from_secs(10)),
            SetVolume(0.5),
            OpenUri("x".into()),
            Raise,
            Quit,
        ] {
            assert_eq!(command_for(&event), None, "{event:?}");
        }
    }

    #[test]
    fn command_payload_matches_the_js_contract() {
        let seek = Command { action: "seek", position_ms: Some(1200) };
        assert_eq!(serde_json::to_value(seek).unwrap(), serde_json::json!({"action": "seek", "positionMs": 1200}));
        assert_eq!(serde_json::to_value(Command::new("toggle")).unwrap(), serde_json::json!({"action": "toggle"}));
    }

    #[test]
    fn track_converts_to_metadata() {
        let t = Track::new(
            Some("Intro".into()),
            Some("The xx".into()),
            Some("xx".into()),
            Some("https://i.scdn.co/image/abc".into()),
            Some(128_000.0),
        );
        assert_eq!(
            t.metadata(),
            MediaMetadata {
                title: Some("Intro"),
                artist: Some("The xx"),
                album: Some("xx"),
                cover_url: Some("https://i.scdn.co/image/abc"),
                duration: Some(Duration::from_secs(128)),
            }
        );
    }

    #[test]
    fn missing_or_empty_fields_are_none() {
        // mode "other" (an ad, a podcast): no song data at all
        assert_eq!(Track::new(None, None, None, None, None).metadata(), MediaMetadata::default());
        let t = Track::new(Some("".into()), Some("  ".into()), None, Some("".into()), Some(0.0));
        assert_eq!(t, Track::default());
    }

    #[test]
    fn bad_numbers_are_dropped() {
        for v in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Track::new(None, None, None, None, Some(v)).duration, None, "{v}");
            assert_eq!(playback(Some(true), Some(v)), MediaPlayback::Playing { progress: None }, "{v}");
        }
    }

    #[test]
    fn playback_follows_playing_and_position() {
        let at = |ms| Some(MediaPosition(Duration::from_millis(ms)));
        assert_eq!(playback(Some(true), Some(5000.0)), MediaPlayback::Playing { progress: at(5000) });
        assert_eq!(playback(Some(false), Some(5000.0)), MediaPlayback::Paused { progress: at(5000) });
        assert_eq!(playback(None, None), MediaPlayback::Paused { progress: None });
        // fractional ms from JS keep their sub-ms part
        assert_eq!(
            playback(Some(true), Some(1.5)),
            MediaPlayback::Playing { progress: Some(MediaPosition(Duration::from_micros(1500))) }
        );
    }
}
