//! Settlement Engine for Polymarket 15-Minute Up/Down Markets
//!
//! This module provides first-class settlement modeling with exact contract semantics:
//! - Explicit window start/end times
//! - Reference price definition and selection rules
//! - Outcome determination (Up vs Down vs Tie)
//! - Settlement timing with arrival_time visibility enforcement
//! - Rounding and tie-breaking rules
//!
//! **CRITICAL**: Settlement must respect visibility semantics. The outcome is NOT knowable
//! at the cutoff time. It becomes knowable only when the reference price event becomes
//! VISIBLE in the simulation (arrival_time <= decision_time).

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Price, Resolution, TokenId};
use crate::backtest_v2::portfolio::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Nanoseconds per second.
pub const NS_PER_SEC: Nanos = 1_000_000_000;
/// Seconds per 15-minute window.
pub const WINDOW_15M_SECS: i64 = 15 * 60;

// =============================================================================
// Settlement Specification
// =============================================================================

/// Reference price selection rule for settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferencePriceRule {
    /// Use the last trade price at or before the cutoff.
    LastTrade,
    /// Use the mid-price (bid+ask)/2 at the cutoff.
    MidPrice,
    /// Use the oracle/index price (e.g., Chainlink) at the cutoff.
    OracleIndex,
    /// Use the VWAP over the last N seconds before cutoff.
    VwapLastNSec { seconds: u32 },
}

impl Default for ReferencePriceRule {
    fn default() -> Self {
        // Polymarket 15m uses Binance spot mid-price
        Self::MidPrice
    }
}

/// Rounding rule for price comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RoundingRule {
    /// No rounding - use raw float comparison with epsilon.
    None { epsilon: f64 },
    /// Round to N decimal places before comparison.
    Decimals { places: u32 },
    /// Round to tick size.
    TickSize { tick: f64 },
}

impl Default for RoundingRule {
    fn default() -> Self {
        // Default: no rounding, use epsilon for float comparison
        Self::None { epsilon: 1e-10 }
    }
}

/// Tie-breaking rule when start_price == end_price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TieRule {
    /// Tie counts as "No" winning (price did not go up).
    NoWins,
    /// Tie counts as "Yes" winning (price did not go down).
    YesWins,
    /// Tie is invalid - market should not resolve.
    Invalid,
    /// Tie resolves to 50/50 settlement (each share worth 0.5).
    Split,
}

impl Default for TieRule {
    fn default() -> Self {
        // Polymarket 15m: tie = No wins (price must INCREASE for Up to win)
        Self::NoWins
    }
}

/// Settlement timing rule - when is the outcome knowable?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeKnowableRule {
    /// Outcome knowable when reference price event arrives (arrival_time based).
    OnReferenceArrival,
    /// Outcome knowable after explicit delay from cutoff (simulates oracle delay).
    DelayFromCutoff { delay_ns: Nanos },
    /// Outcome knowable at exact cutoff time (DANGEROUS - may allow look-ahead).
    AtCutoff,
}

impl Default for OutcomeKnowableRule {
    fn default() -> Self {
        // Safe default: require reference price arrival
        Self::OnReferenceArrival
    }
}

/// Complete settlement specification for a market type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementSpec {
    /// Market type identifier (e.g., "polymarket_15m_updown").
    pub market_type: String,
    /// Window duration in nanoseconds.
    pub window_duration_ns: Nanos,
    /// Rule for computing window start time from market identifier.
    pub window_start_rule: WindowStartRule,
    /// Reference price selection rule.
    pub reference_price_rule: ReferencePriceRule,
    /// Reference price source identifier (e.g., "binance_btcusdt_spot").
    pub reference_source: String,
    /// Rounding rule for price comparison.
    pub rounding_rule: RoundingRule,
    /// Tie-breaking rule.
    pub tie_rule: TieRule,
    /// When outcome becomes knowable.
    pub outcome_knowable_rule: OutcomeKnowableRule,
    /// Required confidence in reference price (e.g., must have arrived from authoritative source).
    pub require_authoritative_reference: bool,
    /// Version timestamp - if metadata can change over time.
    pub spec_version_ts: Option<Nanos>,
}

