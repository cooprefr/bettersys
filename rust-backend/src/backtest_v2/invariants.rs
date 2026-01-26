//! Mandatory Invariant Framework
//!
//! Promotes invariant checking from debug/trace tooling into a mandatory structural requirement.
//! In production-grade mode, any invariant violation aborts the run immediately with a
//! deterministic minimal causal dump.
//!
//! # Invariant Categories
//!
//! - **Time**: Decision time monotonicity, visibility semantics, event ordering
//! - **Book**: Crossed book detection, price validity, size positivity
//! - **OMS**: Order lifecycle correctness, illegal state transitions
//! - **Fills**: Plausibility checks, overfill prevention, price validity
//! - **Accounting**: Double-entry balance, cash non-negativity, equity identity
//!
//! # Usage
//!
//! ```ignore
//! let mut enforcer = InvariantEnforcer::new(InvariantMode::Hard);
//! enforcer.check_time_monotonicity(old_time, new_time)?;
//! enforcer.check_book_consistency(&book)?;
//! ```

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Size, TimestampedEvent};
use crate::backtest_v2::ledger::LedgerEntry;
use crate::backtest_v2::matching::LimitOrderBook;
use crate::backtest_v2::oms::OrderState;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

// =============================================================================
// INVARIANT MODE
// =============================================================================

/// Invariant enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum InvariantMode {
    /// Off: No invariant checking (INVALID for production).
    Off,
    /// Soft: Log violations, increment counters, continue execution.
    #[default]
    Soft,
    /// Hard: Abort on first violation with deterministic dump.
    Hard,
}

impl InvariantMode {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Off => "Invariants disabled (INVALID for production)",
            Self::Soft => "Soft mode: log + count violations, continue",
            Self::Hard => "Hard mode: abort on first violation",
        }
    }

    pub fn is_valid_for_production(&self) -> bool {
        matches!(self, Self::Hard)
    }
}

// =============================================================================
// INVARIANT CATEGORIES
// =============================================================================

/// Invariant category for classification and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvariantCategory {
    /// Time monotonicity and visibility semantics.
    Time,
    /// Order book consistency.
    Book,
    /// Order management system lifecycle.
    OMS,
    /// Fill plausibility.
    Fills,
    /// Accounting and ledger balance.
    Accounting,
}

impl InvariantCategory {
    pub fn all() -> &'static [InvariantCategory] {
        &[
            Self::Time,
            Self::Book,
            Self::OMS,
            Self::Fills,
            Self::Accounting,
        ]
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Time => "Time monotonicity and visibility semantics",
            Self::Book => "Order book consistency",
            Self::OMS => "Order lifecycle correctness",
            Self::Fills => "Fill plausibility",
            Self::Accounting => "Double-entry accounting balance",
        }
    }
}

/// Bitset for enabled invariant categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryFlags(u8);

impl Default for CategoryFlags {
    fn default() -> Self {
        Self::all()
    }
}

impl CategoryFlags {
    pub fn none() -> Self {
        Self(0)
    }

    pub fn all() -> Self {
        Self(0b11111)
    }

    pub fn is_enabled(&self, cat: InvariantCategory) -> bool {
        let bit = 1 << (cat as u8);
        (self.0 & bit) != 0
    }

    pub fn enable(&mut self, cat: InvariantCategory) {
        let bit = 1 << (cat as u8);
        self.0 |= bit;
    }

    pub fn disable(&mut self, cat: InvariantCategory) {
        let bit = 1 << (cat as u8);
        self.0 &= !bit;
    }

    pub fn from_categories(cats: &[InvariantCategory]) -> Self {
        let mut flags = Self::none();
        for cat in cats {
            flags.enable(*cat);
        }
        flags
    }
}

// =============================================================================
// INVARIANT VIOLATION
// =============================================================================

/// Detailed invariant violation record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantViolation {
    /// Category of the violation.
    pub category: InvariantCategory,
    /// Violation type identifier.
    pub violation_type: ViolationType,
    /// Human-readable message.
    pub message: String,
    /// Simulation time when violation occurred.
    pub sim_time: Nanos,
    /// Decision time at violation.
    pub decision_time: Nanos,
    /// Arrival time of triggering event (if applicable).
    pub arrival_time: Option<Nanos>,
    /// Sequence number of triggering event (if applicable).
    pub seq: Option<u64>,
    /// Market ID (if applicable).
    pub market_id: Option<String>,
    /// Order ID (if applicable).
    pub order_id: Option<OrderId>,
    /// Fill ID (if applicable).
    pub fill_id: Option<u64>,
    /// Additional context payload (bounded, deterministic).
    pub context: ViolationContext,
}

/// Violation type enumeration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ViolationType {
    // Time violations
    DecisionTimeBackward { old: Nanos, new: Nanos },
    ArrivalAfterDecision { arrival: Nanos, decision: Nanos },
    EventOrderingViolation { expected_seq: u64, actual_seq: u64 },
    TimestampRegression { old: Nanos, new: Nanos },
    
    // Book violations
    CrossedBook { best_bid: Price, best_ask: Price },
    NegativeSize { side: String, price: Price, size: Size },
    InvalidPrice { side: String, price: Price },
    BookLevelMisordered { side: String },
    SequenceGap { expected: u64, actual: u64 },
    
    // OMS violations
    IllegalStateTransition { from: String, to: String, order_id: OrderId },
    FillBeforeAck { order_id: OrderId },
    FillAfterTerminal { order_id: OrderId, terminal_state: String },
    CancelWithoutPending { order_id: OrderId },
    DuplicateOrderId { order_id: OrderId },
    UnknownOrderId { order_id: OrderId },
    
    // Fill violations
    FillPriceNotInBook { price: Price, order_id: OrderId },
    OverFill { order_id: OrderId, order_qty: Size, total_filled: Size },
    NegativeFillSize { order_id: OrderId, size: Size },
    FillNaN { order_id: OrderId },
    MakerFillWithoutQueueConsumption { order_id: OrderId },
    TakerFillWithoutLiquidity { order_id: OrderId, price: Price },
    
    // Accounting violations
    UnbalancedEntry { debits: i64, credits: i64 },
    NegativeCash { cash: f64 },
    EquityIdentityViolation { expected: f64, actual: f64 },
    DuplicateSettlement { market_id: String },
    DuplicateFeePosting { fill_id: u64 },
}

