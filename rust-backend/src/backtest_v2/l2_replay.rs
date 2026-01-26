//! L2 Delta Replay Feed and Contract Verifier
//!
//! Provides deterministic replay of L2 snapshots and deltas with:
//! - Strict sequence verification
//! - Snapshot consistency checking
//! - Book state reconstruction
//! - Trust gate integration

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Side, TimestampedEvent};
use crate::backtest_v2::feed::MarketDataFeed;
use crate::backtest_v2::l2_delta::{
    BookError, BookFingerprint, DeterministicBook, GapPolicy, L2DatasetMetadata, 
    L2DeltaContractRequirement, L2DeltaContractResult, PolymarketL2Delta, 
    PolymarketL2Snapshot, SequenceOrigin, SequenceScope,
};
use crate::backtest_v2::l2_storage::L2Storage;
use crate::backtest_v2::queue::StreamSource;
use crate::backtest_v2::strategy::BookSnapshot as StrategyBookSnapshot;
use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, warn};

// =============================================================================
// L2 EVENT UNION
// =============================================================================

/// Union type for replay events (snapshot or delta).
#[derive(Debug, Clone)]
pub enum L2Event {
    Snapshot(PolymarketL2Snapshot),
    Delta(PolymarketL2Delta),
}

impl L2Event {
    /// Get the token ID.
    pub fn token_id(&self) -> &str {
        match self {
            Self::Snapshot(s) => &s.token_id,
            Self::Delta(d) => &d.token_id,
        }
    }

    /// Get the market ID.
    pub fn market_id(&self) -> &str {
        match self {
            Self::Snapshot(s) => &s.market_id,
            Self::Delta(d) => &d.market_id,
        }
    }

    /// Get the ingest timestamp.
    pub fn ingest_ts(&self) -> Nanos {
        match self {
            Self::Snapshot(s) => s.time.ingest_ts,
            Self::Delta(d) => d.time.ingest_ts,
        }
    }

    /// Get the sequence number.
    pub fn seq(&self) -> u64 {
        match self {
            Self::Snapshot(s) => s.seq_snapshot,
            Self::Delta(d) => d.seq,
        }
    }

    /// Convert to TimestampedEvent.
    pub fn to_timestamped_event(&self, tick_size: f64, source: u8) -> TimestampedEvent {
        match self {
            Self::Snapshot(s) => s.to_timestamped_event(tick_size, source),
            Self::Delta(d) => d.to_timestamped_event(tick_size, source),
        }
    }
}

// =============================================================================
// L2 REPLAY FEED
// =============================================================================

/// Replay feed that emits L2 events in deterministic order.
pub struct L2ReplayFeed {
    /// All events sorted by (ingest_ts, seq).
    events: Vec<L2Event>,
    /// Current position in the event stream.
    index: usize,
    /// Tick size for price conversion.
    tick_size: f64,
    /// Stream source ID.
    source: u8,
    /// Feed name.
    name: String,
}

impl L2ReplayFeed {
    /// Create from storage for a specific token.
    pub fn from_storage(
        storage: &L2Storage,
        token_id: &str,
        start_ns: Nanos,
        end_ns: Nanos,
    ) -> Result<Self> {
        let snapshots = storage.load_snapshots(token_id, start_ns, end_ns)?;
        let deltas = storage.load_deltas(token_id, start_ns, end_ns)?;

        let mut events = Vec::with_capacity(snapshots.len() + deltas.len());

        for snapshot in snapshots {
            events.push(L2Event::Snapshot(snapshot));
        }

        for delta in deltas {
            events.push(L2Event::Delta(delta));
        }

        // Sort by (ingest_ts, seq) for deterministic order
        events.sort_by(|a, b| {
            a.ingest_ts()
                .cmp(&b.ingest_ts())
                .then_with(|| a.seq().cmp(&b.seq()))
        });

        Ok(Self {
            events,
            index: 0,
            tick_size: storage.tick_size(),
            source: StreamSource::MarketData as u8,
            name: format!("L2ReplayFeed({})", token_id),
        })
    }

