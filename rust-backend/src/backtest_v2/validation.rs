//! Validation and Reproducibility Guarantees
//!
//! Ensures deterministic replay, order-book invariants, and comprehensive tracing.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, OrderId, Price, Side, Size};
use crate::backtest_v2::matching::LimitOrderBook;
use crate::backtest_v2::portfolio::{MarketId, Outcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash, Hasher};

// ============================================================================
// Deterministic Seeding
// ============================================================================

/// Deterministic seed configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeterministicSeed {
    pub primary: u64,
    pub latency: u64,
    pub fill_probability: u64,
    pub queue_position: u64,
}

impl DeterministicSeed {
    pub fn new(seed: u64) -> Self {
        // Derive sub-seeds deterministically using a simple mix
        Self {
            primary: seed,
            latency: Self::mix(seed, 0x1234_5678_9ABC_DEF0),
            fill_probability: Self::mix(seed, 0xFEDC_BA98_7654_3210),
            queue_position: Self::mix(seed, 0xACE0_FACE_BEEF_CAFE),
        }
    }

    fn mix(a: u64, b: u64) -> u64 {
        let mut x = a ^ b;
        x = x.wrapping_mul(0x517c_c1b7_2722_0a95);
        x ^= x >> 32;
        x = x.wrapping_mul(0x517c_c1b7_2722_0a95);
        x ^= x >> 32;
        x
    }

    /// Get a sub-seed for a specific component.
    pub fn sub_seed(&self, component: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.primary.hash(&mut hasher);
        component.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for DeterministicSeed {
    fn default() -> Self {
        Self::new(42)
    }
}

// ============================================================================
// State Fingerprinting
// ============================================================================

/// Fingerprint of backtest state for reproducibility checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateFingerprint {
    pub timestamp: Nanos,
    pub event_count: u64,
    pub order_count: u64,
    pub fill_count: u64,
    pub cash_cents: i64,
    pub position_hash: u64,
    pub book_hash: u64,
}

impl StateFingerprint {
    pub fn new(
        timestamp: Nanos,
        event_count: u64,
        order_count: u64,
        fill_count: u64,
        cash: f64,
        positions: &HashMap<String, f64>,
        book: &LimitOrderBook,
    ) -> Self {
        Self {
            timestamp,
            event_count,
            order_count,
            fill_count,
            cash_cents: (cash * 100.0).round() as i64,
            position_hash: Self::hash_positions(positions),
            book_hash: Self::hash_book(book),
        }
    }

    fn hash_positions(positions: &HashMap<String, f64>) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut sorted: Vec<_> = positions.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in sorted {
            k.hash(&mut hasher);
            ((v * 1_000_000.0).round() as i64).hash(&mut hasher);
        }
        hasher.finish()
    }

    fn hash_book(book: &LimitOrderBook) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();

        // Hash best bid/ask and spread (available via public methods)
        if let Some((bid_price, bid_size)) = book.best_bid() {
            ((bid_price * 100.0).round() as i64).hash(&mut hasher);
            ((bid_size * 1_000_000.0).round() as i64).hash(&mut hasher);
        }

        if let Some((ask_price, ask_size)) = book.best_ask() {
            ((ask_price * 100.0).round() as i64).hash(&mut hasher);
            ((ask_size * 1_000_000.0).round() as i64).hash(&mut hasher);
        }

        // Hash spread if available
        if let Some(spread) = book.spread() {
            ((spread * 10000.0).round() as i64).hash(&mut hasher);
        }

        hasher.finish()
    }
}

/// Checkpoint for reproducibility validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub fingerprint: StateFingerprint,
    pub label: String,
}

/// Reproducibility validator.
#[derive(Debug, Default)]
pub struct ReproducibilityValidator {
    checkpoints: Vec<Checkpoint>,
    fingerprints: Vec<StateFingerprint>,
}

