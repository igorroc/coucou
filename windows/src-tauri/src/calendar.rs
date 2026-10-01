// Google Calendar's next appointment, via the Composio MCP.
//
// Composio (https://connect.composio.dev/mcp) is authenticated in opencode and
// fronts `GOOGLECALENDAR_EVENTS_LIST`. We call it through
// `COMPOSIO_MULTI_EXECUTE_TOOL`; the event list comes back as JSON inside the
// tool's text payload. The next event is cached for 15 minutes; a refresh
// button forces it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::log;
use crate::mcp;

const MCP_SERVER: &str = "composio";
const CACHE_FILE: &str = "calendar_next.json";
/// Cache lifetime. A refresh button always bypasses it.
const TTL_SECS: f64 = 900.0;
/// How far ahead the next appointment is looked for.
const HORIZON_DAYS: i64 = 7;

/// One calendar entry, trimmed to what the card shows.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub title: String,
    /// RFC3339 (timed) or `yyyy-mm-dd` (all day).
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: String,
    pub url: String,
    pub provider: String,
}

/// What the front end receives.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarNext {
    pub events: Vec<CalendarEvent>,
    pub fetched_at: f64,
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    #[serde(default)]
    fetched_at: f64,
    /// The upcoming events, soonest first.
    #[serde(default)]
    events: Vec<CalendarEvent>,
    /// Older caches stored a single event. Read for migration; never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<CalendarEvent>,
}

impl Cache {
    /// The events to use, tolerant of the pre-list cache format.
    fn items(&self) -> Vec<CalendarEvent> {
        if !self.events.is_empty() {
            self.events.clone()
        } else {
            self.event.clone().into_iter().collect()
        }
    }
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
            log::line(format!("calendar cache write failed: {err}"));
        }
    }
}

// ── Date helpers (no chrono dependency) ───────────────────────────────────────

/// `days` since the Unix epoch → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix seconds → RFC3339 in UTC (`2026-10-01T13:00:00Z`).
fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
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

