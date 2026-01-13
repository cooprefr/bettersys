use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    Buy,
    Sell,
    Hold,
}

impl DecisionAction {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BUY" => Some(Self::Buy),
            "SELL" => Some(Self::Sell),
            "HOLD" => Some(Self::Hold),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionUncertainty {
    Low,
    Med,
    High,
}

impl DecisionUncertainty {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "LOW" => Some(Self::Low),
            "MED" | "MID" => Some(Self::Med),
            "HIGH" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedDecisionDsl {
    pub action: DecisionAction,
    pub outcome_raw: Option<String>,
    pub outcome_index: Option<usize>,
    pub p_true: Option<f64>,
    pub uncertainty: Option<DecisionUncertainty>,
    pub size_mult: Option<f64>,
    pub flags: Vec<String>,
    pub rationale_hash: Option<String>,
}

impl ParsedDecisionDsl {
    pub fn is_buy(&self) -> bool {
        self.action == DecisionAction::Buy
    }

    pub fn map_outcome_index(&self, outcomes: &[String]) -> Option<usize> {
        if outcomes.is_empty() {
            return None;
        }

        if let Some(i) = self.outcome_index {
            return (i < outcomes.len()).then_some(i);
        }

        let Some(raw) = self.outcome_raw.as_ref() else {
            return None;
        };
        let raw_trim = raw.trim();

        if let Ok(i) = raw_trim.parse::<usize>() {
            return (i < outcomes.len()).then_some(i);
        }

        let raw_upper = raw_trim.to_ascii_uppercase();
        if raw_upper == "YES" {
            if let Some(i) = outcomes.iter().position(|o| o.eq_ignore_ascii_case("Yes")) {
                return Some(i);
            }
            return outcomes.len().ge(&2).then_some(0);
        }
        if raw_upper == "NO" {
            if let Some(i) = outcomes.iter().position(|o| o.eq_ignore_ascii_case("No")) {
                return Some(i);
            }
            return outcomes.len().ge(&2).then_some(1);
        }
        if raw_upper == "UP" {
            if let Some(i) = outcomes.iter().position(|o| o.eq_ignore_ascii_case("Up")) {
                return Some(i);
            }
        }
        if raw_upper == "DOWN" {
            if let Some(i) = outcomes.iter().position(|o| o.eq_ignore_ascii_case("Down")) {
                return Some(i);
            }
        }

        outcomes
            .iter()
            .position(|o| o.trim().eq_ignore_ascii_case(raw_trim))
    }
}

pub fn parse_decision_dsl(raw: &str) -> Result<ParsedDecisionDsl> {
    let mut action: Option<DecisionAction> = None;
    let mut outcome_raw: Option<String> = None;
    let mut outcome_index: Option<usize> = None;
    let mut p_true: Option<f64> = None;
    let mut uncertainty: Option<DecisionUncertainty> = None;
    let mut size_mult: Option<f64> = None;
    let mut flags: Vec<String> = Vec::new();
    let mut rationale_hash: Option<String> = None;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some((k, v)) = line.split_once('=') else {
            continue;
        };

        let key = k.trim().to_ascii_uppercase();
        let val = v.trim();

        match key.as_str() {
            "ACTION" => action = DecisionAction::parse(val),
            "OUTCOME" => {
                if !val.is_empty() {
                    outcome_raw = Some(val.chars().take(64).collect());
                }
            }
            "OUTCOME_INDEX" => {
                outcome_index = val.parse::<usize>().ok();
            }
            "P_TRUE" => {
                p_true = val
                    .parse::<f64>()
                    .ok()
                    .filter(|x| x.is_finite())
                    .map(|x| x.clamp(0.0001, 0.9999));
            }
            "UNCERTAINTY" => uncertainty = DecisionUncertainty::parse(val),
            "SIZE_MULT" => {
                size_mult = val
                    .parse::<f64>()
                    .ok()
                    .filter(|x| x.is_finite())
                    .map(|x| x.clamp(0.0, 1.0));
            }
            "FLAGS" => {
                flags = val
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .take(16)
                    .map(|s| s.chars().take(32).collect::<String>())
                    .collect();
            }
            "RATIONALE_HASH" => {
                if !val.is_empty() {
                    rationale_hash = Some(val.chars().take(64).collect());
                }
            }
            _ => return Err(anyhow!("unknown key in dsl: {key}")),
        }
    }