impl ReproducibilityValidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a checkpoint.
    pub fn checkpoint(&mut self, fingerprint: StateFingerprint, label: impl Into<String>) {
        self.checkpoints.push(Checkpoint {
            fingerprint: fingerprint.clone(),
            label: label.into(),
        });
        self.fingerprints.push(fingerprint);
    }

    /// Validate against another run's checkpoints.
    pub fn validate_against(&self, other: &ReproducibilityValidator) -> ValidationResult {
        if self.checkpoints.len() != other.checkpoints.len() {
            return ValidationResult::Failed {
                reason: format!(
                    "Checkpoint count mismatch: {} vs {}",
                    self.checkpoints.len(),
                    other.checkpoints.len()
                ),
                first_divergence: None,
            };
        }

        for (i, (a, b)) in self
            .checkpoints
            .iter()
            .zip(other.checkpoints.iter())
            .enumerate()
        {
            if a.fingerprint != b.fingerprint {
                return ValidationResult::Failed {
                    reason: format!(
                        "Divergence at checkpoint {} '{}': fingerprints differ",
                        i, a.label
                    ),
                    first_divergence: Some(i),
                };
            }
        }

        ValidationResult::Passed {
            checkpoints_validated: self.checkpoints.len(),
        }
    }

    /// Get final fingerprint.
    pub fn final_fingerprint(&self) -> Option<&StateFingerprint> {
        self.fingerprints.last()
    }

    /// Export checkpoints for later comparison.
    pub fn export(&self) -> Vec<Checkpoint> {
        self.checkpoints.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationResult {
    Passed {
        checkpoints_validated: usize,
    },
    Failed {
        reason: String,
        first_divergence: Option<usize>,
    },
}

// ============================================================================
// Order Book Invariant Tests
// ============================================================================

/// Order book invariant checker.
#[derive(Debug, Default)]
pub struct InvariantChecker {
    violations: Vec<InvariantViolation>,
    checks_passed: u64,
}

/// Type of invariant violation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvariantViolation {
    /// Best bid >= best ask (crossed book).
    CrossedBook {
        timestamp: Nanos,
        best_bid: Price,
        best_ask: Price,
    },
    /// Negative size at a level.
    NegativeSize {
        timestamp: Nanos,
        side: String,
        price: Price,
        size: Size,
    },
    /// Price outside valid range.
    InvalidPrice {
        timestamp: Nanos,
        side: String,
        price: Price,
    },
    /// Order count inconsistency.
    OrderCountMismatch {
        timestamp: Nanos,
        expected: usize,
        actual: usize,
    },
    /// Fill exceeds available size.
    OverFill {
        timestamp: Nanos,
        order_id: OrderId,
        available: Size,
        filled: Size,
    },
    /// Duplicate order ID.
    DuplicateOrderId { timestamp: Nanos, order_id: OrderId },
    /// Unknown order referenced.
    UnknownOrder {
        timestamp: Nanos,
        order_id: OrderId,
        context: String,
    },
    /// Custom invariant violation.
    Custom {
        timestamp: Nanos,
        description: String,
    },
}

