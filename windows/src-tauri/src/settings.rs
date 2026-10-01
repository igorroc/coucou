// Preferences, stored as plain JSON in %APPDATA%\Coucou\settings.json.
// No secret ever lands here — API keys live in the Windows Credential Manager.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    /// Width of the island in compact mode, logical px. Defaulted so older
    /// settings.json still loads.
    #[serde(default = "default_compact_width")]
    pub compact_width: f64,
    /// When true the island never auto-closes. Defaulted so older settings.json still loads.
    #[serde(default)]
    pub keep_visible: bool,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    /// Claude model used by the chat. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
    /// Chat backend: "claude" (Anthropic API) or "opencode" (local CLI).
    #[serde(default = "default_chat_provider")]
    pub chat_provider: String,
    /// Optional explicit path to opencode.exe; empty = auto-detect.
    #[serde(default)]
    pub opencode_bin: String,
    /// Optional `provider/model` override for opencode chat; empty = its default.
    #[serde(default)]
    pub opencode_model: String,
    /// Allow sending new messages in conversations started outside the notch
    /// (in a repository). Off by default: those chats open read-only.
    #[serde(default)]
    pub allow_repo_chat: bool,
    /// Per-agent body colour overrides (`integration_*` id → `#rrggbb`). Empty
    /// entries fall back to the built-in colours.
    #[serde(default)]
    pub agent_colors: HashMap<String, String>,
    /// Body colour of the main Mochi (`#rrggbb`); empty = the built-in gradient.
    #[serde(default)]
    pub mochi_color: String,
    /// Show the VS Code (Claude Code) pill. Defaults on so older settings.json
    /// (written before it was toggleable) keep showing it.
    #[serde(default = "default_true")]
    pub vscode_pill: bool,
    /// Display name of the assistant. Empty = the built-in "Mochi".
    #[serde(default)]
    pub assistant_name: String,
    /// Who the user is: background, skills, preferences. Rides along with every
    /// answer so the assistant can tailor what it does and how it says it.
    #[serde(default)]
    pub about_user: String,
    /// How the assistant should behave and what it should prioritise. This is
    /// the instruction half, kept apart from who the user is.
    #[serde(default)]
    pub about_assistant: String,
    /// Legacy single instruction. Read-only: `load()` folds it into
    /// `about_assistant`, and it is never written back.
    #[serde(default, skip_serializing)]
    pub master_instruction: String,
    /// Cached "Sugestões" for the dashboard, generated from the name + master
    /// instruction. Empty = never generated (the fixture is shown instead).
    #[serde(default)]
    pub assistant_suggestions: Vec<SuggestedAction>,
    /// Enabled dashboard news categories (ids from `news::categories`).
    #[serde(default = "default_news_categories")]
    pub news_categories: Vec<String>,
}

/// One dashboard suggestion: an icon key, a short label and the prompt sent to
/// the chat when it is clicked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedAction {
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub label: String,
    /// The prompt sent to the chat. `None` for older cached payloads.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// The assistant's display name, falling back to the built-in one.
pub fn assistant_name(settings: &Settings) -> String {
    let name = settings.assistant_name.trim();
    if name.is_empty() {
        "Mochi".to_string()
    } else {
        name.to_string()
    }
}

fn default_true() -> bool {
    true
}

fn default_news_categories() -> Vec<String> {
    crate::news::DEFAULT_CATEGORIES.iter().map(|s| s.to_string()).collect()
}

fn default_chat_provider() -> String {
    "claude".into()
}

/// NOTCH_W + 104, the compact width from docs/SPEC.md (see layout.ts).
fn default_compact_width() -> f64 {
    288.0
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            compact_width: default_compact_width(),
            keep_visible: false,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            model: default_model(),
            chat_provider: default_chat_provider(),
            opencode_bin: String::new(),
            opencode_model: String::new(),
            allow_repo_chat: false,
            agent_colors: HashMap::new(),
            mochi_color: String::new(),
            vscode_pill: true,
            assistant_name: String::new(),
            about_user: String::new(),
            about_assistant: String::new(),
            master_instruction: String::new(),
            assistant_suggestions: Vec::new(),
            news_categories: default_news_categories(),
        }
    }
}

/// %APPDATA%\Coucou
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

/// %LOCALAPPDATA%\Coucou — where coucou-hook.exe and the log live.
pub fn local_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join("coucou-hook.exe")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let mut settings = match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    };
    // Migration: the old single "master instruction" becomes "about the
    // assistant" (it was used as behaviour/context). Only when both new fields
    // are still empty, so a real edit is never overwritten.
    if settings.about_user.trim().is_empty()
        && settings.about_assistant.trim().is_empty()
        && !settings.master_instruction.trim().is_empty()
    {
        settings.about_assistant = settings.master_instruction.clone();
    }
    settings
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}
