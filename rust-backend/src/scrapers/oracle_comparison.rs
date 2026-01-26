//! Oracle Comparison Module for 15m Up/Down Market Settlement
//!
//! Tracks and compares Chainlink oracle prices vs Binance spot prices at 15-minute
//! resolution windows to analyze settlement discrepancies.
//!
//! Key metrics:
//! - Agreement rate (both sources agree on up/down outcome)
//! - Price divergence at settlement time
//! - Historical rolling window (last 100 windows) + all-time stats

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Maximum rolling window size (100 most recent 15m windows)
const MAX_ROLLING_WINDOW: usize = 100;

/// Maximum historical records to keep (for all-time stats)
const MAX_HISTORICAL_RECORDS: usize = 10_000;

/// Assets supported for oracle comparison
pub const SUPPORTED_ASSETS: &[&str] = &["BTC", "ETH", "SOL", "XRP"];

/// A single 15-minute window resolution record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowResolution {
    /// Asset symbol (BTC, ETH, etc.)
    pub asset: String,
    /// Window start timestamp (Unix seconds)
    pub window_start_ts: i64,
    /// Window end timestamp (Unix seconds)
    pub window_end_ts: i64,
    /// Chainlink price at window start
    pub chainlink_start: Option<f64>,
    /// Chainlink price at window end (settlement price)
    pub chainlink_end: Option<f64>,
    /// Binance price at window start
    pub binance_start: Option<f64>,
    /// Binance price at window end
    pub binance_end: Option<f64>,
    /// Chainlink determined outcome: true = Up, false = Down, None = unknown
    pub chainlink_outcome: Option<bool>,
    /// Binance determined outcome
    pub binance_outcome: Option<bool>,
    /// Whether both sources agreed on the outcome
    pub agreed: Option<bool>,
    /// Price divergence at settlement (chainlink_end - binance_end) in USD
    pub divergence_usd: Option<f64>,
    /// Price divergence in basis points
    pub divergence_bps: Option<f64>,
    /// Timestamp when this record was created
    pub recorded_at: i64,
}

impl WindowResolution {
    pub fn new(asset: &str, window_start_ts: i64, window_end_ts: i64) -> Self {
        Self {
            asset: asset.to_uppercase(),
            window_start_ts,
            window_end_ts,
            chainlink_start: None,
            chainlink_end: None,
            binance_start: None,
            binance_end: None,
            chainlink_outcome: None,
            binance_outcome: None,
            agreed: None,
            divergence_usd: None,
            divergence_bps: None,
            recorded_at: Utc::now().timestamp(),
        }
    }

    /// Compute outcomes and agreement once all prices are recorded
    pub fn finalize(&mut self) {
        // Compute Chainlink outcome
        if let (Some(start), Some(end)) = (self.chainlink_start, self.chainlink_end) {
            self.chainlink_outcome = Some(end >= start);
        }

        // Compute Binance outcome
        if let (Some(start), Some(end)) = (self.binance_start, self.binance_end) {
            self.binance_outcome = Some(end >= start);
        }

        // Compute agreement
        if let (Some(cl), Some(bn)) = (self.chainlink_outcome, self.binance_outcome) {
            self.agreed = Some(cl == bn);
        }

        // Compute divergence
        if let (Some(cl_end), Some(bn_end)) = (self.chainlink_end, self.binance_end) {
            self.divergence_usd = Some(cl_end - bn_end);
            if cl_end > 0.0 {
                self.divergence_bps = Some(((cl_end - bn_end) / cl_end) * 10_000.0);
            }
        }
    }
}