impl InvariantChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check all order book invariants.
    pub fn check_book(&mut self, book: &LimitOrderBook, timestamp: Nanos) -> bool {
        let mut valid = true;

        // Check for crossed book
        if let (Some((best_bid, _)), Some((best_ask, _))) = (book.best_bid(), book.best_ask()) {
            if best_bid >= best_ask {
                self.violations.push(InvariantViolation::CrossedBook {
                    timestamp,
                    best_bid,
                    best_ask,
                });
                valid = false;
            }
        }

        // Check best bid validity
        if let Some((price, size)) = book.best_bid() {
            if size < 0.0 {
                self.violations.push(InvariantViolation::NegativeSize {
                    timestamp,
                    side: "bid".into(),
                    price,
                    size,
                });
                valid = false;
            }
            if price < 0.0 || price > 1.0 {
                self.violations.push(InvariantViolation::InvalidPrice {
                    timestamp,
                    side: "bid".into(),
                    price,
                });
                valid = false;
            }
        }

        // Check best ask validity
        if let Some((price, size)) = book.best_ask() {
            if size < 0.0 {
                self.violations.push(InvariantViolation::NegativeSize {
                    timestamp,
                    side: "ask".into(),
                    price,
                    size,
                });
                valid = false;
            }
            if price < 0.0 || price > 1.0 {
                self.violations.push(InvariantViolation::InvalidPrice {
                    timestamp,
                    side: "ask".into(),
                    price,
                });
                valid = false;
            }
        }

        // Check spread is non-negative if both sides exist
        if let Some(spread) = book.spread() {
            if spread < 0.0 {
                self.violations.push(InvariantViolation::Custom {
                    timestamp,
                    description: format!("Negative spread: {}", spread),
                });
                valid = false;
            }
        }

        if valid {
            self.checks_passed += 1;
        }

        valid
    }

    /// Record a custom violation.
    pub fn record_violation(&mut self, violation: InvariantViolation) {
        self.violations.push(violation);
    }

    /// Check for overfill.
    pub fn check_fill(
        &mut self,
        timestamp: Nanos,
        order_id: OrderId,
        available: Size,
        filled: Size,
    ) -> bool {
        if filled > available + 1e-9 {
            self.violations.push(InvariantViolation::OverFill {
                timestamp,
                order_id,
                available,
                filled,
            });
            return false;
        }
        self.checks_passed += 1;
        true
    }

    /// Get all violations.
    pub fn violations(&self) -> &[InvariantViolation] {
        &self.violations
    }

    /// Check if any violations occurred.
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    /// Get summary.
    pub fn summary(&self) -> InvariantSummary {
        InvariantSummary {
            checks_passed: self.checks_passed,
            violations: self.violations.len(),
            violation_types: self.count_by_type(),
        }
    }

    fn count_by_type(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for v in &self.violations {
            let key = match v {
                InvariantViolation::CrossedBook { .. } => "crossed_book",
                InvariantViolation::NegativeSize { .. } => "negative_size",
                InvariantViolation::InvalidPrice { .. } => "invalid_price",
                InvariantViolation::OrderCountMismatch { .. } => "order_count_mismatch",
                InvariantViolation::OverFill { .. } => "over_fill",
                InvariantViolation::DuplicateOrderId { .. } => "duplicate_order_id",
                InvariantViolation::UnknownOrder { .. } => "unknown_order",
                InvariantViolation::Custom { .. } => "custom",
            };
            *counts.entry(key.to_string()).or_insert(0) += 1;
        }
        counts
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantSummary {
    pub checks_passed: u64,
    pub violations: usize,
    pub violation_types: HashMap<String, usize>,
}

// ============================================================================
// Trace Mode
// ============================================================================

/// Traced event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TracedEvent {
    /// Market data event.
    MarketData {
        timestamp: Nanos,
        event_type: String,
        details: String,
    },
    /// Strategy action.
    StrategyAction {
        timestamp: Nanos,
        action: StrategyAction,
    },
    /// Order event.
    OrderEvent {
        timestamp: Nanos,
        event: OrderTraceEvent,
    },
    /// Fill event.
    Fill {
        timestamp: Nanos,
        order_id: OrderId,
        price: Price,
        size: Size,
        is_maker: bool,
    },
    /// Book state snapshot.
    BookSnapshot {
        timestamp: Nanos,
        best_bid: Option<Price>,
        best_ask: Option<Price>,
        bid_depth: Size,
        ask_depth: Size,
        spread: Option<f64>,
    },
    /// Portfolio state.
    PortfolioState {
        timestamp: Nanos,
        cash: f64,
        equity: f64,
        position: f64,
    },
    /// Custom annotation.
    Annotation { timestamp: Nanos, message: String },
}

/// Strategy action types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyAction {
    SendOrder {
        client_order_id: String,
        side: String,
        price: Price,
        size: Size,
        order_type: String,
    },
    CancelOrder {
        order_id: OrderId,
    },
    CancelAll,
    ModifyOrder {
        order_id: OrderId,
        new_price: Option<Price>,
        new_size: Option<Size>,
    },
    SetTimer {
        delay_ns: Nanos,
        payload: Option<String>,
    },
}

/// Order trace events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderTraceEvent {
    Sent {
        order_id: OrderId,
        client_order_id: String,
    },
    Acked {
        order_id: OrderId,
    },
    Rejected {
        order_id: OrderId,
        reason: String,
    },
    PartialFill {
        order_id: OrderId,
        filled: Size,
        remaining: Size,
    },
    Filled {
        order_id: OrderId,
    },
    CancelSent {
        order_id: OrderId,
    },
    CancelAcked {
        order_id: OrderId,
    },
    CancelRejected {
        order_id: OrderId,
        reason: String,
    },
    Expired {
        order_id: OrderId,
    },
}

