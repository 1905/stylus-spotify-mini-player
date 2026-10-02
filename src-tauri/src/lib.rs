mod auth;
mod local;
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
            spotify::resume_at,
            local::launch_local_spotify,
            spotify::pause,
            spotify::next_track,
            spotify::previous_track,
            spotify::seek,
            spotify::transfer_playback,
            spotify::set_volume,
            spotify::set_shuffle,
            spotify::set_repeat,
            spotify::get_saved_tracks,
            spotify::liked_count,
            spotify::get_saved_albums,
            spotify::is_saved,
            spotify::save_track,
            spotify::unsave_track,
            spotify::play_context,
            spotify::mix_info,
            spotify::get_top,
            spotify::get_artist,
            spotify::get_artist_albums,
            spotify::get_followed_artists,
            spotify::add_to_queue
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