/// Real-time price tick for display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceTick {
    pub ts: i64,
    pub chainlink_price: Option<f64>,
    pub binance_price: Option<f64>,
    pub divergence_bps: Option<f64>,
    /// Timestamp (Unix seconds) of the Chainlink price used for this tick
    pub chainlink_ts: Option<i64>,
    /// Timestamp (Unix seconds) of the Binance price used for this tick
    pub binance_ts: Option<i64>,
    /// How stale the Chainlink price is (now_ms - chainlink_ts_ms)
    pub chainlink_staleness_ms: Option<u64>,
    /// How stale the Binance price is (now_ms - binance_ts_ms)
    pub binance_staleness_ms: Option<u64>,
    /// Binance price sampled nearest to the Chainlink timestamp (for lag-adjusted comparisons)
    pub binance_price_at_chainlink_ts: Option<f64>,
    /// Signed skew between the matched Binance sample and Chainlink timestamp (binance_ts - chainlink_ts)
    pub binance_chainlink_skew_sec: Option<i64>,
    /// Divergence computed using Binance sampled at the Chainlink timestamp
    pub divergence_aligned_bps: Option<f64>,
    /// Chainlink update latency in microseconds (time since last update)
    pub chainlink_latency_us: Option<u64>,
    /// Binance update latency in microseconds (time since last update)
    pub binance_latency_us: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct TimedPrice {
    ts: i64,
    price: f64,
}

fn nearest_price(
    series: &VecDeque<TimedPrice>,
    target_ts: i64,
    max_skew_sec: i64,
) -> Option<TimedPrice> {
    let mut best: Option<TimedPrice> = None;
    let mut best_abs = i64::MAX;

    for p in series.iter() {
        let abs = (p.ts - target_ts).abs();
        if abs <= max_skew_sec && abs < best_abs {
            best_abs = abs;
            best = Some(*p);
        }
    }

    best
}

/// Per-asset tracking state
#[derive(Debug, Default)]
pub struct AssetTracker {
    /// Rolling window of recent resolutions
    pub rolling_window: VecDeque<WindowResolution>,
    /// All-time historical records
    pub historical: VecDeque<WindowResolution>,
    /// Current pending window (not yet resolved)
    pub pending_window: Option<WindowResolution>,
    /// Real-time price tick history (last ~5 minutes at 1Hz)
    pub price_ticks: VecDeque<PriceTick>,
    /// Recent Chainlink price points (for time-aligned comparisons)
    pub chainlink_points: VecDeque<TimedPrice>,
    /// Recent Binance price points (for time-aligned comparisons)
    pub binance_points: VecDeque<TimedPrice>,
    /// Last observed Chainlink price timestamp (Unix seconds, from oracle data)
    pub last_chainlink_ts: Option<i64>,
    /// Last observed Binance price timestamp (Unix seconds, from WS receive time)
    pub last_binance_ts: Option<i64>,
    /// Last Chainlink update timestamp (ms)
    pub last_chainlink_update_ms: Option<i64>,
    /// Last Binance update timestamp (ms)  
    pub last_binance_update_ms: Option<i64>,
    /// Chainlink update interval history (us)
    pub chainlink_intervals_us: VecDeque<u64>,
    /// Binance update interval history (us)
    pub binance_intervals_us: VecDeque<u64>,
}

impl AssetTracker {
    pub fn new() -> Self {
        Self {
            rolling_window: VecDeque::with_capacity(MAX_ROLLING_WINDOW + 1),
            historical: VecDeque::with_capacity(MAX_HISTORICAL_RECORDS + 1),
            pending_window: None,
            price_ticks: VecDeque::with_capacity(310), // ~5 min at 1Hz
            chainlink_points: VecDeque::with_capacity(310),
            binance_points: VecDeque::with_capacity(310),
            last_chainlink_ts: None,
            last_binance_ts: None,
            last_chainlink_update_ms: None,
            last_binance_update_ms: None,
            chainlink_intervals_us: VecDeque::with_capacity(100),
            binance_intervals_us: VecDeque::with_capacity(100),
        }
    }

    /// Add a completed resolution to the tracker
    pub fn add_resolution(&mut self, resolution: WindowResolution) {
        // Add to rolling window
        self.rolling_window.push_back(resolution.clone());
        while self.rolling_window.len() > MAX_ROLLING_WINDOW {
            self.rolling_window.pop_front();
        }

        // Add to historical
        self.historical.push_back(resolution);
        while self.historical.len() > MAX_HISTORICAL_RECORDS {
            self.historical.pop_front();
        }
    }

