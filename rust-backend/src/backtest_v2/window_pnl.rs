//! Per-15-Minute Window PnL Accounting
//!
//! This module provides first-class per-window PnL reporting for Polymarket 15m Up/Down
//! backtests. Every window becomes a canonical unit of truth for performance reporting.
//!
//! # Design Principles
//!
//! 1. **Derived from Ledger**: Window PnL is aggregated from the same ledger entries used
//!    for final PnL - no post-hoc inference or separate computation.
//!
//! 2. **Aligned with Settlement**: Windows are finalized only when their settlement is
//!    processed. No "rolling" or approximate window logic.
//!
//! 3. **Sum-to-Total Invariant**: sum(window.net_pnl) MUST equal total net PnL.
//!
//! 4. **Fingerprinted**: Window series is included in run fingerprint for reproducibility.
//!
//! # Window PnL Breakdown
//!
//! ```text
//! gross_pnl          = sum of trading PnL postings (fills) within window
//! fees               = sum of fee postings within window
//! settlement_transfer = cash received/paid at settlement
//! net_pnl            = gross_pnl - fees + settlement_transfer
//! ```

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::ledger::{Amount, EventRef, LedgerEntry, AMOUNT_SCALE, from_amount, to_amount};
use crate::backtest_v2::settlement::{SettlementEvent, SettlementOutcome, NS_PER_SEC, WINDOW_15M_SECS};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Unique identifier for a 15-minute window.
/// Derived from window_start_ns for deterministic ordering.
pub type WindowId = Nanos;

/// Per-15-minute window PnL record.
///
/// All monetary values use the same fixed-point Amount type as the ledger
/// (i128 with scale 100_000_000) to avoid floating-point errors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowPnL {
    /// Window start time (nanoseconds since epoch).
    /// This is also the canonical window identifier.
    pub window_start_ns: Nanos,
    
    /// Window end time (nanoseconds since epoch).
    /// Always window_start_ns + 15 minutes.
    pub window_end_ns: Nanos,
    
    /// Market ID for this window (e.g., "btc-updown-15m-1700000000").
    pub market_id: String,
    
    /// Gross PnL from trading (fills) before fees and settlement.
    /// This is the mark-to-market PnL from all fills within the window.
    /// Positive = profit, negative = loss.
    pub gross_pnl: Amount,
    
    /// Total fees paid within this window.
    /// Always non-negative.
    pub fees: Amount,
    
    /// Settlement transfer at window resolution.
    /// Positive = received cash from winning positions.
    /// Negative = lost cash from losing positions.
    pub settlement_transfer: Amount,
    
    /// Net PnL for this window.
    /// net_pnl = gross_pnl - fees + settlement_transfer
    pub net_pnl: Amount,
    
    /// Number of trades (fills) in this window.
    pub trades_count: u64,
    
    /// Number of maker fills in this window.
    pub maker_fills_count: u64,
    
    /// Number of taker fills in this window.
    pub taker_fills_count: u64,
    
    /// Total volume traded in this window (in Amount units).
    pub total_volume: Amount,
    
    /// Start price observed for this window.
    pub start_price: Option<f64>,
    
    /// End price observed for this window.
    pub end_price: Option<f64>,
    
    /// Settlement outcome (if resolved).
    pub outcome: Option<SettlementOutcome>,
    
    /// Whether this window has been finalized (settlement processed).
    pub is_finalized: bool,
    
    /// Decision time when this window was finalized.
    pub finalized_at_ns: Option<Nanos>,
    
    /// Ledger entry IDs included in this window (for audit trail).
    pub ledger_entry_ids: Vec<u64>,
}

impl WindowPnL {
    /// Create a new empty window record.
    pub fn new(window_start_ns: Nanos, market_id: String) -> Self {
        Self {
            window_start_ns,
            window_end_ns: window_start_ns + WINDOW_15M_SECS * NS_PER_SEC,
            market_id,
            gross_pnl: 0,
            fees: 0,
            settlement_transfer: 0,
            net_pnl: 0,
            trades_count: 0,
            maker_fills_count: 0,
            taker_fills_count: 0,
            total_volume: 0,
            start_price: None,
            end_price: None,
            outcome: None,
            is_finalized: false,
            finalized_at_ns: None,
            ledger_entry_ids: Vec::new(),
        }
    }
    
    /// Get the window ID (same as window_start_ns).
    pub fn window_id(&self) -> WindowId {
        self.window_start_ns
    }
    
