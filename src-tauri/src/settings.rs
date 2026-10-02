//! App settings: `<app dir>/settings.json` (0600), e.g. `{"bitrate": 320}`.
//! A missing or unreadable file, or an unknown value, means the default.

use librespot_playback::config::Bitrate;
use serde::{Deserialize, Serialize};

/// Spotify's stream qualities, kbps.
pub const BITRATES: [u16; 3] = [96, 160, 320];
pub const DEFAULT_BITRATE: u16 = 320;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub bitrate: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { bitrate: DEFAULT_BITRATE }
    }
}

fn path() -> std::path::PathBuf {
    crate::auth::app_dir().join("settings.json")
}

/// The settings in `json`; anything unknown or broken falls back to the default.
fn parse(json: &str) -> Settings {
    let raw: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let bitrate = raw["bitrate"].as_u64().and_then(|b| u16::try_from(b).ok()).filter(|b| BITRATES.contains(b));
    Settings { bitrate: bitrate.unwrap_or(DEFAULT_BITRATE) }
}

pub fn load() -> Settings {
    std::fs::read_to_string(path()).map(|s| parse(&s)).unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_string(settings).map_err(|e| e.to_string())?;
    crate::auth::write_private(&path(), &json).map_err(|e| format!("could not save {}: {e}", path().display()))
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
    }

    #[test]
    fn round_trip_json() {
        let s = Settings { bitrate: 96 };
        assert_eq!(parse(&serde_json::to_string(&s).unwrap()), s);
        assert_eq!(serde_json::to_string(&s).unwrap(), r#"{"bitrate":96}"#);
    }

    #[test]
    fn maps_to_librespot() {
        assert!(matches!(librespot_bitrate(96), Bitrate::Bitrate96));
        assert!(matches!(librespot_bitrate(160), Bitrate::Bitrate160));
        assert!(matches!(librespot_bitrate(320), Bitrate::Bitrate320));
    }
}