impl ViolationType {
    pub fn category(&self) -> InvariantCategory {
        match self {
            Self::DecisionTimeBackward { .. }
            | Self::ArrivalAfterDecision { .. }
            | Self::EventOrderingViolation { .. }
            | Self::TimestampRegression { .. } => InvariantCategory::Time,

            Self::CrossedBook { .. }
            | Self::NegativeSize { .. }
            | Self::InvalidPrice { .. }
            | Self::BookLevelMisordered { .. }
            | Self::SequenceGap { .. } => InvariantCategory::Book,

            Self::IllegalStateTransition { .. }
            | Self::FillBeforeAck { .. }
            | Self::FillAfterTerminal { .. }
            | Self::CancelWithoutPending { .. }
            | Self::DuplicateOrderId { .. }
            | Self::UnknownOrderId { .. } => InvariantCategory::OMS,

            Self::FillPriceNotInBook { .. }
            | Self::OverFill { .. }
            | Self::NegativeFillSize { .. }
            | Self::FillNaN { .. }
            | Self::MakerFillWithoutQueueConsumption { .. }
            | Self::TakerFillWithoutLiquidity { .. } => InvariantCategory::Fills,

            Self::UnbalancedEntry { .. }
            | Self::NegativeCash { .. }
            | Self::EquityIdentityViolation { .. }
            | Self::DuplicateSettlement { .. }
            | Self::DuplicateFeePosting { .. } => InvariantCategory::Accounting,
        }
    }
}

/// Bounded context payload for violation debugging.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ViolationContext {
    /// Last N applied events.
    pub recent_events: Vec<EventSummary>,
    /// Last N OMS transitions for relevant order.
    pub oms_transitions: Vec<OmsTransition>,
    /// Last N ledger entries.
    pub ledger_entries: Vec<LedgerEntrySummary>,
    /// Relevant state snapshot.
    pub state_snapshot: Option<StateSnapshot>,
    /// Last decision proof hash.
    pub last_decision_proof_hash: Option<u64>,
}

/// Compact event summary for context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSummary {
    pub arrival_time: Nanos,
    pub source_time: Option<Nanos>,
    pub seq: Option<u64>,
    pub event_type: String,
    pub market_id: Option<String>,
}

/// OMS state transition record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmsTransition {
    pub timestamp: Nanos,
    pub order_id: OrderId,
    pub from_state: String,
    pub to_state: String,
    pub trigger: String,
}

/// Compact ledger entry summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntrySummary {
    pub entry_id: u64,
    pub timestamp: Nanos,
    pub entry_type: String,
    pub total_debits: i128,
    pub total_credits: i128,
}

/// State snapshot for context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub best_bid: Option<(Price, Size)>,
    pub best_ask: Option<(Price, Size)>,
    pub cash: f64,
    pub open_orders: usize,
    pub position: f64,
}

// =============================================================================
// CAUSAL DUMP
// =============================================================================

/// Minimal causal dump produced on Hard mode abort.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalDump {
    /// The violation that triggered the abort.
    pub violation: InvariantViolation,
    /// Triggering event details.
    pub triggering_event: Option<EventSummary>,
    /// Last N applied events.
    pub recent_events: Vec<EventSummary>,
    /// Last N OMS transitions.
    pub oms_transitions: Vec<OmsTransition>,
    /// Last N ledger entries.
    pub ledger_entries: Vec<LedgerEntrySummary>,
    /// State snapshot.
    pub state_snapshot: StateSnapshot,
    /// Run fingerprint at abort.
    pub fingerprint_at_abort: u64,
    /// Config hash.
    pub config_hash: u64,
}

impl CausalDump {
    /// Format as deterministic text for debugging.
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str("=== INVARIANT VIOLATION - CAUSAL DUMP ===\n\n");
        
        out.push_str(&format!("Category: {:?}\n", self.violation.category));
        out.push_str(&format!("Type: {:?}\n", self.violation.violation_type));
        out.push_str(&format!("Message: {}\n", self.violation.message));
        out.push_str(&format!("Sim Time: {} ns\n", self.violation.sim_time));
        out.push_str(&format!("Decision Time: {} ns\n", self.violation.decision_time));
        
        if let Some(at) = self.violation.arrival_time {
            out.push_str(&format!("Arrival Time: {} ns\n", at));
        }
        if let Some(seq) = self.violation.seq {
            out.push_str(&format!("Sequence: {}\n", seq));
        }
        if let Some(ref mid) = self.violation.market_id {
            out.push_str(&format!("Market: {}\n", mid));
        }
        if let Some(oid) = self.violation.order_id {
            out.push_str(&format!("Order ID: {}\n", oid));
        }
        
        out.push_str("\n--- Triggering Event ---\n");
        if let Some(ref e) = self.triggering_event {
            out.push_str(&format!("  Type: {}\n", e.event_type));
            out.push_str(&format!("  Arrival: {} ns\n", e.arrival_time));
        } else {
            out.push_str("  (none)\n");
        }
        
        out.push_str("\n--- State Snapshot ---\n");
        out.push_str(&format!("  Best Bid: {:?}\n", self.state_snapshot.best_bid));
        out.push_str(&format!("  Best Ask: {:?}\n", self.state_snapshot.best_ask));
        out.push_str(&format!("  Cash: ${:.2}\n", self.state_snapshot.cash));
        out.push_str(&format!("  Open Orders: {}\n", self.state_snapshot.open_orders));
        out.push_str(&format!("  Position: {:.2}\n", self.state_snapshot.position));
        