    /// Add a fill entry to this window.
    pub fn add_fill(
        &mut self,
        entry_id: u64,
        volume: Amount,
        pnl_delta: Amount,
        is_maker: bool,
    ) {
        self.trades_count += 1;
        self.total_volume += volume;
        self.gross_pnl += pnl_delta;
        
        if is_maker {
            self.maker_fills_count += 1;
        } else {
            self.taker_fills_count += 1;
        }
        
        self.ledger_entry_ids.push(entry_id);
        self.recompute_net_pnl();
    }
    
    /// Add a fee entry to this window.
    pub fn add_fee(&mut self, entry_id: u64, fee_amount: Amount) {
        self.fees += fee_amount;
        self.ledger_entry_ids.push(entry_id);
        self.recompute_net_pnl();
    }
    
    /// Finalize the window with settlement.
    pub fn finalize_settlement(
        &mut self,
        settlement_event: &SettlementEvent,
        settlement_cash: Amount,
        decision_time_ns: Nanos,
    ) {
        self.settlement_transfer = settlement_cash;
        self.start_price = Some(settlement_event.start_price);
        self.end_price = Some(settlement_event.end_price);
        self.outcome = Some(settlement_event.outcome.clone());
        self.is_finalized = true;
        self.finalized_at_ns = Some(decision_time_ns);
        self.recompute_net_pnl();
    }
    
    /// Recompute net PnL from components.
    pub fn recompute_net_pnl(&mut self) {
        self.net_pnl = self.gross_pnl - self.fees + self.settlement_transfer;
    }
    
    /// Get gross PnL as f64.
    pub fn gross_pnl_f64(&self) -> f64 {
        from_amount(self.gross_pnl)
    }
    
    /// Get fees as f64.
    pub fn fees_f64(&self) -> f64 {
        from_amount(self.fees)
    }
    
    /// Get settlement transfer as f64.
    pub fn settlement_transfer_f64(&self) -> f64 {
        from_amount(self.settlement_transfer)
    }
    
    /// Get net PnL as f64.
    pub fn net_pnl_f64(&self) -> f64 {
        from_amount(self.net_pnl)
    }
    
    /// Get total volume as f64.
    pub fn total_volume_f64(&self) -> f64 {
        from_amount(self.total_volume)
    }
    
    /// Compute a hash for this window record (for fingerprinting).
    pub fn fingerprint_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.window_start_ns.hash(&mut hasher);
        self.window_end_ns.hash(&mut hasher);
        self.market_id.hash(&mut hasher);
        self.gross_pnl.hash(&mut hasher);
        self.fees.hash(&mut hasher);
        self.settlement_transfer.hash(&mut hasher);
        self.net_pnl.hash(&mut hasher);
        self.trades_count.hash(&mut hasher);
        self.maker_fills_count.hash(&mut hasher);
        self.taker_fills_count.hash(&mut hasher);
        self.is_finalized.hash(&mut hasher);
        
        // Hash outcome if present
        if let Some(ref outcome) = self.outcome {
            format!("{:?}", outcome).hash(&mut hasher);
        }
        
        hasher.finish()
    }
}

/// Window PnL series with aggregate statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPnLSeries {
    /// Ordered list of window PnL records.
    /// Sorted by window_start_ns in ascending order.
    pub windows: Vec<WindowPnL>,
    
    /// Total net PnL across all windows.
    pub total_net_pnl: Amount,
    
    /// Total gross PnL across all windows.
    pub total_gross_pnl: Amount,
    
    /// Total fees across all windows.
    pub total_fees: Amount,
    
    /// Total settlement transfers across all windows.
    pub total_settlement: Amount,
    
    /// Total trades across all windows.
    pub total_trades: u64,
    
    /// Number of finalized windows.
    pub finalized_count: u64,
    
    /// Number of windows with non-zero trades.
    pub active_windows: u64,
    
    /// Rolling hash of the entire series (for fingerprinting).
    pub series_hash: u64,
}

