//! Shadow Maker Validation System
//!
//! Empirically validates the queue model against live Polymarket execution using
//! controlled "shadow maker" experiments. Quantifies model error explicitly without
//! smoothing or averaging away discrepancies.
//!
//! # Purpose
//!
//! This module validates simulator fidelity by comparing:
//! - SIMULATED predictions (queue position, fill timing, fill probability)
//! - ACTUAL live fills observed on Polymarket
//!
//! # Shadow Maker Mode
//!
//! Places real maker orders (small size, low risk) and runs simulator in parallel
//! on the same data stream, comparing predictions to actual outcomes.
//!
//! # Discrepancy Classification
//!
//! - DATA_GAP: Missing deltas or trades explain mismatch
//! - LATENCY_ERROR: Arrival-time or cancel latency mis-modeled
//! - QUEUE_MODEL_ERROR: FIFO assumption violated
//! - VENUE_RULE_ERROR: Polymarket execution differs from model
//! - UNKNOWN: Default if none proven

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::data_contract::{
    ArrivalTimeSemantics, DatasetReadiness, HistoricalDataContract, OrderBookHistory, TradeHistory,
};
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use crate::backtest_v2::queue_model::{QueuePositionModel, QueueStats};
use crate::backtest_v2::maker_fill_gate::{QueueProof, CancelRaceProof};
use crate::backtest_v2::integrity::PathologyCounters;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

// =============================================================================
// PRECONDITION VERIFICATION
// =============================================================================

/// Preconditions that must be met before shadow maker validation can proceed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowMakerPreconditions {
    /// Dataset supports maker execution (incremental L2 deltas + trade prints).
    pub dataset_readiness_ok: bool,
    /// QueuePositionModel is active and used by MakerFillGate.
    pub queue_model_active: bool,
    /// Arrival-time semantics are enforced.
    pub arrival_time_enforced: bool,
    /// Strict accounting enabled.
    pub strict_accounting_enabled: bool,
    /// Invariants are enabled.
    pub invariants_enabled: bool,
    /// Reasons for any failed preconditions.
    pub failure_reasons: Vec<String>,
}

impl ShadowMakerPreconditions {
    /// Check all preconditions against a data contract and config.
    pub fn check(
        contract: &HistoricalDataContract,
        readiness: DatasetReadiness,
        strict_accounting: bool,
        invariants_hard: bool,
    ) -> Self {
        let mut failure_reasons = Vec::new();
        
        // Check dataset readiness supports maker
        let dataset_readiness_ok = readiness.allows_maker();
        if !dataset_readiness_ok {
            failure_reasons.push(format!(
                "DatasetReadiness is {} (must be MakerViable for shadow maker validation)",
                readiness.label()
            ));
        }
        
        // Check for L2 deltas + trade prints
        let queue_model_active = contract.supports_queue_modeling();
        if !queue_model_active {
            if let Some(reason) = contract.queue_modeling_unsupported_reason() {
                failure_reasons.push(format!("Queue modeling not supported: {}", reason));
            } else {
                failure_reasons.push("Queue modeling not supported (missing L2 deltas or trade prints)".to_string());
            }
        }
        
        // Check arrival time semantics
        let arrival_time_enforced = matches!(
            contract.arrival_time,
            ArrivalTimeSemantics::RecordedArrival
        );
        if !arrival_time_enforced {
            failure_reasons.push(format!(
                "Arrival time semantics is {:?} (must be RecordedArrival for shadow validation)",
                contract.arrival_time
            ));
        }
        
        // Check strict accounting
        let strict_accounting_enabled = strict_accounting;
        if !strict_accounting_enabled {
            failure_reasons.push("Strict accounting must be enabled for shadow maker validation".to_string());
        }
        
        // Check invariants
        let invariants_enabled = invariants_hard;
        if !invariants_enabled {
            failure_reasons.push("Invariants must be in Hard mode for shadow maker validation".to_string());
        }
        
        Self {
            dataset_readiness_ok,
            queue_model_active,
            arrival_time_enforced,
            strict_accounting_enabled,
            invariants_enabled,
            failure_reasons,
        }
    }
    
    /// Check if all preconditions pass.
    pub fn all_pass(&self) -> bool {
        self.dataset_readiness_ok
            && self.queue_model_active
            && self.arrival_time_enforced
            && self.strict_accounting_enabled
            && self.invariants_enabled
    }
    
    /// Format as an abort message if preconditions fail.
    pub fn abort_message(&self) -> Option<String> {
        if self.all_pass() {
            None
        } else {
            Some(format!(
                "Shadow Maker Validation ABORTED - Preconditions failed:\n{}",
                self.failure_reasons.iter()
                    .map(|r| format!("  - {}", r))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }
}

// =============================================================================
// SHADOW ORDER - LIVE ORDER TRACKING
// =============================================================================

/// Terminal reason for a shadow order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShadowOrderTerminalReason {
    /// Order was filled completely.
    Filled,
    /// Order was cancelled by strategy.
    Cancelled,
    /// Order expired (time-in-force).
    Expired,
    /// Order was rejected by venue.
    Rejected,
    /// Order is still open (not terminal).
    Open,
}

/// A shadow order placed live for validation purposes.
/// 
/// Records all lifecycle events for comparison with simulator predictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowOrder {
    /// Unique order ID assigned by the system.
    pub order_id: OrderId,
    /// Client order ID for correlation (if used).
    pub client_order_id: Option<String>,
    /// Token ID / market ID.
    pub token_id: String,
    /// Market slug for human readability.
    pub market_slug: String,
    /// Order side.
    pub side: Side,
    /// Limit price.
    pub price: Price,
    /// Order size.
    pub size: Size,
    
    // === Timestamps (all in arrival-time semantics) ===
    
    /// Time order was submitted by strategy (arrival_time at our system).
    pub order_submit_time_ns: Nanos,
    /// Time order ack was received from venue (arrival_time).
    pub order_ack_time_ns: Option<Nanos>,
    /// Time cancel was submitted (if cancelled).
    pub cancel_submit_time_ns: Option<Nanos>,
    /// Time cancel ack was received (if cancelled).
    pub cancel_ack_time_ns: Option<Nanos>,
    /// Time fill was received (arrival_time). First fill if partial.
    pub actual_fill_time_ns: Option<Nanos>,
    /// Time order became terminal (filled/cancelled/expired).
    pub terminal_time_ns: Option<Nanos>,
    
    // === Execution outcomes ===
    
    /// Actual fill size (may be partial).
    pub actual_fill_size: Size,
    /// Actual fill price.
    pub actual_fill_price: Option<Price>,
    /// Terminal reason.
    pub terminal_reason: ShadowOrderTerminalReason,
    /// Number of partial fills received.
    pub fill_count: u32,
    
    // === Venue metadata (if available) ===
    
    /// Exchange-assigned order ID.
    pub exchange_order_id: Option<String>,
    /// Exchange-provided fill IDs.
    pub exchange_fill_ids: Vec<String>,
    /// Fees paid.
    pub fees_paid: f64,
    
    // === Context for replay ===
    
    /// Snapshot of book state at order submit time.
    pub book_snapshot_at_submit: Option<BookSnapshotContext>,
    /// Data hash for the window this order covers.
    pub data_window_hash: u64,
}

/// Minimal book snapshot for context preservation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookSnapshotContext {
    /// Best bid price.
    pub best_bid: Option<Price>,
    /// Best ask price.
    pub best_ask: Option<Price>,
    /// Size at best bid.
    pub bid_size: Size,
    /// Size at best ask.
    pub ask_size: Size,
    /// Total bid depth (top N levels).
    pub bid_depth: Size,
    /// Total ask depth (top N levels).
    pub ask_depth: Size,
    /// Spread in basis points.
    pub spread_bps: Option<f64>,
    /// Mid price.
    pub mid_price: Option<Price>,
    /// Timestamp of snapshot.
    pub snapshot_time_ns: Nanos,
}