    /// Record a price tick with lag/staleness info.
    /// NOTE: We intentionally keep this ~1Hz by overwriting the last tick if it shares the same `ts`.
    pub fn record_tick(
        &mut self,
        chainlink: Option<f64>,
        binance: Option<f64>,
        chainlink_latency_us: Option<u64>,
        binance_latency_us: Option<u64>,
    ) {
        let now_s = Utc::now().timestamp();
        let now_ms = Utc::now().timestamp_millis();

        let chainlink_ts = self.last_chainlink_ts;
        let binance_ts = self.last_binance_ts;

        let chainlink_staleness_ms = chainlink_ts.map(|ts| {
            let ts_ms = ts * 1000;
            (now_ms - ts_ms).max(0) as u64
        });
        let binance_staleness_ms = binance_ts.map(|ts| {
            let ts_ms = ts * 1000;
            (now_ms - ts_ms).max(0) as u64
        });

        let divergence_bps = match (chainlink, binance) {
            (Some(cl), Some(bn)) if cl > 0.0 => Some(((cl - bn) / cl) * 10_000.0),
            _ => None,
        };

        // Lag-adjusted divergence: compare Chainlink price to Binance sampled near Chainlink's timestamp.
        let mut binance_price_at_chainlink_ts: Option<f64> = None;
        let mut binance_chainlink_skew_sec: Option<i64> = None;
        let divergence_aligned_bps = match (chainlink, chainlink_ts) {
            (Some(cl_price), Some(cl_ts)) if cl_price > 0.0 => {
                let matched = nearest_price(&self.binance_points, cl_ts, 10);
                if let Some(p) = matched {
                    binance_price_at_chainlink_ts = Some(p.price);
                    binance_chainlink_skew_sec = Some(p.ts - cl_ts);
                    Some(((cl_price - p.price) / cl_price) * 10_000.0)
                } else {
                    None
                }
            }
            _ => None,
        };

        let tick = PriceTick {
            ts: now_s,
            chainlink_price: chainlink,
            binance_price: binance,
            divergence_bps,
            chainlink_ts,
            binance_ts,
            chainlink_staleness_ms,
            binance_staleness_ms,
            binance_price_at_chainlink_ts,
            binance_chainlink_skew_sec,
            divergence_aligned_bps,
            chainlink_latency_us,
            binance_latency_us,
        };

        match self.price_ticks.back_mut() {
            Some(last) if last.ts == now_s => {
                *last = tick;
            }
            _ => {
                self.price_ticks.push_back(tick);
                while self.price_ticks.len() > 300 {
                    self.price_ticks.pop_front();
                }
            }
        }
    }

    /// Record Chainlink update and compute interval
    pub fn record_chainlink_update(&mut self, price: f64) -> Option<u64> {
        let now_ms = Utc::now().timestamp_millis();
        let interval_us = self
            .last_chainlink_update_ms
            .map(|last| ((now_ms - last).max(0) as u64) * 1000);
        self.last_chainlink_update_ms = Some(now_ms);

        if let Some(interval) = interval_us {
            self.chainlink_intervals_us.push_back(interval);
            while self.chainlink_intervals_us.len() > 100 {
                self.chainlink_intervals_us.pop_front();
            }
        }
        interval_us
    }

    /// Record Binance update and compute interval
    pub fn record_binance_update(&mut self, price: f64) -> Option<u64> {
        let now_ms = Utc::now().timestamp_millis();
        let interval_us = self
            .last_binance_update_ms
            .map(|last| ((now_ms - last).max(0) as u64) * 1000);
        self.last_binance_update_ms = Some(now_ms);

        if let Some(interval) = interval_us {
            self.binance_intervals_us.push_back(interval);
            while self.binance_intervals_us.len() > 100 {
                self.binance_intervals_us.pop_front();
            }
        }
        interval_us
    }

    /// Get average Chainlink update interval in us
    pub fn avg_chainlink_interval_us(&self) -> Option<u64> {
        if self.chainlink_intervals_us.is_empty() {
            return None;
        }
        let sum: u64 = self.chainlink_intervals_us.iter().sum();
        Some(sum / self.chainlink_intervals_us.len() as u64)
    }

