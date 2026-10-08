use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::account_store::{Account, Profile, Store};
use crate::api_account::ApiAccount;
use crate::auth::AuthDocument;
use crate::config::Config;
use crate::fs::{atomic_copy, atomic_write, remove_file_if_exists, sync_parent};
use crate::{Error, Result};

// Only these connection settings belong to cxa. Other configuration remains live.
const PROVIDER_KEYS: [&str; 6] = [
    "base_url",
    "wire_api",
    "requires_openai_auth",
    "env_key",
    "experimental_bearer_token",
    "auth",
];

#[derive(Deserialize, Serialize)]
struct ConnectionState {
    provider: String,
    oauth: String,
    api: Option<AppliedApi>,
}

#[derive(Deserialize, Serialize)]
struct AppliedApi {
    slot: u32,
    settings: String,
}

#[derive(Deserialize, Serialize)]
struct SwitchJournal {
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
    state: Option<Vec<u8>>,
    config_changed: bool,
    auth_changed: bool,
}

fn state_path(config: &Config) -> std::path::PathBuf {
    config.account_store.join("connection.json")
}

fn journal_path(config: &Config) -> std::path::PathBuf {
    config.account_store.join("switch-pending.json")
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

fn read_state(config: &Config) -> Result<Option<ConnectionState>> {
    let path = state_path(config);
    read_optional(&path)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|error| Error::json(&path, error)))
        .transpose()
}

fn parse_document(text: &str) -> Result<DocumentMut> {
    text.parse()
        .map_err(|_| Error::Message("Invalid Codex connection TOML; nothing was changed.".into()))
}

fn read_document(config: &Config) -> Result<DocumentMut> {
    let path = config.codex_home.join("config.toml");
    let bytes = read_optional(&path)?.unwrap_or_default();
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        Error::Message("Codex config.toml is not UTF-8; nothing was changed.".into())
    })?;
    parse_document(text)
}

fn provider_id(document: &DocumentMut) -> Result<String> {
    match document.get("model_provider") {
        None => Ok("openai".into()),
        Some(item) => item
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Message("model_provider must be a string.".into())),
    }
}

/// The user-level default is not the model selected in a running Desktop chat.
pub fn configured_model(config: &Config) -> Result<Option<String>> {
    read_document(config)?
        .get("model")
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::Message("Codex config.toml model must be a string.".into()))
        })
        .transpose()
}

fn capture(document: &DocumentMut, provider: &str) -> DocumentMut {
    let mut snapshot = DocumentMut::new();
    if provider == "openai" {
        if let Some(item) = document.get("openai_base_url") {
            snapshot["openai_base_url"] = item.clone();
        }
    } else if let Some(table) = document
        .get("model_providers")
        .and_then(|item| item.get(provider))
        .and_then(|item| item.as_table_like())
    {
        snapshot["model_providers"] = Item::Table(Table::new());
        snapshot["model_providers"][provider] = Item::Table(Table::new());
        for key in PROVIDER_KEYS {
            if let Some(item) = table.get(key) {
                snapshot["model_providers"][provider][key] = item.clone();
            }
        }
    }
    snapshot
}

fn restore(document: &mut DocumentMut, provider: &str, snapshot: &DocumentMut) -> Result<()> {
    if provider == "openai" {
        document.remove("openai_base_url");
        if let Some(item) = snapshot.get("openai_base_url") {
            document["openai_base_url"] = item.clone();
        }
    } else {
        let table = document
            .get_mut("model_providers")
            .and_then(|item| item.get_mut(provider))
            .and_then(|item| item.as_table_like_mut())
            .ok_or_else(|| {
                Error::Message(
                    "The existing model provider table is missing; nothing was changed.".into(),
                )
            })?;
        let source = snapshot
            .get("model_providers")
            .and_then(|item| item.get(provider));
        for key in PROVIDER_KEYS {
            table.remove(key);
            if let Some(item) = source.and_then(|item| item.get(key)) {
                table.insert(key, item.clone());
            }
        }
    }
    Ok(())
}