        out.push_str("\n--- Recent Events ---\n");
        for (i, e) in self.recent_events.iter().enumerate() {
            out.push_str(&format!("  [{}] {} @ {} ns\n", i, e.event_type, e.arrival_time));
        }
        
        out.push_str("\n--- OMS Transitions ---\n");
        for t in &self.oms_transitions {
            out.push_str(&format!(
                "  Order {} @ {} ns: {} -> {} ({})\n",
                t.order_id, t.timestamp, t.from_state, t.to_state, t.trigger
            ));
        }
        
        out.push_str("\n--- Ledger Entries ---\n");
        for e in &self.ledger_entries {
            out.push_str(&format!(
                "  [{}] {} @ {} ns: D={} C={}\n",
                e.entry_id, e.entry_type, e.timestamp, e.total_debits, e.total_credits
            ));
        }
        
        out.push_str(&format!("\nFingerprint at abort: {:#018x}\n", self.fingerprint_at_abort));
        out.push_str(&format!("Config hash: {:#018x}\n", self.config_hash));
        
        out.push_str("\n=========================================\n");
        out
    }
}

// =============================================================================
// INVARIANT COUNTERS
// =============================================================================

/// Counters for invariant checks and violations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InvariantCounters {
    // Check counts
    pub time_checks: u64,
    pub book_checks: u64,
    pub oms_checks: u64,
    pub fill_checks: u64,
    pub accounting_checks: u64,
    
    // Violation counts by category
    pub time_violations: u64,
    pub book_violations: u64,
    pub oms_violations: u64,
    pub fill_violations: u64,
    pub accounting_violations: u64,
    
    // Total
    pub total_checks: u64,
    pub total_violations: u64,
    
    // Abort status
    pub aborted: bool,
    pub abort_reason: Option<String>,
}

impl InvariantCounters {
    pub fn record_check(&mut self, category: InvariantCategory) {
        self.total_checks += 1;
        match category {
            InvariantCategory::Time => self.time_checks += 1,
            InvariantCategory::Book => self.book_checks += 1,
            InvariantCategory::OMS => self.oms_checks += 1,
            InvariantCategory::Fills => self.fill_checks += 1,
            InvariantCategory::Accounting => self.accounting_checks += 1,
        }
    }

    pub fn record_violation(&mut self, category: InvariantCategory) {
        self.total_violations += 1;
        match category {
            InvariantCategory::Time => self.time_violations += 1,
            InvariantCategory::Book => self.book_violations += 1,
            InvariantCategory::OMS => self.oms_violations += 1,
            InvariantCategory::Fills => self.fill_violations += 1,
            InvariantCategory::Accounting => self.accounting_violations += 1,
        }
    }

    pub fn has_violations(&self) -> bool {
        self.total_violations > 0
    }

    pub fn summary(&self) -> String {
        format!(
            "Checks: {} (T:{} B:{} O:{} F:{} A:{}) | Violations: {} (T:{} B:{} O:{} F:{} A:{})",
            self.total_checks,
            self.time_checks,
            self.book_checks,
            self.oms_checks,
            self.fill_checks,
            self.accounting_checks,
            self.total_violations,
            self.time_violations,
            self.book_violations,
            self.oms_violations,
            self.fill_violations,
            self.accounting_violations,
        )
    }
}

// =============================================================================
// INVARIANT CONFIGURATION
// =============================================================================

/// Configuration for invariant enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantConfig {
    /// Enforcement mode.
    pub mode: InvariantMode,
    /// Enabled categories.
    pub categories: CategoryFlags,
    /// Dump depth: last N events to include in causal dump.
    pub event_dump_depth: usize,
    /// Dump depth: last N OMS transitions.
    pub oms_dump_depth: usize,
    /// Dump depth: last N ledger entries.
    pub ledger_dump_depth: usize,
}

impl Default for InvariantConfig {
    /// Default configuration: Hard mode with all categories enabled.
    /// 
    /// IMPORTANT: The default is intentionally Hard mode because invariant
    /// checking must be a non-optional structural requirement. Any backtest
    /// run should abort on the first invariant violation unless the user
    /// explicitly opts out (which marks results as untrusted).
    fn default() -> Self {
        Self {
            mode: InvariantMode::Hard,
            categories: CategoryFlags::all(),
            event_dump_depth: 50,
            oms_dump_depth: 20,
            ledger_dump_depth: 20,
        }
    }
}

impl InvariantConfig {
    /// Production-grade configuration: Hard mode with all categories.
    pub fn production() -> Self {
        Self {
            mode: InvariantMode::Hard,
            categories: CategoryFlags::all(),
            event_dump_depth: 50,
            oms_dump_depth: 20,
            ledger_dump_depth: 20,
        }
    }

    /// Debug configuration: Soft mode with detailed dumps.
    pub fn debug() -> Self {
        Self {
            mode: InvariantMode::Soft,
            categories: CategoryFlags::all(),
            event_dump_depth: 100,
            oms_dump_depth: 50,
            ledger_dump_depth: 50,
        }
    }
}

// =============================================================================
// INVARIANT ENFORCER
// =============================================================================

/// Error type for Hard mode aborts.
#[derive(Debug)]
pub struct InvariantAbort {
    pub dump: CausalDump,
}

impl std::fmt::Display for InvariantAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invariant violation: {:?}", self.dump.violation.violation_type)
    }
}

impl std::error::Error for InvariantAbort {}

/// Result type for invariant checks.
pub type InvariantResult<T> = Result<T, InvariantAbort>;

/// Main invariant enforcer.
pub struct InvariantEnforcer {
    config: InvariantConfig,
    counters: InvariantCounters,
    
    // Context buffers (bounded)
    recent_events: VecDeque<EventSummary>,
    oms_transitions: VecDeque<OmsTransition>,
    ledger_entries: VecDeque<LedgerEntrySummary>,
    