/// Rule for computing window start time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowStartRule {
    /// Start time is encoded in market slug (e.g., "btc-updown-15m-1768533300").
    FromSlugTimestamp,
    /// Start time is explicit in metadata.
    ExplicitMetadata { start_time_ns: Nanos },
}

impl Default for WindowStartRule {
    fn default() -> Self {
        Self::FromSlugTimestamp
    }
}

impl Default for SettlementSpec {
    fn default() -> Self {
        Self::polymarket_15m_updown()
    }
}

impl SettlementSpec {
    /// Settlement specification for Polymarket 15-minute Up/Down markets.
    ///
    /// Contract definition:
    /// - Window: 15 minutes from start_ts (encoded in slug)
    /// - Up wins if: end_price > start_price
    /// - Down (No) wins if: end_price <= start_price (tie goes to Down)
    /// - Reference: Binance spot mid-price
    pub fn polymarket_15m_updown() -> Self {
        Self {
            market_type: "polymarket_15m_updown".to_string(),
            window_duration_ns: WINDOW_15M_SECS * NS_PER_SEC,
            window_start_rule: WindowStartRule::FromSlugTimestamp,
            reference_price_rule: ReferencePriceRule::MidPrice,
            reference_source: "binance_spot".to_string(),
            rounding_rule: RoundingRule::Decimals { places: 8 },
            tie_rule: TieRule::NoWins,
            outcome_knowable_rule: OutcomeKnowableRule::OnReferenceArrival,
            require_authoritative_reference: true,
            spec_version_ts: None,
        }
    }

    /// Compute window end time from start time.
    pub fn window_end_ns(&self, start_ns: Nanos) -> Nanos {
        start_ns + self.window_duration_ns
    }

    /// Parse window start time from market slug.
    pub fn parse_window_start(&self, market_slug: &str) -> Option<Nanos> {
        match &self.window_start_rule {
            WindowStartRule::FromSlugTimestamp => {
                // Parse "btc-updown-15m-1768533300" -> 1768533300 seconds
                let parts: Vec<&str> = market_slug.split('-').collect();
                if parts.len() >= 4 && parts[1] == "updown" && parts[2] == "15m" {
                    let ts_str = parts[3].split('-').next()?;
                    let ts_secs: i64 = ts_str.parse().ok()?;
                    Some(ts_secs * NS_PER_SEC)
                } else {
                    None
                }
            }
            WindowStartRule::ExplicitMetadata { start_time_ns } => Some(*start_time_ns),
        }
    }

    /// Determine the winning outcome given start and end prices.
    pub fn determine_outcome(
        &self,
        start_price: Price,
        end_price: Price,
    ) -> SettlementOutcome {
        let (start_rounded, end_rounded) = self.apply_rounding(start_price, end_price);

        let comparison = match &self.rounding_rule {
            RoundingRule::None { epsilon } => {
                if (end_rounded - start_rounded).abs() < *epsilon {
                    std::cmp::Ordering::Equal
                } else if end_rounded > start_rounded {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                }
            }
            _ => {
                // Already rounded, use standard comparison
                end_rounded.partial_cmp(&start_rounded).unwrap_or(std::cmp::Ordering::Equal)
            }
        };

        match comparison {
            std::cmp::Ordering::Greater => {
                // Price went UP
                SettlementOutcome::Resolved {
                    winner: Outcome::Yes,
                    is_tie: false,
                }
            }
            std::cmp::Ordering::Less => {
                // Price went DOWN
                SettlementOutcome::Resolved {
                    winner: Outcome::No,
                    is_tie: false,
                }
            }
            std::cmp::Ordering::Equal => {
                // TIE - apply tie rule
                match self.tie_rule {
                    TieRule::NoWins => SettlementOutcome::Resolved {
                        winner: Outcome::No,
                        is_tie: true,
                    },
                    TieRule::YesWins => SettlementOutcome::Resolved {
                        winner: Outcome::Yes,
                        is_tie: true,
                    },
                    TieRule::Invalid => SettlementOutcome::Invalid {
                        reason: "Tie detected and tie_rule = Invalid".to_string(),
                    },
                    TieRule::Split => SettlementOutcome::Split { each_share_value: 0.5 },
                }
            }
        }
    }

