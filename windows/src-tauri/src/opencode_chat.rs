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
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Mutex;
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
    let mut text = format!(
        "You are {name}, a personal AI assistant living at the top of the user's screen."
    );
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

#[derive(Default)]
pub struct OpencodeChat {
    session: Mutex<Option<ChatSession>>,
}

struct ChatSession {
    id: String,
    dir: String,
}

impl OpencodeChat {
    pub fn reset(&self) {
        *self.session.lock().unwrap() = None;
    }

    /// Resumes an existing session: the next `send` reuses it with `--session`
    /// instead of starting a fresh conversation.
    pub fn attach(&self, id: String, dir: String) {
        *self.session.lock().unwrap() = Some(ChatSession { id, dir });
    }
}

/// The dedicated working directory for Mochi's own conversations. Kept apart from
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

/// The MCP servers Mochi's chat folder is provisioned with. Declared here so the
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

/// Makes sure `%LOCALAPPDATA%\Coucou\chat\opencode.json` declares the MCP servers
/// the notch needs. Additive on purpose: it only guarantees the managed entries
/// exist and never removes a server or a key the user added by hand. The file is
/// written only when its content actually changes, so opencode is not disturbed
/// on every launch.
pub fn ensure_chat_config() -> std::io::Result<()> {
    let path = chat_dir().join("opencode.json");
    let existing = std::fs::read(&path).ok();
    let rendered = merge_chat_config(existing.as_deref());
    // Untouched if nothing changed — avoids churn (and LSP reloads) each launch.
    if std::fs::read_to_string(&path).map(|s| s == rendered).unwrap_or(false) {
        return Ok(());
    }
    std::fs::write(&path, rendered)
}