impl ShadowOrder {
    /// Create a new shadow order at submission time.
    pub fn new(
        order_id: OrderId,
        token_id: String,
        market_slug: String,
        side: Side,
        price: Price,
        size: Size,
        order_submit_time_ns: Nanos,
    ) -> Self {
        Self {
            order_id,
            client_order_id: None,
            token_id,
            market_slug,
            side,
            price,
            size,
            order_submit_time_ns,
            order_ack_time_ns: None,
            cancel_submit_time_ns: None,
            cancel_ack_time_ns: None,
            actual_fill_time_ns: None,
            terminal_time_ns: None,
            actual_fill_size: 0.0,
            actual_fill_price: None,
            terminal_reason: ShadowOrderTerminalReason::Open,
            fill_count: 0,
            exchange_order_id: None,
            exchange_fill_ids: Vec::new(),
            fees_paid: 0.0,
            book_snapshot_at_submit: None,
            data_window_hash: 0,
        }
    }
    
    /// Record order acknowledgment.
    pub fn record_ack(&mut self, ack_time_ns: Nanos, exchange_order_id: Option<String>) {
        self.order_ack_time_ns = Some(ack_time_ns);
        self.exchange_order_id = exchange_order_id;
    }
    
    /// Record a fill (partial or complete).
    pub fn record_fill(
        &mut self,
        fill_time_ns: Nanos,
        fill_size: Size,
        fill_price: Price,
        fee: f64,
        exchange_fill_id: Option<String>,
    ) {
        // First fill sets the fill time
        if self.actual_fill_time_ns.is_none() {
            self.actual_fill_time_ns = Some(fill_time_ns);
            self.actual_fill_price = Some(fill_price);
        }
        
        self.actual_fill_size += fill_size;
        self.fees_paid += fee;
        self.fill_count += 1;
        
        if let Some(id) = exchange_fill_id {
            self.exchange_fill_ids.push(id);
        }
        
        // Check if fully filled
        if self.actual_fill_size >= self.size - 1e-9 {
            self.terminal_reason = ShadowOrderTerminalReason::Filled;
            self.terminal_time_ns = Some(fill_time_ns);
        }
    }
    
    /// Record cancel submission.
    pub fn record_cancel_submit(&mut self, cancel_time_ns: Nanos) {
        self.cancel_submit_time_ns = Some(cancel_time_ns);
    }
    
    /// Record cancel acknowledgment.
    pub fn record_cancel_ack(&mut self, ack_time_ns: Nanos) {
        self.cancel_ack_time_ns = Some(ack_time_ns);
        if self.terminal_reason == ShadowOrderTerminalReason::Open {
            self.terminal_reason = ShadowOrderTerminalReason::Cancelled;
            self.terminal_time_ns = Some(ack_time_ns);
        }
    }
    
    /// Record rejection.
    pub fn record_rejection(&mut self, reject_time_ns: Nanos) {
        self.terminal_reason = ShadowOrderTerminalReason::Rejected;
        self.terminal_time_ns = Some(reject_time_ns);
    }
    
    /// Check if order is terminal.
    pub fn is_terminal(&self) -> bool {
        !matches!(self.terminal_reason, ShadowOrderTerminalReason::Open)
    }
    
    /// Check if order was filled (fully or partially).
    pub fn was_filled(&self) -> bool {
        self.actual_fill_size > 0.0
    }
    
    /// Get order lifetime in nanoseconds.
    pub fn lifetime_ns(&self) -> Option<Nanos> {
        self.terminal_time_ns.map(|t| t - self.order_submit_time_ns)
    }
    
    /// Get time from submit to first fill.
    pub fn time_to_fill_ns(&self) -> Option<Nanos> {
        self.actual_fill_time_ns.map(|t| t - self.order_submit_time_ns)
    }
    
    /// Compute deterministic hash for this order.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.order_id.hash(&mut hasher);
        self.token_id.hash(&mut hasher);
        (self.side as u8).hash(&mut hasher);
        self.price.to_bits().hash(&mut hasher);
        self.size.to_bits().hash(&mut hasher);
        self.order_submit_time_ns.hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// SHADOW PREDICTION - SIMULATOR OUTPUTS
// =============================================================================

/// Simulator prediction for a shadow order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowPrediction {
    /// Order ID this prediction is for.
    pub order_id: OrderId,
    /// Hash of the order for verification.
    pub order_hash: u64,
    
    // === Queue predictions ===
    
    /// Predicted queue ahead (in size) when order arrives at venue.
    pub predicted_queue_ahead_at_submit: Size,
    /// Predicted queue consumed over time (by trades before fill).
    pub predicted_queue_consumed: Size,
    /// Predicted queue remaining at predicted fill time (should be <= 0 for fill).
    pub predicted_queue_remaining: Size,
    
    // === Fill predictions ===
    
    /// Whether simulator predicts a fill.
    pub predicted_fill: bool,
    /// Predicted fill time (if predicted_fill is true).
    pub predicted_fill_time_ns: Option<Nanos>,
    /// Predicted fill size.
    pub predicted_fill_size: Size,
    
    // === Cancel race predictions ===
    
    /// Whether cancel was predicted to win the race (if cancel submitted).
    pub predicted_cancel_wins: Option<bool>,
    
    // === Proof outputs ===
    
    /// Queue proof generated by simulator.
    pub queue_proof: Option<QueueProof>,
    /// Cancel race proof generated by simulator.
    pub cancel_race_proof: Option<CancelRaceProof>,
    
    // === Simulation context ===
    
    /// Timestamp when simulation was run.
    pub simulation_time_ns: Nanos,
    /// Number of L2 deltas processed in simulation.
    pub deltas_processed: u64,
    /// Number of trade prints processed in simulation.
    pub trades_processed: u64,
    /// Data window hash for verification.
    pub data_window_hash: u64,
}