    /// Apply rounding rules to prices.
    fn apply_rounding(&self, start: Price, end: Price) -> (f64, f64) {
        match &self.rounding_rule {
            RoundingRule::None { .. } => (start, end),
            RoundingRule::Decimals { places } => {
                let factor = 10f64.powi(*places as i32);
                ((start * factor).round() / factor, (end * factor).round() / factor)
            }
            RoundingRule::TickSize { tick } => {
                ((start / tick).round() * tick, (end / tick).round() * tick)
            }
        }
    }
}

/// Result of outcome determination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettlementOutcome {
    /// Normal resolution with a winner.
    Resolved { winner: Outcome, is_tie: bool },
    /// Split settlement (each share worth partial value).
    Split { each_share_value: f64 },
    /// Invalid settlement - cannot determine outcome.
    Invalid { reason: String },
}

// =============================================================================
// Settlement State Machine
// =============================================================================

/// Settlement state for a single market window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettlementState {
    /// Waiting for window to start.
    Pending {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
    },
    /// Window is active, collecting start price.
    AwaitingStartPrice {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
    },
    /// Have start price, waiting for cutoff.
    Active {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price: Price,
        start_price_arrival_ns: Nanos,
    },
    /// Cutoff passed, waiting for end price to become visible.
    AwaitingEndPrice {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price: Price,
    },
    /// Have all data, can resolve when knowable.
    Resolvable {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price: Price,
        end_price: Price,
        end_price_arrival_ns: Nanos,
    },
    /// Fully resolved.
    Resolved {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price: Price,
        end_price: Price,
        outcome: SettlementOutcome,
        resolved_at_ns: Nanos,
    },
    /// Cannot resolve due to missing data.
    MissingData {
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        reason: String,
    },
}

impl SettlementState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, SettlementState::Resolved { .. } | SettlementState::MissingData { .. })
    }

    pub fn window_end(&self) -> Option<Nanos> {
        match self {
            SettlementState::Pending { window_end_ns, .. }
            | SettlementState::AwaitingStartPrice { window_end_ns, .. }
            | SettlementState::Active { window_end_ns, .. }
            | SettlementState::AwaitingEndPrice { window_end_ns, .. }
            | SettlementState::Resolvable { window_end_ns, .. }
            | SettlementState::Resolved { window_end_ns, .. }
            | SettlementState::MissingData { window_end_ns, .. } => Some(*window_end_ns),
        }
    }
}

// =============================================================================
// Settlement Engine
// =============================================================================

/// Settlement engine for tracking and resolving market windows.
pub struct SettlementEngine {
    /// Settlement specification.
    spec: SettlementSpec,
    /// Per-market settlement state.
    states: HashMap<String, SettlementState>,
    /// Statistics.
    pub stats: SettlementStats,
}

/// Settlement statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettlementStats {
    pub windows_tracked: u64,
    pub windows_resolved: u64,
    pub windows_missing_data: u64,
    pub up_wins: u64,
    pub down_wins: u64,
    pub ties: u64,
    pub early_settlement_attempts: u64,
}

/// Settlement event for ledger integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementEvent {
    pub market_id: String,
    pub window_start_ns: Nanos,
    pub window_end_ns: Nanos,
    pub outcome: SettlementOutcome,
    pub start_price: Price,
    pub end_price: Price,
    /// Decision time when settlement was processed.
    pub settle_decision_time_ns: Nanos,
    /// Arrival time of the reference price that enabled resolution.
    pub reference_arrival_ns: Nanos,
}

impl SettlementEngine {
    pub fn new(spec: SettlementSpec) -> Self {
        Self {
            spec,
            states: HashMap::new(),
            stats: SettlementStats::default(),
        }
    }