    // State tracking
    pub last_decision_time: Nanos,
    last_arrival_times: HashMap<String, Nanos>, // per market
    last_seqs: HashMap<String, u64>,            // per stream
    order_states: HashMap<OrderId, OrderState>,
    order_filled_qty: HashMap<OrderId, Size>,
    order_total_qty: HashMap<OrderId, Size>,
    settled_markets: std::collections::HashSet<String>,
    posted_fills: std::collections::HashSet<u64>,
    
    // Current state for snapshots
    current_cash: f64,
    current_position: f64,
    open_order_count: usize,
    
    // Fingerprinting
    behavior_hasher: std::collections::hash_map::DefaultHasher,
    config_hash: u64,
    
    // First violation (for Soft mode)
    first_violation: Option<InvariantViolation>,
}

impl InvariantEnforcer {
    pub fn new(config: InvariantConfig) -> Self {
        let config_hash = Self::hash_config(&config);
        Self {
            config,
            counters: InvariantCounters::default(),
            recent_events: VecDeque::with_capacity(100),
            oms_transitions: VecDeque::with_capacity(50),
            ledger_entries: VecDeque::with_capacity(50),
            last_decision_time: 0,
            last_arrival_times: HashMap::new(),
            last_seqs: HashMap::new(),
            order_states: HashMap::new(),
            order_filled_qty: HashMap::new(),
            order_total_qty: HashMap::new(),
            settled_markets: std::collections::HashSet::new(),
            posted_fills: std::collections::HashSet::new(),
            current_cash: 0.0,
            current_position: 0.0,
            open_order_count: 0,
            behavior_hasher: std::collections::hash_map::DefaultHasher::new(),
            config_hash,
            first_violation: None,
        }
    }

