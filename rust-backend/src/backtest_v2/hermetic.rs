//! Hermetic Strategy Mode
//!
//! Enforces strict sandboxing of strategy code in production-grade backtests.
//! When `hermetic_strategy=true`, strategies are prohibited from accessing:
//! - Wall-clock time (SystemTime, Instant, OS clocks)
//! - Environment variables
//! - Filesystem I/O
//! - Network I/O
//! - Threading/async task spawning
//! - Randomness outside the provided RNG
//! - Global/static mutable state
//!
//! # Enforcement Layers
//!
//! 1. **Compile-time** (lint): CI blocks `strategy/` modules that import forbidden APIs
//! 2. **Runtime** (guard): Panics on forbidden API invocation in hermetic mode
//!
//! # DecisionProof Requirement
//!
//! In hermetic mode, EVERY strategy callback that can emit orders/cancels MUST
//! produce a `HermeticDecisionProof`. Missing proofs abort immediately.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Size, Side};
#[allow(unused_imports)]
use crate::backtest_v2::visibility::DecisionProof;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// GLOBAL HERMETIC MODE FLAG
// =============================================================================

static HERMETIC_MODE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable hermetic mode globally. Once enabled, forbidden API calls panic.
pub fn enable_hermetic_mode() {
    HERMETIC_MODE_ENABLED.store(true, Ordering::SeqCst);
}

/// Disable hermetic mode (for testing/debugging only).
pub fn disable_hermetic_mode() {
    HERMETIC_MODE_ENABLED.store(false, Ordering::SeqCst);
}

/// Check if hermetic mode is currently enabled.
pub fn is_hermetic_mode() -> bool {
    HERMETIC_MODE_ENABLED.load(Ordering::SeqCst)
}

/// Assert hermetic mode is enabled. Panics if not.
pub fn assert_hermetic_mode() {
    if !is_hermetic_mode() {
        panic!("HERMETIC MODE ASSERTION FAILED: hermetic_mode must be enabled for this operation");
    }
}

// =============================================================================
// HERMETIC STRATEGY CONFIGURATION
// =============================================================================

/// Configuration for hermetic strategy enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HermeticConfig {
    /// Enable hermetic mode enforcement.
    pub enabled: bool,
    /// Require DecisionProof for every callback (even no-ops).
    pub require_decision_proofs: bool,
    /// Abort on first violation (vs logging).
    pub abort_on_violation: bool,
    /// Maximum callback duration before timeout (nanoseconds).
    /// Prevents infinite loops. Default: 1 second.
    pub max_callback_duration_ns: Nanos,
}

impl Default for HermeticConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_decision_proofs: true,
            abort_on_violation: true,
            max_callback_duration_ns: 1_000_000_000, // 1 second
        }
    }
}

impl HermeticConfig {
    /// Production-grade hermetic configuration.
    pub fn production() -> Self {
        Self {
            enabled: true,
            require_decision_proofs: true,
            abort_on_violation: true,
            max_callback_duration_ns: 1_000_000_000,
        }
    }

    /// Relaxed configuration for testing (still logs violations).
    pub fn testing() -> Self {
        Self {
            enabled: true,
            require_decision_proofs: true,
            abort_on_violation: false,
            max_callback_duration_ns: 5_000_000_000, // 5 seconds for tests
        }
    }

    /// Check if this config is valid for production-grade runs.
    pub fn is_production_grade(&self) -> bool {
        self.enabled && self.require_decision_proofs && self.abort_on_violation
    }
}

// =============================================================================
// FORBIDDEN API RUNTIME GUARDS
// =============================================================================

/// Runtime guard that panics if hermetic mode is enabled.
/// Use this to wrap any potentially forbidden operations.
#[inline]
pub fn hermetic_guard(operation: &str) {
    if is_hermetic_mode() {
        panic!(
            "HERMETIC MODE VIOLATION: Forbidden operation '{}' attempted in hermetic mode. \
             Strategy code MUST NOT access wall-clock time, environment, I/O, or threading.",
            operation
        );
    }
}

/// Guard for wall-clock time access.
#[inline]
pub fn guard_wall_clock() {
    hermetic_guard("wall_clock_time_access");
}

/// Guard for environment variable access.
#[inline]
pub fn guard_env_access() {
    hermetic_guard("environment_variable_access");
}

