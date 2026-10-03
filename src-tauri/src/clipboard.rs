//! Text to the macOS pasteboard from Rust. The webview's `navigator.clipboard` needs a fresh
//! click, and fetching the text from Rust first (an awaited invoke) has already used it up.

#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    write(&text)
}

#[cfg(target_os = "macos")]
fn write(text: &str) -> Result<(), String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: NSPasteboardTypeString is an immutable AppKit constant
    let kind = unsafe { NSPasteboardTypeString };
    if pasteboard.setString_forType(&NSString::from_str(text), kind) {
        Ok(())
    } else {
        Err("macOS refused the text".into())
    }
}

#[cfg(not(target_os = "macos"))]
fn write(_text: &str) -> Result<(), String> {
    Err("copying is only built for macOS".into())
}
