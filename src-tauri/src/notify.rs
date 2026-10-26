//! The song-start and ad notifications. `notify-rust` directly, not `tauri-plugin-notification`:
//! the plugin drops the image on macOS, and the cover is shown through `image_path`.
use tauri::{command, AppHandle};

/// `cover` = the song's cover URL (a Spotify CDN one): shown in the banner from the cover store.
#[command]
pub async fn notify(app: AppHandle, message: String, cover: Option<String>) {
    let image = match cover {
        Some(url) => crate::covers::path_for(&url).await,
        None => None,
    };
    // as the plugin did: an unbundled dev binary can't own notifications, so the Terminal's identity
    let _ = notify_rust::set_application(if tauri::is_dev() { "com.apple.Terminal" } else { &app.config().identifier });
    let mut n = notify_rust::Notification::new();
    n.summary("Stylus").body(&message);
    if let Some(path) = image {
        n.image_path(&path.to_string_lossy());
    }
    match n.show() { // the notification is sent when the handle drops; never blocks
        Ok(_) => log::info!("notify: {message:?}"),
        Err(why) => log::warn!("notify failed: {message:?} ({why})"),
    }
}