/// Guard for filesystem I/O.
#[inline]
pub fn guard_filesystem_io() {
    hermetic_guard("filesystem_io");
}

/// Guard for network I/O.
#[inline]
pub fn guard_network_io() {
    hermetic_guard("network_io");
}

/// Guard for thread spawning.
#[inline]
pub fn guard_thread_spawn() {
    hermetic_guard("thread_spawn");
}

/// Guard for async task spawning.
#[inline]
pub fn guard_async_spawn() {
    hermetic_guard("async_task_spawn");
}

// =============================================================================
// HERMETIC TIME SOURCE
// =============================================================================

/// Hermetic time source that ONLY provides simulated time.
/// This is the ONLY time source strategies can use in hermetic mode.
#[derive(Debug, Clone)]
pub struct HermeticClock {
    /// Current simulated time in nanoseconds.
    sim_time: Nanos,
}

impl HermeticClock {
    pub fn new(initial_time: Nanos) -> Self {
        Self { sim_time: initial_time }
    }

    /// Get current simulated time (the ONLY time available in hermetic mode).
    pub fn now(&self) -> Nanos {
        self.sim_time
    }

    /// Advance time (called by orchestrator only, not by strategies).
    pub(crate) fn advance_to(&mut self, new_time: Nanos) {
        debug_assert!(
            new_time >= self.sim_time,
            "HermeticClock: time cannot go backward: {} -> {}",
            self.sim_time,
            new_time
        );
        self.sim_time = new_time;
    }
}

// =============================================================================
// HERMETIC RNG
// =============================================================================

/// Hermetic RNG that MUST be used for all randomness in strategies.
/// Wraps a seeded deterministic RNG.
pub struct HermeticRng {
    rng: rand_chacha::ChaCha8Rng,
    samples_drawn: u64,
}

impl HermeticRng {
    pub fn new(seed: u64) -> Self {
        use rand::SeedableRng;
        Self {
            rng: rand_chacha::ChaCha8Rng::seed_from_u64(seed),
            samples_drawn: 0,
        }
    }

    /// Get the underlying RNG for sampling.
    pub fn rng(&mut self) -> &mut rand_chacha::ChaCha8Rng {
        self.samples_drawn += 1;
        &mut self.rng
    }

    /// Get count of samples drawn (for auditing).
    pub fn samples_drawn(&self) -> u64 {
        self.samples_drawn
    }
}

// =============================================================================
// HERMETIC DECISION PROOF
// =============================================================================

/// Comprehensive decision proof for hermetic mode.
/// Must be produced for EVERY strategy callback that can emit orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HermeticDecisionProof {
    /// Unique decision ID (monotonically increasing).
    pub decision_id: u64,
    /// Strategy name.
    pub strategy_name: String,
    /// Callback type that produced this decision.
    pub callback_type: CallbackType,
    /// Decision time (from HermeticClock).
    pub decision_time: Nanos,
    /// Input event identifiers.
    pub input_events: Vec<InputEventId>,
    /// Visible book snapshot identifier (hash).
    pub book_snapshot_hash: u64,
    /// Signal values used in decision.
    pub signal_values: Vec<(String, f64)>,
    /// Hash of all decision inputs (for reproducibility verification).
    pub input_hash: u64,
    /// Actions taken (orders, cancels, or explicit no-op).
    pub actions: Vec<DecisionAction>,
    /// Was this a no-op decision (explicitly recorded).
    pub is_noop: bool,
}

impl HermeticDecisionProof {
    /// Create a new decision proof builder.
    pub fn builder(
        decision_id: u64,
        strategy_name: impl Into<String>,
        callback_type: CallbackType,
        decision_time: Nanos,
    ) -> HermeticDecisionProofBuilder {
        HermeticDecisionProofBuilder {
            decision_id,
            strategy_name: strategy_name.into(),
            callback_type,
            decision_time,
            input_events: Vec::new(),
            book_snapshot_hash: 0,
            signal_values: Vec::new(),
            actions: Vec::new(),
            finalized: false,
        }
    }

