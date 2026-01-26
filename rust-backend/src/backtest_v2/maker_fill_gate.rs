//! Maker Fill Gate - Single Choke Point for Passive Fill Validation
//!
//! This module enforces truthfulness in maker (passive) fill simulation by requiring
//! explicit proofs for every maker fill:
//!
//! 1. **QueueProof**: Proves queue position was consumed (queue_ahead <= 0)
//! 2. **CancelRaceProof**: Proves order was live at venue at fill time
//!
//! # Production-Grade Invariant
//!
//! In production-grade mode, a maker fill is admissible if and only if:
//! - QueueProof exists AND validates (remaining_queue_ahead <= 0)
//! - CancelRaceProof exists AND validates (order_live_at_fill == true)
//!
//! Missing or failing proofs result in REJECTED fills that are never credited to PnL.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderId, Price, Side, Size};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// =============================================================================
// QUEUE PROOF
// =============================================================================

/// Proof that queue position allows a maker fill.
///
/// A maker fill is only valid if the order has reached the front of the queue,
/// meaning all orders ahead have been consumed by trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueProof {
    /// Order being filled.
    pub order_id: OrderId,
    /// Market/token identifier.
    pub market_id: String,
    /// Order side.
    pub side: Side,
    /// Order price.
    pub price: Price,
    /// Order quantity.
    pub qty: Size,
    /// Time strategy decided to place the order.
    pub decision_time_ns: Nanos,
    /// Time order becomes live at venue (after submission latency).
    pub venue_arrival_time_ns: Nanos,
    /// Time fill would occur.
    pub fill_time_ns: Nanos,
    /// Queue ahead (in size) when order arrived at venue.
    pub queue_ahead_at_arrival: Size,
    /// Queue consumed (by trades) between arrival and fill time.
    pub queue_consumed_by_time: Size,
    /// Remaining queue ahead = queue_ahead_at_arrival - queue_consumed_by_time.
    pub remaining_queue_ahead: Size,
    /// Deterministic hash over proof inputs.
    pub proof_hash: u64,
}

impl QueueProof {
    /// Create a new queue proof.
    pub fn new(
        order_id: OrderId,
        market_id: String,
        side: Side,
        price: Price,
        qty: Size,
        decision_time_ns: Nanos,
        venue_arrival_time_ns: Nanos,
        fill_time_ns: Nanos,
        queue_ahead_at_arrival: Size,
        queue_consumed_by_time: Size,
    ) -> Self {
        let remaining_queue_ahead = (queue_ahead_at_arrival - queue_consumed_by_time).max(0.0);
        
        let mut proof = Self {
            order_id,
            market_id,
            side,
            price,
            qty,
            decision_time_ns,
            venue_arrival_time_ns,
            fill_time_ns,
            queue_ahead_at_arrival,
            queue_consumed_by_time,
            remaining_queue_ahead,
            proof_hash: 0,
        };
        proof.proof_hash = proof.compute_hash();
        proof
    }
    
    /// Compute deterministic hash over proof inputs.
    fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.order_id.hash(&mut hasher);
        self.market_id.hash(&mut hasher);
        (self.side as u8).hash(&mut hasher);
        self.price.to_bits().hash(&mut hasher);
        self.qty.to_bits().hash(&mut hasher);
        self.decision_time_ns.hash(&mut hasher);
        self.venue_arrival_time_ns.hash(&mut hasher);
        self.fill_time_ns.hash(&mut hasher);
        self.queue_ahead_at_arrival.to_bits().hash(&mut hasher);
        self.queue_consumed_by_time.to_bits().hash(&mut hasher);
        hasher.finish()
    }
    
    /// Check if queue position allows fill (queue consumed to front).
    pub fn validates(&self) -> bool {
        self.remaining_queue_ahead <= 0.0
    }
}

// =============================================================================
// CANCEL RACE PROOF
// =============================================================================

