//! The Dock icon shows the current album art, drawn like a macOS app icon: a rounded
//! square with a margin on a transparent 1024×1024 canvas. None restores the app icon.

use tauri::AppHandle;

/// Largest image download accepted (Spotify covers are ~50–300 KB).
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// The url on the Dock now (None = the app's own icon). The async lock also serializes calls.
static CURRENT: tokio::sync::Mutex<Option<String>> = tokio::sync::Mutex::const_new(None);

/// Only Spotify's image CDNs: `https://i.scdn.co/…` and `https://*.spotifycdn.com/…`.
fn art_url_ok(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    let host = u.host_str().unwrap_or_default();
    u.scheme() == "https" && u.port().is_none() && (host == "i.scdn.co" || host.ends_with(".spotifycdn.com"))
}

/// `url` = the cover to show; None = back to the app icon. A repeat of the current url
/// does nothing. Errors are logged and leave the icon as it was.
#[tauri::command]
pub async fn set_dock_art(app: AppHandle, url: Option<String>) -> Result<(), String> {
    let mut current = CURRENT.lock().await;
    if *current == url {
        return Ok(());
    }
    let result = match &url {
        Some(u) => show(&app, u).await,
        None => on_main(&app, None).await,
    };
    match result {
        Ok(()) => {
            *current = url;
            Ok(())
        }
        Err(e) => {
            log::warn!("dock art: {e}");
            Err(e)
        }
    }
}

async fn show(app: &AppHandle, url: &str) -> Result<(), String> {
    if !art_url_ok(url) {
        return Err(format!("BAD_ARGS: not a Spotify image url: {url}"));
    }
    let resp = crate::auth::http().get(url).send().await.map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("download failed: {e}"))?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(format!("download failed: {} bytes", bytes.len()));
    }
    on_main(app, Some(bytes.to_vec())).await
}

/// Runs the AppKit part on the main thread and waits for its answer.
async fn on_main(app: &AppHandle, image: Option<Vec<u8>>) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(macos::set_icon(image.as_deref()));
    })
    .map_err(|e| format!("main thread: {e}"))?;
    rx.await.map_err(|_| "main thread: no answer".to_string())?
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::{rc::Retained, AnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSBezierPath, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace, NSGraphicsContext,
        NSImage, NSImageInterpolation,
    };
    use objc2_foundation::{NSData, NSPoint, NSRect, NSSize};

    const CANVAS: f64 = 1024.0;
    /// Apple's icon grid: the shape is ~80 % of the canvas, corners ~22 % of the shape.
    const MARGIN: f64 = 0.10;
    const CORNER: f64 = 0.22;

    /// `Some(bytes)` = an encoded image to draw as the icon; None = the app's own icon.
    pub fn set_icon(image: Option<&[u8]>) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
        let icon = match image {
            Some(bytes) => Some(rounded(bytes)?),
            None => None,
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: nil is documented to restore the bundle's icon; both values are valid
        unsafe { app.setApplicationIconImage(icon.as_deref()) };
        Ok(())
    }

    /// The cover drawn into a rounded rect with a margin, on a transparent canvas.
    fn rounded(bytes: &[u8]) -> Result<Retained<NSImage>, String> {
        let cover = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes)).ok_or("not an image")?;
        let side = CANVAS as isize;
        // SAFETY: null planes = AppKit allocates the pixel buffer itself
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                side,
                side,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )
        }
        .ok_or("no bitmap")?;
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).ok_or("no graphics context")?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        ctx.setImageInterpolation(NSImageInterpolation::High);
        let inset = CANVAS * MARGIN;
        let side_f = CANVAS - 2.0 * inset;
        let shape = NSRect::new(NSPoint::new(inset, inset), NSSize::new(side_f, side_f));
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(shape, side_f * CORNER, side_f * CORNER).addClip();
        let whole = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        cover.drawInRect_fromRect_operation_fraction(shape, whole, NSCompositingOperation::SourceOver, 1.0);
        ctx.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let icon = NSImage::initWithSize(NSImage::alloc(), NSSize::new(CANVAS, CANVAS));
        icon.addRepresentation(&rep);
        Ok(icon)
    }
}

#[cfg(not(target_os = "macos"))]
mod macos {
    pub fn set_icon(_image: Option<&[u8]>) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_spotify_image_hosts() {
        for ok in [
            "https://i.scdn.co/image/ab67616d0000b273abc",
            "https://image-cdn-ak.spotifycdn.com/image/ab67616d",
            "https://mosaic.scdn.co.spotifycdn.com/x",
        ] {
            assert!(art_url_ok(ok), "{ok}");
        }
        for bad in [
            "http://i.scdn.co/image/x",
            "https://i.scdn.co:8443/image/x",
            "https://evil.com/i.scdn.co",
            "https://i.scdn.co.evil.com/x",
            "https://spotifycdn.com/x",
            "https://evilspotifycdn.com/x",
            "file:///etc/passwd",
            "not a url",
            "",
        ] {
            assert!(!art_url_ok(bad), "{bad}");
        }
    }
}