    /// Create from vectors of events (for testing or custom loading).
    pub fn from_events(
        snapshots: Vec<PolymarketL2Snapshot>,
        deltas: Vec<PolymarketL2Delta>,
        tick_size: f64,
    ) -> Self {
        let mut events = Vec::with_capacity(snapshots.len() + deltas.len());

        for snapshot in snapshots {
            events.push(L2Event::Snapshot(snapshot));
        }

        for delta in deltas {
            events.push(L2Event::Delta(delta));
        }

        events.sort_by(|a, b| {
            a.ingest_ts()
                .cmp(&b.ingest_ts())
                .then_with(|| a.seq().cmp(&b.seq()))
        });

        Self {
            events,
            index: 0,
            tick_size,
            source: StreamSource::MarketData as u8,
            name: "L2ReplayFeed".to_string(),
        }
    }

    /// Get total event count.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get remaining event count.
    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.index)
    }

    /// Peek at next event without consuming.
    pub fn peek(&self) -> Option<&L2Event> {
        self.events.get(self.index)
    }

    /// Get next event.
    pub fn next_l2_event(&mut self) -> Option<&L2Event> {
        if self.index < self.events.len() {
            let event = &self.events[self.index];
            self.index += 1;
            Some(event)
        } else {
            None
        }
    }

    /// Reset to beginning.
    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Get all events (for inspection).
    pub fn events(&self) -> &[L2Event] {
        &self.events
    }
}

impl MarketDataFeed for L2ReplayFeed {
    fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.index < self.events.len() {
            let event = &self.events[self.index];
            self.index += 1;
            Some(event.to_timestamped_event(self.tick_size, self.source))
        } else {
            None
        }
    }

    fn peek_time(&self) -> Option<Nanos> {
        self.events.get(self.index).map(|e| e.ingest_ts())
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.events.len().saturating_sub(self.index))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// =============================================================================
// L2 BOOK MANAGER
// =============================================================================

/// Manages multiple DeterministicBook instances for replay.
pub struct L2BookManager {
    /// Books by token_id.
    books: HashMap<String, DeterministicBook>,
    /// Default market ID (for new books).
    default_market_id: String,
    /// Tick size.
    tick_size: f64,
    /// Sequence scope.
    seq_scope: SequenceScope,
    /// Gap policy.
    gap_policy: GapPolicy,
    /// Total events processed.
    events_processed: u64,
    /// Errors encountered.
    errors: Vec<(u64, String)>,
}

impl L2BookManager {
    /// Create a new book manager.
    pub fn new(
        default_market_id: String,
        tick_size: f64,
        seq_scope: SequenceScope,
        gap_policy: GapPolicy,
    ) -> Self {
        Self {
            books: HashMap::new(),
            default_market_id,
            tick_size,
            seq_scope,
            gap_policy,
            events_processed: 0,
            errors: Vec::new(),
        }
    }

    /// Get or create a book for a token.
    pub fn get_or_create(&mut self, token_id: &str, market_id: &str) -> &mut DeterministicBook {
        if !self.books.contains_key(token_id) {
            let book = DeterministicBook::new(
                market_id.to_string(),
                token_id.to_string(),
                self.tick_size,
                self.seq_scope,
                self.gap_policy,
            );
            self.books.insert(token_id.to_string(), book);
        }
        self.books.get_mut(token_id).unwrap()
    }

    /// Get a book (read-only).
    pub fn get(&self, token_id: &str) -> Option<&DeterministicBook> {
        self.books.get(token_id)
    }

    /// Process an L2 event.
    pub fn process_event(&mut self, event: &L2Event) -> Result<(), BookError> {
        self.events_processed += 1;

        match event {
            L2Event::Snapshot(snapshot) => {
                let book = self.get_or_create(&snapshot.token_id, &snapshot.market_id);
                book.apply_snapshot(snapshot)?;
            }
            L2Event::Delta(delta) => {
                let book = self.get_or_create(&delta.token_id, &delta.market_id);
                book.apply_delta(delta)?;
            }
        }

        Ok(())
    }

    /// Process event and log errors instead of propagating.
    pub fn process_event_logged(&mut self, event: &L2Event) -> bool {
        if let Err(e) = self.process_event(event) {
            self.errors.push((self.events_processed, e.to_string()));
            false
        } else {
            true
        }
    }