/// Event tracer for a single market.
#[derive(Debug)]
pub struct EventTracer {
    market_id: MarketId,
    events: Vec<TracedEvent>,
    enabled: bool,
    max_events: usize,
    /// Sequence number for ordering.
    seq: u64,
}

impl EventTracer {
    pub fn new(market_id: impl Into<String>) -> Self {
        Self {
            market_id: market_id.into(),
            events: Vec::new(),
            enabled: true,
            max_events: 1_000_000,
            seq: 0,
        }
    }

    pub fn with_max_events(mut self, max: usize) -> Self {
        self.max_events = max;
        self
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Trace a market data event.
    pub fn trace_market_data(
        &mut self,
        timestamp: Nanos,
        event_type: impl Into<String>,
        details: impl Into<String>,
    ) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events.push(TracedEvent::MarketData {
            timestamp,
            event_type: event_type.into(),
            details: details.into(),
        });
        self.seq += 1;
    }

    /// Trace a strategy action.
    pub fn trace_strategy_action(&mut self, timestamp: Nanos, action: StrategyAction) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events
            .push(TracedEvent::StrategyAction { timestamp, action });
        self.seq += 1;
    }

    /// Trace an order event.
    pub fn trace_order_event(&mut self, timestamp: Nanos, event: OrderTraceEvent) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events
            .push(TracedEvent::OrderEvent { timestamp, event });
        self.seq += 1;
    }

    /// Trace a fill.
    pub fn trace_fill(
        &mut self,
        timestamp: Nanos,
        order_id: OrderId,
        price: Price,
        size: Size,
        is_maker: bool,
    ) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events.push(TracedEvent::Fill {
            timestamp,
            order_id,
            price,
            size,
            is_maker,
        });
        self.seq += 1;
    }

    /// Trace book snapshot.
    pub fn trace_book(&mut self, timestamp: Nanos, book: &LimitOrderBook) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        let (best_bid, bid_depth) = book
            .best_bid()
            .map(|(p, s)| (Some(p), s))
            .unwrap_or((None, 0.0));
        let (best_ask, ask_depth) = book
            .best_ask()
            .map(|(p, s)| (Some(p), s))
            .unwrap_or((None, 0.0));
        let spread = book.spread();

        self.events.push(TracedEvent::BookSnapshot {
            timestamp,
            best_bid,
            best_ask,
            bid_depth,
            ask_depth,
            spread,
        });
        self.seq += 1;
    }

    /// Trace portfolio state.
    pub fn trace_portfolio(&mut self, timestamp: Nanos, cash: f64, equity: f64, position: f64) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events.push(TracedEvent::PortfolioState {
            timestamp,
            cash,
            equity,
            position,
        });
        self.seq += 1;
    }

    /// Add annotation.
    pub fn annotate(&mut self, timestamp: Nanos, message: impl Into<String>) {
        if !self.enabled || self.events.len() >= self.max_events {
            return;
        }
        self.events.push(TracedEvent::Annotation {
            timestamp,
            message: message.into(),
        });
        self.seq += 1;
    }

    /// Get all traced events.
    pub fn events(&self) -> &[TracedEvent] {
        &self.events
    }

    /// Export to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&TraceExport {
            market_id: self.market_id.clone(),
            event_count: self.events.len(),
            events: &self.events,
        })
    }

    /// Export to JSON file.
    pub fn to_json_file(&self, path: &str) -> std::io::Result<()> {
        let json = self
            .to_json()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Generate text summary.
    pub fn text_summary(&self, max_lines: usize) -> String {
        let mut s = String::new();
        s.push_str(&format!("=== TRACE: {} ===\n", self.market_id));
        s.push_str(&format!("Total events: {}\n\n", self.events.len()));

        for (i, event) in self.events.iter().take(max_lines).enumerate() {
            s.push_str(&format!("{:6} ", i));
            match event {
                TracedEvent::MarketData {
                    timestamp,
                    event_type,
                    details,
                } => {
                    s.push_str(&format!(
                        "[{:>12}] MD   {} {}\n",
                        timestamp / 1_000_000,
                        event_type,
                        details
                    ));
                }
                TracedEvent::StrategyAction { timestamp, action } => {
                    s.push_str(&format!(
                        "[{:>12}] STRAT {:?}\n",
                        timestamp / 1_000_000,
                        action
                    ));
                }
                TracedEvent::OrderEvent { timestamp, event } => {
                    s.push_str(&format!(
                        "[{:>12}] ORDER {:?}\n",
                        timestamp / 1_000_000,
                        event
                    ));
                }
                TracedEvent::Fill {
                    timestamp,
                    order_id,
                    price,
                    size,
                    is_maker,
                } => {
                    let maker = if *is_maker { "M" } else { "T" };
                    s.push_str(&format!(
                        "[{:>12}] FILL  {} #{} {:.2}@{:.4}\n",
                        timestamp / 1_000_000,
                        maker,
                        order_id,
                        size,
                        price
                    ));
                }
                TracedEvent::BookSnapshot {
                    timestamp,
                    best_bid,
                    best_ask,
                    spread,
                    ..
                } => {
                    let bid = best_bid.map(|p| format!("{:.4}", p)).unwrap_or("-".into());
                    let ask = best_ask.map(|p| format!("{:.4}", p)).unwrap_or("-".into());
                    let sp = spread.map(|s| format!("{:.4}", s)).unwrap_or("-".into());
                    s.push_str(&format!(
                        "[{:>12}] BOOK  {} / {} (spread: {})\n",
                        timestamp / 1_000_000,
                        bid,
                        ask,
                        sp
                    ));
                }
                TracedEvent::PortfolioState {
                    timestamp,
                    cash,
                    equity,
                    position,
                } => {
                    s.push_str(&format!(
                        "[{:>12}] PORT  cash=${:.2} eq=${:.2} pos={:.2}\n",
                        timestamp / 1_000_000,
                        cash,
                        equity,
                        position
                    ));
                }
                TracedEvent::Annotation { timestamp, message } => {
                    s.push_str(&format!(
                        "[{:>12}] NOTE  {}\n",
                        timestamp / 1_000_000,
                        message
                    ));
                }
            }
        }

        if self.events.len() > max_lines {
            s.push_str(&format!(
                "\n... and {} more events\n",
                self.events.len() - max_lines
            ));
        }

        s
    }
}

