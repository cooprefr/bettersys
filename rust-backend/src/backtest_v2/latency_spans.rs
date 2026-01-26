//! Trade Span Instrumentation for Backtest V2
//!
//! This module provides end-to-end per-decision/per-order latency spans that mirror
//! the live system's TradeSpan stage model from `vault/fast15m_reactive.rs`.
//!
//! # Stage Model (7 stages)
//!
//! 1. **receive**: Inbound market data event arrives at ingestion boundary
//! 2. **visible**: Event becomes visible to strategy (after visibility delay)
//! 3. **evaluate**: Strategy callback execution (start/done)
//! 4. **place**: Order submission to OMS/OrderSender
//! 5. **ack**: Order acknowledgment from simulated venue
//! 6. **fill**: First/last fill notifications
//! 7. **ledger**: Ledger update application
//!
//! # Design Principles
//!
//! - All timestamps are SimClock nanos (i64), never wall-clock
//! - Deterministic: identical runs produce identical spans
//! - Zero-allocation hot path: pre-allocated Vec, no locks
//! - Linkable: event -> decision -> order -> ack/fill -> ledger
//!
//! # Integration
//!
//! The `SpanCollector` is owned by the orchestrator and receives callbacks
//! at each instrumentation point. At run end, spans are serialized to the
//! run artifact as JSON lines.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// CONSTANTS
// =============================================================================

/// Current span schema version.
pub const SPAN_SCHEMA_VERSION: u32 = 1;

/// Pre-allocation size for span vectors.
const DEFAULT_CAPACITY: usize = 10_000;

// =============================================================================
// EVENT KIND
// =============================================================================

/// Kind of event that triggered a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    BookUpdate,
    Trade,
    Timer,
    OrderAck,
    Fill,
    CancelAck,
    Settlement,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BookUpdate => "book_update",
            Self::Trade => "trade",
            Self::Timer => "timer",
            Self::OrderAck => "order_ack",
            Self::Fill => "fill",
            Self::CancelAck => "cancel_ack",
            Self::Settlement => "settlement",
        }
    }
}

// =============================================================================
// DECISION SPAN
// =============================================================================

/// Span for a single strategy decision (evaluation).
///
/// A decision is triggered by a visible event and may produce zero or more orders.
/// This captures the receive -> visible -> evaluate latency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionSpan {
    /// Monotonic decision ID (per strategy instance per run).
    pub decision_id: u64,

    /// Kind of event that triggered this decision.
    pub event_kind: EventKind,

    /// Event identifier (source-specific sequence or hash).
    pub event_seq: u64,

    /// Token/market ID for context.
    pub token_id: String,

    /// Timestamp when the event was received (dataset source time).
    pub receive_ns: Nanos,

    /// Timestamp when the event became visible to strategy.
    pub visible_ns: Nanos,

    /// Timestamp at start of strategy callback.
    pub evaluate_start_ns: Nanos,

    /// Timestamp at end of strategy callback.
    pub evaluate_done_ns: Nanos,

    /// Number of orders placed during this decision.
    pub orders_placed: u32,

    /// Whether receive_ns equals exchange timestamp (vs ingest timestamp).
    /// True means receive_ns == exchange_ts (no separate ingest timestamp available).
    pub receive_is_exchange_ts: bool,

    /// Optional exchange timestamp (if different from receive_ns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange_ts: Option<Nanos>,
}

impl DecisionSpan {
    /// Create a new decision span at the receive stage.
    pub fn new(
        decision_id: u64,
        event_kind: EventKind,
        event_seq: u64,
        token_id: String,
        receive_ns: Nanos,
    ) -> Self {
        Self {
            decision_id,
            event_kind,
            event_seq,
            token_id,
            receive_ns,
            visible_ns: 0,
            evaluate_start_ns: 0,
            evaluate_done_ns: 0,
            orders_placed: 0,
            receive_is_exchange_ts: true,
            exchange_ts: None,
        }
    }

    /// Set the exchange timestamp if different from receive.
    pub fn with_exchange_ts(mut self, exchange_ts: Nanos) -> Self {
        if exchange_ts != self.receive_ns {
            self.exchange_ts = Some(exchange_ts);
            self.receive_is_exchange_ts = false;
        }
        self
    }