fn semantic_settings(text: &str) -> Result<Value> {
    toml_edit::de::from_str(text)
        .map_err(|_| Error::Message("Invalid saved Codex connection settings.".into()))
}

fn check_active(config: &Config, document: &DocumentMut, state: &ConnectionState) -> Result<()> {
    let Some(api) = &state.api else {
        return Ok(());
    };
    if provider_id(document)? != state.provider
        || semantic_settings(&capture(document, &state.provider).to_string())?
            != semantic_settings(&api.settings)?
    {
        return Err(Error::Message("Codex connection settings changed outside cxa. Restore the selected API connection before switching; nothing was changed.".into()));
    }
    if state.provider == "openai" {
        let account = ApiAccount::read(&config.profile_dir(api.slot).join("api.json"))?;
        let path = &config.session_auth;
        let bytes = fs::read(path).map_err(|error| Error::io(path, error))?;
        let auth: Value =
            serde_json::from_slice(&bytes).map_err(|error| Error::json(path, error))?;
        if auth.get("OPENAI_API_KEY").and_then(Value::as_str) != Some(&account.api_key) {
            return Err(Error::Message(
                "Codex API credentials changed outside cxa; nothing was changed.".into(),
            ));
        }
    }
    Ok(())
}

pub fn active_api_slot(config: &Config) -> Result<Option<u32>> {
    let Some(state) = read_state(config)? else {
        return Ok(None);
    };
    if state.api.is_none() {
        return Ok(None);
    }
    check_active(config, &read_document(config)?, &state)?;
    Ok(state.api.map(|api| api.slot))
}

/// Build an OAuth-only config for isolated login/quota processes, even in API mode.
pub fn copy_oauth_config(config: &Config, destination: &Path) -> Result<()> {
    let source = config.codex_home.join("config.toml");
    let Some(state) = read_state(config)?.filter(|state| state.api.is_some()) else {
        if source.is_file() {
            atomic_copy(&source, destination, 0o600)?;
        }
        return Ok(());
    };
    let mut document = read_document(config)?;
    check_active(config, &document, &state)?;
    restore(
        &mut document,
        &state.provider,
        &parse_document(&state.oauth)?,
    )?;
    atomic_write(destination, document.to_string().as_bytes(), 0o600)
}

pub fn select(config: &Config, profile: &Profile) -> Result<()> {
    let existing = read_state(config)?;
    if existing.as_ref().is_none_or(|state| state.api.is_none())
        && matches!(profile.account, Account::Chatgpt(_))
    {
        return atomic_copy(
            &config.profile_auth(profile.slot),
            &config.session_auth,
            0o600,
        );
    }
    let mut document = read_document(config)?;
    let provider = provider_id(&document)?;
    if let Some(state) = &existing {
        check_active(config, &document, state)?;
    }
    let mut state = existing
        .filter(|state| state.api.is_some())
        .unwrap_or_else(|| ConnectionState {
            provider: provider.clone(),
            oauth: capture(&document, &provider).to_string(),
            api: None,
        });
    let auth = match &profile.account {
        Account::Chatgpt(_) => {
            restore(
                &mut document,
                &state.provider,
                &parse_document(&state.oauth)?,
            )?;
            state.api = None;
            Some(
                fs::read(config.profile_auth(profile.slot))
                    .map_err(|error| Error::io(config.profile_auth(profile.slot), error))?,
            )
        }
        Account::Api(account) => {
            if provider == "openai" {
                if state.api.is_none() && config.session_auth.exists() {
                    let current = AuthDocument::read(&config.session_auth)?;
                    if Store::new(config.clone())
                        .slot_for_identity(&current.identity)?
                        .is_none()
                    {
                        return Err(Error::Message("Import the current ChatGPT login with `cxa init` before switching to an API account.".into()));
                    }
                }
                document["openai_base_url"] = value(&account.base_url);
            } else {
                if matches!(provider.as_str(), "ollama" | "lmstudio" | "amazon-bedrock") {
                    return Err(Error::Message("This built-in provider cannot use a CPA API key without changing model_provider.".into()));
                }
                let table = document
                    .get_mut("model_providers")
                    .and_then(|item| item.get_mut(&provider))
                    .and_then(|item| item.as_table_like_mut())
                    .ok_or_else(|| {
                        Error::Message(
                            "The configured custom model provider is missing; nothing was changed."
                                .into(),
                        )
                    })?;
                table.remove("env_key");
                table.remove("auth");
                table.insert("base_url", value(&account.base_url));
                table.insert("wire_api", value("responses"));
                table.insert("requires_openai_auth", value(false));
                table.insert("experimental_bearer_token", value(&account.api_key));
            }
            state.api = Some(AppliedApi {
                slot: profile.slot,
                settings: capture(&document, &provider).to_string(),
            });
            (provider == "openai").then(|| {
                serde_json::to_vec(&serde_json::json!({
                    "OPENAI_API_KEY": account.api_key,
                }))
                .expect("a string-only JSON object can be encoded")
            })
        }
    };
    let state_bytes = serde_json::to_vec(&state)
        .map_err(|_| Error::Message("Could not encode connection state.".into()))?;
    apply_switch(config, document.to_string().into_bytes(), auth, state_bytes)
}

