//! Data Normalization Layer
//!
//! Parsers for historical Polymarket data formats and conversion to canonical events.
//! Enforces data integrity checks with optional repair mode.

use crate::backtest_v2::clock::{parse_timestamp, Nanos, NANOS_PER_MILLI, NANOS_PER_SEC};
use crate::backtest_v2::events::{
    Event, Level, MarketStatus, Resolution, Side, TimestampedEvent, TokenId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

/// Raw Polymarket orderbook snapshot (from CLOB API or historical dumps).
#[derive(Debug, Clone, Deserialize)]
pub struct RawOrderBookSnapshot {
    #[serde(alias = "asset_id", alias = "tokenId")]
    pub token_id: String,
    pub bids: Vec<RawPriceLevel>,
    pub asks: Vec<RawPriceLevel>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, alias = "timestamp_iso")]
    pub timestamp_str: Option<String>,
    #[serde(default, alias = "sequence", alias = "seq")]
    pub exchange_seq: Option<u64>,
}

/// Raw price level (price/size as strings or numbers).
#[derive(Debug, Clone, Deserialize)]
pub struct RawPriceLevel {
    #[serde(deserialize_with = "deserialize_number_or_string")]
    pub price: f64,
    #[serde(deserialize_with = "deserialize_number_or_string")]
    pub size: f64,
    #[serde(default)]
    pub order_count: Option<u32>,
}

/// Raw orderbook delta (incremental update).
#[derive(Debug, Clone, Deserialize)]
pub struct RawOrderBookDelta {
    #[serde(alias = "asset_id", alias = "tokenId")]
    pub token_id: String,
    #[serde(default)]
    pub bids: Vec<RawPriceLevel>,
    #[serde(default)]
    pub asks: Vec<RawPriceLevel>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, alias = "timestamp_iso")]
    pub timestamp_str: Option<String>,
    #[serde(alias = "sequence", alias = "seq")]
    pub exchange_seq: u64,
}

/// Raw trade print.
#[derive(Debug, Clone, Deserialize)]
pub struct RawTrade {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(alias = "asset_id", alias = "tokenId", alias = "market")]
    pub token_id: String,
    #[serde(deserialize_with = "deserialize_number_or_string")]
    pub price: f64,
    #[serde(deserialize_with = "deserialize_number_or_string")]
    pub size: f64,
    pub side: String,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, alias = "timestamp_iso")]
    pub timestamp_str: Option<String>,
}

/// Raw market resolution event.
#[derive(Debug, Clone, Deserialize)]
pub struct RawResolution {
    #[serde(alias = "asset_id", alias = "tokenId", alias = "condition_id")]
    pub token_id: String,
    #[serde(alias = "winner", alias = "winning_outcome")]
    pub outcome: bool,
    #[serde(default = "default_settlement_price")]
    pub settlement_price: f64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, alias = "timestamp_iso")]
    pub timestamp_str: Option<String>,
}

fn default_settlement_price() -> f64 {
    1.0
}

/// Deserialize a number that may come as a string or number.
fn deserialize_number_or_string<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(f64),
    }

    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse().map_err(serde::de::Error::custom),
        StringOrNumber::Number(n) => Ok(n),
    }
}

/// Data integrity statistics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IntegrityStats {
    pub total_events: u64,
    pub valid_events: u64,
    pub sequence_gaps: u64,
    pub negative_sizes: u64,
    pub invalid_prices: u64,
    pub book_inconsistencies: u64,
    pub timestamp_issues: u64,
    pub repairs_applied: u64,
    pub snapshots_for_resync: u64,
}

impl IntegrityStats {
    pub fn defect_rate(&self) -> f64 {
        if self.total_events == 0 {
            return 0.0;
        }
        let defects = self.sequence_gaps
            + self.negative_sizes
            + self.invalid_prices
            + self.book_inconsistencies
            + self.timestamp_issues;
        defects as f64 / self.total_events as f64
    }
}

