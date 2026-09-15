use crate::logger::dirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppearanceMode {
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

impl Default for AppearanceMode {
    fn default() -> Self {
        Self::System
    }
}

/// App-wide text size. The scale multiplies every font size the UI uses, so one
/// choice covers the transcript, the sidebar, the composer and Settings alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontSize {
    Small,
    Default,
    Large,
    ExtraLarge,
}

impl FontSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Default, Self::Large, Self::ExtraLarge];

    pub fn id(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Default => "default",
            Self::Large => "large",
            Self::ExtraLarge => "extra-large",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Default => "Default",
            Self::Large => "Large",
            Self::ExtraLarge => "Extra Large",
        }
    }

    /// Read back an [`Self::id`], for a frontend that hands the choice over as
    /// a string rather than as a serialized enum.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.id() == id)
    }

    /// Multiplier applied to every design-time font size. The steps stay modest
    /// on purpose: only the panes that are sized around their text (the sidebar,
    /// the settings label column) grow with the scale, so a larger jump starts
    /// pushing content out of the chrome that is still sized in plain pixels.
    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Default => 1.0,
            Self::Large => 1.15,
            Self::ExtraLarge => 1.3,
        }
    }
}

impl Default for FontSize {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    Hermes,
    OpenCode,
    Codex,
    ClaudeCode,
    Pi,
    OpenClaw,
    MiMoCode,
}

impl BackendKind {
    pub const ALL: [BackendKind; 7] = [
        BackendKind::Hermes,
        BackendKind::OpenCode,
        BackendKind::MiMoCode,
        BackendKind::Codex,
        BackendKind::ClaudeCode,
        BackendKind::Pi,
        BackendKind::OpenClaw,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            BackendKind::Hermes => "hermes",
            BackendKind::OpenCode => "opencode",
            BackendKind::Codex => "codex",
            BackendKind::ClaudeCode => "claudecode",
            BackendKind::Pi => "pi",
            BackendKind::OpenClaw => "openclaw",
            BackendKind::MiMoCode => "mimocode",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            BackendKind::Hermes => "Hermes",
            BackendKind::OpenCode => "OpenCode",
            BackendKind::Codex => "Codex CLI",
            BackendKind::ClaudeCode => "Claude Code",
            BackendKind::Pi => "Pi",
            BackendKind::OpenClaw => "OpenClaw",
            BackendKind::MiMoCode => "MiMoCode",
        }
    }

    pub fn default_port(&self) -> u16 {
        match self {
            BackendKind::Hermes => 9119,
            BackendKind::OpenCode | BackendKind::MiMoCode => 4096,
            BackendKind::OpenClaw => 18789,
            BackendKind::Codex | BackendKind::ClaudeCode | BackendKind::Pi => 0,
        }
    }

    pub fn uses_network_server(&self) -> bool {
        matches!(
            self,
            BackendKind::Hermes
                | BackendKind::OpenCode
                | BackendKind::OpenClaw
                | BackendKind::MiMoCode
        )
    }

    pub fn description(&self) -> &'static str {
        match self {
            BackendKind::Hermes => {
                "Connects to hermes serve; loopback instances are managed automatically."
            }
            BackendKind::OpenCode => "Connects to the structured HTTP/SSE API from opencode serve.",
            BackendKind::MiMoCode => "Connects to mimo serve using its OpenCode-compatible API.",
            BackendKind::OpenClaw => {
                "Connects directly to the OpenClaw Gateway WebSocket protocol."
            }
            BackendKind::Codex => "Runs codex app-server locally using its JSON-RPC protocol.",
            BackendKind::ClaudeCode => "Runs Claude Code locally using bidirectional stream-json.",
            BackendKind::Pi => "Runs Pi locally in RPC mode over JSONL.",
        }
    }

    pub fn credential_label(&self) -> &'static str {
        match self {
            BackendKind::Hermes => "Session token",
            BackendKind::OpenClaw => "Gateway token",
            _ => "Server password (optional)",
        }
    }
}