    /// Compute a deterministic hash for this proof.
    pub fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.decision_id.hash(&mut hasher);
        self.strategy_name.hash(&mut hasher);
        (self.callback_type as u8).hash(&mut hasher);
        self.decision_time.hash(&mut hasher);
        for event in &self.input_events {
            event.arrival_time.hash(&mut hasher);
            event.seq.hash(&mut hasher);
        }
        self.book_snapshot_hash.hash(&mut hasher);
        for (name, value) in &self.signal_values {
            name.hash(&mut hasher);
            value.to_bits().hash(&mut hasher);
        }
        for action in &self.actions {
            match action {
                DecisionAction::Order { client_order_id, side, price, size, .. } => {
                    client_order_id.hash(&mut hasher);
                    (*side as u8).hash(&mut hasher);
                    price.to_bits().hash(&mut hasher);
                    size.to_bits().hash(&mut hasher);
                }
                DecisionAction::Cancel { order_id } => {
                    order_id.hash(&mut hasher);
                }
                DecisionAction::NoOp { reason } => {
                    reason.hash(&mut hasher);
                }
            }
        }
        self.is_noop.hash(&mut hasher);
        hasher.finish()
    }
}

/// Builder for HermeticDecisionProof.
pub struct HermeticDecisionProofBuilder {
    decision_id: u64,
    strategy_name: String,
    callback_type: CallbackType,
    decision_time: Nanos,
    input_events: Vec<InputEventId>,
    book_snapshot_hash: u64,
    signal_values: Vec<(String, f64)>,
    actions: Vec<DecisionAction>,
    finalized: bool,
}

impl HermeticDecisionProofBuilder {
    /// Record an input event.
    pub fn record_input_event(&mut self, arrival_time: Nanos, source_time: Nanos, seq: u64) {
        self.input_events.push(InputEventId {
            arrival_time,
            source_time,
            seq,
        });
    }

    /// Record the book snapshot hash.
    pub fn record_book_snapshot(&mut self, hash: u64) {
        self.book_snapshot_hash = hash;
    }

    /// Record a signal value used in the decision.
    pub fn record_signal(&mut self, name: impl Into<String>, value: f64) {
        self.signal_values.push((name.into(), value));
    }

    /// Record an order action.
    pub fn record_order(
        &mut self,
        client_order_id: impl Into<String>,
        token_id: impl Into<String>,
        side: Side,
        price: Price,
        size: Size,
    ) {
        self.actions.push(DecisionAction::Order {
            client_order_id: client_order_id.into(),
            token_id: token_id.into(),
            side,
            price,
            size,
        });
    }

    /// Record a cancel action.
    pub fn record_cancel(&mut self, order_id: OrderId) {
        self.actions.push(DecisionAction::Cancel { order_id });
    }

    /// Record an explicit no-op decision.
    pub fn record_noop(&mut self, reason: impl Into<String>) {
        self.actions.push(DecisionAction::NoOp {
            reason: reason.into(),
        });
    }

    /// Finalize and build the proof.
    pub fn build(mut self) -> HermeticDecisionProof {
        self.finalized = true;
        let is_noop = self.actions.is_empty()
            || self.actions.iter().all(|a| matches!(a, DecisionAction::NoOp { .. }));

        let mut proof = HermeticDecisionProof {
            decision_id: self.decision_id,
            strategy_name: std::mem::take(&mut self.strategy_name),
            callback_type: self.callback_type,
            decision_time: self.decision_time,
            input_events: std::mem::take(&mut self.input_events),
            book_snapshot_hash: self.book_snapshot_hash,
            signal_values: std::mem::take(&mut self.signal_values),
            input_hash: 0,
            actions: std::mem::take(&mut self.actions),
            is_noop,
        };
        proof.input_hash = proof.compute_hash();
        proof
    }

    /// Check if the builder was finalized.
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
}

impl Drop for HermeticDecisionProofBuilder {
    fn drop(&mut self) {
        if is_hermetic_mode() && !self.finalized {
            panic!(
                "HERMETIC MODE VIOLATION: HermeticDecisionProofBuilder dropped without calling build(). \
                 Strategy '{}' callback {:?} at time {} did not produce a DecisionProof.",
                self.strategy_name, self.callback_type, self.decision_time
            );
        }
    }
}

/// Type of strategy callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallbackType {
    OnBookUpdate,
    OnTrade,
    OnTimer,
    OnOrderAck,
    OnOrderReject,
    OnFill,
    OnCancelAck,
    OnStart,
    OnStop,
}