    /// Get book state as StrategyBookSnapshot (for strategy consumption).
    pub fn get_book_snapshot(&self, token_id: &str, timestamp: Nanos) -> Option<StrategyBookSnapshot> {
        let book = self.books.get(token_id)?;
        
        let bids = book.top_bids(20);
        let asks = book.top_asks(20);
        
        // Get current sequence
        let scope_key = book.market_id.clone();
        let exchange_seq = book.current_seq(&scope_key).unwrap_or(0);
        
        Some(StrategyBookSnapshot {
            token_id: token_id.to_string(),
            bids,
            asks,
            timestamp,
            exchange_seq,
        })
    }

    /// Get fingerprint for a token.
    pub fn get_fingerprint(&self, token_id: &str) -> Option<BookFingerprint> {
        self.books.get(token_id).map(|b| b.fingerprint())
    }

    /// Get all token IDs.
    pub fn token_ids(&self) -> Vec<String> {
        self.books.keys().cloned().collect()
    }

    /// Get total events processed.
    pub fn events_processed(&self) -> u64 {
        self.events_processed
    }

    /// Get errors.
    pub fn errors(&self) -> &[(u64, String)] {
        &self.errors
    }

    /// Check if any errors occurred.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

// =============================================================================
// L2 CONTRACT VERIFIER
// =============================================================================

/// Verifies L2 delta contract requirements during replay.
pub struct L2ContractVerifier {
    /// Sequence scope.
    seq_scope: SequenceScope,
    /// Sequence origin.
    seq_origin: SequenceOrigin,
    /// Tick size.
    tick_size: f64,
    /// Per-token tracking: has initial snapshot.
    has_initial_snapshot: HashMap<String, bool>,
    /// Per-scope tracking: last sequence.
    last_seq: HashMap<String, u64>,
    /// Gaps detected.
    gaps: Vec<(String, u64, u64)>,
    /// Checkpoint fingerprints.
    checkpoint_fingerprints: Vec<BookFingerprint>,
    /// Checkpoint interval (in events).
    checkpoint_interval: u64,
    /// Events processed.
    events_processed: u64,
    /// Book manager for state reconstruction.
    book_manager: L2BookManager,
    /// Errors encountered.
    errors: Vec<String>,
    /// Warnings.
    warnings: Vec<String>,
}

impl L2ContractVerifier {
    /// Create a new verifier.
    pub fn new(seq_scope: SequenceScope, seq_origin: SequenceOrigin, tick_size: f64) -> Self {
        let book_manager = L2BookManager::new(
            "unknown".to_string(),
            tick_size,
            seq_scope,
            GapPolicy::WarnAndContinue, // Continue to collect all issues
        );

        Self {
            seq_scope,
            seq_origin,
            tick_size,
            has_initial_snapshot: HashMap::new(),
            last_seq: HashMap::new(),
            gaps: Vec::new(),
            checkpoint_fingerprints: Vec::new(),
            checkpoint_interval: 10000, // Checkpoint every 10k events
            events_processed: 0,
            book_manager,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Set checkpoint interval.
    pub fn with_checkpoint_interval(mut self, interval: u64) -> Self {
        self.checkpoint_interval = interval;
        self
    }

    /// Process an event and verify contract requirements.
    pub fn process_event(&mut self, event: &L2Event) {
        self.events_processed += 1;

        match event {
            L2Event::Snapshot(snapshot) => {
                self.process_snapshot(snapshot);
            }
            L2Event::Delta(delta) => {
                self.process_delta(delta);
            }
        }

        // Checkpoint fingerprints
        if self.events_processed % self.checkpoint_interval == 0 {
            for token_id in self.book_manager.token_ids() {
                if let Some(fp) = self.book_manager.get_fingerprint(&token_id) {
                    self.checkpoint_fingerprints.push(fp);
                }
            }
        }
    }

    fn process_snapshot(&mut self, snapshot: &PolymarketL2Snapshot) {
        let token_id = &snapshot.token_id;

        // Track initial snapshot
        if !self.has_initial_snapshot.contains_key(token_id) {
            self.has_initial_snapshot.insert(token_id.clone(), true);
        }

        // Update sequence tracking
        let scope_key = self.seq_scope.scope_key(&snapshot.market_id, Side::Buy);
        self.last_seq.insert(scope_key.clone(), snapshot.seq_snapshot);
        
        if self.seq_scope == SequenceScope::PerMarketSide {
            let ask_scope = self.seq_scope.scope_key(&snapshot.market_id, Side::Sell);
            self.last_seq.insert(ask_scope, snapshot.seq_snapshot);
        }

        // Apply to book manager
        if let Err(e) = self.book_manager.process_event(&L2Event::Snapshot(snapshot.clone())) {
            self.errors.push(format!("Snapshot error at seq {}: {}", snapshot.seq_snapshot, e));
        }
    }

    fn process_delta(&mut self, delta: &PolymarketL2Delta) {
        let token_id = &delta.token_id;

        // Check for initial snapshot
        if !self.has_initial_snapshot.get(token_id).copied().unwrap_or(false) {
            self.errors.push(format!(
                "Delta at seq {} for {} without initial snapshot",
                delta.seq, token_id
            ));
            self.has_initial_snapshot.insert(token_id.clone(), false);
        }

        // Check sequence
        let scope_key = self.seq_scope.scope_key(&delta.market_id, delta.side);
        if let Some(&last) = self.last_seq.get(&scope_key) {
            if delta.seq <= last {
                self.errors.push(format!(
                    "Non-monotone sequence in {}: expected > {}, got {}",
                    scope_key, last, delta.seq
                ));
            } else if delta.seq > last + 1 {
                let gap_size = delta.seq - last - 1;
                self.gaps.push((scope_key.clone(), last + 1, delta.seq - 1));
                self.warnings.push(format!(
                    "Sequence gap in {}: {} missing ({}..{})",
                    scope_key, gap_size, last + 1, delta.seq - 1
                ));
            }
        }
        self.last_seq.insert(scope_key, delta.seq);

        // Apply to book manager
        if let Err(e) = self.book_manager.process_event(&L2Event::Delta(delta.clone())) {
            self.errors.push(format!("Delta error at seq {}: {}", delta.seq, e));
        }
    }

    /// Verify against stored fingerprints.
    pub fn verify_fingerprints(&mut self, expected: &[BookFingerprint]) {
        for expected_fp in expected {
            // Find matching checkpoint
            let actual_fp = self.checkpoint_fingerprints
                .iter()
                .find(|fp| fp.seq == expected_fp.seq);

            if let Some(actual) = actual_fp {
                if actual.hash != expected_fp.hash {
                    self.errors.push(format!(
                        "Fingerprint mismatch at seq {}: expected {:016x}, got {:016x}",
                        expected_fp.seq, expected_fp.hash, actual.hash
                    ));
                }
            } else {
                self.warnings.push(format!(
                    "No checkpoint fingerprint found for seq {}",
                    expected_fp.seq
                ));
            }
        }
    }

    /// Build the contract verification result.
    pub fn build_result(&self) -> L2DeltaContractResult {
        let mut result = L2DeltaContractResult::new();

        // Check InitialSnapshot
        let all_have_initial = self.has_initial_snapshot.values().all(|&v| v);
        if all_have_initial && !self.has_initial_snapshot.is_empty() {
            result.pass(L2DeltaContractRequirement::InitialSnapshot);
        } else {
            result.fail(
                L2DeltaContractRequirement::InitialSnapshot,
                format!("{} tokens missing initial snapshot", 
                    self.has_initial_snapshot.values().filter(|&&v| !v).count()),
            );
        }

        // Check MonotoneSeq
        let monotone_errors = self.errors.iter()
            .filter(|e| e.contains("Non-monotone"))
            .count();
        if monotone_errors == 0 {
            result.pass(L2DeltaContractRequirement::MonotoneSeq);
        } else {
            result.fail(
                L2DeltaContractRequirement::MonotoneSeq,
                format!("{} non-monotone sequence violations", monotone_errors),
            );
        }

        // Check NoSeqGaps
        if self.gaps.is_empty() {
            result.pass(L2DeltaContractRequirement::NoSeqGaps);
        } else {
            result.fail(
                L2DeltaContractRequirement::NoSeqGaps,
                format!("{} sequence gaps detected", self.gaps.len()),
            );
        }

        // Check NoNegativeSizes
        let negative_errors = self.errors.iter()
            .filter(|e| e.contains("Negative size"))
            .count();
        if negative_errors == 0 {
            result.pass(L2DeltaContractRequirement::NoNegativeSizes);
        } else {
            result.fail(
                L2DeltaContractRequirement::NoNegativeSizes,
                format!("{} negative size violations", negative_errors),
            );
        }

        // Check TickOrdering (via crossed book detection)
        let crossed_errors = self.errors.iter()
            .filter(|e| e.contains("Crossed book"))
            .count();
        if crossed_errors == 0 {
            result.pass(L2DeltaContractRequirement::TickOrdering);
        } else {
            result.fail(
                L2DeltaContractRequirement::TickOrdering,
                format!("{} crossed book violations", crossed_errors),
            );
        }

        // Check ExchangeSequence
        if self.seq_origin.is_production_grade() {
            result.pass(L2DeltaContractRequirement::ExchangeSequence);
        } else {
            result.fail(
                L2DeltaContractRequirement::ExchangeSequence,
                format!("Sequence origin {:?} is not production-grade", self.seq_origin),
            );
        }

        // Add warnings
        for warning in &self.warnings {
            result.warn(warning.clone());
        }

        // Add fingerprints
        for fp in &self.checkpoint_fingerprints {
            result.add_fingerprint(*fp);
        }

        result
    }

    /// Get the book manager (for accessing final book state).
    pub fn book_manager(&self) -> &L2BookManager {
        &self.book_manager
    }

    /// Get events processed.
    pub fn events_processed(&self) -> u64 {
        self.events_processed
    }
}

// =============================================================================
// TRUST GATE INTEGRATION
// =============================================================================

/// Extension for trust gate to check L2 delta contract.
pub trait L2TrustGateExt {
    /// Check if L2 delta contract is satisfied for production-grade.
    fn check_l2_contract(&self, contract_result: &L2DeltaContractResult) -> Vec<String>;
}

impl L2TrustGateExt for L2DeltaContractResult {
    fn check_l2_contract(&self, contract_result: &L2DeltaContractResult) -> Vec<String> {
        let mut failures = Vec::new();

        if !contract_result.satisfied {
            for (req, reason) in &contract_result.failed {
                failures.push(format!("L2 contract requirement {:?} failed: {}", req, reason));
            }
        }

        failures
    }
}

/// Classify dataset based on L2 delta contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2DatasetClassification {
    /// Full incremental L2 with exchange sequences - production grade.
    FullIncrementalExchangeSeq,
    /// Full incremental L2 with synthetic sequences - not production grade.
    FullIncrementalSyntheticSeq,
    /// Snapshot only (no deltas) - taker only.
    SnapshotOnly,
    /// Incomplete or invalid data.
    Incomplete,
}

impl L2DatasetClassification {
    /// Classify from metadata.
    pub fn from_metadata(metadata: &L2DatasetMetadata) -> Self {
        if metadata.delta_count == 0 {
            if metadata.snapshot_count > 0 {
                return Self::SnapshotOnly;
            } else {
                return Self::Incomplete;
            }
        }

        if !metadata.has_initial_snapshots {
            return Self::Incomplete;
        }

        if !metadata.sequence_gaps.is_empty() {
            return Self::Incomplete;
        }

        match metadata.seq_origin {
            SequenceOrigin::Exchange | SequenceOrigin::DerivedFromHash => {
                Self::FullIncrementalExchangeSeq
            }
            SequenceOrigin::SyntheticFromArrival => Self::FullIncrementalSyntheticSeq,
            SequenceOrigin::None => Self::Incomplete,
        }
    }

