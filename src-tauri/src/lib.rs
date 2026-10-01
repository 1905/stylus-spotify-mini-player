mod auth;
mod spotify;

use serde_json::Value;

#[tauri::command]
fn auth_status() -> &'static str {
    auth::auth_status()
}

#[tauri::command]
async fn login() -> Result<(), String> {
    auth::login().await
}

#[tauri::command]
async fn get_playlists() -> Result<Value, String> {
    spotify::get_playlists().await
}

#[tauri::command]
async fn get_playlist_tracks(playlist_id: String) -> Result<Value, String> {
    spotify::get_playlist_tracks(playlist_id).await
}

#[tauri::command]
async fn search(query: String) -> Result<Value, String> {
    spotify::search(query).await
}

#[tauri::command]
async fn get_album_tracks(album_id: String) -> Result<Value, String> {
    spotify::get_album_tracks(album_id).await
}

#[tauri::command]
async fn get_queue() -> Result<Value, String> {
    spotify::get_queue().await
}

#[tauri::command]
async fn get_recently_played() -> Result<Value, String> {
    spotify::get_recently_played().await
}

// ---- Spotify Connect playback control ----

#[tauri::command]
async fn list_devices() -> Result<Value, String> {
    spotify::list_devices().await
}

#[tauri::command]
async fn playback_state() -> Result<Value, String> {
    spotify::playback_state().await
}

#[tauri::command]
async fn play_on_device(device_id: String, uris: Vec<String>) -> Result<(), String> {
    spotify::play_on_device(device_id, uris).await
}

#[tauri::command]
async fn resume(device_id: String) -> Result<(), String> {
    spotify::resume(device_id).await
}

#[tauri::command]
async fn pause() -> Result<(), String> {
    spotify::pause().await
}

#[tauri::command]
async fn next_track() -> Result<(), String> {
    spotify::next_track().await
}

#[tauri::command]
async fn previous_track() -> Result<(), String> {
    spotify::previous_track().await
}

#[tauri::command]
async fn seek(position_ms: u64) -> Result<(), String> {
    spotify::seek(position_ms).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            auth_status,
            login,
            get_playlists,
            get_playlist_tracks,
            search,
            get_album_tracks,
            get_queue,
            get_recently_played,
            list_devices,
            playback_state,
            play_on_device,
            resume,
            pause,
            next_track,
            previous_track,
            seek
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
