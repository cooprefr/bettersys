//! Stream Integrity Guard
//!
//! Unified integrity enforcement for both live and backtest data paths.
//! Detects and handles duplicates, gaps, and out-of-order events with
//! explicit, deterministic policies.
//!
//! # Design Principles
//!
//! 1. **No silent best-effort**: Every pathology triggers a documented action
//! 2. **Same code path**: Identical guard used in live and backtest
//! 3. **Deterministic**: Same input always produces same output/counters
//! 4. **Configurable**: Policy can be adjusted per stream type
//! 5. **Observable**: All actions are logged and counted

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, TimestampedEvent, TokenId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use tracing::{debug, error, info, warn};

// =============================================================================
// PATHOLOGY POLICY DEFINITIONS
// =============================================================================

/// Action to take when a duplicate event is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicatePolicy {
    /// Drop the duplicate, increment counter, continue processing.
    Drop,
    /// Halt processing immediately (duplicate implies corruption).
    Halt,
}

impl Default for DuplicatePolicy {
    fn default() -> Self {
        Self::Drop
    }
}

/// Action to take when a sequence gap is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GapPolicy {
    /// Halt processing immediately (gap may indicate data loss).
    Halt,
    /// Request resync via snapshot, drop deltas until snapshot arrives.
    Resync,
}

impl Default for GapPolicy {
    fn default() -> Self {
        Self::Halt
    }
}

/// Action to take when an out-of-order event is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutOfOrderPolicy {
    /// Drop the out-of-order event, increment counter.
    Drop,
    /// Buffer events and release in-order (bounded buffer).
    Reorder,
    /// Halt processing immediately.
    Halt,
}

impl Default for OutOfOrderPolicy {
    fn default() -> Self {
        Self::Halt
    }
}

/// Complete pathology policy for a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathologyPolicy {
    /// Action on duplicate event.
    pub on_duplicate: DuplicatePolicy,
    /// Action on sequence gap.
    pub on_gap: GapPolicy,
    /// Action on out-of-order event.
    pub on_out_of_order: OutOfOrderPolicy,
    /// Maximum allowed gap before triggering action (0 = any gap triggers).
    pub gap_tolerance: u64,
    /// Size of reorder buffer (only used if on_out_of_order = Reorder).
    pub reorder_buffer_size: usize,
    /// How far back in time to accept (nanoseconds, 0 = strict monotonic).
    pub timestamp_jitter_tolerance_ns: Nanos,
}

impl Default for PathologyPolicy {
    fn default() -> Self {
        Self::strict()
    }
}

impl PathologyPolicy {
    /// Strict policy - halts on any pathology. Use for production-grade backtests.
    pub fn strict() -> Self {
        Self {
            on_duplicate: DuplicatePolicy::Drop,
            on_gap: GapPolicy::Halt,
            on_out_of_order: OutOfOrderPolicy::Halt,
            gap_tolerance: 0,
            reorder_buffer_size: 0,
            timestamp_jitter_tolerance_ns: 0,
        }
    }

    /// Resilient policy - recovers from common issues. Use for exploratory analysis.
    pub fn resilient() -> Self {
        Self {
            on_duplicate: DuplicatePolicy::Drop,
            on_gap: GapPolicy::Resync,
            on_out_of_order: OutOfOrderPolicy::Reorder,
            gap_tolerance: 10, // Allow small gaps
            reorder_buffer_size: 100,
            timestamp_jitter_tolerance_ns: 1_000_000, // 1ms jitter allowed
        }
    }

    /// Permissive policy - drops problematic events, never halts.
    /// Results must be marked APPROXIMATE.
    pub fn permissive() -> Self {
        Self {
            on_duplicate: DuplicatePolicy::Drop,
            on_gap: GapPolicy::Resync,
            on_out_of_order: OutOfOrderPolicy::Drop,
            gap_tolerance: 1000, // Large gap tolerance
            reorder_buffer_size: 0,
            timestamp_jitter_tolerance_ns: 100_000_000, // 100ms jitter allowed
        }
    }
}

