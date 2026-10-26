//! The song-start and ad notifications. `notify-rust` directly, not `tauri-plugin-notification`:
//! the plugin drops the image on macOS, and the cover is shown through `image_path`.
use tauri::{command, AppHandle};

/// `cover` = the song's cover URL (a Spotify CDN one): shown in the banner from the cover store.
/// `summary` overrides the banner's app line; the ad uses it ("Stylus — реклама"), because a
/// song's start lands a second later and macOS would stack the identical "Stylus" banner over it.
#[command]
pub async fn notify(app: AppHandle, message: String, cover: Option<String>, summary: Option<String>) {
    let image = match cover {
        Some(url) => crate::covers::path_for(&url).await,
        None => None,
    };
    // as the plugin did: an unbundled dev binary can't own notifications, so the Terminal's identity
    let _ = notify_rust::set_application(if tauri::is_dev() { "com.apple.Terminal" } else { &app.config().identifier });
    let mut n = notify_rust::Notification::new();
    n.summary(summary.as_deref().unwrap_or("Stylus")).body(&message);
    if let Some(path) = image {
        n.image_path(&path.to_string_lossy());
    }
    match n.show() { // the notification is sent when the handle drops; never blocks
        Ok(_) => log::info!("notify: {message:?}"),
        Err(why) => log::warn!("notify failed: {message:?} ({why})"),
    }
}