    /// Compute ingest-to-visible latency.
    pub fn ingest_to_visible_ns(&self) -> Nanos {
        if self.visible_ns > self.receive_ns {
            self.visible_ns - self.receive_ns
        } else {
            0
        }
    }

    /// Compute visible-to-evaluate-start latency.
    pub fn visible_to_eval_start_ns(&self) -> Nanos {
        if self.evaluate_start_ns > self.visible_ns {
            self.evaluate_start_ns - self.visible_ns
        } else {
            0
        }
    }

    /// Compute evaluate duration.
    pub fn evaluate_duration_ns(&self) -> Nanos {
        if self.evaluate_done_ns > self.evaluate_start_ns {
            self.evaluate_done_ns - self.evaluate_start_ns
        } else {
            0
        }
    }
}

// =============================================================================
// ORDER SPAN
// =============================================================================

/// Span for a single order lifecycle.
///
/// Tracks the order from placement through ack, fill(s), and ledger update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSpan {
    /// Decision that created this order.
    pub decision_id: u64,

    /// Order ID (from OMS).
    pub order_id: OrderId,

    /// Client order ID for correlation.
    pub client_order_id: String,

    /// Token/market ID.
    pub token_id: String,

    /// Order side.
    pub side: Side,

    /// Limit price (in ticks or raw price).
    pub price: Price,

    /// Order size.
    pub size: Size,

    /// Whether this is a maker (post-only) order.
    pub is_maker: bool,

    /// Timestamp when order was placed (sent to OMS).
    pub place_ns: Nanos,

    /// Timestamp when order was acknowledged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_ns: Option<Nanos>,

    /// Timestamp of first fill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_fill_ns: Option<Nanos>,

    /// Timestamp of last fill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fill_ns: Option<Nanos>,

    /// Total fill count.
    pub fill_count: u32,

    /// Total filled size.
    pub filled_size: Size,

    /// Timestamp when ledger was updated for this order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_ns: Option<Nanos>,

    /// Last ledger update timestamp (for partial fills).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ledger_ns: Option<Nanos>,

    /// Timestamp if order was rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_ns: Option<Nanos>,

    /// Rejection reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,

    /// Timestamp if order was cancelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_ack_ns: Option<Nanos>,

    /// Cancelled quantity.
    pub cancelled_size: Size,

    /// Terminal state reached.
    pub is_terminal: bool,
}

impl OrderSpan {
    /// Create a new order span at placement.
    pub fn new(
        decision_id: u64,
        order_id: OrderId,
        client_order_id: String,
        token_id: String,
        side: Side,
        price: Price,
        size: Size,
        is_maker: bool,
        place_ns: Nanos,
    ) -> Self {
        Self {
            decision_id,
            order_id,
            client_order_id,
            token_id,
            side,
            price,
            size,
            is_maker,
            place_ns,
            ack_ns: None,
            first_fill_ns: None,
            last_fill_ns: None,
            fill_count: 0,
            filled_size: 0.0,
            ledger_ns: None,
            last_ledger_ns: None,
            reject_ns: None,
            reject_reason: None,
            cancel_ack_ns: None,
            cancelled_size: 0.0,
            is_terminal: false,
        }
    }

    /// Record an acknowledgment.
    pub fn record_ack(&mut self, ack_ns: Nanos) {
        self.ack_ns = Some(ack_ns);
    }

    /// Record a fill.
    pub fn record_fill(&mut self, fill_ns: Nanos, fill_size: Size) {
        if self.first_fill_ns.is_none() {
            self.first_fill_ns = Some(fill_ns);
        }
        self.last_fill_ns = Some(fill_ns);
        self.fill_count += 1;
        self.filled_size += fill_size;
    }

    /// Record a ledger update.
    pub fn record_ledger(&mut self, ledger_ns: Nanos) {
        if self.ledger_ns.is_none() {
            self.ledger_ns = Some(ledger_ns);
        }
        self.last_ledger_ns = Some(ledger_ns);
    }

    /// Record a rejection.
    pub fn record_reject(&mut self, reject_ns: Nanos, reason: String) {
        self.reject_ns = Some(reject_ns);
        self.reject_reason = Some(reason);
        self.is_terminal = true;
    }