/// Configuration for the normalizer.
#[derive(Debug, Clone)]
pub struct NormalizerConfig {
    /// Enable repair mode (resync from snapshots on gaps).
    pub repair_mode: bool,
    /// Maximum sequence gap before triggering resync.
    pub max_seq_gap: u64,
    /// Drop events with invalid data instead of repairing.
    pub strict_mode: bool,
    /// Source identifier for events.
    pub source_id: u8,
}

impl Default for NormalizerConfig {
    fn default() -> Self {
        Self {
            repair_mode: true,
            max_seq_gap: 100,
            strict_mode: false,
            source_id: 1, // MarketData
        }
    }
}

/// Market data normalizer with integrity checking.
pub struct DataNormalizer {
    config: NormalizerConfig,
    stats: IntegrityStats,
    /// Last seen sequence number per token.
    last_seq: HashMap<TokenId, u64>,
    /// Last snapshot per token (for resync).
    last_snapshot: HashMap<TokenId, Event>,
    /// Last timestamp per token (for monotonicity check).
    last_timestamp: HashMap<TokenId, Nanos>,
    /// Pending resync tokens.
    pending_resync: HashMap<TokenId, bool>,
}

impl DataNormalizer {
    pub fn new(config: NormalizerConfig) -> Self {
        Self {
            config,
            stats: IntegrityStats::default(),
            last_seq: HashMap::new(),
            last_snapshot: HashMap::new(),
            last_timestamp: HashMap::new(),
            pending_resync: HashMap::new(),
        }
    }

    /// Get current integrity statistics.
    pub fn stats(&self) -> &IntegrityStats {
        &self.stats
    }

    /// Reset state for a new run.
    pub fn reset(&mut self) {
        self.stats = IntegrityStats::default();
        self.last_seq.clear();
        self.last_snapshot.clear();
        self.last_timestamp.clear();
        self.pending_resync.clear();
    }

    /// Parse and normalize a raw snapshot.
    pub fn normalize_snapshot(&mut self, raw: RawOrderBookSnapshot) -> Option<TimestampedEvent> {
        self.stats.total_events += 1;

        let timestamp = self.extract_timestamp(raw.timestamp, raw.timestamp_str.as_deref())?;
        let exchange_seq = raw.exchange_seq.unwrap_or(0);

        // Parse levels with validation
        let bids = self.parse_levels(&raw.bids, &raw.token_id, true)?;
        let asks = self.parse_levels(&raw.asks, &raw.token_id, false)?;

        // Validate book consistency
        if !self.validate_book_consistency(&bids, &asks) {
            self.stats.book_inconsistencies += 1;
            if self.config.strict_mode {
                return None;
            }
        }

        // Update state
        self.last_seq.insert(raw.token_id.clone(), exchange_seq);
        self.last_timestamp.insert(raw.token_id.clone(), timestamp);
        self.pending_resync.remove(&raw.token_id);

        let event = Event::L2BookSnapshot {
            token_id: raw.token_id.clone(),
            bids: bids.clone(),
            asks: asks.clone(),
            exchange_seq,
        };

        // Store for potential resync
        self.last_snapshot.insert(raw.token_id, event.clone());
        self.stats.valid_events += 1;

        Some(TimestampedEvent::new(
            timestamp,
            self.config.source_id,
            event,
        ))
    }

