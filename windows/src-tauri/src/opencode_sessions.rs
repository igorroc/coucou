// Reads the chats the user has had — with the notch or in any terminal — out of
// opencode's canonical store.
//
// opencode now keeps sessions, messages and parts in `opencode.db` (SQLite),
// under `%USERPROFILE%\.local\share\opencode\`. We query it through
// `opencode db "<sql>" --format json` rather than adding a SQLite dependency,
// and fall back to the legacy `storage/*.json` layout for older opencode
// builds. Reads are direct; the only write is deletion through the official
// `opencode session delete`, and only after an explicit click.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::opencode_chat;
use crate::settings;

/// A conversation as shown in the history list.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// The working directory opencode recorded for the session.
    pub directory: String,
    /// The project the session belongs to (its git worktree, or "global").
    pub project_id: String,
    /// Display name of that project: "Mochi" for the notch chats.
    pub project_name: String,
    /// Full worktree path, for a secondary line in the UI.
    pub project_path: String,
    pub updated_at: i64,
}

/// A message as shown when a conversation is reopened.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// Internal opencode runs (the suggestion generator) must not show up as chats.
fn is_internal(title: &str, directory: &str) -> bool {
    title.starts_with("Coucou suggestions") || directory.to_lowercase().contains("coucou-suggest")
}

/// A session id we are willing to interpolate into SQL: `ses_…` and nothing else.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ── opencode.db (canonical) ───────────────────────────────────────────────────

/// Runs a read-only query through `opencode db` and returns the rows. `None`
/// means the DB path is unavailable (old opencode, missing binary) so the caller
/// can fall back to the JSON files.
fn query_rows(sql: &str) -> Option<Vec<Value>> {
    let configured = settings::load().opencode_bin;
    let bin = opencode_chat::resolve_bin(&configured)?;
    let args = vec![
        "db".to_string(),
        sql.to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let (ok, stdout, _stderr) = opencode_chat::run_for(20, &bin.to_string_lossy(), &args).ok()?;
    if !ok {
        return None;
    }
    // Tolerate any banner/log line before the JSON array.
    let start = stdout.find('[')?;
    let end = stdout.rfind(']')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(&stdout[start..=end]).ok()
}

fn list_sessions_db() -> Option<Vec<SessionInfo>> {
    let sql = "SELECT s.id AS id, s.title AS title, s.directory AS directory, \
                      s.project_id AS project_id, p.worktree AS worktree, \
                      s.time_updated AS updated \
               FROM session s LEFT JOIN project p ON p.id = s.project_id \
               WHERE s.parent_id IS NULL \
               ORDER BY s.time_updated DESC LIMIT 300";
    let rows = query_rows(sql)?;
    let assistant = settings::assistant_name(&settings::load());
    let mut sessions: Vec<SessionInfo> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id").and_then(Value::as_str)?;
            let project_id = row.get("project_id").and_then(Value::as_str).unwrap_or("").to_string();
            let worktree = row.get("worktree").and_then(Value::as_str).unwrap_or("").to_string();
            Some(SessionInfo {
                id: id.to_string(),
                title: row.get("title").and_then(Value::as_str).unwrap_or("Chat").to_string(),
                directory: row.get("directory").and_then(Value::as_str).unwrap_or("").to_string(),
                project_name: project_label(&project_id, &worktree, &assistant),
                project_id,
                project_path: worktree,
                updated_at: row.get("updated").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .filter(|s| !is_internal(&s.title, &s.directory))
        .collect();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Some(sessions)
}

fn push_msg(out: &mut Vec<(i64, String, String)>, time: i64, role: &str, text: &str) {
    if (role == "user" || role == "assistant") && !text.trim().is_empty() {
        let content = if role == "user" {
            strip_injected_prefix(text).to_string()
        } else {
            text.to_string()
        };
        if !content.trim().is_empty() {
            out.push((time, role.to_string(), content));
        }
    }
}

/// The first turn of a notch conversation is sent to opencode as one message:
/// the persona (`You are …`), the context and the actual question after a
/// `\n\nUser: ` separator — so opencode keeps the persona in context. When the
/// conversation is reopened, only the question belongs on screen.
fn strip_injected_prefix(text: &str) -> &str {
    if text.starts_with("You are ") {
        if let Some(pos) = text.find("\n\nUser: ") {
            return text[pos + "\n\nUser: ".len()..].trim_start_matches('\n');
        }
    }
    text
}

fn load_session_db(session_id: &str) -> Option<Vec<HistoryMessage>> {
    if !is_safe_id(session_id) {
        return Some(Vec::new());
    }
    let sql = format!(
        "SELECT m.id AS mid, m.data AS mdata, m.time_created AS mtime, p.data AS pdata \
         FROM message m LEFT JOIN part p ON p.message_id = m.id \
         WHERE m.session_id = '{session_id}' \
         ORDER BY m.time_created ASC, p.time_created ASC, p.id ASC"
    );
    let rows = query_rows(&sql)?;

    let mut messages: Vec<(i64, String, String)> = Vec::new();
    let mut cur_id = String::new();
    let mut cur_time = 0i64;
    let mut cur_role = String::new();
    let mut cur_text = String::new();
    let mut started = false;

    for row in &rows {
        let mid = row.get("mid").and_then(Value::as_str).unwrap_or("");
        if mid != cur_id {
            if started {
                push_msg(&mut messages, cur_time, &cur_role, &cur_text);
            }
            cur_id = mid.to_string();
            cur_time = row.get("mtime").and_then(Value::as_i64).unwrap_or(0);
            cur_role = row
                .get("mdata")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| v.get("role").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            cur_text.clear();
            started = true;
        }
        if let Some(pdata) = row.get("pdata").and_then(Value::as_str) {
            if let Ok(part) = serde_json::from_str::<Value>(pdata) {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !cur_text.is_empty() {
                            cur_text.push('\n');
                        }
                        cur_text.push_str(text);
                    }
                }
            }
        }
    }
    if started {
        push_msg(&mut messages, cur_time, &cur_role, &cur_text);
    }

    messages.sort_by_key(|(time, _, _)| *time);
    Some(
        messages
            .into_iter()
            .map(|(_, role, content)| HistoryMessage { role, content })
            .collect(),
    )
}

// ── Legacy JSON storage (older opencode) ──────────────────────────────────────

fn storage_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let dir = home.join(".local/share/opencode/storage");
    dir.is_dir().then_some(dir)
}