    /// Get average Binance update interval in us
    pub fn avg_binance_interval_us(&self) -> Option<u64> {
        if self.binance_intervals_us.is_empty() {
            return None;
        }
        let sum: u64 = self.binance_intervals_us.iter().sum();
        Some(sum / self.binance_intervals_us.len() as u64)
    }

    /// Compute agreement stats for rolling window
    pub fn rolling_stats(&self) -> AgreementStats {
        compute_stats(self.rolling_window.iter())
    }

    /// Compute agreement stats for all-time historical
    pub fn all_time_stats(&self) -> AgreementStats {
        compute_stats(self.historical.iter())
    }
}

/// Agreement statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgreementStats {
    pub total_windows: usize,
    pub windows_with_data: usize,
    pub agreed_count: usize,
    pub disagreed_count: usize,
    pub agreement_rate: Option<f64>,
    pub avg_divergence_bps: Option<f64>,
    pub max_divergence_bps: Option<f64>,
    pub min_divergence_bps: Option<f64>,
}

fn compute_stats<'a>(iter: impl Iterator<Item = &'a WindowResolution>) -> AgreementStats {
    let mut total = 0usize;
    let mut with_data = 0usize;
    let mut agreed = 0usize;
    let mut disagreed = 0usize;
    let mut divergences: Vec<f64> = Vec::new();

    for r in iter {
        total += 1;
        if r.agreed.is_some() {
            with_data += 1;
            if r.agreed == Some(true) {
                agreed += 1;
            } else {
                disagreed += 1;
            }
        }
        if let Some(div) = r.divergence_bps {
            divergences.push(div);
        }
    }

    let agreement_rate = if with_data > 0 {
        Some((agreed as f64 / with_data as f64) * 100.0)
    } else {
        None
    };

    let (avg_div, max_div, min_div) = if !divergences.is_empty() {
        let sum: f64 = divergences.iter().sum();
        let avg = sum / divergences.len() as f64;
        let max = divergences
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        let min = divergences.iter().cloned().fold(f64::INFINITY, f64::min);
        (Some(avg), Some(max), Some(min))
    } else {
        (None, None, None)
    };

    AgreementStats {
        total_windows: total,
        windows_with_data: with_data,
        agreed_count: agreed,
        disagreed_count: disagreed,
        agreement_rate,
        avg_divergence_bps: avg_div,
        max_divergence_bps: max_div,
        min_divergence_bps: min_div,
    }
}

/// Main Oracle Comparison Tracker
pub struct OracleComparisonTracker {
    assets: RwLock<HashMap<String, AssetTracker>>,
    /// Track which windows are currently active
    active_windows: RwLock<HashMap<String, i64>>,
}

impl OracleComparisonTracker {
    pub fn new() -> Arc<Self> {
        let mut assets = HashMap::new();
        for asset in SUPPORTED_ASSETS {
            assets.insert(asset.to_uppercase(), AssetTracker::new());
        }

        Arc::new(Self {
            assets: RwLock::new(assets),
            active_windows: RwLock::new(HashMap::new()),
        })
    }

    /// Record Chainlink price update
    pub fn record_chainlink(&self, asset: &str, price: f64, ts: i64) {
        let asset = asset.to_uppercase();
        let mut assets = self.assets.write();

        if let Some(tracker) = assets.get_mut(&asset) {
            tracker.last_chainlink_ts = Some(ts);
            tracker.chainlink_points.push_back(TimedPrice { ts, price });
            while tracker.chainlink_points.len() > 300 {
                tracker.chainlink_points.pop_front();
            }

            // Update pending window if exists
            if let Some(ref mut pending) = tracker.pending_window {
                if pending.chainlink_start.is_none() {
                    pending.chainlink_start = Some(price);
                }
                // Always update end price
                pending.chainlink_end = Some(price);
            }

            // Record update interval
            let cl_latency = tracker.record_chainlink_update(price);
            let bn_latency = tracker
                .price_ticks
                .back()
                .and_then(|t| t.binance_latency_us);

            // Record tick
            let binance = tracker.binance_points.back().map(|p| p.price);
            tracker.record_tick(Some(price), binance, cl_latency, bn_latency);
        }
    }