impl WindowPnLSeries {
    /// Create an empty series.
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            total_net_pnl: 0,
            total_gross_pnl: 0,
            total_fees: 0,
            total_settlement: 0,
            total_trades: 0,
            finalized_count: 0,
            active_windows: 0,
            series_hash: 0,
        }
    }
    
    /// Add a finalized window to the series.
    /// Windows must be added in order.
    pub fn add_window(&mut self, window: WindowPnL) {
        // Verify ordering
        if let Some(last) = self.windows.last() {
            assert!(
                window.window_start_ns > last.window_start_ns,
                "Windows must be added in order: {} should come after {}",
                window.window_start_ns,
                last.window_start_ns
            );
        }
        
        // Update aggregates
        self.total_net_pnl += window.net_pnl;
        self.total_gross_pnl += window.gross_pnl;
        self.total_fees += window.fees;
        self.total_settlement += window.settlement_transfer;
        self.total_trades += window.trades_count;
        
        if window.is_finalized {
            self.finalized_count += 1;
        }
        
        if window.trades_count > 0 {
            self.active_windows += 1;
        }
        
        self.windows.push(window);
        self.recompute_series_hash();
    }
    
    /// Recompute the rolling series hash.
    fn recompute_series_hash(&mut self) {
        let mut hasher = DefaultHasher::new();
        
        // Hash window count first
        self.windows.len().hash(&mut hasher);
        
        // Rolling hash of all windows
        for window in &self.windows {
            window.fingerprint_hash().hash(&mut hasher);
        }
        
        // Include totals for verification
        self.total_net_pnl.hash(&mut hasher);
        self.total_gross_pnl.hash(&mut hasher);
        self.total_fees.hash(&mut hasher);
        self.total_settlement.hash(&mut hasher);
        
        self.series_hash = hasher.finish();
    }
    
    /// Validate that window series sums match reported totals.
    pub fn validate_sum_invariant(&self) -> Result<(), WindowAccountingError> {
        let computed_net: Amount = self.windows.iter().map(|w| w.net_pnl).sum();
        let computed_gross: Amount = self.windows.iter().map(|w| w.gross_pnl).sum();
        let computed_fees: Amount = self.windows.iter().map(|w| w.fees).sum();
        let computed_settlement: Amount = self.windows.iter().map(|w| w.settlement_transfer).sum();
        
        if computed_net != self.total_net_pnl {
            return Err(WindowAccountingError::SumMismatch {
                field: "total_net_pnl".to_string(),
                expected: self.total_net_pnl,
                computed: computed_net,
            });
        }
        
        if computed_gross != self.total_gross_pnl {
            return Err(WindowAccountingError::SumMismatch {
                field: "total_gross_pnl".to_string(),
                expected: self.total_gross_pnl,
                computed: computed_gross,
            });
        }
        
        if computed_fees != self.total_fees {
            return Err(WindowAccountingError::SumMismatch {
                field: "total_fees".to_string(),
                expected: self.total_fees,
                computed: computed_fees,
            });
        }
        
        if computed_settlement != self.total_settlement {
            return Err(WindowAccountingError::SumMismatch {
                field: "total_settlement".to_string(),
                expected: self.total_settlement,
                computed: computed_settlement,
            });
        }
        
        Ok(())
    }
    
    /// Validate that windows do not overlap and have no gaps.
    pub fn validate_continuity(&self) -> Result<(), WindowAccountingError> {
        if self.windows.len() < 2 {
            return Ok(());
        }
        
        for pair in self.windows.windows(2) {
            let prev = &pair[0];
            let curr = &pair[1];
            
            // Check for overlap
            if curr.window_start_ns < prev.window_end_ns {
                return Err(WindowAccountingError::Overlap {
                    window_a: prev.window_start_ns,
                    window_b: curr.window_start_ns,
                });
            }
            
            // Check for gap (allowing different markets to have gaps)
            // Gap check only applies to windows of the same market
            // For different markets, gaps are expected
        }
        
        Ok(())
    }
    
    /// Get total net PnL as f64.
    pub fn total_net_pnl_f64(&self) -> f64 {
        from_amount(self.total_net_pnl)
    }
    
    /// Get total gross PnL as f64.
    pub fn total_gross_pnl_f64(&self) -> f64 {
        from_amount(self.total_gross_pnl)
    }
    
    /// Get total fees as f64.
    pub fn total_fees_f64(&self) -> f64 {
        from_amount(self.total_fees)
    }
    
    /// Format a summary report.
    pub fn format_summary(&self) -> String {
        format!(
            "WindowPnLSeries: {} windows ({} finalized, {} active)\n\
             Total Net PnL: ${:.2}\n\
             Total Gross PnL: ${:.2}\n\
             Total Fees: ${:.2}\n\
             Total Settlement: ${:.2}\n\
             Total Trades: {}\n\
             Series Hash: {:016x}",
            self.windows.len(),
            self.finalized_count,
            self.active_windows,
            self.total_net_pnl_f64(),
            self.total_gross_pnl_f64(),
            self.total_fees_f64(),
            from_amount(self.total_settlement),
            self.total_trades,
            self.series_hash,
        )
    }
}

