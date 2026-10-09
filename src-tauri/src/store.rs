//! The UI's saved state on disk: `<app dir>/state.json` (0600, atomic writes), a flat map of
//! keys → JSON values (`lastSession`, `knownMixes`, …). Not the WebView's localStorage: that
//! is tied to the app identity and to WebKit, and the user wants state in a plain file.
//! A file that doesn't parse is moved aside to `state.json.bad` and the store starts empty.

use serde_json::{Map, Value};
use std::sync::Mutex;

static STATE: Mutex<Option<Map<String, Value>>> = Mutex::new(None);

fn path() -> std::path::PathBuf {
    crate::paths::app_dir().join("state.json")
}

/// The map in `json`, or None when it isn't a JSON object.
fn parse(json: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(json) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// Run f on the loaded map (read from disk on first use).
fn with_state<T>(f: impl FnOnce(&mut Map<String, Value>) -> T) -> T {
    let mut guard = crate::nowplaying::lock(&STATE);
    let state = guard.get_or_insert_with(|| {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(s) => parse(&s).unwrap_or_else(|| {
                log::warn!(target: "stylus::store", "state.json doesn't parse: moved to state.json.bad");
                let _ = std::fs::rename(&p, p.with_extension("json.bad"));
                Map::new()
            }),
            Err(_) => Map::new(),
        }
    });
    f(state)
}

fn save(state: &Map<String, Value>) -> Result<(), String> {
    let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
    crate::paths::write_private(&path(), &json).map_err(|e| {
        log::warn!(target: "stylus::store", "could not save state.json: {e}");
        format!("could not save state: {e}")
    })
}

/// Every stored key and value, read once by the UI at startup.
#[tauri::command]
pub fn store_all() -> Value {
    with_state(|s| Value::Object(s.clone()))
}

/// The value stored under key, for Rust's own reads.
pub fn get(key: &str) -> Option<Value> {
    with_state(|s| s.get(key).cloned())
}

/// Read, change and write one key under a single lock, so two concurrent updates can't lose each
/// other. `f` gets the current value and returns the new one (None: leave it) plus a result.
pub fn update<T>(key: &str, f: impl FnOnce(Option<&Value>) -> (Option<Value>, T)) -> Result<T, String> {
    with_state(|s| {
        let (next, out) = f(s.get(key));
        if let Some(v) = next {
            if s.get(key) != Some(&v) {
                // write a changed copy first: a failed save must leave memory as on disk, or a
                // retry would see "no change" and never write
                let mut changed = s.clone();
                changed.insert(key.to_string(), v);
                save(&changed)?;
                *s = changed;
            }
        }
        Ok(out)
    })
}

/// Store value under key (null removes it). Written to disk before it returns.
#[tauri::command]
pub fn store_set(key: String, value: Value) -> Result<(), String> {
    with_state(|s| {
        if (value.is_null() && !s.contains_key(&key)) || s.get(&key) == Some(&value) {
            return Ok(());
        }
        // a changed copy is written first, then kept: a failed save leaves memory as on disk
        let mut changed = s.clone();
        if value.is_null() {
            changed.remove(&key);
        } else {
            changed.insert(key, value);
        }
        save(&changed)?;
        *s = changed;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_only_objects() {
        assert!(parse(r#"{"a":1}"#).is_some());
        for bad in ["", "[]", "null", "1", "{", "\"x\""] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }
}
