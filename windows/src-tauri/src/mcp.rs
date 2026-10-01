// Minimal MCP "Streamable HTTP" client, shared by the Jira (Atlassian) and
// Google Calendar (Composio) integrations.
//
// HTTP goes through `curl.exe` (shipped with Windows 10 1803+) rather than
// reqwest: some environments block raw sockets for freshly built executables
// while allowing the system curl, and curl also follows the OS proxy/TLS setup.
//
// The OAuth session is read from opencode's `~/.local/share/opencode/mcp-auth.json`,
// so Navi Assistant never asks for — or stores — a credential of its own.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

const CURL_TIMEOUT: &str = "30";
/// Keeps the spawned curl from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// One authenticated MCP server from opencode's auth file.
#[derive(Clone)]
pub struct Session {
    pub access_token: String,
    pub server_url: String,
}

fn auth_path() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("opencode")
        .join("mcp-auth.json")
}

fn temp_path(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "navi-assistant-mcp-{}-{}-{}",
        std::process::id(),
        seq,
        tag
    ))
}

/// One HTTP POST through curl. Returns the raw response headers and body.
pub fn curl_post(url: &str, headers: &[String], body: &str) -> Result<(String, String), String> {
    let body_path = temp_path("body.json");
    let head_path = temp_path("head.txt");
    let out_path = temp_path("out.txt");
    std::fs::write(&body_path, body).map_err(|e| format!("curl: {e}"))?;

    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .arg("--max-time")
        .arg(CURL_TIMEOUT)
        .arg("-X")
        .arg("POST")
        .arg(url);
    for h in headers {
        cmd.arg("-H").arg(h);
    }
    cmd.arg("--data-binary")
        .arg(format!("@{}", body_path.display()))
        .arg("-D")
        .arg(&head_path)
        .arg("-o")
        .arg(&out_path)
        .creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .map_err(|e| format!("curl não encontrado: {e}"))?;
    let headers_text = std::fs::read_to_string(&head_path).unwrap_or_default();
    let body_text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&body_path);
    let _ = std::fs::remove_file(&head_path);
    let _ = std::fs::remove_file(&out_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr
            .trim()
            .lines()
            .last()
            .unwrap_or("falha de rede")
            .to_string();
        return Err(format!("Falha de rede: {detail}"));
    }
    Ok((headers_text, body_text))
}

/// Case-insensitive lookup of a header value in a curl `-D` dump.
pub fn header_value(headers: &str, name: &str) -> Option<String> {
    let want = format!("{}:", name.to_lowercase());
    for line in headers.lines() {
        let line = line.trim();
        if line.to_lowercase().starts_with(&want) {
            return Some(line[want.len()..].trim().to_string());
        }
    }
    None
}

/// The stored OAuth session for a server in opencode's `mcp-auth.json`.
pub fn read_session(server: &str) -> Result<Session, String> {
    let text = std::fs::read_to_string(auth_path()).map_err(|_| {
        format!("'{server}' não autenticado no opencode. Rode `opencode mcp auth {server}`.")
    })?;
    let root: Value =
        serde_json::from_str(&text).map_err(|e| format!("mcp-auth.json inválido: {e}"))?;
    let entry = root.get(server).ok_or_else(|| {
        format!("'{server}' não autenticado no opencode. Rode `opencode mcp auth {server}`.")
    })?;
    let access_token = entry
        .pointer("/tokens/accessToken")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let server_url = entry
        .get("serverUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() || server_url.is_empty() {
        return Err(format!(
            "'{server}' sem token ou URL. Rode `opencode mcp auth {server}`."
        ));
    }
    Ok(Session {
        access_token,
        server_url,
    })
}

/// One JSON-RPC POST. `id` None = a notification (no reply). SSE bodies
/// (`event: message\ndata: {...}`) are supported.
pub fn rpc(
    session: &Session,
    session_id: Option<&str>,
    id: Option<u64>,
    method: &str,
    params: Value,
) -> Result<Option<Value>, String> {
    let mut payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if let Some(id) = id {
        payload["id"] = json!(id);
    }
    let mut headers = vec![
        format!("Authorization: Bearer {}", session.access_token),
        "Accept: application/json, text/event-stream".to_string(),
        "Content-Type: application/json".to_string(),
    ];
    if let Some(sid) = session_id {
        headers.push(format!("Mcp-Session-Id: {sid}"));
    }
    let (head, body) = curl_post(&session.server_url, &headers, &payload.to_string())?;

    if id.is_none() {
        return Ok(None);
    }
    if head
        .lines()
        .next()
        .map(|s| s.contains(" 401"))
        .unwrap_or(false)
    {
        return Err("Sessão do MCP expirada. Autentique de novo no opencode.".to_string());
    }
    let wanted = id.unwrap();
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(msg) = serde_json::from_str::<Value>(rest.trim()) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_u64) != Some(wanted) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let detail = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("erro do MCP");
            return Err(detail.to_string());
        }
        return Ok(msg.get("result").cloned());
    }
    Err("MCP não respondeu.".to_string())
}

/// Opens an MCP session; returns its id when the server issues one (Composio
/// does not, Atlassian does).
pub fn initialize(session: &Session) -> Result<Option<String>, String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "navi-assistant", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let headers = vec![
        format!("Authorization: Bearer {}", session.access_token),
        "Accept: application/json, text/event-stream".to_string(),
        "Content-Type: application/json".to_string(),
    ];
    let (head, _body) = curl_post(&session.server_url, &headers, &payload.to_string())?;
    let session_id = header_value(&head, "mcp-session-id").filter(|s| !s.is_empty());
    rpc(
        session,
        session_id.as_deref(),
        None,
        "notifications/initialized",
        json!({}),
    )?;
    Ok(session_id)
}

/// Text payload of a tools/call result.
pub fn tool_text(result: Value) -> Result<String, String> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("erro do MCP");
        return Err(text.to_string());
    }
    result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "MCP devolveu um resultado vazio.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_is_case_insensitive() {
        let head =
            "HTTP/1.1 200 OK\r\nMcp-Session-Id: r11-abc\r\nContent-Type: text/event-stream\r\n\r\n";
        assert_eq!(
            header_value(head, "mcp-session-id").as_deref(),
            Some("r11-abc")
        );
        assert!(header_value(head, "x-missing").is_none());
    }
}