impl ShadowPrediction {
    /// Create an empty prediction (will be filled by simulator).
    pub fn new(order_id: OrderId, order_hash: u64) -> Self {
        Self {
            order_id,
            order_hash,
            predicted_queue_ahead_at_submit: 0.0,
            predicted_queue_consumed: 0.0,
            predicted_queue_remaining: 0.0,
            predicted_fill: false,
            predicted_fill_time_ns: None,
            predicted_fill_size: 0.0,
            predicted_cancel_wins: None,
            queue_proof: None,
            cancel_race_proof: None,
            simulation_time_ns: 0,
            deltas_processed: 0,
            trades_processed: 0,
            data_window_hash: 0,
        }
    }
}

// =============================================================================
// DISCREPANCY CLASSIFICATION
// =============================================================================

/// Classification of why predicted vs actual outcomes differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscrepancyClass {
    /// No discrepancy - prediction matched actual.
    None,
    /// Missing deltas or trades explain the mismatch.
    DataGap,
    /// Arrival-time or cancel latency was mis-modeled.
    LatencyError,
    /// FIFO assumption violated (queue model error).
    QueueModelError,
    /// Polymarket execution differs from model assumptions.
    VenueRuleError,
    /// Default when cause cannot be determined.
    Unknown,
}

impl DiscrepancyClass {
    /// Get a human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::DataGap => "DATA_GAP",
            Self::LatencyError => "LATENCY_ERROR",
            Self::QueueModelError => "QUEUE_MODEL_ERROR",
            Self::VenueRuleError => "VENUE_RULE_ERROR",
            Self::Unknown => "UNKNOWN",
        }
    }
    
    /// Whether this discrepancy indicates model risk.
    pub fn is_model_risk(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Detailed discrepancy analysis for a single order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderDiscrepancy {
    /// Order ID.
    pub order_id: OrderId,
    /// Classification of the discrepancy.
    pub classification: DiscrepancyClass,
    
    // === Fill occurrence mismatch ===
    
    /// Predicted fill vs actual fill mismatch.
    pub fill_occurrence_mismatch: bool,
    /// False positive: predicted fill but no actual fill.
    pub false_positive: bool,
    /// False negative: no predicted fill but actual fill.
    pub false_negative: bool,
    
    // === Timing errors ===
    
    /// Fill time error: predicted_fill_time − actual_fill_time (ns).
    pub fill_time_error_ns: Option<i64>,
    /// Absolute fill time error.
    pub fill_time_error_abs_ns: Option<u64>,
    
    // === Queue errors ===
    
    /// Queue error at fill: predicted_queue_remaining when fill occurred.
    pub queue_error_at_fill: Option<f64>,
    /// Queue ahead prediction error.
    pub queue_ahead_error: Option<f64>,
    
    // === Context for classification ===
    
    /// Was there a data gap in the observation window?
    pub had_data_gap: bool,
    /// Number of missing deltas detected.
    pub missing_deltas: u64,
    /// Latency model error detected.
    pub latency_model_error: bool,
    /// Evidence supporting the classification.
    pub evidence: Vec<String>,
}

impl OrderDiscrepancy {
    /// Compute from actual order and prediction.
    pub fn compute(
        order: &ShadowOrder,
        prediction: &ShadowPrediction,
        data_integrity: &DataIntegrityContext,
    ) -> Self {
        let actual_filled = order.was_filled();
        let predicted_filled = prediction.predicted_fill;
        
        // Fill occurrence mismatch
        let fill_occurrence_mismatch = actual_filled != predicted_filled;
        let false_positive = predicted_filled && !actual_filled;
        let false_negative = !predicted_filled && actual_filled;
        
        // Timing errors (only if both filled)
        let (fill_time_error_ns, fill_time_error_abs_ns) = 
            if actual_filled && predicted_filled {
                if let (Some(actual_time), Some(predicted_time)) = 
                    (order.actual_fill_time_ns, prediction.predicted_fill_time_ns) 
                {
                    let error = predicted_time - actual_time;
                    (Some(error), Some(error.unsigned_abs()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
        
        // Queue errors
        let queue_error_at_fill = if actual_filled {
            Some(prediction.predicted_queue_remaining)
        } else {
            None
        };
        
        let queue_ahead_error = if actual_filled && order.book_snapshot_at_submit.is_some() {
            // Would need more context to compute this precisely
            None
        } else {
            None
        };
        
        // Check for data gaps
        let had_data_gap = data_integrity.has_gaps;
        let missing_deltas = data_integrity.missing_deltas;
        
        // Determine latency model error
        let latency_model_error = if let Some(error_ns) = fill_time_error_ns {
            // If timing error exceeds expected latency variance, flag it
            error_ns.abs() > 100_000_000 // 100ms threshold
        } else {
            false
        };
        
        // Classify the discrepancy
        let mut evidence = Vec::new();
        let classification = if !fill_occurrence_mismatch && fill_time_error_abs_ns.map_or(true, |e| e < 10_000_000) {
            DiscrepancyClass::None
        } else if had_data_gap || missing_deltas > 0 {
            evidence.push(format!("Data gap detected: {} missing deltas", missing_deltas));
            DiscrepancyClass::DataGap
        } else if latency_model_error {
            if let Some(error_ns) = fill_time_error_ns {
                evidence.push(format!("Fill time error: {}ms", error_ns / 1_000_000));
            }
            DiscrepancyClass::LatencyError
        } else if fill_occurrence_mismatch && !had_data_gap {
            evidence.push(format!(
                "Fill prediction mismatch: predicted={}, actual={}",
                predicted_filled, actual_filled
            ));
            if let Some(queue_remaining) = queue_error_at_fill {
                evidence.push(format!("Queue remaining at fill: {:.2}", queue_remaining));
            }
            DiscrepancyClass::QueueModelError
        } else {
            evidence.push("Unable to determine discrepancy cause".to_string());
            DiscrepancyClass::Unknown
        };
        
        Self {
            order_id: order.order_id,
            classification,
            fill_occurrence_mismatch,
            false_positive,
            false_negative,
            fill_time_error_ns,
            fill_time_error_abs_ns,
            queue_error_at_fill,
            queue_ahead_error,
            had_data_gap,
            missing_deltas,
            latency_model_error,
            evidence,
        }
    }
}

/// Data integrity context for discrepancy analysis.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrityContext {
    /// Whether any data gaps were detected.
    pub has_gaps: bool,
    /// Number of missing deltas.
    pub missing_deltas: u64,
    /// Number of out-of-order events.
    pub out_of_order_events: u64,
    /// Pathology counters from integrity guard.
    pub pathology_counters: Option<PathologyCounters>,
}

// =============================================================================
// AGGREGATE STATISTICS
// =============================================================================

/// Aggregate statistics from shadow maker validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShadowValidationStats {
    /// Total shadow orders analyzed.
    pub total_orders: u64,
    /// Orders that received fills (actual).
    pub orders_filled: u64,
    /// Orders cancelled before fill.
    pub orders_cancelled: u64,
    /// Orders rejected.
    pub orders_rejected: u64,
    
    // === Fill prediction metrics ===
    
    /// True positives: predicted fill AND actual fill.
    pub true_positives: u64,
    /// True negatives: predicted no-fill AND actual no-fill.
    pub true_negatives: u64,
    /// False positives: predicted fill but no actual fill.
    pub false_positives: u64,
    /// False negatives: predicted no-fill but actual fill.
    pub false_negatives: u64,
    
    // === Timing error statistics ===
    
    /// Fill time errors (ns) for orders where both predicted and actual filled.
    pub fill_time_errors_ns: Vec<i64>,
    /// Sum of absolute fill time errors.
    pub sum_abs_fill_time_error_ns: i64,
    /// Max absolute fill time error.
    pub max_abs_fill_time_error_ns: i64,
    
    // === Discrepancy counts ===
    
    pub discrepancies_none: u64,
    pub discrepancies_data_gap: u64,
    pub discrepancies_latency_error: u64,
    pub discrepancies_queue_model_error: u64,
    pub discrepancies_venue_rule_error: u64,
    pub discrepancies_unknown: u64,
}

impl ShadowValidationStats {
    /// Add results from one order.
    pub fn add_order(&mut self, order: &ShadowOrder, prediction: &ShadowPrediction, discrepancy: &OrderDiscrepancy) {
        self.total_orders += 1;
        
        match order.terminal_reason {
            ShadowOrderTerminalReason::Filled => self.orders_filled += 1,
            ShadowOrderTerminalReason::Cancelled => self.orders_cancelled += 1,
            ShadowOrderTerminalReason::Rejected => self.orders_rejected += 1,
            _ => {}
        }
        
        // Fill prediction metrics
        let actual_filled = order.was_filled();
        let predicted_filled = prediction.predicted_fill;
        
        match (predicted_filled, actual_filled) {
            (true, true) => self.true_positives += 1,
            (false, false) => self.true_negatives += 1,
            (true, false) => self.false_positives += 1,
            (false, true) => self.false_negatives += 1,
        }
        
        // Timing errors
        if let Some(error_ns) = discrepancy.fill_time_error_ns {
            self.fill_time_errors_ns.push(error_ns);
            self.sum_abs_fill_time_error_ns += error_ns.abs();
            self.max_abs_fill_time_error_ns = self.max_abs_fill_time_error_ns.max(error_ns.abs());
        }
        
        // Discrepancy classification counts
        match discrepancy.classification {
            DiscrepancyClass::None => self.discrepancies_none += 1,
            DiscrepancyClass::DataGap => self.discrepancies_data_gap += 1,
            DiscrepancyClass::LatencyError => self.discrepancies_latency_error += 1,
            DiscrepancyClass::QueueModelError => self.discrepancies_queue_model_error += 1,
            DiscrepancyClass::VenueRuleError => self.discrepancies_venue_rule_error += 1,
            DiscrepancyClass::Unknown => self.discrepancies_unknown += 1,
        }
    }
    
    /// Fill prediction precision: TP / (TP + FP).
    pub fn fill_precision(&self) -> f64 {
        let denom = self.true_positives + self.false_positives;
        if denom == 0 {
            1.0
        } else {
            self.true_positives as f64 / denom as f64
        }
    }
    
    /// Fill prediction recall: TP / (TP + FN).
    pub fn fill_recall(&self) -> f64 {
        let denom = self.true_positives + self.false_negatives;
        if denom == 0 {
            1.0
        } else {
            self.true_positives as f64 / denom as f64
        }
    }
    
    /// Fill prediction F1 score.
    pub fn fill_f1(&self) -> f64 {
        let precision = self.fill_precision();
        let recall = self.fill_recall();
        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }
    
    /// Mean fill time error (ns).
    pub fn mean_fill_time_error_ns(&self) -> Option<f64> {
        if self.fill_time_errors_ns.is_empty() {
            None
        } else {
            let sum: i64 = self.fill_time_errors_ns.iter().sum();
            Some(sum as f64 / self.fill_time_errors_ns.len() as f64)
        }
    }
    
    /// Median fill time error (ns).
    pub fn median_fill_time_error_ns(&self) -> Option<i64> {
        if self.fill_time_errors_ns.is_empty() {
            return None;
        }
        let mut sorted = self.fill_time_errors_ns.clone();
        sorted.sort();
        let mid = sorted.len() / 2;
        if sorted.len() % 2 == 0 {
            Some((sorted[mid - 1] + sorted[mid]) / 2)
        } else {
            Some(sorted[mid])
        }
    }
    
    /// P95 absolute fill time error (ns).
    pub fn p95_abs_fill_time_error_ns(&self) -> Option<i64> {
        if self.fill_time_errors_ns.is_empty() {
            return None;
        }
        let mut sorted: Vec<i64> = self.fill_time_errors_ns.iter().map(|e| e.abs()).collect();
        sorted.sort();
        let idx = ((sorted.len() as f64) * 0.95).floor() as usize;
        Some(sorted[idx.min(sorted.len() - 1)])
    }
    
    /// Total discrepancies (excluding None).
    pub fn total_discrepancies(&self) -> u64 {
        self.discrepancies_data_gap
            + self.discrepancies_latency_error
            + self.discrepancies_queue_model_error
            + self.discrepancies_venue_rule_error
            + self.discrepancies_unknown
    }
    
    /// Discrepancy rate.
    pub fn discrepancy_rate(&self) -> f64 {
        if self.total_orders == 0 {
            0.0
        } else {
            self.total_discrepancies() as f64 / self.total_orders as f64
        }
    }
}

// =============================================================================
// QUEUE MODEL VALIDATION REPORT
// =============================================================================

/// Thresholds for determining queue model validity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueModelThresholds {
    /// Maximum allowed fill prediction false positive rate.
    pub max_false_positive_rate: f64,
    /// Maximum allowed fill prediction false negative rate.
    pub max_false_negative_rate: f64,
    /// Maximum allowed mean fill time error (ms).
    pub max_mean_fill_time_error_ms: f64,
    /// Maximum allowed P95 fill time error (ms).
    pub max_p95_fill_time_error_ms: f64,
    /// Minimum required fill prediction precision.
    pub min_fill_precision: f64,
    /// Minimum required fill prediction recall.
    pub min_fill_recall: f64,
    /// Maximum allowed queue model error rate.
    pub max_queue_model_error_rate: f64,
    /// Minimum number of shadow orders for statistical validity.
    pub min_sample_size: u64,
}

impl Default for QueueModelThresholds {
    fn default() -> Self {
        Self {
            max_false_positive_rate: 0.10,      // 10%
            max_false_negative_rate: 0.10,      // 10%
            max_mean_fill_time_error_ms: 50.0,  // 50ms
            max_p95_fill_time_error_ms: 200.0,  // 200ms
            min_fill_precision: 0.85,           // 85%
            min_fill_recall: 0.85,              // 85%
            max_queue_model_error_rate: 0.05,   // 5%
            min_sample_size: 100,
        }
    }
}

/// Comprehensive queue model validation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueModelValidationReport {
    /// Validation timestamp.
    pub validation_timestamp_ns: Nanos,
    /// Dataset hash for reproducibility.
    pub dataset_hash: u64,
    
    // === Coverage ===
    
    /// Number of shadow orders analyzed.
    pub coverage_orders: u64,
    /// Time range covered (start).
    pub coverage_start_ns: Nanos,
    /// Time range covered (end).
    pub coverage_end_ns: Nanos,
    /// Tokens/markets covered.
    pub tokens_covered: Vec<String>,
    
    // === Aggregate statistics ===
    
    /// Aggregate validation statistics.
    pub stats: ShadowValidationStats,
    
    // === Threshold checks ===
    
    /// Thresholds used for validation.
    pub thresholds: QueueModelThresholds,
    /// Whether fill precision threshold passed.
    pub fill_precision_passed: bool,
    /// Whether fill recall threshold passed.
    pub fill_recall_passed: bool,
    /// Whether timing error threshold passed.
    pub timing_error_passed: bool,
    /// Whether queue model error rate passed.
    pub queue_model_error_passed: bool,
    /// Whether sample size is sufficient.
    pub sample_size_sufficient: bool,
    
    // === Known failure modes ===
    
    /// Documented failure modes observed.
    pub known_failure_modes: Vec<FailureMode>,
    
    // === Confidence bounds ===
    
    /// Price levels where model is reliable (distance from best in ticks).
    pub reliable_price_levels: Option<i32>,
    /// Queue depths where model is reliable.
    pub reliable_queue_depth_max: Option<Size>,
    /// Time-to-settlement where model is reliable (seconds).
    pub reliable_time_to_settlement_min_sec: Option<i64>,
    
    // === Unsupported regimes ===
    
    /// Regimes where model should refuse to certify maker fills.
    pub unsupported_regimes: Vec<UnsupportedRegime>,
    
    // === Final verdict ===
    
    /// Overall validation passed.
    pub validation_passed: bool,
    /// Reasons for failure (if any).
    pub failure_reasons: Vec<String>,
    /// Recommended trust level for maker fills.
    pub recommended_trust_level: QueueModelTrustLevel,
}

/// Known failure mode documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureMode {
    /// Identifier for this failure mode.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Number of occurrences in validation.
    pub occurrence_count: u64,
    /// Example order IDs.
    pub example_order_ids: Vec<OrderId>,
    /// Mitigation if any.
    pub mitigation: Option<String>,
}