    let action = action.ok_or_else(|| anyhow!("missing ACTION"))?;
    if action != DecisionAction::Hold && outcome_raw.is_none() && outcome_index.is_none() {
        return Err(anyhow!("missing OUTCOME/OUTCOME_INDEX"));
    }

    Ok(ParsedDecisionDsl {
        action,
        outcome_raw,
        outcome_index,
        p_true,
        uncertainty,
        size_mult,
        flags,
        rationale_hash,
    })
}

#[derive(Debug, Clone)]
pub struct LlmUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LlmCallOutput {
    pub model: String,
    pub content: String,
    pub usage: LlmUsage,
    pub latency_ms: u64,
}

#[derive(Clone)]
pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: String,
    referer: Option<String>,
    title: Option<String>,
}

impl OpenRouterClient {
    pub fn from_env(http: reqwest::Client) -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY missing (set env var)")?;
        if api_key.trim().is_empty() {
            return Err(anyhow!("OPENROUTER_API_KEY empty"));
        }

        let referer = std::env::var("OPENROUTER_HTTP_REFERER")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let title = std::env::var("OPENROUTER_APP_TITLE")
            .ok()
            .filter(|s| !s.trim().is_empty());

        Ok(Self {
            http,
            api_key,
            referer,
            title,
        })
    }

    pub async fn chat_completion(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        temperature: f64,
        timeout: Duration,
    ) -> Result<LlmCallOutput> {
        let start = Instant::now();

        let req = ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user.to_string(),
                },
            ],
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
        };

        let mut http_req = self
            .http
            .post("https://openrouter.ai/api/v1/chat/completions")
            .timeout(timeout)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json");

        if let Some(r) = &self.referer {
            http_req = http_req.header("HTTP-Referer", r);
        }
        if let Some(t) = &self.title {
            http_req = http_req.header("X-Title", t);
        }

        let resp = http_req
            .json(&req)
            .send()
            .await
            .context("openrouter request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let snippet: String = body.chars().take(800).collect();
            return Err(anyhow!("openrouter {}: {}", status.as_u16(), snippet));
        }

        let parsed: ChatCompletionResponse =
            serde_json::from_str(&body).context("openrouter json parse")?;
        let content = parsed
            .choices
            .get(0)
            .and_then(|c| c.message.as_ref())
            .map(|m| m.content.clone())
            .unwrap_or_default();

        Ok(LlmCallOutput {
            model: model.to_string(),
            content,
            usage: LlmUsage {
                prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
                completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
                total_tokens: parsed.usage.as_ref().and_then(|u| u.total_tokens),
            },
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatChoice {
    pub message: Option<ChatMessageOut>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatMessageOut {
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dsl_buy_yes() {
        let dsl = "ACTION=BUY\nOUTCOME=YES\nP_TRUE=0.62\nUNCERTAINTY=LOW\nSIZE_MULT=0.50\nFLAGS=\nRATIONALE_HASH=abc";
        let parsed = parse_decision_dsl(dsl).unwrap();
        assert_eq!(parsed.action, DecisionAction::Buy);
        assert_eq!(parsed.outcome_raw.as_deref(), Some("YES"));
        assert_eq!(parsed.p_true, Some(0.62));
        assert_eq!(parsed.size_mult, Some(0.5));
    }

    #[test]
    fn parse_dsl_unknown_key_rejected() {
        let dsl = "ACTION=BUY\nWAT=NO\n";
        assert!(parse_decision_dsl(dsl).is_err());
    }
}
