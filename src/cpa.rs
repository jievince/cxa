use std::time::Duration;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::account_store::{UsageRecord, now_epoch};
use crate::api_account::ApiAccount;
use crate::app_server::CancellationToken;
use crate::{Error, Result};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CpaUsage {
    pub unlimited: bool,
    pub dimensions: Vec<CpaDimension>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CpaDimension {
    pub window: usize,
    pub window_name: Option<String>,
    pub period_seconds: Option<u64>,
    pub resets_at: Option<i64>,
    pub metric: String,
    pub limit: f64,
    pub remaining: Option<f64>,
    pub remaining_percent: Option<f64>,
}

impl CpaDimension {
    pub fn name_label(&self) -> String {
        self.window_name
            .clone()
            .unwrap_or_else(|| format!("Window {}", self.window))
    }

    pub fn period_label(&self) -> Option<String> {
        Some(match self.period_seconds? {
            604_800 => "Weekly".into(),
            86_400 => "Daily".into(),
            value if value > 0 && value % 86_400 == 0 => format!("{}-day", value / 86_400),
            value if value > 0 && value % 3_600 == 0 => format!("{}-hour", value / 3_600),
            value if value > 0 && value % 60 == 0 => format!("{}-minute", value / 60),
            value => format!("{value}-second"),
        })
    }

    pub fn window_label(&self) -> String {
        let name = self.name_label();
        match self.period_label() {
            Some(period) => format!("{name} · {period}"),
            None => name,
        }
    }

    pub fn metric_label(&self) -> &str {
        match self.metric.as_str() {
            "amount_usd" => "USD",
            "tokens" => "Tokens",
            "requests" => "Requests",
            metric => metric,
        }
    }

    pub fn reset_at_label(&self) -> Option<String> {
        DateTime::from_timestamp(self.resets_at?, 0).map(|reset| {
            reset
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        })
    }

    pub fn exhausted(&self) -> bool {
        self.limit > 0.0
            && (self.remaining.is_some_and(|remaining| remaining <= 0.0)
                || self.remaining_percent.is_some_and(|percent| percent <= 0.0))
    }

    pub fn amount_label(&self) -> String {
        if self.limit == 0.0 {
            return "Unlimited".into();
        }
        let remaining = self
            .remaining
            .map(|remaining| self.format_amount(remaining))
            .unwrap_or_else(|| "--".into());
        format!("{remaining} / {} left", self.format_amount(self.limit))
    }

    fn format_amount(&self, amount: f64) -> String {
        if self.metric == "amount_usd" {
            format!("${amount:.2}")
        } else {
            let unit = if self.metric == "tokens" {
                " tokens"
            } else if self.metric == "requests" {
                " requests"
            } else {
                ""
            };
            format!("{amount:.0}{unit}")
        }
    }
}

pub fn query(account: &ApiAccount, cancellation: CancellationToken) -> Result<UsageRecord> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::io("CPA quota runtime", error))?;
    let result = runtime.block_on(async {
        tokio::select! {
            result = fetch(account) => result,
            _ = async {
                while !cancellation.is_cancelled() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => Err(Error::Cancelled),
        }
    });
    match result {
        Ok(usage) => Ok(usage),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(error) => {
            let now = now_epoch();
            Ok(UsageRecord {
                observed_at: now,
                last_attempted_at: now,
                error: Some(error.to_string()),
                ..UsageRecord::default()
            })
        }
    }
}

async fn fetch(account: &ApiAccount) -> Result<UsageRecord> {
    // Never follow redirects carrying the CPA key to another endpoint.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| Error::Message("CPA quota HTTP client failed.".into()))?;
    let response = client
        .get(account.billing_url())
        .bearer_auth(&account.api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            Error::Message(if error.is_timeout() {
                "CPA quota request timed out.".into()
            } else {
                "CPA quota request failed (connection or TLS error).".into()
            })
        })?;
    if !response.status().is_success() {
        return Err(Error::Message(format!(
            "CPA quota request failed (HTTP {}).",
            response.status().as_u16()
        )));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| Error::Message("CPA quota response is not valid JSON.".into()))?;
    parse_usage(&payload)
}

