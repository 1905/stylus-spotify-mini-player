//! Persisted-query hashes for Spotify's internal GraphQL endpoint (pathfinder, internal.rs).
//!
//! The web player's JS bundles name each query by a sha256 hash, and Spotify rotates them.
//! The defaults below come from `spikes/internal-api/tools/hashes.json` (scraped 2026-10-03).
//! A `hashes.json` in the app dir overrides them without a rebuild: the same format the scraper
//! writes (`{"op": {"hash": "…", "kind": "query"}}`) or a flat `{"op": "hash"}`. Bad entries are
//! skipped; a broken file is logged and ignored.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::Value;

const LOG: &str = "stylus::internal";

/// The operations internal.rs uses, with their default hashes.
const DEFAULTS: &[(&str, &str)] = &[
    ("searchDesktop", "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03"),
    ("searchTracks", "b02683192a98dde7966b5e6655a79eeb62713eab703eda9902c932818dd52751"),
    ("searchAlbums", "202cb3305e31e5a0767ba7925f28bd728cf8f8b0217e6da43909056071cd70e9"),
    ("libraryV3", "390c78e5b951029bad359785e69b07b536a509c581cbcd0aded5e5067f187455"),
    ("fetchPlaylist", "8964e8eafb21aa992a7d951d256d83285c04be2105d209262901de70cb97584a"),
    ("fetchLibraryTracks", "087278b20b743578a6262c2b0b4bcd20d879c503cc359a2285baf083ef944240"),
    ("getAlbum", "6a74b456cd1735c9193d9e8ec8cc5184cad7ce13572210315229db3975964361"),
    ("queryArtistOverview", "1ac33ddab5d39a3a9c27802774e6d78b9405cc188c6f75aed007df2a32737c72"),
    ("queryArtistDiscographyAll", "5e07d323febb57b4a56a42abbf781490e58764aa45feb6e3dc0591564fc56599"),
    ("fetchEntitiesForRecentlyPlayed", "cf5d2e94ffd82788470788ae1f6090cc3e9e774fb8fd383580634c6e6f50f7be"),
    ("areEntitiesInLibrary", "134337999233cc6fdd6b1e6dbf94841409f04a946c5c7b744b09ba0dfe5a85ed"),
    ("userTopContent", "49ee15704de4a7fdeac65a02db20604aa11e46f02e809c55d9a89f6db9754356"),
    ("home", "76243c78b0e20ecdbe41b794dec8cbe73f75e585b0a7201b8d2e84578412847a"),
];

/// A sha256 in hex: what a persisted-query hash looks like.
fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The overrides in a `hashes.json` body. Unknown shapes and non-hash values are skipped.
pub fn parse_overrides(body: &str) -> Result<HashMap<String, String>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("hashes.json is not an object")?;
    Ok(obj
        .iter()
        .filter_map(|(op, entry)| {
            let h = entry.as_str().or_else(|| entry["hash"].as_str())?;
            is_hash(h).then(|| (op.clone(), h.to_ascii_lowercase()))
        })
        .collect())
}

/// The defaults with `overrides` on top.
pub fn merged(overrides: HashMap<String, String>) -> HashMap<String, String> {
    let mut all: HashMap<String, String> = DEFAULTS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    all.extend(overrides);
    all
}

/// Defaults merged with `<dir>/hashes.json` when it exists.
pub fn load_from(dir: &std::path::Path) -> HashMap<String, String> {
    let path = dir.join("hashes.json");
    let overrides = match std::fs::read_to_string(&path) {
        Ok(body) => match parse_overrides(&body) {
            Ok(o) => {
                log::info!(target: LOG, "{} pathfinder hashes from {}", o.len(), path.display());
                o
            }
            Err(e) => {
                log::warn!(target: LOG, "ignoring {}: {e}", path.display());
                HashMap::new()
            }
        },
        Err(_) => HashMap::new(),
    };
    merged(overrides)
}

/// The hash of `op` for this run (the override file is read once).
pub fn get(op: &str) -> Option<String> {
    static ALL: OnceLock<HashMap<String, String>> = OnceLock::new();
    ALL.get_or_init(|| load_from(&crate::paths::app_dir())).get(op).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "abcdefABCDEF0000000000000000000000000000000000000000000000000000";

    #[test]
    fn defaults_are_hashes() {
        for (op, h) in DEFAULTS {
            assert!(is_hash(h), "{op}");
        }
        assert!(merged(HashMap::new()).contains_key("searchDesktop"));
    }

    #[test]
    fn overrides_both_formats_and_skips_bad() {
        let body = format!(
            r#"{{"searchDesktop": {{"hash": "{H1}", "kind": "query"}}, "getAlbum": "{H2}",
                "bad": "nothex", "short": {{"hash": "abc"}}, "num": 5}}"#
        );
        let o = parse_overrides(&body).unwrap();
        assert_eq!(o.len(), 2);
        assert_eq!(o["searchDesktop"], H1);
        assert_eq!(o["getAlbum"], H2.to_ascii_lowercase());
        let all = merged(o);
        assert_eq!(all["searchDesktop"], H1);
        // untouched ops keep their default
        assert_eq!(all["libraryV3"], DEFAULTS.iter().find(|(k, _)| *k == "libraryV3").unwrap().1);
        assert!(parse_overrides("[1]").is_err());
        assert!(parse_overrides("{not json").is_err());
    }

    #[test]
    fn load_from_dir() {
        let dir = std::env::temp_dir().join(format!("stylus-hashes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("hashes.json"));
        assert_eq!(load_from(&dir), merged(HashMap::new()), "no file: defaults");
        std::fs::write(dir.join("hashes.json"), format!(r#"{{"newOp": "{H1}"}}"#)).unwrap();
        let all = load_from(&dir);
        assert_eq!(all["newOp"], H1);
        assert!(all.contains_key("getAlbum"));
        std::fs::write(dir.join("hashes.json"), "garbage").unwrap();
        assert_eq!(load_from(&dir), merged(HashMap::new()), "broken file: defaults");
    }
}