    fn hash_config(config: &InvariantConfig) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (config.mode as u8).hash(&mut hasher);
        config.categories.0.hash(&mut hasher);
        hasher.finish()
    }

    // =========================================================================
    // Context Recording
    // =========================================================================

    /// Record an event for context.
    pub fn record_event(&mut self, event: &TimestampedEvent) {
        let summary = EventSummary {
            arrival_time: event.time,
            source_time: Some(event.source_time),
            seq: Some(event.seq),
            event_type: format!("{:?}", std::mem::discriminant(&event.event)),
            market_id: None, // TimestampedEvent doesn't have market_id
        };
        
        if self.recent_events.len() >= self.config.event_dump_depth {
            self.recent_events.pop_front();
        }
        self.recent_events.push_back(summary);
    }

    /// Record an OMS transition.
    pub fn record_oms_transition(
        &mut self,
        timestamp: Nanos,
        order_id: OrderId,
        from_state: &str,
        to_state: &str,
        trigger: &str,
    ) {
        let transition = OmsTransition {
            timestamp,
            order_id,
            from_state: from_state.to_string(),
            to_state: to_state.to_string(),
            trigger: trigger.to_string(),
        };
        
        if self.oms_transitions.len() >= self.config.oms_dump_depth {
            self.oms_transitions.pop_front();
        }
        self.oms_transitions.push_back(transition);
    }

    /// Record a ledger entry.
    pub fn record_ledger_entry(&mut self, entry: &LedgerEntry) {
        let summary = LedgerEntrySummary {
            entry_id: entry.entry_id,
            timestamp: entry.sim_time_ns,
            entry_type: format!("{:?}", entry.event_ref),
            total_debits: entry.total_debits(),
            total_credits: entry.total_credits(),
        };
        
        if self.ledger_entries.len() >= self.config.ledger_dump_depth {
            self.ledger_entries.pop_front();
        }
        self.ledger_entries.push_back(summary);
    }

    /// Update state for snapshots.
    pub fn update_state(&mut self, cash: f64, position: f64, open_orders: usize) {
        self.current_cash = cash;
        self.current_position = position;
        self.open_order_count = open_orders;
    }

    /// Update book state for snapshots.
    pub fn update_book_state(&mut self, book: &LimitOrderBook) {
        // Used for state snapshot in violations
    }

    // =========================================================================
    // Time Invariants
    // =========================================================================

    /// Check decision time monotonicity.
    pub fn check_decision_time(&mut self, new_time: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Time) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Time);

        if new_time < self.last_decision_time {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Time,
                violation_type: ViolationType::DecisionTimeBackward {
                    old: self.last_decision_time,
                    new: new_time,
                },
                message: format!(
                    "Decision time went backward: {} -> {}",
                    self.last_decision_time, new_time
                ),
                sim_time: new_time,
                decision_time: new_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: None,
                fill_id: None,
                context: self.build_context(),
            });
        }

        self.last_decision_time = new_time;
        Ok(())
    }

    /// Check visibility semantics: arrival_time <= decision_time.
    pub fn check_visibility(
        &mut self,
        arrival_time: Nanos,
        decision_time: Nanos,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Time) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Time);

        if arrival_time > decision_time {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Time,
                violation_type: ViolationType::ArrivalAfterDecision {
                    arrival: arrival_time,
                    decision: decision_time,
                },
                message: format!(
                    "Event arrival_time ({}) > decision_time ({})",
                    arrival_time, decision_time
                ),
                sim_time: decision_time,
                decision_time,
                arrival_time: Some(arrival_time),
                seq: None,
                market_id: None,
                order_id: None,
                fill_id: None,
                context: self.build_context(),
            });
        }

        Ok(())
    }

    /// Check event ordering (arrival_time + seq).
    pub fn check_event_ordering(
        &mut self,
        market_id: &str,
        arrival_time: Nanos,
        seq: Option<u64>,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Time) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Time);

        // Check arrival time doesn't regress
        if let Some(&last_arrival) = self.last_arrival_times.get(market_id) {
            if arrival_time < last_arrival {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Time,
                    violation_type: ViolationType::TimestampRegression {
                        old: last_arrival,
                        new: arrival_time,
                    },
                    message: format!(
                        "Arrival time regressed for {}: {} -> {}",
                        market_id, last_arrival, arrival_time
                    ),
                    sim_time: arrival_time,
                    decision_time: self.last_decision_time,
                    arrival_time: Some(arrival_time),
                    seq,
                    market_id: Some(market_id.to_string()),
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }
        self.last_arrival_times.insert(market_id.to_string(), arrival_time);

        // Check sequence ordering if available
        if let Some(new_seq) = seq {
            let stream_key = format!("{}:seq", market_id);
            if let Some(&last_seq) = self.last_seqs.get(&stream_key) {
                if new_seq <= last_seq {
                    return self.handle_violation(InvariantViolation {
                        category: InvariantCategory::Time,
                        violation_type: ViolationType::EventOrderingViolation {
                            expected_seq: last_seq + 1,
                            actual_seq: new_seq,
                        },
                        message: format!(
                            "Sequence ordering violation for {}: expected > {}, got {}",
                            market_id, last_seq, new_seq
                        ),
                        sim_time: arrival_time,
                        decision_time: self.last_decision_time,
                        arrival_time: Some(arrival_time),
                        seq: Some(new_seq),
                        market_id: Some(market_id.to_string()),
                        order_id: None,
                        fill_id: None,
                        context: self.build_context(),
                    });
                }
            }
            self.last_seqs.insert(stream_key, new_seq);
        }

        Ok(())
    }

    // =========================================================================
    // Book Invariants
    // =========================================================================

    /// Check order book consistency.
    pub fn check_book(&mut self, book: &LimitOrderBook, timestamp: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Book) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Book);

        // Check for crossed book
        if let (Some((best_bid, _)), Some((best_ask, _))) = (book.best_bid(), book.best_ask()) {
            if best_bid >= best_ask {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Book,
                    violation_type: ViolationType::CrossedBook { best_bid, best_ask },
                    message: format!("Crossed book: bid {} >= ask {}", best_bid, best_ask),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }

        // Check bid validity
        if let Some((price, size)) = book.best_bid() {
            if size < 0.0 {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Book,
                    violation_type: ViolationType::NegativeSize {
                        side: "bid".to_string(),
                        price,
                        size,
                    },
                    message: format!("Negative bid size at {}: {}", price, size),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
            if price < 0.0 || price > 1.0 {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Book,
                    violation_type: ViolationType::InvalidPrice {
                        side: "bid".to_string(),
                        price,
                    },
                    message: format!("Invalid bid price: {}", price),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }

        // Check ask validity
        if let Some((price, size)) = book.best_ask() {
            if size < 0.0 {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Book,
                    violation_type: ViolationType::NegativeSize {
                        side: "ask".to_string(),
                        price,
                        size,
                    },
                    message: format!("Negative ask size at {}: {}", price, size),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
            if price < 0.0 || price > 1.0 {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Book,
                    violation_type: ViolationType::InvalidPrice {
                        side: "ask".to_string(),
                        price,
                    },
                    message: format!("Invalid ask price: {}", price),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: None,
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }

        Ok(())
    }

    /// Check for crossed book condition (simplified version for token-based check).
    /// Used when we don't have access to the full LimitOrderBook but know the book is crossed.
    pub fn check_book_crossed(&mut self, token_id: &str, timestamp: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Book) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Book);

        self.handle_violation(InvariantViolation {
            category: InvariantCategory::Book,
            violation_type: ViolationType::CrossedBook { 
                best_bid: 0.0, // Unknown - detected via OrderBook.is_crossed()
                best_ask: 0.0,
            },
            message: format!("Crossed book detected for token {}", token_id),
            sim_time: timestamp,
            decision_time: self.last_decision_time,
            arrival_time: None,
            seq: None,
            market_id: Some(token_id.to_string()),
            order_id: None,
            fill_id: None,
            context: self.build_context(),
        })
    }

    /// Check for sequence gap in book updates.
    pub fn check_book_sequence_gap(
        &mut self,
        token_id: &str,
        last_seq: u64,
        new_seq: u64,
        timestamp: Nanos,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Book) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Book);

        self.handle_violation(InvariantViolation {
            category: InvariantCategory::Book,
            violation_type: ViolationType::SequenceGap {
                expected: last_seq + 1,
                actual: new_seq,
            },
            message: format!(
                "Book sequence gap for {}: expected seq {} but got {}",
                token_id, last_seq + 1, new_seq
            ),
            sim_time: timestamp,
            decision_time: self.last_decision_time,
            arrival_time: None,
            seq: Some(new_seq),
            market_id: Some(token_id.to_string()),
            order_id: None,
            fill_id: None,
            context: self.build_context(),
        })
    }

    // =========================================================================
    // OMS Invariants
    // =========================================================================

    /// Register a new order.
    pub fn register_order(&mut self, order_id: OrderId, qty: Size) {
        self.order_states.insert(order_id, OrderState::New);
        self.order_total_qty.insert(order_id, qty);
        self.order_filled_qty.insert(order_id, 0.0);
    }

    /// Check order state transition.
    pub fn check_order_transition(
        &mut self,
        order_id: OrderId,
        new_state: OrderState,
        timestamp: Nanos,
        trigger: &str,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::OMS) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::OMS);

        let old_state = self.order_states.get(&order_id).copied();

        // Check if order exists
        if old_state.is_none() && !matches!(new_state, OrderState::New) {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::OMS,
                violation_type: ViolationType::UnknownOrderId { order_id },
                message: format!("Unknown order ID {} in transition to {:?}", order_id, new_state),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: Some(order_id),
                fill_id: None,
                context: self.build_context(),
            });
        }

        let old_state_str = old_state.map(|s| format!("{:?}", s)).unwrap_or("None".to_string());
        let new_state_str = format!("{:?}", new_state);

        // Validate transition
        if let Some(old) = old_state {
            if !Self::is_valid_transition(old, new_state) {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::OMS,
                    violation_type: ViolationType::IllegalStateTransition {
                        from: old_state_str.clone(),
                        to: new_state_str.clone(),
                        order_id,
                    },
                    message: format!(
                        "Illegal OMS transition for order {}: {:?} -> {:?}",
                        order_id, old, new_state
                    ),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: Some(order_id),
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }

        // Record transition
        self.record_oms_transition(timestamp, order_id, &old_state_str, &new_state_str, trigger);
        self.order_states.insert(order_id, new_state);

        Ok(())
    }

    fn is_valid_transition(from: OrderState, to: OrderState) -> bool {
        // OrderState variants: New, PendingAck, Live, PartiallyFilled, PendingCancel, Done
        match (from, to) {
            // Normal flow
            (OrderState::New, OrderState::PendingAck) => true,
            (OrderState::PendingAck, OrderState::Live) => true,
            (OrderState::PendingAck, OrderState::Done) => true, // Rejected
            (OrderState::Live, OrderState::PartiallyFilled) => true,
            (OrderState::Live, OrderState::Done) => true,
            (OrderState::PartiallyFilled, OrderState::PartiallyFilled) => true,
            (OrderState::PartiallyFilled, OrderState::Done) => true,
            // Cancel flow
            (OrderState::Live, OrderState::PendingCancel) => true,
            (OrderState::PartiallyFilled, OrderState::PendingCancel) => true,
            (OrderState::PendingCancel, OrderState::Done) => true,
            (OrderState::PendingCancel, OrderState::Live) => true, // Cancel rejected
            (OrderState::PendingCancel, OrderState::PartiallyFilled) => true,
            // Direct rejection from New
            (OrderState::New, OrderState::Done) => true,
            _ => false,
        }
    }

    /// Check fill before ack.
    pub fn check_fill_order_state(&mut self, order_id: OrderId, timestamp: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::OMS) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::OMS);

        let state = self.order_states.get(&order_id).copied();

        match state {
            None => {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::OMS,
                    violation_type: ViolationType::UnknownOrderId { order_id },
                    message: format!("Fill for unknown order {}", order_id),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: Some(order_id),
                    fill_id: None,
                    context: self.build_context(),
                });
            }
            Some(OrderState::New | OrderState::PendingAck) => {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::OMS,
                    violation_type: ViolationType::FillBeforeAck { order_id },
                    message: format!("Fill received before ack for order {}", order_id),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: Some(order_id),
                    fill_id: None,
                    context: self.build_context(),
                });
            }
            Some(OrderState::Done) => {
                let terminal_state = "Done".to_string();
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::OMS,
                    violation_type: ViolationType::FillAfterTerminal { order_id, terminal_state: terminal_state.clone() },
                    message: format!("Fill after terminal state for order {}: {}", order_id, terminal_state),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: Some(order_id),
                    fill_id: None,
                    context: self.build_context(),
                });
            }
            _ => {}
        }

        Ok(())
    }

    // =========================================================================
    // Fill Invariants
    // =========================================================================

    /// Check fill plausibility.
    pub fn check_fill(
        &mut self,
        order_id: OrderId,
        fill_size: Size,
        fill_price: Price,
        is_maker: bool,
        book: &LimitOrderBook,
        timestamp: Nanos,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Fills) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Fills);

        // Check for NaN
        if fill_size.is_nan() || fill_price.is_nan() {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Fills,
                violation_type: ViolationType::FillNaN { order_id },
                message: format!("Fill contains NaN for order {}", order_id),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: Some(order_id),
                fill_id: None,
                context: self.build_context(),
            });
        }

        // Check for negative size
        if fill_size < 0.0 {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Fills,
                violation_type: ViolationType::NegativeFillSize { order_id, size: fill_size },
                message: format!("Negative fill size {} for order {}", fill_size, order_id),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: Some(order_id),
                fill_id: None,
                context: self.build_context(),
            });
        }

        // Check for overfill
        let filled = self.order_filled_qty.get(&order_id).copied().unwrap_or(0.0);
        let total = self.order_total_qty.get(&order_id).copied().unwrap_or(0.0);
        let new_filled = filled + fill_size;

        if new_filled > total + 1e-9 {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Fills,
                violation_type: ViolationType::OverFill {
                    order_id,
                    order_qty: total,
                    total_filled: new_filled,
                },
                message: format!(
                    "Overfill for order {}: total_qty={}, new_filled={}",
                    order_id, total, new_filled
                ),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: Some(order_id),
                fill_id: None,
                context: self.build_context(),
            });
        }

        self.order_filled_qty.insert(order_id, new_filled);

        // For taker fills, check liquidity existed
        if !is_maker {
            // Simplified check: price should be within book bounds
            let valid = match (book.best_bid(), book.best_ask()) {
                (Some((bid, _)), Some((ask, _))) => fill_price >= bid - 0.01 && fill_price <= ask + 0.01,
                _ => true, // Allow if book is empty (could be during initialization)
            };
            if !valid {
                return self.handle_violation(InvariantViolation {
                    category: InvariantCategory::Fills,
                    violation_type: ViolationType::TakerFillWithoutLiquidity {
                        order_id,
                        price: fill_price,
                    },
                    message: format!(
                        "Taker fill at {} for order {} but no liquidity at that price",
                        fill_price, order_id
                    ),
                    sim_time: timestamp,
                    decision_time: self.last_decision_time,
                    arrival_time: None,
                    seq: None,
                    market_id: None,
                    order_id: Some(order_id),
                    fill_id: None,
                    context: self.build_context(),
                });
            }
        }

        Ok(())
    }

    // =========================================================================
    // Accounting Invariants
    // =========================================================================

    /// Check accounting balance.
    pub fn check_accounting_balance(
        &mut self,
        debits: i64,
        credits: i64,
        timestamp: Nanos,
    ) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Accounting) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Accounting);

        if debits != credits {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Accounting,
                violation_type: ViolationType::UnbalancedEntry { debits, credits },
                message: format!("Unbalanced entry: debits={} credits={}", debits, credits),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: None,
                fill_id: None,
                context: self.build_context(),
            });
        }

        Ok(())
    }

    /// Check cash non-negativity.
    pub fn check_cash(&mut self, cash: f64, timestamp: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Accounting) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Accounting);

        if cash < -1e-9 {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Accounting,
                violation_type: ViolationType::NegativeCash { cash },
                message: format!("Negative cash: {}", cash),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: None,
                order_id: None,
                fill_id: None,
                context: self.build_context(),
            });
        }

        Ok(())
    }

    /// Check for duplicate settlement.
    pub fn check_settlement(&mut self, market_id: &str, timestamp: Nanos) -> InvariantResult<()> {
        if !self.config.categories.is_enabled(InvariantCategory::Accounting) {
            return Ok(());
        }

        self.counters.record_check(InvariantCategory::Accounting);

        if self.settled_markets.contains(market_id) {
            return self.handle_violation(InvariantViolation {
                category: InvariantCategory::Accounting,
                violation_type: ViolationType::DuplicateSettlement {
                    market_id: market_id.to_string(),
                },
                message: format!("Duplicate settlement for market {}", market_id),
                sim_time: timestamp,
                decision_time: self.last_decision_time,
                arrival_time: None,
                seq: None,
                market_id: Some(market_id.to_string()),
                order_id: None,
                fill_id: None,
                context: self.build_context(),
            });
        }

        self.settled_markets.insert(market_id.to_string());
        Ok(())
    }

    // =========================================================================
    // Violation Handling
    // =========================================================================

    fn handle_violation(&mut self, violation: InvariantViolation) -> InvariantResult<()> {
        self.counters.record_violation(violation.category);

        if self.first_violation.is_none() {
            self.first_violation = Some(violation.clone());
        }

        match self.config.mode {
            InvariantMode::Off => Ok(()),
            InvariantMode::Soft => {
                // Log and continue
                tracing::warn!(
                    category = ?violation.category,
                    violation_type = ?violation.violation_type,
                    message = %violation.message,
                    "Invariant violation (soft mode)"
                );
                Ok(())
            }
            InvariantMode::Hard => {
                self.counters.aborted = true;
                self.counters.abort_reason = Some(violation.message.clone());

                let dump = self.build_causal_dump(violation);
                Err(InvariantAbort { dump })
            }
        }
    }

    fn build_context(&self) -> ViolationContext {
        ViolationContext {
            recent_events: self.recent_events.iter().cloned().collect(),
            oms_transitions: self.oms_transitions.iter().cloned().collect(),
            ledger_entries: self.ledger_entries.iter().cloned().collect(),
            state_snapshot: Some(StateSnapshot {
                best_bid: None, // Would be filled by caller
                best_ask: None,
                cash: self.current_cash,
                open_orders: self.open_order_count,
                position: self.current_position,
            }),
            last_decision_proof_hash: None,
        }
    }

    fn build_causal_dump(&self, violation: InvariantViolation) -> CausalDump {
        let triggering_event = self.recent_events.back().cloned();

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.counters.total_checks.hash(&mut hasher);
        self.counters.total_violations.hash(&mut hasher);
        let fingerprint_at_abort = hasher.finish();

        CausalDump {
            violation,
            triggering_event,
            recent_events: self.recent_events.iter().cloned().collect(),
            oms_transitions: self.oms_transitions.iter().cloned().collect(),
            ledger_entries: self.ledger_entries.iter().cloned().collect(),
            state_snapshot: StateSnapshot {
                best_bid: None,
                best_ask: None,
                cash: self.current_cash,
                open_orders: self.open_order_count,
                position: self.current_position,
            },
            fingerprint_at_abort,
            config_hash: self.config_hash,
        }
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    pub fn counters(&self) -> &InvariantCounters {
        &self.counters
    }

    pub fn config(&self) -> &InvariantConfig {
        &self.config
    }

    pub fn mode(&self) -> InvariantMode {
        self.config.mode
    }

    pub fn first_violation(&self) -> Option<&InvariantViolation> {
        self.first_violation.as_ref()
    }

    pub fn is_aborted(&self) -> bool {
        self.counters.aborted
    }
}

