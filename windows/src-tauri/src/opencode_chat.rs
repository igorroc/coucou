// opencode CLI chat backend — routes the island chat through the user's own
// opencode (`opencode run`) instead of the Anthropic API, so whatever model
// the user configured there (GPT, Claude, …) answers from the notch.
//
// Everything happens here rather than in the island: the prompt, the session
// id and any file bytes never cross the IPC boundary, and the query is passed
// as argv (never through a shell), so paths and punctuation stay inert.
//
// Multi-turn works by keeping opencode's session id after the first turn and
// passing `--session` on later ones, in the same working directory.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::claude::{ChatContext, ChatReply};

/// `opencode run` can think for a while; the Claude API path uses 90 s, but a
/// local agent with tools deserves more rope before the island gives up.
const RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// Preamble sent once per session so answers fit a notch readout. Carries the
/// user-chosen name, who the user is, and how the assistant should behave.
fn persona(name: &str, about_user: &str, about_assistant: &str) -> String {
    let mut text =
        format!("You are {name}, a personal AI assistant living at the top of the user's screen.");
    let about_user = about_user.trim();
    if !about_user.is_empty() {
        text.push_str("\n\nAbout the user you are helping:\n");
        text.push_str(about_user);
    }
    let about_assistant = about_assistant.trim();
    if !about_assistant.is_empty() {
        text.push_str("\n\nHow you should behave and what to prioritise:\n");
        text.push_str(about_assistant);
    }
    text.push_str(
        "\nAnswer in the user's language. Be helpful and complete, but concise enough for a small popup. \
Use plain text with line breaks, no markdown formatting.",
    );
    text
}

/// Cap on anything written to the log. opencode's stderr can echo prompt text,
/// so keep it short — enough to identify the failure, not a transcript.
const LOG_CLIP: usize = 4_000;

/// Keeps spawned helpers from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Truncates on a char boundary so a multi-byte character is never split.
fn clip(s: &str) -> String {
    if s.len() <= LOG_CLIP {
        return s.to_string();
    }
    let mut end = LOG_CLIP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[{} bytes]", &s[..end], s.len())
}

/// Turns opencode's headless permission auto-reject into an actionable message.
/// `opencode run` has no TTY, so any permission resolving to "ask" — most
/// commonly `external_directory` — is rejected on the spot and the turn ends
/// with nothing. The stderr line looks like:
///   `! permission requested: external_directory (C:\…\storage\project\*); auto-rejecting`
fn blocked_permission_error(stderr: &str) -> Option<String> {
    if !stderr.contains("permission requested") || !stderr.contains("auto-rejecting") {
        return None;
    }
    let after = stderr.split("permission requested").nth(1)?;
    let after = after.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let action = after.split_whitespace().next().unwrap_or("");
    let target = after
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(path, _)| path.trim());

    let mut message = String::from("opencode blocked a permission");
    if !action.is_empty() {
        message.push_str(&format!(" ({action})"));
    }
    if let Some(target) = target {
        message.push_str(&format!(" for {target}"));
    }
    message.push_str(
        ". Grant external access in Settings → Chat or add it to the chat opencode.json.",
    );
    Some(message)
}

#[derive(Default)]
pub struct OpencodeChat {
    session: Mutex<Option<ChatSession>>,
    /// A long-lived `opencode serve` child, kept warm so every turn after the
    /// first skips opencode's per-process bootstrap (config, plugins, skills,
    /// watcher, snapshot, MCP connections) — around 5 s on a cold start.
    server: Arc<Mutex<Option<ServerChild>>>,
}

struct ChatSession {
    id: String,
    dir: String,
}

/// A running `opencode serve` process and the URL it listens on.
struct ServerChild {
    child: std::process::Child,
    url: String,
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extracts the URL out of opencode's `opencode server listening on <url>` line.
fn parse_listen_url(line: &str) -> Option<String> {
    let idx = line.find("listening on ")?;
    let url = line[idx + "listening on ".len()..].trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(
            url.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
                .to_string(),
        )
    } else {
        None
    }
}

/// Returns the URL of a running opencode server, starting one if needed. The
/// child is remembered so later turns reuse it; a dead child is replaced. Returns
/// `None` when the server cannot be reached, so the caller falls back to a cold
/// `opencode run`.
fn ensure_server(server: &Mutex<Option<ServerChild>>, bin: &str) -> Option<String> {
    let mut guard = server.lock().unwrap();
    if let Some(existing) = guard.as_mut() {
        if matches!(existing.child.try_wait(), Ok(None)) {
            let url = existing.url.clone();
            return Some(url);
        }
    }
    *guard = None;

    // `--port 0` lets opencode pick a free port, so a stale server from a previous
    // run can never make this fail. The URL comes back on stdout.
    let mut child = std::process::Command::new(bin)
        .args(["serve", "--port", "0", "--hostname", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| crate::log::line(format!("opencode serve failed to start: {e}")))
        .ok()?;

    let stdout = child.stdout.take()?;
    // Drain stderr on its own thread; a full pipe would otherwise wedge the server.
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        });
    }

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !sent {
                if let Some(url) = parse_listen_url(&line) {
                    let _ = tx.send(url);
                    sent = true;
                }
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(url) => {
            crate::log::line(format!("opencode server ready at {url}"));
            *guard = Some(ServerChild {
                child,
                url: url.clone(),
            });
            Some(url)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            crate::log::line("opencode serve did not report a URL in time".to_string());
            None
        }
    }
}

impl OpencodeChat {
    pub fn reset(&self) {
        *self.session.lock().unwrap() = None;
    }