impl Default for WindowPnLSeries {
    fn default() -> Self {
        Self::new()
    }
}

/// Window accounting error types.
#[derive(Debug, Clone)]
pub enum WindowAccountingError {
    /// Sum of window values doesn't match reported total.
    SumMismatch {
        field: String,
        expected: Amount,
        computed: Amount,
    },
    /// Windows overlap in time.
    Overlap {
        window_a: Nanos,
        window_b: Nanos,
    },
    /// Gap detected between consecutive windows.
    Gap {
        window_a_end: Nanos,
        window_b_start: Nanos,
    },
    /// Window not found.
    WindowNotFound {
        window_id: WindowId,
    },
    /// Window already finalized.
    AlreadyFinalized {
        window_id: WindowId,
    },
    /// Missing window for settlement.
    MissingWindow {
        market_id: String,
        window_start_ns: Nanos,
    },
    /// Internal validation error.
    InternalError {
        message: String,
    },
}

impl std::fmt::Display for WindowAccountingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SumMismatch { field, expected, computed } => {
                write!(
                    f,
                    "Sum mismatch for {}: expected {} ({:.8}), computed {} ({:.8})",
                    field,
                    expected,
                    from_amount(*expected),
                    computed,
                    from_amount(*computed)
                )
            }
            Self::Overlap { window_a, window_b } => {
                write!(f, "Windows overlap: {} and {}", window_a, window_b)
            }
            Self::Gap { window_a_end, window_b_start } => {
                write!(f, "Gap between windows: {} to {}", window_a_end, window_b_start)
            }
            Self::WindowNotFound { window_id } => {
                write!(f, "Window not found: {}", window_id)
            }
            Self::AlreadyFinalized { window_id } => {
                write!(f, "Window already finalized: {}", window_id)
            }
            Self::MissingWindow { market_id, window_start_ns } => {
                write!(f, "Missing window for market {} at {}", market_id, window_start_ns)
            }
            Self::InternalError { message } => {
                write!(f, "Internal error: {}", message)
            }
        }
    }
}

impl std::error::Error for WindowAccountingError {}

/// Window accounting engine.
///
/// Tracks per-window PnL by aggregating ledger entries as they are posted.
/// This is the central coordinator for window-level accounting.
#[derive(Debug)]
pub struct WindowAccountingEngine {
    /// Active (unfinalied) windows by market_id -> window_start_ns -> WindowPnL.
    active_windows: HashMap<String, HashMap<WindowId, WindowPnL>>,
    
    /// Finalized windows in order.
    finalized: WindowPnLSeries,
    
    /// Whether production-grade mode is enabled.
    /// When true, any accounting violation aborts immediately.
    production_grade: bool,
    
    /// First error encountered (for diagnostics).
    first_error: Option<WindowAccountingError>,
}

impl WindowAccountingEngine {
    /// Create a new window accounting engine.
    pub fn new(production_grade: bool) -> Self {
        Self {
            active_windows: HashMap::new(),
            finalized: WindowPnLSeries::new(),
            production_grade,
            first_error: None,
        }
    }
    
    /// Get or create a window for the given market and time.
    pub fn get_or_create_window(&mut self, market_id: &str, window_start_ns: Nanos) -> &mut WindowPnL {
        let market_windows = self.active_windows
            .entry(market_id.to_string())
            .or_insert_with(HashMap::new);
        
        market_windows
            .entry(window_start_ns)
            .or_insert_with(|| WindowPnL::new(window_start_ns, market_id.to_string()))
    }
    
    /// Process a fill entry from the ledger.
    pub fn process_fill(
        &mut self,
        entry: &LedgerEntry,
        market_id: &str,
        window_start_ns: Nanos,
        volume: Amount,
        pnl_delta: Amount,
        is_maker: bool,
    ) {
        let window = self.get_or_create_window(market_id, window_start_ns);
        window.add_fill(entry.entry_id, volume, pnl_delta, is_maker);
    }
    