// =============================================================================
// PATHOLOGY COUNTERS
// =============================================================================

/// Counters for detected pathologies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathologyCounters {
    /// Number of duplicate events dropped.
    pub duplicates_dropped: u64,
    /// Number of sequence gaps detected.
    pub gaps_detected: u64,
    /// Total missing sequence numbers across all gaps.
    pub total_missing_sequences: u64,
    /// Number of out-of-order events detected.
    pub out_of_order_detected: u64,
    /// Number of events dropped due to out-of-order.
    pub out_of_order_dropped: u64,
    /// Number of events reordered.
    pub reordered_events: u64,
    /// Number of resync operations triggered.
    pub resync_count: u64,
    /// Number of reorder buffer overflows.
    pub reorder_buffer_overflows: u64,
    /// Whether processing was halted due to integrity failure.
    pub halted: bool,
    /// Halt reason (if halted).
    pub halt_reason: Option<String>,
    /// Total events processed.
    pub total_events_processed: u64,
    /// Total events forwarded (after filtering).
    pub total_events_forwarded: u64,
}

impl PathologyCounters {
    /// Check if any pathology was detected.
    pub fn has_pathologies(&self) -> bool {
        self.duplicates_dropped > 0
            || self.gaps_detected > 0
            || self.out_of_order_detected > 0
            || self.resync_count > 0
            || self.halted
    }

    /// Get a summary suitable for logging.
    pub fn summary(&self) -> String {
        format!(
            "processed={}, forwarded={}, dups={}, gaps={} (missing={}), ooo={}, resyncs={}, halted={}",
            self.total_events_processed,
            self.total_events_forwarded,
            self.duplicates_dropped,
            self.gaps_detected,
            self.total_missing_sequences,
            self.out_of_order_detected,
            self.resync_count,
            self.halted
        )
    }
}

// =============================================================================
// STREAM STATE
// =============================================================================

/// Synchronization state for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    /// Initial state - waiting for first event or snapshot.
    Initial,
    /// In sync - accepting deltas normally.
    InSync,
    /// Need snapshot - gap detected, waiting for snapshot to resync.
    NeedSnapshot,
    /// Resyncing - processing snapshot, will transition to InSync.
    Resyncing,
    /// Halted - integrity failure, no further processing.
    Halted,
}

impl Default for SyncState {
    fn default() -> Self {
        Self::Initial
    }
}

/// Per-token stream state.
#[derive(Debug, Clone)]
struct TokenState {
    /// Current sync state.
    sync_state: SyncState,
    /// Last seen sequence number.
    last_seq: Option<u64>,
    /// Last seen timestamp.
    last_timestamp: Nanos,
    /// Set of seen event hashes (for deduplication).
    seen_hashes: HashSet<u64>,
    /// Reorder buffer (for out-of-order handling).
    reorder_buffer: VecDeque<(u64, TimestampedEvent)>, // (seq, event)
    /// Expected next sequence (if known).
    expected_seq: Option<u64>,
}

impl TokenState {
    fn new() -> Self {
        Self {
            sync_state: SyncState::Initial,
            last_seq: None,
            last_timestamp: 0,
            seen_hashes: HashSet::with_capacity(1000),
            reorder_buffer: VecDeque::new(),
            expected_seq: None,
        }
    }
}

// =============================================================================
// INTEGRITY GUARD
// =============================================================================

/// Result of processing an event through the integrity guard.
#[derive(Debug, Clone)]
pub enum IntegrityResult {
    /// Event passed integrity checks, forward it.
    Forward(TimestampedEvent),
    /// Event was dropped (duplicate, out-of-order, etc.).
    Dropped(DropReason),
    /// Events released from reorder buffer (in-order).
    Reordered(Vec<TimestampedEvent>),
    /// Stream needs resync - require snapshot before continuing.
    NeedResync { token_id: TokenId, last_good_seq: Option<u64> },
    /// Processing halted due to integrity failure.
    Halted(HaltReason),
}

