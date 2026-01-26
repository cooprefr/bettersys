//! Vault Trading Engine
//!
//! RN-JD Integration Checklist:
//! [x] BeliefVolTracker initialized in AppState (main.rs)
//! [x] Market observations recorded in evaluate_updown15m()
//! [x] estimate_p_up_enhanced() called with RN-JD correction
//! [x] Jump regime handling: 2x min edge requirement
//! [x] Vol-adjusted Kelly: kelly_with_belief_vol() integrated
//! [x] Backtest recording: BacktestCollector in AppState
//! [x] A/B testing: ABTestTracker in AppState
//! [x] API endpoints: /api/belief-vol/stats, /api/backtest/*, /api/ab-test/*

use anyhow::Result;
use chrono::Utc;
use std::{
    collections::{HashMap, VecDeque},
    env,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    models::{MarketSignal, SignalType, WsServerEvent},
    scrapers::{polymarket::OrderBook, polymarket_gamma},
    vault::{
        calculate_kelly_position, estimate_p_up_enhanced, p_up_driftless_lognormal,
        parse_decision_dsl, parse_updown_15m_slug, shrink_to_half, DecisionAction,
        ExecutionAdapter, KellyParams, OpenRouterClient, OrderRequest, OrderSide,
        PaperExecutionAdapter, PolymarketClobAdapter, TimeInForce, UpDown15mMarket, UpDownAsset,
        VaultActivityRecord, VaultNavSnapshotRecord,
    },
    AppState,
};

#[derive(Debug, Clone)]
pub struct VaultEngineConfig {
    pub enabled: bool,
    pub paper: bool,
    pub updown_poll_ms: u64,
    pub updown_min_edge: f64,
    pub updown_kelly_fraction: f64,
    pub updown_max_position_pct: f64,
    pub updown_shrink_to_half: f64,
    pub updown_cooldown_sec: i64,

    /// Maximum drawdown before halting trading (e.g., 0.30 = 30%)
    pub max_drawdown_pct: f64,
    /// Track initial bankroll for drawdown calculation
    pub initial_bankroll: f64,

    pub long_enabled: bool,
    pub long_poll_ms: u64,
    pub long_min_edge: f64,
    pub long_kelly_fraction: f64,
    pub long_max_position_pct: f64,
    pub long_min_trade_usd: f64,
    pub long_max_trade_usd: f64,
    pub long_min_infer_interval_sec: i64,
    pub long_cooldown_sec: i64,
    pub long_max_calls_per_day: u32,
    pub long_max_calls_per_market_per_day: u32,
    pub long_max_tokens_per_day: u64,
    pub long_llm_timeout_sec: u64,
    pub long_llm_max_tokens: u32,
    pub long_llm_temperature: f64,
    pub long_max_tte_days: f64,
    pub long_max_spread_bps: f64,
    pub long_min_top_of_book_usd: f64,
    pub long_fee_buffer: f64,
    pub long_slippage_buffer_min: f64,
    pub long_dispersion_max: f64,
    pub long_exit_price_90: f64,
    pub long_exit_price_95: f64,
    pub long_exit_frac_90: f64,
    pub long_exit_frac_95: f64,
    pub long_wallet_window_sec: i64,
    pub long_wallet_max_trades_per_window: usize,
    pub long_wallet_min_notional_usd: f64,
    pub long_models: Vec<String>,
}

impl Default for VaultEngineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper: true,
            updown_poll_ms: 2000,
            updown_min_edge: 0.01,
            updown_kelly_fraction: 0.05,
            updown_max_position_pct: 0.01,
            updown_shrink_to_half: 0.35,
            updown_cooldown_sec: 30,

            max_drawdown_pct: 0.30,
            initial_bankroll: 50.0,

            long_enabled: false,
            long_poll_ms: 5000,
            long_min_edge: 0.02,
            long_kelly_fraction: 0.02,
            long_max_position_pct: 0.01,
            long_min_trade_usd: 5.0,
            long_max_trade_usd: 250.0,
            long_min_infer_interval_sec: 60,
            long_cooldown_sec: 300,
            long_max_calls_per_day: 200,
            long_max_calls_per_market_per_day: 30,
            long_max_tokens_per_day: 300_000,
            long_llm_timeout_sec: 20,
            long_llm_max_tokens: 220,
            long_llm_temperature: 0.15,
            long_max_tte_days: 240.0,
            long_max_spread_bps: 500.0,
            long_min_top_of_book_usd: 250.0,
            long_fee_buffer: 0.01,
            long_slippage_buffer_min: 0.01,
            long_dispersion_max: 0.12,
            long_exit_price_90: 0.90,
            long_exit_price_95: 0.95,
            long_exit_frac_90: 0.50,
            long_exit_frac_95: 0.90,
            long_wallet_window_sec: 60,
            long_wallet_max_trades_per_window: 6,
            long_wallet_min_notional_usd: 50.0,
            long_models: vec![
                "x-ai/grok-4.1-thinking".to_string(),
                "google/gemini-3.0-high-think".to_string(),
                "openai/gpt-5.2-extra-high-thinking".to_string(),
                "anthropic/opus-4.5-thinking".to_string(),
            ],
        }
    }
}

#[derive(Debug, Default)]
struct LongEngineState {
    markets: HashMap<String, LongMarketState>,
    wallet_activity: HashMap<String, WalletMarketActivity>,
    budget_day_start_ts: i64,
    calls_today: u32,
    tokens_today: u64,
    exit_bands: HashMap<String, u8>,
}

#[derive(Debug, Clone)]
struct LongMarketState {
    market_slug: String,
    latest_signal: Option<MarketSignal>,
    last_signal_at: i64,
    last_infer_at: i64,
    pending_signal: bool,
    last_trade_at: i64,
    calls_day_start_ts: i64,
    calls_today: u32,
}

#[derive(Debug, Default)]
struct WalletMarketActivity {
    events: VecDeque<(i64, String)>,
}

impl LongEngineState {
    fn new() -> Self {
        Self::default()
    }

    fn reset_day_if_needed(&mut self, now_ts: i64) {
        let day_start = utc_day_start(now_ts);
        if self.budget_day_start_ts != day_start {
            self.budget_day_start_ts = day_start;
            self.calls_today = 0;
            self.tokens_today = 0;
            for m in self.markets.values_mut() {
                m.calls_day_start_ts = day_start;
                m.calls_today = 0;
            }
        }
    }

    fn can_spend_calls(
        &self,
        market_slug: &str,
        needed_calls: u32,
        cfg: &VaultEngineConfig,
    ) -> bool {
        if self.tokens_today >= cfg.long_max_tokens_per_day {
            return false;
        }
        if self.calls_today.saturating_add(needed_calls) > cfg.long_max_calls_per_day {
            return false;
        }

        let slug = market_slug.to_lowercase();
        let Some(m) = self.markets.get(&slug) else {
            return true;
        };
        if m.calls_today.saturating_add(needed_calls) > cfg.long_max_calls_per_market_per_day {
            return false;
        }

        true
    }

    fn spend_call(&mut self, market_slug: &str, tokens: u64, cfg: &VaultEngineConfig) {
        self.calls_today = self.calls_today.saturating_add(1);
        self.tokens_today = self.tokens_today.saturating_add(tokens);

        let slug = market_slug.to_lowercase();
        if let Some(m) = self.markets.get_mut(&slug) {
            m.calls_today = m.calls_today.saturating_add(1);
        }

        // Once exceeded, can_spend_calls will prevent new entries.
        if self.tokens_today > cfg.long_max_tokens_per_day {
            self.tokens_today = cfg.long_max_tokens_per_day;
        }
    }