    /// Record a cancellation.
    pub fn record_cancel(&mut self, cancel_ns: Nanos, cancelled_size: Size) {
        self.cancel_ack_ns = Some(cancel_ns);
        self.cancelled_size = cancelled_size;
        if self.filled_size + cancelled_size >= self.size - 1e-9 {
            self.is_terminal = true;
        }
    }

    /// Mark as terminal (fully filled).
    pub fn mark_terminal(&mut self) {
        self.is_terminal = true;
    }

    // === Derived latencies ===

    /// Place to ack latency.
    pub fn place_to_ack_ns(&self) -> Option<Nanos> {
        self.ack_ns.map(|ack| ack - self.place_ns)
    }

    /// Place to first fill latency.
    pub fn place_to_first_fill_ns(&self) -> Option<Nanos> {
        self.first_fill_ns.map(|fill| fill - self.place_ns)
    }

    /// First fill to ledger latency.
    pub fn first_fill_to_ledger_ns(&self) -> Option<Nanos> {
        match (self.first_fill_ns, self.ledger_ns) {
            (Some(fill), Some(ledger)) => Some(ledger - fill),
            _ => None,
        }
    }

    /// Ack to first fill latency.
    pub fn ack_to_first_fill_ns(&self) -> Option<Nanos> {
        match (self.ack_ns, self.first_fill_ns) {
            (Some(ack), Some(fill)) => Some(fill - ack),
            _ => None,
        }
    }
}

// =============================================================================
// SPAN COLLECTOR
// =============================================================================

/// Collector for decision and order spans.
///
/// Owned by the orchestrator, accumulates spans during the run,
/// and serializes at run end.
#[derive(Debug)]
pub struct SpanCollector {
    /// Schema version.
    version: u32,

    /// Decision spans (append-only).
    decisions: Vec<DecisionSpan>,

    /// Order spans indexed by order_id for fast lookup.
    orders: HashMap<OrderId, OrderSpan>,

    /// Order spans in insertion order (for stable output).
    order_ids: Vec<OrderId>,

    /// Next decision ID.
    next_decision_id: u64,

    /// Current decision being built (in-flight).
    current_decision: Option<DecisionSpan>,

    /// Statistics.
    pub stats: SpanCollectorStats,

    /// Whether collection is enabled.
    enabled: bool,
}

/// Statistics from span collection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpanCollectorStats {
    pub total_decisions: u64,
    pub total_orders: u64,
    pub total_fills: u64,
    pub total_rejects: u64,
    pub total_cancels: u64,
    pub orders_with_fills: u64,
    pub orders_fully_filled: u64,
}

impl Default for SpanCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SpanCollector {
    /// Create a new span collector.
    pub fn new() -> Self {
        Self {
            version: SPAN_SCHEMA_VERSION,
            decisions: Vec::with_capacity(DEFAULT_CAPACITY),
            orders: HashMap::with_capacity(DEFAULT_CAPACITY),
            order_ids: Vec::with_capacity(DEFAULT_CAPACITY),
            next_decision_id: 1,
            current_decision: None,
            stats: SpanCollectorStats::default(),
            enabled: true,
        }
    }