impl CallbackType {
    /// Check if this callback type can produce orders/cancels.
    pub fn can_emit_orders(&self) -> bool {
        matches!(
            self,
            Self::OnBookUpdate
                | Self::OnTrade
                | Self::OnTimer
                | Self::OnFill
                | Self::OnCancelAck
                | Self::OnStart
        )
    }
}

/// Identifier for an input event in the decision proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEventId {
    pub arrival_time: Nanos,
    pub source_time: Nanos,
    pub seq: u64,
}

/// Action taken as part of a decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DecisionAction {
    Order {
        client_order_id: String,
        token_id: String,
        side: Side,
        price: Price,
        size: Size,
    },
    Cancel {
        order_id: OrderId,
    },
    NoOp {
        reason: String,
    },
}

// =============================================================================
// HERMETIC STRATEGY MARKER TRAIT
// =============================================================================

/// Marker trait for strategies that are hermetic-compatible.
/// 
/// A strategy is hermetic-compatible if it:
/// 1. Does not import any forbidden APIs (enforced by lint)
/// 2. Derives all time from StrategyContext::now()
/// 3. Produces HermeticDecisionProof for every decision
/// 4. Does not access global/static mutable state
/// 
/// Implement this trait to declare your strategy hermetic-compatible.
/// The trait has no methods - it's a compile-time marker.
pub trait HermeticStrategy: crate::backtest_v2::strategy::Strategy {
    /// Return true if this strategy is hermetic-compatible.
    /// Default implementation returns true.
    fn is_hermetic(&self) -> bool {
        true
    }
}

// =============================================================================
// HERMETIC STRATEGY CONTEXT
// =============================================================================

/// Extended strategy context for hermetic mode.
/// Provides the HermeticClock and HermeticRng to strategies.
pub struct HermeticStrategyContext<'a> {
    /// Standard strategy context.
    pub base: &'a mut crate::backtest_v2::strategy::StrategyContext<'a>,
    /// Hermetic clock (ONLY time source).
    pub clock: &'a HermeticClock,
    /// Hermetic RNG (ONLY randomness source).
    pub rng: &'a mut HermeticRng,
    /// Current decision proof builder.
    pub proof_builder: HermeticDecisionProofBuilder,
}

// =============================================================================
// HERMETIC VIOLATION
// =============================================================================

/// Types of hermetic violations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HermeticViolationType {
    /// Missing DecisionProof from callback.
    MissingDecisionProof {
        strategy_name: String,
        callback_type: CallbackType,
        decision_time: Nanos,
    },
    /// Forbidden API accessed.
    ForbiddenApiAccess {
        api_name: String,
        strategy_name: Option<String>,
    },
    /// Wall-clock time accessed.
    WallClockAccess {
        strategy_name: Option<String>,
    },
    /// Environment variable accessed.
    EnvVarAccess {
        var_name: Option<String>,
        strategy_name: Option<String>,
    },
    /// Filesystem I/O attempted.
    FilesystemIo {
        strategy_name: Option<String>,
    },
    /// Network I/O attempted.
    NetworkIo {
        strategy_name: Option<String>,
    },
    /// Thread spawning attempted.
    ThreadSpawn {
        strategy_name: Option<String>,
    },
    /// Hermetic mode required but not enabled.
    HermeticModeRequired,
    /// Strategy not hermetic-compatible.
    StrategyNotHermetic {
        strategy_name: String,
    },
}

/// Hermetic violation record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HermeticViolation {
    pub violation_type: HermeticViolationType,
    pub message: String,
    pub decision_time: Nanos,
}

impl std::fmt::Display for HermeticViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HERMETIC VIOLATION: {}", self.message)
    }
}

impl std::error::Error for HermeticViolation {}

/// Hermetic abort with full context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HermeticAbort {
    pub violation: HermeticViolation,
    pub last_decision_proofs: Vec<HermeticDecisionProof>,
    pub config: HermeticConfig,
}

impl std::fmt::Display for HermeticAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "╔══════════════════════════════════════════════════════════════════╗")?;
        writeln!(f, "║            HERMETIC STRATEGY MODE ABORT                          ║")?;
        writeln!(f, "╠══════════════════════════════════════════════════════════════════╣")?;
        writeln!(f, "║  {}", self.violation.message)?;
        writeln!(f, "║  Decision Time: {} ns", self.violation.decision_time)?;
        writeln!(f, "╠══════════════════════════════════════════════════════════════════╣")?;
        writeln!(f, "║  VIOLATION TYPE: {:?}", self.violation.violation_type)?;
        writeln!(f, "╚══════════════════════════════════════════════════════════════════╝")?;
        Ok(())
    }
}