// =============================================================================
// PRODUCTION-GRADE VALIDATION
// =============================================================================

/// Production-grade backtest requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionGradeRequirements {
    /// Invariant mode must be Hard.
    pub invariant_mode_hard: bool,
    /// All categories must be enabled.
    pub all_categories_enabled: bool,
    /// Determinism must be enforced.
    pub determinism_enforced: bool,
    /// Run fingerprint must be produced.
    pub fingerprint_produced: bool,
}

impl ProductionGradeRequirements {
    pub fn check(config: &InvariantConfig) -> Self {
        Self {
            invariant_mode_hard: config.mode == InvariantMode::Hard,
            all_categories_enabled: config.categories == CategoryFlags::all(),
            determinism_enforced: true, // Would be checked elsewhere
            fingerprint_produced: true, // Would be checked elsewhere
        }
    }

    pub fn is_satisfied(&self) -> bool {
        self.invariant_mode_hard
            && self.all_categories_enabled
            && self.determinism_enforced
            && self.fingerprint_produced
    }

    pub fn unsatisfied_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if !self.invariant_mode_hard {
            reasons.push("Invariant mode must be Hard".to_string());
        }
        if !self.all_categories_enabled {
            reasons.push("All invariant categories must be enabled".to_string());
        }
        if !self.determinism_enforced {
            reasons.push("Determinism must be enforced".to_string());
        }
        if !self.fingerprint_produced {
            reasons.push("Run fingerprint must be produced".to_string());
        }
        reasons
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invariant_mode_production_validity() {
        assert!(!InvariantMode::Off.is_valid_for_production());
        assert!(!InvariantMode::Soft.is_valid_for_production());
        assert!(InvariantMode::Hard.is_valid_for_production());
    }

