//! Visibility Enforcement and Look-Ahead Prevention
//!
//! This module implements hard invariants to prevent timestamp leakage in backtests:
//! - `SimArrivalPolicy`: Maps historical data timestamps to simulated arrival times
//! - `VisibilityWatermark`: Tracks what data is visible at the current decision_time
//! - `DecisionProof`: Audit trail proving no look-ahead occurred
//!
//! # Time Semantics
//! - `source_time`: Timestamp provided by upstream feed (may be missing/untrusted)
//! - `arrival_time`: Time when our system "sees" the event (ONLY time used for visibility)
//! - `decision_time`: Current SimClock time when strategy is invoked
//!
//! # Hard Invariant
//! Strategy MUST only read state derived from events with `arrival_time <= decision_time`.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::TimestampedEvent;
use crate::backtest_v2::latency::{LatencyDistribution, NS_PER_MS, NS_PER_US};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global strict mode flag - when enabled, any visibility violation causes panic.
static STRICT_MODE: AtomicBool = AtomicBool::new(false);

/// Enable strict mode (panics on first visibility violation).
pub fn enable_strict_mode() {
    STRICT_MODE.store(true, Ordering::SeqCst);
}

/// Disable strict mode.
pub fn disable_strict_mode() {
    STRICT_MODE.store(false, Ordering::SeqCst);
}

/// Check if strict mode is enabled.
pub fn is_strict_mode() -> bool {
    STRICT_MODE.load(Ordering::SeqCst)
}

/// Policy for mapping historical data to simulated arrival times.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimArrivalPolicy {
    /// Policy A: Use recorded arrival timestamps from historical dataset (best).
    /// Requires `arrival_time` field to be present and trusted.
    RecordedArrival,

    /// Policy B: Derive arrival_time from source_time + simulated latency.
    /// `arrival_time := source_time + latency_distribution.sample()`
    SimulatedLatency {
        /// Latency distribution for market data events.
        market_data_latency: LatencyDistribution,
        /// Latency distribution for internal events (acks, fills).
        internal_event_latency: LatencyDistribution,
        /// Random seed for deterministic replay.
        seed: u64,
    },

    /// Policy C: Neither source nor arrival timestamps available.
    /// Backtester must be labeled as "approximate mode" and blocked from production-grade claims.
    Unusable,
}

impl Default for SimArrivalPolicy {
    fn default() -> Self {
        // Default to simulated latency with conservative values
        Self::SimulatedLatency {
            market_data_latency: LatencyDistribution::market_data_realistic(),
            internal_event_latency: LatencyDistribution::Fixed {
                latency_ns: 100 * NS_PER_US,
            },
            seed: 42,
        }
    }
}

impl SimArrivalPolicy {
    /// Create a policy for recorded arrival times.
    pub fn recorded() -> Self {
        Self::RecordedArrival
    }

    /// Create a policy with fixed latency offset.
    pub fn fixed_latency(latency_ns: Nanos) -> Self {
        Self::SimulatedLatency {
            market_data_latency: LatencyDistribution::Fixed { latency_ns },
            internal_event_latency: LatencyDistribution::Fixed { latency_ns },
            seed: 42,
        }
    }

    /// Create a policy with deterministic seed.
    pub fn with_seed(mut self, seed: u64) -> Self {
        if let Self::SimulatedLatency { seed: ref mut s, .. } = self {
            *s = seed;
        }
        self
    }

    /// Get a human-readable description of this policy.
    pub fn description(&self) -> &'static str {
        match self {
            Self::RecordedArrival => "Policy A: Recorded arrival timestamps",
            Self::SimulatedLatency { .. } => "Policy B: Simulated latency from source timestamps",
            Self::Unusable => "Policy C: Unusable - no valid timestamps",
        }
    }

    /// Check if this policy supports production-grade backtesting.
    pub fn is_production_grade(&self) -> bool {
        !matches!(self, Self::Unusable)
    }
}