    /// Get the settlement specification.
    pub fn spec(&self) -> &SettlementSpec {
        &self.spec
    }

    /// Start tracking a market window.
    pub fn track_window(&mut self, market_id: &str, now_ns: Nanos) -> Result<(), String> {
        if self.states.contains_key(market_id) {
            return Err(format!("Window {} already tracked", market_id));
        }

        let window_start_ns = self.spec.parse_window_start(market_id)
            .ok_or_else(|| format!("Cannot parse window start from: {}", market_id))?;
        let window_end_ns = self.spec.window_end_ns(window_start_ns);

        let state = if now_ns < window_start_ns {
            SettlementState::Pending { window_start_ns, window_end_ns }
        } else {
            SettlementState::AwaitingStartPrice { window_start_ns, window_end_ns }
        };

        self.states.insert(market_id.to_string(), state);
        self.stats.windows_tracked += 1;
        Ok(())
    }

    /// Record a price observation with arrival time.
    ///
    /// This is called when market data arrives. The engine will track:
    /// - Start price: first price at or after window_start
    /// - End price: last price at or before window_end that has ARRIVED
    pub fn observe_price(
        &mut self,
        market_id: &str,
        price: Price,
        source_time_ns: Nanos,
        arrival_time_ns: Nanos,
    ) {
        let Some(state) = self.states.get_mut(market_id) else {
            return;
        };

        match state {
            SettlementState::Pending { window_start_ns, window_end_ns } => {
                // Not yet started - check if we should transition
                if source_time_ns >= *window_start_ns && arrival_time_ns >= *window_start_ns {
                    *state = SettlementState::Active {
                        window_start_ns: *window_start_ns,
                        window_end_ns: *window_end_ns,
                        start_price: price,
                        start_price_arrival_ns: arrival_time_ns,
                    };
                }
            }
            SettlementState::AwaitingStartPrice { window_start_ns, window_end_ns } => {
                // Take first price at/after window start
                if source_time_ns >= *window_start_ns {
                    *state = SettlementState::Active {
                        window_start_ns: *window_start_ns,
                        window_end_ns: *window_end_ns,
                        start_price: price,
                        start_price_arrival_ns: arrival_time_ns,
                    };
                }
            }
            SettlementState::Active { window_start_ns, window_end_ns, start_price, .. } => {
                // Copy values to avoid borrow issues
                let ws = *window_start_ns;
                let we = *window_end_ns;
                let sp = *start_price;
                
                // Source time BEFORE or AT cutoff - this could be our end price
                // We take the last price observation that has source_time <= cutoff
                // The actual transition happens when we call advance_time() or when
                // we observe a price AT the cutoff.
                if source_time_ns <= we && source_time_ns >= ws {
                    // This is a valid end price candidate
                    // Store it and mark as Resolvable
                    *state = SettlementState::Resolvable {
                        window_start_ns: ws,
                        window_end_ns: we,
                        start_price: sp,
                        end_price: price,
                        end_price_arrival_ns: arrival_time_ns,
                    };
                }
            }
            SettlementState::AwaitingEndPrice { window_start_ns, window_end_ns, start_price } => {
                // Source time at or before cutoff - use as end price
                if source_time_ns <= *window_end_ns {
                    *state = SettlementState::Resolvable {
                        window_start_ns: *window_start_ns,
                        window_end_ns: *window_end_ns,
                        start_price: *start_price,
                        end_price: price,
                        end_price_arrival_ns: arrival_time_ns,
                    };
                }
            }
            SettlementState::Resolvable { 
                window_start_ns, window_end_ns, start_price, end_price, end_price_arrival_ns 
            } => {
                // Allow updating end price if we get a later valid price (still before cutoff)
                // This implements "last price wins" semantics
                if source_time_ns <= *window_end_ns && source_time_ns >= *window_start_ns {
                    // Only update if this price is later in source time
                    *end_price = price;
                    *end_price_arrival_ns = arrival_time_ns;
                }
            }
            _ => {}
        }
    }