fn restore_file(path: &Path, contents: &Option<Vec<u8>>) -> Result<()> {
    if let Some(bytes) = contents {
        atomic_write(path, bytes, 0o600)
    } else {
        remove_file_if_exists(path)?;
        sync_parent(path)
    }
}

fn rollback(config: &Config, journal: &SwitchJournal) -> Result<()> {
    if journal.auth_changed {
        restore_file(&config.session_auth, &journal.auth)?;
    }
    if journal.config_changed {
        restore_file(&config.codex_home.join("config.toml"), &journal.config)?;
    }
    restore_file(&state_path(config), &journal.state)?;
    remove_file_if_exists(&journal_path(config))?;
    sync_parent(&journal_path(config))
}

/// Recover an interrupted multi-file switch while holding the account-store lock.
pub fn recover(config: &Config) -> Result<()> {
    let path = journal_path(config);
    if let Some(bytes) = read_optional(&path)? {
        let journal = serde_json::from_slice(&bytes).map_err(|error| Error::json(&path, error))?;
        rollback(config, &journal)?;
    }
    Ok(())
}

fn apply_switch(
    config: &Config,
    settings: Vec<u8>,
    auth: Option<Vec<u8>>,
    state: Vec<u8>,
) -> Result<()> {
    let config_path = config.codex_home.join("config.toml");
    let journal = SwitchJournal {
        config: read_optional(&config_path)?,
        auth: read_optional(&config.session_auth)?,
        state: read_optional(&state_path(config))?,
        config_changed: true,
        auth_changed: auth.is_some(),
    };
    let bytes = serde_json::to_vec(&journal)
        .map_err(|_| Error::Message("Could not encode switch recovery journal.".into()))?;
    atomic_write(&journal_path(config), &bytes, 0o600)?;
    let result = (|| {
        atomic_write(&config_path, &settings, 0o600)?;
        if let Some(auth) = auth {
            atomic_write(&config.session_auth, &auth, 0o600)?;
        }
        atomic_write(&state_path(config), &state, 0o600)?;
        remove_file_if_exists(&journal_path(config))?;
        sync_parent(&journal_path(config))
    })();
    if let Err(error) = result {
        if let Err(recovery) = rollback(config, &journal) {
            return Err(Error::Message(format!(
                "Switch failed: {error}. Recovery also failed: {recovery}. Recovery journal retained; fix the I/O error and rerun cxa."
            )));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_snapshot_with_comments_round_trips() {
        let document = parse_document("model_provider = \"unicodex\"\n[model_providers.unicodex]\nname = \"OpenAI\"\nbase_url = \"https://chatgpt.com/backend-api/codex\" # Personal route\nwire_api = \"responses\"\nrequires_openai_auth = true\n").unwrap();
        let snapshot = capture(&document, "unicodex").to_string();
        assert!(parse_document(&snapshot).is_ok(), "snapshot: {snapshot:?}");
    }
}
