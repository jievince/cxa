use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use tempfile::Builder;

use crate::api_account::ApiAccount;
use crate::auth::{AuthDocument, Identity};
use crate::config::Config;
use crate::connection;
use crate::cpa::CpaUsage;
use crate::fs::{ExclusiveLock, atomic_copy, atomic_write, private_dir, sync_parent};
use crate::{Error, Result};

#[derive(Clone)]
pub struct Profile {
    pub slot: u32,
    pub account: Account,
}

#[derive(Clone)]
pub enum Account {
    Chatgpt(AuthDocument),
    Api(ApiAccount),
}

impl Profile {
    pub fn label(&self) -> &str {
        match &self.account {
            Account::Chatgpt(auth) => auth.identity.label(),
            Account::Api(account) => &account.name,
        }
    }

    pub fn oauth(&self) -> Result<&AuthDocument> {
        match &self.account {
            Account::Chatgpt(auth) => Ok(auth),
            Account::Api(_) => Err(Error::Message(
                "API-key accounts do not use ChatGPT OAuth login.".into(),
            )),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageWindow {
    pub used_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<i64>,
}

impl UsageWindow {
    pub fn remaining_percent(&self) -> Option<f64> {
        self.used_percent
            .map(|used| (100.0 - used).clamp(0.0, 100.0))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageBucket {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub primary_window: Option<UsageWindow>,
    pub secondary_window: Option<UsageWindow>,
    pub individual_window: Option<UsageWindow>,
    #[serde(default)]
    pub reached: bool,
    pub reached_type: Option<String>,
    #[serde(default)]
    pub spend_control_reached: bool,
    pub plan_type: Option<String>,
}

impl UsageBucket {
    pub fn windows(&self) -> impl Iterator<Item = (&'static str, &UsageWindow)> {
        [
            ("primary", self.primary_window.as_ref()),
            ("secondary", self.secondary_window.as_ref()),
            ("individual", self.individual_window.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, window)| window.map(|window| (name, window)))
    }

    pub fn exhausted_now(&self, now: i64) -> bool {
        if self.spend_control_reached {
            return self
                .individual_window
                .as_ref()
                .and_then(|window| window.resets_at)
                .is_none_or(|reset| reset > now);
        }
        let windows: Vec<&UsageWindow> = self.windows().map(|(_, window)| window).collect();
        let exhausted: Vec<&UsageWindow> = windows
            .iter()
            .copied()
            .filter(|window| window.used_percent.unwrap_or_default() >= 100.0)
            .collect();
        if !exhausted.is_empty() {
            return exhausted.iter().any(|window| window.resets_at.is_none())
                || exhausted
                    .iter()
                    .filter_map(|window| window.resets_at)
                    .max()
                    .is_some_and(|reset| reset > now);
        }
        if !self.reached {
            return false;
        }
        if matches!(
            self.reached_type.as_deref(),
            Some("workspace_owner_credits_depleted" | "workspace_member_credits_depleted")
        ) {
            return true;
        }
        windows.is_empty()
            || windows.iter().any(|window| window.resets_at.is_none())
            || windows
                .iter()
                .filter_map(|window| window.resets_at)
                .max()
                .is_some_and(|reset| reset > now)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageRecord {
    pub observed_at: i64,
    #[serde(default)]
    pub last_attempted_at: i64,
    #[serde(default)]
    pub buckets: Vec<UsageBucket>,
    #[serde(default)]
    pub cpa: Option<CpaUsage>,
    #[serde(default)]
    pub refresh_error: Option<String>,
    pub error: Option<String>,
}

impl UsageRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| Error::io(path, error))?;
        serde_json::from_slice(&bytes).map_err(|error| Error::json(path, error))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| Error::Message(format!("could not encode usage: {error}")))?;
        atomic_write(path, &bytes, 0o600)
    }

    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    fn refreshed_recently(&self, now: i64, ttl_seconds: u64) -> bool {
        let refreshed_at = self.observed_at.max(self.last_attempted_at);
        now.saturating_sub(refreshed_at) < ttl_seconds as i64
    }

    pub fn label(&self, now: i64) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        let mut bits = Vec::new();
        if let Some(cpa) = &self.cpa {
            if cpa.unlimited {
                bits.push("CPA allocation unlimited".into());
            }
            for dimension in &cpa.dimensions {
                bits.push(format!(
                    "CPA {} {} {}",
                    dimension.window_label(),
                    dimension.metric,
                    dimension.amount_label()
                ));
                if let Some(when) = dimension.reset_at_label() {
                    bits.push(format!("resets {when}"));
                }
            }
        }
        let show_bucket = self.buckets.len() > 1;
        for bucket in &self.buckets {
            let bucket_label = bucket.limit_name.as_deref().unwrap_or(&bucket.limit_id);
            for (window_label, window) in bucket.windows() {
                let label = if show_bucket {
                    format!("{bucket_label} {window_label}")
                } else {
                    window_label.to_owned()
                };
                if let Some(remaining) = window.remaining_percent() {
                    bits.push(format!("{label} {}% left", format_percent(remaining)));
                }
                if let Some(reset) = window.resets_at {
                    let when = DateTime::from_timestamp(reset, 0)
                        .map(|value| {
                            value
                                .with_timezone(&Local)
                                .format("%b %d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_else(|| reset.to_string());
                    let suffix = if reset > now {
                        format!(" (in {}h)", (reset - now) / 3600)
                    } else {
                        " (passed)".to_owned()
                    };
                    bits.push(format!("{label} resets {when}{suffix}"));
                }
            }
            if bucket.exhausted_now(now) {
                bits.push(if show_bucket {
                    format!("{bucket_label} EXHAUSTED")
                } else {
                    "EXHAUSTED".into()
                });
            }
            if let Some(plan) = &bucket.plan_type {
                if !bits.iter().any(|bit| bit == plan) {
                    bits.push(plan.clone());
                }
            }
        }
        let age = now.saturating_sub(self.observed_at);
        bits.push(if age < 3600 {
            "seen just now".into()
        } else {
            format!("seen {}h ago", age / 3600)
        });
        bits.join(", ")
    }

    pub fn exhausted_now(&self, now: i64) -> bool {
        self.buckets.iter().any(|bucket| bucket.exhausted_now(now))
            || self.cpa.as_ref().is_some_and(|cpa| {
                !cpa.unlimited && cpa.dimensions.iter().any(|dimension| dimension.exhausted())
            })
    }

    pub fn max_current_used_percent(&self, now: i64) -> Option<f64> {
        self.buckets
            .iter()
            .flat_map(UsageBucket::windows)
            .map(|(_, window)| window)
            .filter(|window| window.resets_at.is_none_or(|reset| reset > now))
            .filter_map(|window| window.used_percent)
            .reduce(f64::max)
    }
}

fn format_percent(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

pub struct Store {
    pub config: Config,
}

impl Store {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn lock(&self) -> Result<ExclusiveLock> {
        ExclusiveLock::acquire(&self.config.switch_lock)
    }

    pub fn try_lock(&self) -> Result<Option<ExclusiveLock>> {
        ExclusiveLock::try_acquire(&self.config.switch_lock)
    }

    pub fn sync_session_profile(&self) -> Result<()> {
        let Ok(session) = AuthDocument::read(&self.config.session_auth) else {
            return Ok(());
        };
        let Some(slot) = self.slot_for_identity(&session.identity)? else {
            return Ok(());
        };
        session.copy_to_same_account(&self.config.profile_auth(slot))?;
        Ok(())
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        let entries = match fs::read_dir(&self.config.account_store) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::io(&self.config.account_store, error)),
        };
        let mut profiles = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(&self.config.account_store, error))?;
            let Some(slot) = profile_slot(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let api_path = entry.path().join("api.json");
            let account = if api_path.exists() {
                Account::Api(ApiAccount::read(&api_path)?)
            } else {
                Account::Chatgpt(AuthDocument::read(entry.path().join("auth.json"))?)
            };
            profiles.push(Profile { slot, account });
        }
        profiles.sort_by_key(|profile| profile.slot);
        Ok(profiles)
    }

    pub fn selected(&self) -> Result<Option<u32>> {
        if let Some(slot) = connection::active_api_slot(&self.config)? {
            return Ok(Some(slot));
        }
        let Ok(auth) = AuthDocument::read(&self.config.session_auth) else {
            return Ok(None);
        };
        self.slot_for_identity(&auth.identity)
    }

    pub fn resolve(&self, selector: &str) -> Result<Profile> {
        let profiles = self.profiles()?;
        if let Ok(slot) = selector.parse::<u32>() {
            return profiles
                .into_iter()
                .find(|profile| profile.slot == slot)
                .ok_or_else(|| Error::Message(format!("No account {slot} is enrolled.")));
        }
        let selector = selector.to_ascii_lowercase();
        let matches: Vec<Profile> = profiles
            .into_iter()
            .filter(|profile| profile.label().to_ascii_lowercase().contains(&selector))
            .collect();
        match matches.as_slice() {
            [profile] => Ok(profile.clone()),
            [] => Err(Error::Message(format!("No account matches `{selector}`."))),
            _ => Err(Error::Message(format!(
                "More than one account matches `{selector}`; use its slot number."
            ))),
        }
    }

    pub fn slot_for_identity(&self, identity: &Identity) -> Result<Option<u32>> {
        Ok(self
            .profiles()?
            .into_iter()
            .find(|profile| matches!(&profile.account, Account::Chatgpt(auth) if auth.identity.same_account(identity)))
            .map(|profile| profile.slot))
    }

    pub fn next_slot(&self) -> Result<u32> {
        let entries = match fs::read_dir(&self.config.account_store) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
            Err(error) => return Err(Error::io(&self.config.account_store, error)),
        };
        let mut highest = 0;
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(&self.config.account_store, error))?;
            if let Some(slot) = profile_slot(&entry.file_name().to_string_lossy()) {
                highest = highest.max(slot);
            }
        }
        Ok(highest + 1)
    }