/// `projectID → worktree`, from storage/project/*.json.
fn project_worktrees(root: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(root.join("project")) else {
        return map;
    };
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let (Some(id), Some(worktree)) = (
            json.get("id").and_then(|v| v.as_str()),
            json.get("worktree").and_then(|v| v.as_str()),
        ) {
            map.insert(id.to_string(), worktree.to_string());
        }
    }
    map
}

/// The assistant's name for the notch's own folder and for the fallback
/// `global` project; otherwise the last path segment of the worktree (the
/// repo name).
fn project_label(project_id: &str, worktree: &str, assistant: &str) -> String {
    if worktree.is_empty() || worktree == "/" || project_id == "global" {
        return assistant.to_string();
    }
    // Compare canonically, then fall back to a case-insensitive string compare:
    // opencode may store the path with a different casing or separator.
    let chat = opencode_chat::chat_dir();
    let same = std::fs::canonicalize(worktree)
        .map(|p| p == chat)
        .unwrap_or(false)
        || worktree.eq_ignore_ascii_case(&chat.to_string_lossy());
    if same {
        return assistant.to_string();
    }
    Path::new(worktree)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(worktree)
        .to_string()
}

/// Every top-level conversation, newest first. Sub-agent sessions (children with
/// a `parentID`) are left out: the list is for the chats the user started.
pub fn list_sessions() -> Vec<SessionInfo> {
    if let Some(sessions) = list_sessions_db() {
        return sessions;
    }
    list_sessions_json()
}