    /// Classify from contract result.
    pub fn from_contract_result(result: &L2DeltaContractResult, seq_origin: SequenceOrigin) -> Self {
        if !result.satisfied {
            // Check if it's just missing exchange sequences
            let only_seq_origin_failed = result.failed.len() == 1
                && result.failed.iter().any(|(req, _)| {
                    matches!(req, L2DeltaContractRequirement::ExchangeSequence)
                });

            if only_seq_origin_failed {
                return Self::FullIncrementalSyntheticSeq;
            }

            return Self::Incomplete;
        }

        Self::FullIncrementalExchangeSeq
    }

    /// Check if production-grade.
    pub fn is_production_grade(&self) -> bool {
        matches!(self, Self::FullIncrementalExchangeSeq)
    }

    /// Check if maker strategies are supported.
    pub fn supports_maker(&self) -> bool {
        matches!(
            self,
            Self::FullIncrementalExchangeSeq | Self::FullIncrementalSyntheticSeq
        )
    }

    /// Check if taker strategies are supported.
    pub fn supports_taker(&self) -> bool {
        !matches!(self, Self::Incomplete)
    }

    /// Get description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::FullIncrementalExchangeSeq => {
                "Full incremental L2 with exchange sequences (production-grade)"
            }
            Self::FullIncrementalSyntheticSeq => {
                "Full incremental L2 with synthetic sequences (not production-grade)"
            }
            Self::SnapshotOnly => "Periodic snapshots only (taker-only)",
            Self::Incomplete => "Incomplete or invalid data",
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_v2::l2_delta::{EventTime, TickPriceLevel, POLYMARKET_TICK_SIZE};

