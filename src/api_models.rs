use std::time::Duration;

use serde::Deserialize;

use crate::api_account::ApiAccount;
use crate::{Error, Result};

#[derive(Deserialize)]
struct ModelList {
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

/// Read the gateway's advertised IDs without inference or model substitution.
pub fn query(account: &ApiAccount) -> Result<Vec<String>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::io("API models runtime", error))?;
    runtime.block_on(fetch(account))
}

async fn fetch(account: &ApiAccount) -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| Error::Message("API models HTTP client failed.".into()))?;
    let response = client
        .get(format!("{}/models", account.base_url.trim_end_matches('/')))
        .bearer_auth(&account.api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            Error::Message(if error.is_timeout() {
                "API models request timed out.".into()
            } else {
                "API models request failed (connection or TLS error).".into()
            })
        })?;
    if !response.status().is_success() {
        // A gateway error body may echo credentials; never include it in output.
        return Err(Error::Message(format!(
            "API models request failed (HTTP {}).",
            response.status().as_u16()
        )));
    }
    let payload: ModelList = response
        .json()
        .await
        .map_err(|_| Error::Message("API models response has no valid model list.".into()))?;
    if payload.data.is_empty() {
        return Err(Error::Message("API gateway advertised no models.".into()));
    }
    let mut models = Vec::with_capacity(payload.data.len());
    for model in payload.data {
        if model.id.is_empty() || model.id.chars().any(char::is_control) {
            return Err(Error::Message(
                "API models response has an invalid model ID.".into(),
            ));
        }
        models.push(model.id);
    }
    models.sort();
    models.dedup();
    Ok(models)
}