    /// Create a disabled collector (no-op for all calls).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::new()
        }
    }

    /// Enable or disable collection.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if collection is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    // === Stage 1: Receive ===

    /// Record that an event was received.
    /// Called when event enters the queue from feed.
    pub fn on_event_received(
        &mut self,
        event_kind: EventKind,
        event_seq: u64,
        token_id: &str,
        receive_ns: Nanos,
        exchange_ts: Option<Nanos>,
    ) {
        if !self.enabled {
            return;
        }

        let decision_id = self.next_decision_id;
        self.next_decision_id += 1;

        let mut span = DecisionSpan::new(
            decision_id,
            event_kind,
            event_seq,
            token_id.to_string(),
            receive_ns,
        );

        if let Some(exch_ts) = exchange_ts {
            span = span.with_exchange_ts(exch_ts);
        }

        self.current_decision = Some(span);
    }

    // === Stage 2: Visible ===

    /// Record that an event became visible to strategy.
    /// Called when event is popped from queue and dispatched.
    pub fn on_event_visible(&mut self, visible_ns: Nanos) {
        if !self.enabled {
            return;
        }

        if let Some(ref mut span) = self.current_decision {
            span.visible_ns = visible_ns;
        }
    }

    // === Stage 3: Evaluate ===

    /// Record start of strategy evaluation.
    pub fn on_evaluate_start(&mut self, eval_start_ns: Nanos) {
        if !self.enabled {
            return;
        }

        if let Some(ref mut span) = self.current_decision {
            span.evaluate_start_ns = eval_start_ns;
        }
    }

    /// Record end of strategy evaluation.
    pub fn on_evaluate_done(&mut self, eval_done_ns: Nanos) {
        if !self.enabled {
            return;
        }

        if let Some(mut span) = self.current_decision.take() {
            span.evaluate_done_ns = eval_done_ns;
            self.stats.total_decisions += 1;
            self.decisions.push(span);
        }
    }

    /// Get the current decision ID (for linking orders).
    pub fn current_decision_id(&self) -> Option<u64> {
        self.current_decision
            .as_ref()
            .map(|d| d.decision_id)
            .or_else(|| self.decisions.last().map(|d| d.decision_id))
    }

    // === Stage 4: Place ===

    /// Record that an order was placed.
    pub fn on_order_placed(
        &mut self,
        order_id: OrderId,
        client_order_id: &str,
        token_id: &str,
        side: Side,
        price: Price,
        size: Size,
        is_maker: bool,
        place_ns: Nanos,
    ) {
        if !self.enabled {
            return;
        }

        let decision_id = self.current_decision_id().unwrap_or(0);

        // Increment orders_placed in current decision
        if let Some(ref mut span) = self.current_decision {
            span.orders_placed += 1;
        }

        let span = OrderSpan::new(
            decision_id,
            order_id,
            client_order_id.to_string(),
            token_id.to_string(),
            side,
            price,
            size,
            is_maker,
            place_ns,
        );

        self.stats.total_orders += 1;
        self.order_ids.push(order_id);
        self.orders.insert(order_id, span);
    }

    // === Stage 5: Ack ===

    /// Record that an order was acknowledged.
    pub fn on_order_acked(&mut self, order_id: OrderId, ack_ns: Nanos) {
        if !self.enabled {
            return;
        }

        if let Some(span) = self.orders.get_mut(&order_id) {
            span.record_ack(ack_ns);
        }
    }

    /// Record that an order was rejected.
    pub fn on_order_rejected(&mut self, order_id: OrderId, reject_ns: Nanos, reason: &str) {
        if !self.enabled {
            return;
        }

        self.stats.total_rejects += 1;

        if let Some(span) = self.orders.get_mut(&order_id) {
            span.record_reject(reject_ns, reason.to_string());
        }
    }

    // === Stage 6: Fill ===

    /// Record that an order was filled.
    pub fn on_order_filled(
        &mut self,
        order_id: OrderId,
        fill_ns: Nanos,
        fill_size: Size,
        total_size: Size,
    ) {
        if !self.enabled {
            return;
        }

        self.stats.total_fills += 1;

        if let Some(span) = self.orders.get_mut(&order_id) {
            let was_unfilled = span.fill_count == 0;
            span.record_fill(fill_ns, fill_size);

            if was_unfilled {
                self.stats.orders_with_fills += 1;
            }

            // Check if fully filled
            if span.filled_size >= total_size - 1e-9 {
                span.mark_terminal();
                self.stats.orders_fully_filled += 1;
            }
        }
    }

    /// Record that an order was cancelled.
    pub fn on_order_cancelled(
        &mut self,
        order_id: OrderId,
        cancel_ns: Nanos,
        cancelled_size: Size,
    ) {
        if !self.enabled {
            return;
        }

        self.stats.total_cancels += 1;

        if let Some(span) = self.orders.get_mut(&order_id) {
            span.record_cancel(cancel_ns, cancelled_size);
        }
    }

    // === Stage 7: Ledger ===

    /// Record that ledger was updated for an order.
    pub fn on_ledger_updated(&mut self, order_id: OrderId, ledger_ns: Nanos) {
        if !self.enabled {
            return;
        }

        if let Some(span) = self.orders.get_mut(&order_id) {
            span.record_ledger(ledger_ns);
        }
    }

    // === Output ===

    /// Get all decision spans.
    pub fn decisions(&self) -> &[DecisionSpan] {
        &self.decisions
    }

    /// Get all order spans in insertion order.
    pub fn orders(&self) -> Vec<&OrderSpan> {
        self.order_ids
            .iter()
            .filter_map(|id| self.orders.get(id))
            .collect()
    }

    /// Get an order span by ID.
    pub fn get_order(&self, order_id: OrderId) -> Option<&OrderSpan> {
        self.orders.get(&order_id)
    }

    /// Serialize all spans to JSON lines (one object per line).
    /// Format: decision spans first, then order spans, stable ordering.
    pub fn to_json_lines(&self) -> String {
        let mut lines = Vec::with_capacity(self.decisions.len() + self.orders.len());

        // Decision spans
        for span in &self.decisions {
            if let Ok(json) = serde_json::to_string(span) {
                lines.push(format!("{{\"type\":\"decision\",\"span\":{}}}", json));
            }
        }

        // Order spans in insertion order
        for order_id in &self.order_ids {
            if let Some(span) = self.orders.get(order_id) {
                if let Ok(json) = serde_json::to_string(span) {
                    lines.push(format!("{{\"type\":\"order\",\"span\":{}}}", json));
                }
            }
        }

        lines.join("\n")
    }

    /// Generate a compact latency summary with percentiles.
    pub fn latency_summary(&self) -> LatencySummary {
        LatencySummary::from_collector(self)
    }
}