/// Persistent app settings. Stored as JSON in
/// `~/Library/Application Support/VanGoal/settings.json`
/// (UserDefaults equivalent for a non-bundled GPUI app).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub backend_kind: BackendKind,
    #[serde(default)]
    pub appearance: AppearanceMode,
    /// How large every font in the interface is drawn.
    #[serde(default)]
    pub font_size: FontSize,
    #[serde(default)]
    #[serde(skip_serializing)]
    pub session_token: String,
    /// Legacy global switch. Kept only so a settings file written before the
    /// per-backend switches existed still decides what to connect on launch;
    /// it is never written back, and the choice is materialised into each
    /// backend's own `enabled` flag by [`Settings::migrate_switches`].
    #[serde(default = "default_true")]
    #[serde(skip_serializing)]
    pub auto_connect: bool,
    #[serde(default)]
    pub selected_profile: String,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub backend_host: String,
    #[serde(default)]
    pub backend_port: u16,
    #[serde(default)]
    pub backend_use_tls: bool,
    #[serde(default)]
    pub debug_logging_enabled: bool,
    #[serde(default)]
    pub per_backend: std::collections::HashMap<String, PerBackendConnection>,
    /// Whether the session list was showing when the app was last used.
    #[serde(default = "default_true")]
    pub sidebar_open: bool,
    /// Whether the transcript draws the tool calls a turn ran, or only the text
    /// it produced. A long turn can run dozens of them, and on a phone they take
    /// more room than the reply they belong to.
    #[serde(default = "default_true")]
    pub show_tool_calls: bool,
    /// Where and how big the window was. `None` until it has been observed
    /// once, so a fresh install opens centred at its default size.
    #[serde(default)]
    pub window: Option<SavedWindow>,
    /// The session this client had open last, so a phone that was closed, locked
    /// or disconnected comes back to the conversation the user was in. Without
    /// it every launch starts an empty chat, and the first thing typed creates
    /// *another* session — which is how one conversation's context goes missing
    /// and looks like messages landing in the wrong chat.
    #[serde(default)]
    pub last_session: Option<SavedSession>,
}

/// The session a client had open, and the backend that issued its key. A
/// session key means nothing to a backend that did not create it, so the pair
/// travels together and is only restored onto the backend that made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    pub backend: String,
    pub id: String,
}

/// Window geometry remembered across launches. Kept as plain numbers so this
/// module stays free of GPUI types and stays testable without a window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The window was zoomed. `width` and `height` still hold the size to
    /// restore to when it is un-zoomed.
    #[serde(default)]
    pub maximized: bool,
}

