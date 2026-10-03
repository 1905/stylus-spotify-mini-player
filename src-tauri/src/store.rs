//! The UI's saved state on disk: `<app dir>/state.json` (0600, atomic writes), a flat map of
//! keys → JSON values (`lastSession`, `knownMixes`, …). Not the WebView's localStorage: that
//! is tied to the app identity and to WebKit, and the user wants state in a plain file.
//! A file that doesn't parse is moved aside to `state.json.bad` and the store starts empty.

use serde_json::{Map, Value};
use std::sync::Mutex;

static STATE: Mutex<Option<Map<String, Value>>> = Mutex::new(None);

fn path() -> std::path::PathBuf {
    crate::auth::app_dir().join("state.json")
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
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.get_or_insert_with(|| {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(s) => parse(&s).unwrap_or_else(|| {
                log::warn!(target: "needle::store", "state.json doesn't parse: moved to state.json.bad");
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
    crate::auth::write_private(&path(), &json).map_err(|e| {
        log::warn!(target: "needle::store", "could not save state.json: {e}");
        format!("could not save state: {e}")
    })
}

/// Every stored key and value, read once by the UI at startup.
#[tauri::command]
pub fn store_all() -> Value {
    with_state(|s| Value::Object(s.clone()))
}

/// The value stored under key, for Rust's own keys (`apiBlockedUntil`).
pub fn get(key: &str) -> Option<Value> {
    with_state(|s| s.get(key).cloned())
}

/// Store value under key (null removes it). Written to disk before it returns.
#[tauri::command]
pub fn store_set(key: String, value: Value) -> Result<(), String> {
    with_state(|s| {
        let changed = if value.is_null() {
            s.remove(&key).is_some()
        } else if s.get(&key) == Some(&value) {
            false
        } else {
            s.insert(key, value);
            true
        };
        if changed {
            save(s)
        } else {
            Ok(())
        }
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