/// Reason for dropping an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DropReason {
    Duplicate { hash: u64 },
    OutOfOrder { expected_seq: u64, actual_seq: u64 },
    TimestampRegression { expected_min: Nanos, actual: Nanos },
    StreamHalted,
    AwaitingResync,
}

/// Reason for halting processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HaltReason {
    pub token_id: TokenId,
    pub reason: HaltType,
    pub context: String,
}

/// Type of halt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HaltType {
    SequenceGap { expected: u64, actual: u64, gap_size: u64 },
    OutOfOrder { expected: u64, actual: u64 },
    DuplicateCorruption { hash: u64 },
    ReorderBufferOverflow { buffer_size: usize },
    TimestampRegression { expected_min: Nanos, actual: Nanos },
}

/// Stream integrity guard - enforces pathology policies deterministically.
pub struct StreamIntegrityGuard {
    /// Policy configuration.
    policy: PathologyPolicy,
    /// Per-token state.
    token_states: HashMap<TokenId, TokenState>,
    /// Global counters.
    counters: PathologyCounters,
    /// Whether guard is in strict mode (affects logging).
    strict_mode: bool,
    /// Maximum size for seen_hashes before pruning.
    max_dedupe_set_size: usize,
}

impl StreamIntegrityGuard {
    /// Create a new integrity guard with the given policy.
    pub fn new(policy: PathologyPolicy) -> Self {
        let strict_mode = matches!(policy.on_gap, GapPolicy::Halt);
        Self {
            policy,
            token_states: HashMap::new(),
            counters: PathologyCounters::default(),
            strict_mode,
            max_dedupe_set_size: 100_000,
        }
    }

    /// Create with strict policy (default for production).
    pub fn strict() -> Self {
        Self::new(PathologyPolicy::strict())
    }

    /// Create with resilient policy.
    pub fn resilient() -> Self {
        Self::new(PathologyPolicy::resilient())
    }

    /// Get current counters.
    pub fn counters(&self) -> &PathologyCounters {
        &self.counters
    }

    /// Get sync state for a token.
    pub fn sync_state(&self, token_id: &str) -> SyncState {
        self.token_states
            .get(token_id)
            .map(|s| s.sync_state)
            .unwrap_or(SyncState::Initial)
    }

    /// Check if all streams are in sync.
    pub fn all_in_sync(&self) -> bool {
        self.token_states.values().all(|s| matches!(s.sync_state, SyncState::InSync | SyncState::Initial))
    }

    /// Reset state for a new run.
    pub fn reset(&mut self) {
        self.token_states.clear();
        self.counters = PathologyCounters::default();
    }