/// Mapper that applies SimArrivalPolicy to convert source_time -> arrival_time.
pub struct ArrivalTimeMapper {
    policy: SimArrivalPolicy,
    rng: Option<StdRng>,
    events_mapped: u64,
}

impl ArrivalTimeMapper {
    pub fn new(policy: SimArrivalPolicy) -> Self {
        let rng = match &policy {
            SimArrivalPolicy::SimulatedLatency { seed, .. } => Some(StdRng::seed_from_u64(*seed)),
            _ => None,
        };
        Self {
            policy,
            rng,
            events_mapped: 0,
        }
    }

    /// Map a source timestamp to an arrival timestamp.
    /// Returns (arrival_time, latency_applied).
    pub fn map_arrival_time(&mut self, source_time: Nanos, is_internal: bool) -> (Nanos, Nanos) {
        self.events_mapped += 1;

        match &mut self.policy {
            SimArrivalPolicy::RecordedArrival => {
                // Caller should have already set arrival_time; return source_time as fallback
                (source_time, 0)
            }
            SimArrivalPolicy::SimulatedLatency {
                market_data_latency,
                internal_event_latency,
                ..
            } => {
                let rng = self.rng.as_mut().expect("RNG must exist for SimulatedLatency");
                let latency = if is_internal {
                    internal_event_latency.sample(rng)
                } else {
                    market_data_latency.sample(rng)
                };
                (source_time + latency, latency)
            }
            SimArrivalPolicy::Unusable => {
                panic!("Cannot map arrival time with Unusable policy");
            }
        }
    }

    /// Apply arrival time mapping to an event (mutates in place).
    pub fn apply_to_event(&mut self, event: &mut TimestampedEvent, is_internal: bool) {
        let (arrival_time, _latency) = self.map_arrival_time(event.source_time, is_internal);
        event.time = arrival_time;
    }

    pub fn policy(&self) -> &SimArrivalPolicy {
        &self.policy
    }

    pub fn events_mapped(&self) -> u64 {
        self.events_mapped
    }
}

/// Tracks the visibility watermark - what data is visible at the current decision_time.
#[derive(Debug, Clone)]
pub struct VisibilityWatermark {
    /// Current decision time - events with arrival_time > this are NOT visible.
    decision_time: Nanos,
    /// Highest arrival_time of any event that has been applied to state.
    latest_applied_arrival: Nanos,
    /// Total events applied.
    events_applied: u64,
    /// Violations detected (only in non-strict mode).
    violations: Vec<VisibilityViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityViolation {
    pub decision_time: Nanos,
    pub event_arrival_time: Nanos,
    pub event_seq: u64,
    pub event_type: String,
    pub description: String,
}

impl VisibilityWatermark {
    pub fn new() -> Self {
        Self {
            decision_time: 0,
            latest_applied_arrival: 0,
            events_applied: 0,
            violations: Vec::new(),
        }
    }

    /// Advance the decision time (clock moves forward).
    pub fn advance_to(&mut self, new_time: Nanos) {
        debug_assert!(
            new_time >= self.decision_time,
            "Decision time cannot go backward: {} -> {}",
            self.decision_time,
            new_time
        );
        self.decision_time = new_time;
    }

    /// Get current decision time.
    pub fn decision_time(&self) -> Nanos {
        self.decision_time
    }

    /// Check if an event is visible at the current decision_time.
    /// Returns true if `event.arrival_time <= decision_time`.
    #[inline]
    pub fn is_visible(&self, event: &TimestampedEvent) -> bool {
        event.time <= self.decision_time
    }

    /// Assert that an event is visible, panicking in strict mode if not.
    pub fn assert_visible(&mut self, event: &TimestampedEvent) {
        if event.time > self.decision_time {
            let violation = VisibilityViolation {
                decision_time: self.decision_time,
                event_arrival_time: event.time,
                event_seq: event.seq,
                event_type: format!("{:?}", event.event.priority()),
                description: format!(
                    "Event with arrival_time={} not visible at decision_time={}",
                    event.time, self.decision_time
                ),
            };

            if is_strict_mode() {
                panic!(
                    "VISIBILITY VIOLATION (strict mode): {}",
                    violation.description
                );
            } else {
                self.violations.push(violation);
            }
        }
    }