/// Unsupported regime where model should not certify maker fills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedRegime {
    /// Identifier for this regime.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Condition that triggers this regime.
    pub trigger_condition: String,
    /// Recommended action (disable maker, flag non-representative, etc).
    pub recommended_action: String,
}

/// Queue model trust level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueModelTrustLevel {
    /// Model validated - maker fills can be trusted.
    Validated,
    /// Model partially validated - some conditions have uncertainty.
    PartiallyValidated,
    /// Model not validated - maker fills should not be trusted.
    NotValidated,
    /// Validation insufficient - not enough data to determine.
    InsufficientData,
}

impl QueueModelTrustLevel {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Validated => "VALIDATED",
            Self::PartiallyValidated => "PARTIALLY_VALIDATED",
            Self::NotValidated => "NOT_VALIDATED",
            Self::InsufficientData => "INSUFFICIENT_DATA",
        }
    }
    
    /// Whether maker fills can be trusted at this level.
    pub fn allows_maker_trust(&self) -> bool {
        matches!(self, Self::Validated)
    }
}

impl QueueModelValidationReport {
    /// Create an empty report (will be populated during validation).
    pub fn new(thresholds: QueueModelThresholds) -> Self {
        Self {
            validation_timestamp_ns: 0,
            dataset_hash: 0,
            coverage_orders: 0,
            coverage_start_ns: 0,
            coverage_end_ns: 0,
            tokens_covered: Vec::new(),
            stats: ShadowValidationStats::default(),
            thresholds,
            fill_precision_passed: false,
            fill_recall_passed: false,
            timing_error_passed: false,
            queue_model_error_passed: false,
            sample_size_sufficient: false,
            known_failure_modes: Vec::new(),
            reliable_price_levels: None,
            reliable_queue_depth_max: None,
            reliable_time_to_settlement_min_sec: None,
            unsupported_regimes: Vec::new(),
            validation_passed: false,
            failure_reasons: Vec::new(),
            recommended_trust_level: QueueModelTrustLevel::InsufficientData,
        }
    }
    