fn to_event(item: &Value) -> Option<CalendarEvent> {
    if item.get("status").and_then(Value::as_str) == Some("cancelled") {
        return None;
    }
    let all_day = item.pointer("/start/date").is_some();
    let start = item
        .pointer("/start/dateTime")
        .and_then(Value::as_str)
        .or_else(|| item.pointer("/start/date").and_then(Value::as_str))?
        .to_string();
    let end = item
        .pointer("/end/dateTime")
        .and_then(Value::as_str)
        .or_else(|| item.pointer("/end/date").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    // Only a real join link counts: `htmlLink` is the event's calendar page and
    // would otherwise make every event look like an online meeting.
    let url = item
        .get("hangoutLink")
        .and_then(Value::as_str)
        .or_else(|| item.get("display_url").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let provider = if url.contains("meet.google.com") {
        "Google Meet"
    } else if !url.is_empty() {
        "Online"
    } else {
        "Google Calendar"
    };
    Some(CalendarEvent {
        title: item.get("summary").and_then(Value::as_str).unwrap_or("(sem título)").to_string(),
        start,
        end,
        all_day,
        location: item.get("location").and_then(Value::as_str).unwrap_or("").to_string(),
        url,
        provider: provider.to_string(),
    })
}

/// Pulls the event list out of a `COMPOSIO_MULTI_EXECUTE_TOOL` payload.
fn parse_events(text: &str) -> Result<Vec<CalendarEvent>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    if value.get("successful").and_then(Value::as_bool) == Some(false) {
        let detail = value.get("error").map(value_error).unwrap_or_else(|| "erro do Composio".into());
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

    let items = value
        .pointer("/data/results/0/response/data/items")
        .or_else(|| value.pointer("/data/items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(items.iter().filter_map(to_event).collect())
}

/// The events shown on the card, soonest first. Every event is kept — across
/// days, all-day entries included. `start` is `yyyy-mm-dd` (all-day) or RFC3339
/// (timed); both compare correctly as strings, and the shorter all-day form
/// sorts before any time on the same day, which is what we want. Ties keep the
/// API's own order.
fn pick_upcoming(events: Vec<CalendarEvent>) -> Vec<CalendarEvent> {
    let mut all: Vec<(usize, CalendarEvent)> = events.into_iter().enumerate().collect();
    all.sort_by(|a, b| a.1.start.cmp(&b.1.start).then(a.0.cmp(&b.0)));
    all.into_iter().map(|(_, e)| e).collect()
}

// ── Public API ────────────────────────────────────────────────────────────────

fn fetch() -> Result<Vec<CalendarEvent>, String> {
    let session = mcp::read_session(MCP_SERVER)?;
    let session_id = mcp::initialize(&session)?;
    let now = now_secs() as i64;
    let args = json!({
        "tools": [{
            "tool_slug": "GOOGLECALENDAR_EVENTS_LIST",
            "arguments": {
                "calendarId": "primary",
                "timeMin": iso_utc(now),
                "timeMax": iso_utc(now + HORIZON_DAYS * 86_400),
                "singleEvents": true,
                "orderBy": "startTime",
                "maxResults": 15,
                "fields": "items(id,summary,start,end,location,hangoutLink,status),nextPageToken"
            }
        }],
        "sync_response_to_workbench": false,
        "thought": "fetch the upcoming calendar events for the Navi Assistant dashboard",
        "memory": {},
        "current_step": "FETCHING_EVENTS"
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
    Ok(pick_upcoming(parse_events(&text)?))
}

/// The cached upcoming events, refreshed from Composio when the cache is stale
/// (or `force` is set). Never fails hard: on a fetch error it returns the stale
/// cache with `error` set.
pub fn next(force: bool, paused: bool) -> CalendarNext {
    let cache = read_cache();
    let fresh = cache.fetched_at > 0.0 && now_secs() - cache.fetched_at < TTL_SECS;
    if !force && fresh {
        return CalendarNext { events: cache.items(), fetched_at: cache.fetched_at, cached: true, error: None };
    }
    if paused && !force {
        return CalendarNext { events: cache.items(), fetched_at: cache.fetched_at, cached: true, error: None };
    }

    match fetch() {
        Ok(events) => {
            let fetched_at = now_secs();
            write_cache(&Cache {
                fetched_at,
                events: events.clone(),
                event: None,
            });
            CalendarNext { events, fetched_at, cached: false, error: None }
        }
        Err(err) => {
            log::line(format!("calendar fetch failed: {err}"));
            CalendarNext { events: cache.items(), fetched_at: cache.fetched_at, cached: true, error: Some(err) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_matches_known_epochs() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_735_689_600), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn parses_timed_and_all_day_events() {
        let text = r#"{
            "data": { "results": [ { "response": { "successful": true, "data": { "items": [
                {"status":"confirmed","summary":"Escritório","start":{"date":"2026-10-01"},"end":{"date":"2026-10-02"},"htmlLink":"https://www.google.com/calendar/event?eid=abc"},
                {"status":"confirmed","summary":"Daily","start":{"dateTime":"2026-10-01T10:00:00-03:00","timeZone":"America/Sao_Paulo"},"end":{"dateTime":"2026-10-01T10:15:00-03:00"},"hangoutLink":"https://meet.google.com/wbj-khzp-dyb"},
                {"status":"cancelled","summary":"Cancelado","start":{"dateTime":"2026-10-01T11:00:00-03:00"}}
            ] } } } ] }
        }"#;
        let events = parse_events(text).unwrap();
        assert_eq!(events.len(), 2, "cancelled dropped");
        let upcoming = pick_upcoming(events);
        assert_eq!(upcoming.len(), 2);
        // All-day is kept, not discarded, and shown first (it has no time).
        assert!(upcoming.iter().any(|e| e.all_day));
        let daily = upcoming.iter().find(|e| e.title == "Daily").unwrap();
        assert_eq!(daily.provider, "Google Meet");
        assert_eq!(daily.url, "https://meet.google.com/wbj-khzp-dyb");
        // An in-person/regular event keeps its `htmlLink` out of `url`, so the UI
        // shows neither the meeting icon nor the "Entrar" button.
        let office = upcoming.iter().find(|e| e.title == "Escritório").unwrap();
        assert_eq!(office.url, "");
        assert_eq!(office.provider, "Google Calendar");
    }

    #[test]
    fn keeps_events_across_days_sorted_by_start() {
        let text = r#"{
            "data": { "results": [ { "response": { "successful": true, "data": { "items": [
                {"status":"confirmed","summary":"Amanhã cedo","start":{"dateTime":"2026-10-02T09:00:00-03:00"}},
                {"status":"confirmed","summary":"Hoje tarde","start":{"dateTime":"2026-10-01T18:00:00-03:00"}},
                {"status":"confirmed","summary":"Hoje cedo","start":{"dateTime":"2026-10-01T08:00:00-03:00"}},
                {"status":"confirmed","summary":"Dia inteiro","start":{"date":"2026-10-01"},"end":{"date":"2026-10-02"}}
            ] } } } ] }
        }"#;
        let upcoming = pick_upcoming(parse_events(text).unwrap());
        let titles: Vec<_> = upcoming.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, vec!["Dia inteiro", "Hoje cedo", "Hoje tarde", "Amanhã cedo"]);
    }

    #[test]
    fn cache_reads_both_list_and_legacy_single_event() {
        let legacy: Cache = serde_json::from_str(
            r#"{"fetchedAt":10,"event":{"title":"Só um","start":"2026-10-01T08:00:00-03:00","end":"","allDay":false,"location":"","url":"","provider":"Google Calendar"}}"#,
        )
        .unwrap();
        assert_eq!(legacy.items().len(), 1);
        assert_eq!(legacy.items()[0].title, "Só um");

        let list: Cache = serde_json::from_str(
            r#"{"fetchedAt":10,"events":[{"title":"Um","start":"x","end":"","allDay":false,"location":"","url":"","provider":""},{"title":"Dois","start":"y","end":"","allDay":false,"location":"","url":"","provider":""}]}"#,
        )
        .unwrap();
        assert_eq!(list.items().len(), 2);
    }

    #[test]
    fn surfaces_composio_errors() {
        let text = r#"{"data":{"results":[{"response":{"successful":false,"error":{"message":"No connected account"}}}]}}"#;
        assert_eq!(parse_events(text).unwrap_err(), "No connected account");
    }
}