    /// Record that an event has been applied to state.
    /// Panics in strict mode if event.arrival_time > decision_time.
    pub fn record_applied(&mut self, event: &TimestampedEvent) {
        self.assert_visible(event);
        self.latest_applied_arrival = self.latest_applied_arrival.max(event.time);
        self.events_applied += 1;
    }

    /// Get the latest applied event's arrival time.
    pub fn latest_applied_arrival(&self) -> Nanos {
        self.latest_applied_arrival
    }

    /// Check invariant: latest_applied <= decision_time.
    pub fn check_invariant(&self) -> bool {
        self.latest_applied_arrival <= self.decision_time
    }

    /// Get all violations (only populated in non-strict mode).
    pub fn violations(&self) -> &[VisibilityViolation] {
        &self.violations
    }

    /// Get count of events applied.
    pub fn events_applied(&self) -> u64 {
        self.events_applied
    }
}

impl Default for VisibilityWatermark {
    fn default() -> Self {
        Self::new()
    }
}

/// Proof that a strategy decision was made without look-ahead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionProof {
    /// Monotonically increasing decision ID.
    pub decision_id: u64,
    /// Time at which the decision was made.
    pub decision_time: Nanos,
    /// Market/token ID if applicable.
    pub market_id: Option<String>,
    /// Input events that affected state since last decision.
    pub input_events: Vec<InputEventRecord>,
    /// Hash of the proof for quick comparison.
    pub proof_hash: u64,
}

/// Record of an input event for DecisionProof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEventRecord {
    pub arrival_time: Nanos,
    pub source_time: Nanos,
    pub seq: u64,
    pub event_type: String,
}

impl DecisionProof {
    pub fn new(decision_id: u64, decision_time: Nanos) -> Self {
        Self {
            decision_id,
            decision_time,
            market_id: None,
            input_events: Vec::new(),
            proof_hash: 0,
        }
    }

    pub fn with_market(mut self, market_id: impl Into<String>) -> Self {
        self.market_id = Some(market_id.into());
        self
    }

    pub fn add_input_event(&mut self, event: &TimestampedEvent) {
        self.input_events.push(InputEventRecord {
            arrival_time: event.time,
            source_time: event.source_time,
            seq: event.seq,
            event_type: format!("{:?}", event.event.priority()),
        });
    }

    pub fn finalize(&mut self) {
        // Simple hash for comparison
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.decision_id.hash(&mut hasher);
        self.decision_time.hash(&mut hasher);
        for ev in &self.input_events {
            ev.arrival_time.hash(&mut hasher);
            ev.seq.hash(&mut hasher);
        }
        self.proof_hash = hasher.finish();
    }
}

/// Ring buffer for storing recent DecisionProofs.
#[derive(Debug)]
pub struct DecisionProofBuffer {
    buffer: VecDeque<DecisionProof>,
    capacity: usize,
    next_decision_id: u64,
}

