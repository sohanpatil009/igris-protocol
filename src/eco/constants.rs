/// Default TCP port for ecosystem HTTP server
pub const DEFAULT_ECO_PORT: u16 = 53327;

/// TLS proxy port wrapping the HTTP ecosystem server
pub const ECO_TLS_PORT: u16 = 53328;

/// Port range to try when binding (port..port+RANGE)
pub const PORT_RANGE: u16 = 10;

/// How often (seconds) a device broadcasts its presence
pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// How long (seconds) without a heartbeat before a device is considered offline
pub const DEVICE_TIMEOUT_SECS: u64 = 120;

/// How often (seconds) we check local clipboard for changes
pub const CLIPBOARD_POLL_INTERVAL_SECS: u64 = 1;

/// Maximum size (bytes) for a single clipboard image payload
pub const MAX_CLIPBOARD_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum size (bytes) for a single clipboard text payload
pub const MAX_CLIPBOARD_TEXT_BYTES: u64 = 1024 * 1024;

/// Maximum number of clipboard history entries to keep
pub const CLIPBOARD_HISTORY_MAX: usize = 50;

/// Protocol version for ecosystem messages
pub const ECO_PROTOCOL_VERSION: &str = "1.0.0";

/// Ecosystem storage directory name under pkg/
pub const ECO_STORAGE_DIR: &str = "ecosystem";

/// Ecosystem config file name
pub const ECO_CONFIG_FILE: &str = "ecosystem_config.json";

/// Trusted devices file name
pub const TRUSTED_DEVICES_FILE: &str = "trusted_devices.json";

/// Clipboard history file name
pub const CLIPBOARD_HISTORY_FILE: &str = "clipboard_history.json";

/// Timeout (seconds) for HTTP requests to peer devices
pub const PEER_REQUEST_TIMEOUT_SECS: u64 = 5;

/// Maximum number of devices to track simultaneously
pub const MAX_TRACKED_DEVICES: usize = 50;

/// Maximum number of notification history entries to keep
pub const NOTIFICATION_HISTORY_MAX: usize = 100;

/// Notification history file name
pub const NOTIFICATION_HISTORY_FILE: &str = "notification_history.json";