    pub fn enroll(&self, source: &Path) -> Result<Profile> {
        let auth = AuthDocument::read(source)?;
        if let Some(slot) = self.slot_for_identity(&auth.identity)? {
            return Err(Error::Message(format!(
                "{} is already enrolled as account {slot}; nothing was changed.",
                auth.identity.label()
            )));
        }
        let slot = self.next_slot()?;
        private_dir(&self.config.account_store)?;
        let staging = Builder::new()
            .prefix(".profile-")
            .tempdir_in(&self.config.account_store)
            .map_err(|error| Error::io(&self.config.account_store, error))?;
        auth.write_to(&staging.path().join("auth.json"))?;
        let profile_dir = self.config.profile_dir(slot);
        fs::rename(staging.path(), &profile_dir).map_err(|error| Error::io(&profile_dir, error))?;
        sync_parent(&profile_dir)?;
        Ok(Profile {
            slot,
            account: Account::Chatgpt(AuthDocument::read(self.config.profile_auth(slot))?),
        })
    }

    pub fn enroll_api(&self, account: ApiAccount) -> Result<Profile> {
        if self
            .profiles()?
            .iter()
            .any(|profile| profile.label().eq_ignore_ascii_case(&account.name))
        {
            return Err(Error::Message(
                "An account with this name already exists; nothing was changed.".into(),
            ));
        }
        let slot = self.next_slot()?;
        private_dir(&self.config.account_store)?;
        let staging = Builder::new()
            .prefix(".profile-")
            .tempdir_in(&self.config.account_store)
            .map_err(|error| Error::io(&self.config.account_store, error))?;
        account.write(&staging.path().join("api.json"))?;
        let profile_dir = self.config.profile_dir(slot);
        fs::rename(staging.path(), &profile_dir).map_err(|error| Error::io(&profile_dir, error))?;
        sync_parent(&profile_dir)?;
        Ok(Profile {
            slot,
            account: Account::Api(account),
        })
    }