    /// Process an event through the integrity guard.
    pub fn process(&mut self, event: TimestampedEvent) -> IntegrityResult {
        self.counters.total_events_processed += 1;

        // Extract token_id and sequence from event
        let (token_id, exchange_seq, is_snapshot) = match &event.event {
            Event::L2BookSnapshot { token_id, exchange_seq, .. } => {
                (token_id.clone(), Some(*exchange_seq), true)
            }
            Event::L2Delta { token_id, exchange_seq, .. } => {
                (token_id.clone(), Some(*exchange_seq), false)
            }
            Event::TradePrint { token_id, .. } => {
                // Trade prints don't have inherent sequence numbers
                // They are checked for duplicates via event hash, not sequence gaps
                (token_id.clone(), None, false)
            }
            Event::MarketStatusChange { token_id, .. } |
            Event::ResolutionEvent { token_id, .. } => {
                (token_id.clone(), None, false)
            }
            // Non-market events bypass integrity checks
            _ => {
                self.counters.total_events_forwarded += 1;
                return IntegrityResult::Forward(event);
            }
        };

        // Ensure state exists and get quick checks
        if !self.token_states.contains_key(&token_id) {
            self.token_states.insert(token_id.clone(), TokenState::new());
        }
        
        // Quick state checks (read-only)
        {
            let state = self.token_states.get(&token_id).unwrap();
            
            // Check if halted
            if state.sync_state == SyncState::Halted {
                return IntegrityResult::Dropped(DropReason::StreamHalted);
            }

            // Check if awaiting resync
            if state.sync_state == SyncState::NeedSnapshot && !is_snapshot {
                return IntegrityResult::Dropped(DropReason::AwaitingResync);
            }
        }

        // Compute event hash for deduplication
        let event_hash = compute_event_hash(&event);

        // Check for duplicate
        let is_duplicate = {
            let state = self.token_states.get(&token_id).unwrap();
            state.seen_hashes.contains(&event_hash)
        };
        
        if is_duplicate {
            self.counters.duplicates_dropped += 1;
            debug!(
                token_id = %token_id,
                hash = event_hash,
                "Duplicate event detected"
            );

            match self.policy.on_duplicate {
                DuplicatePolicy::Drop => {
                    return IntegrityResult::Dropped(DropReason::Duplicate { hash: event_hash });
                }
                DuplicatePolicy::Halt => {
                    let state = self.token_states.get_mut(&token_id).unwrap();
                    state.sync_state = SyncState::Halted;
                    self.counters.halted = true;
                    self.counters.halt_reason = Some(format!(
                        "Duplicate event for {} (hash={})",
                        token_id, event_hash
                    ));
                    return IntegrityResult::Halted(HaltReason {
                        token_id,
                        reason: HaltType::DuplicateCorruption { hash: event_hash },
                        context: "Duplicate implies data corruption".to_string(),
                    });
                }
            }
        }

        // Handle snapshot (resync opportunity)
        if is_snapshot {
            let seq = exchange_seq.unwrap_or(0);
            return self.process_snapshot_internal(token_id, seq, event_hash, event);
        }

        // Handle delta/trade with sequence
        if let Some(seq) = exchange_seq {
            return self.process_sequenced_event_internal(token_id, seq, event_hash, event);
        }

        // Non-sequenced event - just check timestamp monotonicity
        self.process_unsequenced_event_internal(token_id, event_hash, event)
    }

    fn process_snapshot_internal(
        &mut self,
        token_id: TokenId,
        exchange_seq: u64,
        event_hash: u64,
        event: TimestampedEvent,
    ) -> IntegrityResult {
        let state = self.token_states.get_mut(&token_id).unwrap();
        
        // If we were waiting for resync, count it
        let was_waiting = state.sync_state == SyncState::NeedSnapshot;
        
        // Snapshots always reset state
        state.sync_state = SyncState::InSync;
        state.last_seq = Some(exchange_seq);
        state.last_timestamp = event.time;
        state.expected_seq = Some(exchange_seq + 1);
        state.reorder_buffer.clear();

        // Add to dedupe set
        add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);

        if was_waiting {
            self.counters.resync_count += 1;
            info!(
                token_id = %token_id,
                seq = exchange_seq,
                "Resync complete via snapshot"
            );
        }