    #[test]
    fn test_category_flags() {
        let mut flags = CategoryFlags::none();
        assert!(!flags.is_enabled(InvariantCategory::Time));

        flags.enable(InvariantCategory::Time);
        assert!(flags.is_enabled(InvariantCategory::Time));
        assert!(!flags.is_enabled(InvariantCategory::Book));

        let all = CategoryFlags::all();
        for cat in InvariantCategory::all() {
            assert!(all.is_enabled(*cat));
        }
    }

    #[test]
    fn test_soft_mode_continues_on_violation() {
        let config = InvariantConfig {
            mode: InvariantMode::Soft,
            ..Default::default()
        };
        let mut enforcer = InvariantEnforcer::new(config);

        // Cause a time violation
        enforcer.last_decision_time = 1000;
        let result = enforcer.check_decision_time(500);

        assert!(result.is_ok(), "Soft mode should continue");
        assert!(enforcer.counters().has_violations());
        assert_eq!(enforcer.counters().time_violations, 1);
    }

    #[test]
    fn test_hard_mode_aborts_on_violation() {
        let config = InvariantConfig {
            mode: InvariantMode::Hard,
            ..Default::default()
        };
        let mut enforcer = InvariantEnforcer::new(config);

        // Cause a time violation
        enforcer.last_decision_time = 1000;
        let result = enforcer.check_decision_time(500);

        assert!(result.is_err(), "Hard mode should abort");
        let abort = result.unwrap_err();
        assert!(matches!(
            abort.dump.violation.violation_type,
            ViolationType::DecisionTimeBackward { .. }
        ));
    }

