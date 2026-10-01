mod auth;
mod spotify;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            auth::auth_status,
            auth::login,
            spotify::get_playlists,
            spotify::get_playlist_tracks,
            spotify::search,
            spotify::get_album_tracks,
            spotify::get_queue,
            spotify::get_recently_played,
            spotify::list_devices,
            spotify::playback_state,
            spotify::play_on_device,
            spotify::resume,
            spotify::pause,
            spotify::next_track,
            spotify::previous_track,
            spotify::seek
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
