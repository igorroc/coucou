// Chat "thinking" logs — one JSONL file per turn, in a TEMP folder, so the
// island's reasoning, tool calls, step timings and errors can be inspected and
// searched without digging through the mixed navi-assistant.log.
//
//   %TEMP%\Navi Assistant\logs\<YYYY-MM-DD_HH-MM-SS-mmm>.jsonl   one turn
//   %TEMP%\Navi Assistant\logs\chat-latest.jsonl                 always the last
//
// Every line is a JSON object, so `rg` finds a word and `jq` picks fields. The
// events keep opencode's own millisecond timestamps, which is what makes a slow
// turn (model latency vs. a slow tool) readable after the fact.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use windows::Win32::System::SystemInformation::GetLocalTime;

/// Cap on a single free-text field (reasoning, tool output, raw stdout). Much
/// larger than navi-assistant.log's: this file exists to be read in full.
const CLIP: usize = 100_000;

/// How many turns the TEMP folder keeps before the oldest are dropped.
const KEEP: usize = 200;

/// Truncates on a char boundary so a multi-byte character is never split.
pub fn clip(s: &str) -> String {
    if s.len() <= CLIP {
        return s.to_string();
    }
    let mut end = CLIP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[{} bytes]", &s[..end], s.len())
}

/// %TEMP%\Navi Assistant\logs, created if missing.
pub fn logs_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("Navi Assistant").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn latest_path() -> PathBuf {
    logs_dir().join("chat-latest.jsonl")
}

/// The opencode CLI keeps its own raw provider log here — the place to look when
/// the event stream comes back empty.
pub fn opencode_log_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("opencode")
        .join("log")
}

/// Wall-clock stamp, split into a filename-safe id and a human `at`.
fn now() -> (String, String) {
    let t = unsafe { GetLocalTime() };
    let date = format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay);
    let hms = format!("{:02}-{:02}-{:02}", t.wHour, t.wMinute, t.wSecond);
    let id = format!("{date}_{hms}-{:03}", t.wMilliseconds);
    let at = format!(
        "{date} {:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    (id, at)
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One turn's log being built in memory; flushed to disk by `finish`.
pub struct TurnLog {
    id: String,
    started: Instant,
    /// First event timestamp, so later events can carry a relative offset.
    base_ms: Option<i64>,
    records: Vec<Value>,
}

impl TurnLog {
    /// Opens a turn. `meta` is merged into the `start` record (query, model,
    /// dir, session…). Use `None` for the provider's default model.
    pub fn begin(provider: &str, meta: Value) -> Self {
        let (id, at) = now();
        let mut start = json!({ "event": "start", "at": at, "provider": provider });
        if let (Some(dst), Some(src)) = (start.as_object_mut(), meta.as_object()) {
            for (key, value) in src {
                dst.insert(key.clone(), value.clone());
            }
        }
        Self { id, started: Instant::now(), base_ms: None, records: vec![start] }
    }

    /// Records one decoded event. `at_ms` is the provider's own timestamp (ms
    /// since the epoch) when it has one; it becomes `atMs` plus an `offsetMs`
    /// relative to the turn's first event.
    pub fn event(&mut self, kind: &str, at_ms: Option<i64>, mut data: Value) {
        if !data.is_object() {
            data = json!({ "value": data });
        }
        data["event"] = json!(kind);
        if let Some(t) = at_ms {
            let base = *self.base_ms.get_or_insert(t);
            data["atMs"] = json!(t);
            data["offsetMs"] = json!(t - base);
        }
        self.records.push(data);
    }

    /// A free-form record (e.g. the raw stdout when no events were parsed).
    pub fn raw(&mut self, kind: &str, text: &str) {
        self.records.push(json!({ "event": kind, "text": clip(text) }));
    }

    /// Writes the turn to its own file and refreshes `chat-latest.jsonl`.
    /// Returns the turn's own path, or `None` when the folder is unwritable.
    pub fn finish(self, mut result: Value) -> Option<PathBuf> {
        if !result.is_object() {
            result = json!({ "value": result });
        }
        result["event"] = json!("end");
        result["elapsedMs"] = json!(self.started.elapsed().as_millis() as u64);
        let (_, at) = now();
        result["at"] = json!(at);

        let mut body = String::new();
        for record in self.records.iter().chain(std::iter::once(&result)) {
            match serde_json::to_string(record) {
                Ok(line) => {
                    body.push_str(&line);
                    body.push('\n');
                }
                Err(_) => continue,
            }
        }

        let dir = logs_dir();
        let path = dir.join(format!("{}.jsonl", self.id));
        if std::fs::write(&path, &body).is_err() {
            return None;
        }
        let _ = std::fs::write(latest_path(), &body);
        prune_old();
        Some(path)
    }
}

/// Keeps the newest `KEEP` turns; filenames sort chronologically.
fn prune_old() {
    let Ok(entries) = std::fs::read_dir(logs_dir()) else { return };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name().is_some_and(|n| n != "chat-latest.jsonl")
        })
        .collect();
    if files.len() <= KEEP {
        return;
    }
    files.sort();
    for path in &files[..files.len() - KEEP] {
        let _ = std::fs::remove_file(path);
    }
}