    /// Finalize the report after all orders have been processed.
    pub fn finalize(&mut self) {
        // Check sample size
        self.sample_size_sufficient = self.stats.total_orders >= self.thresholds.min_sample_size;
        
        if !self.sample_size_sufficient {
            self.failure_reasons.push(format!(
                "Insufficient sample size: {} orders (minimum {})",
                self.stats.total_orders, self.thresholds.min_sample_size
            ));
            self.recommended_trust_level = QueueModelTrustLevel::InsufficientData;
            return;
        }
        
        // Check fill precision
        let precision = self.stats.fill_precision();
        self.fill_precision_passed = precision >= self.thresholds.min_fill_precision;
        if !self.fill_precision_passed {
            self.failure_reasons.push(format!(
                "Fill precision below threshold: {:.2}% (minimum {:.2}%)",
                precision * 100.0, self.thresholds.min_fill_precision * 100.0
            ));
        }
        
        // Check fill recall
        let recall = self.stats.fill_recall();
        self.fill_recall_passed = recall >= self.thresholds.min_fill_recall;
        if !self.fill_recall_passed {
            self.failure_reasons.push(format!(
                "Fill recall below threshold: {:.2}% (minimum {:.2}%)",
                recall * 100.0, self.thresholds.min_fill_recall * 100.0
            ));
        }
        
        // Check timing error
        if let Some(mean_error_ns) = self.stats.mean_fill_time_error_ns() {
            let mean_error_ms = mean_error_ns.abs() / 1_000_000.0;
            let p95_error_ms = self.stats.p95_abs_fill_time_error_ns()
                .map(|ns| ns as f64 / 1_000_000.0)
                .unwrap_or(0.0);
            
            self.timing_error_passed = mean_error_ms <= self.thresholds.max_mean_fill_time_error_ms
                && p95_error_ms <= self.thresholds.max_p95_fill_time_error_ms;
            
            if !self.timing_error_passed {
                self.failure_reasons.push(format!(
                    "Timing error exceeds threshold: mean={:.1}ms (max {:.1}), P95={:.1}ms (max {:.1})",
                    mean_error_ms, self.thresholds.max_mean_fill_time_error_ms,
                    p95_error_ms, self.thresholds.max_p95_fill_time_error_ms
                ));
            }
        } else {
            self.timing_error_passed = true; // No timing data
        }
        
        // Check queue model error rate
        let queue_model_error_rate = if self.stats.total_orders > 0 {
            self.stats.discrepancies_queue_model_error as f64 / self.stats.total_orders as f64
        } else {
            0.0
        };
        self.queue_model_error_passed = queue_model_error_rate <= self.thresholds.max_queue_model_error_rate;
        if !self.queue_model_error_passed {
            self.failure_reasons.push(format!(
                "Queue model error rate exceeds threshold: {:.2}% (maximum {:.2}%)",
                queue_model_error_rate * 100.0, self.thresholds.max_queue_model_error_rate * 100.0
            ));
        }
        
        // Determine overall validation status
        self.validation_passed = self.fill_precision_passed
            && self.fill_recall_passed
            && self.timing_error_passed
            && self.queue_model_error_passed;
        
        // Determine trust level
        if self.validation_passed {
            self.recommended_trust_level = QueueModelTrustLevel::Validated;
        } else if self.fill_precision_passed && self.fill_recall_passed {
            self.recommended_trust_level = QueueModelTrustLevel::PartiallyValidated;
        } else {
            self.recommended_trust_level = QueueModelTrustLevel::NotValidated;
        }
        
        // Add known failure modes
        self.populate_failure_modes();
        
        // Add unsupported regimes
        self.populate_unsupported_regimes();
    }
    