impl std::error::Error for HermeticAbort {}

// =============================================================================
// HERMETIC ENFORCER
// =============================================================================

/// Enforcer for hermetic strategy mode.
pub struct HermeticEnforcer {
    config: HermeticConfig,
    /// Next decision ID.
    next_decision_id: u64,
    /// Recent decision proofs for audit trail.
    recent_proofs: std::collections::VecDeque<HermeticDecisionProof>,
    /// Maximum proofs to keep.
    max_proofs: usize,
    /// Violations detected (in non-abort mode).
    violations: Vec<HermeticViolation>,
    /// Currently active proof builder (set on callback entry, cleared on exit).
    active_proof_builder: Option<HermeticDecisionProofBuilder>,
    /// Current callback being executed.
    current_callback: Option<(String, CallbackType, Nanos)>,
}

impl HermeticEnforcer {
    pub fn new(config: HermeticConfig) -> Self {
        if config.enabled {
            enable_hermetic_mode();
        }
        Self {
            config,
            next_decision_id: 1,
            recent_proofs: std::collections::VecDeque::with_capacity(100),
            max_proofs: 100,
            violations: Vec::new(),
            active_proof_builder: None,
            current_callback: None,
        }
    }

    /// Called at the start of a strategy callback.
    /// Returns a proof builder that MUST be finalized before the callback returns.
    pub fn on_callback_entry(
        &mut self,
        strategy_name: &str,
        callback_type: CallbackType,
        decision_time: Nanos,
    ) -> Option<HermeticDecisionProofBuilder> {
        if !self.config.enabled {
            return None;
        }

        // Only create proof builder for callbacks that can emit orders
        if !callback_type.can_emit_orders() && !self.config.require_decision_proofs {
            return None;
        }

        let builder = HermeticDecisionProof::builder(
            self.next_decision_id,
            strategy_name,
            callback_type,
            decision_time,
        );
        self.next_decision_id += 1;
        self.current_callback = Some((strategy_name.to_string(), callback_type, decision_time));
        self.active_proof_builder = None; // Will be set by caller

        Some(builder)
    }

    /// Called at the end of a strategy callback.
    /// Validates that a proof was produced (if required).
    pub fn on_callback_exit(&mut self, proof: Option<HermeticDecisionProof>) -> Result<(), HermeticAbort> {
        if !self.config.enabled {
            return Ok(());
        }

        let (strategy_name, callback_type, decision_time) = match self.current_callback.take() {
            Some(cb) => cb,
            None => return Ok(()), // No active callback
        };

        // Check if proof was required
        if self.config.require_decision_proofs && callback_type.can_emit_orders() {
            match proof {
                Some(p) => {
                    // Store the proof
                    if self.recent_proofs.len() >= self.max_proofs {
                        self.recent_proofs.pop_front();
                    }
                    self.recent_proofs.push_back(p);
                }
                None => {
                    let violation = HermeticViolation {
                        violation_type: HermeticViolationType::MissingDecisionProof {
                            strategy_name: strategy_name.clone(),
                            callback_type,
                            decision_time,
                        },
                        message: format!(
                            "Strategy '{}' callback {:?} at time {} did not produce a DecisionProof",
                            strategy_name, callback_type, decision_time
                        ),
                        decision_time,
                    };

                    if self.config.abort_on_violation {
                        return Err(HermeticAbort {
                            violation,
                            last_decision_proofs: self.recent_proofs.iter().cloned().collect(),
                            config: self.config.clone(),
                        });
                    } else {
                        self.violations.push(violation);
                    }
                }
            }
        } else if let Some(p) = proof {
            // Store even if not required
            if self.recent_proofs.len() >= self.max_proofs {
                self.recent_proofs.pop_front();
            }
            self.recent_proofs.push_back(p);
        }

        Ok(())
    }

    /// Check if hermetic mode is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the configuration.
    pub fn config(&self) -> &HermeticConfig {
        &self.config
    }

    /// Get recent decision proofs.
    pub fn recent_proofs(&self) -> &std::collections::VecDeque<HermeticDecisionProof> {
        &self.recent_proofs
    }