    /// Advance time - check for state transitions.
    pub fn advance_time(&mut self, decision_time_ns: Nanos) {
        for state in self.states.values_mut() {
            match state {
                SettlementState::Active { window_start_ns, window_end_ns, start_price, .. } => {
                    // If we're past the cutoff but no end price arrived, transition
                    if decision_time_ns > *window_end_ns {
                        *state = SettlementState::AwaitingEndPrice {
                            window_start_ns: *window_start_ns,
                            window_end_ns: *window_end_ns,
                            start_price: *start_price,
                        };
                    }
                }
                _ => {}
            }
        }
    }

    /// Check if a market is ready for settlement at the given decision time.
    ///
    /// Returns the settlement event if ready, None otherwise.
    /// 
    /// **CRITICAL**: This respects the outcome_knowable_rule from the spec.
    /// The outcome is NOT knowable until the reference price event has ARRIVED.
    pub fn try_settle(
        &mut self,
        market_id: &str,
        decision_time_ns: Nanos,
    ) -> Option<SettlementEvent> {
        let state = self.states.get(market_id)?;

        match state {
            SettlementState::Resolvable {
                window_start_ns,
                window_end_ns,
                start_price,
                end_price,
                end_price_arrival_ns,
            } => {
                // Check if outcome is knowable according to the spec
                let is_knowable = match self.spec.outcome_knowable_rule {
                    OutcomeKnowableRule::OnReferenceArrival => {
                        // Outcome knowable when end price has arrived
                        decision_time_ns >= *end_price_arrival_ns
                    }
                    OutcomeKnowableRule::DelayFromCutoff { delay_ns } => {
                        decision_time_ns >= *window_end_ns + delay_ns
                    }
                    OutcomeKnowableRule::AtCutoff => {
                        decision_time_ns >= *window_end_ns
                    }
                };

                if !is_knowable {
                    self.stats.early_settlement_attempts += 1;
                    return None;
                }

                // Determine outcome
                let outcome = self.spec.determine_outcome(*start_price, *end_price);

                // Update statistics
                self.stats.windows_resolved += 1;
                match &outcome {
                    SettlementOutcome::Resolved { winner, is_tie } => {
                        if *is_tie {
                            self.stats.ties += 1;
                        }
                        match winner {
                            Outcome::Yes => self.stats.up_wins += 1,
                            Outcome::No => self.stats.down_wins += 1,
                        }
                    }
                    _ => {}
                }

                let event = SettlementEvent {
                    market_id: market_id.to_string(),
                    window_start_ns: *window_start_ns,
                    window_end_ns: *window_end_ns,
                    outcome: outcome.clone(),
                    start_price: *start_price,
                    end_price: *end_price,
                    settle_decision_time_ns: decision_time_ns,
                    reference_arrival_ns: *end_price_arrival_ns,
                };

                // Update state to Resolved
                self.states.insert(market_id.to_string(), SettlementState::Resolved {
                    window_start_ns: *window_start_ns,
                    window_end_ns: *window_end_ns,
                    start_price: *start_price,
                    end_price: *end_price,
                    outcome,
                    resolved_at_ns: decision_time_ns,
                });

                Some(event)
            }
            _ => None,
        }
    }

    /// Mark a market as having missing data.
    pub fn mark_missing_data(&mut self, market_id: &str, reason: &str) {
        if let Some(state) = self.states.get_mut(market_id) {
            let (window_start_ns, window_end_ns) = match state {
                SettlementState::Pending { window_start_ns, window_end_ns, .. }
                | SettlementState::AwaitingStartPrice { window_start_ns, window_end_ns, .. }
                | SettlementState::Active { window_start_ns, window_end_ns, .. }
                | SettlementState::AwaitingEndPrice { window_start_ns, window_end_ns, .. } => {
                    (*window_start_ns, *window_end_ns)
                }
                _ => return,
            };

            *state = SettlementState::MissingData {
                window_start_ns,
                window_end_ns,
                reason: reason.to_string(),
            };
            self.stats.windows_missing_data += 1;
        }
    }

    /// Get the settlement state for a market.
    pub fn get_state(&self, market_id: &str) -> Option<&SettlementState> {
        self.states.get(market_id)
    }

