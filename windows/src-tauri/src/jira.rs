// Jira tasks for the dashboard's "Minhas tarefas" card.
//
// There is no Coucou-side Jira credential: we reuse the OAuth session opencode
// already stores for the Atlassian MCP (`~/.local/share/opencode/mcp-auth.json`,
// server `atlassian`). We speak just enough of the MCP "Streamable HTTP"
// transport (see `mcp.rs`) to call two read-only tools —
// `getAccessibleAtlassianResources` and `searchJiraIssuesUsingJql` — with the
// Bearer token, refreshing it through the Atlassian OAuth token endpoint
// (writing the rotated tokens back) when needed.
//
// The result is cached on disk in `%LOCALAPPDATA%\Coucou\jira_tasks.json` with a
// one-hour TTL, so the dashboard never hammers Jira; a refresh button forces it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mcp;
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
const TOKEN_ENDPOINT: &str = "https://auth.atlassian.com/oauth/token";

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
    /// Link to open the issue in the browser; empty when the site URL is unknown.
    #[serde(default)]
    pub url: String,
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
    /// Base site URL (`https://x.atlassian.net`), used to build issue links.
    #[serde(default)]
    site_url: String,
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

// ── MCP OAuth session (borrowed from opencode) ────────────────────────────────

/// Reads the Atlassian MCP credentials opencode stored, refreshing the access
/// token (and writing the rotated pair back) when it is about to expire.
fn session() -> Result<mcp::Session, String> {
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
        return Ok(mcp::Session { access_token, server_url });
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
    let (_head, resp) = mcp::curl_post(TOKEN_ENDPOINT, &headers, &body)?;
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

    Ok(mcp::Session { access_token: new_access, server_url })
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

/// The site's base URL (`https://x.atlassian.net`) from the same resource list.
fn parse_site_url(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            v.as_array()
                .and_then(|a| a.first())
                .and_then(|r| r.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
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
            url: String::new(),
        });
    }
    Ok(out)
}

// ── Public API ────────────────────────────────────────────────────────────────

fn call_tool(
    session: &mcp::Session,
    session_id: Option<&str>,
    id: u64,
    name: &str,
    arguments: Value,
) -> Result<String, String> {
    let result = mcp::rpc(
        session,
        session_id,
        Some(id),
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )?
    .ok_or_else(|| "MCP não respondeu.".to_string())?;
    mcp::tool_text(result)
}

fn fetch(cached_cloud_id: &str, cached_site_url: &str) -> Result<(String, String, Vec<JiraTask>), String> {
    let session = session()?;
    let session_id = mcp::initialize(&session)?;
    let sid = session_id.as_deref();

    // Re-read the resources when the site URL is missing too, so a cache written
    // before issue links existed gets one (without losing the cloud id).
    let (cloud_id, site_url) = if cached_cloud_id.is_empty() || cached_site_url.is_empty() {
        let text = call_tool(&session, sid, 2, "getAccessibleAtlassianResources", json!({}))?;
        (parse_cloud_id(&text)?, parse_site_url(&text))
    } else {
        (cached_cloud_id.to_string(), cached_site_url.to_string())
    };

    let text = call_tool(
        &session,
        sid,
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
    // The browse link is deterministic from the site URL + issue key.
    if !site_url.is_empty() {
        let base = site_url.trim_end_matches('/');
        for t in &mut tasks {
            t.url = format!("{base}/browse/{}", t.key);
        }
    }
    tasks.truncate(MAX_RESULTS as usize);
    Ok((cloud_id, site_url, tasks))
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

    match fetch(&cache.cloud_id, &cache.site_url) {
        Ok((cloud_id, site_url, tasks)) => {
            let fetched_at = now_secs();
            write_cache(&Cache { fetched_at, cloud_id, site_url, tasks: tasks.clone() });
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
        assert_eq!(parse_site_url(text), "https://x.atlassian.net");
        assert!(parse_cloud_id("[]").is_err());
        assert_eq!(parse_site_url("[]"), "");
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
        // The browse link is added by `fetch`, not the parser.
        assert_eq!(tasks[0].url, "");
        // Missing fields degrade to safe defaults, never panic.
        assert_eq!(tasks[1].status, "");
        assert_eq!(tasks[1].category, "new");
    }
}