    /// Record Binance price update
    pub fn record_binance(&self, asset: &str, price: f64, ts: i64) {
        let asset = asset.to_uppercase();
        let mut assets = self.assets.write();

        if let Some(tracker) = assets.get_mut(&asset) {
            tracker.last_binance_ts = Some(ts);
            tracker.binance_points.push_back(TimedPrice { ts, price });
            while tracker.binance_points.len() > 300 {
                tracker.binance_points.pop_front();
            }

            // Update pending window if exists
            if let Some(ref mut pending) = tracker.pending_window {
                if pending.binance_start.is_none() {
                    pending.binance_start = Some(price);
                }
                // Always update end price
                pending.binance_end = Some(price);
            }

            // Record update interval
            let bn_latency = tracker.record_binance_update(price);
            let cl_latency = tracker
                .price_ticks
                .back()
                .and_then(|t| t.chainlink_latency_us);

            // Record tick
            let chainlink = tracker.chainlink_points.back().map(|p| p.price);
            tracker.record_tick(chainlink, Some(price), cl_latency, bn_latency);
        }
    }

    /// Start a new 15m window for tracking
    pub fn start_window(&self, asset: &str, window_start_ts: i64, window_end_ts: i64) {
        let asset = asset.to_uppercase();

        // First finalize any pending window
        let _ = self.finalize_window(&asset);

        let mut assets = self.assets.write();
        let mut active = self.active_windows.write();

        if let Some(tracker) = assets.get_mut(&asset) {
            let mut pending = WindowResolution::new(&asset, window_start_ts, window_end_ts);

            // Pre-fill start prices from latest known points if they are close to the window boundary.
            // This avoids using the first post-boundary tick as the "start".
            const START_SKEW_SEC: i64 = 10;

            if let Some(last) = tracker.chainlink_points.back() {
                if (last.ts - window_start_ts).abs() <= START_SKEW_SEC {
                    pending.chainlink_start = Some(last.price);
                }
            }

            if let Some(last) = tracker.binance_points.back() {
                if (last.ts - window_start_ts).abs() <= START_SKEW_SEC {
                    pending.binance_start = Some(last.price);
                }
            }

            tracker.pending_window = Some(pending);
            debug!(
                "Started 15m window for {}: {} -> {}",
                asset, window_start_ts, window_end_ts
            );
            active.insert(asset, window_end_ts);
        }
    }

    /// Finalize a window when it ends
    pub fn finalize_window(&self, asset: &str) -> Option<WindowResolution> {
        let asset = asset.to_uppercase();
        let mut assets = self.assets.write();
        let mut active = self.active_windows.write();

        if let Some(tracker) = assets.get_mut(&asset) {
            if let Some(pending) = tracker.pending_window.take() {
                let mut finalized = pending;
                finalized.finalize();
                tracker.add_resolution(finalized.clone());
                active.remove(&asset);
                info!("Finalized 15m window for {}", asset);
                return Some(finalized);
            }
        }

        None
    }

    /// Check and auto-finalize expired windows
    pub fn check_and_finalize_expired(&self) -> Vec<WindowResolution> {
        let now = Utc::now().timestamp();
        let active = self.active_windows.read();
        let expired: Vec<String> = active
            .iter()
            .filter(|(_, &end_ts)| now >= end_ts)
            .map(|(asset, _)| asset.clone())
            .collect();
        drop(active);

        let mut finalized: Vec<WindowResolution> = Vec::new();
        for asset in expired {
            if let Some(r) = self.finalize_window(&asset) {
                finalized.push(r);
            }
        }

        finalized
    }

    /// Get rolling window resolutions for an asset
    pub fn get_rolling_window(&self, asset: &str) -> Vec<WindowResolution> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .map(|t| t.rolling_window.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get recent price ticks for an asset
    pub fn get_price_ticks(&self, asset: &str) -> Vec<PriceTick> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .map(|t| t.price_ticks.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get current divergence for an asset
    pub fn get_current_divergence(&self, asset: &str) -> Option<f64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.price_ticks.back())
            .and_then(|tick| tick.divergence_bps)
    }