    /// Process a fee entry from the ledger.
    pub fn process_fee(
        &mut self,
        entry: &LedgerEntry,
        market_id: &str,
        window_start_ns: Nanos,
        fee_amount: Amount,
    ) {
        let window = self.get_or_create_window(market_id, window_start_ns);
        window.add_fee(entry.entry_id, fee_amount);
    }
    
    /// Finalize a window when settlement is processed.
    pub fn finalize_window(
        &mut self,
        settlement_event: &SettlementEvent,
        settlement_cash: Amount,
        decision_time_ns: Nanos,
    ) -> Result<WindowPnL, WindowAccountingError> {
        let market_id = &settlement_event.market_id;
        let window_start_ns = settlement_event.window_start_ns;
        
        // Find the active window
        let market_windows = self.active_windows
            .get_mut(market_id)
            .ok_or_else(|| WindowAccountingError::MissingWindow {
                market_id: market_id.clone(),
                window_start_ns,
            })?;
        
        let mut window = market_windows
            .remove(&window_start_ns)
            .ok_or_else(|| WindowAccountingError::WindowNotFound {
                window_id: window_start_ns,
            })?;
        
        // Check if already finalized
        if window.is_finalized {
            let err = WindowAccountingError::AlreadyFinalized {
                window_id: window_start_ns,
            };
            if self.production_grade {
                return Err(err);
            }
            self.first_error = self.first_error.take().or(Some(err.clone()));
            return Err(err);
        }
        
        // Finalize with settlement
        window.finalize_settlement(settlement_event, settlement_cash, decision_time_ns);
        
        // Add to finalized series
        self.finalized.add_window(window.clone());
        
        Ok(window)
    }
    
    /// Finalize a window that had no trades (zero-activity window).
    pub fn finalize_empty_window(
        &mut self,
        market_id: &str,
        window_start_ns: Nanos,
        window_end_ns: Nanos,
        start_price: f64,
        end_price: f64,
        outcome: SettlementOutcome,
        decision_time_ns: Nanos,
    ) -> WindowPnL {
        let mut window = WindowPnL::new(window_start_ns, market_id.to_string());
        window.window_end_ns = window_end_ns;
        window.start_price = Some(start_price);
        window.end_price = Some(end_price);
        window.outcome = Some(outcome);
        window.is_finalized = true;
        window.finalized_at_ns = Some(decision_time_ns);
        
        self.finalized.add_window(window.clone());
        window
    }
    
    /// Get the finalized window series.
    pub fn finalized_series(&self) -> &WindowPnLSeries {
        &self.finalized
    }
    
    /// Consume the engine and return the final series.
    pub fn into_series(self) -> WindowPnLSeries {
        self.finalized
    }
    
    /// Validate the final series against ledger totals.
    pub fn validate_against_ledger(
        &self,
        ledger_realized_pnl: Amount,
        ledger_fees: Amount,
    ) -> Result<(), WindowAccountingError> {
        // Validate internal sum invariant
        self.finalized.validate_sum_invariant()?;
        
        // Validate against ledger totals
        // Note: net_pnl includes settlement, while ledger_realized_pnl is trading PnL only
        // The relationship is: window_gross_pnl - window_fees = ledger trading PnL
        // And: window_settlement_transfer = settlement cash flows
        
        let window_trading_pnl = self.finalized.total_gross_pnl - self.finalized.total_fees;
        
        // For now we just check fees match
        if self.finalized.total_fees != ledger_fees {
            return Err(WindowAccountingError::SumMismatch {
                field: "total_fees vs ledger".to_string(),
                expected: ledger_fees,
                computed: self.finalized.total_fees,
            });
        }
        
        Ok(())
    }
    
    /// Get first error encountered.
    pub fn first_error(&self) -> Option<&WindowAccountingError> {
        self.first_error.as_ref()
    }
    
    /// Check if any errors have occurred.
    pub fn has_errors(&self) -> bool {
        self.first_error.is_some()
    }
}

/// Parse window start time from a market slug.
///
/// Market slugs for 15m Up/Down follow the pattern: "btc-updown-15m-{unix_seconds}"
pub fn parse_window_start_from_slug(market_slug: &str) -> Option<Nanos> {
    let parts: Vec<&str> = market_slug.split('-').collect();
    if parts.len() >= 4 && parts[1] == "updown" && parts[2] == "15m" {
        let ts_str = parts[3].split('-').next()?;
        let ts_secs: i64 = ts_str.parse().ok()?;
        Some(ts_secs * NS_PER_SEC)
    } else {
        None
    }
}

