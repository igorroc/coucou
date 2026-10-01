// API keys live in the Windows Credential Manager, never on disk and never in
// the front end — the island can only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "fr.louisraille.naviassistant";
/// The Credential Manager service used before the rename.
const LEGACY_SERVICE: &str = "fr.louisraille.coucou";

/// Every key Navi Assistant may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
];

fn entry(key: &str) -> Option<Entry> {
    if !KNOWN_KEYS.contains(&key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

/// Copies every stored key from the pre-rename Credential Manager service and
/// drops the old entry, so API keys and URLs survive the rebrand. A key already
/// present under the new service is left untouched.
pub fn migrate_legacy() {
    for key in KNOWN_KEYS {
        if get(key).is_some() {
            continue;
        }
        let Ok(old) = Entry::new(LEGACY_SERVICE, key) else { continue };
        let Ok(value) = old.get_password() else { continue };
        if value.is_empty() {
            continue;
        }
        if let Some(new) = entry(key) {
            if new.set_password(&value).is_ok() {
                let _ = old.delete_credential();
            }
        }
    }
}