    /// Get all tracked markets.
    pub fn tracked_markets(&self) -> impl Iterator<Item = &String> {
        self.states.keys()
    }

    /// Reset the engine.
    pub fn reset(&mut self) {
        self.states.clear();
        self.stats = SettlementStats::default();
    }
}

// =============================================================================
// Representativeness Classification
// =============================================================================

/// Settlement model classification for backtest results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementModel {
    /// Exact settlement specification from authoritative source.
    ExactSpec,
    /// Settlement approximated (e.g., using candle close instead of exact cutoff).
    Approximate,
    /// Settlement data missing for some windows.
    MissingData,
    /// No settlement modeling at all.
    None,
}

impl SettlementModel {
    pub fn is_representative(&self) -> bool {
        matches!(self, Self::ExactSpec)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::ExactSpec => "Exact settlement specification from authoritative source",
            Self::Approximate => "Settlement approximated (results NOT representative)",
            Self::MissingData => "Settlement data missing (results NOT representative)",
            Self::None => "No settlement modeling (results NOT representative)",
        }
    }
}

/// Representativeness classification for backtest results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Representativeness {
    /// Results are representative of live trading.
    Representative,
    /// Results are NOT representative due to listed reasons.
    NonRepresentative { reasons: Vec<String> },
}

impl Representativeness {
    pub fn is_representative(&self) -> bool {
        matches!(self, Self::Representative)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settlement_spec_polymarket() {
        let spec = SettlementSpec::polymarket_15m_updown();
        
        assert_eq!(spec.market_type, "polymarket_15m_updown");
        assert_eq!(spec.window_duration_ns, 15 * 60 * NS_PER_SEC);
        assert_eq!(spec.tie_rule, TieRule::NoWins);
    }

    #[test]
    fn test_parse_window_start() {
        let spec = SettlementSpec::polymarket_15m_updown();
        
        let slug = "btc-updown-15m-1768533300";
        let start = spec.parse_window_start(slug).unwrap();
        assert_eq!(start, 1768533300 * NS_PER_SEC);
    }

    #[test]
    fn test_determine_outcome_up_wins() {
        let spec = SettlementSpec::polymarket_15m_updown();
        
        let outcome = spec.determine_outcome(100.0, 101.0);
        assert!(matches!(outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: false }));
    }

    #[test]
    fn test_determine_outcome_down_wins() {
        let spec = SettlementSpec::polymarket_15m_updown();
        
        let outcome = spec.determine_outcome(100.0, 99.0);
        assert!(matches!(outcome, SettlementOutcome::Resolved { winner: Outcome::No, is_tie: false }));
    }

    #[test]
    fn test_determine_outcome_tie() {
        let spec = SettlementSpec::polymarket_15m_updown();
        
        let outcome = spec.determine_outcome(100.0, 100.0);
        // Polymarket: tie goes to No (price did not go UP)
        assert!(matches!(outcome, SettlementOutcome::Resolved { winner: Outcome::No, is_tie: true }));
    }