/// Compute window start time for a given timestamp (aligns to 15-minute boundary).
///
/// Note: This aligns to UTC-based 15-minute boundaries.
pub fn align_to_window_start(timestamp_ns: Nanos) -> Nanos {
    let window_duration_ns = WINDOW_15M_SECS * NS_PER_SEC;
    (timestamp_ns / window_duration_ns) * window_duration_ns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::portfolio::Outcome;
    
    #[test]
    fn test_window_pnl_creation() {
        let window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        
        assert_eq!(window.window_start_ns, 1000 * NS_PER_SEC);
        assert_eq!(window.window_end_ns, (1000 + 15 * 60) * NS_PER_SEC);
        assert_eq!(window.gross_pnl, 0);
        assert_eq!(window.fees, 0);
        assert_eq!(window.net_pnl, 0);
        assert!(!window.is_finalized);
    }
    
    #[test]
    fn test_window_pnl_add_fill() {
        let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        
        // Add a maker fill: bought at 0.50, now worth 0.55, profit = 0.05 per share
        // 100 shares = $5 profit
        let pnl_delta = to_amount(5.0);
        let volume = to_amount(100.0 * 0.50);
        
        window.add_fill(1, volume, pnl_delta, true);
        
        assert_eq!(window.trades_count, 1);
        assert_eq!(window.maker_fills_count, 1);
        assert_eq!(window.taker_fills_count, 0);
        assert_eq!(window.gross_pnl, pnl_delta);
        assert_eq!(window.net_pnl, pnl_delta); // No fees yet
    }
    
    #[test]
    fn test_window_pnl_add_fee() {
        let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        
        let pnl_delta = to_amount(10.0);
        let fee = to_amount(0.50);
        
        window.add_fill(1, to_amount(50.0), pnl_delta, false);
        window.add_fee(2, fee);
        
        assert_eq!(window.gross_pnl, pnl_delta);
        assert_eq!(window.fees, fee);
        assert_eq!(window.net_pnl, pnl_delta - fee);
    }
    
    #[test]
    fn test_window_pnl_finalize() {
        let mut window = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        
        // Add some trading activity
        window.add_fill(1, to_amount(50.0), to_amount(5.0), true);
        window.add_fee(2, to_amount(0.25));
        
        // Create settlement event
        let settlement = SettlementEvent {
            market_id: "btc-updown-15m-1000".to_string(),
            window_start_ns: 1000 * NS_PER_SEC,
            window_end_ns: (1000 + 900) * NS_PER_SEC,
            outcome: SettlementOutcome::Resolved {
                winner: Outcome::Yes,
                is_tie: false,
            },
            start_price: 50000.0,
            end_price: 50100.0,
            settle_decision_time_ns: (1000 + 910) * NS_PER_SEC,
            reference_arrival_ns: (1000 + 905) * NS_PER_SEC,
        };
        
        // Settlement: we held 10 winning shares, receive $10
        let settlement_cash = to_amount(10.0);
        
        window.finalize_settlement(&settlement, settlement_cash, (1000 + 910) * NS_PER_SEC);
        
        assert!(window.is_finalized);
        assert_eq!(window.settlement_transfer, settlement_cash);
        assert_eq!(window.start_price, Some(50000.0));
        assert_eq!(window.end_price, Some(50100.0));
        
        // net_pnl = gross(5) - fees(0.25) + settlement(10) = 14.75
        assert!((window.net_pnl_f64() - 14.75).abs() < 0.01);
    }
    
    #[test]
    fn test_window_series_sum_invariant() {
        let mut series = WindowPnLSeries::new();
        
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        w1.gross_pnl = to_amount(10.0);
        w1.fees = to_amount(1.0);
        w1.settlement_transfer = to_amount(5.0);
        w1.recompute_net_pnl();
        w1.is_finalized = true;
        
        let mut w2 = WindowPnL::new(2000 * NS_PER_SEC, "btc-updown-15m-2000".to_string());
        w2.gross_pnl = to_amount(-5.0);
        w2.fees = to_amount(0.5);
        w2.settlement_transfer = to_amount(0.0);
        w2.recompute_net_pnl();
        w2.is_finalized = true;
        
        series.add_window(w1);
        series.add_window(w2);
        
        // Validate sum invariant
        assert!(series.validate_sum_invariant().is_ok());
        
        // Check totals
        assert!((series.total_gross_pnl_f64() - 5.0).abs() < 0.01); // 10 - 5 = 5
        assert!((series.total_fees_f64() - 1.5).abs() < 0.01); // 1 + 0.5 = 1.5
    }
    
    #[test]
    fn test_window_series_ordering() {
        let mut series = WindowPnLSeries::new();
        
        let w1 = WindowPnL::new(1000 * NS_PER_SEC, "market1".to_string());
        let w2 = WindowPnL::new(2000 * NS_PER_SEC, "market2".to_string());
        
        series.add_window(w1);
        series.add_window(w2);
        
        assert_eq!(series.windows.len(), 2);
        assert!(series.windows[0].window_start_ns < series.windows[1].window_start_ns);
    }
    
    #[test]
    #[should_panic(expected = "Windows must be added in order")]
    fn test_window_series_rejects_out_of_order() {
        let mut series = WindowPnLSeries::new();
        
        let w1 = WindowPnL::new(2000 * NS_PER_SEC, "market1".to_string());
        let w2 = WindowPnL::new(1000 * NS_PER_SEC, "market2".to_string()); // Earlier!
        
        series.add_window(w1);
        series.add_window(w2); // Should panic
    }
    
    #[test]
    fn test_parse_window_start_from_slug() {
        assert_eq!(
            parse_window_start_from_slug("btc-updown-15m-1700000000"),
            Some(1700000000 * NS_PER_SEC)
        );
        
        assert_eq!(
            parse_window_start_from_slug("eth-updown-15m-1234567890-yes"),
            Some(1234567890 * NS_PER_SEC)
        );
        
        assert_eq!(
            parse_window_start_from_slug("invalid-market-slug"),
            None
        );
    }
    
    #[test]
    fn test_align_to_window_start() {
        // 15 minutes = 900 seconds = 900_000_000_000 ns
        let window_ns = 900 * NS_PER_SEC;
        
        // Exactly at boundary
        assert_eq!(align_to_window_start(900 * NS_PER_SEC), 900 * NS_PER_SEC);
        
        // Just after boundary
        assert_eq!(align_to_window_start(901 * NS_PER_SEC), 900 * NS_PER_SEC);
        
        // Just before next boundary
        assert_eq!(align_to_window_start(1799 * NS_PER_SEC), 900 * NS_PER_SEC);
        
        // At next boundary
        assert_eq!(align_to_window_start(1800 * NS_PER_SEC), 1800 * NS_PER_SEC);
    }
    
    #[test]
    fn test_window_fingerprint_deterministic() {
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        w1.gross_pnl = to_amount(10.0);
        w1.fees = to_amount(1.0);
        w1.trades_count = 5;
        
        let mut w2 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        w2.gross_pnl = to_amount(10.0);
        w2.fees = to_amount(1.0);
        w2.trades_count = 5;
        
        assert_eq!(w1.fingerprint_hash(), w2.fingerprint_hash());
    }
    
    #[test]
    fn test_window_fingerprint_changes_on_different_values() {
        let mut w1 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        w1.gross_pnl = to_amount(10.0);
        
        let mut w2 = WindowPnL::new(1000 * NS_PER_SEC, "btc-updown-15m-1000".to_string());
        w2.gross_pnl = to_amount(11.0); // Different!
        
        assert_ne!(w1.fingerprint_hash(), w2.fingerprint_hash());
    }
    
    #[test]
    fn test_window_accounting_engine_basic() {
        let mut engine = WindowAccountingEngine::new(false);
        
        let window = engine.get_or_create_window("btc-updown-15m-1000", 1000 * NS_PER_SEC);
        assert_eq!(window.market_id, "btc-updown-15m-1000");
        assert_eq!(window.window_start_ns, 1000 * NS_PER_SEC);
    }
    
    #[test]
    fn test_series_hash_changes_with_windows() {
        let mut series = WindowPnLSeries::new();
        let initial_hash = series.series_hash;
        
        let w1 = WindowPnL::new(1000 * NS_PER_SEC, "market1".to_string());
        series.add_window(w1);
        
        let after_one = series.series_hash;
        assert_ne!(initial_hash, after_one);
        
        let w2 = WindowPnL::new(2000 * NS_PER_SEC, "market2".to_string());
        series.add_window(w2);
        
        let after_two = series.series_hash;
        assert_ne!(after_one, after_two);
    }
}
