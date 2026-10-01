// "Sugestões" do dashboard, geradas a partir do nome e da instrução master do
// assistente.
//
// Reusa o opencode CLI (o mesmo backend do chat, com o modelo e a autenticação
// que o usuário já configurou) para pedir 4 ações curtas em JSON. Roda em um
// diretório temporário para que a conversa não polua o histórico do opencode e
// não carregue nenhum AGENTS.md do repositório.
//
// O resultado é cacheado em `%LOCALAPPDATA%\Coucou\assistant_suggestions.json`
// com um TTL de uma hora; um refresh explícito (botão) ignora o TTL. Se o
// opencode não estiver disponível ou falhar, cai nas sugestões padrão — o card
// nunca fica vazio.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log;
use crate::opencode_chat;
use crate::settings::{self, SuggestedAction};

/// Cache lifetime. Um refresh explícito sempre ignora o TTL.
const TTL_SECS: f64 = 3600.0;
const CACHE_FILE: &str = "assistant_suggestions.json";
/// O mesmo teto do chat: um agente local com ferramentas pode demorar.
const RUN_TIMEOUT_SECS: u64 = 120;
const MAX_LABEL_CHARS: usize = 40;

/// Ícones aceitos pelo card. Qualquer outra coisa vira `sparkle`.
const ICONS: [&str; 4] = ["calendar", "clipboard", "list", "checkCircle"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestions {
    pub items: Vec<SuggestedAction>,
    /// Unix seconds da geração que produziu `items` (0 quando são as padrão).
    pub fetched_at: f64,
    /// True quando veio do cache em disco.
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    #[serde(default)]
    fetched_at: f64,
    /// Impressão do nome + instrução que gerou este cache: mudou, regenera.
    #[serde(default)]
    source: String,
    #[serde(default)]
    items: Vec<SuggestedAction>,
}

fn cache_path() -> PathBuf {
    settings::local_dir().join(CACHE_FILE)
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
    let dir = settings::local_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_vec_pretty(cache) {
        if let Err(err) = std::fs::write(cache_path(), json) {
            log::line(format!("assistant cache write failed: {err}"));
        }
    }
}

/// As sugestões padrão — usadas antes da primeira geração e sempre que o
/// opencode falha. Espelham `windows/src/mocks/home.ts`.
fn defaults() -> Vec<SuggestedAction> {
    let item = |icon: &str, label: &str, prompt: &str| SuggestedAction {
        icon: icon.to_string(),
        label: label.to_string(),
        prompt: Some(prompt.to_string()),
    };
    vec![
        item("clipboard", "Relatório do sprint", "Gere um relatório do sprint atual."),
        item("list", "Tarefas atribuídas", "Resuma as tarefas que estão atribuídas a mim."),
        item("calendar", "Preparar daily", "Prepare a daily: o que fiz, o que vou fazer e bloqueios."),
        item("checkCircle", "Tarefas abertas", "Liste minhas tarefas abertas e priorize."),
    ]
}

/// A impressão que decide se o cache ainda vale: nome + instrução.
fn source_key(name: &str, instruction: &str) -> String {
    format!("{name}\u{1f}{instruction}")
}

/// O nome já aparece no cabeçalho da dash, então não deve aparecer nos chips.
fn strip_name(label: &str, name: &str) -> String {
    let mut out = label.trim().to_string();
    if name.is_empty() {
        return out;
    }
    if let Some(rest) = out.strip_prefix(name) {
        out = rest
            .trim_start_matches(|c: char| c == ':' || c == '-' || c.is_whitespace())
            .trim()
            .to_string();
    }
    out
}

/// Primeira ocorrência de `[` até a sua `]` correspondente, respeitando strings.
fn json_array_slice(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Converte o rótulo cru (frase ou `{"label":…}` solto) em texto limpo.
fn extract_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if let Some(l) = v.get("label").and_then(Value::as_str) {
                return l.trim().to_string();
            }
        }
    }
    trimmed
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '-')
        .trim()
        .to_string()
}