fn list_sessions_json() -> Vec<SessionInfo> {
    let Some(root) = storage_dir() else {
        return Vec::new();
    };
    let projects = project_worktrees(&root);
    let assistant = settings::assistant_name(&settings::load());
    let mut sessions = Vec::new();

    let Ok(project_dirs) = std::fs::read_dir(root.join("session")) else {
        return sessions;
    };
    for project_dir in project_dirs.flatten() {
        let project_id = project_dir.file_name().to_string_lossy().to_string();
        let worktree = projects.get(&project_id).cloned().unwrap_or_default();
        let project_name = project_label(&project_id, &worktree, &assistant);

        let Ok(files) = std::fs::read_dir(project_dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(bytes) = std::fs::read(file.path()) else {
                continue;
            };
            let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            // Sub-agents are child sessions; skip them.
            if json.get("parentID").map(|v| !v.is_null()).unwrap_or(false) {
                continue;
            }
            let Some(id) = json.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let title = json
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Chat")
                .to_string();
            let directory = json
                .get("directory")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let updated_at = json
                .get("time")
                .and_then(|t| t.get("updated"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            sessions.push(SessionInfo {
                id: id.to_string(),
                title,
                directory,
                project_id: project_id.clone(),
                project_name: project_name.clone(),
                project_path: worktree.clone(),
                updated_at,
            });
        }
    }

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

/// Deletes one conversation through the official
/// `opencode session delete <id>` (cascades to messages, parts and child
/// sessions), with the legacy JSON files as fallback. Returns true when
/// anything was removed.
pub fn delete_session(session_id: &str) -> bool {
    if !is_safe_id(session_id) {
        return false;
    }
    let mut removed = delete_session_db(session_id);
    if delete_session_json(session_id) {
        removed = true;
    }
    removed
}

fn delete_session_db(session_id: &str) -> bool {
    let configured = settings::load().opencode_bin;
    let Some(bin) = opencode_chat::resolve_bin(&configured) else {
        return false;
    };
    let args = vec![
        "session".to_string(),
        "delete".to_string(),
        session_id.to_string(),
    ];
    opencode_chat::run_for(20, &bin.to_string_lossy(), &args)
        .map(|(ok, _, _)| ok)
        .unwrap_or(false)
}

/// Deletes the legacy JSON files of one conversation: the session file, its
/// message files and their part directories.
fn delete_session_json(session_id: &str) -> bool {
    if session_id.contains(['/', '\\', '.']) {
        return false;
    }
    let Some(root) = storage_dir() else {
        return false;
    };
    let mut removed = false;

    // Message ids first, so their part directories can go too.
    let msg_dir = root.join("message").join(session_id);
    if let Ok(files) = std::fs::read_dir(&msg_dir) {
        for file in files.flatten() {
            if let Ok(bytes) = std::fs::read(file.path()) {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                        if !id.contains(['/', '\\', '.']) {
                            let _ = std::fs::remove_dir_all(root.join("part").join(id));
                        }
                    }
                }
            }
        }
    }
    if msg_dir.exists() && std::fs::remove_dir_all(&msg_dir).is_ok() {
        removed = true;
    }

    if let Ok(project_dirs) = std::fs::read_dir(root.join("session")) {
        for project_dir in project_dirs.flatten() {
            let candidate = project_dir.path().join(format!("{session_id}.json"));
            if candidate.is_file() && std::fs::remove_file(&candidate).is_ok() {
                removed = true;
            }
        }
    }
    removed
}

/// The user/assistant turns of one conversation, in order. Tool and file parts
/// are skipped: this is the chat as the user saw it.
pub fn load_session(session_id: &str) -> Vec<HistoryMessage> {
    if let Some(messages) = load_session_db(session_id) {
        return messages;
    }
    load_session_json(session_id)
}

fn load_session_json(session_id: &str) -> Vec<HistoryMessage> {
    let Some(root) = storage_dir() else {
        return Vec::new();
    };

    // Tolerate a path-traversal attempt rather than trusting the id blindly.
    if session_id.contains(['/', '\\', '.']) {
        return Vec::new();
    }

    let msg_dir = root.join("message").join(session_id);
    let mut messages: Vec<(i64, String, String)> = Vec::new();

    let Ok(files) = std::fs::read_dir(&msg_dir) else {
        return Vec::new();
    };
    for file in files.flatten() {
        let Ok(bytes) = std::fs::read(file.path()) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let role = json
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if role != "user" && role != "assistant" {
            continue;
        }
        let Some(id) = json.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let created = json
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let text = message_text(&root, id);
        let text = if role == "user" {
            strip_injected_prefix(&text).to_string()
        } else {
            text
        };
        if text.trim().is_empty() {
            continue;
        }
        messages.push((created, role, text));
    }

    messages.sort_by_key(|(created, _, _)| *created);
    messages
        .into_iter()
        .map(|(_, role, content)| HistoryMessage { role, content })
        .collect()
}

/// Concatenates the `text` parts of a message, skipping tool/file parts.
fn message_text(root: &Path, message_id: &str) -> String {
    if message_id.contains(['/', '\\', '.']) {
        return String::new();
    }
    let part_dir = root.join("part").join(message_id);
    let Ok(files) = std::fs::read_dir(&part_dir) else {
        return String::new();
    };

    let mut parts: Vec<(String, String)> = Vec::new();
    for file in files.flatten() {
        let Ok(bytes) = std::fs::read(file.path()) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if json.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        let Some(text) = json.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        // Parts have no timestamp; their id increases within a message, so it
        // is the ordering key. The filename is the same id and always present.
        let order = json
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| file.file_name().to_string_lossy().to_string());
        parts.push((order, text.to_string()));
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    parts
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_ids() {
        assert!(load_session("../secrets").is_empty());
        assert!(load_session_json("../secrets").is_empty());
    }

    #[test]
    fn labels_mochi_projects() {
        assert_eq!(project_label("global", "/", "Mochi"), "Mochi");
        assert_eq!(project_label("global", "/", "Navi"), "Navi");
        assert_eq!(project_label("abc", "D:\\repos\\gateway.fy", "Navi"), "gateway.fy");
    }

    #[test]
    fn strips_injected_persona_prefix() {
        let blob = "You are Navi, a personal AI assistant living at the top of the user's screen.\n\nContext: File: a.txt (attached)\n\nUser: qual modelo vc está usando?";
        assert_eq!(strip_injected_prefix(blob), "qual modelo vc está usando?");
        // Untouched when it is not our injected first turn.
        assert_eq!(strip_injected_prefix("qual modelo vc está usando?"), "qual modelo vc está usando?");
        assert_eq!(strip_injected_prefix("You are awesome"), "You are awesome");
    }

    #[test]
    fn only_session_ids_reach_the_sql() {
        assert!(is_safe_id("ses_f0a406f50ffeuESRpHgBmjDal7"));
        assert!(is_safe_id("ses-abc_123"));
        assert!(!is_safe_id("ses'; DROP TABLE session; --"));
        assert!(!is_safe_id("../secrets"));
        assert!(!is_safe_id(""));
    }

    #[test]
    fn suggestion_runs_are_internal() {
        assert!(is_internal("Coucou suggestions", "C:/tmp/x"));
        assert!(is_internal("anything", "C:/AppData/Local/Temp/coucou-suggest-1"));
        assert!(!is_internal("Coucou chat", "C:/Users/x/AppData/Local/Coucou/chat"));
    }

    #[test]
    fn push_msg_keeps_only_user_and_assistant_text() {
        let mut out = Vec::new();
        push_msg(&mut out, 1, "user", "oi");
        push_msg(&mut out, 2, "tool", "ignore");
        push_msg(&mut out, 3, "assistant", "  ");
        push_msg(&mut out, 4, "assistant", "olá");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].2, "olá");
    }
}