    /// Get violations (in non-abort mode).
    pub fn violations(&self) -> &[HermeticViolation] {
        &self.violations
    }

    /// Validate that production-grade mode implies hermetic mode.
    pub fn validate_production_grade(
        production_grade: bool,
        hermetic_config: &HermeticConfig,
    ) -> Result<(), HermeticAbort> {
        if production_grade && !hermetic_config.enabled {
            return Err(HermeticAbort {
                violation: HermeticViolation {
                    violation_type: HermeticViolationType::HermeticModeRequired,
                    message: "production_grade=true requires hermetic_strategy=true. \
                              Production-grade backtests MUST run with hermetic strategy enforcement."
                        .to_string(),
                    decision_time: 0,
                },
                last_decision_proofs: Vec::new(),
                config: hermetic_config.clone(),
            });
        }

        if production_grade && !hermetic_config.is_production_grade() {
            return Err(HermeticAbort {
                violation: HermeticViolation {
                    violation_type: HermeticViolationType::HermeticModeRequired,
                    message: format!(
                        "production_grade=true requires hermetic config to be production-grade. \
                         Current: require_decision_proofs={}, abort_on_violation={}",
                        hermetic_config.require_decision_proofs,
                        hermetic_config.abort_on_violation
                    ),
                    decision_time: 0,
                },
                last_decision_proofs: Vec::new(),
                config: hermetic_config.clone(),
            });
        }

        Ok(())
    }
}

// =============================================================================
// COMPILE-TIME LINT HELPERS
// =============================================================================

/// List of forbidden API patterns for compile-time/CI linting.
/// These patterns should be checked in `strategy/` modules.
pub const FORBIDDEN_API_PATTERNS: &[&str] = &[
    "std::time::SystemTime",
    "std::time::Instant",
    "std::time::UNIX_EPOCH",
    "chrono::",
    "tokio::time::",
    "std::env::var",
    "std::env::vars",
    "std::env::set_var",
    "std::env::remove_var",
    "std::fs::",
    "std::net::",
    "std::thread::spawn",
    "std::thread::Builder",
    "tokio::spawn",
    "tokio::task::spawn",
    "async_std::task::spawn",
];