/// Corta num número de caracteres (não bytes) para nunca partir um acento.
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Transforma o texto devolvido pelo modelo em sugestões válidas: no máximo 4,
/// ícone conhecido, rótulo curto e sem o nome do assistente (já está no header).
fn normalize(text: &str, name: &str) -> Vec<SuggestedAction> {
    let mut raw: Vec<(String, String)> = Vec::new();
    if let Some(slice) = json_array_slice(text) {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(slice) {
            for item in arr {
                match &item {
                    Value::Object(obj) => {
                        let label = obj.get("label").and_then(Value::as_str).unwrap_or("");
                        let prompt = obj.get("prompt").and_then(Value::as_str);
                        let icon = obj.get("icon").and_then(Value::as_str);
                        if !label.trim().is_empty() {
                            raw.push((label.to_string(), icon.unwrap_or("sparkle").to_string()));
                            let _ = prompt;
                        }
                    }
                    Value::String(s) if !s.trim().is_empty() => {
                        raw.push((s.clone(), "sparkle".to_string()));
                    }
                    _ => {}
                }
            }
        }
    }
    // Sem JSON: cada linha que não seja um marcador de cerca vira um chip.
    if raw.is_empty() {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("```") {
                continue;
            }
            raw.push((extract_label(line), "sparkle".to_string()));
        }
    }

    let mut items = Vec::new();
    for (label, icon) in raw {
        let label = clamp_chars(&strip_name(&extract_label(&label), name), MAX_LABEL_CHARS);
        if label.is_empty() {
            continue;
        }
        let icon = if ICONS.contains(&icon.as_str()) { icon } else { "sparkle".to_string() };
        items.push(SuggestedAction { icon, label, prompt: None });
        if items.len() == ICONS.len() {
            break;
        }
    }
    items
}

fn build_prompt(name: &str, instruction: &str) -> String {
    let context = if instruction.trim().is_empty() {
        "( nenhuma instrução específica )".to_string()
    } else {
        instruction.trim().to_string()
    };
    format!(
        "Você ajuda a configurar um assistente chamado \"{name}\" que vive na notch do macOS/Windows.\n\
         Contexto e objetivo de uso do usuário:\n{context}\n\n\
         Sugira exatamente 4 ações curtas que esse assistente deveria oferecer como atalhos na tela inicial.\n\
         Regras: cada rótulo deve ter no máximo 22 caracteres, em português, começando por verbo no infinitivo; \
         não inclua o nome do assistente; escolha um ícone por ação entre: calendar, clipboard, list, checkCircle.\n\
         Responda APENAS com um array JSON de objetos com as chaves \"label\", \"icon\" e \"prompt\" (o prompt é a \
         instrução completa enviada ao assistente quando a ação é clicada). Nada de texto fora do JSON.",
        name = name,
        context = context,
    )
}