/// Rationale for order live status determination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelRaceRationale {
    /// No cancel was requested - order is live.
    NoCancelRequested,
    /// Fill time precedes cancel arrival at venue.
    FillBeforeCancelArrival,
    /// Cancel arrival precedes fill time - order was cancelled.
    CancelBeforeFill,
    /// Cancel ack received before fill - order definitely cancelled.
    CancelAckedBeforeFill,
    /// Fill time is between cancel request and cancel ack (ambiguous, treat as race).
    RaceCondition,
}

/// Proof that order was live at venue at fill time.
///
/// A maker fill is only valid if the order was still live at the venue
/// when the fill would have occurred, accounting for cancel latency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRaceProof {
    /// Order being filled.
    pub order_id: OrderId,
    /// Time cancel request was sent (None if no cancel).
    pub cancel_request_time_ns: Option<Nanos>,
    /// Time cancel would arrive at venue (request + latency).
    pub cancel_venue_arrival_time_ns: Option<Nanos>,
    /// Time cancel was acknowledged by venue (if modeled).
    pub cancel_ack_time_ns: Option<Nanos>,
    /// Time fill would occur.
    pub fill_time_ns: Nanos,
    /// Was order live at fill time?
    pub order_live_at_fill: bool,
    /// Rationale for determination.
    pub rationale: CancelRaceRationale,
    /// Deterministic hash over proof inputs.
    pub proof_hash: u64,
}

impl CancelRaceProof {
    /// Create a cancel race proof with no pending cancel.
    pub fn no_cancel(order_id: OrderId, fill_time_ns: Nanos) -> Self {
        let mut proof = Self {
            order_id,
            cancel_request_time_ns: None,
            cancel_venue_arrival_time_ns: None,
            cancel_ack_time_ns: None,
            fill_time_ns,
            order_live_at_fill: true,
            rationale: CancelRaceRationale::NoCancelRequested,
            proof_hash: 0,
        };
        proof.proof_hash = proof.compute_hash();
        proof
    }
    
    /// Create a cancel race proof with pending cancel.
    pub fn with_cancel(
        order_id: OrderId,
        cancel_request_time_ns: Nanos,
        cancel_latency_ns: Nanos,
        cancel_ack_time_ns: Option<Nanos>,
        fill_time_ns: Nanos,
    ) -> Self {
        let cancel_venue_arrival_time_ns = cancel_request_time_ns + cancel_latency_ns;
        
        // Determine if order is live at fill time
        let (order_live_at_fill, rationale) = if let Some(ack_time) = cancel_ack_time_ns {
            if fill_time_ns < ack_time {
                // Fill before ack - check if before venue arrival
                if fill_time_ns < cancel_venue_arrival_time_ns {
                    (true, CancelRaceRationale::FillBeforeCancelArrival)
                } else {
                    // Fill after cancel arrival but before ack - race condition
                    // Conservative: treat as cancelled
                    (false, CancelRaceRationale::RaceCondition)
                }
            } else {
                // Fill after cancel ack - definitely cancelled
                (false, CancelRaceRationale::CancelAckedBeforeFill)
            }
        } else {
            // No ack modeled - use venue arrival time
            if fill_time_ns < cancel_venue_arrival_time_ns {
                (true, CancelRaceRationale::FillBeforeCancelArrival)
            } else {
                (false, CancelRaceRationale::CancelBeforeFill)
            }
        };
        
        let mut proof = Self {
            order_id,
            cancel_request_time_ns: Some(cancel_request_time_ns),
            cancel_venue_arrival_time_ns: Some(cancel_venue_arrival_time_ns),
            cancel_ack_time_ns,
            fill_time_ns,
            order_live_at_fill,
            rationale,
            proof_hash: 0,
        };
        proof.proof_hash = proof.compute_hash();
        proof
    }
    
