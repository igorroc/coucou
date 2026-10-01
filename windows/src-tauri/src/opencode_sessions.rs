// Reads the chats the user has had from the notch, straight out of the
// opencode data directory on disk.
//
// opencode owns the history: sessions live under
// `%USERPROFILE%\.local\share\opencode\storage`, one JSON file per session,
// message and part. That is read-only here — Coucou never writes to it — so the
// list stays consistent with whatever opencode itself shows.
//
// TODO: migrate to reading opencode.db (SQLite), the canonical store that
// supersedes these JSON files. The layout below is a documented, best-effort
// snapshot and unknown shapes are skipped rather than fatal.

use std::path::{Path, PathBuf};

use serde::Serialize;

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

/// "Mochi" for the notch's own folder and for the fallback `global` project;
/// otherwise the last path segment of the worktree (the repo name).
fn project_label(project_id: &str, worktree: &str) -> String {
    if worktree.is_empty() || worktree == "/" || project_id == "global" {
        return "Mochi".to_string();
    }
    // Compare canonically, then fall back to a case-insensitive string compare:
    // opencode may store the path with a different casing or separator.
    let chat = crate::opencode_chat::chat_dir();
    let same = std::fs::canonicalize(worktree)
        .map(|p| p == chat)
        .unwrap_or(false)
        || worktree.eq_ignore_ascii_case(&chat.to_string_lossy());
    if same {
        return "Mochi".to_string();
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
    let Some(root) = storage_dir() else {
        return Vec::new();
    };
    let projects = project_worktrees(&root);
    let mut sessions = Vec::new();

    let Ok(project_dirs) = std::fs::read_dir(root.join("session")) else {
        return sessions;
    };
    for project_dir in project_dirs.flatten() {
        let project_id = project_dir.file_name().to_string_lossy().to_string();
        let worktree = projects.get(&project_id).cloned().unwrap_or_default();
        let project_name = project_label(&project_id, &worktree);

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

/// The user/assistant turns of one conversation, in order. Tool and file parts
/// are skipped: this is the chat as the user saw it.
pub fn load_session(session_id: &str) -> Vec<HistoryMessage> {
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
        assert!(load_session("ses_ok").is_empty());
    }

    #[test]
    fn labels_mochi_projects() {
        assert_eq!(project_label("global", "/"), "Mochi");
        assert_eq!(project_label("abc", "D:\\repos\\gateway.fy"), "gateway.fy");
    }
}