    fn make_snapshot(seq: u64, ingest_ts: Nanos) -> PolymarketL2Snapshot {
        PolymarketL2Snapshot {
            market_id: "market1".to_string(),
            token_id: "token1".to_string(),
            seq_snapshot: seq,
            bids: vec![TickPriceLevel { price_ticks: 4500, size_fp: 1000_00000000 }],
            asks: vec![TickPriceLevel { price_ticks: 5500, size_fp: 1500_00000000 }],
            time: EventTime::ingest_only(ingest_ts),
            total_bid_depth_fp: 1000_00000000,
            total_ask_depth_fp: 1500_00000000,
        }
    }

    fn make_delta(seq: u64, side: Side, price_ticks: i64, size_fp: i64, ingest_ts: Nanos) -> PolymarketL2Delta {
        PolymarketL2Delta::absolute(
            "market1".to_string(),
            "token1".to_string(),
            side,
            price_ticks,
            size_fp,
            seq,
            EventTime::ingest_only(ingest_ts),
            None,
        )
    }

    #[test]
    fn test_replay_feed_ordering() {
        let snapshots = vec![make_snapshot(1, 1000000000)];
        let deltas = vec![
            make_delta(3, Side::Buy, 4600, 500_00000000, 1000002000),
            make_delta(2, Side::Buy, 4500, 1100_00000000, 1000001000),
        ];

        let mut feed = L2ReplayFeed::from_events(snapshots, deltas, POLYMARKET_TICK_SIZE);

        // First event should be snapshot (earliest)
        let e1 = feed.next_l2_event().unwrap();
        assert!(matches!(e1, L2Event::Snapshot(_)));

        // Second should be delta seq 2
        let e2 = feed.next_l2_event().unwrap();
        assert_eq!(e2.seq(), 2);

        // Third should be delta seq 3
        let e3 = feed.next_l2_event().unwrap();
        assert_eq!(e3.seq(), 3);
    }