    /// Parse and normalize a raw delta.
    pub fn normalize_delta(&mut self, raw: RawOrderBookDelta) -> Option<TimestampedEvent> {
        self.stats.total_events += 1;

        let timestamp = self.extract_timestamp(raw.timestamp, raw.timestamp_str.as_deref())?;

        // Check sequence continuity
        if let Some(&last_seq) = self.last_seq.get(&raw.token_id) {
            let gap = raw.exchange_seq.saturating_sub(last_seq);
            if gap > 1 {
                self.stats.sequence_gaps += 1;
                if gap > self.config.max_seq_gap {
                    warn!(
                        token_id = %raw.token_id,
                        last_seq,
                        new_seq = raw.exchange_seq,
                        gap,
                        "Large sequence gap detected, requesting resync"
                    );
                    self.pending_resync.insert(raw.token_id.clone(), true);

                    if self.config.strict_mode {
                        return None;
                    }
                }
            }
        }

        // Check timestamp monotonicity
        if let Some(&last_ts) = self.last_timestamp.get(&raw.token_id) {
            if timestamp < last_ts {
                self.stats.timestamp_issues += 1;
                debug!(
                    token_id = %raw.token_id,
                    last_ts,
                    new_ts = timestamp,
                    "Non-monotonic timestamp in delta"
                );
                if self.config.strict_mode {
                    return None;
                }
            }
        }

        // Parse levels
        let bid_updates = self.parse_levels(&raw.bids, &raw.token_id, true)?;
        let ask_updates = self.parse_levels(&raw.asks, &raw.token_id, false)?;

        // Update state
        self.last_seq.insert(raw.token_id.clone(), raw.exchange_seq);
        self.last_timestamp.insert(raw.token_id.clone(), timestamp);

        let event = Event::L2Delta {
            token_id: raw.token_id,
            bid_updates,
            ask_updates,
            exchange_seq: raw.exchange_seq,
        };

        self.stats.valid_events += 1;
        Some(TimestampedEvent::new(
            timestamp,
            self.config.source_id,
            event,
        ))
    }

    /// Parse and normalize a raw trade.
    pub fn normalize_trade(&mut self, raw: RawTrade) -> Option<TimestampedEvent> {
        self.stats.total_events += 1;

        let timestamp = self.extract_timestamp(raw.timestamp, raw.timestamp_str.as_deref())?;

        // Validate price
        if !self.validate_price(raw.price) {
            self.stats.invalid_prices += 1;
            if self.config.strict_mode {
                return None;
            }
        }

        // Validate size
        if raw.size <= 0.0 {
            self.stats.negative_sizes += 1;
            if self.config.strict_mode {
                return None;
            }
            // In non-strict mode, skip the trade
            return None;
        }

        let aggressor_side = match raw.side.to_uppercase().as_str() {
            "BUY" | "B" => Side::Buy,
            "SELL" | "S" => Side::Sell,
            _ => {
                debug!(side = %raw.side, "Unknown trade side, defaulting to Buy");
                Side::Buy
            }
        };

        let event = Event::TradePrint {
            token_id: raw.token_id,
            price: raw.price,
            size: raw.size,
            aggressor_side,
            trade_id: raw.id,
        };

        self.stats.valid_events += 1;
        Some(TimestampedEvent::new(
            timestamp,
            self.config.source_id,
            event,
        ))
    }

    /// Parse and normalize a resolution event.
    pub fn normalize_resolution(&mut self, raw: RawResolution) -> Option<TimestampedEvent> {
        self.stats.total_events += 1;

        let timestamp = self.extract_timestamp(raw.timestamp, raw.timestamp_str.as_deref())?;

        let event = Event::ResolutionEvent {
            token_id: raw.token_id,
            resolution: Resolution {
                outcome: raw.outcome,
                settlement_price: raw.settlement_price.clamp(0.0, 1.0),
                source: raw.source,
            },
        };

        self.stats.valid_events += 1;
        Some(TimestampedEvent::new(
            timestamp,
            self.config.source_id,
            event,
        ))
    }

    /// Check if a token needs resync.
    pub fn needs_resync(&self, token_id: &str) -> bool {
        self.pending_resync.get(token_id).copied().unwrap_or(false)
    }

    /// Provide a snapshot for resync.
    pub fn provide_snapshot_for_resync(
        &mut self,
        snapshot: RawOrderBookSnapshot,
    ) -> Option<TimestampedEvent> {
        self.stats.snapshots_for_resync += 1;
        self.normalize_snapshot(snapshot)
    }

    // --- Private helpers ---

    fn extract_timestamp(&mut self, ts_millis: Option<i64>, ts_str: Option<&str>) -> Option<Nanos> {
        // Try milliseconds first
        if let Some(ms) = ts_millis {
            if ms > 0 {
                return Some(ms * NANOS_PER_MILLI);
            }
        }

        // Try ISO string
        if let Some(s) = ts_str {
            if let Some(nanos) = parse_timestamp(s) {
                return Some(nanos);
            }
        }

        self.stats.timestamp_issues += 1;
        if self.config.strict_mode {
            None
        } else {
            // Use epoch as fallback (will be sorted to beginning)
            Some(0)
        }
    }