    /// Stops the warm server, if any. Best-effort: called on app quit.
    pub fn shutdown_server(&self) {
        // Dropping the child kills and reaps it.
        *self.server.lock().unwrap() = None;
    }

    /// Resumes an existing session: the next `send` reuses it with `--session`
    /// instead of starting a fresh conversation.
    pub fn attach(&self, id: String, dir: String) {
        *self.session.lock().unwrap() = Some(ChatSession { id, dir });
    }

    /// Forgets the live session when it is the one being deleted, so the next
    /// send starts a fresh conversation instead of continuing a ghost.
    pub fn detach_if(&self, id: &str) {
        let mut guard = self.session.lock().unwrap();
        if guard.as_ref().is_some_and(|s| s.id == id) {
            *guard = None;
        }
    }
}

/// The dedicated working directory for Navi's own conversations. Kept apart from
/// every repo so opencode groups them under their own project instead of inheriting
/// whichever repository `%USERPROFILE%` (or a dropped file) happens to sit in. A
/// `git init` here gives it a worktree of its own — the nearest `.git` wins, so it
/// can never be absorbed by an ancestor repo either.
pub fn chat_dir() -> PathBuf {
    let dir = crate::settings::local_dir().join("chat");
    let _ = std::fs::create_dir_all(&dir);
    if !dir.join(".git").exists() {
        let _ = std::process::Command::new("git")
            .arg("init")
            .current_dir(&dir)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    dir
}

/// The MCP servers Navi's chat folder is provisioned with. Declared here so the
/// notch can reach Jira (Atlassian), Intercom and Composio; everything else in
/// the file is the user's.
fn managed_mcps() -> serde_json::Value {
    serde_json::json!({
        "atlassian": {
            "type": "remote",
            "url": "https://mcp.atlassian.com/v1/mcp/authv2",
            "enabled": true,
            "oauth": {}
        },
        "composio": {
            "type": "remote",
            "url": "https://connect.composio.dev/mcp",
            "enabled": true,
            "oauth": {}
        },
        "intercom": {
            "type": "remote",
            "url": "https://mcp.intercom.com/mcp",
            "enabled": true,
            "oauth": {}
        }
    })
}

/// Makes sure `%LOCALAPPDATA%\Navi Assistant\chat\opencode.json` declares the MCP servers
/// the notch needs plus the `small_model` the hidden `title` agent uses.
/// Additive on purpose: it only guarantees the managed entries exist and never
/// removes a server or a key the user added by hand. The file is
/// written only when its content actually changes, so opencode is not disturbed
/// on every launch.
/// Guidance file dropped in the chat folder and referenced from the chat
/// config's `instructions`. It tells the agent to call the exact Composio tool
/// instead of spending a whole model round trip on `COMPOSIO_SEARCH_TOOLS` first.
const INSTRUCTIONS_FILE: &str = "navi.md";

const INSTRUCTIONS_BODY: &str = "\
Navi Assistant — tool guidance:
- The user's calendar, tasks and news come from Composio. When a question is about
  them, call the specific tool directly through COMPOSIO_MULTI_EXECUTE_TOOL:
  GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS (time_min, time_max, response_detail=\"minimal\"),
  GOOGLETASKS_LIST_ALL_TASKS, or COMPOSIO_SEARCH_NEWS.
- Do NOT call COMPOSIO_SEARCH_TOOLS first: the tool you need is already known here.
  One tool call, then answer.
- Answer in the user's language, plainly and concisely.
";

/// Absolute path of the guidance file inside the chat folder.
fn instructions_path() -> PathBuf {
    chat_dir().join(INSTRUCTIONS_FILE)
}

/// Writes the guidance file when its content changed. Best-effort.
fn ensure_instructions_file() {
    let path = instructions_path();
    if std::fs::read_to_string(&path)
        .map(|s| s == INSTRUCTIONS_BODY)
        .unwrap_or(false)
    {
        return;
    }
    let _ = std::fs::write(&path, INSTRUCTIONS_BODY);
}

pub fn ensure_chat_config() -> std::io::Result<()> {
    ensure_instructions_file();
    let small = crate::settings::load().opencode_model;
    let small = small.trim();
    let small = if small.is_empty() { None } else { Some(small) };
    let path = chat_dir().join("opencode.json");
    let existing = std::fs::read(&path).ok();
    let notes = instructions_path();
    let home = user_home();
    let rendered = merge_chat_config(
        existing.as_deref(),
        small,
        &notes.to_string_lossy(),
        home.as_deref(),
    );
    // Untouched if nothing changed — avoids churn (and LSP reloads) each launch.
    if std::fs::read_to_string(&path)
        .map(|s| s == rendered)
        .unwrap_or(false)
    {
        return Ok(());
    }
    std::fs::write(&path, rendered)
}

/// Same guarantee as `ensure_chat_config` for the `small_model` only, applied
/// right before a send so a model chosen in Settings → Chat vale já na próxima
/// conversa sem precisar reiniciar. Nunca sobrescreve um `small_model` que o
/// usuário definiu à mão.
fn ensure_small_model(model: &str) {
    let model = model.trim();
    if model.is_empty() {
        return;
    }
    let path = chat_dir().join("opencode.json");
    let existing = std::fs::read(&path).ok();
    let notes = instructions_path();
    let home = user_home();
    let rendered = merge_chat_config(
        existing.as_deref(),
        Some(model),
        &notes.to_string_lossy(),
        home.as_deref(),
    );
    if std::fs::read_to_string(&path)
        .map(|s| s == rendered)
        .unwrap_or(false)
    {
        return;
    }
    let _ = std::fs::write(&path, rendered);
}

/// The user's home directory, or `None` when `%USERPROFILE%` is unavailable.
fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Pure merge (no I/O) so it can be tested: guarantees the managed MCP servers
/// exist, leaves every other key and server exactly as the user left them.
/// When `small_model` carries a non-empty model and the file doesn't define
/// one, it is added so the title agent authenticates with the same model as
/// the chat instead of failing silently on an unavailable default. The guidance
/// file (`instructions`) is appended additively. `home` lets the permission
/// block point at the user's opencode directories without hardcoding a path.
fn merge_chat_config(
    existing: Option<&[u8]>,
    small_model: Option<&str>,
    instructions: &str,
    home: Option<&Path>,
) -> String {
    let mut root = existing
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }

    let obj = root.as_object_mut().expect("root is an object");
    obj.entry("$schema")
        .or_insert_with(|| serde_json::json!("https://opencode.ai/config.json"));
    if let Some(model) = small_model.map(str::trim).filter(|m| !m.is_empty()) {
        let dominated = obj
            .get("small_model")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        if !dominated {
            obj.insert("small_model".to_string(), serde_json::json!(model));
        }
    }
    let list = obj
        .entry("instructions")
        .or_insert_with(|| serde_json::json!([]));
    if !list.is_array() {
        *list = serde_json::json!([]);
    }
    let list = list.as_array_mut().expect("instructions is an array");
    if !list.iter().any(|v| v.as_str() == Some(instructions)) {
        list.push(serde_json::json!(instructions));
    }
    let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
    if !mcp.is_object() {
        *mcp = serde_json::json!({});
    }
    let mcp_obj = mcp.as_object_mut().expect("mcp is an object");
    for (name, server) in managed_mcps()
        .as_object()
        .expect("managed_mcps is an object")
    {
        // Never clobber a server the user has customised under the same name.
        mcp_obj
            .entry(name.clone())
            .or_insert_with(|| server.clone());
    }

    if let Some(home) = home {
        ensure_external_permissions(obj, home);
    }

    serde_json::to_string_pretty(&root).unwrap_or_default()
}

/// The opencode data directories Navi's chat may reach outside its working
/// folder: session history and the global config/plugins. opencode's
/// `external_directory` permission defaults to "ask", and a non-interactive
/// `opencode run` auto-rejects every ask — the tool call dies and the turn comes
/// back empty. Granting exactly these two trees fixes that without opening the
/// rest of the machine.
fn managed_external_dirs(home: &Path) -> Vec<String> {
    [".config/opencode", ".local/share/opencode"]
        .iter()
        .map(|rel| format!("{}/**", home.join(rel).to_string_lossy().replace('\\', "/")))
        .collect()
}

/// Adds the managed `external_directory` allow rules (and an `edit` deny so the
/// assistant only reads opencode's own files). Additive: a `permission`,
/// `external_directory` or `edit` the user already defined — including the
/// string shorthand `"allow"`/`"ask"` — is left untouched.
fn ensure_external_permissions(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    home: &Path,
) {
    let dirs = managed_external_dirs(home);
    let permission = obj
        .entry("permission")
        .or_insert_with(|| serde_json::json!({}));
    let Some(permission) = permission.as_object_mut() else {
        return;
    };

    let external = permission
        .entry("external_directory")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(external) = external.as_object_mut() {
        for dir in &dirs {
            external
                .entry(dir.clone())
                .or_insert_with(|| serde_json::json!("allow"));
        }
    }

    let edit = permission
        .entry("edit")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(edit) = edit.as_object_mut() {
        for dir in &dirs {
            edit.entry(dir.clone())
                .or_insert_with(|| serde_json::json!("deny"));
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStatus {
    pub bin_configured: String,
    pub bin_resolved: Option<String>,
    pub claude_key_present: bool,
}

/// One MCP server opencode can reach, as shown on the dashboard pill and in
/// Settings → Integrations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    pub name: String,
    /// "remote" or "local".
    pub kind: String,
    /// URL (remote) or command line (local).
    pub target: String,
    pub enabled: bool,
    /// Where it was declared: "Navi" (the chat folder) or "Global".
    pub source: String,
}

/// The global opencode config directory — `~/.config/opencode`.
fn global_config_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("opencode")
}

/// Best-effort description of one `mcp` entry, tolerant of the shapes opencode
/// accepts (remote `url`, local `command` as string or argv array).
fn describe_mcp(name: &str, server: &Value, source: &str) -> McpInfo {
    let obj = server.as_object();
    let command = obj.and_then(|o| o.get("command"));
    let kind = obj
        .and_then(|o| o.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| if command.is_some() { "local" } else { "remote" })
        .to_string();
    let target = obj
        .and_then(|o| o.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            command.map(|c| match c {
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::String(s) => s.clone(),
                _ => "—".to_string(),
            })
        })
        .unwrap_or_else(|| "—".to_string());
    let enabled = obj
        .and_then(|o| o.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    McpInfo {
        name: name.to_string(),
        kind,
        target,
        enabled,
        source: source.to_string(),
    }
}

/// Reads every MCP server the notch can use: the chat folder's own config first
/// (the managed Jira/Intercom plus anything the user added there), then the
/// global opencode config. Names are deduped, the first declaration winning.
pub fn list_mcps() -> Vec<McpInfo> {
    let sources = [
        ("Navi", chat_dir().join("opencode.json")),
        ("Global", global_config_dir().join("opencode.json")),
        ("Global", global_config_dir().join("config.json")),
    ];
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<McpInfo> = Vec::new();
    for (source, path) in sources {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(root) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(mcp) = root.get("mcp").and_then(Value::as_object) else {
            continue;
        };
        for (name, server) in mcp {
            if !seen.insert(name.clone()) {
                continue;
            }
            out.push(describe_mcp(name, server, source));
        }
    }
    out
}

/// Where the binary comes from: explicit setting first, then well-known spots.
pub fn resolve_bin(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            return Some(p);
        }
        return None;
    }
    if let Some(p) = crate::find_on_path("opencode") {
        return Some(p);
    }
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for rel in [
            ".bun/bin/opencode.exe",
            "scoop/shims/opencode.exe",
            ".opencode/bin/opencode.exe",
        ] {
            let p = home.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// One chat turn. Returns the assistant's text, or a message the island shows
/// in the note view.
#[allow(clippy::too_many_arguments)]
pub async fn send(
    chat: &OpencodeChat,
    bin_configured: &str,
    model_override: &str,
    name: &str,
    about_user: &str,
    about_assistant: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let bin = resolve_bin(bin_configured).ok_or_else(|| {
        "opencode not found. Install it (opencode.ai) or set its path in Settings → Chat."
            .to_string()
    })?;

    // First turn of a conversation carries the persona + context; later turns
    // reuse the session in the same directory.
    let first;
    let (session_id, dir, files, message) = {
        let guard = chat.session.lock().unwrap();
        match &*guard {
            Some(s) => {
                first = false;
                // A context attached mid-conversation rides the next turn: the
                // opencode session is reused, so prepend it to this message.
                let note = context_note(&context);
                let message = if note.is_empty() {
                    query.clone()
                } else {
                    format!("Context: {note}\n\nUser: {query}")
                };
                (Some(s.id.clone()), s.dir.clone(), Vec::new(), message)
            }
            None => {
                first = true;
                let (dir, files, ctx_text) = first_turn_context(&context);
                let mut message = persona(name, about_user, about_assistant);
                if !ctx_text.is_empty() {
                    message.push_str("\n\nContext: ");
                    message.push_str(&ctx_text);
                }
                message.push_str("\n\nUser: ");
                message.push_str(&query);
                (None, dir, files, message)
            }
        }
    };

    let model = model_override.trim();
    ensure_small_model(model);

    // Prefer a warm `opencode serve` so the per-turn bootstrap is skipped; when it
    // cannot be started we still run a cold `opencode run` exactly as before.
    let attach = {
        let server = chat.server.clone();
        let bin_for_server = bin.clone();
        tokio::task::spawn_blocking(move || {
            ensure_server(&server, &bin_for_server.to_string_lossy())
        })
        .await
        .ok()
        .flatten()
    };

    // Sem `--title`: o opencode gera o título da sessão automaticamente
    // (agente `title` oculto, via `small_model`) a partir da primeira mensagem.
    // Passar um título fixo aqui congelaria todas as conversas com o mesmo nome.
    let mut args: Vec<String> = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        // Non-interactive runs do not emit reasoning by default; this is what
        // makes the "thinking" show up in the chat log.
        "--thinking".into(),
        "--dir".into(),
        dir.clone(),
    ];
    if let Some(url) = &attach {
        args.push("--attach".into());
        args.push(url.clone());
    }
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    if let Some(id) = &session_id {
        args.push("--session".into());
        args.push(id.clone());
    }
    // NOTE: the message must come before `--file`: yargs array options greedily
    // swallow every positional after them, so `--file f "message"` eats the
    // message as a second file ("File not found: <your question>").
    args.push(message);
    for f in &files {
        args.push("--file".into());
        args.push(f.clone());
    }

    // One line per turn so a failure can be reconstructed from navi-assistant.log.
    // The question is clipped: it may hold anything the user typed.
    crate::log::line(format!(
        "chat send bin={} model={} dir={} session={} first={} attach={} q={}",
        bin.display(),
        if model.is_empty() { "(default)" } else { model },
        dir,
        session_id.as_deref().unwrap_or("-"),
        first,
        attach.is_some(),
        clip(&query),
    ));

    // The full turn — reasoning, tools, steps, timings — goes to its own file in
    // the TEMP logs folder. Started before the spawn so `elapsedMs` is the whole
    // turn, process startup included.
    let mut turn = crate::chatlog::TurnLog::begin(
        "opencode",
        serde_json::json!({
            "model": if model.is_empty() { serde_json::Value::Null } else { serde_json::json!(model) },
            "bin": bin.display().to_string(),
            "dir": dir.clone(),
            "session": session_id.clone(),
            "first": first,
            "query": clip(&query),
        }),
    );

    let bin_str = bin.to_string_lossy().to_string();
    let (ok, stdout, stderr) = tokio::task::spawn_blocking(move || run_blocking(&bin_str, &args))
        .await
        .map_err(|e| format!("opencode task failed: {e}"))??;

    let parsed = parse_events(&stdout);
    // The session id is echoed in every log line below, so keep it borrowed.
    if let Some(id) = parsed.session_id.clone() {
        *chat.session.lock().unwrap() = Some(ChatSession { id, dir });
    }

    for ev in &parsed.events {
        turn.event(&ev.kind, ev.at_ms, ev.data.clone());
    }
    // The stream is sometimes empty (a known opencode regression): keep the raw
    // output so the shape is still visible in the log.
    if parsed.events.is_empty() {
        turn.raw("stdout", stdout.trim());
    }
    if !stderr.trim().is_empty() {
        turn.raw("stderr", stderr.trim());
    }

    // The event stream's own error is the most precise one opencode gives us:
    // surface it to the island *and* keep it in the log with its context.
    if let Some(err) = parsed.error {
        crate::log::line(format!(
            "chat error session={} provider={} model={} name={} msg={}",
            parsed
                .session_id
                .as_deref()
                .unwrap_or(session_id.as_deref().unwrap_or("-")),
            parsed.provider_id.as_deref().unwrap_or("-"),
            parsed.model_id.as_deref().unwrap_or("-"),
            parsed.error_name.as_deref().unwrap_or("-"),
            clip(&err),
        ));
        if !stderr.trim().is_empty() {
            crate::log::line(format!("chat error stderr={}", clip(stderr.trim())));
        }
        turn.finish(serde_json::json!({
            "ok": false,
            "error": clip(&err),
            "errorName": parsed.error_name,
            "providerID": parsed.provider_id,
            "modelID": parsed.model_id,
        }));
        return Err(err);
    }
    if !ok {
        crate::log::line(format!(
            "chat failed session={} exit≠0 stderr={}",
            parsed
                .session_id
                .as_deref()
                .unwrap_or(session_id.as_deref().unwrap_or("-")),
            clip(stderr.trim()),
        ));
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        let detail = if tail.trim().is_empty() {
            "unknown error"
        } else {
            tail.trim()
        };
        turn.finish(serde_json::json!({ "ok": false, "exitOk": false, "error": clip(detail) }));
        return Err(format!("opencode failed: {detail}"));
    }
    if parsed.text.trim().is_empty() {
        // No text and no error event: dump what came back so the shape is
        // visible — this is what distinguishes "answered empty" from "produced
        // no events at all".
        crate::log::line(format!(
            "chat empty session={} stdout={} stderr={}",
            parsed
                .session_id
                .as_deref()
                .unwrap_or(session_id.as_deref().unwrap_or("-")),
            clip(stdout.trim()),
            clip(stderr.trim()),
        ));
        // A headless permission auto-reject manifests exactly like this: no
        // error event, no text. Say what actually happened instead of the
        // useless "returned no text".
        if let Some(reason) = blocked_permission_error(&stderr) {
            turn.finish(serde_json::json!({ "ok": false, "exitOk": true, "error": reason }));
            return Err(reason);
        }
        turn.finish(
            serde_json::json!({ "ok": false, "exitOk": true, "error": "returned no text" }),
        );
        return Err("opencode returned no text.".into());
    }
    let text = parsed.text.trim().to_string();
    turn.finish(
        serde_json::json!({ "ok": true, "exitOk": true, "textChars": text.chars().count() }),
    );
    Ok(ChatReply { text })
}

/// Working dir + file attachments + context line for a fresh conversation.
/// The working directory is always Navi's own folder (never the repo a dropped
/// file came from), so the conversation lands under the "Navi" project.
fn first_turn_context(context: &Option<ChatContext>) -> (String, Vec<String>, String) {
    let dir = chat_dir().to_string_lossy().to_string();
    let files = match context {
        Some(ChatContext::File { path, .. }) => vec![path.clone()],
        _ => Vec::new(),
    };
    (dir, files, context_note(context))
}

/// One-line description of an attached context, shared by the first turn and by
/// a context attached later in the conversation.
fn context_note(context: &Option<ChatContext>) -> String {
    match context {
        Some(ChatContext::File { name, .. }) => format!("File: {name} (attached)"),
        Some(ChatContext::Window {
            app_name,
            title,
            url,
        }) => {
            let mut text = format!("App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            text
        }
        None => String::new(),
    }
}

/// Runs the binary synchronously (call from `spawn_blocking`): drains both
/// pipes on helper threads so large `--format json` output can never wedge
/// the child on a full pipe buffer, then enforces the deadline.
fn run_blocking(bin: &str, args: &[String]) -> Result<(bool, String, String), String> {
    run_with_timeout(RUN_TIMEOUT, bin, args)
}

/// Same as `run_blocking`, with the deadline in seconds — shared with the
/// assistant's suggestion generator.
pub(crate) fn run_for(
    timeout_secs: u64,
    bin: &str,
    args: &[String],
) -> Result<(bool, String, String), String> {
    run_with_timeout(Duration::from_secs(timeout_secs), bin, args)
}

fn run_with_timeout(
    timeout: Duration,
    bin: &str,
    args: &[String],
) -> Result<(bool, String, String), String> {
    use std::io::Read;

    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not start opencode: {e}"))?;

    let out_handle = std::thread::spawn({
        let mut out = child.stdout.take();
        move || {
            let mut buf = Vec::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).to_string()
        }
    });
    let err_handle = std::thread::spawn({
        let mut err = child.stderr.take();
        move || {
            let mut buf = Vec::new();
            if let Some(e) = err.as_mut() {
                let _ = e.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).to_string()
        }
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "opencode took too long ({} s) — try a shorter question.",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    Ok((status.success(), stdout, stderr))
}

/// The concatenated assistant text of an `opencode run --format json` stream.
pub(crate) fn collect_text(stdout: &str) -> String {
    parse_events(stdout).text
}

/// One event decoded from the stream, ready to be written to the chat log.
pub(crate) struct ParsedEvent {
    pub kind: String,
    /// The event's own `timestamp` (ms since the epoch), when present.
    pub at_ms: Option<i64>,
    pub data: Value,
}

struct Parsed {
    session_id: Option<String>,
    text: String,
    error: Option<String>,
    /// Extra fields from the error event, for the log only (never the UI).
    error_name: Option<String>,
    provider_id: Option<String>,
    model_id: Option<String>,
    /// Every event, decoded, for the chat log (never the UI).
    events: Vec<ParsedEvent>,
}

/// Pulls the assistant text + session id out of `opencode run --format json`
/// (newline-delimited events, verified against opencode 1.x):
///   {"type":"text","sessionID":"ses_…","part":{"type":"text","text":"…"}}
///   {"type":"reasoning","part":{"type":"reasoning","text":"…"}}  → the thinking
///   {"type":"tool_use","part":{"tool":"bash","state":{…}}}      → tool call
///   {"type":"step_start" | "step_finish",…}                     → step + tokens
///   {"type":"error","error":{"data":{"message":…}}}             → surfaced to the island
/// Everything is best-effort: unknown lines and shapes are skipped, never fatal.
fn parse_events(stdout: &str) -> Parsed {
    let mut session_id: Option<String> = None;
    let mut text = String::new();
    let mut error: Option<String> = None;
    let mut error_name: Option<String> = None;
    let mut provider_id: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut events: Vec<ParsedEvent> = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if session_id.is_none() {
            session_id = event
                .get("sessionID")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        let at_ms = event.get("timestamp").and_then(Value::as_i64);
        match event.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = event
                    .get("part")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    text.push_str(t);
                    events.push(ParsedEvent {
                        kind: "text".into(),
                        at_ms,
                        data: serde_json::json!({ "text": t }),
                    });
                }
            }
            Some("reasoning") => {
                if let Some(t) = event
                    .get("part")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    events.push(ParsedEvent {
                        kind: "reasoning".into(),
                        at_ms,
                        data: serde_json::json!({ "text": t }),
                    });
                }
            }
            Some("tool_use") => {
                events.push(ParsedEvent {
                    kind: "tool".into(),
                    at_ms,
                    data: tool_event(event.get("part")),
                });
            }
            Some("step_start") => {
                events.push(ParsedEvent {
                    kind: "step_start".into(),
                    at_ms,
                    data: serde_json::json!({}),
                });
            }
            Some("step_finish") => {
                let part = event.get("part");
                let mut data = serde_json::json!({});
                if let Some(reason) = part.and_then(|p| p.get("reason")).and_then(Value::as_str) {
                    data["reason"] = serde_json::json!(reason);
                }
                if let Some(cost) = part.and_then(|p| p.get("cost")).cloned() {
                    data["cost"] = cost;
                }
                if let Some(tokens) = part.and_then(|p| p.get("tokens")).cloned() {
                    data["tokens"] = tokens;
                }
                events.push(ParsedEvent {
                    kind: "step_finish".into(),
                    at_ms,
                    data,
                });
            }
            Some("error") => {
                error_name = error_name.or_else(|| {
                    event
                        .get("error")
                        .and_then(|e| e.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
                provider_id = provider_id.or_else(|| {
                    event
                        .get("providerID")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
                model_id = model_id.or_else(|| {
                    event
                        .get("modelID")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
                let msg = event
                    .get("error")
                    .and_then(|e| {
                        e.get("data")
                            .and_then(|d| d.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| {
                                e.get("message").and_then(Value::as_str).map(str::to_string)
                            })
                    })
                    .or_else(|| {
                        event
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "opencode reported an error.".to_string());
                events.push(ParsedEvent {
                    kind: "error".into(),
                    at_ms,
                    data: serde_json::json!({
                        "message": msg,
                        "name": error_name.clone(),
                        "providerID": provider_id.clone(),
                        "modelID": model_id.clone(),
                    }),
                });
                error = Some(msg);
            }
            _ => {}
        }
    }

    Parsed {
        session_id,
        text,
        error,
        error_name,
        provider_id,
        model_id,
        events,
    }
}

/// Flattens one `tool_use` part into a log-friendly object: which tool, its
/// human title, status, input, a clipped output and how long it took.
fn tool_event(part: Option<&Value>) -> Value {
    let tool = part
        .and_then(|p| p.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.and_then(|p| p.get("state"));
    let mut data = serde_json::json!({ "tool": tool });
    if let Some(title) = state.and_then(|s| s.get("title")).and_then(Value::as_str) {
        data["title"] = serde_json::json!(title);
    }
    if let Some(status) = state.and_then(|s| s.get("status")).and_then(Value::as_str) {
        data["status"] = serde_json::json!(status);
    }
    if let Some(input) = state.and_then(|s| s.get("input")).cloned() {
        data["input"] = input;
    }
    if let Some(output) = state.and_then(|s| s.get("output")).and_then(Value::as_str) {
        data["output"] = serde_json::json!(clip(output));
    }
    let start = state
        .and_then(|s| s.pointer("/time/start"))
        .and_then(Value::as_i64);
    let end = state
        .and_then(|s| s.pointer("/time/end"))
        .and_then(Value::as_i64);
    if let (Some(a), Some(b)) = (start, end) {
        data["durationMs"] = serde_json::json!(b - a);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "/chat/navi.md";

    #[test]
    fn persona_carries_both_instructions_and_skips_empty() {
        let p = persona("Noma", "Sou CTO e foco em pagamentos.", "Seja direta.");
        assert!(p.starts_with("You are Noma,"));
        assert!(p.contains("About the user you are helping:\nSou CTO e foco em pagamentos."));
        assert!(p.contains("How you should behave and what to prioritise:\nSeja direta."));
        assert!(p.contains("no markdown formatting"));

        let bare = persona("Navi", "  ", "");
        assert!(bare.starts_with("You are Navi,"));
        assert!(!bare.contains("About the user"));
        assert!(!bare.contains("How you should behave"));
    }

    #[test]
    fn collect_text_reads_the_json_stream() {
        let stdout = concat!(
            "{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{\"type\":\"text\",\"text\":\"olá \"}}\n",
            "{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{\"type\":\"text\",\"text\":\"mundo\"}}\n",
        );
        assert_eq!(collect_text(stdout), "olá mundo");
    }

    #[test]
    fn parses_assistant_text_and_session_id() {
        let stdout = concat!(
            "{\"type\":\"step_start\",\"timestamp\":1,\"sessionID\":\"ses_abc\",\"part\":{\"type\":\"step-start\"}}\n",
            "{\"type\":\"text\",\"timestamp\":2,\"sessionID\":\"ses_abc\",\"part\":{\"type\":\"text\",\"text\":\"Hello! \"}}\n",
            "{\"type\":\"tool_use\",\"timestamp\":3,\"sessionID\":\"ses_abc\",\"part\":{\"type\":\"tool\",\"tool\":\"read\"}}\n",
            "{\"type\":\"text\",\"timestamp\":4,\"sessionID\":\"ses_abc\",\"part\":{\"type\":\"text\",\"text\":\"How can I help?\"}}\n",
            "{\"type\":\"step_finish\",\"timestamp\":5,\"sessionID\":\"ses_abc\",\"part\":{\"type\":\"step-finish\"}}\n",
        );
        let p = parse_events(stdout);
        assert_eq!(p.session_id.as_deref(), Some("ses_abc"));
        assert_eq!(p.text, "Hello! How can I help?");
        assert!(p.error.is_none());
    }

    #[test]
    fn skips_garbage_lines() {
        let stdout = "not json\n\n{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{\"type\":\"text\",\"text\":\"hi\"}}\n";
        let p = parse_events(stdout);
        assert_eq!(p.text, "hi");
        assert_eq!(p.session_id.as_deref(), Some("s"));
    }

    #[test]
    fn surfaces_errors() {
        let stdout = "{\"type\":\"error\",\"sessionID\":\"ses_x\",\"error\":{\"name\":\"UnknownError\",\"data\":{\"message\":\"boom\"}}}\n";
        let p = parse_events(stdout);
        assert_eq!(p.error.as_deref(), Some("boom"));
    }

    #[test]
    fn captures_error_context_for_the_log() {
        let stdout = concat!(
            "{\"type\":\"error\",\"sessionID\":\"ses_x\",\"providerID\":\"opencode-go\",\"modelID\":\"gpt-5\",",
            "\"error\":{\"name\":\"AI_APICallError\",\"data\":{\"message\":\"Unexpected server error.\"}}}\n",
        );
        let p = parse_events(stdout);
        assert_eq!(p.error.as_deref(), Some("Unexpected server error."));
        assert_eq!(p.error_name.as_deref(), Some("AI_APICallError"));
        assert_eq!(p.provider_id.as_deref(), Some("opencode-go"));
        assert_eq!(p.model_id.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn parses_reasoning_tools_and_steps_for_the_log() {
        let stdout = concat!(
            "{\"type\":\"step_start\",\"timestamp\":1000,\"sessionID\":\"s\",\"part\":{\"type\":\"step-start\"}}\n",
            "{\"type\":\"reasoning\",\"timestamp\":1200,\"sessionID\":\"s\",\"part\":{\"type\":\"reasoning\",\"text\":\"Let me think\"}}\n",
            "{\"type\":\"tool_use\",\"timestamp\":1300,\"sessionID\":\"s\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"title\":\"ls\",\"input\":{\"command\":\"ls\"},\"output\":\"a\\nb\",\"time\":{\"start\":1250,\"end\":1300}}}}\n",
            "{\"type\":\"step_finish\",\"timestamp\":1800,\"sessionID\":\"s\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\",\"cost\":0.01,\"tokens\":{\"input\":10,\"output\":5}}}\n",
        );
        let p = parse_events(stdout);
        let kinds: Vec<&str> = p.events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["step_start", "reasoning", "tool", "step_finish"]
        );

        let reasoning = p.events.iter().find(|e| e.kind == "reasoning").unwrap();
        assert_eq!(reasoning.data["text"], "Let me think");
        assert_eq!(reasoning.at_ms, Some(1200));
        assert!(
            reasoning.data.get("offsetMs").is_none(),
            "offset is set by TurnLog"
        );

        let tool = p.events.iter().find(|e| e.kind == "tool").unwrap();
        assert_eq!(tool.data["tool"], "bash");
        assert_eq!(tool.data["durationMs"], 50);
        assert_eq!(tool.data["output"], "a\nb");
    }

    #[test]
    fn clip_keeps_short_strings_and_bounds_long_ones() {
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(10_000);
        let clipped = clip(&long);
        assert!(clipped.len() < long.len());
        assert!(clipped.ends_with("[20000 bytes]"));
    }

    #[test]
    fn resolve_bin_rejects_missing_configured_path() {
        assert!(resolve_bin("C:\\definitely\\not\\here\\opencode.exe").is_none());
    }

    #[test]
    fn merge_adds_managed_mcps_from_nothing() {
        let out = merge_chat_config(None, None, NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcp"]["atlassian"]["type"], "remote");
        assert_eq!(v["mcp"]["intercom"]["url"], "https://mcp.intercom.com/mcp");
        assert_eq!(
            v["mcp"]["composio"]["url"],
            "https://connect.composio.dev/mcp"
        );
        assert_eq!(v["$schema"], "https://opencode.ai/config.json");
        assert_eq!(v["instructions"][0], NOTES);
    }

    #[test]
    fn merge_preserves_user_keys_and_servers() {
        let existing = br#"{"model":"opencode-go/x","mcp":{"mine":{"type":"local"}},"instructions":["/user/own.md"]}"#;
        let out = merge_chat_config(Some(existing), None, NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["model"], "opencode-go/x");
        assert!(v["mcp"]["mine"].is_object(), "user MCP kept");
        assert!(v["mcp"]["intercom"].is_object(), "managed MCP added");
        assert_eq!(
            v["instructions"][0], "/user/own.md",
            "user instruction kept"
        );
        assert_eq!(v["instructions"][1], NOTES, "managed instruction appended");
    }

    #[test]
    fn merge_does_not_clobber_a_customised_server() {
        let existing = br#"{"mcp":{"atlassian":{"type":"remote","url":"https://mine.example"}}}"#;
        let out = merge_chat_config(Some(existing), None, NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcp"]["atlassian"]["url"], "https://mine.example");
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_chat_config(None, Some("opencode/gpt-5-mini"), NOTES, None);
        let twice = merge_chat_config(Some(once.as_bytes()), Some("opencode/gpt-5-mini"), NOTES, None);
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_adds_small_model_when_missing() {
        let out = merge_chat_config(None, Some("opencode/gpt-5-mini"), NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["small_model"], "opencode/gpt-5-mini");
    }

    #[test]
    fn merge_does_not_clobber_small_model() {
        let existing = br#"{"small_model":"anthropic/claude-haiku-4-5"}"#;
        let out = merge_chat_config(Some(existing), Some("opencode/gpt-5-mini"), NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["small_model"], "anthropic/claude-haiku-4-5");
    }

    #[test]
    fn merge_ignores_empty_small_model() {
        let out = merge_chat_config(None, Some("  "), NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("small_model").is_none());
    }

    #[test]
    fn merge_adds_external_directory_allow_and_edit_deny() {
        let home = Path::new("C:\\Users\\tester");
        let out = merge_chat_config(None, None, NOTES, Some(home));
        let v: Value = serde_json::from_str(&out).unwrap();
        let ext = &v["permission"]["external_directory"];
        assert_eq!(ext["C:/Users/tester/.config/opencode/**"], "allow");
        assert_eq!(ext["C:/Users/tester/.local/share/opencode/**"], "allow");
        let edit = &v["permission"]["edit"];
        assert_eq!(edit["C:/Users/tester/.config/opencode/**"], "deny");
        assert_eq!(edit["C:/Users/tester/.local/share/opencode/**"], "deny");
    }

    #[test]
    fn merge_keeps_user_permission_rules() {
        let existing =
            br#"{"permission":{"external_directory":{"~/mine/**":"allow"},"edit":{"~/mine/**":"allow"}}}"#;
        let out = merge_chat_config(
            Some(existing),
            None,
            NOTES,
            Some(Path::new("C:/Users/tester")),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["permission"]["external_directory"]["~/mine/**"], "allow");
        assert_eq!(v["permission"]["edit"]["~/mine/**"], "allow");
        assert_eq!(
            v["permission"]["external_directory"]["C:/Users/tester/.config/opencode/**"],
            "allow"
        );
    }

    #[test]
    fn merge_leaves_string_permission_shorthand_alone() {
        let existing = br#"{"permission":"allow"}"#;
        let out = merge_chat_config(
            Some(existing),
            None,
            NOTES,
            Some(Path::new("C:/Users/tester")),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["permission"], "allow");
    }

    #[test]
    fn merge_skips_permissions_without_home() {
        let out = merge_chat_config(None, None, NOTES, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("permission").is_none());
    }

    #[test]
    fn merge_with_permissions_is_idempotent() {
        let home = Path::new("C:\\Users\\tester");
        let once = merge_chat_config(None, None, NOTES, Some(home));
        let twice = merge_chat_config(Some(once.as_bytes()), None, NOTES, Some(home));
        assert_eq!(once, twice);
    }

    #[test]
    fn blocked_permission_error_names_the_action_and_path() {
        let stderr = concat!(
            "! \u{1b}[93mpermission requested: external_directory ",
            "(C:\\Users\\igor_\\.local\\share\\opencode\\storage\\project\\*); ",
            "auto-rejecting\u{1b}[0m"
        );
        let msg = blocked_permission_error(stderr).expect("detected");
        assert!(msg.contains("external_directory"), "{msg}");
        assert!(msg.contains("storage\\project\\*"), "{msg}");
    }

    #[test]
    fn blocked_permission_error_ignores_unrelated_stderr() {
        assert!(blocked_permission_error("opencode returned nothing").is_none());
        assert!(blocked_permission_error("permission requested").is_none());
    }

    #[test]
    fn context_note_formats_window_and_file() {
        let window = Some(ChatContext::Window {
            app_name: "Chrome".into(),
            title: "Olá".into(),
            url: Some("https://x.test".into()),
        });
        assert_eq!(
            context_note(&window),
            "App: Chrome, Window: Olá, URL: https://x.test"
        );
        let file = Some(ChatContext::File {
            name: "a.txt".into(),
            path: "C:\\a.txt".into(),
        });
        assert_eq!(context_note(&file), "File: a.txt (attached)");
        assert_eq!(context_note(&None), "");
    }

    #[test]
    fn parse_listen_url_reads_the_server_line() {
        assert_eq!(
            parse_listen_url("opencode server listening on http://127.0.0.1:49321"),
            Some("http://127.0.0.1:49321".to_string())
        );
        assert_eq!(
            parse_listen_url("Warning: OPENCODE_SERVER_PASSWORD is not set"),
            None
        );
    }

    #[test]
    fn describes_remote_and_local_mcps() {
        let remote = describe_mcp(
            "intercom",
            &serde_json::json!({
                "type": "remote",
                "url": "https://mcp.intercom.com/mcp",
                "enabled": true
            }),
            "Navi",
        );
        assert_eq!(remote.kind, "remote");
        assert_eq!(remote.target, "https://mcp.intercom.com/mcp");
        assert!(remote.enabled);
        assert_eq!(remote.source, "Navi");

        let local = describe_mcp(
            "fs",
            &serde_json::json!({ "command": ["npx", "-y", "server-fs"], "enabled": false }),
            "Global",
        );
        assert_eq!(local.kind, "local");
        assert_eq!(local.target, "npx -y server-fs");
        assert!(!local.enabled);
    }
}