/// One turn as shown in Settings → Logs, summarised from its `start`/`end`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLogFile {
    pub name: String,
    pub size: u64,
    pub modified_at: u64,
    pub at: String,
    pub provider: String,
    pub model: Option<String>,
    pub query: String,
    pub ok: bool,
    pub elapsed_ms: u64,
    pub tools: u32,
    pub steps: u32,
    pub thinking_chars: u32,
}

/// The most recent turns, newest first, capped at 50.
pub fn list() -> Vec<ChatLogFile> {
    let Ok(entries) = std::fs::read_dir(logs_dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name().is_some_and(|n| n != "chat-latest.jsonl")
        })
        .collect();
    files.sort();
    files.reverse();
    files.truncate(50);
    files.into_iter().filter_map(summarize).collect()
}

fn summarize(path: PathBuf) -> Option<ChatLogFile> {
    let meta = std::fs::metadata(&path).ok()?;
    let name = path.file_name()?.to_string_lossy().to_string();
    let text = std::fs::read_to_string(&path).ok()?;

    let mut out = ChatLogFile {
        name,
        size: meta.len(),
        modified_at: meta.modified().map(unix_ms).unwrap_or(0),
        at: String::new(),
        provider: String::new(),
        model: None,
        query: String::new(),
        ok: false,
        elapsed_ms: 0,
        tools: 0,
        steps: 0,
        thinking_chars: 0,
    };

    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
        match record.get("event").and_then(Value::as_str) {
            Some("start") => {
                out.at = str_field(&record, "at");
                out.provider = str_field(&record, "provider");
                out.model = record
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string);
                out.query = str_field(&record, "query");
            }
            Some("end") => {
                out.ok = record.get("ok").and_then(Value::as_bool).unwrap_or(false);
                out.elapsed_ms = record.get("elapsedMs").and_then(Value::as_u64).unwrap_or(0);
            }
            Some("tool") => out.tools += 1,
            Some("reasoning") => {
                out.thinking_chars += record
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|t| t.chars().count() as u32)
                    .unwrap_or(0);
            }
            Some("step_finish") => out.steps += 1,
            _ => {}
        }
    }
    Some(out)
}

fn str_field(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Resolves a log file by bare name, refusing anything that escapes the folder.
pub fn resolve(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.contains(['\\', '/'])
        || name.contains("..")
        || !name.ends_with(".jsonl")
    {
        return None;
    }
    let path = logs_dir().join(name);
    path.is_file().then_some(path)
}

/// Opens a folder in Explorer, or a file with its default app.
pub fn open(path: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if path.is_dir() {
        let _ = std::process::Command::new("explorer")
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    } else {
        let _ = std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler"])
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_short_and_bounds_long() {
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(60_000);
        assert!(clip(&long).len() < long.len());
    }

    #[test]
    fn resolve_rejects_traversal_and_foreign_extensions() {
        assert!(resolve("../secret.jsonl").is_none());
        assert!(resolve("sub\\x.jsonl").is_none());
        assert!(resolve("x.txt").is_none());
        assert!(resolve("does-not-exist.jsonl").is_none());
    }

    #[test]
    fn event_carries_relative_offsets() {
        let mut turn = TurnLog::begin("opencode", json!({ "query": "hi" }));
        turn.event("step_start", Some(1_000), json!({}));
        turn.event("reasoning", Some(1_250), json!({ "text": "thinking" }));
        turn.event("step_finish", Some(1_800), json!({ "reason": "stop" }));
        let offsets: Vec<i64> = turn
            .records
            .iter()
            .filter_map(|r| r.get("offsetMs").and_then(Value::as_i64))
            .collect();
        assert_eq!(offsets, vec![0, 250, 800]);
    }

    #[test]
    fn start_record_merges_meta() {
        let turn = TurnLog::begin("opencode", json!({ "model": "x", "first": true }));
        assert_eq!(turn.records[0]["event"], "start");
        assert_eq!(turn.records[0]["provider"], "opencode");
        assert_eq!(turn.records[0]["model"], "x");
        assert_eq!(turn.records[0]["first"], true);
    }
}