    fn populate_failure_modes(&mut self) {
        // Data gap failures
        if self.stats.discrepancies_data_gap > 0 {
            self.known_failure_modes.push(FailureMode {
                id: "DATA_GAP".to_string(),
                description: "Missing L2 deltas or trade prints caused prediction errors".to_string(),
                occurrence_count: self.stats.discrepancies_data_gap,
                example_order_ids: Vec::new(),
                mitigation: Some("Ensure continuous data recording with integrity checking".to_string()),
            });
        }
        
        // Latency error failures
        if self.stats.discrepancies_latency_error > 0 {
            self.known_failure_modes.push(FailureMode {
                id: "LATENCY_ERROR".to_string(),
                description: "Arrival-time or cancel latency model inaccuracies".to_string(),
                occurrence_count: self.stats.discrepancies_latency_error,
                example_order_ids: Vec::new(),
                mitigation: Some("Calibrate latency distribution from historical fill data".to_string()),
            });
        }
        
        // Queue model errors
        if self.stats.discrepancies_queue_model_error > 0 {
            self.known_failure_modes.push(FailureMode {
                id: "QUEUE_MODEL".to_string(),
                description: "FIFO queue assumption violated or queue position tracking error".to_string(),
                occurrence_count: self.stats.discrepancies_queue_model_error,
                example_order_ids: Vec::new(),
                mitigation: Some("Review queue position model; may need order-level data".to_string()),
            });
        }
    }
    
    fn populate_unsupported_regimes(&mut self) {
        // Add standard unsupported regimes based on failure analysis
        if self.stats.discrepancies_queue_model_error > 0 {
            self.unsupported_regimes.push(UnsupportedRegime {
                id: "HIGH_VOLATILITY".to_string(),
                description: "High volatility periods where queue dynamics change rapidly".to_string(),
                trigger_condition: "Price moves > 5% in 60 seconds".to_string(),
                recommended_action: "Disable maker fills or flag as NON_REPRESENTATIVE".to_string(),
            });
        }
        
        // Near-settlement regime
        self.unsupported_regimes.push(UnsupportedRegime {
            id: "NEAR_SETTLEMENT".to_string(),
            description: "Final minutes before market settlement".to_string(),
            trigger_condition: "Time to settlement < 60 seconds".to_string(),
            recommended_action: "Disable maker fills in final minute".to_string(),
        });
    }
    
    /// Format as a human-readable report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                    QUEUE MODEL VALIDATION REPORT                             ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!("║  VERDICT: {:^64} ║\n", 
            if self.validation_passed { "PASSED" } else { "FAILED" }
        ));
        out.push_str(&format!("║  Trust Level: {:^60} ║\n", self.recommended_trust_level.label()));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        
        out.push_str("║  COVERAGE:                                                                   ║\n");
        out.push_str(&format!("║    Orders analyzed: {:>56} ║\n", self.coverage_orders));
        out.push_str(&format!("║    Orders filled:   {:>56} ║\n", self.stats.orders_filled));
        out.push_str(&format!("║    Tokens covered:  {:>56} ║\n", self.tokens_covered.len()));
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  FILL PREDICTION:                                                            ║\n");
        let check = |b: bool| if b { "✓" } else { "✗" };
        out.push_str(&format!("║    [{}] Precision: {:.2}% (min {:.2}%)                                        ║\n",
            check(self.fill_precision_passed),
            self.stats.fill_precision() * 100.0,
            self.thresholds.min_fill_precision * 100.0
        ));
        out.push_str(&format!("║    [{}] Recall:    {:.2}% (min {:.2}%)                                        ║\n",
            check(self.fill_recall_passed),
            self.stats.fill_recall() * 100.0,
            self.thresholds.min_fill_recall * 100.0
        ));
        out.push_str(&format!("║    F1 Score:      {:.2}%                                                     ║\n",
            self.stats.fill_f1() * 100.0
        ));
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  TIMING ERROR:                                                               ║\n");
        if let Some(mean_ns) = self.stats.mean_fill_time_error_ns() {
            out.push_str(&format!("║    Mean:   {:>10.1}ms                                                     ║\n",
                mean_ns / 1_000_000.0
            ));
        }
        if let Some(median_ns) = self.stats.median_fill_time_error_ns() {
            out.push_str(&format!("║    Median: {:>10.1}ms                                                     ║\n",
                median_ns as f64 / 1_000_000.0
            ));
        }
        if let Some(p95_ns) = self.stats.p95_abs_fill_time_error_ns() {
            out.push_str(&format!("║    P95:    {:>10.1}ms                                                     ║\n",
                p95_ns as f64 / 1_000_000.0
            ));
        }
        
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  DISCREPANCY BREAKDOWN:                                                      ║\n");
        out.push_str(&format!("║    None (correct):      {:>52} ║\n", self.stats.discrepancies_none));
        out.push_str(&format!("║    Data Gap:            {:>52} ║\n", self.stats.discrepancies_data_gap));
        out.push_str(&format!("║    Latency Error:       {:>52} ║\n", self.stats.discrepancies_latency_error));
        out.push_str(&format!("║    Queue Model Error:   {:>52} ║\n", self.stats.discrepancies_queue_model_error));
        out.push_str(&format!("║    Venue Rule Error:    {:>52} ║\n", self.stats.discrepancies_venue_rule_error));
        out.push_str(&format!("║    Unknown:             {:>52} ║\n", self.stats.discrepancies_unknown));
        
        if !self.failure_reasons.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  FAILURE REASONS:                                                            ║\n");
            for reason in &self.failure_reasons {
                let truncated = if reason.len() > 72 {
                    format!("{}...", &reason[..69])
                } else {
                    reason.clone()
                };
                out.push_str(&format!("║    • {:70} ║\n", truncated));
            }
        }
        
        if !self.unsupported_regimes.is_empty() {
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str("║  UNSUPPORTED REGIMES (maker fills should be disabled):                      ║\n");
            for regime in &self.unsupported_regimes {
                out.push_str(&format!("║    • {}: {}                                 ║\n", 
                    regime.id, 
                    if regime.description.len() > 50 {
                        format!("{}...", &regime.description[..47])
                    } else {
                        regime.description.clone()
                    }
                ));
            }
        }
        
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        
        out
    }
}