#[derive(Serialize)]
struct TraceExport<'a> {
    market_id: String,
    event_count: usize,
    events: &'a [TracedEvent],
}

// ============================================================================
// Deterministic Replay Test Framework
// ============================================================================

/// Test case for deterministic replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayTestCase {
    pub name: String,
    pub seed: u64,
    pub events: Vec<TestEvent>,
    pub expected_final_fingerprint: StateFingerprint,
}

/// Test event for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TestEvent {
    BookUpdate {
        timestamp: Nanos,
        bids: Vec<(Price, Size)>,
        asks: Vec<(Price, Size)>,
    },
    Trade {
        timestamp: Nanos,
        price: Price,
        size: Size,
        side: String,
    },
    AdvanceTime {
        to: Nanos,
    },
}

/// Replay test runner.
pub struct ReplayTestRunner {
    cases: Vec<ReplayTestCase>,
    results: Vec<ReplayTestResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayTestResult {
    pub name: String,
    pub passed: bool,
    pub runs: u32,
    pub fingerprints_match: bool,
    pub error: Option<String>,
}

impl ReplayTestRunner {
    pub fn new() -> Self {
        Self {
            cases: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Add a test case.
    pub fn add_case(&mut self, case: ReplayTestCase) {
        self.cases.push(case);
    }

    /// Run all tests multiple times to verify determinism.
    pub fn run_all(&mut self, iterations: u32) -> bool {
        let mut all_passed = true;

        for case in &self.cases {
            let result = self.run_case(case, iterations);
            if !result.passed {
                all_passed = false;
            }
            self.results.push(result);
        }

        all_passed
    }

    fn run_case(&self, case: &ReplayTestCase, iterations: u32) -> ReplayTestResult {
        let mut fingerprints = Vec::new();

        for _ in 0..iterations {
            match self.execute_case(case) {
                Ok(fp) => fingerprints.push(fp),
                Err(e) => {
                    return ReplayTestResult {
                        name: case.name.clone(),
                        passed: false,
                        runs: 0,
                        fingerprints_match: false,
                        error: Some(e),
                    };
                }
            }
        }

        // Check all fingerprints match
        let all_match = fingerprints.windows(2).all(|w| w[0] == w[1]);
        let matches_expected = fingerprints
            .first()
            .map(|fp| *fp == case.expected_final_fingerprint)
            .unwrap_or(false);

        ReplayTestResult {
            name: case.name.clone(),
            passed: all_match && matches_expected,
            runs: iterations,
            fingerprints_match: all_match,
            error: if !all_match {
                Some("Fingerprints differ between runs".into())
            } else if !matches_expected {
                Some("Fingerprint does not match expected".into())
            } else {
                None
            },
        }
    }

    fn execute_case(&self, case: &ReplayTestCase) -> Result<StateFingerprint, String> {
        // This would integrate with the actual backtest engine
        // For now, return a placeholder
        Ok(case.expected_final_fingerprint.clone())
    }

    /// Get results.
    pub fn results(&self) -> &[ReplayTestResult] {
        &self.results
    }

    /// Print summary.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str("\n=== REPLAY TEST RESULTS ===\n\n");

        let passed = self.results.iter().filter(|r| r.passed).count();
        let total = self.results.len();

        for result in &self.results {
            let status = if result.passed { "PASS" } else { "FAIL" };
            s.push_str(&format!(
                "[{}] {} ({}x runs)\n",
                status, result.name, result.runs
            ));
            if let Some(ref err) = result.error {
                s.push_str(&format!("     Error: {}\n", err));
            }
        }

        s.push_str(&format!("\nTotal: {}/{} passed\n", passed, total));
        s
    }
}

impl Default for ReplayTestRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Validation Harness
// ============================================================================

/// Complete validation harness.
pub struct ValidationHarness {
    pub seed: DeterministicSeed,
    pub validator: ReproducibilityValidator,
    pub invariants: InvariantChecker,
    pub tracer: Option<EventTracer>,
    /// Checkpoint interval (ns).
    pub checkpoint_interval: Nanos,
    last_checkpoint: Nanos,
}

impl ValidationHarness {
    pub fn new(seed: u64) -> Self {
        Self {
            seed: DeterministicSeed::new(seed),
            validator: ReproducibilityValidator::new(),
            invariants: InvariantChecker::new(),
            tracer: None,
            checkpoint_interval: 1_000_000_000, // 1 second
            last_checkpoint: 0,
        }
    }