// =============================================================================
// LATENCY SUMMARY
// =============================================================================

/// Summary statistics for latency spans.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencySummary {
    /// Number of decisions sampled.
    pub decision_count: u64,

    /// Number of orders sampled.
    pub order_count: u64,

    /// Ingest-to-visible percentiles (ns).
    pub ingest_to_visible: LatencyPercentiles,

    /// Visible-to-evaluate percentiles (ns).
    pub visible_to_evaluate: LatencyPercentiles,

    /// Place-to-ack percentiles (ns).
    pub place_to_ack: LatencyPercentiles,

    /// Place-to-first-fill percentiles (ns).
    pub place_to_first_fill: LatencyPercentiles,

    /// First-fill-to-ledger percentiles (ns).
    pub fill_to_ledger: LatencyPercentiles,
}

/// Latency percentiles for a single metric.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    pub count: u64,
    pub min_ns: Nanos,
    pub p50_ns: Nanos,
    pub p90_ns: Nanos,
    pub p99_ns: Nanos,
    pub max_ns: Nanos,
    pub mean_ns: f64,
}

impl LatencyPercentiles {
    /// Compute percentiles from a Vec of values.
    fn from_values(mut values: Vec<Nanos>) -> Self {
        if values.is_empty() {
            return Self::default();
        }

        values.sort_unstable();
        let count = values.len() as u64;
        let sum: i64 = values.iter().sum();
        let mean_ns = sum as f64 / count as f64;

        Self {
            count,
            min_ns: values[0],
            p50_ns: percentile(&values, 0.50),
            p90_ns: percentile(&values, 0.90),
            p99_ns: percentile(&values, 0.99),
            max_ns: *values.last().unwrap(),
            mean_ns,
        }
    }
}

