// Coucou for Windows — app wiring and the commands the island calls.

mod assistant;
mod browse;
mod calendar;
mod claude;
mod files;
mod hooks;
mod integrations;
mod island;
mod jira;
mod log;
mod mcp;
mod news;
mod opencode;
mod opencode_chat;
mod opencode_sessions;
mod pipe;
mod secrets;
mod settings;
mod tray;
mod win_user;

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use claude::{Chat, ChatContext, ChatReply};
use files::DroppedFile;
use hooks::{HookPreview, HookStatus};
use island::{PollGate, ScreenInfo};
use opencode::{OpencodePreview, OpencodeStatus};
use pipe::Pending;
use settings::Settings;

/// Keeps spawned helpers from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub gate: Arc<PollGate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    settings: Settings,
    screen: ScreenInfo,
    version: String,
    hook_path: String,
}

#[tauri::command]
fn boot(app: AppHandle, shared: State<Shared>) -> BootInfo {
    let mut settings = shared.settings.lock().unwrap().clone();
    // The real state of ~/.claude/settings.json wins over whatever we stored.
    settings.hooks_installed = hooks::status().installed;
    let screen = island::screen_info(&app, &settings.screen);
    BootInfo {
        settings,
        screen,
        version: env!("CARGO_PKG_VERSION").to_string(),
        hook_path: settings::hook_exe_path().to_string_lossy().to_string(),
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Shared>, settings: Settings) {
    let (screen_changed, autostart_changed) = {
        let mut current = shared.settings.lock().unwrap();
        let screen_changed = current.screen != settings.screen;
        let autostart_changed = current.autostart != settings.autostart;
        *current = settings.clone();
        (screen_changed, autostart_changed)
    };
    if let Err(err) = settings::save(&settings) {
        eprintln!("[coucou] could not save settings: {err}");
    }
    if autostart_changed {
        let manager = app.autolaunch();
        let result = if settings.autostart { manager.enable() } else { manager.disable() };
        if let Err(err) = result {
            eprintln!("[coucou] autostart: {err}");
        }
    }
    if screen_changed {
        let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
        island::apply_geometry(&app, &settings.screen, collapsed);
    }
    // Keep the other window in step (island ⇄ settings window).
    let _ = app.emit("settings-changed", settings);
}

/// Hidden island → shrink the window to the invisible wake strip and park the
/// cursor poll; anything else → full panel and 60 Hz polling.
#[tauri::command]
fn set_collapsed(app: AppHandle, shared: State<Shared>, collapsed: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    shared.gate.collapsed.store(collapsed, Ordering::Relaxed);
    // Park the poll BEFORE touching click-through. Otherwise a poll tick that was
    // already awake recomputes the flag from the cursor against the just-resized
    // window and re-enables click-through on its way out — which leaves the wake
    // strip unable to receive the hover that should bring the island back.
    if collapsed {
        shared.gate.set_active(false);
    }
    island::apply_geometry(&app, &pref, collapsed);
    // The wake strip must always take the mouse, and a resize invalidates the flag.
    island::set_ignore_cursor(&app, false);
    shared.gate.forget_ignore_state();
    if !collapsed {
        shared.gate.set_active(true);
    }
}

/// The front end pushes the island shape; Rust decides click-through from it.
#[tauri::command]
fn set_island_rect(shared: State<Shared>, x: f64, y: f64, width: f64, height: f64) {
    shared.gate.set_rect(island::IslandRect { x, y, w: width, h: height });
}

#[tauri::command]
fn focus_window(app: AppHandle, focused: bool) {
    let Some(win) = island::window(&app) else { return };
    island::set_activating(&win, focused);
    if focused {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn reposition(app: AppHandle, shared: State<Shared>) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
}

#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let _ = Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// "Open terminal" opens the working folder in VS Code when `code` is on PATH,
/// and falls back to Explorer otherwise.
#[tauri::command]
fn open_in_vscode(path: Option<String>) -> bool {
    // No `cmd /C` anywhere near this. The path is a project folder chosen by
    // whoever is using Claude Code, and cmd would happily read `&`, `^` and `%`
    // in a folder name as syntax. Finding the launcher ourselves and handing the
    // path over as a separate argument keeps it a path.
    if let Some(code) = find_on_path("code") {
        let mut cmd = Command::new(code);
        if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
            cmd.arg(p);
        }
        if cmd.creation_flags(CREATE_NO_WINDOW).spawn().is_ok() {
            return true;
        }
    }
    if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
        let _ = Command::new("explorer").arg(p).spawn();
    }
    false
}