        self.counters.total_events_forwarded += 1;
        IntegrityResult::Forward(event)
    }

    fn process_sequenced_event_internal(
        &mut self,
        token_id: TokenId,
        seq: u64,
        event_hash: u64,
        event: TimestampedEvent,
    ) -> IntegrityResult {
        let state = self.token_states.get_mut(&token_id).unwrap();
        
        // First event for this token - establish baseline
        if state.sync_state == SyncState::Initial {
            state.sync_state = SyncState::InSync;
            state.last_seq = Some(seq);
            state.last_timestamp = event.time;
            state.expected_seq = Some(seq + 1);
            add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);
            self.counters.total_events_forwarded += 1;
            return IntegrityResult::Forward(event);
        }

        let expected_seq = state.expected_seq.unwrap_or(1);
        let last_seq_opt = state.last_seq;

        // Check for gap (seq > expected)
        if seq > expected_seq {
            let gap_size = seq - expected_seq;
            self.counters.gaps_detected += 1;
            self.counters.total_missing_sequences += gap_size;

            warn!(
                token_id = %token_id,
                expected = expected_seq,
                actual = seq,
                gap = gap_size,
                "Sequence gap detected"
            );

            // Check gap tolerance
            if gap_size > self.policy.gap_tolerance {
                match self.policy.on_gap {
                    GapPolicy::Halt => {
                        state.sync_state = SyncState::Halted;
                        self.counters.halted = true;
                        self.counters.halt_reason = Some(format!(
                            "Sequence gap for {}: expected {}, got {} (gap={})",
                            token_id, expected_seq, seq, gap_size
                        ));
                        return IntegrityResult::Halted(HaltReason {
                            token_id: token_id.clone(),
                            reason: HaltType::SequenceGap {
                                expected: expected_seq,
                                actual: seq,
                                gap_size,
                            },
                            context: format!(
                                "Gap size {} exceeds tolerance {}",
                                gap_size, self.policy.gap_tolerance
                            ),
                        });
                    }
                    GapPolicy::Resync => {
                        state.sync_state = SyncState::NeedSnapshot;
                        self.counters.resync_count += 1;
                        return IntegrityResult::NeedResync {
                            token_id,
                            last_good_seq: last_seq_opt,
                        };
                    }
                }
            }

            // Gap within tolerance - accept with warning
            state.last_seq = Some(seq);
            state.expected_seq = Some(seq + 1);
            state.last_timestamp = event.time;
            add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);
            self.counters.total_events_forwarded += 1;
            return IntegrityResult::Forward(event);
        }

        // Check for out-of-order (seq < expected but > 0)
        if seq < expected_seq && seq > 0 {
            self.counters.out_of_order_detected += 1;

            debug!(
                token_id = %token_id,
                expected = expected_seq,
                actual = seq,
                "Out-of-order event detected"
            );

            match self.policy.on_out_of_order {
                OutOfOrderPolicy::Drop => {
                    self.counters.out_of_order_dropped += 1;
                    return IntegrityResult::Dropped(DropReason::OutOfOrder {
                        expected_seq,
                        actual_seq: seq,
                    });
                }
                OutOfOrderPolicy::Halt => {
                    state.sync_state = SyncState::Halted;
                    self.counters.halted = true;
                    self.counters.halt_reason = Some(format!(
                        "Out-of-order event for {}: expected {}, got {}",
                        token_id, expected_seq, seq
                    ));
                    return IntegrityResult::Halted(HaltReason {
                        token_id,
                        reason: HaltType::OutOfOrder {
                            expected: expected_seq,
                            actual: seq,
                        },
                        context: "Out-of-order events not permitted by policy".to_string(),
                    });
                }
                OutOfOrderPolicy::Reorder => {
                    // Check buffer size limit
                    if state.reorder_buffer.len() >= self.policy.reorder_buffer_size {
                        self.counters.reorder_buffer_overflows += 1;
                        state.sync_state = SyncState::Halted;
                        self.counters.halted = true;
                        self.counters.halt_reason = Some(format!(
                            "Reorder buffer overflow for {} (size={})",
                            token_id, self.policy.reorder_buffer_size
                        ));
                        return IntegrityResult::Halted(HaltReason {
                            token_id,
                            reason: HaltType::ReorderBufferOverflow {
                                buffer_size: self.policy.reorder_buffer_size,
                            },
                            context: "Buffer overflow indicates excessive disorder".to_string(),
                        });
                    }

                    // Add to buffer sorted by sequence
                    let insert_pos = state.reorder_buffer
                        .binary_search_by_key(&seq, |(s, _)| *s)
                        .unwrap_or_else(|pos| pos);
                    state.reorder_buffer.insert(insert_pos, (seq, event));
                    add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);

                    // Try to flush any in-order events from buffer
                    let expected = state.expected_seq.unwrap_or(1);
                    let mut events = Vec::new();

                    while let Some((s, _)) = state.reorder_buffer.front() {
                        if *s == expected + events.len() as u64 {
                            if let Some((_, ev)) = state.reorder_buffer.pop_front() {
                                self.counters.reordered_events += 1;
                                self.counters.total_events_forwarded += 1;
                                events.push(ev);
                            }
                        } else {
                            break;
                        }
                    }

                    if events.is_empty() {
                        return IntegrityResult::Dropped(DropReason::OutOfOrder {
                            expected_seq: expected,
                            actual_seq: 0, // Buffered, not dropped
                        });
                    } else {
                        state.expected_seq = Some(expected + events.len() as u64);
                        if events.len() == 1 {
                            return IntegrityResult::Forward(events.pop().unwrap());
                        } else {
                            return IntegrityResult::Reordered(events);
                        }
                    }
                }
            }
        }

        // In-order event (seq == expected)
        state.last_seq = Some(seq);
        state.expected_seq = Some(seq + 1);
        state.last_timestamp = event.time;
        add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);
        self.counters.total_events_forwarded += 1;

        // Check if reorder buffer can be flushed
        if !state.reorder_buffer.is_empty() {
            let mut events = vec![event];
            let expected = state.expected_seq.unwrap_or(1);

            while let Some((s, _)) = state.reorder_buffer.front() {
                if *s == expected + events.len() as u64 - 1 {
                    if let Some((_, ev)) = state.reorder_buffer.pop_front() {
                        self.counters.reordered_events += 1;
                        events.push(ev);
                    }
                } else {
                    break;
                }
            }

            if let Some(last_ev) = events.last() {
                if let Event::L2Delta { exchange_seq, .. } = &last_ev.event {
                    state.expected_seq = Some(*exchange_seq + 1);
                }
            }

            self.counters.total_events_forwarded += events.len() as u64 - 1; // Already counted first

            if events.len() == 1 {
                return IntegrityResult::Forward(events.pop().unwrap());
            } else {
                return IntegrityResult::Reordered(events);
            }
        }

        IntegrityResult::Forward(event)
    }

    fn process_unsequenced_event_internal(
        &mut self,
        token_id: TokenId,
        event_hash: u64,
        event: TimestampedEvent,
    ) -> IntegrityResult {
        let state = self.token_states.get_mut(&token_id).unwrap();
        
        // Check timestamp monotonicity
        if event.time < state.last_timestamp {
            let regression = state.last_timestamp - event.time;

            if regression > self.policy.timestamp_jitter_tolerance_ns {
                self.counters.out_of_order_detected += 1;

                match self.policy.on_out_of_order {
                    OutOfOrderPolicy::Drop => {
                        self.counters.out_of_order_dropped += 1;
                        return IntegrityResult::Dropped(DropReason::TimestampRegression {
                            expected_min: state.last_timestamp,
                            actual: event.time,
                        });
                    }
                    OutOfOrderPolicy::Halt => {
                        state.sync_state = SyncState::Halted;
                        self.counters.halted = true;
                        self.counters.halt_reason = Some(format!(
                            "Timestamp regression for {}: expected >= {}, got {}",
                            token_id, state.last_timestamp, event.time
                        ));
                        return IntegrityResult::Halted(HaltReason {
                            token_id,
                            reason: HaltType::TimestampRegression {
                                expected_min: state.last_timestamp,
                                actual: event.time,
                            },
                            context: format!(
                                "Regression of {}ns exceeds tolerance {}ns",
                                regression, self.policy.timestamp_jitter_tolerance_ns
                            ),
                        });
                    }
                    OutOfOrderPolicy::Reorder => {
                        // For unsequenced events, we can't reorder - just accept with warning
                        warn!(
                            token_id = %token_id,
                            expected_min = state.last_timestamp,
                            actual = event.time,
                            "Timestamp regression within tolerance"
                        );
                    }
                }
            }
        } else {
            state.last_timestamp = event.time;
        }

        // Add to dedupe set and forward
        add_to_dedupe_set(state, event_hash, self.max_dedupe_set_size);
        self.counters.total_events_forwarded += 1;
        IntegrityResult::Forward(event)
    }
    
    fn add_to_dedupe_set_for_token(&mut self, token_id: &str, hash: u64) {
        if let Some(state) = self.token_states.get_mut(token_id) {
            add_to_dedupe_set(state, hash, self.max_dedupe_set_size);
        }
    }
}