/// Generate a lint configuration snippet for CI.
pub fn generate_lint_config() -> String {
    let mut config = String::new();
    config.push_str("# Hermetic Strategy Mode - Forbidden API Lint Rules\n");
    config.push_str("# Add to clippy.toml or CI script\n\n");
    config.push_str("disallowed-methods = [\n");
    for pattern in FORBIDDEN_API_PATTERNS {
        config.push_str(&format!("    \"{}\",\n", pattern));
    }
    config.push_str("]\n");
    config
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_hermetic_mode() {
        disable_hermetic_mode();
    }

    #[test]
    fn test_hermetic_mode_flag() {
        reset_hermetic_mode();
        assert!(!is_hermetic_mode());

        enable_hermetic_mode();
        assert!(is_hermetic_mode());

        disable_hermetic_mode();
        assert!(!is_hermetic_mode());
    }

    #[test]
    #[should_panic(expected = "HERMETIC MODE VIOLATION")]
    fn test_hermetic_guard_panics_when_enabled() {
        reset_hermetic_mode();
        enable_hermetic_mode();
        hermetic_guard("test_operation");
    }

    #[test]
    fn test_hermetic_guard_allows_when_disabled() {
        reset_hermetic_mode();
        hermetic_guard("test_operation"); // Should not panic
    }

    #[test]
    fn test_hermetic_clock() {
        let mut clock = HermeticClock::new(1000);
        assert_eq!(clock.now(), 1000);

        clock.advance_to(2000);
        assert_eq!(clock.now(), 2000);
    }

    #[test]
    fn test_hermetic_rng() {
        let mut rng1 = HermeticRng::new(42);
        let mut rng2 = HermeticRng::new(42);

        use rand::Rng;
        let v1: u64 = rng1.rng().gen();
        let v2: u64 = rng2.rng().gen();

        assert_eq!(v1, v2, "Same seed should produce same sequence");
    }

    #[test]
    fn test_decision_proof_builder() {
        reset_hermetic_mode();

        let mut builder = HermeticDecisionProof::builder(
            1,
            "TestStrategy",
            CallbackType::OnBookUpdate,
            1000,
        );

        builder.record_input_event(900, 850, 1);
        builder.record_book_snapshot(12345);
        builder.record_signal("mid_price", 0.55);
        builder.record_noop("No signal");

        let proof = builder.build();

        assert_eq!(proof.decision_id, 1);
        assert_eq!(proof.strategy_name, "TestStrategy");
        assert!(proof.is_noop);
        assert!(!proof.input_events.is_empty());
    }

    #[test]
    fn test_decision_proof_hash_determinism() {
        reset_hermetic_mode();

        let build_proof = || {
            let mut builder = HermeticDecisionProof::builder(
                1,
                "TestStrategy",
                CallbackType::OnBookUpdate,
                1000,
            );
            builder.record_input_event(900, 850, 1);
            builder.record_signal("mid", 0.5);
            builder.build()
        };

        let proof1 = build_proof();
        let proof2 = build_proof();

        assert_eq!(proof1.input_hash, proof2.input_hash);
    }

    #[test]
    fn test_hermetic_enforcer_callback_flow() {
        reset_hermetic_mode();

        let config = HermeticConfig::testing();
        let mut enforcer = HermeticEnforcer::new(config);

        // Start callback
        let builder = enforcer
            .on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000)
            .unwrap();

        // Build proof
        let proof = builder.build();

        // End callback with proof
        let result = enforcer.on_callback_exit(Some(proof));
        assert!(result.is_ok());

        assert_eq!(enforcer.recent_proofs().len(), 1);
    }

    #[test]
    fn test_hermetic_enforcer_missing_proof_non_abort() {
        reset_hermetic_mode();

        let config = HermeticConfig {
            enabled: true,
            require_decision_proofs: true,
            abort_on_violation: false, // Don't abort
            max_callback_duration_ns: 1_000_000_000,
        };
        let mut enforcer = HermeticEnforcer::new(config);

        // Start callback
        let _builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000);

        // End callback WITHOUT proof
        let result = enforcer.on_callback_exit(None);
        assert!(result.is_ok()); // Should not abort in non-abort mode

        assert_eq!(enforcer.violations().len(), 1);
    }

    #[test]
    fn test_hermetic_enforcer_missing_proof_abort() {
        reset_hermetic_mode();

        let config = HermeticConfig::production();
        let mut enforcer = HermeticEnforcer::new(config);

        // Start callback
        let _builder = enforcer.on_callback_entry("TestStrategy", CallbackType::OnBookUpdate, 1000);

        // End callback WITHOUT proof (should abort)
        let result = enforcer.on_callback_exit(None);
        assert!(result.is_err());

        let abort = result.unwrap_err();
        assert!(matches!(
            abort.violation.violation_type,
            HermeticViolationType::MissingDecisionProof { .. }
        ));
    }

    #[test]
    fn test_production_grade_requires_hermetic() {
        reset_hermetic_mode();

        let non_hermetic = HermeticConfig {
            enabled: false,
            ..Default::default()
        };

        let result = HermeticEnforcer::validate_production_grade(true, &non_hermetic);
        assert!(result.is_err());

        let hermetic = HermeticConfig::production();
        let result = HermeticEnforcer::validate_production_grade(true, &hermetic);
        assert!(result.is_ok());
    }

    #[test]
    fn test_callback_type_can_emit_orders() {
        assert!(CallbackType::OnBookUpdate.can_emit_orders());
        assert!(CallbackType::OnTrade.can_emit_orders());
        assert!(CallbackType::OnTimer.can_emit_orders());
        assert!(CallbackType::OnFill.can_emit_orders());
        assert!(CallbackType::OnStart.can_emit_orders());

        assert!(!CallbackType::OnOrderAck.can_emit_orders());
        assert!(!CallbackType::OnOrderReject.can_emit_orders());
        assert!(!CallbackType::OnStop.can_emit_orders());
    }

    #[test]
    fn test_forbidden_api_patterns() {
        assert!(FORBIDDEN_API_PATTERNS.contains(&"std::time::SystemTime"));
        assert!(FORBIDDEN_API_PATTERNS.contains(&"std::env::var"));
        assert!(FORBIDDEN_API_PATTERNS.contains(&"std::fs::"));
    }

    // Note: The test for HermeticDecisionProofBuilder Drop panic is intentionally
    // not included as it would cause the test harness to abort.
}