    #[test]
    fn test_book_manager_state() {
        let mut manager = L2BookManager::new(
            "market1".to_string(),
            POLYMARKET_TICK_SIZE,
            SequenceScope::PerMarket,
            GapPolicy::WarnAndContinue,
        );

        let snapshot = make_snapshot(1, 1000000000);
        manager.process_event(&L2Event::Snapshot(snapshot)).unwrap();

        let delta = make_delta(2, Side::Buy, 4600, 500_00000000, 1000001000);
        manager.process_event(&L2Event::Delta(delta)).unwrap();

        let book = manager.get("token1").unwrap();
        assert_eq!(book.best_bid_ticks(), Some(4600)); // New bid is better
        assert_eq!(book.delta_count(), 1);
    }

    #[test]
    fn test_contract_verifier_happy_path() {
        let mut verifier = L2ContractVerifier::new(
            SequenceScope::PerMarket,
            SequenceOrigin::Exchange,
            POLYMARKET_TICK_SIZE,
        );

        // Process snapshot
        verifier.process_event(&L2Event::Snapshot(make_snapshot(1, 1000000000)));

        // Process sequential deltas
        for seq in 2..=10 {
            let delta = make_delta(seq, Side::Buy, 4500 + seq as i64, 100_00000000, 1000000000i64 + seq as i64 * 1000);
            verifier.process_event(&L2Event::Delta(delta));
        }

        let result = verifier.build_result();
        assert!(result.satisfied);
        assert!(result.requirement_passed(L2DeltaContractRequirement::InitialSnapshot));
        assert!(result.requirement_passed(L2DeltaContractRequirement::MonotoneSeq));
        assert!(result.requirement_passed(L2DeltaContractRequirement::NoSeqGaps));
    }

