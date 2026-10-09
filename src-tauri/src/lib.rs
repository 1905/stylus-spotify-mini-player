mod applog;
mod audio_out;
mod auth;
mod cache;
mod clipboard;
pub mod control;
mod dock;
mod hashes;
mod internal;
mod library;
pub mod links;
pub mod mcp;
mod mcp_app;
pub mod mcp_tools;
mod media;
mod nowplaying;
mod parse;
mod paths;
mod pb;
mod player;
mod session;
mod settings;
mod spotify;
mod store;
mod tray;

use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    applog::init();
    paths::remove_legacy_files();
    let engine = player::Engine::new(Arc::new(player::FileStore));
    internal::attach(engine.clone());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(engine.clone())
        .setup(|app| {
            // setup runs on the main thread: the media controls live there (see media.rs)
            media::init(app.handle());
            library::attach(app.handle().clone());
            // the local MCP server, when it's on in Settings
            tauri::async_runtime::spawn(mcp::start_if_enabled());
            // the speaker "This Mac" starts with the app (or waits in needs_login)
            let engine = app.state::<player::Engine>().inner().clone();
            engine.attach(app.handle().clone());
            tauri::async_runtime::spawn(async move { engine.restart(None).await });
            // "Log Out" in the app menu (and the handler for the tray menu's one)
            if let Err(e) = tray::init_app_menu(app.handle()) {
                log::warn!("app menu: {e}");
            }
            // the menu-bar icon and its mini player (tray.rs); the app runs fine without them
            if let Err(e) = tray::init(app.handle()) {
                log::warn!("menu bar: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            applog::app_log,
            clipboard::copy_text,
            store::store_all,
            store::store_set,
            auth::auth_status,
            player::engine_status,
            player::engine_login,
            player::logout,
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
            spotify::get_album_info,
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
            player::local_shuffle,
            player::local_repeat,
            player::local_state,
            spotify::mix_info,
            spotify::get_top,
            spotify::get_artist,
            spotify::get_artist_albums,
            spotify::get_followed_artists,
            spotify::add_to_queue,
            control::control_transfer,
            library::mixes_list,
            library::links_list,
            library::link_resolve,
            library::link_save,
            library::link_remove,
            mcp::mcp_status,
            mcp::mcp_set_enabled,
            mcp::mcp_reset_key,
            mcp::mcp_connect_text,
            mcp::mcp_skill_text,
            tray::mini_push,
            tray::mini_get,
            tray::mini_command,
            tray::mini_hide,
            tray::tray_config
        ])
        // Cmd+W / the close button hides the window (music keeps playing); Cmd+Q quits.
        // The mini player hides when it loses focus (a click outside it).
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if window.label() == tray::MINI {
                    tray::hide_mini(window.app_handle());
                } else {
                    let _ = window.hide();
                }
            }
            tauri::WindowEvent::Focused(false) if window.label() == tray::MINI => tray::hide_mini(window.app_handle()),
            _ => {}
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
            tray::show_main(app);
        }
        _ => {}
    });
}