/// Compute percentile from sorted values.
fn percentile(sorted: &[Nanos], p: f64) -> Nanos {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

impl LatencySummary {
    /// Build summary from a span collector.
    pub fn from_collector(collector: &SpanCollector) -> Self {
        let mut ingest_to_visible = Vec::new();
        let mut visible_to_evaluate = Vec::new();

        for d in &collector.decisions {
            let i2v = d.ingest_to_visible_ns();
            if i2v > 0 {
                ingest_to_visible.push(i2v);
            }
            let v2e = d.visible_to_eval_start_ns();
            if v2e >= 0 {
                visible_to_evaluate.push(v2e);
            }
        }

        let mut place_to_ack = Vec::new();
        let mut place_to_first_fill = Vec::new();
        let mut fill_to_ledger = Vec::new();

        for order_id in &collector.order_ids {
            if let Some(o) = collector.orders.get(order_id) {
                if let Some(p2a) = o.place_to_ack_ns() {
                    place_to_ack.push(p2a);
                }
                if let Some(p2f) = o.place_to_first_fill_ns() {
                    place_to_first_fill.push(p2f);
                }
                if let Some(f2l) = o.first_fill_to_ledger_ns() {
                    fill_to_ledger.push(f2l);
                }
            }
        }

        Self {
            decision_count: collector.decisions.len() as u64,
            order_count: collector.orders.len() as u64,
            ingest_to_visible: LatencyPercentiles::from_values(ingest_to_visible),
            visible_to_evaluate: LatencyPercentiles::from_values(visible_to_evaluate),
            place_to_ack: LatencyPercentiles::from_values(place_to_ack),
            place_to_first_fill: LatencyPercentiles::from_values(place_to_first_fill),
            fill_to_ledger: LatencyPercentiles::from_values(fill_to_ledger),
        }
    }

    /// Format as a compact text summary.
    pub fn format_compact(&self) -> String {
        let fmt_lat = |p: &LatencyPercentiles| -> String {
            if p.count == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "p50={:.0}us p90={:.0}us p99={:.0}us max={:.0}us (n={})",
                    p.p50_ns as f64 / 1000.0,
                    p.p90_ns as f64 / 1000.0,
                    p.p99_ns as f64 / 1000.0,
                    p.max_ns as f64 / 1000.0,
                    p.count
                )
            }
        };

        format!(
            "LatencySummary (decisions={}, orders={}):\n  \
             ingest_to_visible:   {}\n  \
             visible_to_evaluate: {}\n  \
             place_to_ack:        {}\n  \
             place_to_first_fill: {}\n  \
             fill_to_ledger:      {}",
            self.decision_count,
            self.order_count,
            fmt_lat(&self.ingest_to_visible),
            fmt_lat(&self.visible_to_evaluate),
            fmt_lat(&self.place_to_ack),
            fmt_lat(&self.place_to_first_fill),
            fmt_lat(&self.fill_to_ledger),
        )
    }
}

// =============================================================================
// SPAN ARTIFACT
// =============================================================================

/// Complete span artifact for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanArtifact {
    /// Schema version.
    pub schema_version: u32,

    /// Run ID for correlation.
    pub run_id: String,

    /// Latency summary.
    pub summary: LatencySummary,

    /// Collector statistics.
    pub stats: SpanCollectorStats,

    /// Number of decision spans.
    pub decision_count: usize,

    /// Number of order spans.
    pub order_count: usize,

    /// JSON lines blob (decisions + orders).
    /// This is stored separately for streaming access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans_json_lines: Option<String>,
}