    #[test]
    fn test_contract_verifier_missing_snapshot() {
        let mut verifier = L2ContractVerifier::new(
            SequenceScope::PerMarket,
            SequenceOrigin::Exchange,
            POLYMARKET_TICK_SIZE,
        );

        // Process delta without snapshot
        let delta = make_delta(1, Side::Buy, 4500, 100_00000000, 1000000000);
        verifier.process_event(&L2Event::Delta(delta));

        let result = verifier.build_result();
        assert!(!result.satisfied);
        assert!(!result.requirement_passed(L2DeltaContractRequirement::InitialSnapshot));
    }

    #[test]
    fn test_contract_verifier_sequence_gap() {
        let mut verifier = L2ContractVerifier::new(
            SequenceScope::PerMarket,
            SequenceOrigin::Exchange,
            POLYMARKET_TICK_SIZE,
        );

        verifier.process_event(&L2Event::Snapshot(make_snapshot(1, 1000000000)));
        verifier.process_event(&L2Event::Delta(make_delta(2, Side::Buy, 4500, 100_00000000, 1000001000)));
        // Skip seq 3-4
        verifier.process_event(&L2Event::Delta(make_delta(5, Side::Buy, 4600, 200_00000000, 1000002000)));

        let result = verifier.build_result();
        assert!(!result.satisfied);
        assert!(!result.requirement_passed(L2DeltaContractRequirement::NoSeqGaps));
    }

    #[test]
    fn test_contract_verifier_synthetic_sequences() {
        let mut verifier = L2ContractVerifier::new(
            SequenceScope::PerMarket,
            SequenceOrigin::SyntheticFromArrival, // Not production-grade
            POLYMARKET_TICK_SIZE,
        );

        verifier.process_event(&L2Event::Snapshot(make_snapshot(1, 1000000000)));
        verifier.process_event(&L2Event::Delta(make_delta(2, Side::Buy, 4500, 100_00000000, 1000001000)));

        let result = verifier.build_result();
        assert!(!result.satisfied);
        assert!(result.requirement_passed(L2DeltaContractRequirement::InitialSnapshot));
        assert!(result.requirement_passed(L2DeltaContractRequirement::MonotoneSeq));
        assert!(!result.requirement_passed(L2DeltaContractRequirement::ExchangeSequence));
    }

    #[test]
    fn test_dataset_classification() {
        // Full incremental with exchange sequences
        let meta1 = L2DatasetMetadata {
            version: "L2_V1".to_string(),
            market_id: "market1".to_string(),
            token_ids: vec!["token1".to_string()],
            tick_size: POLYMARKET_TICK_SIZE,
            seq_scope: SequenceScope::PerMarket,
            seq_origin: SequenceOrigin::Exchange,
            time_range_ns: (0, 1000000000),
            snapshot_count: 1,
            delta_count: 100,
            has_initial_snapshots: true,
            sequence_gaps: vec![],
            checkpoint_fingerprints: vec![],
            recorded_at: 0,
            warnings: vec![],
        };
        assert_eq!(
            L2DatasetClassification::from_metadata(&meta1),
            L2DatasetClassification::FullIncrementalExchangeSeq
        );

        // Synthetic sequences
        let mut meta2 = meta1.clone();
        meta2.seq_origin = SequenceOrigin::SyntheticFromArrival;
        assert_eq!(
            L2DatasetClassification::from_metadata(&meta2),
            L2DatasetClassification::FullIncrementalSyntheticSeq
        );

        // Snapshot only
        let mut meta3 = meta1.clone();
        meta3.delta_count = 0;
        assert_eq!(
            L2DatasetClassification::from_metadata(&meta3),
            L2DatasetClassification::SnapshotOnly
        );

        // Missing initial snapshots
        let mut meta4 = meta1.clone();
        meta4.has_initial_snapshots = false;
        assert_eq!(
            L2DatasetClassification::from_metadata(&meta4),
            L2DatasetClassification::Incomplete
        );
    }
}