    /// Compute deterministic hash over proof inputs.
    fn compute_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.order_id.hash(&mut hasher);
        self.cancel_request_time_ns.hash(&mut hasher);
        self.cancel_venue_arrival_time_ns.hash(&mut hasher);
        self.cancel_ack_time_ns.hash(&mut hasher);
        self.fill_time_ns.hash(&mut hasher);
        self.order_live_at_fill.hash(&mut hasher);
        (self.rationale as u8).hash(&mut hasher);
        hasher.finish()
    }
    
    /// Check if cancel race allows fill.
    pub fn validates(&self) -> bool {
        self.order_live_at_fill
    }
}

// =============================================================================
// MAKER FILL CANDIDATE
// =============================================================================

/// A candidate maker fill awaiting validation.
#[derive(Debug, Clone)]
pub struct MakerFillCandidate {
    pub order_id: OrderId,
    pub market_id: String,
    pub side: Side,
    pub price: Price,
    pub size: Size,
    pub fill_time_ns: Nanos,
    pub fee: f64,
}

// =============================================================================
// ADMISSIBLE FILL
// =============================================================================

/// A maker fill that has passed all validation gates.
#[derive(Debug, Clone)]
pub struct AdmissibleFill {
    pub candidate: MakerFillCandidate,
    pub queue_proof: QueueProof,
    pub cancel_proof: CancelRaceProof,
    /// Combined hash of both proofs for determinism verification.
    pub combined_proof_hash: u64,
}

impl AdmissibleFill {
    fn new(
        candidate: MakerFillCandidate,
        queue_proof: QueueProof,
        cancel_proof: CancelRaceProof,
    ) -> Self {
        let combined_proof_hash = {
            let mut hasher = DefaultHasher::new();
            queue_proof.proof_hash.hash(&mut hasher);
            cancel_proof.proof_hash.hash(&mut hasher);
            hasher.finish()
        };
        
        Self {
            candidate,
            queue_proof,
            cancel_proof,
            combined_proof_hash,
        }
    }
}

// =============================================================================
// REJECTION REASON
// =============================================================================

/// Reason a maker fill was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Queue proof is missing (order not tracked).
    MissingQueueProof,
    /// Queue proof failed (queue_ahead > 0).
    QueueNotConsumed { remaining_ahead: i64 },
    /// Cancel race proof missing.
    MissingCancelProof,
    /// Order was cancelled before fill.
    CancelledBeforeFill,
    /// Dataset doesn't support maker fills.
    DatasetNotMakerViable,
    /// Maker fills explicitly disabled.
    MakerFillsDisabled,
    /// Production mode requires explicit proofs.
    ProductionRequiresProofs,
}

impl RejectionReason {
    pub fn description(&self) -> &'static str {
        match self {
            Self::MissingQueueProof => "Queue proof missing - order not tracked in queue model",
            Self::QueueNotConsumed { .. } => "Queue not consumed - orders ahead remain",
            Self::MissingCancelProof => "Cancel race proof missing",
            Self::CancelledBeforeFill => "Order cancelled before fill time",
            Self::DatasetNotMakerViable => "Dataset does not support maker fills",
            Self::MakerFillsDisabled => "Maker fills explicitly disabled in configuration",
            Self::ProductionRequiresProofs => "Production mode requires explicit queue and cancel proofs",
        }
    }
}

// =============================================================================
// MAKER FILL GATE CONFIGURATION
// =============================================================================

/// Configuration for the maker fill gate.
#[derive(Debug, Clone)]
pub struct MakerFillGateConfig {
    /// Whether production-grade strictness is required.
    pub production_grade: bool,
    /// Whether maker fills are enabled at all.
    pub maker_fills_enabled: bool,
    /// Whether dataset supports maker fills.
    pub dataset_maker_viable: bool,
    /// Allow fills without explicit queue proof (NEVER in production).
    pub allow_missing_queue_proof: bool,
    /// Allow fills without explicit cancel proof (NEVER in production).
    pub allow_missing_cancel_proof: bool,
}

