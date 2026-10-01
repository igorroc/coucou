// Fast native answers for the queries the dashboard integrations already cover.
//
// A question like "quais as minhas próximas reuniões?" would otherwise pay the
// whole opencode round trip (bootstrap + model + Composio search). The same data
// is one direct MCP call away (calendar.rs / google_tasks.rs / news.rs), so the
// clear cases are answered here without the model. Anything ambiguous — anything
// that mutates, asks "how", or does not clearly request a listing — returns
// `None` and falls through to the normal chat path.

use std::sync::atomic::Ordering;

use crate::calendar;
use crate::google_tasks;
use crate::news;

/// A recognized read-only intent.
#[derive(Debug, PartialEq, Eq)]
pub enum Intent {
    Calendar,
    Tasks,
    News,
}

/// Lowercases and strips accents so keyword matching is stable across the ways
/// people type Portuguese.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            _ => c,
        })
        .collect()
}

/// Which native intent (if any) a query maps to. Pure, so it is testable without
/// touching the network.
pub fn detect(query: &str) -> Option<Intent> {
    let normalized = normalize(query);
    let tokens: Vec<&str> = normalized
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let has = |set: &[&str]| tokens.iter().any(|t| set.contains(t));

    // Anything that writes, sends or explains is left to the full agent — the
    // native path is read-only.
    const ACTIONS: &[&str] = &[
        "criar",
        "crie",
        "crio",
        "cria",
        "agendar",
        "agende",
        "agendo",
        "marcar",
        "marque",
        "marco",
        "deletar",
        "delete",
        "apagar",
        "apague",
        "remover",
        "remova",
        "adicionar",
        "adicione",
        "convite",
        "convide",
        "convidar",
        "enviar",
        "envie",
        "mande",
        "cancelar",
        "cancele",
        "editar",
        "edite",
        "resuma",
        "resumo",
        "resumir",
        "explique",
        "explicar",
        "traduza",
        "corrija",
        "responda",
    ];
    if has(ACTIONS) {
        return None;
    }

    const CALENDAR_NOUNS: &[&str] = &[
        "reuniao",
        "reunioes",
        "agenda",
        "evento",
        "eventos",
        "compromisso",
        "compromissos",
        "meeting",
        "meetings",
        "calendar",
        "calendario",
    ];
    const TASK_NOUNS: &[&str] = &["tarefa", "tarefas", "task", "tasks"];
    const NEWS_NOUNS: &[&str] = &["noticias", "manchete", "manchetes", "noticiario", "news"];
    // A listing question, not a statement about a single item.
    const LIST_MARKERS: &[&str] = &[
        "proxima",
        "proximas",
        "proximo",
        "proximos",
        "tenho",
        "hoje",
        "amanha",
        "semana",
        "quais",
        "qual",
        "quando",
        "mostrar",
        "mostre",
        "lista",
        "listar",
        "marcada",
        "marcadas",
        "agendada",
        "agendadas",
        "minha",
        "minhas",
        "meu",
        "meus",
        "terei",
        "ultima",
        "ultimas",
        "ultimo",
        "ultimos",
        "dia",
    ];

    if has(CALENDAR_NOUNS) && has(LIST_MARKERS) {
        return Some(Intent::Calendar);
    }
    if has(TASK_NOUNS) && has(LIST_MARKERS) {
        return Some(Intent::Tasks);
    }
    if has(NEWS_NOUNS) && has(LIST_MARKERS) {
        return Some(Intent::News);
    }
    None
}

/// The answer for a recognized intent, or `None` to use the full agent.
pub fn try_answer(query: &str) -> Option<String> {
    let paused = crate::integrations::PAUSED.load(Ordering::Relaxed);
    match detect(query)? {
        Intent::Calendar => calendar_answer(paused),
        Intent::Tasks => tasks_answer(paused),
        Intent::News => news_answer(paused),
    }
}

fn calendar_answer(paused: bool) -> Option<String> {
    let next = calendar::next(false, paused);
    // A fetch error is not an answer: let the agent try (and surface the error).
    if next.error.is_some() {
        return None;
    }
    if next.events.is_empty() {
        return Some("Você não tem eventos próximos agendados.".to_string());
    }
    let mut out = String::from("Próximos eventos:");
    for event in &next.events {
        out.push_str(&format!(
            "\n- {}  {}",
            when(&event.start, event.all_day),
            event.title
        ));
    }
    Some(out)
}

fn tasks_answer(paused: bool) -> Option<String> {
    let tasks = google_tasks::tasks(false, paused);
    if tasks.error.is_some() {
        return None;
    }
    if tasks.tasks.is_empty() {
        return Some("Você não tem tarefas pendentes.".to_string());
    }
    let mut out = String::from("Suas tarefas:");
    for task in &tasks.tasks {
        let mut line = format!("\n- {}", task.title);
        if !task.list.is_empty() {
            line.push_str(&format!(" ({})", task.list));
        }
        if !task.due.is_empty() {
            line.push_str(&format!(" — vence {}", date(&task.due)));
        }
        out.push_str(&line);
    }
    Some(out)
}

fn news_answer(paused: bool) -> Option<String> {
    let feed = news::feed(false, paused);
    if feed.error.is_some() || feed.items.is_empty() {
        return None;
    }
    let mut out = String::from("Notícias do dia:");
    for item in &feed.items {
        if item.source.is_empty() {
            out.push_str(&format!("\n- {}: {}", item.category, item.title));
        } else {
            out.push_str(&format!(
                "\n- {}: {} — {}",
                item.category, item.title, item.source
            ));
        }
    }
    Some(out)
}

/// `dd/mm HH:MM` for a timed event, `dd/mm (dia inteiro)` for an all-day one.
fn when(start: &str, all_day: bool) -> String {
    if all_day {
        return format!("{} (dia inteiro)", date(start));
    }
    match start.split_once('T') {
        Some((d, rest)) => {
            let hhmm: String = rest.chars().take(5).collect();
            format!("{} {}", date(d), hhmm)
        }
        None => start.to_string(),
    }
}

/// `yyyy-mm-dd` (or an RFC3339 prefix) → `dd/mm`.
fn date(value: &str) -> String {
    let d = value.split('T').next().unwrap_or(value);
    let parts: Vec<&str> = d.split('-').collect();
    if parts.len() >= 3 && parts[2].len() >= 2 {
        format!("{}/{}", &parts[2][..2], parts[1])
    } else {
        d.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_listing_questions() {
        assert_eq!(
            detect("quais as próximas reuniões que eu tenho?"),
            Some(Intent::Calendar)
        );
        assert_eq!(detect("minha agenda"), Some(Intent::Calendar));
        assert_eq!(detect("meus eventos de hoje"), Some(Intent::Calendar));
        assert_eq!(detect("quais são minhas tarefas?"), Some(Intent::Tasks));
        assert_eq!(detect("quais as últimas notícias?"), Some(Intent::News));
    }

    #[test]
    fn leaves_actions_and_ambiguous_queries_to_the_agent() {
        assert_eq!(detect("crie uma reunião amanhã"), None);
        assert_eq!(detect("resuma essa notícia"), None);
        assert_eq!(detect("como faço um bolo"), None);
        assert_eq!(detect("qual a capital da França"), None);
    }

    #[test]
    fn formats_dates_and_times() {
        assert_eq!(date("2026-10-02T10:00:00-03:00"), "02/10");
        assert_eq!(date("2026-10-05"), "05/10");
        assert_eq!(when("2026-10-02T10:00:00-03:00", false), "02/10 10:00");
        assert_eq!(when("2026-10-01", true), "01/10 (dia inteiro)");
    }
}