/// Helper to add hash to dedupe set (free function to avoid borrow issues).
fn add_to_dedupe_set(state: &mut TokenState, hash: u64, max_size: usize) {
    // Prune if too large (LRU-style: just clear half)
    if state.seen_hashes.len() >= max_size {
        let to_remove: Vec<u64> = state.seen_hashes
            .iter()
            .take(max_size / 2)
            .copied()
            .collect();
        for h in to_remove {
            state.seen_hashes.remove(&h);
        }
    }
    state.seen_hashes.insert(hash);
}

// =============================================================================
// HASH FUNCTIONS
// =============================================================================

/// Compute a hash for deduplication.
fn compute_event_hash(event: &TimestampedEvent) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();
    event.source.hash(&mut hasher);
    event.source_time.hash(&mut hasher);

    match &event.event {
        Event::L2BookSnapshot { token_id, exchange_seq, .. } => {
            "L2BookSnapshot".hash(&mut hasher);
            token_id.hash(&mut hasher);
            exchange_seq.hash(&mut hasher);
        }
        Event::L2Delta { token_id, exchange_seq, .. } => {
            "L2Delta".hash(&mut hasher);
            token_id.hash(&mut hasher);
            exchange_seq.hash(&mut hasher);
        }
        Event::TradePrint { token_id, price, size, trade_id, .. } => {
            "TradePrint".hash(&mut hasher);
            token_id.hash(&mut hasher);
            price.to_bits().hash(&mut hasher);
            size.to_bits().hash(&mut hasher);
            trade_id.hash(&mut hasher);
        }
        Event::Fill { order_id, fill_id, .. } => {
            "Fill".hash(&mut hasher);
            order_id.hash(&mut hasher);
            fill_id.hash(&mut hasher);
        }
        Event::OrderAck { order_id, .. } => {
            "OrderAck".hash(&mut hasher);
            order_id.hash(&mut hasher);
        }
        _ => {
            // For other events, use time + source as identity
            event.time.hash(&mut hasher);
        }
    }

    hasher.finish()
}