fn number(object: &Map<String, Value>, name: &str) -> Result<Option<f64>> {
    let Some(value) = object.get(name).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()));
    number
        .filter(|number| number.is_finite() && *number >= 0.0)
        .map(Some)
        .ok_or_else(|| Error::Message(format!("Invalid CPA quota field: {name}.")))
}

fn parse_usage(payload: &Value) -> Result<UsageRecord> {
    let subscription = payload
        .get("subscription")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Message("CPA quota response has no subscription.".into()))?;
    let unlimited = match subscription.get("unlimited") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => {
            return Err(Error::Message(
                "Invalid CPA subscription unlimited flag.".into(),
            ));
        }
    };
    let mut quota = CpaUsage {
        unlimited,
        dimensions: Vec::new(),
    };
    if !unlimited {
        let windows = subscription
            .get("windows")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Message("CPA subscription has no quota windows.".into()))?;
        for (index, window) in windows.iter().enumerate() {
            let window_name = match window.get("name").filter(|value| !value.is_null()) {
                None => None,
                Some(Value::String(name))
                    if !name.trim().is_empty() && !name.chars().any(char::is_control) =>
                {
                    Some(name.trim().to_owned())
                }
                _ => return Err(Error::Message("Invalid CPA quota window name.".into())),
            };
            let period_seconds = window
                .get("period_seconds")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        Error::Message("Invalid CPA quota window period_seconds.".into())
                    })
                })
                .transpose()?;
            let resets_at = window
                .get("end_at")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.timestamp())
                        .ok_or_else(|| Error::Message("Invalid CPA quota window end_at.".into()))
                })
                .transpose()?;
            let dimensions = window
                .get("dimensions")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::Message("CPA quota window has no dimensions.".into()))?;
            for dimension in dimensions {
                let object = dimension
                    .as_object()
                    .ok_or_else(|| Error::Message("Invalid CPA quota dimension.".into()))?;
                let metric = object
                    .get("metric")
                    .and_then(Value::as_str)
                    .filter(|metric| !metric.is_empty() && !metric.chars().any(char::is_control))
                    .ok_or_else(|| {
                        Error::Message("CPA quota dimension has no valid metric.".into())
                    })?;
                let limit = number(object, "limit")?
                    .ok_or_else(|| Error::Message("CPA quota dimension has no limit.".into()))?;
                let used = number(object, "used")?;
                let remaining = number(object, "remaining")?.or_else(|| {
                    used.filter(|_| limit > 0.0)
                        .map(|used| (limit - used).max(0.0))
                });
                let remaining_percent = number(object, "used_percent")?
                    .map(|used| (100.0 - used).clamp(0.0, 100.0))
                    .or_else(|| {
                        remaining
                            .filter(|_| limit > 0.0)
                            .map(|remaining| (remaining / limit * 100.0).clamp(0.0, 100.0))
                    });
                if limit > 0.0 && remaining_percent.is_none() {
                    return Err(Error::Message(
                        "CPA quota dimension has no usage or remaining value.".into(),
                    ));
                }
                quota.dimensions.push(CpaDimension {
                    window: index + 1,
                    window_name: window_name.clone(),
                    period_seconds,
                    resets_at,
                    metric: metric.to_owned(),
                    limit,
                    remaining,
                    remaining_percent,
                });
            }
        }
        if quota.dimensions.is_empty() {
            return Err(Error::Message(
                "CPA subscription has no configured quota.".into(),
            ));
        }
    }
    let now = now_epoch();
    Ok(UsageRecord {
        observed_at: now,
        last_attempted_at: now,
        cpa: Some(quota),
        ..UsageRecord::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_keeps_server_window_name_period_and_exact_end_time() {
        let usage = parse_usage(&json!({"subscription": {"windows": [{
            "name": "Core", "period_seconds": 604800,
            "start_at": "2026-10-06T16:00:00Z", "end_at": "2026-10-13T16:00:00Z",
            "dimensions": [{"metric": "amount_usd", "limit": 400, "used": 1}]
        }]}}))
        .unwrap();
        let dimension = &usage.cpa.as_ref().unwrap().dimensions[0];
        assert_eq!(dimension.window_name.as_deref(), Some("Core"));
        assert_eq!(dimension.period_seconds, Some(604800));
        assert_eq!(dimension.window_label(), "Core · Weekly");
        assert_eq!(
            dimension.resets_at,
            Some(
                DateTime::parse_from_rfc3339("2026-10-14T00:00:00+08:00")
                    .unwrap()
                    .timestamp()
            )
        );
    }

    #[test]
    fn missing_window_metadata_stays_unknown_including_old_cached_usage() {
        let usage = parse_usage(&json!({"subscription": {"windows": [{
            "dimensions": [{"metric": "tokens", "limit": 100, "used": 30}]
        }]}}))
        .unwrap();
        let dimension = &usage.cpa.as_ref().unwrap().dimensions[0];
        assert_eq!(dimension.window_label(), "Window 1");
        assert!(dimension.period_seconds.is_none());
        assert!(dimension.resets_at.is_none());
        let cached: CpaUsage = serde_json::from_value(json!({
            "unlimited": false,
            "dimensions": [{"window": 1, "metric": "tokens", "limit": 100,
                "remaining": 70, "remaining_percent": 70}]
        }))
        .unwrap();
        assert!(cached.dimensions[0].reset_at_label().is_none());
    }

    #[test]
    fn invalid_window_metadata_is_reported_not_ignored() {
        for metadata in [
            json!({"name": 123}),
            json!({"name": "Core\nInjected"}),
            json!({"period_seconds": -1}),
            json!({"period_seconds": 1.5}),
            json!({"period_seconds": "604800"}),
            json!({"end_at": "not-a-timestamp"}),
            json!({"end_at": 123}),
        ] {
            let mut window = metadata;
            window["dimensions"] = json!([{"metric": "amount_usd", "limit": 100, "used": 30}]);
            assert!(parse_usage(&json!({"subscription": {"windows": [window]}})).is_err());
        }
    }

    #[test]
    fn quota_keeps_every_window_and_dimension_and_computes_remaining() {
        let usage = parse_usage(&json!({"subscription": {"windows": [
            {"dimensions": [
                {"metric": "amount_usd", "limit": 100, "used": 30},
                {"metric": "tokens", "limit": "1000", "remaining": "200"}
            ]},
            {"dimensions": [{"metric": "requests", "limit": 10, "used": 15}]}
        ]}}))
        .unwrap();
        assert!(usage.exhausted_now(now_epoch()));
        let quota = usage.cpa.unwrap();
        assert_eq!(quota.dimensions.len(), 3);
        assert_eq!(quota.dimensions[0].remaining_percent, Some(70.0));
        assert_eq!(quota.dimensions[0].amount_label(), "$70.00 / $100.00 left");
        assert_eq!(quota.dimensions[1].remaining_percent, Some(20.0));
        assert_eq!(quota.dimensions[2].window, 2);
        assert_eq!(quota.dimensions[2].remaining_percent, Some(0.0));
    }

    #[test]
    fn unlimited_and_missing_quota_are_distinct() {
        assert!(
            parse_usage(&json!({"subscription": {"unlimited": true}}))
                .unwrap()
                .cpa
                .unwrap()
                .unlimited
        );
        for payload in [
            json!({}),
            json!({"subscription": {"windows": []}}),
            json!({"subscription": {"windows": [{"dimensions": [
                {"metric": "amount_usd", "limit": 100}
            ]}]}}),
        ] {
            assert!(parse_usage(&payload).is_err());
        }
    }
}