// =============================================================================
// SHADOW MAKER VALIDATOR
// =============================================================================

/// Shadow maker validation coordinator.
/// 
/// Manages:
/// - Shadow order placement and tracking
/// - Parallel simulation for predictions
/// - Comparison and discrepancy analysis
/// - Report generation
pub struct ShadowMakerValidator {
    /// Preconditions (checked at initialization).
    preconditions: ShadowMakerPreconditions,
    /// Active shadow orders (order_id -> order).
    active_orders: HashMap<OrderId, ShadowOrder>,
    /// Completed shadow orders with predictions.
    completed_orders: Vec<(ShadowOrder, ShadowPrediction, OrderDiscrepancy)>,
    /// Validation thresholds.
    thresholds: QueueModelThresholds,
    /// Next order ID.
    next_order_id: OrderId,
    /// Data integrity context.
    data_integrity: DataIntegrityContext,
    /// Whether validation is enabled (preconditions passed).
    enabled: bool,
}

impl ShadowMakerValidator {
    /// Create a new validator (will check preconditions).
    pub fn new(
        contract: &HistoricalDataContract,
        readiness: DatasetReadiness,
        strict_accounting: bool,
        invariants_hard: bool,
        thresholds: QueueModelThresholds,
    ) -> Self {
        let preconditions = ShadowMakerPreconditions::check(
            contract,
            readiness,
            strict_accounting,
            invariants_hard,
        );
        let enabled = preconditions.all_pass();
        
        Self {
            preconditions,
            active_orders: HashMap::new(),
            completed_orders: Vec::new(),
            thresholds,
            next_order_id: 1,
            data_integrity: DataIntegrityContext::default(),
            enabled,
        }
    }
    
    /// Check if validation is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    
    /// Get preconditions (for abort message if needed).
    pub fn preconditions(&self) -> &ShadowMakerPreconditions {
        &self.preconditions
    }
    
    /// Submit a shadow order for validation.
    /// 
    /// Returns the order ID assigned, or None if validation is disabled.
    pub fn submit_shadow_order(
        &mut self,
        token_id: String,
        market_slug: String,
        side: Side,
        price: Price,
        size: Size,
        submit_time_ns: Nanos,
        book_snapshot: Option<BookSnapshotContext>,
    ) -> Option<OrderId> {
        if !self.enabled {
            return None;
        }
        
        let order_id = self.next_order_id;
        self.next_order_id += 1;
        
        let mut order = ShadowOrder::new(
            order_id,
            token_id,
            market_slug,
            side,
            price,
            size,
            submit_time_ns,
        );
        order.book_snapshot_at_submit = book_snapshot;
        
        self.active_orders.insert(order_id, order);
        Some(order_id)
    }
    
    /// Record order acknowledgment.
    pub fn record_ack(&mut self, order_id: OrderId, ack_time_ns: Nanos, exchange_order_id: Option<String>) {
        if let Some(order) = self.active_orders.get_mut(&order_id) {
            order.record_ack(ack_time_ns, exchange_order_id);
        }
    }
    
    /// Record a fill.
    pub fn record_fill(
        &mut self,
        order_id: OrderId,
        fill_time_ns: Nanos,
        fill_size: Size,
        fill_price: Price,
        fee: f64,
        exchange_fill_id: Option<String>,
    ) {
        if let Some(order) = self.active_orders.get_mut(&order_id) {
            order.record_fill(fill_time_ns, fill_size, fill_price, fee, exchange_fill_id);
        }
    }
    
    /// Record cancel submission.
    pub fn record_cancel_submit(&mut self, order_id: OrderId, cancel_time_ns: Nanos) {
        if let Some(order) = self.active_orders.get_mut(&order_id) {
            order.record_cancel_submit(cancel_time_ns);
        }
    }
    
    /// Record cancel acknowledgment.
    pub fn record_cancel_ack(&mut self, order_id: OrderId, ack_time_ns: Nanos) {
        if let Some(order) = self.active_orders.get_mut(&order_id) {
            order.record_cancel_ack(ack_time_ns);
        }
    }
    
    /// Record rejection.
    pub fn record_rejection(&mut self, order_id: OrderId, reject_time_ns: Nanos) {
        if let Some(order) = self.active_orders.get_mut(&order_id) {
            order.record_rejection(reject_time_ns);
        }
    }
    
    /// Update data integrity context.
    pub fn update_data_integrity(&mut self, counters: &PathologyCounters) {
        self.data_integrity.has_gaps = counters.gaps_detected > 0;
        self.data_integrity.missing_deltas = counters.total_missing_sequences;
        self.data_integrity.out_of_order_events = counters.out_of_order_detected;
        self.data_integrity.pathology_counters = Some(counters.clone());
    }
    
    /// Complete a shadow order with its simulation prediction.
    pub fn complete_order(&mut self, order_id: OrderId, prediction: ShadowPrediction) {
        if let Some(order) = self.active_orders.remove(&order_id) {
            let discrepancy = OrderDiscrepancy::compute(&order, &prediction, &self.data_integrity);
            self.completed_orders.push((order, prediction, discrepancy));
        }
    }
    
    /// Get active orders.
    pub fn active_orders(&self) -> impl Iterator<Item = &ShadowOrder> {
        self.active_orders.values()
    }
    
    /// Get number of active orders.
    pub fn active_order_count(&self) -> usize {
        self.active_orders.len()
    }
    
    /// Get number of completed orders.
    pub fn completed_order_count(&self) -> usize {
        self.completed_orders.len()
    }
    
    /// Generate validation report.
    pub fn generate_report(&self, validation_timestamp_ns: Nanos, dataset_hash: u64) -> QueueModelValidationReport {
        let mut report = QueueModelValidationReport::new(self.thresholds.clone());
        
        report.validation_timestamp_ns = validation_timestamp_ns;
        report.dataset_hash = dataset_hash;
        report.coverage_orders = self.completed_orders.len() as u64;
        
        // Find time range
        if !self.completed_orders.is_empty() {
            report.coverage_start_ns = self.completed_orders.iter()
                .map(|(o, _, _)| o.order_submit_time_ns)
                .min()
                .unwrap_or(0);
            report.coverage_end_ns = self.completed_orders.iter()
                .map(|(o, _, _)| o.terminal_time_ns.unwrap_or(o.order_submit_time_ns))
                .max()
                .unwrap_or(0);
        }
        
        // Collect unique tokens
        let mut tokens: Vec<String> = self.completed_orders.iter()
            .map(|(o, _, _)| o.token_id.clone())
            .collect();
        tokens.sort();
        tokens.dedup();
        report.tokens_covered = tokens;
        
        // Aggregate statistics
        for (order, prediction, discrepancy) in &self.completed_orders {
            report.stats.add_order(order, prediction, discrepancy);
        }
        
        // Finalize (runs threshold checks)
        report.finalize();
        
        report
    }
}

// =============================================================================
// TRUST GATING INTEGRATION
// =============================================================================

