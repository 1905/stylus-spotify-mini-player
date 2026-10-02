//! The Spotify app on this Mac. This app is only a remote: sound out of this computer
//! comes from the Spotify app, which shows up as a Connect device only while it runs.

use std::process::Command;

/// Starts Spotify in the background (no window comes to the front). Errors starting
/// with `SPOTIFY_NOT_INSTALLED` mean there is no Spotify app to start.
#[tauri::command]
pub fn launch_local_spotify() -> Result<(), String> {
    let status = Command::new("open").args(["-g", "-a", "Spotify"]).status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("SPOTIFY_NOT_INSTALLED: Spotify for Mac isn't installed".into())
    }
}
