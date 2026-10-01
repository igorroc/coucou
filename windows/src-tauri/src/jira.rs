// Jira tasks for the dashboard's "Minhas tarefas" card.
//
// There is no Coucou-side Jira credential: we reuse the OAuth session opencode
// already stores for the Atlassian MCP (`~/.local/share/opencode/mcp-auth.json`,
// server `atlassian`). We speak just enough of the MCP "Streamable HTTP"
// transport to call two read-only tools — `getAccessibleAtlassianResources` and
// `searchJiraIssuesUsingJql` — with the Bearer token, refreshing it through the
// Atlassian OAuth token endpoint (writing the rotated tokens back) when needed.
//
// HTTP goes through `curl.exe` (shipped with Windows 10 1803+) rather than
// reqwest: some environments block raw sockets for freshly built executables
// while allowing the system curl, and curl also follows the OS proxy/TLS setup.
//
// The result is cached on disk in `%LOCALAPPDATA%\Coucou\jira_tasks.json` with a
// one-hour TTL, so the dashboard never hammers Jira; a refresh button forces it.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::log;

/// MCP server name inside opencode's auth file.
const MCP_SERVER: &str = "atlassian";
/// Local cache filename inside Coucou's local dir.
const CACHE_FILE: &str = "jira_tasks.json";
/// Cache lifetime. A refresh button always bypasses it.
const TTL_SECS: f64 = 3600.0;
/// Refresh the access token this many seconds before it actually expires.
const REFRESH_MARGIN: f64 = 120.0;
const MAX_RESULTS: u32 = 20;
const JQL: &str = "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC";
const CURL_TIMEOUT: &str = "30";
const TOKEN_ENDPOINT: &str = "https://auth.atlassian.com/oauth/token";
/// Keeps the spawned curl from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// One Jira issue, trimmed to what the card shows.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JiraTask {
    pub key: String,
    pub summary: String,
    /// Status display name, e.g. "Fase de Testes".
    pub status: String,
    /// statusCategory.key: "new" | "indeterminate" | "done".
    pub category: String,
    /// statusCategory.colorName: "blue" | "yellow" | "green" | "red" | "blue-gray".
    pub color: String,
    pub project: String,
}

/// What the front end receives.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraTasks {
    pub tasks: Vec<JiraTask>,
    /// Unix seconds of the fetch that produced `tasks` (0 when never fetched).
    pub fetched_at: f64,
    /// True when the payload comes from the on-disk cache.
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    #[serde(default)]
    fetched_at: f64,
    #[serde(default)]
    cloud_id: String,
    #[serde(default)]
    tasks: Vec<JiraTask>,
}

fn cache_path() -> PathBuf {
    crate::settings::local_dir().join(CACHE_FILE)
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

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn read_cache() -> Cache {
    std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_cache(cache: &Cache) {
    let dir = crate::settings::local_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_vec_pretty(cache) {
        if let Err(err) = std::fs::write(cache_path(), json) {
            log::line(format!("jira cache write failed: {err}"));
        }
    }
}

// ── curl transport ────────────────────────────────────────────────────────────

fn temp_path(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("coucou-jira-{}-{}-{}", std::process::id(), seq, tag))
}

/// One HTTP POST through curl. Returns the raw response headers and body.
fn curl_post(url: &str, headers: &[String], body: &str) -> Result<(String, String), String> {
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

    let output = cmd.output().map_err(|e| format!("curl não encontrado: {e}"))?;
    let headers_text = std::fs::read_to_string(&head_path).unwrap_or_default();
    let body_text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&body_path);
    let _ = std::fs::remove_file(&head_path);
    let _ = std::fs::remove_file(&out_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.trim().lines().last().unwrap_or("falha de rede").to_string();
        return Err(format!("Falha de rede: {detail}"));
    }
    Ok((headers_text, body_text))
}

/// Case-insensitive lookup of a header value in a curl `-D` dump.
fn header_value(headers: &str, name: &str) -> Option<String> {
    let want = format!("{}:", name.to_lowercase());
    for line in headers.lines() {
        let line = line.trim();
        if line.to_lowercase().starts_with(&want) {
            return Some(line[want.len()..].trim().to_string());
        }
    }
    None
}

/// JSON-RPC over MCP. Returns the `result` for the given id, or None for a
/// notification. SSE bodies (`event: message\ndata: {...}`) are supported.
fn mcp_post(
    session: &McpSession,
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
    if let Some(status) = head.lines().next() {
        if status.contains(" 401") {
            return Err("Sessão do Jira MCP expirada. Rode `opencode mcp auth atlassian`.".to_string());
        }
    }
    let wanted = id.unwrap();
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("data:") else { continue };
        let Ok(msg) = serde_json::from_str::<Value>(rest.trim()) else { continue };
        if msg.get("id").and_then(Value::as_u64) != Some(wanted) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let detail = err.get("message").and_then(Value::as_str).unwrap_or("erro do MCP");
            return Err(detail.to_string());
        }
        return Ok(msg.get("result").cloned());
    }
    Err("MCP não respondeu.".to_string())
}