/// Our own `where`: walks %PATH% against %PATHEXT%, no shell involved.
/// Rust quotes arguments correctly for `.cmd`/`.bat` targets since 1.77, so
/// spawning `code.cmd` directly is safe.
pub(crate) fn find_on_path(stem: &str) -> Option<std::path::PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{stem}{}", ext.to_lowercase()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Tray → Pause. Paused means paused: the pollers stop talking to the network,
/// not just the island stopping showing things.
#[tauri::command]
fn set_paused(paused: bool) {
    integrations::set_paused(paused);
}

// ── Claude Code hooks ─────────────────────────────────────────────────────────

#[tauri::command]
fn hooks_status() -> HookStatus {
    hooks::status()
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn hooks_preview(install: bool) -> Result<HookPreview, String> {
    hooks::preview(install)
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn hooks_apply(
    app: AppHandle,
    shared: State<Shared>,
    install: bool,
    fingerprint: String,
) -> Result<String, String> {
    // The fingerprint comes from the preview the user actually looked at, so a
    // settings.json that changed in between is refused rather than overwritten.
    let backup = hooks::write(install, &fingerprint)?;
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        current.hooks_installed = install;
        let _ = settings::save(&current);
        current.clone()
    };
    let _ = app.emit("settings-changed", updated);
    Ok(backup)
}

// ── opencode plugin ─────────────────────────────────────────────────────────

#[tauri::command]
fn opencode_status() -> OpencodeStatus {
    opencode::status()
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn opencode_preview(install: bool) -> Result<OpencodePreview, String> {
    opencode::preview(install)
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn opencode_apply(install: bool, fingerprint: String) -> Result<String, String> {
    opencode::write(install, &fingerprint)
}

#[tauri::command]
fn approval_decision(app: AppHandle, request_id: String, decision: String) {
    pipe::answer(&app, &request_id, &decision);
}
/// The island has the card on screen, so the long wait for a human may begin.
/// Until this arrives the relay only waits a few hundred milliseconds, which is
/// what stops a paused or unresponsive island from freezing Claude Code.
#[tauri::command]
fn approval_ack(app: AppHandle, request_id: String) {
    pipe::acknowledge(&app, &request_id);
}

/// Nobody can act on this request — the island is paused, or another card is
/// already up. Claude Code falls back to asking in the terminal immediately.
#[tauri::command]
fn approval_decline(app: AppHandle, request_id: String) {
    pipe::decline(&app, &request_id);
}

// ── Chat, files and secrets ───────────────────────────────────────────────────

/// One chat turn. The API key and any file bytes stay on the Rust side.
/// Routes to the user's opencode when Settings → Chat says so.
#[tauri::command]
async fn chat_send(
    shared: State<'_, Shared>,
    chat: State<'_, Chat>,
    ochat: State<'_, opencode_chat::OpencodeChat>,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let (provider, model, bin, omodel, name, about_user, about_assistant) = {
        let s = shared.settings.lock().unwrap();
        (
            s.chat_provider.clone(),
            s.model.clone(),
            s.opencode_bin.clone(),
            s.opencode_model.clone(),
            settings::assistant_name(&s),
            s.about_user.clone(),
            s.about_assistant.clone(),
        )
    };
    if provider == "opencode" {
        opencode_chat::send(
            &ochat, &bin, &omodel, &name, &about_user, &about_assistant, query, context,
        )
        .await
    } else {
        claude::send(&chat, &model, &name, &about_user, &about_assistant, query, context).await
    }
}

#[tauri::command]
fn chat_reset(chat: State<Chat>, ochat: State<opencode_chat::OpencodeChat>) {
    chat.reset();
    ochat.reset();
}

/// What the Settings → Chat section shows: resolved binary, key presence.
#[tauri::command]
fn chat_status(shared: State<Shared>) -> opencode_chat::ChatStatus {
    let s = shared.settings.lock().unwrap();
    opencode_chat::ChatStatus {
        bin_configured: s.opencode_bin.clone(),
        bin_resolved: opencode_chat::resolve_bin(&s.opencode_bin)
            .map(|p| p.to_string_lossy().to_string()),
        claude_key_present: secrets::present("anthropic-api-key"),
    }
}

/// Every conversation opencode has on disk, for the history list. Reads
/// `opencode.db` (via the opencode CLI), so keep it off the main thread.
#[tauri::command]
async fn chat_list_sessions() -> Vec<opencode_sessions::SessionInfo> {
    tokio::task::spawn_blocking(opencode_sessions::list_sessions)
        .await
        .unwrap_or_default()
}

/// MCP servers the notch can use: the chat folder's own config plus the global
/// opencode config. Shown on the dashboard and in Settings → Integrations.
#[tauri::command]
fn mcp_list() -> Vec<opencode_chat::McpInfo> {
    opencode_chat::list_mcps()
}

/// The dashboard's "Minhas tarefas": Jira issues assigned to the user, via the
/// Atlassian MCP opencode is already authenticated with. Cached for one hour;
/// `force: true` (the refresh button) bypasses the cache.
#[tauri::command]
async fn jira_tasks(force: bool) -> jira::JiraTasks {
    let paused = integrations::PAUSED.load(Ordering::Relaxed);
    // The transport spawns curl; keep it off the async runtime's threads.
    tokio::task::spawn_blocking(move || jira::tasks(force, paused))
        .await
        .unwrap_or_else(|e| jira::JiraTasks {
            tasks: Vec::new(),
            fetched_at: 0.0,
            cached: false,
            error: Some(format!("jira task failed: {e}")),
        })
}

/// The dashboard's "Sugestões": 4 short actions generated from the assistant's
/// name + master instruction, via the user's opencode. Cached for one hour;
/// `force` (the refresh button or saving the settings) bypasses the TTL.
#[tauri::command]
async fn assistant_suggestions(force: bool) -> assistant::Suggestions {
    let paused = integrations::PAUSED.load(Ordering::Relaxed);
    // Spawns opencode; keep it off the async runtime's threads.
    tokio::task::spawn_blocking(move || assistant::suggestions(force, paused))
        .await
        .unwrap_or_else(|e| assistant::Suggestions {
            items: Vec::new(),
            fetched_at: 0.0,
            cached: false,
            error: Some(format!("assistant task failed: {e}")),
        })
}

/// The dashboard's "Próximos eventos": the upcoming Google Calendar events, via
/// the Composio MCP. Cached for 15 minutes; `force` (the refresh button) bypasses it.
#[tauri::command]
async fn calendar_next(force: bool) -> calendar::CalendarNext {
    let paused = integrations::PAUSED.load(Ordering::Relaxed);
    tokio::task::spawn_blocking(move || calendar::next(force, paused))
        .await
        .unwrap_or_else(|e| calendar::CalendarNext {
            events: Vec::new(),
            fetched_at: 0.0,
            cached: false,
            error: Some(format!("calendar task failed: {e}")),
        })
}

/// The categories offered in Settings → Assistente.
#[tauri::command]
fn news_categories() -> Vec<news::NewsCategory> {
    news::categories()
}

/// The dashboard's "Notícias do dia": the top headline of each enabled category,
/// via the Composio MCP. Cached for 45 minutes; `force` bypasses it.
#[tauri::command]
async fn news_feed(force: bool) -> news::NewsFeed {
    let paused = integrations::PAUSED.load(Ordering::Relaxed);
    tokio::task::spawn_blocking(move || news::feed(force, paused))
        .await
        .unwrap_or_else(|e| news::NewsFeed {
            items: Vec::new(),
            fetched_at: 0.0,
            cached: false,
            error: Some(format!("news task failed: {e}")),
        })
}

/// Reopens an old conversation: loads its turns and makes the next `chat_send`
/// continue it, so the context on opencode's side is preserved.
#[tauri::command]
async fn chat_open_session(
    ochat: State<'_, opencode_chat::OpencodeChat>,
    id: String,
) -> Result<Vec<opencode_sessions::HistoryMessage>, String> {
    let load_id = id.clone();
    let messages = tokio::task::spawn_blocking(move || opencode_sessions::load_session(&load_id))
        .await
        .map_err(|e| format!("chat load failed: {e}"))?;
    if !messages.is_empty() {
        let dir = opencode_chat::chat_dir().to_string_lossy().to_string();
        ochat.attach(id, dir);
    }
    Ok(messages)
}

/// Deletes one conversation from opencode's store. Only ever runs after an
/// explicit click on the chat list's trash button. Detaches the live session
/// when it is the one being deleted so the next send starts fresh.
#[tauri::command]
async fn chat_delete_session(
    ochat: State<'_, opencode_chat::OpencodeChat>,
    id: String,
) -> Result<bool, String> {
    ochat.detach_if(&id);
    let deleted = tokio::task::spawn_blocking(move || opencode_sessions::delete_session(&id))
        .await
        .map_err(|e| format!("chat delete failed: {e}"))?;
    Ok(deleted)
}

/// Copies a dropped file into the inbox and reports its name back.
#[tauri::command]
fn ingest_file(path: String) -> Result<DroppedFile, String> {
    files::ingest(&path)
}

/// Native Explorer picker — the drop zone's click-to-browse fallback.
/// Returns the picked path, or None when the user cancels.
#[tauri::command]
fn browse_file(app: AppHandle) -> Result<Option<String>, String> {
    Ok(browse::pick_file(&app)?.map(|p| p.to_string_lossy().to_string()))
}

/// The island may only ask whether a key exists — never read it.
#[tauri::command]
fn secret_present(key: String) -> bool {
    secrets::present(&key)
}

#[tauri::command]
fn secret_set(key: String, value: String) -> Result<(), String> {
    secrets::set(&key, &value)
}

#[tauri::command]
fn secret_clear(key: String) -> Result<(), String> {
    secrets::clear(&key)
}

/// Opens the configured n8n instance — the URL lives in the Credential Manager.
#[tauri::command]
fn open_n8n() {
    if let Some(url) = secrets::get("n8n-url") {
        open_url(url);
    }
}

/// Refresh buttons in the integration cards.
#[tauri::command]
async fn refresh_integration(app: AppHandle, id: String) {
    integrations::poll_once(app, &id).await;
}

/// Lets the island write to the same log as the Rust side.
#[tauri::command]
fn log_line(message: String) {
    log::line(format!("ui  {message}"));
}

pub fn run() {
    let loaded = settings::load();
    let gate = Arc::new(PollGate::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open".to_string());
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Ctrl+Space, system-wide: expand the island, or compact it.
                    if event.state == ShortcutState::Pressed {
                        let _ = app.emit_to(island::WINDOW_LABEL, "hotkey", "toggle");
                    }
                })
                .build(),
        )
        .manage(Shared {
            settings: Mutex::new(loaded.clone()),
            gate: gate.clone(),
        })
        .manage(Pending::default())
        .manage(Chat::default())
        .manage(opencode_chat::OpencodeChat::default())
        .invoke_handler(tauri::generate_handler![
            boot,
            save_settings,
            set_collapsed,
            set_island_rect,
            focus_window,
            reposition,
            open_url,
            open_in_vscode,
            quit_app,
            hooks_status,
            hooks_preview,
            hooks_apply,
            opencode_status,
            opencode_preview,
            opencode_apply,
            approval_decision,
            approval_ack,
            approval_decline,
            log_line,
            chat_send,
            chat_reset,
            chat_status,
            chat_list_sessions,
            mcp_list,
            jira_tasks,
            calendar_next,
            news_categories,
            news_feed,
            assistant_suggestions,
            chat_open_session,
            chat_delete_session,
            ingest_file,
            browse_file,
            secret_present,
            secret_set,
            secret_clear,
            refresh_integration,
            open_n8n,
            set_paused,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Register globally rather than in the plugin builder so a shortcut
            // already taken by another app degrades to a log line, not a crash.
            let ctrl_space = Shortcut::new(Some(Modifiers::CONTROL), Code::Space);
            if let Err(err) = app.global_shortcut().register(ctrl_space) {
                log::line(format!("global shortcut Ctrl+Space not registered: {err}"));
            }

            tray::build(&handle)?;

            if let Some(win) = island::window(&handle) {
                island::make_non_activating(&win);
                island::apply_geometry(&handle, &loaded.screen, false);
                let _ = win.show();
            }
            gate.collapsed.store(false, Ordering::Relaxed);
            gate.set_active(true);
            island::spawn_cursor_poll(handle.clone(), gate.clone());

            log::line(format!("--- Coucou {} started ---", env!("CARGO_PKG_VERSION")));
            hooks::ensure_hook_exe(&handle);
            opencode::ensure_plugin();
            // Gives the notch's chat folder its MCP servers (Jira, Intercom).
            if let Err(err) = opencode_chat::ensure_chat_config() {
                log::line(format!("chat config provisioning failed: {err}"));
            }
            pipe::start(handle.clone());
            integrations::start(handle.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Coucou");
}
