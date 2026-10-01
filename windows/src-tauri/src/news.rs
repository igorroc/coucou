// "Notícias do dia" for the dashboard, via the Composio MCP.
//
// Composio's own `COMPOSIO_SEARCH_NEWS` provider is a Google-News-style search
// (country/language/recency), reached through `COMPOSIO_MULTI_EXECUTE_TOOL` with
// one query per enabled category. The top headline of each category becomes a
// slide in the dashboard carousel. Cached on disk for 45 minutes; a refresh
// button forces it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::log;
use crate::mcp;

const MCP_SERVER: &str = "composio";
const CACHE_FILE: &str = "news.json";
/// Cache lifetime. A refresh button always bypasses it.
const TTL_SECS: f64 = 2700.0;
const MAX_CATEGORIES: usize = 6;
const MAX_ITEMS: usize = 5;
const LOCALE_COUNTRY: &str = "br";
const LOCALE_LANG: &str = "pt";

/// One selectable category, offered in Settings → Assistente.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NewsCategory {
    pub id: &'static str,
    pub label: &'static str,
}

struct Cat {
    id: &'static str,
    label: &'static str,
    /// Free-text query sent to COMPOSIO_SEARCH_NEWS.
    query: &'static str,
}

/// The category catalog. The `id` is what settings stores.
const CATALOG: &[Cat] = &[
    Cat {
        id: "tecnologia",
        label: "Tecnologia",
        query: "tecnologia",
    },
    Cat {
        id: "ia",
        label: "Inteligência Artificial",
        query: "inteligência artificial",
    },
    Cat {
        id: "dev",
        label: "Desenvolvimento",
        query: "desenvolvimento de software",
    },
    Cat {
        id: "fintech",
        label: "Fintech & Pagamentos",
        query: "fintech pagamentos",
    },
    Cat {
        id: "economia",
        label: "Economia",
        query: "economia mercado",
    },
    Cat {
        id: "negocios",
        label: "Negócios & Startups",
        query: "startups negócios",
    },
    Cat {
        id: "mundo",
        label: "Mundo",
        query: "mundo",
    },
    Cat {
        id: "brasil",
        label: "Brasil",
        query: "brasil",
    },
    Cat {
        id: "ciencia",
        label: "Ciência",
        query: "ciência pesquisa",
    },
    Cat {
        id: "esportes",
        label: "Esportes",
        query: "esportes",
    },
];

/// The default set when the user has not picked any.
pub const DEFAULT_CATEGORIES: &[&str] = &["tecnologia", "ia", "economia", "mundo"];

/// The list offered to the settings UI.
pub fn categories() -> Vec<NewsCategory> {
    CATALOG
        .iter()
        .map(|c| NewsCategory {
            id: c.id,
            label: c.label,
        })
        .collect()
}

fn is_known(id: &str) -> bool {
    CATALOG.iter().any(|c| c.id == id)
}

/// One headline slide.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewsItem {
    pub category_id: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub source: String,
    pub url: String,
    pub published_at: String,
}

/// What the front end receives.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsFeed {
    pub items: Vec<NewsItem>,
    pub fetched_at: f64,
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    #[serde(default)]
    fetched_at: f64,
    /// Which categories the cache was built for — a change invalidates it.
    #[serde(default)]
    enabled: Vec<String>,
    #[serde(default)]
    items: Vec<NewsItem>,
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
            log::line(format!("news cache write failed: {err}"));
        }
    }
}

/// The categories to fetch: the enabled ones in catalog order (falling back to
/// the defaults), capped.
fn selected(enabled: &[String]) -> Vec<&'static Cat> {
    let mut cats: Vec<&'static Cat> = CATALOG
        .iter()
        .filter(|c| enabled.iter().any(|e| e == c.id))
        .collect();
    if cats.is_empty() {
        cats = CATALOG
            .iter()
            .filter(|c| DEFAULT_CATEGORIES.contains(&c.id))
            .collect();
    }
    cats.truncate(MAX_CATEGORIES);
    cats
}

fn to_item(cat: &Cat, value: &Value) -> Option<NewsItem> {
    let title = value
        .get("title")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    Some(NewsItem {
        category_id: cat.id.to_string(),
        category: cat.label.to_string(),
        title,
        summary: value
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        url: value
            .get("link")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        published_at: value
            .get("published_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
    })
}

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

/// Parses a `COMPOSIO_MULTI_EXECUTE_TOOL` payload into one slide per category.
fn parse_items(text: &str, cats: &[&Cat]) -> Result<Vec<NewsItem>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if value.get("successful").and_then(Value::as_bool) == Some(false) {
        let detail = value
            .get("error")
            .map(value_error)
            .unwrap_or_else(|| "erro do Composio".into());
        return Err(detail);
    }

    let mut items = Vec::new();
    if let Some(results) = value.pointer("/data/results").and_then(Value::as_array) {
        for (i, entry) in results.iter().enumerate() {
            let Some(cat) = cats.get(i) else { break };
            let response = entry.get("response");
            if response
                .and_then(|r| r.get("successful"))
                .and_then(Value::as_bool)
                == Some(false)
            {
                continue;
            }
            let news = response
                .and_then(|r| r.pointer("/data/news_results"))
                .or_else(|| response.and_then(|r| r.pointer("/data/results/news_results")))
                .and_then(Value::as_array);
            if let Some(first) = news.and_then(|n| n.first()) {
                if let Some(item) = to_item(*cat, first) {
                    items.push(item);
                }
            }
            if items.len() >= MAX_ITEMS {
                break;
            }
        }
    }

    if items.is_empty() {
        if let Some(err) = value.pointer("/data/error") {
            return Err(value_error(err));
        }
    }
    Ok(items)
}