/// Gera as sugestões chamando `opencode run` num diretório temporário.
fn generate(name: &str, instruction: &str, bin: &str, model: &str) -> Result<Vec<SuggestedAction>, String> {
    let exe = opencode_chat::resolve_bin(bin).ok_or_else(|| {
        "opencode não encontrado. Defina o caminho em Configurações → Chat.".to_string()
    })?;

    // Diretório temporário próprio: a conversa não aparece no histórico do
    // opencode e nenhum AGENTS.md do repositório contamina a resposta.
    let dir = std::env::temp_dir().join(format!("coucou-suggest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("diretório temporário: {e}"))?;

    let mut args: Vec<String> = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--title".into(),
        "Coucou suggestions".into(),
        "--dir".into(),
        dir.to_string_lossy().to_string(),
    ];
    let model = model.trim();
    if !model.is_empty() {
        args.push("--model".into());
        args.push(model.to_string());
    }
    args.push(build_prompt(name, instruction));

    let bin_str = exe.to_string_lossy().to_string();
    let (ok, stdout, stderr) = opencode_chat::run_for(RUN_TIMEOUT_SECS, &bin_str, &args)?;
    let text = opencode_chat::collect_text(&stdout);

    if text.trim().is_empty() {
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        log::line(format!("assistant generate empty: {}", tail.trim()));
        return Err(if ok { "sem resposta do opencode".into() } else { tail });
    }

    let items = normalize(&text, name);
    if items.is_empty() {
        return Err("resposta sem sugestões válidas".into());
    }
    Ok(items)
}

/// As sugestões mostradas no dashboard. Nunca falha: em qualquer erro devolve o
/// cache anterior (ou as sugestões padrão) com `error` preenchido.
pub fn suggestions(force: bool, paused: bool) -> Suggestions {
    let settings = settings::load();
    let name = settings::assistant_name(&settings);
    let instruction = settings.master_instruction.clone();
    let key = source_key(settings.assistant_name.trim(), instruction.trim());

    let cache = read_cache();
    let fresh = cache.fetched_at > 0.0 && now_secs() - cache.fetched_at < TTL_SECS;
    let cache_valid = cache.source == key;
    let fallback = if cache_valid && !cache.items.is_empty() {
        cache.items.clone()
    } else {
        defaults()
    };

    if !force && cache_valid && fresh {
        return Suggestions { items: cache.items, fetched_at: cache.fetched_at, cached: true, error: None };
    }
    // Pausar o Coucou é não fazer rede, incluindo a geração automática.
    if paused && !force {
        return Suggestions { items: fallback, fetched_at: cache.fetched_at, cached: true, error: None };
    }

    match generate(&name, &instruction, &settings.opencode_bin, &settings.opencode_model) {
        Ok(items) => {
            let fetched_at = now_secs();
            write_cache(&Cache { fetched_at, source: key, items: items.clone() });
            Suggestions { items, fetched_at, cached: false, error: None }
        }
        Err(err) => {
            log::line(format!("assistant generate failed: {err}"));
            Suggestions { items: fallback, fetched_at: cache.fetched_at, cached: true, error: Some(err) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_four_short_labels() {
        let items = defaults();
        assert_eq!(items.len(), 4);
        assert!(items.iter().all(|i| !i.label.is_empty() && i.prompt.is_some()));
        assert!(items.iter().all(|i| ICONS.contains(&i.icon.as_str())));
    }

    #[test]
    fn parses_a_json_array() {
        let text = r#"Aqui estão: [{"label":"Resumo do dia","icon":"calendar","prompt":"Resuma meu dia."},
            {"label":"Ver deploys","icon":"list","prompt":"Mostre os deploys."}]"#;
        let items = normalize(text, "Mochi");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "Resumo do dia");
        assert_eq!(items[0].icon, "calendar");
        assert!(items[0].prompt.is_none(), "o prompt já vive no array");
    }

    #[test]
    fn caps_at_four_and_unknown_icons_fall_back() {
        let text = r#"[{"label":"a","icon":"bogus"},{"label":"b"},{"label":"c"},{"label":"d"},{"label":"e"}]"#;
        let items = normalize(text, "Mochi");
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].icon, "sparkle");
    }

    #[test]
    fn strips_the_assistant_name_and_clamps_the_label() {
        let text = r#"[{"label":"Noma: resumir minhas tarefas de hoje inteirinhas","icon":"list"}]"#;
        let items = normalize(text, "Noma");
        assert_eq!(items[0].label.chars().count(), MAX_LABEL_CHARS);
        assert!(!items[0].label.starts_with("Noma"));
    }

    #[test]
    fn falls_back_to_lines_without_json() {
        let text = "- Lavar a louça\n- Pagar contas\n```\nignore\n```";
        let items = normalize(text, "Mochi");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "Lavar a louça");
    }

    #[test]
    fn empty_text_yields_nothing() {
        assert!(normalize("   \n  ", "Mochi").is_empty());
    }

    #[test]
    fn source_key_changes_with_name_or_instruction() {
        assert_ne!(source_key("a", "x"), source_key("b", "x"));
        assert_ne!(source_key("a", "x"), source_key("a", "y"));
        assert_eq!(source_key("a", "x"), source_key("a", "x"));
    }
}