impl SavedWindow {
    /// Reject geometry that would open the window unusably small or absurdly
    /// large, which is what a stale file costs after the display setup changes.
    /// A rejected record simply falls back to the default size.
    pub fn is_plausible(&self) -> bool {
        const MIN_WIDTH: f32 = 560.0;
        const MIN_HEIGHT: f32 = 480.0;
        const MAX: f32 = 20_000.0;
        let size_ok = |value: f32, min: f32| value.is_finite() && (min..=MAX).contains(&value);
        size_ok(self.width, MIN_WIDTH)
            && size_ok(self.height, MIN_HEIGHT)
            && self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() <= MAX
            && self.y.abs() <= MAX
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PerBackendConnection {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub use_tls: bool,
    #[serde(default)]
    pub credential: String,
    /// This backend's own on/off switch. `None` means the file was written
    /// before per-backend switches existed; [`Settings::migrate_switches`]
    /// fills those in on first load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

fn default_true() -> bool {
    true
}

/// Write a file only the current user can read. Settings hold credentials, so
/// the mode is applied at creation time rather than fixed up afterwards.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 9119;

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend_kind: BackendKind::Hermes,
            appearance: AppearanceMode::System,
            font_size: FontSize::default(),
            session_token: String::new(),
            auto_connect: true,
            selected_profile: String::new(),
            workspace_path: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            backend_host: DEFAULT_HOST.to_string(),
            backend_port: DEFAULT_PORT,
            backend_use_tls: false,
            debug_logging_enabled: false,
            per_backend: std::collections::HashMap::new(),
            sidebar_open: true,
            show_tool_calls: true,
            window: None,
            last_session: None,
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        dirs::app_dir().join("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    fn load_from(path: &std::path::Path) -> Self {
        let mut settings = match std::fs::read(path) {
            Ok(data) => match serde_json::from_slice::<Settings>(&data) {
                Ok(settings) => settings,
                Err(error) => {
                    // Never overwrite a file we could not parse. Silently falling
                    // back to the defaults is what makes the app forget which
                    // backend the user was on, so keep the bad file for
                    // inspection and let the user re-enter their settings.
                    crate::log_debug!("settings", "settings.json unreadable: {error}");
                    let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        };
        // Restore the scoped credential into the flat fields *before* anything
        // writes those fields back, otherwise the empty `session_token` that
        // deserialization produces would overwrite the stored credential.
        settings.apply_scoped_connection();
        settings.migrate_switches();
        settings.save_to(path);
        settings
    }

    /// Fold the per-backend record of the active backend into the flat fields
    /// the rest of the app reads.
    fn apply_scoped_connection(&mut self) {
        let scoped = self
            .per_backend
            .get(self.backend_kind.id())
            .cloned()
            .unwrap_or_default();
        if self.backend_host.is_empty() {
            self.backend_host = if scoped.host.is_empty() {
                DEFAULT_HOST.to_string()
            } else {
                scoped.host
            };
        }
        if self.backend_port == 0 {
            self.backend_port = if scoped.port > 0 {
                scoped.port
            } else {
                self.backend_kind.default_port()
            };
        }
        if !scoped.credential.is_empty() {
            self.session_token = scoped.credential;
        }
    }

    /// Give a settings file written before per-backend switches existed an
    /// explicit switch per backend: the backend the user was last on inherits
    /// the old global `auto_connect`, everything else starts switched off.
    /// Without this an upgrade would either connect every backend or none.
    fn migrate_switches(&mut self) {
        if self
            .per_backend
            .values()
            .any(|connection| connection.enabled.is_some())
        {
            return;
        }
        let active = self.backend_kind;
        let was_auto_connect = self.auto_connect;
        self.sync_active_connection();
        for kind in BackendKind::ALL {
            let enabled = kind == active && was_auto_connect;
            self.per_backend
                .entry(kind.id().to_string())
                .or_default()
                .enabled = Some(enabled);
        }
    }

    /// Mirror the flat fields of the backend currently being edited into its
    /// per-backend record, so that writing another backend's switch cannot
    /// drop the host, port, TLS or credential the user just typed.
    fn sync_active_connection(&mut self) {
        let enabled = self
            .per_backend
            .get(self.backend_kind.id())
            .and_then(|connection| connection.enabled);
        self.per_backend.insert(
            self.backend_kind.id().to_string(),
            PerBackendConnection {
                host: self.backend_host.clone(),
                port: self.backend_port,
                use_tls: self.backend_use_tls,
                credential: self.session_token.trim().to_string(),
                enabled,
            },
        );
    }

    /// Whether this backend's switch is on. Only one backend can be connected
    /// at a time, so `true` here means "this is the backend the app uses".
    pub fn is_backend_enabled(&self, kind: BackendKind) -> bool {
        self.per_backend
            .get(kind.id())
            .and_then(|connection| connection.enabled)
            .unwrap_or(false)
    }

    /// Characters of credential held for a backend. The active backend keeps
    /// its token in the flat field the Settings editor writes to; every other
    /// backend keeps its own copy, so read whichever is authoritative.
    pub fn stored_credential_characters(&self, kind: BackendKind) -> usize {
        let scoped = self
            .per_backend
            .get(kind.id())
            .map(|connection| connection.credential.trim().chars().count())
            .unwrap_or(0);
        if kind == self.backend_kind {
            // The scoped copy can lag the flat field by one keystroke, so report
            // the larger of the two rather than briefly claiming nothing is saved.
            return scoped.max(self.session_token.trim().chars().count());
        }
        scoped
    }

    /// Flip one backend's switch. Turning a backend on turns every other one
    /// off: a stale on-flag would otherwise reconnect a backend the user had
    /// deliberately disconnected. The caller persists via [`Settings::save`],
    /// so this stays pure and cannot touch a real file from a test.
    pub fn set_backend_enabled(&mut self, kind: BackendKind, enabled: bool) {
        self.sync_active_connection();
        if enabled {
            for other in BackendKind::ALL {
                if other != kind {
                    if let Some(connection) = self.per_backend.get_mut(other.id()) {
                        connection.enabled = Some(false);
                    }
                }
            }
        }
        self.per_backend
            .entry(kind.id().to_string())
            .or_default()
            .enabled = Some(enabled);
    }

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    fn save_to(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut persisted = self.clone();
        persisted.sync_active_connection();
        let Ok(data) = serde_json::to_vec_pretty(&persisted) else {
            return;
        };
        // Write a sibling file and rename it into place. `save` runs on every
        // keystroke in Settings, and a plain write that is interrupted leaves
        // truncated JSON behind — which the next launch reads as "no settings"
        // and resets to the defaults.
        let temporary = path.with_extension("json.tmp");
        if write_private(&temporary, &data).is_err() {
            return;
        }
        let _ = std::fs::rename(&temporary, path);
    }

    /// Persist the outgoing backend's connection under its scoped key and load
    /// the incoming one, mirroring the didSet logic in the SwiftUI store.
    pub fn switch_backend(&mut self, next: BackendKind) {
        self.sync_active_connection();
        self.session_token.clear();
        self.backend_kind = next;
        let scoped = self.per_backend.get(next.id()).cloned().unwrap_or_default();
        self.backend_host = if scoped.host.is_empty() {
            DEFAULT_HOST.to_string()
        } else {
            scoped.host
        };
        self.backend_port = if scoped.port > 0 {
            scoped.port
        } else {
            next.default_port()
        };
        self.backend_use_tls = scoped.use_tls;
        self.session_token = scoped.credential;
    }

    pub fn resolved_host(&self) -> String {
        let value = self.backend_host.trim();
        if value.is_empty() {
            DEFAULT_HOST.to_string()
        } else {
            value.to_string()
        }
    }

    pub fn resolved_port(&self) -> u16 {
        if self.backend_port > 0 {
            self.backend_port
        } else {
            self.backend_kind.default_port()
        }
    }

    pub fn active_backend_url(&self) -> String {
        let host = self.resolved_host();
        if let Ok(mut url) = url::Url::parse(&host) {
            let normalized_scheme = match url.scheme() {
                "ws" | "http" => Some("http"),
                "wss" | "https" => Some("https"),
                _ => None,
            };
            if let Some(scheme) = normalized_scheme {
                let _ = url.set_scheme(scheme);
                return url.to_string();
            }
        }
        let scheme = if self.backend_use_tls {
            "https"
        } else {
            "http"
        };
        format!("{}://{}:{}", scheme, host, self.resolved_port())
    }

    /// Only loopback Hermes addresses are managed (auto-started) by Van-Goal.
    pub fn is_managed_local_backend(&self) -> bool {
        if self.backend_kind != BackendKind::Hermes
            || self.backend_use_tls
            || url::Url::parse(&self.resolved_host()).is_ok()
        {
            return false;
        }
        matches!(
            self.resolved_host().to_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "::1"
        )
    }

    pub fn normalized_profile(&self) -> Option<String> {
        let value = self.selected_profile.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }

    pub fn workspace_trimmed(&self) -> Option<String> {
        let value = self.workspace_path.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BackendKind, FontSize, SavedWindow, Settings};

    #[test]
    fn full_websocket_url_preserves_reverse_proxy_path() {
        let settings = Settings {
            backend_kind: BackendKind::OpenClaw,
            backend_host: "wss://liuxl.com.cn/openclaw/".into(),
            backend_port: 18789,
            backend_use_tls: false,
            ..Settings::default()
        };

        assert_eq!(
            settings.active_backend_url(),
            "https://liuxl.com.cn/openclaw/"
        );
    }

    #[test]
    fn bare_host_still_uses_port_and_tls_fields() {
        let settings = Settings {
            backend_kind: BackendKind::OpenClaw,
            backend_host: "claw.example.com".into(),
            backend_port: 8443,
            backend_use_tls: true,
            ..Settings::default()
        };

        assert_eq!(
            settings.active_backend_url(),
            "https://claw.example.com:8443"
        );
    }

    /// A settings file in a private directory. Every test here goes through an
    /// explicit path, so none of them can touch the real
    /// `~/Library/Application Support/VanGoal/settings.json`.
    struct TempSettings(std::path::PathBuf);

    impl TempSettings {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("van-goal-settings-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir.join("settings.json"))
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempSettings {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    /// The text-size steps must actually grow, and the default step must leave
    /// the interface exactly as designed: every layout test in the tree is
    /// written against a scale of 1.0.
    #[test]
    fn font_size_steps_are_unique_and_grow_from_a_neutral_default() {
        assert_eq!(FontSize::default().scale(), 1.0);

        let scales: Vec<f32> = FontSize::ALL.iter().map(|size| size.scale()).collect();
        assert!(
            scales.windows(2).all(|pair| pair[1] > pair[0]),
            "each step must be larger than the one before it: {scales:?}"
        );

        let ids: std::collections::HashSet<&str> =
            FontSize::ALL.iter().map(|size| size.id()).collect();
        assert_eq!(
            ids.len(),
            FontSize::ALL.len(),
            "ids become element ids, so they must stay unique"
        );
    }

    /// A settings file written before the option existed has no `font_size`
    /// key, so it has to load at the default rather than fail.
    #[test]
    fn a_settings_file_without_a_font_size_loads_at_the_default() {
        let temp = TempSettings::new();
        std::fs::write(temp.path(), br#"{"backend_kind": "Hermes"}"#).expect("write settings");

        let settings = Settings::load_from(temp.path());

        assert_eq!(settings.font_size, FontSize::default());
    }

    /// The choice has to survive the round trip through disk.
    #[test]
    fn the_font_size_survives_a_save_and_reload() {
        let temp = TempSettings::new();
        let settings = Settings {
            font_size: FontSize::ExtraLarge,
            ..Settings::default()
        };
        settings.save_to(temp.path());

        assert_eq!(
            Settings::load_from(temp.path()).font_size,
            FontSize::ExtraLarge
        );
    }

    #[test]
    fn only_one_backend_switch_is_on_at_a_time() {
        let mut settings = Settings::default();
        settings.set_backend_enabled(BackendKind::OpenClaw, true);
        assert!(settings.is_backend_enabled(BackendKind::OpenClaw));
        assert!(!settings.is_backend_enabled(BackendKind::Hermes));

        // Turning another backend on switches, it does not add: the backend the
        // user came from must end up switched off.
        settings.set_backend_enabled(BackendKind::Hermes, true);
        assert!(settings.is_backend_enabled(BackendKind::Hermes));
        assert!(!settings.is_backend_enabled(BackendKind::OpenClaw));
    }

    #[test]
    fn switching_a_backend_off_leaves_every_backend_off() {
        let mut settings = Settings::default();
        settings.set_backend_enabled(BackendKind::OpenClaw, true);
        settings.set_backend_enabled(BackendKind::OpenClaw, false);

        // Disconnecting is what the user asked for, so nothing may come back on
        // by itself on the next launch.
        assert!(BackendKind::ALL
            .iter()
            .all(|kind| !settings.is_backend_enabled(*kind)));
    }

    #[test]
    fn a_legacy_settings_file_migrates_to_a_single_enabled_backend() {
        let temp = TempSettings::new();
        // Written before per-backend switches existed: no `enabled` key
        // anywhere, only the global auto_connect.
        std::fs::write(
            temp.path(),
            br#"{
                "backend_kind": "OpenClaw",
                "auto_connect": true,
                "backend_host": "claw.example.com",
                "backend_port": 18789,
                "per_backend": {}
            }"#,
        )
        .expect("write legacy settings");

        let settings = Settings::load_from(temp.path());
        assert_eq!(settings.backend_kind, BackendKind::OpenClaw);
        assert!(
            settings.is_backend_enabled(BackendKind::OpenClaw),
            "the backend the user was last on stays on"
        );
        assert_eq!(
            BackendKind::ALL
                .iter()
                .filter(|kind| settings.is_backend_enabled(**kind))
                .count(),
            1,
            "an upgraded install must come up with exactly one backend on"
        );
    }

    /// The shape of a real settings file written by the version before
    /// per-backend switches: every backend has a scoped record, none of them has
    /// an `enabled` key, and only the global `auto_connect` says what to do.
    /// Upgrading must come up on the backend the user was last using, with the
    /// others switched off, and must not lose the stored host or credential.
    #[test]
    fn a_real_legacy_file_upgrades_without_losing_the_active_backend() {
        let temp = TempSettings::new();
        std::fs::write(
            temp.path(),
            br#"{
              "backend_kind": "OpenClaw",
              "appearance": "System",
              "auto_connect": true,
              "selected_profile": "",
              "workspace_path": "/tmp",
              "backend_host": "liuxl.example.com",
              "backend_port": 18789,
              "backend_use_tls": true,
              "debug_logging_enabled": false,
              "per_backend": {
                "hermes":     { "host": "127.0.0.1", "port": 9119,  "use_tls": false, "credential": "hermes-token" },
                "opencode":   { "host": "127.0.0.1", "port": 4096,  "use_tls": false, "credential": "" },
                "mimocode":   { "host": "127.0.0.1", "port": 4096,  "use_tls": false, "credential": "" },
                "codex":      { "host": "127.0.0.1", "port": 0,     "use_tls": false, "credential": "" },
                "claudecode": { "host": "127.0.0.1", "port": 0,     "use_tls": false, "credential": "" },
                "pi":         { "host": "127.0.0.1", "port": 0,     "use_tls": false, "credential": "" },
                "openclaw":   { "host": "liuxl.example.com", "port": 18789, "use_tls": true, "credential": "" }
              }
            }"#,
        )
        .expect("write legacy settings");

        let settings = Settings::load_from(temp.path());
        assert_eq!(settings.backend_kind, BackendKind::OpenClaw);
        assert!(settings.is_backend_enabled(BackendKind::OpenClaw));
        assert_eq!(
            BackendKind::ALL
                .iter()
                .filter(|kind| settings.is_backend_enabled(**kind))
                .count(),
            1,
            "an upgraded install must come up with exactly one backend on"
        );
        // The address and the other backends' credentials must survive.
        assert_eq!(settings.backend_host, "liuxl.example.com");
        assert_eq!(settings.backend_port, 18789);
        assert!(settings.backend_use_tls);
        assert_eq!(
            settings.stored_credential_characters(BackendKind::Hermes),
            "hermes-token".chars().count(),
            "another backend's stored token must not be wiped by the upgrade"
        );
    }

    #[test]
    fn a_legacy_install_with_auto_connect_off_stays_disconnected() {
        let temp = TempSettings::new();
        std::fs::write(
            temp.path(),
            br#"{"backend_kind": "OpenClaw", "auto_connect": false}"#,
        )
        .expect("write legacy settings");

        let settings = Settings::load_from(temp.path());
        assert!(BackendKind::ALL
            .iter()
            .all(|kind| !settings.is_backend_enabled(*kind)));
    }

    #[test]
    fn an_unreadable_settings_file_is_preserved_instead_of_reset() {
        let temp = TempSettings::new();
        let broken = b"{ this is not json";
        std::fs::write(temp.path(), broken).expect("write broken settings");

        let settings = Settings::load_from(temp.path());
        assert_eq!(settings.backend_kind, BackendKind::Hermes);

        // The unreadable file is kept for inspection rather than deleted, and
        // the file that replaced it is valid.
        let backup = temp.path().with_extension("json.corrupt");
        assert_eq!(
            std::fs::read(&backup).expect("the unreadable file must be kept"),
            broken
        );
        let rewritten = std::fs::read(temp.path()).expect("rewritten settings");
        serde_json::from_slice::<Settings>(&rewritten).expect("rewritten file parses");
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let temp = TempSettings::new();
        Settings::default().save_to(temp.path());

        assert!(temp.path().exists());
        assert!(
            !temp.path().with_extension("json.tmp").exists(),
            "the temporary file is renamed into place, never left behind"
        );
    }

    #[test]
    fn each_backend_keeps_its_own_credential_across_a_switch() {
        let temp = TempSettings::new();
        let mut settings = Settings {
            backend_kind: BackendKind::OpenClaw,
            backend_host: "claw.example.com".into(),
            backend_port: 18789,
            session_token: "openclaw-secret".into(),
            ..Settings::default()
        };
        settings.save_to(temp.path());

        settings.switch_backend(BackendKind::Hermes);
        settings.session_token = "hermes-secret".into();
        settings.save_to(temp.path());

        // Reload, then switch back: each backend comes up with its own token and
        // address, which is what makes the separate switches usable.
        let mut reloaded = Settings::load_from(temp.path());
        assert_eq!(reloaded.session_token, "hermes-secret");
        reloaded.switch_backend(BackendKind::OpenClaw);
        assert_eq!(reloaded.session_token, "openclaw-secret");
        assert_eq!(reloaded.backend_host, "claw.example.com");
        assert_eq!(reloaded.backend_port, 18789);
    }

    #[test]
    fn switching_backend_keeps_each_switch_as_it_was() {
        let temp = TempSettings::new();
        let mut settings = Settings::default();
        settings.set_backend_enabled(BackendKind::OpenClaw, true);

        settings.switch_backend(BackendKind::Hermes);
        settings.switch_backend(BackendKind::OpenClaw);
        settings.save_to(temp.path());

        // Switching must not turn a backend on or off behind the user's back.
        assert!(settings.is_backend_enabled(BackendKind::OpenClaw));
        assert!(!settings.is_backend_enabled(BackendKind::Hermes));
    }

    #[test]
    fn window_geometry_and_the_sidebar_survive_a_round_trip() {
        let temp = TempSettings::new();
        let settings = Settings {
            sidebar_open: false,
            window: Some(SavedWindow {
                x: 120.0,
                y: 80.0,
                width: 1440.0,
                height: 900.0,
                maximized: true,
            }),
            ..Settings::default()
        };
        settings.save_to(temp.path());

        let reloaded = Settings::load_from(temp.path());
        assert_eq!(reloaded.window, settings.window);
        assert!(
            !reloaded.sidebar_open,
            "the collapsed sidebar was forgotten"
        );
    }

    #[test]
    fn a_fresh_install_opens_the_sidebar_and_centres_the_window() {
        let settings = Settings::default();
        assert!(settings.sidebar_open);
        assert_eq!(
            settings.window, None,
            "with no geometry saved the window opens at its default size"
        );
    }

    /// A saved position can outlive the display it was saved on, so geometry
    /// that would open the window unusably is rejected rather than restored.
    #[test]
    fn implausible_window_geometry_is_rejected() {
        let with_size = |width: f32, height: f32| SavedWindow {
            x: 0.0,
            y: 0.0,
            width,
            height,
            maximized: false,
        };
        assert!(with_size(1180.0, 760.0).is_plausible());
        assert!(!with_size(100.0, 760.0).is_plausible(), "too narrow");
        assert!(!with_size(1180.0, 100.0).is_plausible(), "too short");
        assert!(!with_size(50_000.0, 760.0).is_plausible(), "absurdly wide");
        assert!(!with_size(f32::NAN, 760.0).is_plausible(), "not a number");
    }
}