fn fetch(enabled: &[String]) -> Result<(Vec<String>, Vec<NewsItem>), String> {
    let cats = selected(enabled);
    let used: Vec<String> = cats.iter().map(|c| c.id.to_string()).collect();

    let session = mcp::read_session(MCP_SERVER)?;
    let session_id = mcp::initialize(&session)?;

    let tools: Vec<Value> = cats
        .iter()
        .map(|c| {
            json!({
                "tool_slug": "COMPOSIO_SEARCH_NEWS",
                "arguments": {
                    "query": c.query,
                    "gl": LOCALE_COUNTRY,
                    "hl": LOCALE_LANG,
                    "when": "d"
                }
            })
        })
        .collect();
    let args = json!({
        "tools": tools,
        "sync_response_to_workbench": false,
        "thought": "fetch today's top headline for each enabled news category",
        "memory": {},
        "current_step": "FETCHING_NEWS"
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
    Ok((used, parse_items(&text, &cats)?))
}

/// The cached headlines, refreshed from Composio when the cache is stale (or
/// `force` is set, or the enabled categories changed). Never fails hard: on a
/// fetch error it returns the stale cache with `error` set.
pub fn feed(force: bool, paused: bool) -> NewsFeed {
    // Read the enabled categories straight from settings.json — the command runs
    // off the async runtime, so it must not hold a borrow into the app state.
    let enabled: Vec<String> = crate::settings::load()
        .news_categories
        .into_iter()
        .filter(|id| is_known(id))
        .collect();
    let cache = read_cache();
    let same_selection = cache.enabled == enabled;
    let fresh =
        cache.fetched_at > 0.0 && same_selection && now_secs() - cache.fetched_at < TTL_SECS;
    if !force && fresh {
        return NewsFeed {
            items: cache.items,
            fetched_at: cache.fetched_at,
            cached: true,
            error: None,
        };
    }
    if paused && !force {
        return NewsFeed {
            items: cache.items,
            fetched_at: cache.fetched_at,
            cached: true,
            error: None,
        };
    }

    match fetch(&enabled) {
        Ok((used, mut items)) => {
            items.truncate(MAX_ITEMS);
            let fetched_at = now_secs();
            write_cache(&Cache {
                fetched_at,
                enabled: used,
                items: items.clone(),
            });
            NewsFeed {
                items,
                fetched_at,
                cached: false,
                error: None,
            }
        }
        Err(err) => {
            log::line(format!("news fetch failed: {err}"));
            NewsFeed {
                items: cache.items,
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
    fn catalog_ids_are_unique_and_defaults_known() {
        let ids: Vec<&str> = CATALOG.iter().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids duplicados no catálogo");
        for d in DEFAULT_CATEGORIES {
            assert!(ids.contains(d), "default desconhecido: {d}");
        }
    }

    #[test]
    fn selected_falls_back_to_defaults_and_filters_unknown() {
        let none: Vec<String> = vec![];
        assert_eq!(selected(&none).len(), DEFAULT_CATEGORIES.len());
        let picked = vec!["mundo".to_string(), "nao-existe".to_string()];
        let cats = selected(&picked);
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].id, "mundo");
    }

    #[test]
    fn parses_one_slide_per_category() {
        let cats = selected(&[]);
        let text = r#"{"data":{"results":[
            {"response":{"successful":true,"data":{"news_results":[
                {"title":"Manchete IA","snippet":"resumo","source":"site.com","link":"https://x","published_at":"2026-09-30 18:12:56 UTC"}
            ]}}},
            {"response":{"successful":true,"data":{"news_results":[]}}}
        ]}}"#;
        let items = parse_items(text, &cats).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Manchete IA");
        assert_eq!(items[0].category_id, cats[0].id);
        assert_eq!(items[0].source, "site.com");
    }

    #[test]
    fn surfaces_composio_errors() {
        let cats = selected(&[]);
        let text = r#"{"successful":false,"error":{"message":"rate limited"}}"#;
        assert_eq!(parse_items(text, &cats).unwrap_err(), "rate limited");
    }
}
