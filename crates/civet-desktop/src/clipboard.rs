//! The system clipboard from Rust: OSC 52 writes from a pane
//! (`Event::Clipboard`, no user gesture needed), the UI's Cmd+C
//! (`write_clipboard`) and Paste as File's read (`read_clipboard`; a
//! webview read would prompt).
//!
//! AppKit wants the main thread: callers run these from a synchronous
//! command or `run_on_main_thread`.

/// Put `text` on the system clipboard. Returns false if it could not.
pub fn write_text(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::write_text(text)
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!(
            len = text.len(),
            "clipboard write is only implemented on macOS"
        );
        false
    }
}

/// The clipboard's plain text, or None if it holds none.
pub fn read_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::read_text()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!("clipboard read is only implemented on macOS");
        None
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    pub(super) fn write_text(text: &str) -> bool {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let s = NSString::from_str(text);
        // SAFETY: `NSPasteboardTypeString` is an immutable NSString constant
        // exported by AppKit, which is loaded (Tauri runs an NSApplication).
        let ty = unsafe { NSPasteboardTypeString };
        pb.setString_forType(&s, ty)
    }

    pub(super) fn read_text() -> Option<String> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: as in `write_text`.
        let ty = unsafe { NSPasteboardTypeString };
        pb.stringForType(ty).map(|s| s.to_string())
    }
}