impl MakerFillGateConfig {
    /// Production-grade configuration - no approximations allowed.
    pub fn production() -> Self {
        Self {
            production_grade: true,
            maker_fills_enabled: true,
            dataset_maker_viable: true,
            allow_missing_queue_proof: false,
            allow_missing_cancel_proof: false,
        }
    }
    
    /// Research configuration - allows some approximations but marks results.
    pub fn research() -> Self {
        Self {
            production_grade: false,
            maker_fills_enabled: true,
            dataset_maker_viable: true,
            allow_missing_queue_proof: true,
            allow_missing_cancel_proof: true,
        }
    }
    
    /// Disabled configuration - no maker fills allowed.
    pub fn disabled() -> Self {
        Self {
            production_grade: false,
            maker_fills_enabled: false,
            dataset_maker_viable: false,
            allow_missing_queue_proof: false,
            allow_missing_cancel_proof: false,
        }
    }
}

// =============================================================================
// MAKER FILL GATE STATISTICS
// =============================================================================

/// Statistics for maker fill gate validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MakerFillGateStats {
    /// Total maker fill candidates received.
    pub candidates_received: u64,
    /// Fills admitted (all proofs valid).
    pub fills_admitted: u64,
    /// Fills rejected (proof missing or invalid).
    pub fills_rejected: u64,
    /// Rejections by reason.
    pub rejections_by_reason: RejectionCounts,
    /// Proofs bypassed (research mode only).
    pub proofs_bypassed: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RejectionCounts {
    pub missing_queue_proof: u64,
    pub queue_not_consumed: u64,
    pub missing_cancel_proof: u64,
    pub cancelled_before_fill: u64,
    pub dataset_not_viable: u64,
    pub maker_disabled: u64,
    pub production_requires_proofs: u64,
}

impl MakerFillGateStats {
    /// Compute the admission rate (fills_admitted / candidates_received).
    pub fn admission_rate(&self) -> f64 {
        if self.candidates_received > 0 {
            self.fills_admitted as f64 / self.candidates_received as f64
        } else {
            0.0
        }
    }
}

impl RejectionCounts {
    fn increment(&mut self, reason: RejectionReason) {
        match reason {
            RejectionReason::MissingQueueProof => self.missing_queue_proof += 1,
            RejectionReason::QueueNotConsumed { .. } => self.queue_not_consumed += 1,
            RejectionReason::MissingCancelProof => self.missing_cancel_proof += 1,
            RejectionReason::CancelledBeforeFill => self.cancelled_before_fill += 1,
            RejectionReason::DatasetNotMakerViable => self.dataset_not_viable += 1,
            RejectionReason::MakerFillsDisabled => self.maker_disabled += 1,
            RejectionReason::ProductionRequiresProofs => self.production_requires_proofs += 1,
        }
    }
}

// =============================================================================
// MAKER FILL GATE
// =============================================================================

/// The single choke point for all maker fill validation.
///
/// Every maker fill MUST pass through this gate. There is no other pathway
/// by which a maker fill can be credited to PnL.
pub struct MakerFillGate {
    config: MakerFillGateConfig,
    stats: MakerFillGateStats,
}

impl MakerFillGate {
    /// Create a new maker fill gate.
    pub fn new(config: MakerFillGateConfig) -> Self {
        Self {
            config,
            stats: MakerFillGateStats::default(),
        }
    }
    
