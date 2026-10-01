// The user's personal Google Tasks, via the Composio MCP.
//
// Composio (https://connect.composio.dev/mcp) fronts `GOOGLETASKS_LIST_ALL_TASKS`,
// which returns the open tasks across every task list, each annotated with its
// list name. We call it through `COMPOSIO_MULTI_EXECUTE_TOOL`; the task list
// comes back as JSON inside the tool's text payload. Cached for 15 minutes; a
// refresh button forces it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::log;
use crate::mcp;

const MCP_SERVER: &str = "composio";
const CACHE_FILE: &str = "google_tasks.json";
/// Cache lifetime. A refresh button always bypasses it.
const TTL_SECS: f64 = 900.0;
/// Cap on the tasks the card will show (the list scrolls internally anyway).
const MAX_TASKS: usize = 25;

/// One personal task, trimmed to what the card shows.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTask {
    pub id: String,
    pub title: String,
    /// The task list's name, e.g. "Pessoal".
    pub list: String,
    /// Due date (RFC3339 or `yyyy-mm-dd`); empty when the task has none.
    pub due: String,
    /// Link to open the task in the Google Tasks web UI.
    pub url: String,
}

/// What the front end receives.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTasks {
    pub tasks: Vec<GoogleTask>,
    pub fetched_at: f64,
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    #[serde(default)]
    fetched_at: f64,
    #[serde(default)]
    tasks: Vec<GoogleTask>,
}

fn cache_path() -> PathBuf {
    crate::settings::local_dir().join(CACHE_FILE)
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
            log::line(format!("google tasks cache write failed: {err}"));
        }
    }
}

// ── Composio response parsing (testable) ──────────────────────────────────────

/// Best-effort human-readable message from a Composio/Google error value.
fn value_error(value: &Value) -> String {
    if let Some(msg) = value.get("message").and_then(Value::as_str) {
        return msg.to_string();
    }
    let text = value.to_string();
    if text.len() > 300 {
        format!("{}…", &text[..300])
    } else {
        text
    }
}

/// Collects every array stored under one of `keys` anywhere in the payload —
/// including one encoded as a JSON string — so the parser survives Composio's
/// wrapping without depending on the exact nesting.
fn collect_arrays(value: &Value, keys: &[&str], out: &mut Vec<Value>) {
    match value {
        Value::Array(arr) => {
            for v in arr {
                collect_arrays(v, keys, out);
            }
        }
        Value::String(s) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                collect_arrays(&parsed, keys, out);
            }
        }
        Value::Object(map) => {
            for (key, v) in map {
                if keys.contains(&key.as_str()) && v.is_array() {
                    out.extend(v.as_array().cloned().unwrap_or_default());
                } else {
                    collect_arrays(v, keys, out);
                }
            }
        }
        _ => {}
    }
}

/// One task object → the card's row, or None when it is empty/completed/deleted.
fn to_task(item: &Value) -> Option<GoogleTask> {
    if item.get("status").and_then(Value::as_str) == Some("completed") {
        return None;
    }
    if item.get("deleted").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    // `webViewLink` deep-links the task in the Google Tasks web UI. `selfLink`
    // is a bare API URL (useless in a browser), so fall back to the tasks home.
    let url = item
        .get("webViewLink")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://tasks.google.com/")
        .to_string();
    Some(GoogleTask {
        id: item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        title,
        list: item
            .get("tasklist_title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        due: item
            .get("due")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url,
    })
}

/// Pulls the tasks out of a `COMPOSIO_MULTI_EXECUTE_TOOL` payload.
fn parse_tasks(text: &str) -> Result<Vec<GoogleTask>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    if value.get("successful").and_then(Value::as_bool) == Some(false) {
        let detail = value
            .get("error")
            .map(value_error)
            .unwrap_or_else(|| "erro do Composio".into());
        return Err(detail);
    }
    if let Some(response) = value.pointer("/data/results/0/response") {
        if response.get("successful").and_then(Value::as_bool) == Some(false) {
            let detail = response
                .get("error")
                .map(value_error)
                .or_else(|| value.pointer("/data/error").map(value_error))
                .unwrap_or_else(|| "erro do Composio".into());
            return Err(detail);
        }
    }

    // Prefer `tasks`; only fall back to the generic `items` key if none is found,
    // so a task-list payload is never mistaken for a task list of titles.
    let mut arrays = Vec::new();
    collect_arrays(&value, &["tasks"], &mut arrays);
    if arrays.is_empty() {
        collect_arrays(&value, &["items"], &mut arrays);
    }
    let mut tasks: Vec<GoogleTask> = arrays.iter().filter_map(to_task).collect();
    // Composio may annotate the same task in more than one list pass.
    tasks.sort_by(|a, b| a.id.cmp(&b.id).then(a.title.cmp(&b.title)));
    tasks.dedup_by(|a, b| a.id == b.id);
    Ok(tasks)
}