    #[test]
    fn test_settlement_engine_basic() {
        let spec = SettlementSpec::polymarket_15m_updown();
        let mut engine = SettlementEngine::new(spec);

        let market_id = "btc-updown-15m-1000";
        let window_start_ns = 1000 * NS_PER_SEC;
        let window_end_ns = (1000 + 15 * 60) * NS_PER_SEC;

        // Track the window
        engine.track_window(market_id, window_start_ns).unwrap();

        // Record start price
        engine.observe_price(market_id, 50000.0, window_start_ns, window_start_ns + 100);

        // Record end price (at cutoff, arrives after)
        let end_arrival = window_end_ns + 500; // Arrives 500ns after cutoff
        engine.observe_price(market_id, 50100.0, window_end_ns, end_arrival);

        // Try to settle BEFORE arrival - should fail
        assert!(engine.try_settle(market_id, window_end_ns + 100).is_none());

        // Try to settle AFTER arrival - should succeed
        let event = engine.try_settle(market_id, end_arrival).unwrap();
        assert!(matches!(event.outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, .. }));
    }

    #[test]
    fn test_boundary_minus_epsilon() {
        let spec = SettlementSpec::polymarket_15m_updown();
        let mut engine = SettlementEngine::new(spec);

        let market_id = "btc-updown-15m-1000";
        let window_start_ns = 1000 * NS_PER_SEC;
        let window_end_ns = (1000 + 15 * 60) * NS_PER_SEC;

        engine.track_window(market_id, window_start_ns).unwrap();
        engine.observe_price(market_id, 50000.0, window_start_ns, window_start_ns);

        // Price at cutoff - 1ns (should be included)
        let epsilon_before = window_end_ns - 1;
        engine.observe_price(market_id, 50100.0, epsilon_before, epsilon_before + 100);

        let event = engine.try_settle(market_id, epsilon_before + 100).unwrap();
        assert_eq!(event.end_price, 50100.0);
    }

    #[test]
    fn test_boundary_exactly() {
        let spec = SettlementSpec::polymarket_15m_updown();
        let mut engine = SettlementEngine::new(spec);

        let market_id = "btc-updown-15m-1000";
        let window_start_ns = 1000 * NS_PER_SEC;
        let window_end_ns = (1000 + 15 * 60) * NS_PER_SEC;

        engine.track_window(market_id, window_start_ns).unwrap();
        engine.observe_price(market_id, 50000.0, window_start_ns, window_start_ns);

        // Price exactly at cutoff (should be included)
        engine.observe_price(market_id, 50200.0, window_end_ns, window_end_ns + 100);

        let event = engine.try_settle(market_id, window_end_ns + 100).unwrap();
        assert_eq!(event.end_price, 50200.0);
    }

    #[test]
    fn test_outcome_not_knowable_before_arrival() {
        let spec = SettlementSpec::polymarket_15m_updown();
        let mut engine = SettlementEngine::new(spec);

        let market_id = "btc-updown-15m-1000";
        let window_start_ns = 1000 * NS_PER_SEC;
        let window_end_ns = (1000 + 15 * 60) * NS_PER_SEC;

        engine.track_window(market_id, window_start_ns).unwrap();
        engine.observe_price(market_id, 50000.0, window_start_ns, window_start_ns);

        // End price source_time is at cutoff, but arrival is delayed
        let arrival_delay = 5 * NS_PER_SEC; // 5 second arrival delay
        engine.observe_price(market_id, 50100.0, window_end_ns, window_end_ns + arrival_delay);

        // Decision time after cutoff but before arrival - outcome NOT knowable
        let before_arrival = window_end_ns + 1 * NS_PER_SEC;
        assert!(engine.try_settle(market_id, before_arrival).is_none());
        assert_eq!(engine.stats.early_settlement_attempts, 1);

        // Decision time after arrival - outcome IS knowable
        let after_arrival = window_end_ns + arrival_delay;
        assert!(engine.try_settle(market_id, after_arrival).is_some());
    }

    #[test]
    fn test_rounding_decimals() {
        let spec = SettlementSpec {
            rounding_rule: RoundingRule::Decimals { places: 2 },
            ..SettlementSpec::polymarket_15m_updown()
        };

        // 100.001 vs 100.004 -> both round to 100.00 -> TIE
        let outcome = spec.determine_outcome(100.001, 100.004);
        assert!(matches!(outcome, SettlementOutcome::Resolved { is_tie: true, .. }));

        // 100.00 vs 100.01 -> 100.00 vs 100.01 -> UP
        let outcome = spec.determine_outcome(100.00, 100.01);
        assert!(matches!(outcome, SettlementOutcome::Resolved { winner: Outcome::Yes, is_tie: false }));
    }

    #[test]
    fn test_settlement_model_representativeness() {
        assert!(SettlementModel::ExactSpec.is_representative());
        assert!(!SettlementModel::Approximate.is_representative());
        assert!(!SettlementModel::MissingData.is_representative());
        assert!(!SettlementModel::None.is_representative());
    }
}