    fn parse_levels(
        &mut self,
        raw_levels: &[RawPriceLevel],
        token_id: &str,
        is_bid: bool,
    ) -> Option<Vec<Level>> {
        let mut levels = Vec::with_capacity(raw_levels.len());

        for raw in raw_levels {
            // Validate price (Polymarket: 0.0 to 1.0)
            if !self.validate_price(raw.price) {
                self.stats.invalid_prices += 1;
                if self.config.strict_mode {
                    return None;
                }
                continue;
            }

            // Validate size (size=0 means removal in deltas, negative is invalid)
            if raw.size < 0.0 {
                self.stats.negative_sizes += 1;
                warn!(
                    token_id = %token_id,
                    price = raw.price,
                    size = raw.size,
                    side = if is_bid { "bid" } else { "ask" },
                    "Negative size detected"
                );
                if self.config.strict_mode {
                    return None;
                }
                continue;
            }

            levels.push(Level {
                price: raw.price,
                size: raw.size,
                order_count: raw.order_count,
            });
        }

        Some(levels)
    }

    fn validate_price(&self, price: f64) -> bool {
        // Polymarket prices are 0.0 to 1.0
        price >= 0.0 && price <= 1.0 && price.is_finite()
    }

    fn validate_book_consistency(&self, bids: &[Level], asks: &[Level]) -> bool {
        // Best bid should be less than best ask (no locked/crossed book)
        if let (Some(best_bid), Some(best_ask)) = (
            bids.iter()
                .map(|l| l.price)
                .max_by(|a, b| a.partial_cmp(b).unwrap()),
            asks.iter()
                .map(|l| l.price)
                .min_by(|a, b| a.partial_cmp(b).unwrap()),
        ) {
            if best_bid >= best_ask {
                debug!(best_bid, best_ask, "Crossed/locked book detected");
                return false;
            }
        }
        true
    }
}

/// Batch normalizer for processing large datasets.
pub struct BatchNormalizer {
    normalizer: DataNormalizer,
}

impl BatchNormalizer {
    pub fn new(config: NormalizerConfig) -> Self {
        Self {
            normalizer: DataNormalizer::new(config),
        }
    }

    /// Normalize a batch of snapshots.
    pub fn normalize_snapshots(
        &mut self,
        raws: Vec<RawOrderBookSnapshot>,
    ) -> Vec<TimestampedEvent> {
        raws.into_iter()
            .filter_map(|r| self.normalizer.normalize_snapshot(r))
            .collect()
    }

    /// Normalize a batch of deltas.
    pub fn normalize_deltas(&mut self, raws: Vec<RawOrderBookDelta>) -> Vec<TimestampedEvent> {
        raws.into_iter()
            .filter_map(|r| self.normalizer.normalize_delta(r))
            .collect()
    }

    /// Normalize a batch of trades.
    pub fn normalize_trades(&mut self, raws: Vec<RawTrade>) -> Vec<TimestampedEvent> {
        raws.into_iter()
            .filter_map(|r| self.normalizer.normalize_trade(r))
            .collect()
    }

    /// Get statistics.
    pub fn stats(&self) -> &IntegrityStats {
        self.normalizer.stats()
    }

