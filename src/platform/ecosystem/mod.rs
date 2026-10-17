pub mod hooks;
pub mod notifications;

use crate::eco::errors::EcoResult;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A phonebook hit served to linked peers (name + callable number).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactEntry {
    pub name: String,
    pub number: String,
}

/// Phonebook access, injected by the host. The desktop has no phonebook —
/// its factory returns a stub that yields no hits (it only queries peers).
pub trait PlatformContacts: Send + Sync {
    fn search(&self, query: &str, limit: u32) -> EcoResult<Vec<ContactEntry>>;
}

struct StubContacts;

impl PlatformContacts for StubContacts {
    fn search(&self, _query: &str, _limit: u32) -> EcoResult<Vec<ContactEntry>> {
        Ok(Vec::new())
    }
}

static CONTACTS_PROVIDER: std::sync::OnceLock<Arc<dyn PlatformContacts>> =
    std::sync::OnceLock::new();

/// Install the host phonebook seam once at node startup (mobile). The
/// desktop never calls this and keeps the empty stub.
pub fn set_contacts_provider(provider: Box<dyn PlatformContacts>) {
    let _ = CONTACTS_PROVIDER.set(Arc::from(provider));
}

fn contacts_provider() -> &'static Arc<dyn PlatformContacts> {
    static STUB: std::sync::OnceLock<Arc<dyn PlatformContacts>> = std::sync::OnceLock::new();
    CONTACTS_PROVIDER.get_or_init(|| {
        STUB.get_or_init(|| Arc::new(StubContacts) as Arc<dyn PlatformContacts>)
            .clone()
    })
}

/// Serve a phonebook query through the installed provider (stub on desktop).
/// Never panics: provider errors collapse to an empty hit list.
pub fn contacts_search(query: &str, limit: u32) -> Vec<ContactEntry> {
    contacts_provider()
        .search(query, limit)
        .unwrap_or_default()
}

/// Platform-specific clipboard access, injected by the host.
///
/// On Android this is backed by `ClipboardManager` and honors the Android 10+
/// restriction that clipboard reads only succeed while the app is foreground
/// (`get_text` returns an error/empty when backgrounded).
pub trait PlatformClipboard: Send + Sync {
    fn get_text(&self) -> EcoResult<String>;
    fn set_text(&self, text: &str) -> EcoResult<()>;
    /// Enterprise media: base64 payload + mime + file name.
    /// Default returns error so existing hosts keep working (text-only).
    fn set_media(&self, _base64: &str, _mime: &str, _file_name: &str) -> EcoResult<()> {
        Err(crate::eco::errors::EcoError::Clipboard("media not supported".to_string()))
    }
    fn get_media(&self) -> EcoResult<Option<(String, String, String)>> {
        Ok(None)
    }
}