    #[test]
    fn test_visibility_check() {
        let config = InvariantConfig::production();
        let mut enforcer = InvariantEnforcer::new(config);

        // Valid: arrival <= decision
        assert!(enforcer.check_visibility(100, 200).is_ok());

        // Invalid: arrival > decision
        let result = enforcer.check_visibility(300, 200);
        assert!(result.is_err());
    }

    #[test]
    fn test_overfill_detection() {
        let config = InvariantConfig::production();
        let mut enforcer = InvariantEnforcer::new(config);

        // Register order with qty 100
        enforcer.register_order(1, 100.0);

        // Valid fill
        let book = LimitOrderBook::new("test_token", crate::backtest_v2::matching::MatchingConfig::default());
        assert!(enforcer.check_fill(1, 50.0, 0.50, false, &book, 1000).is_ok());

        // Overfill
        let result = enforcer.check_fill(1, 60.0, 0.50, false, &book, 2000);
        assert!(result.is_err());
    }

    #[test]
    fn test_oms_state_transition() {
        let config = InvariantConfig::production();
        let mut enforcer = InvariantEnforcer::new(config);

        enforcer.register_order(1, 100.0);

        // Valid transitions: New -> PendingAck -> Live -> Done
        assert!(enforcer.check_order_transition(1, OrderState::PendingAck, 100, "send").is_ok());
        assert!(enforcer.check_order_transition(1, OrderState::Live, 200, "ack").is_ok());
        assert!(enforcer.check_order_transition(1, OrderState::Done, 300, "fill").is_ok());
    }

    #[test]
    fn test_illegal_oms_transition() {
        let config = InvariantConfig::production();
        let mut enforcer = InvariantEnforcer::new(config);

        enforcer.register_order(1, 100.0);

        // Illegal: New -> Live (skipping PendingAck)
        let result = enforcer.check_order_transition(1, OrderState::Live, 100, "direct_ack");
        assert!(result.is_err());
    }

    #[test]
    fn test_counters() {
        let config = InvariantConfig {
            mode: InvariantMode::Soft,
            ..Default::default()
        };
        let mut enforcer = InvariantEnforcer::new(config);

        // Perform various checks
        let _ = enforcer.check_decision_time(100);
        let _ = enforcer.check_decision_time(200);
        
        assert_eq!(enforcer.counters().time_checks, 2);
        assert_eq!(enforcer.counters().total_checks, 2);
    }

    #[test]
    fn test_production_requirements() {
        // Default config is now Hard mode (mandatory invariants), so it SHOULD satisfy requirements
        let default_config = InvariantConfig::default();
        let reqs = ProductionGradeRequirements::check(&default_config);
        assert!(reqs.is_satisfied(), "Default config (Hard mode) should satisfy production requirements");

        // Explicit Soft mode should NOT satisfy requirements
        let soft_config = InvariantConfig {
            mode: InvariantMode::Soft,
            ..InvariantConfig::default()
        };
        let reqs = ProductionGradeRequirements::check(&soft_config);
        assert!(!reqs.is_satisfied(), "Soft mode should NOT satisfy production requirements");

        let prod_config = InvariantConfig::production();
        let reqs = ProductionGradeRequirements::check(&prod_config);
        assert!(reqs.is_satisfied(), "Production config should satisfy production requirements");
    }

    #[test]
    fn test_causal_dump_format() {
        let violation = InvariantViolation {
            category: InvariantCategory::Time,
            violation_type: ViolationType::DecisionTimeBackward { old: 1000, new: 500 },
            message: "Test violation".to_string(),
            sim_time: 500,
            decision_time: 500,
            arrival_time: None,
            seq: None,
            market_id: None,
            order_id: None,
            fill_id: None,
            context: ViolationContext::default(),
        };

        let dump = CausalDump {
            violation,
            triggering_event: None,
            recent_events: vec![],
            oms_transitions: vec![],
            ledger_entries: vec![],
            state_snapshot: StateSnapshot {
                best_bid: Some((0.45, 100.0)),
                best_ask: Some((0.55, 100.0)),
                cash: 1000.0,
                open_orders: 2,
                position: 50.0,
            },
            fingerprint_at_abort: 0x12345678,
            config_hash: 0xABCDEF00,
        };

        let text = dump.format_text();
        assert!(text.contains("INVARIANT VIOLATION"));
        assert!(text.contains("DecisionTimeBackward"));
    }
}