/// Pure merge (no I/O) so it can be tested: guarantees the managed MCP servers
/// exist, leaves every other key and server exactly as the user left them.
fn merge_chat_config(existing: Option<&[u8]>) -> String {
    let mut root = existing
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }

    let obj = root.as_object_mut().expect("root is an object");
    obj.entry("$schema")
        .or_insert_with(|| serde_json::json!("https://opencode.ai/config.json"));
    let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
    if !mcp.is_object() {
        *mcp = serde_json::json!({});
    }
    let mcp_obj = mcp.as_object_mut().expect("mcp is an object");
    for (name, server) in managed_mcps().as_object().expect("managed_mcps is an object") {
        // Never clobber a server the user has customised under the same name.
        mcp_obj.entry(name.clone()).or_insert_with(|| server.clone());
    }

    serde_json::to_string_pretty(&root).unwrap_or_default()
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
    /// Where it was declared: "Mochi" (the chat folder) or "Global".
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
                Value::Array(parts) => {
                    parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
                }
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
        ("Mochi", chat_dir().join("opencode.json")),
        ("Global", global_config_dir().join("opencode.json")),
        ("Global", global_config_dir().join("config.json")),
    ];
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<McpInfo> = Vec::new();
    for (source, path) in sources {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(root) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(mcp) = root.get("mcp").and_then(Value::as_object) else { continue };
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
        "opencode not found. Install it (opencode.ai) or set its path in Settings → Chat.".to_string()
    })?;

    // First turn of a conversation carries the persona + context; later turns
    // reuse the session in the same directory.
    let first;
    let (session_id, dir, files, message) = {
        let guard = chat.session.lock().unwrap();
        match &*guard {
            Some(s) => {
                first = false;
                (Some(s.id.clone()), s.dir.clone(), Vec::new(), query.clone())
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
    let mut args: Vec<String> = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--title".into(),
        "Coucou chat".into(),
        "--dir".into(),
        dir.clone(),
    ];
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

    // One line per turn so a failure can be reconstructed from coucou.log.
    // The question is clipped: it may hold anything the user typed.
    crate::log::line(format!(
        "chat send bin={} model={} dir={} session={} first={} q={}",
        bin.display(),
        if model.is_empty() { "(default)" } else { model },
        dir,
        session_id.as_deref().unwrap_or("-"),
        first,
        clip(&query),
    ));

    let bin_str = bin.to_string_lossy().to_string();
    let (ok, stdout, stderr) =
        tokio::task::spawn_blocking(move || run_blocking(&bin_str, &args))
            .await
            .map_err(|e| format!("opencode task failed: {e}"))??;

    let parsed = parse_events(&stdout);
    // The session id is echoed in every log line below, so keep it borrowed.
    if let Some(id) = parsed.session_id.clone() {
        *chat.session.lock().unwrap() = Some(ChatSession { id, dir });
    }

    // The event stream's own error is the most precise one opencode gives us:
    // surface it to the island *and* keep it in the log with its context.
    if let Some(err) = parsed.error {
        crate::log::line(format!(
            "chat error session={} provider={} model={} name={} msg={}",
            parsed.session_id.as_deref().unwrap_or(session_id.as_deref().unwrap_or("-")),
            parsed.provider_id.as_deref().unwrap_or("-"),
            parsed.model_id.as_deref().unwrap_or("-"),
            parsed.error_name.as_deref().unwrap_or("-"),
            clip(&err),
        ));
        if !stderr.trim().is_empty() {
            crate::log::line(format!("chat error stderr={}", clip(stderr.trim())));
        }
        return Err(err);
    }
    if !ok {
        crate::log::line(format!(
            "chat failed session={} exit≠0 stderr={}",
            parsed.session_id.as_deref().unwrap_or(session_id.as_deref().unwrap_or("-")),
            clip(stderr.trim()),
        ));
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        let detail = if tail.trim().is_empty() {
            "unknown error"
        } else {
            tail.trim()
        };
        return Err(format!("opencode failed: {detail}"));
    }
    if parsed.text.trim().is_empty() {
        // No text and no error event: dump what came back so the shape is
        // visible — this is what distinguishes "answered empty" from "produced
        // no events at all".
        crate::log::line(format!(
            "chat empty session={} stdout={} stderr={}",
            parsed.session_id.as_deref().unwrap_or(session_id.as_deref().unwrap_or("-")),
            clip(stdout.trim()),
            clip(stderr.trim()),
        ));
        return Err("opencode returned no text.".into());
    }
    Ok(ChatReply {
        text: parsed.text.trim().to_string(),
    })
}

/// Working dir + file attachments + context line for a fresh conversation.
/// The working directory is always Mochi's own folder (never the repo a dropped
/// file came from), so the conversation lands under the "Mochi" project.
fn first_turn_context(context: &Option<ChatContext>) -> (String, Vec<String>, String) {
    let dir = chat_dir().to_string_lossy().to_string();
    match context {
        Some(ChatContext::File { name, path }) => {
            (dir, vec![path.clone()], format!("File: {name} (attached)"))
        }
        Some(ChatContext::Window { app_name, title, url }) => {
            let mut text = format!("App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            (dir, Vec::new(), text)
        }
        None => (dir, Vec::new(), String::new()),
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

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

struct Parsed {
    session_id: Option<String>,
    text: String,
    error: Option<String>,
    /// Extra fields from the error event, for the log only (never the UI).
    error_name: Option<String>,
    provider_id: Option<String>,
    model_id: Option<String>,
}

/// Pulls the assistant text + session id out of `opencode run --format json`
/// (newline-delimited events, verified against opencode 1.x):
///   {"type":"text","sessionID":"ses_…","part":{"type":"text","text":"… Ness"}}
///   {"type":"tool_use",…}                          → ignored (tools, not chat)
///   {"type":"step_start" | "step_finish",…}        → ignored
///   {"type":"error","error":{"data":{"message":…}}} → surfaced to the island
/// Everything is best-effort: unknown lines and shapes are skipped, never fatal.
fn parse_events(stdout: &str) -> Parsed {
    let mut session_id: Option<String> = None;
    let mut text = String::new();
    let mut error: Option<String> = None;
    let mut error_name: Option<String> = None;
    let mut provider_id: Option<String> = None;
    let mut model_id: Option<String> = None;

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
        match event.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = event
                    .get("part")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    text.push_str(t);
                }
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
                    event.get("providerID").and_then(Value::as_str).map(str::to_string)
                });
                model_id = model_id.or_else(|| {
                    event.get("modelID").and_then(Value::as_str).map(str::to_string)
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_carries_both_instructions_and_skips_empty() {
        let p = persona("Noma", "Sou CTO e foco em pagamentos.", "Seja direta.");
        assert!(p.starts_with("You are Noma,"));
        assert!(p.contains("About the user you are helping:\nSou CTO e foco em pagamentos."));
        assert!(p.contains("How you should behave and what to prioritise:\nSeja direta."));
        assert!(p.contains("no markdown formatting"));

        let bare = persona("Mochi", "  ", "");
        assert!(bare.starts_with("You are Mochi,"));
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
        let out = merge_chat_config(None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcp"]["atlassian"]["type"], "remote");
        assert_eq!(v["mcp"]["intercom"]["url"], "https://mcp.intercom.com/mcp");
        assert_eq!(v["mcp"]["composio"]["url"], "https://connect.composio.dev/mcp");
        assert_eq!(v["$schema"], "https://opencode.ai/config.json");
    }

    #[test]
    fn merge_preserves_user_keys_and_servers() {
        let existing = br#"{"model":"opencode-go/x","mcp":{"mine":{"type":"local"}}}"#;
        let out = merge_chat_config(Some(existing));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["model"], "opencode-go/x");
        assert!(v["mcp"]["mine"].is_object(), "user MCP kept");
        assert!(v["mcp"]["intercom"].is_object(), "managed MCP added");
    }

    #[test]
    fn merge_does_not_clobber_a_customised_server() {
        let existing = br#"{"mcp":{"atlassian":{"type":"remote","url":"https://mine.example"}}}"#;
        let out = merge_chat_config(Some(existing));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcp"]["atlassian"]["url"], "https://mine.example");
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_chat_config(None);
        let twice = merge_chat_config(Some(once.as_bytes()));
        assert_eq!(once, twice);
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
            "Mochi",
        );
        assert_eq!(remote.kind, "remote");
        assert_eq!(remote.target, "https://mcp.intercom.com/mcp");
        assert!(remote.enabled);
        assert_eq!(remote.source, "Mochi");

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