    /// Get current divergence (lag-adjusted; Binance sampled near Chainlink timestamp)
    pub fn get_current_divergence_aligned(&self, asset: &str) -> Option<f64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.price_ticks.back())
            .and_then(|tick| tick.divergence_aligned_bps)
    }

    pub fn get_current_chainlink_staleness_ms(&self, asset: &str) -> Option<u64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.price_ticks.back())
            .and_then(|tick| tick.chainlink_staleness_ms)
    }

    pub fn get_current_binance_staleness_ms(&self, asset: &str) -> Option<u64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.price_ticks.back())
            .and_then(|tick| tick.binance_staleness_ms)
    }

    /// Get rolling stats for an asset
    pub fn get_rolling_stats(&self, asset: &str) -> AgreementStats {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .map(|t| t.rolling_stats())
            .unwrap_or_default()
    }

    /// Get all-time stats for an asset
    pub fn get_all_time_stats(&self, asset: &str) -> AgreementStats {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .map(|t| t.all_time_stats())
            .unwrap_or_default()
    }

    /// Get average Chainlink update interval for an asset
    pub fn get_avg_chainlink_interval_us(&self, asset: &str) -> Option<u64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.avg_chainlink_interval_us())
    }

    /// Get average Binance update interval for an asset
    pub fn get_avg_binance_interval_us(&self, asset: &str) -> Option<u64> {
        let assets = self.assets.read();
        assets
            .get(&asset.to_uppercase())
            .and_then(|t| t.avg_binance_interval_us())
    }

    /// Get combined stats for all assets
    pub fn get_combined_stats(&self) -> CombinedOracleStats {
        let assets = self.assets.read();

        let mut per_asset = HashMap::new();
        let mut all_rolling = Vec::new();
        let mut all_historical = Vec::new();

        for (asset, tracker) in assets.iter() {
            per_asset.insert(
                asset.clone(),
                AssetOracleStats {
                    rolling_stats: tracker.rolling_stats(),
                    all_time_stats: tracker.all_time_stats(),
                    rolling_window_count: tracker.rolling_window.len(),
                    historical_count: tracker.historical.len(),
                },
            );

            all_rolling.extend(tracker.rolling_window.iter().cloned());
            all_historical.extend(tracker.historical.iter().cloned());
        }

        CombinedOracleStats {
            per_asset,
            total_rolling_stats: compute_stats(all_rolling.iter()),
            total_all_time_stats: compute_stats(all_historical.iter()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetOracleStats {
    pub rolling_stats: AgreementStats,
    pub all_time_stats: AgreementStats,
    pub rolling_window_count: usize,
    pub historical_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedOracleStats {
    pub per_asset: HashMap<String, AssetOracleStats>,
    pub total_rolling_stats: AgreementStats,
    pub total_all_time_stats: AgreementStats,
}

/// Global tracker instance
static ORACLE_TRACKER: std::sync::OnceLock<Arc<OracleComparisonTracker>> =
    std::sync::OnceLock::new();

pub fn global_oracle_tracker() -> Arc<OracleComparisonTracker> {
    ORACLE_TRACKER
        .get_or_init(OracleComparisonTracker::new)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_resolution() {
        let mut res = WindowResolution::new("BTC", 1000, 1900);
        res.chainlink_start = Some(50000.0);
        res.chainlink_end = Some(50100.0);
        res.binance_start = Some(50010.0);
        res.binance_end = Some(50110.0);
        res.finalize();

        assert_eq!(res.chainlink_outcome, Some(true)); // Up
        assert_eq!(res.binance_outcome, Some(true)); // Up
        assert_eq!(res.agreed, Some(true));
    }

    #[test]
    fn test_disagreement() {
        let mut res = WindowResolution::new("ETH", 1000, 1900);
        res.chainlink_start = Some(3000.0);
        res.chainlink_end = Some(3001.0); // Up
        res.binance_start = Some(3000.0);
        res.binance_end = Some(2999.0); // Down
        res.finalize();

        assert_eq!(res.chainlink_outcome, Some(true));
        assert_eq!(res.binance_outcome, Some(false));
        assert_eq!(res.agreed, Some(false));
    }
}