    pub fn replace(&self, slot: u32, source: &Path) -> Result<Profile> {
        let existing = self.resolve(&slot.to_string())?;
        let replacement = AuthDocument::read(source)?;
        if !replacement
            .identity
            .same_account(&existing.oauth()?.identity)
        {
            return Err(Error::Message(format!(
                "Signed in to a different account than account {slot} ({}). Nothing was changed.",
                existing.label()
            )));
        }
        atomic_copy(source, &self.config.profile_auth(slot), 0o600)?;
        Ok(Profile {
            slot,
            account: Account::Chatgpt(AuthDocument::read(self.config.profile_auth(slot))?),
        })
    }

    pub fn select(&self, slot: u32) -> Result<Profile> {
        let profile = self.resolve(&slot.to_string())?;
        connection::select(&self.config, &profile)?;
        Ok(profile)
    }

    pub fn usage(&self, slot: u32) -> Option<UsageRecord> {
        UsageRecord::read(&self.config.profile_usage(slot)).ok()
    }

    pub fn usage_fresh(&self, slot: u32) -> bool {
        self.usage(slot).is_some_and(|usage| {
            usage.refreshed_recently(now_epoch(), self.config.usage_ttl_seconds)
        })
    }

    pub fn status_lines(&self) -> Result<Vec<String>> {
        let selected = self
            .selected()?
            .ok_or_else(|| Error::Message("No Codex account is selected.".into()))?;
        let profile = self.resolve(&selected.to_string())?;
        let mut lines = vec![format!(
            "Selected Codex account: {selected}  {}",
            profile.label()
        )];
        lines.push(format!(
            "Quota: {}",
            self.usage(selected)
                .map(|usage| usage.label(now_epoch()))
                .unwrap_or_else(|| "usage unknown".into())
        ));
        lines.push(format!(
            "Credential file: {}",
            self.credential_status(selected)?
        ));
        Ok(lines)
    }