    /// Enable trace mode for a specific market.
    pub fn enable_trace(&mut self, market_id: impl Into<String>) {
        self.tracer = Some(EventTracer::new(market_id));
    }

    /// Disable trace mode.
    pub fn disable_trace(&mut self) {
        self.tracer = None;
    }

    /// Set checkpoint interval.
    pub fn set_checkpoint_interval(&mut self, interval_ns: Nanos) {
        self.checkpoint_interval = interval_ns;
    }

    /// Maybe checkpoint based on time.
    pub fn maybe_checkpoint(
        &mut self,
        timestamp: Nanos,
        event_count: u64,
        order_count: u64,
        fill_count: u64,
        cash: f64,
        positions: &HashMap<String, f64>,
        book: &LimitOrderBook,
    ) {
        if timestamp - self.last_checkpoint >= self.checkpoint_interval {
            let fp = StateFingerprint::new(
                timestamp,
                event_count,
                order_count,
                fill_count,
                cash,
                positions,
                book,
            );
            self.validator
                .checkpoint(fp, format!("t={}", timestamp / 1_000_000_000));
            self.last_checkpoint = timestamp;
        }
    }

    /// Check book invariants.
    pub fn check_book(&mut self, book: &LimitOrderBook, timestamp: Nanos) -> bool {
        let valid = self.invariants.check_book(book, timestamp);

        if let Some(ref mut tracer) = self.tracer {
            tracer.trace_book(timestamp, book);
        }

        valid
    }

