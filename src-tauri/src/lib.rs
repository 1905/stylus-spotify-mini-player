mod applog;
mod audio_out;
mod auth;
mod cache;
mod dock;
mod media;
mod player;
mod session;
mod settings;
mod spotify;
mod store;

use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    applog::init();
    let engine = player::Engine::new(Arc::new(player::FileStore));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(engine.clone())
        .setup(|app| {
            // setup runs on the main thread: the media controls live there (see media.rs)
            media::init(app.handle());
            // the speaker "This Mac" starts with the app (or waits in needs_login)
            let engine = app.state::<player::Engine>().inner().clone();
            engine.attach(app.handle().clone());
            tauri::async_runtime::spawn(async move { engine.restart(None).await });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            applog::app_log,
            store::store_all,
            store::store_set,
            auth::auth_status,
            auth::login,
            player::engine_status,
            player::engine_login,
            player::engine_restart,
            player::engine_get_quality,
            player::engine_set_quality,
            dock::set_dock_art,
            media::media_update,
            media::media_clear,
            spotify::get_playlists,
            spotify::get_playlist_tracks,
            spotify::search,
            spotify::search_page,
            spotify::get_album_tracks,
            spotify::get_queue,
            spotify::get_recently_played,
            spotify::list_devices,
            spotify::playback_state,
            spotify::play_on_device,
            spotify::resume,
            spotify::resume_at,
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
            spotify::cache_get,
            spotify::me_id,
            player::local_play,
            player::local_pause,
            player::local_next,
            player::local_prev,
            player::local_seek,
            player::local_volume,
            player::local_load,
            player::session_get,
            spotify::mix_info,
            spotify::get_top,
            spotify::get_artist,
            spotify::get_artist_albums,
            spotify::get_followed_artists,
            spotify::add_to_queue
        ])
        // Cmd+W / the close button hides the window (music keeps playing); Cmd+Q quits
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(move |app, event| match event {
        tauri::RunEvent::Exit => {
            // pause and leave Spotify Connect cleanly, so "This Mac" doesn't linger as a device
            engine.shutdown();
        }
        // Dock icon clicked while the window is hidden: bring it back
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        _ => {}
    });
}