/// Masked and ordered for the card: tasks with a due date first (soonest), then
/// undated ones, each group keeping its title order. Truncated.
fn order_tasks(mut tasks: Vec<GoogleTask>) -> Vec<GoogleTask> {
    tasks.sort_by(|a, b| {
        let (ad, bd) = (a.due.is_empty(), b.due.is_empty());
        ad.cmp(&bd)
            .then_with(|| a.due.cmp(&b.due))
            .then_with(|| a.title.cmp(&b.title))
    });
    tasks.truncate(MAX_TASKS);
    tasks
}

// ── Public API ────────────────────────────────────────────────────────────────

fn fetch() -> Result<Vec<GoogleTask>, String> {
    let session = mcp::read_session(MCP_SERVER)?;
    let session_id = mcp::initialize(&session)?;
    let args = json!({
        "tools": [{
            "tool_slug": "GOOGLETASKS_LIST_ALL_TASKS",
            "arguments": {
                "showCompleted": false,
                "showHidden": false,
                "showDeleted": false,
                "max_tasks_total": 50
            }
        }],
        "sync_response_to_workbench": false,
        "thought": "list the user's open personal tasks for the Navi Assistant dashboard",
        "memory": {},
        "current_step": "FETCHING_TASKS"
    });
    let result = mcp::rpc(
        &session,
        session_id.as_deref(),
        Some(2),
        "tools/call",
        json!({ "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "arguments": args }),
    )?
    .ok_or_else(|| "MCP não respondeu.".to_string())?;
    let text = mcp::tool_text(result)?;
    Ok(order_tasks(parse_tasks(&text)?))
}

/// The cached personal tasks, refreshed from Composio when the cache is stale
/// (or `force` is set). Never fails hard: on a fetch error it returns the stale
/// cache with `error` set.
pub fn tasks(force: bool, paused: bool) -> GoogleTasks {
    let cache = read_cache();
    let fresh = cache.fetched_at > 0.0 && now_secs() - cache.fetched_at < TTL_SECS;
    if !force && fresh {
        return GoogleTasks {
            tasks: cache.tasks,
            fetched_at: cache.fetched_at,
            cached: true,
            error: None,
        };
    }
    if paused && !force {
        return GoogleTasks {
            tasks: cache.tasks,
            fetched_at: cache.fetched_at,
            cached: true,
            error: None,
        };
    }

    match fetch() {
        Ok(tasks) => {
            let fetched_at = now_secs();
            write_cache(&Cache {
                fetched_at,
                tasks: tasks.clone(),
            });
            GoogleTasks {
                tasks,
                fetched_at,
                cached: false,
                error: None,
            }
        }
        Err(err) => {
            log::line(format!("google tasks fetch failed: {err}"));
            GoogleTasks {
                tasks: cache.tasks,
                fetched_at: cache.fetched_at,
                cached: true,
                error: Some(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tasks_from_composio_wrapper() {
        let text = r#"{
            "data": { "results": [ { "response": { "successful": true, "data": {
                "tasks": [
                    {"id":"t1","title":"Comprar café","status":"needsAction","tasklist_title":"Pessoal","webViewLink":"https://tasks.google.com/t/1"},
                    {"id":"t2","title":"Concluída","status":"completed"},
                    {"id":"t3","title":"Pagar luz","tasklist_title":"Casa","due":"2026-10-05T00:00:00.000Z"}
                ]
            } } } ] }
        }"#;
        let tasks = parse_tasks(text).unwrap();
        assert_eq!(tasks.len(), 2, "completed dropped");
        let first = tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(first.list, "Pessoal");
        assert_eq!(first.url, "https://tasks.google.com/t/1");
    }

    #[test]
    fn reads_a_json_encoded_data_string() {
        let inner = r#"{"tasks":[{"id":"x","title":"Ir à academia"}]}"#;
        let escaped = serde_json::to_string(inner).unwrap();
        let text = format!(
            r#"{{"data":{{"results":[{{"response":{{"successful":true,"data":{escaped}}}}}]}}}}"#
        );
        let tasks = parse_tasks(&text).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Ir à academia");
    }

    #[test]
    fn orders_dated_before_undated() {
        let mk = |id: &str, due: &str| GoogleTask {
            id: id.into(),
            title: id.into(),
            list: String::new(),
            due: due.into(),
            url: String::new(),
        };
        let ordered = order_tasks(vec![
            mk("b", ""),
            mk("a", "2026-10-02"),
            mk("c", "2026-10-01"),
        ]);
        let ids: Vec<_> = ordered.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn surfaces_composio_errors() {
        let text = r#"{"data":{"results":[{"response":{"successful":false,"error":{"message":"No connected account"}}}]}}"#;
        assert_eq!(parse_tasks(text).unwrap_err(), "No connected account");
    }
}