    fn record_wallet_activity(
        &mut self,
        wallet: &str,
        market_slug: &str,
        now_ts: i64,
        outcome: &str,
        window_sec: i64,
        max_trades: usize,
    ) -> bool {
        if wallet.is_empty() {
            return false;
        }
        let key = format!("{}:{}", wallet.to_lowercase(), market_slug.to_lowercase());
        let entry = self.wallet_activity.entry(key).or_default();

        entry.events.push_back((now_ts, outcome.trim().to_string()));

        let min_ts = now_ts - window_sec.max(1);
        while let Some((ts, _)) = entry.events.front() {
            if *ts < min_ts {
                entry.events.pop_front();
            } else {
                break;
            }
        }

        if entry.events.len() > max_trades {
            return false;
        }

        let mut distinct: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (_ts, o) in &entry.events {
            let o = o.trim();
            if o.is_empty() {
                continue;
            }
            *distinct.entry(o.to_lowercase()).or_insert(0) += 1;
            if distinct.len() >= 2 {
                return false;
            }
        }

        true
    }

    fn upsert_market_signal(&mut self, signal: MarketSignal, now_ts: i64) {
        let slug = signal.market_slug.to_lowercase();
        let entry = self
            .markets
            .entry(slug.clone())
            .or_insert_with(|| LongMarketState {
                market_slug: slug.clone(),
                latest_signal: None,
                last_signal_at: 0,
                last_infer_at: 0,
                pending_signal: false,
                last_trade_at: 0,
                calls_day_start_ts: utc_day_start(now_ts),
                calls_today: 0,
            });

        entry.latest_signal = Some(signal);
        entry.last_signal_at = now_ts;
        entry.pending_signal = true;
    }

    fn take_due_candidates(
        &mut self,
        now_ts: i64,
        cfg: &VaultEngineConfig,
    ) -> Vec<(String, MarketSignal)> {
        let mut out = Vec::new();

        for (slug, st) in self.markets.iter_mut() {
            if !st.pending_signal {
                continue;
            }
            if now_ts - st.last_trade_at < cfg.long_cooldown_sec {
                continue;
            }
            if now_ts - st.last_infer_at < cfg.long_min_infer_interval_sec {
                continue;
            }

            let Some(sig) = st.latest_signal.clone() else {
                st.pending_signal = false;
                continue;
            };

            st.pending_signal = false;
            st.last_infer_at = now_ts;
            out.push((slug.clone(), sig));
        }

        out
    }

    fn note_trade(&mut self, market_slug: &str, now_ts: i64) {
        let slug = market_slug.to_lowercase();
        if let Some(st) = self.markets.get_mut(&slug) {
            st.last_trade_at = now_ts;
        }
    }

    fn exit_band_of(&self, token_id: &str) -> u8 {
        self.exit_bands.get(token_id).copied().unwrap_or(0)
    }

    fn set_exit_band(&mut self, token_id: &str, band: u8) {
        self.exit_bands.insert(token_id.to_string(), band);
    }
}

fn utc_day_start(ts: i64) -> i64 {
    let Some(dt) = chrono::DateTime::<Utc>::from_timestamp(ts, 0) else {
        return 0;
    };
    dt.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

fn parse_expiry_ts(signal_expiry: Option<&str>, gamma_expiry: Option<&str>) -> Option<i64> {
    let mut candidates = Vec::new();
    if let Some(s) = signal_expiry {
        if !s.trim().is_empty() {
            candidates.push(s);
        }
    }
    if let Some(s) = gamma_expiry {
        if !s.trim().is_empty() {
            candidates.push(s);
        }
    }

    for s in candidates {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp());
        }
    }
    None
}

fn long_system_prompt() -> String {
    let braid = r#"mermaid
graph TD
I[Inputs]-->A[Admissible]
A-->B{StrongEdge}
B--No-->H[HOLD]
B--Yes-->C[PickOutcome]
C-->D[EstimateP]
D-->E[SizeMult]
E-->O[OutputDSL]
"#;

    format!(
        "You are a conservative trading decision module. Follow the plan.\n\n{}\n\nReturn ONLY KEY=VALUE lines. Allowed keys: ACTION, OUTCOME_INDEX, P_TRUE, UNCERTAINTY, SIZE_MULT, FLAGS, RATIONALE_HASH.\nRules:\n- ACTION must be BUY or HOLD.\n- OUTCOME_INDEX must be 0 or 1 when ACTION=BUY.\n- P_TRUE must be 0.0001..0.9999.\n- SIZE_MULT must be 0..1.\n- No extra text.",
        braid
    )
}

fn long_user_prompt(
    signal: &MarketSignal,
    gamma: &polymarket_gamma::GammaMarketLookup,
    expiry_ts: i64,
    tte_days: f64,
) -> String {
    let title = signal.details.market_title.trim();
    let question = gamma
        .question
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect::<String>();
    let description = gamma
        .description
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(600)
        .collect::<String>();

    let outcomes = gamma
        .outcomes
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}: {}", i, o))
        .collect::<Vec<_>>()
        .join("\n");

    let mut wallet_line = String::new();
    if let SignalType::TrackedWalletEntry {
        wallet_label,
        position_value_usd,
        order_count,
        token_label,
        ..
    } = &signal.signal_type
    {
        wallet_line = format!(
            "wallet_label={} position_value_usd={:.0} order_count={} token_label={} ",
            wallet_label,
            *position_value_usd,
            *order_count,
            token_label.as_deref().unwrap_or("")
        );
    }

    format!(
        "market_slug={}\nmarket_title={}\nquestion={}\ndescription={}\nexpiry_ts={}\ntte_days={:.2}\n\noutcomes:\n{}\n\nmarket_snapshot: current_price={:.4} liquidity={:.0} volume_24h={:.0}\n\nsignal: {}\n\nReturn DSL now.",
        signal.market_slug,
        title,
        question,
        description,
        expiry_ts,
        tte_days,
        outcomes,
        signal.details.current_price,
        signal.details.liquidity,
        signal.details.volume_24h,
        wallet_line
    )
}

fn stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let mean = xs.iter().sum::<f64>() / (xs.len() as f64);
    let var = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / (xs.len() as f64);
    var.sqrt()
}

fn spread_bps_to_price(spread_bps: f64, price_ref: f64) -> f64 {
    if !(spread_bps.is_finite() && price_ref.is_finite()) {
        return 0.0;
    }
    (price_ref * (spread_bps / 10_000.0)).max(0.0)
}