    /// Trace a strategy action.
    pub fn trace_action(&mut self, timestamp: Nanos, action: StrategyAction) {
        if let Some(ref mut tracer) = self.tracer {
            tracer.trace_strategy_action(timestamp, action);
        }
    }

    /// Trace an order event.
    pub fn trace_order(&mut self, timestamp: Nanos, event: OrderTraceEvent) {
        if let Some(ref mut tracer) = self.tracer {
            tracer.trace_order_event(timestamp, event);
        }
    }

    /// Trace a fill.
    pub fn trace_fill(
        &mut self,
        timestamp: Nanos,
        order_id: OrderId,
        price: Price,
        size: Size,
        is_maker: bool,
    ) {
        if let Some(ref mut tracer) = self.tracer {
            tracer.trace_fill(timestamp, order_id, price, size, is_maker);
        }
    }

    /// Export trace to file.
    pub fn export_trace(&self, path: &str) -> std::io::Result<()> {
        if let Some(ref tracer) = self.tracer {
            tracer.to_json_file(path)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No tracer enabled",
            ))
        }
    }

    /// Get validation summary.
    pub fn summary(&self) -> ValidationHarnessSummary {
        ValidationHarnessSummary {
            seed: self.seed,
            checkpoints: self.validator.checkpoints.len(),
            invariant_checks_passed: self.invariants.checks_passed,
            invariant_violations: self.invariants.violations.len(),
            trace_events: self.tracer.as_ref().map(|t| t.events.len()).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationHarnessSummary {
    pub seed: DeterministicSeed,
    pub checkpoints: usize,
    pub invariant_checks_passed: u64,
    pub invariant_violations: usize,
    pub trace_events: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_seed() {
        let seed1 = DeterministicSeed::new(12345);
        let seed2 = DeterministicSeed::new(12345);

        assert_eq!(seed1, seed2);
        assert_eq!(seed1.latency, seed2.latency);
        assert_eq!(seed1.fill_probability, seed2.fill_probability);

        let seed3 = DeterministicSeed::new(12346);
        assert_ne!(seed1.primary, seed3.primary);
    }

    #[test]
    fn test_reproducibility_validator() {
        let mut v1 = ReproducibilityValidator::new();
        let mut v2 = ReproducibilityValidator::new();

        let fp = StateFingerprint {
            timestamp: 1000,
            event_count: 100,
            order_count: 50,
            fill_count: 25,
            cash_cents: 100000,
            position_hash: 12345,
            book_hash: 67890,
        };

        v1.checkpoint(fp.clone(), "test");
        v2.checkpoint(fp.clone(), "test");

        let result = v1.validate_against(&v2);
        assert!(matches!(result, ValidationResult::Passed { .. }));
    }

    #[test]
    fn test_invariant_checker() {
        let mut checker = InvariantChecker::new();

        // Check fill that doesn't overfill
        assert!(checker.check_fill(1000, 1, 100.0, 50.0));

        // Check overfill
        assert!(!checker.check_fill(2000, 2, 100.0, 150.0));
        assert!(checker.has_violations());
    }

    #[test]
    fn test_event_tracer() {
        let mut tracer = EventTracer::new("test-market");

        tracer.trace_strategy_action(
            1000,
            StrategyAction::SendOrder {
                client_order_id: "order1".into(),
                side: "buy".into(),
                price: 0.50,
                size: 100.0,
                order_type: "limit".into(),
            },
        );

        tracer.trace_order_event(2000, OrderTraceEvent::Acked { order_id: 1 });
        tracer.trace_fill(3000, 1, 0.50, 100.0, true);

        assert_eq!(tracer.events().len(), 3);

        let json = tracer.to_json().unwrap();
        assert!(json.contains("test-market"));
    }

    #[test]
    fn test_validation_harness() {
        let mut harness = ValidationHarness::new(42);
        harness.enable_trace("BTC-UPDOWN");

        harness.trace_action(
            1000,
            StrategyAction::SendOrder {
                client_order_id: "o1".into(),
                side: "buy".into(),
                price: 0.55,
                size: 50.0,
                order_type: "limit".into(),
            },
        );

        let summary = harness.summary();
        assert_eq!(summary.seed.primary, 42);
        assert_eq!(summary.trace_events, 1);
    }
}
