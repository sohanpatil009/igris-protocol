pub mod notifications;

use crate::eco::errors::EcoResult;

/// Platform-specific clipboard access, injected by the host.
///
/// On Android this is backed by `ClipboardManager` and honors the Android 10+
/// restriction that clipboard reads only succeed while the app is foreground
/// (`get_text` returns an error/empty when backgrounded).
pub trait PlatformClipboard: Send + Sync {
    fn get_text(&self) -> EcoResult<String>;
    fn set_text(&self, text: &str) -> EcoResult<()>;
}