fn initialize(session: &McpSession) -> Result<String, String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "coucou", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let headers = vec![
        format!("Authorization: Bearer {}", session.access_token),
        "Accept: application/json, text/event-stream".to_string(),
        "Content-Type: application/json".to_string(),
    ];
    let (head, _body) = curl_post(&session.server_url, &headers, &payload.to_string())?;
    let session_id = header_value(&head, "mcp-session-id").unwrap_or_default();
    if session_id.is_empty() {
        return Err("MCP não devolveu uma sessão.".to_string());
    }
    mcp_post(session, Some(&session_id), None, "notifications/initialized", json!({}))?;
    Ok(session_id)
}

/// Pulls the text payload out of a tools/call result.
fn tool_text(result: Value) -> Result<String, String> {
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

fn call_tool(
    session: &McpSession,
    session_id: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> Result<String, String> {
    let result = mcp_post(
        session,
        Some(session_id),
        Some(id),
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )?
    .ok_or_else(|| "MCP não respondeu.".to_string())?;
    tool_text(result)
}

// ── MCP OAuth session (borrowed from opencode) ────────────────────────────────

struct McpSession {
    access_token: String,
    server_url: String,
}

/// Reads the Atlassian MCP credentials opencode stored, refreshing the access
/// token (and writing the rotated pair back) when it is about to expire.
fn session() -> Result<McpSession, String> {
    let text = std::fs::read_to_string(auth_path())
        .map_err(|_| "Jira MCP não autenticado. Rode `opencode mcp auth atlassian`.".to_string())?;
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("mcp-auth.json inválido: {e}"))?;
    let entry = root
        .get(MCP_SERVER)
        .ok_or_else(|| "Jira MCP não autenticado. Rode `opencode mcp auth atlassian`.".to_string())?;

    let access_token = entry
        .pointer("/tokens/accessToken")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expires_at = entry.pointer("/tokens/expiresAt").and_then(Value::as_f64).unwrap_or(0.0);
    let server_url = entry
        .get("serverUrl")
        .and_then(Value::as_str)
        .unwrap_or("https://mcp.atlassian.com/v1/mcp/authv2")
        .to_string();

    if !access_token.is_empty() && expires_at > now_secs() + REFRESH_MARGIN {
        return Ok(McpSession { access_token, server_url });
    }

    let refresh_token = entry.pointer("/tokens/refreshToken").and_then(Value::as_str).unwrap_or("").to_string();
    let client_id = entry.pointer("/clientInfo/clientId").and_then(Value::as_str).unwrap_or("").to_string();
    let client_secret = entry.pointer("/clientInfo/clientSecret").and_then(Value::as_str).unwrap_or("").to_string();
    if refresh_token.is_empty() || client_id.is_empty() || client_secret.is_empty() {
        return Err("Sessão do Jira MCP expirada. Rode `opencode mcp auth atlassian`.".to_string());
    }

    let body = json!({
        "grant_type": "refresh_token",
        "client_id": client_id,
        "client_secret": client_secret,
        "refresh_token": refresh_token,
    })
    .to_string();
    let headers = vec!["Content-Type: application/json".to_string()];
    let (_head, resp) = curl_post(TOKEN_ENDPOINT, &headers, &body)?;
    let tokens: Value = serde_json::from_str(&resp)
        .map_err(|_| "Sessão do Jira MCP expirada. Rode `opencode mcp auth atlassian`.".to_string())?;
    let new_access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Resposta de token sem access_token.".to_string())?
        .to_string();
    let new_refresh = tokens.get("refresh_token").and_then(Value::as_str).map(str::to_string);
    let expires_in = tokens.get("expires_in").and_then(Value::as_f64).unwrap_or(3600.0);

    // Keep opencode's file in step — its refresh token rotates on every use.
    if let Ok(mut root) = serde_json::from_str::<Value>(&text) {
        if let Some(obj) = root.as_object_mut() {
            if let Some(entry) = obj.get_mut(MCP_SERVER).and_then(Value::as_object_mut) {
                if let Some(t) = entry.get_mut("tokens").and_then(Value::as_object_mut) {
                    t.insert("accessToken".into(), json!(new_access));
                    if let Some(r) = &new_refresh {
                        t.insert("refreshToken".into(), json!(r));
                    }
                    t.insert("expiresAt".into(), json!(now_secs() + expires_in));
                }
            }
            if let Ok(out) = serde_json::to_vec_pretty(&root) {
                let _ = std::fs::write(auth_path(), out);
            }
        }
    }

    Ok(McpSession { access_token: new_access, server_url })
}

// ── Pure parsers (testable) ───────────────────────────────────────────────────

fn parse_cloud_id(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    value
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Nenhum site Atlassian acessível.".to_string())
}

fn parse_issues(text: &str) -> Result<Vec<JiraTask>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let issues = value
        .get("issues")
        .and_then(Value::as_array)
        .ok_or_else(|| "Resposta do Jira sem issues.".to_string())?;
    let mut out = Vec::new();
    for issue in issues {
        let key = issue.get("key").and_then(Value::as_str);
        let fields = issue.get("fields");
        let (Some(key), Some(fields)) = (key, fields) else { continue };
        out.push(JiraTask {
            key: key.to_string(),
            summary: fields.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
            status: fields.pointer("/status/name").and_then(Value::as_str).unwrap_or("").to_string(),
            category: fields
                .pointer("/status/statusCategory/key")
                .and_then(Value::as_str)
                .unwrap_or("new")
                .to_string(),
            color: fields
                .pointer("/status/statusCategory/colorName")
                .and_then(Value::as_str)
                .unwrap_or("blue-gray")
                .to_string(),
            project: fields.pointer("/project/name").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

// ── Public API ────────────────────────────────────────────────────────────────

fn fetch(cached_cloud_id: &str) -> Result<(String, Vec<JiraTask>), String> {
    let session = session()?;
    let session_id = initialize(&session)?;

    let cloud_id = if cached_cloud_id.is_empty() {
        let text = call_tool(&session, &session_id, 2, "getAccessibleAtlassianResources", json!({}))?;
        parse_cloud_id(&text)?
    } else {
        cached_cloud_id.to_string()
    };

    let text = call_tool(
        &session,
        &session_id,
        3,
        "searchJiraIssuesUsingJql",
        json!({
            "cloudId": cloud_id,
            "jql": JQL,
            "maxResults": MAX_RESULTS,
            "fields": ["summary", "status", "priority", "project"],
            "responseContentFormat": "markdown",
            "searchResultMode": "issues",
        }),
    )?;
    let mut tasks = parse_issues(&text)?;
    tasks.truncate(MAX_RESULTS as usize);
    Ok((cloud_id, tasks))
}

/// The cached tasks, refreshed from the Jira MCP when the cache is stale (or
/// `force` is set). Never fails hard: on a fetch error it returns the stale
/// cache with `error` set, so the card keeps showing the last known tasks.
pub fn tasks(force: bool, paused: bool) -> JiraTasks {
    let cache = read_cache();
    let fresh = cache.fetched_at > 0.0 && now_secs() - cache.fetched_at < TTL_SECS;
    if !force && fresh {
        return JiraTasks { tasks: cache.tasks, fetched_at: cache.fetched_at, cached: true, error: None };
    }
    // Pausing Coucou means no network, including an automatic Jira refresh.
    if paused && !force {
        return JiraTasks { tasks: cache.tasks, fetched_at: cache.fetched_at, cached: true, error: None };
    }

    match fetch(&cache.cloud_id) {
        Ok((cloud_id, tasks)) => {
            let fetched_at = now_secs();
            write_cache(&Cache { fetched_at, cloud_id, tasks: tasks.clone() });
            JiraTasks { tasks, fetched_at, cached: false, error: None }
        }
        Err(err) => {
            log::line(format!("jira fetch failed: {err}"));
            JiraTasks { tasks: cache.tasks, fetched_at: cache.fetched_at, cached: true, error: Some(err) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_id_from_resources() {
        let text = r#"[{"id":"81e13e61","url":"https://x.atlassian.net"}]"#;
        assert_eq!(parse_cloud_id(text).unwrap(), "81e13e61");
        assert!(parse_cloud_id("[]").is_err());
    }

    #[test]
    fn parses_issues_into_tasks() {
        let text = r#"{
            "issues": [
                {"id":"1","key":"GFY-989","fields":{
                    "summary":"SSRF webhook",
                    "project":{"name":"GatewayFy Core"},
                    "status":{"name":"Fase de Testes","statusCategory":{"key":"indeterminate","colorName":"yellow"}}
                }},
                {"id":"2","key":"GFY-100","fields":{
                    "summary":"Sem status",
                    "project":{"name":"GatewayFy"}
                }}
            ]
        }"#;
        let tasks = parse_issues(text).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].key, "GFY-989");
        assert_eq!(tasks[0].status, "Fase de Testes");
        assert_eq!(tasks[0].category, "indeterminate");
        assert_eq!(tasks[0].color, "yellow");
        assert_eq!(tasks[0].project, "GatewayFy Core");
        // Missing fields degrade to safe defaults, never panic.
        assert_eq!(tasks[1].status, "");
        assert_eq!(tasks[1].category, "new");
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let head = "HTTP/1.1 200 OK\r\nMcp-Session-Id: r11-abc\r\nContent-Type: text/event-stream\r\n\r\n";
        assert_eq!(header_value(head, "mcp-session-id").as_deref(), Some("r11-abc"));
        assert!(header_value(head, "x-missing").is_none());
    }
}