    pub fn credential_status(&self, selected: u32) -> Result<String> {
        let profile = self.resolve(&selected.to_string())?;
        if let Account::Api(_) = &profile.account {
            return Ok(
                if connection::active_api_slot(&self.config)? == Some(selected) {
                    "matches the selected API connection".into()
                } else {
                    "does not match the selected API connection".into()
                },
            );
        }
        let status = match AuthDocument::read(&self.config.session_auth) {
            Ok(session) if session.identity.same_account(&profile.oauth()?.identity) => {
                "matches the selected account".into()
            }
            Ok(session) => {
                let label = self
                    .slot_for_identity(&session.identity)?
                    .map(|slot| format!("account {slot}"))
                    .unwrap_or_else(|| session.identity.label().to_owned());
                format!("{label}; run `cxa {selected}` to replace it")
            }
            Err(_) => format!(
                "missing or invalid at {}",
                self.config.session_auth.display()
            ),
        };
        Ok(status)
    }
}

fn profile_slot(name: &str) -> Option<u32> {
    name.strip_prefix("profile-")?.parse().ok()
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_label_reports_current_usage() {
        let usage = UsageRecord {
            observed_at: 1_000,
            buckets: vec![UsageBucket {
                limit_id: "codex".into(),
                primary_window: Some(UsageWindow {
                    used_percent: Some(25.0),
                    resets_at: Some(10_000),
                    window_minutes: Some(300),
                }),
                ..UsageBucket::default()
            }],
            ..UsageRecord::default()
        };

        let label = usage.label(1_000);

        assert!(label.contains("primary 75% left"));
        assert!(label.contains("seen just now"));
    }

    #[test]
    fn quota_exhaustion_ignores_expired_windows() {
        let active = UsageRecord {
            buckets: vec![UsageBucket {
                limit_id: "codex".into(),
                primary_window: Some(UsageWindow {
                    used_percent: Some(50.0),
                    resets_at: Some(2_000),
                    window_minutes: None,
                }),
                reached: true,
                ..UsageBucket::default()
            }],
            ..UsageRecord::default()
        };
        let expired = UsageRecord {
            buckets: vec![UsageBucket {
                primary_window: Some(UsageWindow {
                    used_percent: Some(50.0),
                    resets_at: Some(500),
                    ..UsageWindow::default()
                }),
                ..active.buckets[0].clone()
            }],
            ..active.clone()
        };

        assert!(active.exhausted_now(1_000));
        assert!(!expired.exhausted_now(1_000));
    }

    #[test]
    fn recent_failed_attempt_throttles_refresh_of_stale_usage() {
        let usage = UsageRecord {
            observed_at: 1_000,
            last_attempted_at: 2_000,
            ..UsageRecord::default()
        };

        assert!(usage.refreshed_recently(2_050, 120));
        assert!(!usage.refreshed_recently(2_120, 120));
    }

    #[test]
    fn full_active_window_is_exhausted_without_reached_metadata() {
        let active = UsageBucket {
            primary_window: Some(UsageWindow {
                used_percent: Some(100.0),
                resets_at: Some(2_000),
                ..UsageWindow::default()
            }),
            ..UsageBucket::default()
        };
        let expired = UsageBucket {
            primary_window: Some(UsageWindow {
                resets_at: Some(500),
                ..active.primary_window.clone().unwrap()
            }),
            ..active.clone()
        };

        assert!(active.exhausted_now(1_000));
        assert!(!expired.exhausted_now(1_000));
    }
}