impl SpanArtifact {
    /// Build artifact from collector.
    pub fn from_collector(collector: &SpanCollector, run_id: &str, include_spans: bool) -> Self {
        Self {
            schema_version: SPAN_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            summary: collector.latency_summary(),
            stats: collector.stats.clone(),
            decision_count: collector.decisions.len(),
            order_count: collector.orders.len(),
            spans_json_lines: if include_spans {
                Some(collector.to_json_lines())
            } else {
                None
            },
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_span_creation() {
        let span = DecisionSpan::new(1, EventKind::BookUpdate, 100, "token1".to_string(), 1000);
        assert_eq!(span.decision_id, 1);
        assert_eq!(span.event_kind, EventKind::BookUpdate);
        assert_eq!(span.receive_ns, 1000);
        assert!(span.receive_is_exchange_ts);
    }

    #[test]
    fn test_decision_span_with_exchange_ts() {
        let span = DecisionSpan::new(1, EventKind::Trade, 100, "token1".to_string(), 2000)
            .with_exchange_ts(1500);
        assert_eq!(span.receive_ns, 2000);
        assert_eq!(span.exchange_ts, Some(1500));
        assert!(!span.receive_is_exchange_ts);
    }

    #[test]
    fn test_order_span_lifecycle() {
        let mut span = OrderSpan::new(
            1,
            100,
            "ord1".to_string(),
            "token1".to_string(),
            Side::Buy,
            0.55,
            100.0,
            false,
            1000,
        );

        span.record_ack(1100);
        assert_eq!(span.ack_ns, Some(1100));
        assert_eq!(span.place_to_ack_ns(), Some(100));

        span.record_fill(1200, 50.0);
        assert_eq!(span.first_fill_ns, Some(1200));
        assert_eq!(span.fill_count, 1);
        assert_eq!(span.filled_size, 50.0);

        span.record_fill(1300, 50.0);
        assert_eq!(span.last_fill_ns, Some(1300));
        assert_eq!(span.fill_count, 2);
        assert_eq!(span.filled_size, 100.0);

        span.record_ledger(1350);
        assert_eq!(span.ledger_ns, Some(1350));
        assert_eq!(span.first_fill_to_ledger_ns(), Some(150));
    }

    #[test]
    fn test_span_collector_flow() {
        let mut collector = SpanCollector::new();

        // Event received
        collector.on_event_received(EventKind::BookUpdate, 1, "token1", 1000, None);

        // Event visible
        collector.on_event_visible(1100);

        // Evaluate
        collector.on_evaluate_start(1100);

        // Place order
        collector.on_order_placed(100, "ord1", "token1", Side::Buy, 0.55, 100.0, false, 1150);

        // Evaluate done
        collector.on_evaluate_done(1200);

        // Ack
        collector.on_order_acked(100, 1250);

        // Fill
        collector.on_order_filled(100, 1300, 100.0, 100.0);

        // Ledger
        collector.on_ledger_updated(100, 1350);

        // Check results
        assert_eq!(collector.decisions.len(), 1);
        assert_eq!(collector.orders.len(), 1);

        let decision = &collector.decisions[0];
        assert_eq!(decision.receive_ns, 1000);
        assert_eq!(decision.visible_ns, 1100);
        assert_eq!(decision.evaluate_start_ns, 1100);
        assert_eq!(decision.evaluate_done_ns, 1200);
        assert_eq!(decision.orders_placed, 1);

        let order = collector.get_order(100).unwrap();
        assert_eq!(order.place_ns, 1150);
        assert_eq!(order.ack_ns, Some(1250));
        assert_eq!(order.first_fill_ns, Some(1300));
        assert_eq!(order.ledger_ns, Some(1350));
        assert!(order.is_terminal);
    }

    #[test]
    fn test_latency_percentiles() {
        let values: Vec<Nanos> = vec![100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];
        let p = LatencyPercentiles::from_values(values);

        assert_eq!(p.count, 10);
        assert_eq!(p.min_ns, 100);
        assert_eq!(p.max_ns, 1000);
        assert_eq!(p.p50_ns, 500); // ~5th element
    }

    #[test]
    fn test_disabled_collector() {
        let mut collector = SpanCollector::disabled();

        collector.on_event_received(EventKind::BookUpdate, 1, "token1", 1000, None);
        collector.on_event_visible(1100);
        collector.on_evaluate_start(1100);
        collector.on_evaluate_done(1200);

        // Nothing recorded when disabled
        assert!(collector.decisions.is_empty());
    }

    #[test]
    fn test_json_lines_output() {
        let mut collector = SpanCollector::new();

        collector.on_event_received(EventKind::Trade, 1, "token1", 1000, None);
        collector.on_event_visible(1100);
        collector.on_evaluate_start(1100);
        collector.on_order_placed(100, "ord1", "token1", Side::Buy, 0.55, 100.0, false, 1150);
        collector.on_evaluate_done(1200);
        collector.on_order_acked(100, 1250);

        let json_lines = collector.to_json_lines();
        let lines: Vec<&str> = json_lines.lines().collect();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"type\":\"decision\""));
        assert!(lines[1].contains("\"type\":\"order\""));
    }

    #[test]
    fn test_latency_summary_format() {
        let mut collector = SpanCollector::new();

        // Create some spans
        for i in 0..10 {
            collector.on_event_received(
                EventKind::BookUpdate,
                i,
                "token1",
                i as Nanos * 1000,
                None,
            );
            collector.on_event_visible(i as Nanos * 1000 + 100);
            collector.on_evaluate_start(i as Nanos * 1000 + 100);
            collector.on_order_placed(
                i as OrderId + 100,
                &format!("ord{}", i),
                "token1",
                Side::Buy,
                0.55,
                100.0,
                false,
                i as Nanos * 1000 + 150,
            );
            collector.on_evaluate_done(i as Nanos * 1000 + 200);
            collector.on_order_acked(i as OrderId + 100, i as Nanos * 1000 + 250);
            collector.on_order_filled(i as OrderId + 100, i as Nanos * 1000 + 300, 100.0, 100.0);
            collector.on_ledger_updated(i as OrderId + 100, i as Nanos * 1000 + 350);
        }

        let summary = collector.latency_summary();
        assert_eq!(summary.decision_count, 10);
        assert_eq!(summary.order_count, 10);

        let formatted = summary.format_compact();
        assert!(formatted.contains("decisions=10"));
        assert!(formatted.contains("orders=10"));
    }
}