impl DecisionProofBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
            next_decision_id: 1,
        }
    }

    /// Start a new decision proof.
    pub fn start_decision(&mut self, decision_time: Nanos) -> DecisionProof {
        let id = self.next_decision_id;
        self.next_decision_id += 1;
        DecisionProof::new(id, decision_time)
    }

    /// Commit a completed decision proof.
    pub fn commit(&mut self, mut proof: DecisionProof) {
        proof.finalize();
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(proof);
    }

    /// Get the last N decision proofs.
    pub fn last_n(&self, n: usize) -> Vec<&DecisionProof> {
        self.buffer.iter().rev().take(n).collect()
    }

    /// Get all decision proofs.
    pub fn all(&self) -> &VecDeque<DecisionProof> {
        &self.buffer
    }

    /// Get the last decision proof.
    pub fn last(&self) -> Option<&DecisionProof> {
        self.buffer.back()
    }

    /// Dump debug info on invariant failure.
    pub fn dump_on_failure(&self, last_k: usize) -> String {
        let mut output = String::new();
        output.push_str("=== DECISION PROOF DUMP (on failure) ===\n");
        for proof in self.last_n(last_k) {
            output.push_str(&format!(
                "Decision #{}: time={}, market={:?}, inputs={}\n",
                proof.decision_id,
                proof.decision_time,
                proof.market_id,
                proof.input_events.len()
            ));
            for (i, ev) in proof.input_events.iter().enumerate() {
                output.push_str(&format!(
                    "  Input {}: arrival={}, source={}, seq={}, type={}\n",
                    i, ev.arrival_time, ev.source_time, ev.seq, ev.event_type
                ));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::events::{Event, Side};

    fn make_test_event(source_time: Nanos, arrival_time: Nanos, seq: u64) -> TimestampedEvent {
        TimestampedEvent {
            time: arrival_time,
            source_time,
            seq,
            source: 0,
            event: Event::TradePrint {
                token_id: "TEST".into(),
                price: 0.5,
                size: 100.0,
                aggressor_side: Side::Buy,
                trade_id: None,
            },
        }
    }

    #[test]
    fn test_arrival_time_mapper_fixed_latency() {
        let mut mapper = ArrivalTimeMapper::new(SimArrivalPolicy::fixed_latency(5 * NS_PER_MS));

        let (arrival, latency) = mapper.map_arrival_time(100 * NS_PER_MS, false);
        assert_eq!(arrival, 105 * NS_PER_MS);
        assert_eq!(latency, 5 * NS_PER_MS);
    }

    #[test]
    fn test_visibility_watermark_basic() {
        let mut wm = VisibilityWatermark::new();

        // Advance to t=15
        wm.advance_to(15);

        // Event at t=10 should be visible
        let e1 = make_test_event(10, 10, 1);
        assert!(wm.is_visible(&e1));

        // Event at t=20 should NOT be visible
        let e2 = make_test_event(12, 20, 2);
        assert!(!wm.is_visible(&e2));
    }

    #[test]
    fn test_visibility_watermark_record_applied() {
        let mut wm = VisibilityWatermark::new();
        wm.advance_to(15);

        let e1 = make_test_event(10, 10, 1);
        wm.record_applied(&e1);

        assert_eq!(wm.events_applied(), 1);
        assert_eq!(wm.latest_applied_arrival(), 10);
        assert!(wm.check_invariant());
    }

    #[test]
    #[should_panic(expected = "VISIBILITY VIOLATION")]
    fn test_visibility_strict_mode_panics() {
        enable_strict_mode();
        let mut wm = VisibilityWatermark::new();
        wm.advance_to(15);

        // Try to apply a future event - should panic
        let future_event = make_test_event(12, 20, 2);
        wm.record_applied(&future_event);
    }

    #[test]
    fn test_decision_proof_buffer() {
        let mut buffer = DecisionProofBuffer::new(3);

        for t in [10, 20, 30, 40] {
            let mut proof = buffer.start_decision(t);
            proof.add_input_event(&make_test_event(t - 5, t - 2, t as u64));
            buffer.commit(proof);
        }

        // Should only have last 3
        assert_eq!(buffer.all().len(), 3);
        assert_eq!(buffer.last().unwrap().decision_time, 40);
    }

    #[test]
    fn test_decision_proof_deterministic_hash() {
        let mut proof1 = DecisionProof::new(1, 100);
        proof1.add_input_event(&make_test_event(90, 95, 1));
        proof1.finalize();

        let mut proof2 = DecisionProof::new(1, 100);
        proof2.add_input_event(&make_test_event(90, 95, 1));
        proof2.finalize();

        assert_eq!(proof1.proof_hash, proof2.proof_hash);
    }
}
