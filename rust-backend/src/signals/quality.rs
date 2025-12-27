//! Signal Quality Gate
//!
//! Filters out stale and outlier signals using rolling statistics.
//! Uses Welford's online algorithm for numerically stable variance calculation.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use crate::models::{MarketSignal, SignalType};

const MIN_SAMPLE_SIZE: u64 = 30;

/// Maintains rolling statistics for signal families and filters out stale/outlier observations.
pub struct SignalQualityGate {
    stats: HashMap<&'static str, RollingStats>,
    stale_cutoff: chrono::Duration,
    zscore_threshold: f64,
}

impl SignalQualityGate {
    pub fn new(stale_cutoff: Duration, zscore_threshold: f64) -> Self {
        let stale_cutoff = chrono::Duration::from_std(stale_cutoff)
            .unwrap_or_else(|_| chrono::Duration::seconds(3));

        Self {
            stats: HashMap::new(),
            stale_cutoff,
            zscore_threshold,
        }
    }

    /// Filter a batch of signals, returning only those that pass freshness and outlier checks.
    #[inline]
    pub fn filter(&mut self, signals: Vec<MarketSignal>) -> Vec<MarketSignal> {
        if signals.is_empty() {
            return signals;
        }

        let now = Utc::now();

        // Build corroboration map (market_slug -> source count)
        // Use a simpler structure to avoid borrow issues
        let mut slug_sources: HashMap<String, HashSet<String>> =
            HashMap::with_capacity(signals.len());
        for signal in &signals {
            slug_sources
                .entry(signal.market_slug.clone())
                .or_default()
                .insert(signal.source.clone());
        }

        let mut accepted = Vec::with_capacity(signals.len());

        for signal in signals.into_iter() {
            if is_stale(&signal, now, self.stale_cutoff) {
                warn!(
                    market = %signal.market_slug,
                    source = %signal.source,
                    detected_at = %signal.detected_at,
                    "🛑 dropping stale signal (> {:?})",
                    self.stale_cutoff
                );
                continue;
            }

            let mut keep = true;
            if let Some((family, value)) = metric_for_signal(&signal) {
                let stats = self.stats.entry(family).or_default();
                if stats.count >= MIN_SAMPLE_SIZE {
                    let std_dev = stats.std_dev();
                    if std_dev > 0.0 {
                        let threshold = stats.mean + self.zscore_threshold * std_dev;
                        if value > threshold {
                            let corroborated = slug_sources
                                .get(&signal.market_slug)
                                .is_some_and(|sources| sources.len() >= 2);

                            if !corroborated {
                                warn!(
                                    market = %signal.market_slug,
                                    source = %signal.source,
                                    family = family,
                                    mean = stats.mean,
                                    std = std_dev,
                                    observed = value,
                                    "🛑 dropping >{:.1}σ outlier without corroboration",
                                    self.zscore_threshold
                                );
                                keep = false;
                            } else {
                                debug!(
                                    market = %signal.market_slug,
                                    source = %signal.source,
                                    family = family,
                                    "✅ retaining >{:.1}σ outlier (corroborated)",
                                    self.zscore_threshold
                                );
                            }
                        }
                    }
                }

                if keep {
                    stats.update(value);
                }
            }

            if keep {
                accepted.push(signal);
            }
        }

        accepted
    }
}

#[inline]
fn is_stale(signal: &MarketSignal, now: DateTime<Utc>, cutoff: chrono::Duration) -> bool {
    DateTime::parse_from_rfc3339(&signal.detected_at)
        .map(|detected| now - detected.with_timezone(&Utc) > cutoff)
        .unwrap_or(true)
}

#[inline]

fn metric_for_signal(signal: &MarketSignal) -> Option<(&'static str, f64)> {
    match &signal.signal_type {
        SignalType::PriceDeviation { deviation_pct, .. } => {
            Some(("price_deviation", *deviation_pct))
        }
        SignalType::MarketExpiryEdge { volume_spike, .. } => {
            Some(("expiry_edge_volume", *volume_spike))
        }
        SignalType::WhaleFollowing { position_size, .. } => {
            Some(("whale_following_size", *position_size))
        }
        SignalType::TrackedWalletEntry {
            position_value_usd, ..
        } => Some(("tracked_wallet_position", *position_value_usd)),
        SignalType::CrossPlatformArbitrage { spread_pct, .. } => {
            Some(("arbitrage_spread", *spread_pct))
        }
        _ => None,
    }
}

#[derive(Default)]
struct RollingStats {
    count: u64,
    mean: f64,
    m2: f64,
}

impl RollingStats {
    /// Welford's online algorithm for updating mean and variance
    #[inline]
    fn update(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }

    #[inline]
    fn std_dev(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            (self.m2 / (self.count - 1) as f64).sqrt()
        }
    }
}