    /// Reset for new run.
    pub fn reset(&mut self) {
        self.normalizer.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_snapshot() {
        let mut normalizer = DataNormalizer::new(NormalizerConfig::default());

        let raw = RawOrderBookSnapshot {
            token_id: "token123".into(),
            bids: vec![
                RawPriceLevel {
                    price: 0.45,
                    size: 100.0,
                    order_count: Some(5),
                },
                RawPriceLevel {
                    price: 0.44,
                    size: 200.0,
                    order_count: None,
                },
            ],
            asks: vec![RawPriceLevel {
                price: 0.55,
                size: 150.0,
                order_count: Some(3),
            }],
            timestamp: Some(1700000000000),
            timestamp_str: None,
            exchange_seq: Some(1),
        };

        let event = normalizer.normalize_snapshot(raw).unwrap();
        assert_eq!(event.time, 1700000000000 * NANOS_PER_MILLI);

        if let Event::L2BookSnapshot {
            token_id,
            bids,
            asks,
            exchange_seq,
        } = event.event
        {
            assert_eq!(token_id, "token123");
            assert_eq!(bids.len(), 2);
            assert_eq!(asks.len(), 1);
            assert_eq!(exchange_seq, 1);
        } else {
            panic!("Expected L2BookSnapshot");
        }
    }

    #[test]
    fn test_sequence_gap_detection() {
        let mut normalizer = DataNormalizer::new(NormalizerConfig {
            repair_mode: false,
            strict_mode: false,
            ..Default::default()
        });

        // First delta (seq=1)
        let delta1 = RawOrderBookDelta {
            token_id: "token123".into(),
            bids: vec![],
            asks: vec![],
            timestamp: Some(1700000000000),
            timestamp_str: None,
            exchange_seq: 1,
        };
        normalizer.normalize_delta(delta1);

        // Second delta with gap (seq=10)
        let delta2 = RawOrderBookDelta {
            token_id: "token123".into(),
            bids: vec![],
            asks: vec![],
            timestamp: Some(1700000001000),
            timestamp_str: None,
            exchange_seq: 10,
        };
        normalizer.normalize_delta(delta2);

        assert_eq!(normalizer.stats().sequence_gaps, 1);
    }

    #[test]
    fn test_negative_size_rejection() {
        let mut normalizer = DataNormalizer::new(NormalizerConfig {
            strict_mode: true,
            ..Default::default()
        });

        let raw = RawOrderBookSnapshot {
            token_id: "token123".into(),
            bids: vec![RawPriceLevel {
                price: 0.45,
                size: -100.0,
                order_count: None,
            }],
            asks: vec![],
            timestamp: Some(1700000000000),
            timestamp_str: None,
            exchange_seq: Some(1),
        };

        let result = normalizer.normalize_snapshot(raw);
        assert!(result.is_none());
        assert_eq!(normalizer.stats().negative_sizes, 1);
    }

    #[test]
    fn test_crossed_book_detection() {
        let mut normalizer = DataNormalizer::new(NormalizerConfig::default());

        let raw = RawOrderBookSnapshot {
            token_id: "token123".into(),
            bids: vec![
                RawPriceLevel {
                    price: 0.55,
                    size: 100.0,
                    order_count: None,
                }, // bid > ask
            ],
            asks: vec![RawPriceLevel {
                price: 0.50,
                size: 100.0,
                order_count: None,
            }],
            timestamp: Some(1700000000000),
            timestamp_str: None,
            exchange_seq: Some(1),
        };

        normalizer.normalize_snapshot(raw);
        assert_eq!(normalizer.stats().book_inconsistencies, 1);
    }

    #[test]
    fn test_trade_normalization() {
        let mut normalizer = DataNormalizer::new(NormalizerConfig::default());

        let raw = RawTrade {
            id: Some("trade123".into()),
            token_id: "token123".into(),
            price: 0.52,
            size: 500.0,
            side: "BUY".into(),
            timestamp: Some(1700000000000),
            timestamp_str: None,
        };

        let event = normalizer.normalize_trade(raw).unwrap();

        if let Event::TradePrint {
            token_id,
            price,
            size,
            aggressor_side,
            trade_id,
        } = event.event
        {
            assert_eq!(token_id, "token123");
            assert_eq!(price, 0.52);
            assert_eq!(size, 500.0);
            assert_eq!(aggressor_side, Side::Buy);
            assert_eq!(trade_id, Some("trade123".into()));
        } else {
            panic!("Expected TradePrint");
        }
    }

    #[test]
    fn test_defect_rate() {
        let mut stats = IntegrityStats::default();
        stats.total_events = 100;
        stats.sequence_gaps = 5;
        stats.negative_sizes = 2;
        stats.invalid_prices = 1;

        let rate = stats.defect_rate();
        assert!((rate - 0.08).abs() < 0.001);
    }
}