    /// Validate a maker fill candidate.
    ///
    /// This is the ONLY gateway for maker fills. Returns either an admitted fill
    /// with valid proofs, or a rejection reason.
    pub fn validate_or_reject(
        &mut self,
        candidate: MakerFillCandidate,
        queue_proof: Option<QueueProof>,
        cancel_proof: Option<CancelRaceProof>,
    ) -> Result<AdmissibleFill, RejectionReason> {
        self.stats.candidates_received += 1;
        
        // Check basic enablement
        if !self.config.maker_fills_enabled {
            self.stats.fills_rejected += 1;
            self.stats.rejections_by_reason.increment(RejectionReason::MakerFillsDisabled);
            return Err(RejectionReason::MakerFillsDisabled);
        }
        
        if !self.config.dataset_maker_viable {
            self.stats.fills_rejected += 1;
            self.stats.rejections_by_reason.increment(RejectionReason::DatasetNotMakerViable);
            return Err(RejectionReason::DatasetNotMakerViable);
        }
        
        // In production mode, proofs are MANDATORY
        if self.config.production_grade {
            // Queue proof required
            let queue_proof = match queue_proof {
                Some(p) => p,
                None => {
                    self.stats.fills_rejected += 1;
                    self.stats.rejections_by_reason.increment(RejectionReason::MissingQueueProof);
                    return Err(RejectionReason::MissingQueueProof);
                }
            };
            
            // Queue must be consumed
            if !queue_proof.validates() {
                self.stats.fills_rejected += 1;
                let remaining = (queue_proof.remaining_queue_ahead * 1000.0) as i64; // milli-units for logging
                let reason = RejectionReason::QueueNotConsumed { remaining_ahead: remaining };
                self.stats.rejections_by_reason.increment(reason);
                return Err(reason);
            }
            
            // Cancel proof required
            let cancel_proof = match cancel_proof {
                Some(p) => p,
                None => {
                    self.stats.fills_rejected += 1;
                    self.stats.rejections_by_reason.increment(RejectionReason::MissingCancelProof);
                    return Err(RejectionReason::MissingCancelProof);
                }
            };
            
            // Cancel race must allow fill
            if !cancel_proof.validates() {
                self.stats.fills_rejected += 1;
                self.stats.rejections_by_reason.increment(RejectionReason::CancelledBeforeFill);
                return Err(RejectionReason::CancelledBeforeFill);
            }
            
            // All proofs valid - admit fill
            self.stats.fills_admitted += 1;
            return Ok(AdmissibleFill::new(candidate, queue_proof, cancel_proof));
        }
        
        // Research mode - allow some approximations but track them
        
        // Handle missing queue proof
        let queue_proof = match queue_proof {
            Some(p) => {
                if !p.validates() {
                    self.stats.fills_rejected += 1;
                    let remaining = (p.remaining_queue_ahead * 1000.0) as i64;
                    let reason = RejectionReason::QueueNotConsumed { remaining_ahead: remaining };
                    self.stats.rejections_by_reason.increment(reason);
                    return Err(reason);
                }
                p
            }
            None => {
                if self.config.allow_missing_queue_proof {
                    self.stats.proofs_bypassed += 1;
                    // Create synthetic proof for tracking
                    QueueProof::new(
                        candidate.order_id,
                        candidate.market_id.clone(),
                        candidate.side,
                        candidate.price,
                        candidate.size,
                        0, // unknown decision time
                        0, // unknown arrival time
                        candidate.fill_time_ns,
                        0.0, // assume no queue (optimistic)
                        0.0,
                    )
                } else {
                    self.stats.fills_rejected += 1;
                    self.stats.rejections_by_reason.increment(RejectionReason::MissingQueueProof);
                    return Err(RejectionReason::MissingQueueProof);
                }
            }
        };
        
        // Handle missing cancel proof
        let cancel_proof = match cancel_proof {
            Some(p) => {
                if !p.validates() {
                    self.stats.fills_rejected += 1;
                    self.stats.rejections_by_reason.increment(RejectionReason::CancelledBeforeFill);
                    return Err(RejectionReason::CancelledBeforeFill);
                }
                p
            }
            None => {
                if self.config.allow_missing_cancel_proof {
                    self.stats.proofs_bypassed += 1;
                    CancelRaceProof::no_cancel(candidate.order_id, candidate.fill_time_ns)
                } else {
                    self.stats.fills_rejected += 1;
                    self.stats.rejections_by_reason.increment(RejectionReason::MissingCancelProof);
                    return Err(RejectionReason::MissingCancelProof);
                }
            }
        };
        
        // Admit fill (with potential proof bypasses noted in stats)
        self.stats.fills_admitted += 1;
        Ok(AdmissibleFill::new(candidate, queue_proof, cancel_proof))
    }
    
