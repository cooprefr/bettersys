use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

use crate::signals::db_storage::DbSignalStorage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GammaMarketLookup {
    #[serde(default)]
    pub id: Option<String>,
    pub slug: String,
    #[serde(rename = "conditionId")]
    pub condition_id: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "endDateIso", default, alias = "end_date_iso")]
    pub end_date_iso: Option<String>,
    #[serde(default, deserialize_with = "de_string_f64_opt")]
    pub volume: Option<f64>,
    #[serde(default, deserialize_with = "de_string_f64_opt")]
    pub liquidity: Option<f64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub closed: Option<bool>,
    #[serde(deserialize_with = "de_string_vec")]
    pub outcomes: Vec<String>,
    #[serde(rename = "clobTokenIds", deserialize_with = "de_string_vec")]
    pub clob_token_ids: Vec<String>,
}

fn de_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Array(arr) => Ok(arr
            .into_iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect()),
        Value::String(s) => {
            // Some Gamma responses return JSON arrays as a string (e.g. "[\"Yes\",\"No\"]").
            serde_json::from_str::<Vec<String>>(&s).map_err(serde::de::Error::custom)
        }
        _ => Ok(Vec::new()),
    }
}

fn de_string_f64_opt<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(n.as_f64()),
        Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<f64>().map(Some).map_err(serde::de::Error::custom)
            }
        }
        _ => Ok(None),
    }
}

pub async fn gamma_market_lookup(
    storage: &DbSignalStorage,
    http: &reqwest::Client,
    market_slug: &str,
) -> Result<Option<GammaMarketLookup>> {
    let now = Utc::now().timestamp();
    let ttl_seconds = 24 * 3600;
    let cache_key = format!("gamma_market_lookup_v1:{}", market_slug.to_lowercase());

    if let Ok(Some((cache_json, fetched_at))) = storage.get_cache(&cache_key) {
        if now - fetched_at <= ttl_seconds {
            if let Ok(m) = serde_json::from_str::<GammaMarketLookup>(&cache_json) {
                return Ok(Some(m));
            }
        }
    }

    // Gamma API: /markets?slug=...&limit=1 returns Vec
    let response = http
        .get("https://gamma-api.polymarket.com/markets")
        .timeout(Duration::from_secs(8))
        .header(reqwest::header::USER_AGENT, "BetterBot/1.0")
        .query(&[("slug", market_slug), ("limit", "1")])
        .send()
        .await
        .context("gamma markets request failed")?
        .error_for_status()
        .context("gamma markets status")?;

    let body = response.text().await.context("gamma markets text")?;
    debug!(slug = %market_slug, body_len = body.len(), "gamma API response received");

    let markets: Vec<GammaMarketLookup> = serde_json::from_str(&body)
        .map_err(|e| {
            warn!(slug = %market_slug, error = %e, body_preview = %body.chars().take(500).collect::<String>(), "gamma JSON parse failed");
            e
        })
        .context("gamma markets json parse")?;

    let Some(m) = markets.into_iter().next() else {
        return Ok(None);
    };

    if let Ok(json) = serde_json::to_string(&m) {
        let _ = storage.upsert_cache(&cache_key, &json, now);
    }

    Ok(Some(m))
}

pub async fn resolve_clob_token_id_by_slug(
    storage: &DbSignalStorage,
    http: &reqwest::Client,
    market_slug: &str,
    outcome: &str,
) -> Result<Option<String>> {
    let Some(m) = gamma_market_lookup(storage, http, market_slug).await? else {
        return Ok(None);
    };

    let idx = m
        .outcomes
        .iter()
        .position(|o| o.eq_ignore_ascii_case(outcome));
    let Some(i) = idx else {
        return Ok(None);
    };

    Ok(m.clob_token_ids.get(i).cloned())
}