/// Queue model validation flag for BacktestResults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueModelValidationFlag {
    /// Whether queue model validation was performed.
    pub validation_performed: bool,
    /// Validation result.
    pub validation_result: Option<QueueModelTrustLevel>,
    /// Dataset hash used for validation.
    pub validation_dataset_hash: Option<u64>,
    /// Validation timestamp.
    pub validation_timestamp_ns: Option<Nanos>,
    /// Number of shadow orders used.
    pub shadow_order_count: Option<u64>,
}

impl Default for QueueModelValidationFlag {
    fn default() -> Self {
        Self {
            validation_performed: false,
            validation_result: None,
            validation_dataset_hash: None,
            validation_timestamp_ns: None,
            shadow_order_count: None,
        }
    }
}

impl QueueModelValidationFlag {
    /// Create from a validation report.
    pub fn from_report(report: &QueueModelValidationReport) -> Self {
        Self {
            validation_performed: true,
            validation_result: Some(report.recommended_trust_level),
            validation_dataset_hash: Some(report.dataset_hash),
            validation_timestamp_ns: Some(report.validation_timestamp_ns),
            shadow_order_count: Some(report.coverage_orders),
        }
    }
    
    /// Whether maker fills can be certified based on validation.
    pub fn allows_maker_certification(&self) -> bool {
        self.validation_performed 
            && self.validation_result.map_or(false, |r| r.allows_maker_trust())
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preconditions_all_pass() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let preconditions = ShadowMakerPreconditions::check(
            &contract,
            DatasetReadiness::MakerViable,
            true,
            true,
        );
        
        assert!(preconditions.all_pass());
        assert!(preconditions.abort_message().is_none());
    }
    
    #[test]
    fn test_preconditions_fail_non_maker_readiness() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let preconditions = ShadowMakerPreconditions::check(
            &contract,
            DatasetReadiness::TakerOnly,
            true,
            true,
        );
        
        assert!(!preconditions.all_pass());
        assert!(!preconditions.dataset_readiness_ok);
        assert!(preconditions.abort_message().is_some());
    }
    
    #[test]
    fn test_shadow_order_lifecycle() {
        let mut order = ShadowOrder::new(
            1,
            "token123".to_string(),
            "btc-updown-15m".to_string(),
            Side::Buy,
            0.55,
            100.0,
            1_000_000_000,
        );
        
        assert!(!order.is_terminal());
        assert!(!order.was_filled());
        
        order.record_ack(1_000_010_000, Some("exch123".to_string()));
        assert_eq!(order.order_ack_time_ns, Some(1_000_010_000));
        
        order.record_fill(1_000_500_000, 50.0, 0.55, 0.001, Some("fill1".to_string()));
        assert!(order.was_filled());
        assert!(!order.is_terminal()); // Partial fill
        
        order.record_fill(1_000_600_000, 50.0, 0.55, 0.001, Some("fill2".to_string()));
        assert!(order.is_terminal()); // Fully filled
        assert_eq!(order.terminal_reason, ShadowOrderTerminalReason::Filled);
        assert_eq!(order.fill_count, 2);
    }
    
    #[test]
    fn test_discrepancy_classification_no_mismatch() {
        let order = ShadowOrder {
            order_id: 1,
            client_order_id: None,
            token_id: "token".to_string(),
            market_slug: "market".to_string(),
            side: Side::Buy,
            price: 0.5,
            size: 100.0,
            order_submit_time_ns: 1000,
            order_ack_time_ns: Some(1010),
            cancel_submit_time_ns: None,
            cancel_ack_time_ns: None,
            actual_fill_time_ns: Some(2000),
            terminal_time_ns: Some(2000),
            actual_fill_size: 100.0,
            actual_fill_price: Some(0.5),
            terminal_reason: ShadowOrderTerminalReason::Filled,
            fill_count: 1,
            exchange_order_id: None,
            exchange_fill_ids: vec![],
            fees_paid: 0.001,
            book_snapshot_at_submit: None,
            data_window_hash: 0,
        };
        
        let prediction = ShadowPrediction {
            order_id: 1,
            order_hash: 0,
            predicted_queue_ahead_at_submit: 50.0,
            predicted_queue_consumed: 60.0,
            predicted_queue_remaining: -10.0, // Negative = fill allowed
            predicted_fill: true,
            predicted_fill_time_ns: Some(2005), // Close to actual
            predicted_fill_size: 100.0,
            predicted_cancel_wins: None,
            queue_proof: None,
            cancel_race_proof: None,
            simulation_time_ns: 0,
            deltas_processed: 100,
            trades_processed: 50,
            data_window_hash: 0,
        };
        
        let data_integrity = DataIntegrityContext::default();
        let discrepancy = OrderDiscrepancy::compute(&order, &prediction, &data_integrity);
        
        assert_eq!(discrepancy.classification, DiscrepancyClass::None);
        assert!(!discrepancy.fill_occurrence_mismatch);
    }
    
    #[test]
    fn test_validation_stats_precision_recall() {
        let mut stats = ShadowValidationStats::default();
        stats.true_positives = 80;
        stats.true_negatives = 15;
        stats.false_positives = 10;
        stats.false_negatives = 5;
        stats.total_orders = 110;
        
        // Precision = 80 / (80 + 10) = 0.888
        assert!((stats.fill_precision() - 0.888).abs() < 0.01);
        
        // Recall = 80 / (80 + 5) = 0.941
        assert!((stats.fill_recall() - 0.941).abs() < 0.01);
    }
    
    #[test]
    fn test_report_finalization() {
        let thresholds = QueueModelThresholds::default();
        let mut report = QueueModelValidationReport::new(thresholds);
        
        // Add some mock stats
        report.stats.total_orders = 150;
        report.stats.true_positives = 90;
        report.stats.true_negatives = 40;
        report.stats.false_positives = 10;
        report.stats.false_negatives = 10;
        report.stats.discrepancies_none = 130;
        report.stats.discrepancies_queue_model_error = 5;
        report.stats.discrepancies_unknown = 15;
        
        report.finalize();
        
        assert!(report.sample_size_sufficient);
        // With the mock stats above, validation should pass most checks
    }
    
    #[test]
    fn test_validator_disabled_without_preconditions() {
        // Use snapshot-only contract (doesn't support maker)
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let mut validator = ShadowMakerValidator::new(
            &contract,
            DatasetReadiness::TakerOnly,
            true,
            true,
            QueueModelThresholds::default(),
        );
        
        assert!(!validator.is_enabled());
        assert!(validator.submit_shadow_order(
            "token".to_string(),
            "market".to_string(),
            Side::Buy,
            0.5,
            100.0,
            1000,
            None,
        ).is_none());
    }
    
    #[test]
    fn test_trust_level_labels() {
        assert_eq!(QueueModelTrustLevel::Validated.label(), "VALIDATED");
        assert!(QueueModelTrustLevel::Validated.allows_maker_trust());
        assert!(!QueueModelTrustLevel::NotValidated.allows_maker_trust());
    }
}