    /// Get current statistics.
    pub fn stats(&self) -> &MakerFillGateStats {
        &self.stats
    }
    
    /// Get configuration.
    pub fn config(&self) -> &MakerFillGateConfig {
        &self.config
    }
    
    /// Check if any proofs were bypassed (indicates non-representative results).
    pub fn proofs_were_bypassed(&self) -> bool {
        self.stats.proofs_bypassed > 0
    }
    
    /// Get admission rate (delegates to stats).
    pub fn admission_rate(&self) -> f64 {
        self.stats.admission_rate()
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_candidate(order_id: OrderId) -> MakerFillCandidate {
        MakerFillCandidate {
            order_id,
            market_id: "test-market".to_string(),
            side: Side::Buy,
            price: 0.50,
            size: 100.0,
            fill_time_ns: 1_000_000_000,
            fee: 0.01,
        }
    }
    
    fn make_valid_queue_proof(order_id: OrderId) -> QueueProof {
        QueueProof::new(
            order_id,
            "test-market".to_string(),
            Side::Buy,
            0.50,
            100.0,
            500_000_000,  // decision time
            600_000_000,  // venue arrival
            1_000_000_000, // fill time
            50.0,         // queue ahead at arrival
            60.0,         // queue consumed (more than ahead)
        )
    }
    
    fn make_invalid_queue_proof(order_id: OrderId) -> QueueProof {
        QueueProof::new(
            order_id,
            "test-market".to_string(),
            Side::Buy,
            0.50,
            100.0,
            500_000_000,
            600_000_000,
            1_000_000_000,
            100.0,        // queue ahead at arrival
            50.0,         // only 50 consumed - still 50 ahead!
        )
    }
    
    fn make_valid_cancel_proof(order_id: OrderId) -> CancelRaceProof {
        CancelRaceProof::no_cancel(order_id, 1_000_000_000)
    }
    
    fn make_invalid_cancel_proof(order_id: OrderId) -> CancelRaceProof {
        CancelRaceProof::with_cancel(
            order_id,
            800_000_000,  // cancel request
            100_000_000,  // cancel latency (arrives at 900M)
            None,
            1_000_000_000, // fill at 1B > 900M cancel arrival
        )
    }
    
    #[test]
    fn test_production_mode_rejects_missing_queue_proof() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        let candidate = make_candidate(1);
        let cancel_proof = make_valid_cancel_proof(1);
        
        let result = gate.validate_or_reject(candidate, None, Some(cancel_proof));
        
        assert!(matches!(result, Err(RejectionReason::MissingQueueProof)));
        assert_eq!(gate.stats().fills_rejected, 1);
        assert_eq!(gate.stats().rejections_by_reason.missing_queue_proof, 1);
    }
    
    #[test]
    fn test_production_mode_rejects_queue_not_consumed() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        let candidate = make_candidate(1);
        let queue_proof = make_invalid_queue_proof(1);
        let cancel_proof = make_valid_cancel_proof(1);
        
        let result = gate.validate_or_reject(candidate, Some(queue_proof), Some(cancel_proof));
        