async fn best_bid_ask_spread_top_usd(
    state: &AppState,
    token_id: &str,
) -> Result<(Option<f64>, Option<f64>, Option<f64>, Option<f64>)> {
    let Some(mut book) = orderbook_snapshot(state, token_id).await? else {
        return Ok((None, None, None, None));
    };

    book.bids.sort_by(|a, b| {
        b.price
            .partial_cmp(&a.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    book.asks.sort_by(|a, b| {
        a.price
            .partial_cmp(&b.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let bid = book.bids.first().map(|o| (o.price, o.size));
    let ask = book.asks.first().map(|o| (o.price, o.size));
    let spread_bps = match (bid, ask) {
        (Some((b, _)), Some((a, _))) if a > 0.0 && b > 0.0 => {
            let mid = 0.5 * (a + b);
            if mid > 0.0 {
                Some(((a - b) / mid) * 10_000.0)
            } else {
                None
            }
        }
        _ => None,
    };

    let top_usd = ask.map(|(a, sz)| a * sz);

    Ok((bid.map(|x| x.0), ask.map(|x| x.0), spread_bps, top_usd))
}

async fn orderbook_snapshot(state: &AppState, token_id: &str) -> Result<Option<OrderBook>> {
    state.polymarket_market_ws.request_subscribe(token_id);
    if let Some(book) = state.polymarket_market_ws.get_orderbook(token_id, 1500) {
        return Ok(Some((*book).clone()));
    }

    let orderbook = state
        .http_client
        .get("https://clob.polymarket.com/book")
        .timeout(Duration::from_secs(3))
        .query(&[("token_id", token_id)])
        .send()
        .await?
        .error_for_status()?
        .json::<OrderBook>()
        .await?;

    Ok(Some(orderbook))
}

impl VaultEngineConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        cfg.enabled = env::var("VAULT_ENGINE_ENABLED")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false);
        cfg.paper = env::var("VAULT_ENGINE_PAPER")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(true);

        cfg.updown_poll_ms = env::var("UPDOWN15M_POLL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 250)
            .unwrap_or(cfg.updown_poll_ms);

        cfg.updown_min_edge = env::var("UPDOWN15M_MIN_EDGE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.updown_min_edge);

        cfg.updown_kelly_fraction = env::var("UPDOWN15M_KELLY_FRACTION")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.updown_kelly_fraction);

        cfg.updown_max_position_pct = env::var("UPDOWN15M_MAX_POSITION_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.updown_max_position_pct);

        cfg.updown_shrink_to_half = env::var("UPDOWN15M_SHRINK")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.updown_shrink_to_half);

        cfg.updown_cooldown_sec = env::var("UPDOWN15M_COOLDOWN_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .unwrap_or(cfg.updown_cooldown_sec);

        cfg.max_drawdown_pct = env::var("VAULT_MAX_DRAWDOWN_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
            .unwrap_or(cfg.max_drawdown_pct);

        cfg.initial_bankroll = env::var("INITIAL_BANKROLL")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.initial_bankroll);

        cfg.long_enabled = env::var("VAULT_LLM_ENABLED")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(cfg.long_enabled);

        cfg.long_poll_ms = env::var("VAULT_LLM_POLL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 500)
            .unwrap_or(cfg.long_poll_ms);

        cfg.long_min_edge = env::var("VAULT_LLM_MIN_EDGE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_min_edge);

        cfg.long_kelly_fraction = env::var("VAULT_LLM_KELLY_FRACTION")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_kelly_fraction);

        cfg.long_max_position_pct = env::var("VAULT_LLM_MAX_POSITION_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_max_position_pct);

        cfg.long_min_trade_usd = env::var("VAULT_LLM_MIN_TRADE_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_min_trade_usd);

        cfg.long_max_trade_usd = env::var("VAULT_LLM_MAX_TRADE_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_max_trade_usd);

        cfg.long_min_infer_interval_sec = env::var("VAULT_LLM_MIN_INFER_INTERVAL_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .unwrap_or(cfg.long_min_infer_interval_sec);

        cfg.long_cooldown_sec = env::var("VAULT_LLM_COOLDOWN_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .unwrap_or(cfg.long_cooldown_sec);

        cfg.long_max_calls_per_day = env::var("VAULT_LLM_MAX_CALLS_PER_DAY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(cfg.long_max_calls_per_day);

        cfg.long_max_calls_per_market_per_day = env::var("VAULT_LLM_MAX_CALLS_PER_MARKET_PER_DAY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(cfg.long_max_calls_per_market_per_day);

        cfg.long_max_tokens_per_day = env::var("VAULT_LLM_MAX_TOKENS_PER_DAY")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(cfg.long_max_tokens_per_day);

        cfg.long_llm_timeout_sec = env::var("VAULT_LLM_TIMEOUT_SEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(cfg.long_llm_timeout_sec);

        cfg.long_llm_max_tokens = env::var("VAULT_LLM_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v >= 16)
            .unwrap_or(cfg.long_llm_max_tokens);

        cfg.long_llm_temperature = env::var("VAULT_LLM_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_llm_temperature);

        cfg.long_max_tte_days = env::var("VAULT_LLM_MAX_TTE_DAYS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_max_tte_days);

        cfg.long_max_spread_bps = env::var("VAULT_LLM_MAX_SPREAD_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_max_spread_bps);

        cfg.long_min_top_of_book_usd = env::var("VAULT_LLM_MIN_TOP_OF_BOOK_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_min_top_of_book_usd);

        cfg.long_fee_buffer = env::var("VAULT_LLM_FEE_BUFFER")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_fee_buffer);

        cfg.long_slippage_buffer_min = env::var("VAULT_LLM_SLIPPAGE_BUFFER_MIN")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_slippage_buffer_min);

        cfg.long_dispersion_max = env::var("VAULT_LLM_DISPERSION_MAX")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(cfg.long_dispersion_max);

        cfg.long_exit_price_90 = env::var("VAULT_LLM_EXIT_PRICE_90")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
            .unwrap_or(cfg.long_exit_price_90);

        cfg.long_exit_price_95 = env::var("VAULT_LLM_EXIT_PRICE_95")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
            .unwrap_or(cfg.long_exit_price_95);

        cfg.long_exit_frac_90 = env::var("VAULT_LLM_EXIT_FRAC_90")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
            .unwrap_or(cfg.long_exit_frac_90);

        cfg.long_exit_frac_95 = env::var("VAULT_LLM_EXIT_FRAC_95")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
            .unwrap_or(cfg.long_exit_frac_95);

        cfg.long_wallet_window_sec = env::var("VAULT_LLM_WALLET_WINDOW_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(cfg.long_wallet_window_sec);

        cfg.long_wallet_max_trades_per_window = env::var("VAULT_LLM_WALLET_MAX_TRADES_PER_WINDOW")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(cfg.long_wallet_max_trades_per_window);

        cfg.long_wallet_min_notional_usd = env::var("VAULT_LLM_WALLET_MIN_NOTIONAL_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(cfg.long_wallet_min_notional_usd);

        cfg.long_models = env::var("VAULT_LLM_MODELS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or(cfg.long_models);

        cfg
    }
}

#[derive(Clone)]
pub struct VaultEngine {
    state: AppState,
    cfg: VaultEngineConfig,
    exec: Arc<dyn ExecutionAdapter>,
    llm: Option<OpenRouterClient>,
    long_state: Arc<Mutex<LongEngineState>>,
}

impl VaultEngine {
    pub fn spawn(state: AppState) {
        let mut cfg = VaultEngineConfig::from_env();
        if !cfg.enabled {
            info!("vault engine disabled (set VAULT_ENGINE_ENABLED=1)");
            return;
        }

        let exec: Arc<dyn ExecutionAdapter> = if cfg.paper {
            info!("vault engine running in PAPER mode");
            Arc::new(PaperExecutionAdapter::default())
        } else {
            match PolymarketClobAdapter::from_env() {
                Some(clob) => {
                    info!("vault engine running in LIVE mode (Polymarket CLOB)");
                    Arc::new(clob)
                }
                None => {
                    warn!(
                        "VAULT_ENGINE_PAPER=0 but POLYMARKET_CLOB_* env vars not set; falling back to paper"
                    );
                    Arc::new(PaperExecutionAdapter::default())
                }
            }
        };

        let long_state = Arc::new(Mutex::new(LongEngineState::new()));

        let mut llm: Option<OpenRouterClient> = None;
        if cfg.long_enabled {
            if cfg.long_models.len() < 4 {
                warn!(
                    models = cfg.long_models.len(),
                    "VAULT_LLM_ENABLED=1 requires 4 models (VAULT_LLM_MODELS); disabling"
                );
                cfg.long_enabled = false;
            } else {
                match OpenRouterClient::from_env(state.http_client.clone()) {
                    Ok(c) => llm = Some(c),
                    Err(e) => {
                        warn!(error = %e, "VAULT_LLM_ENABLED=1 but OpenRouter env not set; disabling LONG engine");
                        cfg.long_enabled = false;
                    }
                }
            }
        }

        let engine = Self {
            state: state.clone(),
            cfg: cfg.clone(),
            exec,
            llm,
            long_state,
        };

        tokio::spawn(engine.clone().run_updown15m());

        // Periodic NAV snapshots (for PERFORMANCE tab).
        tokio::spawn(engine.clone().run_nav_snapshot_loop());

        if engine.cfg.long_enabled {
            tokio::spawn(engine.clone().run_long_engine());
        }

        // Signal router (non-15m): currently logs only.
        tokio::spawn(engine.run_signal_router(state.signal_broadcast.subscribe()));
    }

    async fn run_signal_router(self, mut rx: tokio::sync::broadcast::Receiver<WsServerEvent>) {
        loop {
            match rx.recv().await {
                Ok(WsServerEvent::Signal(signal)) => {
                    if parse_updown_15m_slug(&signal.market_slug).is_some() {
                        continue;
                    }
                    if matches!(signal.signal_type, SignalType::TrackedWalletEntry { .. }) {
                        if self.cfg.long_enabled {
                            self.ingest_long_signal(signal).await;
                        } else {
                            debug!(market_slug = %signal.market_slug, "non-15m signal observed (LLM engine disabled)");
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(dropped = n, "vault engine signal router lagged")
                }
                Err(_) => break,
            }
        }
    }

    async fn ingest_long_signal(&self, signal: MarketSignal) {
        let now = Utc::now().timestamp();

        let SignalType::TrackedWalletEntry {
            wallet_address,
            wallet_label,
            position_value_usd,
            order_count: _,
            token_label,
        } = &signal.signal_type
        else {
            return;
        };

        let wallet = wallet_address.trim().to_lowercase();
        let label = wallet_label.trim().to_lowercase();
        if wallet.is_empty() || label.is_empty() {
            return;
        }
        if label.contains("high_frequency") || label.contains("hft") {
            return;
        }
        if !(*position_value_usd).is_finite()
            || *position_value_usd < self.cfg.long_wallet_min_notional_usd
        {
            return;
        }

        let outcome = token_label.as_deref().unwrap_or("").trim().to_string();

        let mut long = self.long_state.lock().await;
        if !long.record_wallet_activity(
            &wallet,
            &signal.market_slug,
            now,
            &outcome,
            self.cfg.long_wallet_window_sec,
            self.cfg.long_wallet_max_trades_per_window,
        ) {
            return;
        }

        long.upsert_market_signal(signal, now);
    }

    async fn run_long_engine(self) -> Result<()> {
        let Some(llm) = self.llm.clone() else {
            return Ok(());
        };

        let mut interval = tokio::time::interval(Duration::from_millis(self.cfg.long_poll_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if let Err(e) = self.long_manage_exits().await {
                warn!(error = %e, "vault LONG exit loop error");
            }

            if let Err(e) = self.long_process_pending(&llm).await {
                warn!(error = %e, "vault LONG decision loop error");
            }
        }
    }

    async fn run_nav_snapshot_loop(self) -> Result<()> {
        let secs = env::var("VAULT_NAV_SNAPSHOT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(10, 3600);

        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            let now = Utc::now().timestamp();
            let bucket_ts = now - (now % (secs as i64));

            let positions_snapshot = {
                let ledger = self.state.vault.ledger.lock().await;
                ledger
                    .positions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.shares, v.avg_price))
                    .collect::<Vec<_>>()
            };

            let cash_usdc = { self.state.vault.ledger.lock().await.cash_usdc };

            let mut positions_value_usdc: f64 = 0.0;
            for (token_id, shares, fallback_price) in positions_snapshot {
                if shares <= 0.0 {
                    continue;
                }

                let mark = match best_bid_ask_spread_top_usd(&self.state, &token_id).await {
                    Ok((bid, ask, _spread_bps, _top_usd)) => match (bid, ask) {
                        (Some(b), Some(a)) => 0.5 * (b + a),
                        (Some(b), None) => b,
                        (None, Some(a)) => a,
                        (None, None) => fallback_price,
                    },
                    Err(e) => {
                        warn!(token_id = %token_id, error = %e, "nav snapshot mark failed");
                        fallback_price
                    }
                };

                if mark.is_finite() {
                    positions_value_usdc += shares * mark;
                }
            }

            let total_shares = self.state.vault.shares.lock().await.total_shares;
            let nav_usdc = (cash_usdc + positions_value_usdc).max(0.0);
            let nav_per_share = if total_shares > 0.0 {
                nav_usdc / total_shares
            } else {
                1.0
            };

            let snap_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("betterbot:navsnap:{}:{}", bucket_ts, secs).as_bytes(),
            )
            .to_string();

            let _ = self
                .state
                .vault
                .db
                .insert_nav_snapshot(&VaultNavSnapshotRecord {
                    id: snap_id,
                    ts: bucket_ts,
                    nav_usdc,
                    cash_usdc,
                    positions_value_usdc,
                    total_shares,
                    nav_per_share,
                    source: "periodic".to_string(),
                })
                .await;
        }
    }

    async fn long_manage_exits(&self) -> Result<()> {
        let now = Utc::now().timestamp();
        let positions_snapshot = {
            let ledger = self.state.vault.ledger.lock().await;
            ledger
                .positions
                .iter()
                .map(|(k, v)| (k.clone(), v.shares, v.outcome.clone()))
                .collect::<Vec<_>>()
        };

        for (token_id, shares, outcome) in positions_snapshot {
            if shares <= 0.0 {
                continue;
            }

            let (bid, _ask, _spread_bps, _top_usd) =
                best_bid_ask_spread_top_usd(&self.state, &token_id).await?;
            let Some(bid) = bid else {
                continue;
            };

            let mut exit_state = self.long_state.lock().await;
            let band = exit_state.exit_band_of(&token_id);
            drop(exit_state);

            let (target_band, frac) = if bid >= self.cfg.long_exit_price_95 {
                (2u8, self.cfg.long_exit_frac_95)
            } else if bid >= self.cfg.long_exit_price_90 {
                (1u8, self.cfg.long_exit_frac_90)
            } else {
                continue;
            };

            if band >= target_band {
                continue;
            }

            let shares_to_sell = (shares * frac).max(0.0);
            let notional = shares_to_sell * bid;
            if notional < self.cfg.long_min_trade_usd {
                continue;
            }

            let client_order_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("betterbot:exit:{}:{}:{}", token_id, target_band, now / 30).as_bytes(),
            )
            .to_string();

            let req = OrderRequest {
                client_order_id,
                token_id: token_id.clone(),
                side: OrderSide::Sell,
                price: bid,
                notional_usdc: notional,
                tif: TimeInForce::Ioc,
                market_slug: None,
                outcome: Some(outcome.clone()),
            };

            let ack = self.exec.place_order(req).await?;
            let (cash_usdc, total_shares) = {
                let mut ledger = self.state.vault.ledger.lock().await;
                ledger.apply_sell(
                    &token_id,
                    ack.filled_price,
                    ack.filled_notional_usdc,
                    ack.fees_usdc,
                );
                (
                    ledger.cash_usdc,
                    self.state.vault.shares.lock().await.total_shares,
                )
            };
            let _ = self
                .state
                .vault
                .db
                .upsert_state(cash_usdc, total_shares, ack.filled_at)
                .await;

            let (nav_usdc, positions_value_usdc, nav_per_share) = {
                let ledger = self.state.vault.ledger.lock().await;
                let nav = crate::vault::approximate_nav_usdc(&ledger);
                let pos_v = (nav - ledger.cash_usdc).max(0.0);
                let nav_ps = if total_shares > 0.0 {
                    nav / total_shares
                } else {
                    1.0
                };
                (nav, pos_v, nav_ps)
            };

            let prior_meta = self
                .state
                .vault
                .db
                .get_token_meta(&token_id)
                .await
                .ok()
                .flatten();
            let prior_slug = prior_meta.as_ref().map(|m| m.market_slug.clone());

            let _ = self
                .state
                .vault
                .db
                .insert_activity(&VaultActivityRecord {
                    id: ack.order_id.clone(),
                    ts: ack.filled_at,
                    kind: "TRADE".to_string(),
                    wallet_address: None,
                    amount_usdc: None,
                    shares: Some(ack.filled_notional_usdc / ack.filled_price.max(1e-9)),
                    token_id: Some(token_id.clone()),
                    market_slug: prior_slug,
                    outcome: Some(outcome.clone()),
                    side: Some("SELL".to_string()),
                    price: Some(ack.filled_price),
                    notional_usdc: Some(ack.filled_notional_usdc),
                    strategy: Some("LONG_EXIT".to_string()),
                    decision_id: prior_meta.and_then(|m| m.decision_id),
                })
                .await;
            let _ = self
                .state
                .vault
                .db
                .insert_nav_snapshot(&VaultNavSnapshotRecord {
                    id: Uuid::new_v4().to_string(),
                    ts: ack.filled_at,
                    nav_usdc,
                    cash_usdc,
                    positions_value_usdc,
                    total_shares,
                    nav_per_share,
                    source: "trade:LONG_EXIT".to_string(),
                })
                .await;

            let mut exit_state = self.long_state.lock().await;
            exit_state.set_exit_band(&token_id, target_band);
        }

        Ok(())
    }

    async fn long_process_pending(&self, llm: &OpenRouterClient) -> Result<()> {
        let now = Utc::now().timestamp();

        let candidates: Vec<(String, MarketSignal)> = {
            let mut long = self.long_state.lock().await;
            long.reset_day_if_needed(now);
            long.take_due_candidates(now, &self.cfg)
        };

        for (market_slug, signal) in candidates {
            if let Err(e) = self.long_evaluate_market(llm, &market_slug, &signal).await {
                warn!(market_slug = %market_slug, error = %e, "LONG market evaluation failed");
            }
        }

        Ok(())
    }

    async fn long_evaluate_market(
        &self,
        llm: &OpenRouterClient,
        market_slug: &str,
        signal: &MarketSignal,
    ) -> Result<()> {
        let now = Utc::now().timestamp();

        let Some(gamma) = polymarket_gamma::gamma_market_lookup(
            self.state.signal_storage.as_ref(),
            &self.state.http_client,
            market_slug,
        )
        .await?
        else {
            return Ok(());
        };

        if gamma.outcomes.len() != 2 || gamma.clob_token_ids.len() != 2 {
            return Ok(());
        }

        let Some(expiry_ts) = parse_expiry_ts(
            signal.details.expiry_time.as_deref(),
            gamma.end_date_iso.as_deref(),
        ) else {
            return Ok(());
        };
        let tte_sec = (expiry_ts - now) as f64;
        if !(tte_sec.is_finite() && tte_sec > 60.0) {
            return Ok(());
        }
        let tte_days = tte_sec / 86400.0;
        if tte_days > self.cfg.long_max_tte_days {
            return Ok(());
        }

        let decision_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "betterbot:llm:{}:{}:{}",
                market_slug.to_lowercase(),
                signal.id,
                now / 60
            )
            .as_bytes(),
        )
        .to_string();

        let (scout_model, rest_models) = self.cfg.long_models.split_first().unwrap();
        if !self.long_can_spend_calls(market_slug, 1).await {
            return Ok(());
        }

        let system = long_system_prompt();
        let user = long_user_prompt(signal, &gamma, expiry_ts, tte_days);

        let scout_out = llm
            .chat_completion(
                scout_model,
                &system,
                &user,
                self.cfg.long_llm_max_tokens,
                self.cfg.long_llm_temperature,
                Duration::from_secs(self.cfg.long_llm_timeout_sec),
            )
            .await;

        let (scout_call, scout_parsed, scout_err) = match scout_out {
            Ok(call) => {
                let parsed = parse_decision_dsl(&call.content);
                let err = parsed.as_ref().err().map(|e| e.to_string());
                (Some(call), parsed.ok(), err)
            }
            Err(e) => (None, None, Some(e.to_string())),
        };

        self.long_record_model_call(
            &decision_id,
            scout_model,
            now,
            scout_call.as_ref(),
            scout_parsed.as_ref(),
            scout_err.as_deref(),
        )
        .await;

        self.long_spend_after_call(market_slug, scout_call.as_ref())
            .await;

        let Some(scout_decision) = scout_parsed else {
            return Ok(());
        };

        if scout_decision.action != DecisionAction::Buy {
            self.state
                .signal_storage
                .insert_vault_llm_decision(
                    &decision_id,
                    market_slug,
                    now,
                    "HOLD",
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(scout_model),
                    None,
                    scout_decision.rationale_hash.as_deref(),
                )
                .await
                .ok();
            return Ok(());
        }

        let Some(outcome_i) = scout_decision.map_outcome_index(&gamma.outcomes) else {
            return Ok(());
        };
        let Some(p_true) = scout_decision.p_true else {
            return Ok(());
        };

        let token_id = gamma.clob_token_ids[outcome_i].clone();
        let (bid, ask, spread_bps, top_usd) =
            best_bid_ask_spread_top_usd(&self.state, &token_id).await?;

        let Some(ask) = ask else {
            return Ok(());
        };
        let spread_bps = spread_bps.unwrap_or(999_999.0);
        if spread_bps > self.cfg.long_max_spread_bps {
            return Ok(());
        }
        if top_usd.unwrap_or(0.0) < self.cfg.long_min_top_of_book_usd {
            return Ok(());
        }

        let slippage = self
            .cfg
            .long_slippage_buffer_min
            .max(spread_bps_to_price(spread_bps, ask) * 0.5);
        let p_eff = (ask + self.cfg.long_fee_buffer + slippage).clamp(0.0001, 0.9999);
        let edge = p_true - p_eff;
        if edge < self.cfg.long_min_edge {
            self.state
                .signal_storage
                .insert_vault_llm_decision(
                    &decision_id,
                    market_slug,
                    now,
                    "HOLD",
                    Some(outcome_i as i64),
                    Some(&gamma.outcomes[outcome_i]),
                    Some(p_true),
                    bid,
                    Some(ask),
                    Some(p_eff),
                    Some(edge),
                    scout_decision.size_mult,
                    Some(scout_model),
                    None,
                    scout_decision.rationale_hash.as_deref(),
                )
                .await
                .ok();
            return Ok(());
        }

        if !self.long_can_spend_calls(market_slug, 3).await {
            return Ok(());
        }

        let mut model_parsed: Vec<(String, Option<crate::vault::ParsedDecisionDsl>)> =
            Vec::with_capacity(4);
        model_parsed.push((scout_model.to_string(), Some(scout_decision.clone())));

        for model in rest_models.iter().take(3) {
            let out = llm
                .chat_completion(
                    model,
                    &system,
                    &user,
                    self.cfg.long_llm_max_tokens,
                    self.cfg.long_llm_temperature,
                    Duration::from_secs(self.cfg.long_llm_timeout_sec),
                )
                .await;

            let (call, parsed, err) = match out {
                Ok(call) => {
                    let parsed = parse_decision_dsl(&call.content);
                    let err = parsed.as_ref().err().map(|e| e.to_string());
                    (Some(call), parsed.ok(), err)
                }
                Err(e) => (None, None, Some(e.to_string())),
            };

            self.long_record_model_call(
                &decision_id,
                model,
                now,
                call.as_ref(),
                parsed.as_ref(),
                err.as_deref(),
            )
            .await;
            self.long_spend_after_call(market_slug, call.as_ref()).await;

            model_parsed.push((model.to_string(), parsed));
        }

        let mut votes = vec![0u32; 2];
        for (_model, parsed) in &model_parsed {
            let Some(d) = parsed else {
                continue;
            };
            if d.action != DecisionAction::Buy {
                continue;
            }
            let Some(i) = d.map_outcome_index(&gamma.outcomes) else {
                continue;
            };
            if i >= 2 {
                continue;
            }
            votes[i] += 1;
        }

        let (win_i, win_votes) = if votes[0] >= votes[1] {
            (0usize, votes[0])
        } else {
            (1usize, votes[1])
        };
        if win_votes < 3 {
            self.state
                .signal_storage
                .insert_vault_llm_decision(
                    &decision_id,
                    market_slug,
                    now,
                    "HOLD",
                    Some(outcome_i as i64),
                    Some(&gamma.outcomes[outcome_i]),
                    Some(p_true),
                    bid,
                    Some(ask),
                    Some(p_eff),
                    Some(edge),
                    scout_decision.size_mult,
                    None,
                    Some(&scout_decision.flags.join(",")),
                    scout_decision.rationale_hash.as_deref(),
                )
                .await
                .ok();
            return Ok(());
        }

        let mut p_list: Vec<f64> = Vec::new();
        let mut size_mults: Vec<f64> = Vec::new();
        let mut models_agree: Vec<String> = Vec::new();
        for (model, parsed) in &model_parsed {
            let Some(d) = parsed else {
                continue;
            };
            if d.action != DecisionAction::Buy {
                continue;
            }
            if d.map_outcome_index(&gamma.outcomes) != Some(win_i) {
                continue;
            }
            models_agree.push(model.clone());
            if let Some(p) = d.p_true {
                p_list.push(p);
            }
            if let Some(m) = d.size_mult {
                size_mults.push(m);
            }
        }

        let p_seed = if win_i == outcome_i {
            p_true
        } else {
            p_list.first().copied().unwrap_or(p_true)
        };

        let p_aggr = if !p_list.is_empty() {
            p_list.iter().sum::<f64>() / (p_list.len() as f64)
        } else {
            p_seed
        };
        let size_mult_aggr = if !size_mults.is_empty() {
            (size_mults.iter().sum::<f64>() / (size_mults.len() as f64)).clamp(0.0, 1.0)
        } else {
            scout_decision.size_mult.unwrap_or(0.5).clamp(0.0, 1.0)
        };

        let dispersion = stddev(&p_list);
        let dispersion_penalty = if dispersion.is_finite() {
            (1.0 - (dispersion / self.cfg.long_dispersion_max).clamp(0.0, 1.0)).clamp(0.0, 1.0)
        } else {
            1.0
        };

        let token_id = gamma.clob_token_ids[win_i].clone();
        let (_bid2, ask2, spread_bps2, top_usd2) =
            best_bid_ask_spread_top_usd(&self.state, &token_id).await?;
        let Some(ask2) = ask2 else {
            return Ok(());
        };
        let spread_bps2 = spread_bps2.unwrap_or(999_999.0);
        if spread_bps2 > self.cfg.long_max_spread_bps {
            return Ok(());
        }
        if top_usd2.unwrap_or(0.0) < self.cfg.long_min_top_of_book_usd {
            return Ok(());
        }

        let slippage2 = self
            .cfg
            .long_slippage_buffer_min
            .max(spread_bps_to_price(spread_bps2, ask2) * 0.5);
        let p_eff2 = (ask2 + self.cfg.long_fee_buffer + slippage2).clamp(0.0001, 0.9999);
        let edge2 = p_aggr - p_eff2;
        if edge2 < self.cfg.long_min_edge {
            return Ok(());
        }

        let bankroll = self.state.vault.ledger.lock().await.cash_usdc;
        if bankroll <= 0.0 {
            return Ok(());
        }
        let kelly_params = KellyParams {
            bankroll,
            kelly_fraction: self.cfg.long_kelly_fraction,
            max_position_pct: self.cfg.long_max_position_pct,
            min_position_usd: self.cfg.long_min_trade_usd,
        };

        let kelly = calculate_kelly_position(p_aggr, p_eff2, &kelly_params);
        if !kelly.should_trade {
            return Ok(());
        }

        let mut notional = kelly.position_size_usd * size_mult_aggr * dispersion_penalty;
        notional = notional.min(self.cfg.long_max_trade_usd).min(bankroll);
        if notional < self.cfg.long_min_trade_usd {
            return Ok(());
        }

        let client_order_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("betterbot:entry:{}:{}:{}", decision_id, token_id, now / 30).as_bytes(),
        )
        .to_string();

        let req = OrderRequest {
            client_order_id,
            token_id: token_id.clone(),
            side: OrderSide::Buy,
            price: ask2,
            notional_usdc: notional,
            tif: TimeInForce::Ioc,
            market_slug: Some(market_slug.to_string()),
            outcome: Some(gamma.outcomes[win_i].clone()),
        };

        let ack = self.exec.place_order(req.clone()).await?;
        let updated_at = ack.filled_at;

        let cash_usdc = {
            let mut ledger = self.state.vault.ledger.lock().await;
            ledger.apply_buy(
                &req.token_id,
                req.outcome.as_deref().unwrap_or(""),
                ack.filled_price,
                ack.filled_notional_usdc,
                ack.fees_usdc,
            );
            ledger.cash_usdc
        };
        let total_shares = self.state.vault.shares.lock().await.total_shares;
        let _ = self
            .state
            .vault
            .db
            .upsert_state(cash_usdc, total_shares, updated_at)
            .await;

        let (nav_usdc, positions_value_usdc, nav_per_share) = {
            let ledger = self.state.vault.ledger.lock().await;
            let nav = crate::vault::approximate_nav_usdc(&ledger);
            let pos_v = (nav - ledger.cash_usdc).max(0.0);
            let nav_ps = if total_shares > 0.0 {
                nav / total_shares
            } else {
                1.0
            };
            (nav, pos_v, nav_ps)
        };

        let _ = self
            .state
            .vault
            .db
            .insert_activity(&VaultActivityRecord {
                id: ack.order_id.clone(),
                ts: updated_at,
                kind: "TRADE".to_string(),
                wallet_address: None,
                amount_usdc: None,
                shares: Some(ack.filled_notional_usdc / ack.filled_price.max(1e-9)),
                token_id: Some(req.token_id.clone()),
                market_slug: Some(market_slug.to_string()),
                outcome: Some(gamma.outcomes[win_i].clone()),
                side: Some("BUY".to_string()),
                price: Some(ack.filled_price),
                notional_usdc: Some(ack.filled_notional_usdc),
                strategy: Some("LONG".to_string()),
                decision_id: Some(decision_id.clone()),
            })
            .await;
        let _ = self
            .state
            .vault
            .db
            .insert_nav_snapshot(&VaultNavSnapshotRecord {
                id: Uuid::new_v4().to_string(),
                ts: updated_at,
                nav_usdc,
                cash_usdc,
                positions_value_usdc,
                total_shares,
                nav_per_share,
                source: "trade:LONG".to_string(),
            })
            .await;

        let consensus_models = models_agree.join(",");
        let flags = scout_decision.flags.join(",");
        self.state
            .signal_storage
            .insert_vault_llm_decision(
                &decision_id,
                market_slug,
                now,
                "BUY",
                Some(win_i as i64),
                Some(&gamma.outcomes[win_i]),
                Some(p_aggr),
                None,
                Some(ask2),
                Some(p_eff2),
                Some(edge2),
                Some(size_mult_aggr),
                Some(&consensus_models),
                Some(&flags),
                scout_decision.rationale_hash.as_deref(),
            )
            .await
            .ok();

        {
            let mut long = self.long_state.lock().await;
            long.note_trade(market_slug, now);
        }

        info!(
            market_slug = %market_slug,
            outcome = %gamma.outcomes[win_i],
            price = ask2,
            notional,
            edge = edge2,
            votes = win_votes,
            "LONG paper trade"
        );

        Ok(())
    }

    async fn long_can_spend_calls(&self, market_slug: &str, needed_calls: u32) -> bool {
        let now = Utc::now().timestamp();
        let mut long = self.long_state.lock().await;
        long.reset_day_if_needed(now);
        long.can_spend_calls(market_slug, needed_calls, &self.cfg)
    }

    async fn long_spend_after_call(
        &self,
        market_slug: &str,
        call: Option<&crate::vault::LlmCallOutput>,
    ) {
        let now = Utc::now().timestamp();
        let tokens = call.and_then(|c| c.usage.total_tokens).unwrap_or(0) as u64;
        let mut long = self.long_state.lock().await;
        long.reset_day_if_needed(now);
        long.spend_call(market_slug, tokens, &self.cfg);
    }

    async fn long_record_model_call(
        &self,
        decision_id: &str,
        model: &str,
        created_at: i64,
        call: Option<&crate::vault::LlmCallOutput>,
        parsed: Option<&crate::vault::ParsedDecisionDsl>,
        error: Option<&str>,
    ) {
        let rec_id = Uuid::new_v4().to_string();
        let parsed_ok = parsed.is_some() && error.is_none();
        let action = parsed.map(|d| match d.action {
            DecisionAction::Buy => "BUY",
            DecisionAction::Sell => "SELL",
            DecisionAction::Hold => "HOLD",
        });
        let outcome_index = parsed.and_then(|d| d.outcome_index).map(|i| i as i64);
        let uncertainty = parsed.and_then(|d| d.uncertainty).map(|u| match u {
            crate::vault::DecisionUncertainty::Low => "LOW",
            crate::vault::DecisionUncertainty::Med => "MED",
            crate::vault::DecisionUncertainty::High => "HIGH",
        });
        let flags = parsed.map(|d| d.flags.join(","));

        let latency_ms = call.map(|c| c.latency_ms as i64);
        let prompt_tokens = call.and_then(|c| c.usage.prompt_tokens).map(|t| t as i64);
        let completion_tokens = call
            .and_then(|c| c.usage.completion_tokens)
            .map(|t| t as i64);
        let total_tokens = call.and_then(|c| c.usage.total_tokens).map(|t| t as i64);
        let raw_dsl = call.map(|c| c.content.as_str());

        let _ = self
            .state
            .signal_storage
            .insert_vault_llm_model_record(
                &rec_id,
                decision_id,
                model,
                created_at,
                parsed_ok,
                action,
                outcome_index,
                parsed.and_then(|d| d.p_true),
                uncertainty,
                parsed.and_then(|d| d.size_mult),
                flags.as_deref(),
                parsed.and_then(|d| d.rationale_hash.as_deref()),
                raw_dsl,
                latency_ms,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                error,
            )
            .await;
    }

    async fn run_updown15m(self) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_millis(self.cfg.updown_poll_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut token_cache: HashMap<String, (String, String)> = HashMap::new();
        let mut last_trade_ts: HashMap<String, i64> = HashMap::new();
        let mut halted = false;

        loop {
            interval.tick().await;
            let now = Utc::now().timestamp();

            // === DRAWDOWN CIRCUIT BREAKER ===
            if !halted {
                let nav_usdc = {
                    let ledger = self.state.vault.ledger.lock().await;
                    crate::vault::approximate_nav_usdc(&ledger)
                };
                let drawdown = 1.0 - (nav_usdc / self.cfg.initial_bankroll);
                if drawdown >= self.cfg.max_drawdown_pct {
                    warn!(
                        nav_usdc = %nav_usdc,
                        initial = %self.cfg.initial_bankroll,
                        drawdown_pct = %(drawdown * 100.0),
                        max_drawdown_pct = %(self.cfg.max_drawdown_pct * 100.0),
                        "DRAWDOWN CIRCUIT BREAKER TRIGGERED - HALTING ALL TRADING"
                    );
                    halted = true;
                }
            }
            if halted {
                // Engine is halted due to drawdown - skip all trading
                continue;
            }
            let start_ts = now - (now % (15 * 60));
            let end_ts = start_ts + 15 * 60;

            for asset in [
                UpDownAsset::Btc,
                UpDownAsset::Eth,
                UpDownAsset::Sol,
                UpDownAsset::Xrp,
            ] {
                let slug = format!("{}-updown-15m-{}", asset.as_str(), start_ts);
                let market = UpDown15mMarket {
                    asset,
                    start_ts,
                    end_ts,
                };

                if let Some(last) = last_trade_ts.get(&slug).copied() {
                    if now - last < self.cfg.updown_cooldown_sec {
                        continue;
                    }
                }

                let (token_up, token_down) = match token_cache.get(&slug).cloned() {
                    Some(t) => t,
                    None => {
                        let up = polymarket_gamma::resolve_clob_token_id_by_slug(
                            self.state.signal_storage.as_ref(),
                            &self.state.http_client,
                            &slug,
                            "Up",
                        )
                        .await?
                        .unwrap_or_default();
                        let down = polymarket_gamma::resolve_clob_token_id_by_slug(
                            self.state.signal_storage.as_ref(),
                            &self.state.http_client,
                            &slug,
                            "Down",
                        )
                        .await?
                        .unwrap_or_default();
                        if up.is_empty() || down.is_empty() {
                            continue;
                        }
                        token_cache.insert(slug.clone(), (up.clone(), down.clone()));
                        (up, down)
                    }
                };

                if let Some(req) = self
                    .evaluate_updown15m(&market, &slug, &token_up, &token_down)
                    .await?
                {
                    let ack = self.exec.place_order(req.clone()).await?;
                    let updated_at = ack.filled_at;
                    if req.side == OrderSide::Buy {
                        let cash_usdc = {
                            let mut ledger = self.state.vault.ledger.lock().await;
                            ledger.apply_buy(
                                &req.token_id,
                                req.outcome.as_deref().unwrap_or(""),
                                ack.filled_price,
                                ack.filled_notional_usdc,
                                ack.fees_usdc,
                            );
                            ledger.cash_usdc
                        };

                        let total_shares = self.state.vault.shares.lock().await.total_shares;
                        let _ = self
                            .state
                            .vault
                            .db
                            .upsert_state(cash_usdc, total_shares, updated_at)
                            .await;

                        let (nav_usdc, positions_value_usdc, nav_per_share) = {
                            let ledger = self.state.vault.ledger.lock().await;
                            let nav = crate::vault::approximate_nav_usdc(&ledger);
                            let pos_v = (nav - ledger.cash_usdc).max(0.0);
                            let nav_ps = if total_shares > 0.0 {
                                nav / total_shares
                            } else {
                                1.0
                            };
                            (nav, pos_v, nav_ps)
                        };

                        let _ = self
                            .state
                            .vault
                            .db
                            .insert_activity(&VaultActivityRecord {
                                id: ack.order_id.clone(),
                                ts: updated_at,
                                kind: "TRADE".to_string(),
                                wallet_address: None,
                                amount_usdc: None,
                                shares: Some(ack.filled_notional_usdc / ack.filled_price.max(1e-9)),
                                token_id: Some(req.token_id.clone()),
                                market_slug: req.market_slug.clone(),
                                outcome: req.outcome.clone(),
                                side: Some("BUY".to_string()),
                                price: Some(ack.filled_price),
                                notional_usdc: Some(ack.filled_notional_usdc),
                                strategy: Some("FAST15M".to_string()),
                                decision_id: None,
                            })
                            .await;
                        let _ = self
                            .state
                            .vault
                            .db
                            .insert_nav_snapshot(&VaultNavSnapshotRecord {
                                id: Uuid::new_v4().to_string(),
                                ts: updated_at,
                                nav_usdc,
                                cash_usdc,
                                positions_value_usdc,
                                total_shares,
                                nav_per_share,
                                source: "trade:FAST15M".to_string(),
                            })
                            .await;
                    }
                    last_trade_ts.insert(slug.clone(), now);
                    info!(
                        market_slug = %slug,
                        token_id = %req.token_id,
                        price = req.price,
                        notional = req.notional_usdc,
                        "UPDOWN15M paper trade"
                    );
                }
            }
        }
    }

    async fn evaluate_updown15m(
        &self,
        market: &UpDown15mMarket,
        slug: &str,
        token_up: &str,
        token_down: &str,
    ) -> Result<Option<OrderRequest>> {
        let now = Utc::now().timestamp();
        let t_rem = (market.end_ts - now).max(0) as f64;
        if t_rem < 15.0 {
            return Ok(None);
        }

        let symbol = market.asset.binance_symbol();
        let Some(p_now) = self.state.binance_feed.latest_mid(symbol).map(|p| p.mid) else {
            return Ok(None);
        };
        let Some(p_start) = self
            .state
            .binance_feed
            .mid_near(symbol, market.start_ts, 60)
            .map(|p| p.mid)
        else {
            return Ok(None);
        };
        let Some(sigma) = self.state.binance_feed.sigma_per_sqrt_s(symbol) else {
            return Ok(None);
        };

        // === ORACLE LAG CHECK ===
        // Polymarket settles using Chainlink, NOT Binance. During fast moves,
        // Chainlink can lag Binance by seconds, flipping outcomes.
        // If Chainlink feed is available, check for dangerous divergence.
        if let Some(ref chainlink) = self.state.chainlink_feed {
            let asset_str = market.asset.as_str();
            if let Some(lag) = chainlink.analyze_lag(asset_str) {
                if lag.should_skip_trade() {
                    debug!(
                        market_slug = %slug,
                        divergence_bps = lag.divergence_bps,
                        chainlink_age_ms = lag.chainlink_age_ms,
                        is_stale = lag.is_stale,
                        is_dangerous = lag.is_dangerous_regime,
                        "Skipping trade due to oracle lag/divergence"
                    );
                    return Ok(None);
                }
            }
            // Update Binance price in Chainlink tracker for divergence monitoring
            chainlink.update_binance_price(asset_str, p_now);
        }

        // Get market mid price for RN-JD estimation
        let ask_up = best_ask(&self.state, token_up).await?;
        let ask_down = best_ask(&self.state, token_down).await?;
        let market_mid = match (ask_up, ask_down) {
            (Some(a), Some(b)) => 0.5 * (a + (1.0 - b)), // Average of Up ask and implied Down
            (Some(a), None) => a,
            (None, Some(b)) => 1.0 - b,
            (None, None) => 0.5,
        };

        // Record observation for belief vol tracking
        {
            let mut tracker = self.state.belief_vol_tracker.write();
            tracker.record_observation(slug, market_mid, now);
        }

        // Use RN-JD enhanced estimation
        let est = match estimate_p_up_enhanced(
            p_start,
            p_now,
            market_mid,
            sigma,
            t_rem,
            Some(&*self.state.belief_vol_tracker.read()),
            slug,
            now,
        ) {
            Some(e) => e,
            None => {
                // Fallback to legacy estimation
                let Some(p_up_raw) = p_up_driftless_lognormal(p_start, p_now, sigma, t_rem) else {
                    return Ok(None);
                };
                debug!(market_slug = %slug, "RN-JD estimation failed, using legacy");
                crate::vault::RnjdEstimate {
                    p_up: p_up_raw,
                    drift_correction: 0.0,
                    p_up_raw,
                    confidence: 0.5,
                    jump_regime: false,
                }
            }
        };

        // Apply shrink (conservative adjustment)
        let p_up = shrink_to_half(est.p_up, self.cfg.updown_shrink_to_half);
        let p_down = 1.0 - p_up;

        // Log RN-JD diagnostics
        debug!(
            market_slug = %slug,
            p_up_rnjd = est.p_up,
            p_up_raw = est.p_up_raw,
            drift_correction = est.drift_correction,
            jump_regime = est.jump_regime,
            confidence = est.confidence,
            "RN-JD estimate"
        );
        let (side_token, side_outcome, side_price, side_conf) = match (ask_up, ask_down) {
            (Some(a_up), Some(a_down)) => {
                let edge_up = p_up - a_up;
                let edge_down = p_down - a_down;
                if edge_up >= edge_down {
                    (token_up.to_string(), "Up".to_string(), a_up, p_up)
                } else {
                    (token_down.to_string(), "Down".to_string(), a_down, p_down)
                }
            }
            (Some(a_up), None) => (token_up.to_string(), "Up".to_string(), a_up, p_up),
            (None, Some(a_down)) => (token_down.to_string(), "Down".to_string(), a_down, p_down),
            (None, None) => return Ok(None),
        };

        let edge = side_conf - side_price;

        // Jump regime handling: require 2x edge during elevated jump risk
        let effective_min_edge = if est.jump_regime {
            self.cfg.updown_min_edge * 2.0
        } else {
            self.cfg.updown_min_edge
        };

        if edge < effective_min_edge {
            if est.jump_regime {
                debug!(
                    market_slug = %slug,
                    edge = edge,
                    effective_min_edge = effective_min_edge,
                    "Edge below threshold during jump regime"
                );
            }
            return Ok(None);
        }

        let bankroll = self.state.vault.ledger.lock().await.cash_usdc;
        if bankroll <= 0.0 {
            return Ok(None);
        }

        let kelly_params = KellyParams {
            bankroll,
            kelly_fraction: self.cfg.updown_kelly_fraction,
            max_position_pct: self.cfg.updown_max_position_pct,
            min_position_usd: 1.0,
        };

        // Use vol-adjusted Kelly with belief volatility
        let sigma_b = {
            let tracker = self.state.belief_vol_tracker.read();
            tracker.get_sigma_b(slug)
        };
        let t_years = t_rem / (365.25 * 24.0 * 3600.0);

        let kelly = crate::vault::kelly_with_belief_vol(
            side_conf,
            side_price,
            sigma_b,
            t_years,
            &kelly_params,
        );

        if !kelly.should_trade {
            debug!(
                market_slug = %slug,
                skip_reason = ?kelly.skip_reason,
                sigma_b = sigma_b,
                "Vol-adjusted Kelly skip"
            );
            return Ok(None);
        }

        Ok(Some(OrderRequest {
            client_order_id: Uuid::new_v4().to_string(),
            token_id: side_token,
            side: OrderSide::Buy,
            price: side_price,
            notional_usdc: kelly.position_size_usd,
            tif: TimeInForce::Ioc,
            market_slug: Some(slug.to_string()),
            outcome: Some(side_outcome),
        }))
    }
}

/// Fetch best ask price from cache ONLY - returns None if should skip tick
/// This is the HFT-grade function that NEVER blocks on REST.
#[inline]
pub fn best_ask_cached_hft(state: &AppState, token_id: &str, max_stale_ms: i64) -> Option<f64> {
    state.polymarket_market_ws.request_subscribe(token_id);
    state
        .polymarket_market_ws
        .get_orderbook(token_id, max_stale_ms)
        .and_then(|book| book.asks.first().map(|o| o.price))
}

/// Fetch best bid price from cache ONLY - returns None if should skip tick
#[inline]
pub fn best_bid_cached_hft(state: &AppState, token_id: &str, max_stale_ms: i64) -> Option<f64> {
    state.polymarket_market_ws.request_subscribe(token_id);
    state
        .polymarket_market_ws
        .get_orderbook(token_id, max_stale_ms)
        .and_then(|book| book.bids.first().map(|o| o.price))
}

/// Fetch best ask price for a token, preferring WS cache with REST fallback
///
/// DEPRECATED for HFT paths: Use `best_ask_cached_hft` instead which never blocks.
/// This function should only be used in warmup phase or non-latency-critical paths.
pub async fn best_ask(state: &AppState, token_id: &str) -> Result<Option<f64>> {
    // Prefer ultra-fast WS cache.
    state.polymarket_market_ws.request_subscribe(token_id);
    if let Some(book) = state.polymarket_market_ws.get_orderbook(token_id, 1500) {
        return Ok(book.asks.first().map(|o| o.price));
    }

    // Fallback to REST snapshot - ONLY for non-HFT paths
    // TODO: Consider removing this fallback entirely after warmup is implemented
    let mut orderbook = state
        .http_client
        .get("https://clob.polymarket.com/book")
        .timeout(Duration::from_secs(3))
        .query(&[("token_id", token_id)])
        .send()
        .await?
        .error_for_status()?
        .json::<OrderBook>()
        .await?;

    orderbook.asks.sort_by(|a, b| {
        a.price
            .partial_cmp(&b.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(orderbook.asks.first().map(|o| o.price))
}