/// Hash a string to u64.
fn hash_string(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::events::Level;

    fn make_delta(token_id: &str, seq: u64, time: Nanos) -> TimestampedEvent {
        TimestampedEvent {
            time,
            source_time: time,
            seq: 0,
            source: 1,
            event: Event::L2Delta {
                token_id: token_id.to_string(),
                bid_updates: vec![Level::new(0.5, 100.0)],
                ask_updates: vec![],
                exchange_seq: seq,
            },
        }
    }

    fn make_snapshot(token_id: &str, seq: u64, time: Nanos) -> TimestampedEvent {
        TimestampedEvent {
            time,
            source_time: time,
            seq: 0,
            source: 1,
            event: Event::L2BookSnapshot {
                token_id: token_id.to_string(),
                bids: vec![Level::new(0.5, 100.0)],
                asks: vec![Level::new(0.55, 100.0)],
                exchange_seq: seq,
            },
        }
    }

    #[test]
    fn test_normal_sequence() {
        let mut guard = StreamIntegrityGuard::strict();

        for seq in 1..=10 {
            let event = make_delta("token1", seq, seq as Nanos * 1000);
            let result = guard.process(event);
            assert!(matches!(result, IntegrityResult::Forward(_)));
        }

        assert_eq!(guard.counters().total_events_processed, 10);
        assert_eq!(guard.counters().total_events_forwarded, 10);
        assert!(!guard.counters().has_pathologies());
    }

    #[test]
    fn test_duplicate_detection() {
        let mut guard = StreamIntegrityGuard::strict();

        let event1 = make_delta("token1", 1, 1000);
        let event2 = make_delta("token1", 1, 1000); // Same seq and time

        let r1 = guard.process(event1);
        assert!(matches!(r1, IntegrityResult::Forward(_)));

        let r2 = guard.process(event2);
        assert!(matches!(r2, IntegrityResult::Dropped(DropReason::Duplicate { .. })));

        assert_eq!(guard.counters().duplicates_dropped, 1);
    }

    #[test]
    fn test_gap_detection_strict() {
        let mut guard = StreamIntegrityGuard::strict();

        let event1 = make_delta("token1", 1, 1000);
        let event2 = make_delta("token1", 5, 2000); // Gap of 3

        let r1 = guard.process(event1);
        assert!(matches!(r1, IntegrityResult::Forward(_)));

        let r2 = guard.process(event2);
        assert!(matches!(r2, IntegrityResult::Halted(_)));

        assert!(guard.counters().halted);
        assert_eq!(guard.counters().gaps_detected, 1);
    }

    #[test]
    fn test_gap_detection_resilient() {
        let mut guard = StreamIntegrityGuard::resilient();

        let event1 = make_delta("token1", 1, 1000);
        let event2 = make_delta("token1", 5, 2000); // Gap of 3 (within tolerance of 10)

        let r1 = guard.process(event1);
        assert!(matches!(r1, IntegrityResult::Forward(_)));

        let r2 = guard.process(event2);
        // Within tolerance, should forward with warning
        assert!(matches!(r2, IntegrityResult::Forward(_)));

        assert_eq!(guard.counters().gaps_detected, 1);
        assert!(!guard.counters().halted);
    }

    #[test]
    fn test_out_of_order_strict() {
        let mut guard = StreamIntegrityGuard::strict();

        let event1 = make_delta("token1", 5, 1000);
        let event2 = make_delta("token1", 3, 2000); // Out of order

        let r1 = guard.process(event1);
        assert!(matches!(r1, IntegrityResult::Forward(_)));

        let r2 = guard.process(event2);
        assert!(matches!(r2, IntegrityResult::Halted(_)));
    }

    #[test]
    fn test_snapshot_resync() {
        let mut guard = StreamIntegrityGuard::new(PathologyPolicy {
            on_gap: GapPolicy::Resync,
            ..PathologyPolicy::strict()
        });

        let event1 = make_delta("token1", 1, 1000);
        let event2 = make_delta("token1", 100, 2000); // Large gap

        guard.process(event1);
        let r2 = guard.process(event2);
        assert!(matches!(r2, IntegrityResult::NeedResync { .. }));

        // Events should be dropped while waiting
        let event3 = make_delta("token1", 101, 3000);
        let r3 = guard.process(event3);
        assert!(matches!(r3, IntegrityResult::Dropped(DropReason::AwaitingResync)));

        // Snapshot should resync
        let snapshot = make_snapshot("token1", 200, 4000);
        let r4 = guard.process(snapshot);
        assert!(matches!(r4, IntegrityResult::Forward(_)));

        // Normal operation resumes
        let event5 = make_delta("token1", 201, 5000);
        let r5 = guard.process(event5);
        assert!(matches!(r5, IntegrityResult::Forward(_)));
    }

    #[test]
    fn test_policy_defaults() {
        let strict = PathologyPolicy::strict();
        assert_eq!(strict.gap_tolerance, 0);
        assert!(matches!(strict.on_gap, GapPolicy::Halt));

        let resilient = PathologyPolicy::resilient();
        assert_eq!(resilient.gap_tolerance, 10);
        assert!(matches!(resilient.on_gap, GapPolicy::Resync));
    }
}