        assert!(matches!(result, Err(RejectionReason::QueueNotConsumed { .. })));
        assert_eq!(gate.stats().rejections_by_reason.queue_not_consumed, 1);
    }
    
    #[test]
    fn test_production_mode_rejects_missing_cancel_proof() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        let candidate = make_candidate(1);
        let queue_proof = make_valid_queue_proof(1);
        
        let result = gate.validate_or_reject(candidate, Some(queue_proof), None);
        
        assert!(matches!(result, Err(RejectionReason::MissingCancelProof)));
    }
    
    #[test]
    fn test_production_mode_rejects_cancelled_order() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        let candidate = make_candidate(1);
        let queue_proof = make_valid_queue_proof(1);
        let cancel_proof = make_invalid_cancel_proof(1);
        
        let result = gate.validate_or_reject(candidate, Some(queue_proof), Some(cancel_proof));
        
        assert!(matches!(result, Err(RejectionReason::CancelledBeforeFill)));
    }
    
    #[test]
    fn test_production_mode_admits_valid_fill() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        let candidate = make_candidate(1);
        let queue_proof = make_valid_queue_proof(1);
        let cancel_proof = make_valid_cancel_proof(1);
        
        let result = gate.validate_or_reject(candidate, Some(queue_proof), Some(cancel_proof));
        
        assert!(result.is_ok());
        let fill = result.unwrap();
        assert_eq!(fill.candidate.order_id, 1);
        assert!(fill.queue_proof.validates());
        assert!(fill.cancel_proof.validates());
        assert_eq!(gate.stats().fills_admitted, 1);
    }
    
    #[test]
    fn test_disabled_mode_rejects_all() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::disabled());
        let candidate = make_candidate(1);
        let queue_proof = make_valid_queue_proof(1);
        let cancel_proof = make_valid_cancel_proof(1);
        
        let result = gate.validate_or_reject(candidate, Some(queue_proof), Some(cancel_proof));
        
        assert!(matches!(result, Err(RejectionReason::MakerFillsDisabled)));
    }
    
    #[test]
    fn test_research_mode_allows_missing_proofs_but_tracks() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::research());
        let candidate = make_candidate(1);
        
        let result = gate.validate_or_reject(candidate, None, None);
        
        assert!(result.is_ok());
        assert!(gate.proofs_were_bypassed());
        assert_eq!(gate.stats().proofs_bypassed, 2); // both proofs bypassed
    }
    
    #[test]
    fn test_queue_proof_hash_determinism() {
        let proof1 = make_valid_queue_proof(1);
        let proof2 = make_valid_queue_proof(1);
        
        assert_eq!(proof1.proof_hash, proof2.proof_hash);
    }
    
    #[test]
    fn test_cancel_proof_fill_wins_race() {
        let proof = CancelRaceProof::with_cancel(
            1,
            900_000_000,  // cancel request at 900M
            200_000_000,  // latency 200M (arrives at 1.1B)
            None,
            1_000_000_000, // fill at 1B < 1.1B cancel arrival
        );
        
        assert!(proof.validates());
        assert_eq!(proof.rationale, CancelRaceRationale::FillBeforeCancelArrival);
    }
    
    #[test]
    fn test_cancel_proof_cancel_wins_race() {
        let proof = CancelRaceProof::with_cancel(
            1,
            800_000_000,  // cancel request at 800M
            100_000_000,  // latency 100M (arrives at 900M)
            None,
            1_000_000_000, // fill at 1B > 900M cancel arrival
        );
        
        assert!(!proof.validates());
        assert_eq!(proof.rationale, CancelRaceRationale::CancelBeforeFill);
    }
    
    #[test]
    fn test_admission_rate_calculation() {
        let mut gate = MakerFillGate::new(MakerFillGateConfig::production());
        
        // 2 valid fills
        for i in 1..=2 {
            let candidate = make_candidate(i);
            let queue_proof = make_valid_queue_proof(i);
            let cancel_proof = make_valid_cancel_proof(i);
            let _ = gate.validate_or_reject(candidate, Some(queue_proof), Some(cancel_proof));
        }
        
        // 1 rejected
        let candidate = make_candidate(3);
        let _ = gate.validate_or_reject(candidate, None, None);
        
        assert!((gate.admission_rate() - 0.666).abs() < 0.01);
    }
}
