use std::fs;
use std::path::Path;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::fs::atomic_write;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    Generic,
    // Profiles written before API types existed are CPA accounts.
    #[default]
    Cpa,
}

impl ApiType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Generic => "API key",
            Self::Cpa => "CPA API key",
        }
    }
}

/// Credentials for a Responses-compatible API, independent of ChatGPT OAuth.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiAccount {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub api_type: ApiType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
}

impl ApiAccount {
    pub fn new(
        name: String,
        base_url: String,
        api_key: String,
        api_type: ApiType,
        website: Option<String>,
    ) -> Result<Self> {
        let account = Self {
            name: name.trim().to_owned(),
            base_url: base_url.trim().trim_end_matches('/').to_owned(),
            api_key: api_key.trim().to_owned(),
            api_type,
            website: website
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty()),
        };
        account.validate()?;
        Ok(account)
    }

    fn validate(&self) -> Result<()> {
        if self.name.is_empty() || self.name.chars().any(char::is_control) {
            return Err(Error::Message(
                "API account name must be nonempty and contain no control characters.".into(),
            ));
        }
        let url = Url::parse(&self.base_url).map_err(|_| {
            Error::Message("API base URL must be an absolute HTTP or HTTPS URL.".into())
        })?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Message(
                "API base URL must use HTTP(S) with no credentials, query, or fragment.".into(),
            ));
        }
        if self.api_key.is_empty() || self.api_key.chars().any(char::is_control) {
            return Err(Error::Message(
                "API key must be nonempty and contain no control characters.".into(),
            ));
        }
        if let Some(website) = &self.website {
            let url = Url::parse(website).map_err(|_| {
                Error::Message("Website must be an absolute HTTP or HTTPS URL.".into())
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || website.chars().any(char::is_control)
            {
                return Err(Error::Message(
                    "Website must use HTTP(S) with no embedded credentials or control characters."
                        .into(),
                ));
            }
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| Error::io(path, error))?;
        let account: Self =
            serde_json::from_slice(&bytes).map_err(|error| Error::json(path, error))?;
        account.validate()?;
        Ok(account)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|_| Error::Message("Could not encode API account.".into()))?;
        atomic_write(path, &bytes, 0o600)
    }

    pub fn billing_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let base = if base.to_ascii_lowercase().ends_with("/v1") {
            &base[..base.len() - 3]
        } else {
            base
        };
        format!("{base}/v0/resource/plugins/cpa-key-billing/subscription")
    }
}
