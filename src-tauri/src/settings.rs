//! App settings: `<app dir>/settings.json` (0600), e.g.
//! `{"bitrate": 320, "mcp_enabled": false, "mcp_key": null}`.
//! A missing or unreadable file, or an unknown value, means the default.

use librespot_playback::config::Bitrate;
use serde::{Deserialize, Serialize};

/// Spotify's stream qualities, kbps.
pub const BITRATES: [u16; 3] = [96, 160, 320];
pub const DEFAULT_BITRATE: u16 = 320;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub bitrate: u16,
    /// The local MCP server (mcp.rs) runs while the app is open.
    pub mcp_enabled: bool,
    /// The MCP bearer key: 32 random bytes, base64url. Made on the first enable.
    pub mcp_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { bitrate: DEFAULT_BITRATE, mcp_enabled: false, mcp_key: None }
    }
}

fn path() -> std::path::PathBuf {
    crate::auth::app_dir().join("settings.json")
}

/// The settings in `json`; anything unknown or broken falls back to the default.
fn parse(json: &str) -> Settings {
    let raw: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let bitrate = raw["bitrate"].as_u64().and_then(|b| u16::try_from(b).ok()).filter(|b| BITRATES.contains(b));
    let mcp_key = raw["mcp_key"].as_str().filter(|k| is_key(k)).map(str::to_string);
    Settings { bitrate: bitrate.unwrap_or(DEFAULT_BITRATE), mcp_enabled: raw["mcp_enabled"].as_bool().unwrap_or(false), mcp_key }
}

pub fn load() -> Settings {
    std::fs::read_to_string(path()).map(|s| parse(&s)).unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_string(settings).map_err(|e| e.to_string())?;
    crate::auth::write_private(&path(), &json).map_err(|e| format!("could not save {}: {e}", path().display()))
}

/// Load, change with `f`, save: one at a time, so two writers can't drop each other's change.
pub fn update<T>(f: impl FnOnce(&mut Settings) -> T) -> Result<(T, Settings), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    let out = f(&mut s);
    save(&s)?;
    Ok((out, s))
}

/// A new MCP key: 32 random bytes, base64url without padding (43 chars).
pub fn new_key() -> String {
    use base64::Engine as _;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// What a key from `new_key` looks like; anything else in the file is dropped.
fn is_key(k: &str) -> bool {
    k.len() == 43 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// librespot's bitrate for a stored kbps value (anything unknown → 320).
pub fn librespot_bitrate(kbps: u16) -> Bitrate {
    match kbps {
        96 => Bitrate::Bitrate96,
        160 => Bitrate::Bitrate160,
        _ => Bitrate::Bitrate320,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_bitrates() {
        for b in BITRATES {
            assert_eq!(parse(&format!(r#"{{"bitrate": {b}}}"#)).bitrate, b);
        }
    }

    #[test]
    fn parse_garbage_is_default() {
        for bad in ["", "nope", "{}", r#"{"bitrate": 128}"#, r#"{"bitrate": "320"}"#, r#"{"bitrate": 70000}"#, "[]"] {
            assert_eq!(parse(bad), Settings::default(), "{bad}");
        }
        assert_eq!(Settings::default().bitrate, 320);
        assert!(!Settings::default().mcp_enabled);
    }

    #[test]
    fn round_trip_json() {
        let s = Settings { bitrate: 96, ..Settings::default() };
        assert_eq!(parse(&serde_json::to_string(&s).unwrap()), s);
        assert_eq!(serde_json::to_string(&s).unwrap(), r#"{"bitrate":96,"mcp_enabled":false,"mcp_key":null}"#);
        let s = Settings { bitrate: 320, mcp_enabled: true, mcp_key: Some(new_key()) };
        assert_eq!(parse(&serde_json::to_string(&s).unwrap()), s);
    }

    #[test]
    fn old_file_keeps_its_bitrate() {
        assert_eq!(parse(r#"{"bitrate":160}"#), Settings { bitrate: 160, ..Settings::default() });
    }

    #[test]
    fn keys() {
        let (a, b) = (new_key(), new_key());
        assert_ne!(a, b);
        assert!(is_key(&a), "{a}");
        // a hand-edited short or odd key is dropped, not trusted
        assert_eq!(parse(r#"{"mcp_key":"short"}"#).mcp_key, None);
        assert_eq!(parse(&format!(r#"{{"mcp_key":"{}!"}}"#, &a[..42])).mcp_key, None);
    }

    #[test]
    fn maps_to_librespot() {
        assert!(matches!(librespot_bitrate(96), Bitrate::Bitrate96));
        assert!(matches!(librespot_bitrate(160), Bitrate::Bitrate160));
        assert!(matches!(librespot_bitrate(320), Bitrate::Bitrate320));
    }
}
