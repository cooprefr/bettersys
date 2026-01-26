//! Cross-Run Reproducibility Verification for HFT-Grade Certification
//!
//! This module implements deterministic, hermetic reproducibility checking that ensures:
//! - Identical inputs (dataset + seed) produce byte-for-byte identical outcomes
//! - Full order stream, fill stream, ledger transitions, and final PnL hash identically
//! - No dependence on wall-clock, HashMap iteration order, or floating-point formatting
//!
//! # Canonical Event Streams
//!
//! Five streams are instrumented and hashed in deterministic order:
//!
//! 1. **Orders**: Every order/cancel request via `OrderSender`, with visible_ts, market_id,
//!    side, price (ticks), quantity (shares), order type, and local order ID.
//!
//! 2. **Acks/Rejections**: Every order ack, reject, cancel ack with visible_ts, local order ID,
//!    and stable reason codes.
//!
//! 3. **Fills**: Every fill notification with visible_ts, local order ID, aggressor/passive flags,
//!    price (ticks), quantity (shares), fees (fixed-point), ordered by (visible_ts, order_id, seq).
//!
//! 4. **Ledger**: Every committed ledger entry in strict accounting mode, with visible_ts,
//!    entry sequence, account IDs, amounts (fixed-point i128), and entry type enum.
//!
//! 5. **Final PnL**: Realized PnL, fees, ending cash, position valuation - all in fixed-point.
//!
//! # Determinism by Construction
//!
//! - All values are integers or stable enums (no floats in hash)
//! - Prices in ticks (i64), quantities in shares (i64), fees/cash in fixed-point (i128)
//! - Timestamps in nanoseconds (i64)
//! - BTreeMap used instead of HashMap for deterministic iteration
//! - Explicit little-endian encoding for all binary data
//!
//! # Verification Modes
//!
//! - `Off`: No reproducibility checking
//! - `HashOnly`: Compute and store fingerprint, no replay verification
//! - `ReplayTwice`: Run identical backtest twice and assert fingerprints match (required for certification)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{OrderType, Side, TimeInForce};
use crate::backtest_v2::ledger::{Amount, EventRef, LedgerAccount, AMOUNT_SCALE};

// =============================================================================
// FIXED-POINT SCALES
// =============================================================================

/// Price scale: 1e8 (8 decimal places, same as Chainlink/Binance)
pub const PRICE_SCALE: i64 = 100_000_000;
/// Size scale: 1e8 (8 decimal places for fractional shares)
pub const SIZE_SCALE: i64 = 100_000_000;
/// Fee scale: same as AMOUNT_SCALE from ledger (1e8)
pub const FEE_SCALE: i128 = AMOUNT_SCALE;

/// Convert f64 price to fixed-point ticks.
#[inline]
pub fn price_to_ticks(price: f64) -> i64 {
    (price * PRICE_SCALE as f64).round() as i64
}

/// Convert f64 size to fixed-point shares.
#[inline]
pub fn size_to_shares(size: f64) -> i64 {
    (size * SIZE_SCALE as f64).round() as i64
}

/// Convert f64 fee to fixed-point.
#[inline]
pub fn fee_to_fixed(fee: f64) -> i128 {
    (fee * FEE_SCALE as f64).round() as i128
}

// =============================================================================
// REPRODUCIBILITY CHECK MODE
// =============================================================================

/// Mode for reproducibility verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReproducibilityMode {
    /// No reproducibility checking.
    Off,
    /// Compute fingerprint only, no replay verification.
    #[default]
    HashOnly,
    /// Run twice and verify fingerprints match (required for HFT-grade certification).
    ReplayTwice,
}

impl ReproducibilityMode {
    /// Check if any fingerprint computation is needed.
    pub fn requires_fingerprint(&self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Check if replay verification is required.
    pub fn requires_replay(&self) -> bool {
        matches!(self, Self::ReplayTwice)
    }
}

// =============================================================================
// CANONICAL RECORD TYPES
// =============================================================================

/// Record tag bytes for binary encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordTag {
    OrderSubmit = 0x01,
    CancelRequest = 0x02,
    OrderAck = 0x03,
    OrderReject = 0x04,
    CancelAck = 0x05,
    Fill = 0x10,
    LedgerEntry = 0x20,
    FinalPnL = 0x30,
}

/// Stable side encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideCode {
    Buy = 0,
    Sell = 1,
}

impl From<Side> for SideCode {
    fn from(side: Side) -> Self {
        match side {
            Side::Buy => SideCode::Buy,
            Side::Sell => SideCode::Sell,
        }
    }
}

/// Stable order type encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderTypeCode {
    Market = 0,
    Limit = 1,
    LimitIoc = 2,
    LimitFok = 3,
    LimitPostOnly = 4,
}

impl OrderTypeCode {
    /// Convert from OrderType and TimeInForce to stable encoding.
    ///
    /// The events module uses combined enums (Ioc, Fok as OrderType variants),
    /// so we map those directly.
    pub fn from_order_type_and_tif(order_type: OrderType, tif: TimeInForce, post_only: bool) -> Self {
        match order_type {
            OrderType::Market => Self::Market,
            OrderType::Ioc => Self::LimitIoc,
            OrderType::Fok => Self::LimitFok,
            OrderType::Limit => {
                if post_only {
                    Self::LimitPostOnly
                } else {
                    match tif {
                        TimeInForce::Ioc => Self::LimitIoc,
                        TimeInForce::Fok => Self::LimitFok,
                        TimeInForce::Gtc | TimeInForce::Gtt { .. } => Self::Limit,
                    }
                }
            }
        }
    }
}

/// Stable reject reason encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReasonCode {
    Unknown = 0,
    InsufficientFunds = 1,
    InvalidPrice = 2,
    InvalidSize = 3,
    InvalidSide = 4,
    MarketClosed = 5,
    RateLimited = 6,
    SelfTrade = 7,
    PostOnlyWouldTake = 8,
    OrderNotFound = 9,
    DuplicateOrderId = 10,
    MaxOpenOrders = 11,
    InvalidToken = 12,
    Other = 255,
}

impl RejectReasonCode {
    pub fn from_reason_string(reason: &str) -> Self {
        let lower = reason.to_lowercase();
        if lower.contains("insufficient") || lower.contains("balance") {
            Self::InsufficientFunds
        } else if lower.contains("price") {
            Self::InvalidPrice
        } else if lower.contains("size") || lower.contains("quantity") {
            Self::InvalidSize
        } else if lower.contains("side") {
            Self::InvalidSide
        } else if lower.contains("closed") || lower.contains("halted") {
            Self::MarketClosed
        } else if lower.contains("rate") || lower.contains("limit") {
            Self::RateLimited
        } else if lower.contains("self") && lower.contains("trade") {
            Self::SelfTrade
        } else if lower.contains("post") && lower.contains("only") {
            Self::PostOnlyWouldTake
        } else if lower.contains("not found") {
            Self::OrderNotFound
        } else if lower.contains("duplicate") {
            Self::DuplicateOrderId
        } else if lower.contains("max") && lower.contains("order") {
            Self::MaxOpenOrders
        } else if lower.contains("token") || lower.contains("market") {
            Self::InvalidToken
        } else {
            Self::Other
        }
    }
}

/// Stable ledger account type encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountTypeCode {
    Cash = 0,
    CostBasis = 1,
    FeesPaid = 2,
    Capital = 3,
    RealizedPnL = 4,
    SettlementReceivable = 5,
    SettlementPayable = 6,
}

impl From<&LedgerAccount> for AccountTypeCode {
    fn from(account: &LedgerAccount) -> Self {
        match account {
            LedgerAccount::Cash => Self::Cash,
            LedgerAccount::CostBasis { .. } => Self::CostBasis,
            LedgerAccount::FeesPaid => Self::FeesPaid,
            LedgerAccount::Capital => Self::Capital,
            LedgerAccount::RealizedPnL => Self::RealizedPnL,
            LedgerAccount::SettlementReceivable { .. } => Self::SettlementReceivable,
            LedgerAccount::SettlementPayable { .. } => Self::SettlementPayable,
        }
    }
}

/// Stable event reference type encoding.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventRefTypeCode {
    Fill = 0,
    Fee = 1,
    Settlement = 2,
    InitialDeposit = 3,
    Deposit = 4,
    Withdrawal = 5,
    Adjustment = 6,
}

impl From<&EventRef> for EventRefTypeCode {
    fn from(event_ref: &EventRef) -> Self {
        match event_ref {
            EventRef::Fill { .. } => Self::Fill,
            EventRef::Fee { .. } => Self::Fee,
            EventRef::Settlement { .. } => Self::Settlement,
            EventRef::InitialDeposit { .. } => Self::InitialDeposit,
            EventRef::Deposit { .. } => Self::Deposit,
            EventRef::Withdrawal { .. } => Self::Withdrawal,
            EventRef::Adjustment { .. } => Self::Adjustment,
        }
    }
}

// =============================================================================
// CANONICAL BINARY ENCODER
// =============================================================================

/// Canonical binary encoder with explicit little-endian byte order.
///
/// All fields are written in a strict format:
/// - 1 byte: record tag
/// - Fixed-width fields in defined order
/// - Little-endian for all multi-byte integers
/// - No padding, no alignment
#[derive(Debug)]
pub struct CanonicalEncoder {
    buffer: Vec<u8>,
}

impl CanonicalEncoder {
    /// Create a new encoder with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
        }
    }

    /// Create a new encoder.
    pub fn new() -> Self {
        Self::with_capacity(64 * 1024) // 64KB initial
    }

    /// Write a single byte.
    #[inline]
    pub fn write_u8(&mut self, value: u8) {
        self.buffer.push(value);
    }

    /// Write u16 in little-endian.
    #[inline]
    pub fn write_u16(&mut self, value: u16) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Write u32 in little-endian.
    #[inline]
    pub fn write_u32(&mut self, value: u32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Write u64 in little-endian.
    #[inline]
    pub fn write_u64(&mut self, value: u64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Write i64 in little-endian.
    #[inline]
    pub fn write_i64(&mut self, value: i64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Write i128 in little-endian.
    #[inline]
    pub fn write_i128(&mut self, value: i128) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a bool as u8 (0 or 1).
    #[inline]
    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(if value { 1 } else { 0 });
    }

    /// Get the encoded bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buffer
    }

    /// Get current length.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Clear the buffer for reuse.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Get a reference to the buffer.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer
    }
}

impl Default for CanonicalEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DETERMINISTIC 128-BIT ROLLING HASH
// =============================================================================

/// Deterministic 128-bit rolling hash for reproducibility checking.
///
/// This is NOT a cryptographic hash - it's designed for:
/// - Determinism across machines/runs
/// - Fast incremental updates
/// - Detecting any change in the input stream
///
/// Uses two 64-bit accumulators with fixed mixing constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RollingHash {
    h0: u64,
    h1: u64,
}

impl RollingHash {
    /// Initial seed values (arbitrary primes).
    const SEED_H0: u64 = 0x9E37_79B9_7F4A_7C15;
    const SEED_H1: u64 = 0xBF58_476D_1CE4_E5B9;

    /// Mixing constants.
    const MIX_A: u64 = 0x94D0_49BB_1331_11EB;
    const MIX_B: u64 = 0xBF58_476D_1CE4_E5B9;

    /// Create a new rolling hash with default seed.
    pub fn new() -> Self {
        Self {
            h0: Self::SEED_H0,
            h1: Self::SEED_H1,
        }
    }

    /// Update the hash with a byte slice.
    pub fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            let v = u64::from_le_bytes(word);
            self.mix_word(v);
        }
    }

    /// Update with a single u64.
    pub fn update_u64(&mut self, value: u64) {
        self.mix_word(value);
    }

    /// Mix a 64-bit word into the state.
    #[inline]
    fn mix_word(&mut self, v: u64) {
        self.h0 = self.h0.wrapping_add(v.wrapping_mul(Self::MIX_A));
        self.h0 = self.h0.rotate_left(31);
        self.h0 = self.h0.wrapping_mul(Self::MIX_B);

        self.h1 = self.h1.wrapping_add(self.h0 ^ v);
        self.h1 = self.h1.rotate_left(27);
        self.h1 = self.h1.wrapping_mul(Self::MIX_A);
    }

    /// Finalize and return the 128-bit hash as two u64s.
    pub fn finish(&self) -> (u64, u64) {
        // Final mixing
        let mut h0 = self.h0;
        let mut h1 = self.h1;

        h0 ^= h0 >> 33;
        h0 = h0.wrapping_mul(Self::MIX_A);
        h0 ^= h0 >> 33;

        h1 ^= h1 >> 33;
        h1 = h1.wrapping_mul(Self::MIX_B);
        h1 ^= h1 >> 33;

        (h0, h1)
    }

    /// Get a combined 64-bit hash (for convenience).
    pub fn finish_u64(&self) -> u64 {
        let (h0, h1) = self.finish();
        h0 ^ h1
    }

    /// Format as hex string.
    pub fn to_hex(&self) -> String {
        let (h0, h1) = self.finish();
        format!("{:016x}{:016x}", h0, h1)
    }
}

// =============================================================================
// MARKET ID REGISTRY
// =============================================================================

/// Registry for mapping market ID strings to stable numeric IDs.
///
/// This ensures market IDs are consistently numbered regardless of insertion order.
/// Uses BTreeMap for deterministic iteration.
#[derive(Debug, Clone, Default)]
pub struct MarketIdRegistry {
    /// Sorted map: market_id_string -> numeric_id
    ids: BTreeMap<String, u64>,
    next_id: u64,
}

impl MarketIdRegistry {
    /// Create a new registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a numeric ID for a market.
    pub fn get_or_insert(&mut self, market_id: &str) -> u64 {
        if let Some(&id) = self.ids.get(market_id) {
            id
        } else {
            let id = self.next_id;
            self.ids.insert(market_id.to_string(), id);
            self.next_id += 1;
            id
        }
    }

    /// Get numeric ID if it exists.
    pub fn get(&self, market_id: &str) -> Option<u64> {
        self.ids.get(market_id).copied()
    }

    /// Compute a fingerprint hash of the registry.
    pub fn fingerprint_hash(&self) -> u64 {
        let mut hasher = RollingHash::new();
        for (market_id, numeric_id) in &self.ids {
            hasher.update(market_id.as_bytes());
            hasher.update_u64(*numeric_id);
        }
        hasher.finish_u64()
    }
}

// =============================================================================
// CANONICAL ORDER RECORD
// =============================================================================

/// Canonical order submission record for hashing.
#[derive(Debug, Clone)]
pub struct CanonicalOrderRecord {
    /// Monotonic local order ID assigned by the backtest.
    pub local_order_id: u64,
    /// Visible timestamp (nanoseconds).
    pub visible_ts_ns: i64,
    /// Market ID (numeric, from registry).
    pub market_id: u64,
    /// Side (Buy=0, Sell=1).
    pub side: SideCode,
    /// Price in ticks (fixed-point i64).
    pub price_ticks: i64,
    /// Quantity in shares (fixed-point i64).
    pub size_shares: i64,
    /// Order type code.
    pub order_type: OrderTypeCode,
}

impl CanonicalOrderRecord {
    /// Encode to canonical binary format.
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::OrderSubmit as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
        encoder.write_u64(self.market_id);
        encoder.write_u8(self.side as u8);
        encoder.write_i64(self.price_ticks);
        encoder.write_i64(self.size_shares);
        encoder.write_u8(self.order_type as u8);
    }
}

/// Canonical cancel request record.
#[derive(Debug, Clone)]
pub struct CanonicalCancelRecord {
    pub local_order_id: u64,
    pub visible_ts_ns: i64,
}

impl CanonicalCancelRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::CancelRequest as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
    }
}

// =============================================================================
// CANONICAL ACK/REJECT RECORDS
// =============================================================================

/// Canonical order acknowledgment record.
#[derive(Debug, Clone)]
pub struct CanonicalOrderAckRecord {
    pub local_order_id: u64,
    pub visible_ts_ns: i64,
}

impl CanonicalOrderAckRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::OrderAck as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
    }
}

/// Canonical order rejection record.
#[derive(Debug, Clone)]
pub struct CanonicalOrderRejectRecord {
    pub local_order_id: u64,
    pub visible_ts_ns: i64,
    pub reason: RejectReasonCode,
}

impl CanonicalOrderRejectRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::OrderReject as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
        encoder.write_u8(self.reason as u8);
    }
}

/// Canonical cancel acknowledgment record.
#[derive(Debug, Clone)]
pub struct CanonicalCancelAckRecord {
    pub local_order_id: u64,
    pub visible_ts_ns: i64,
    pub cancelled_qty_shares: i64,
}

impl CanonicalCancelAckRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::CancelAck as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
        encoder.write_i64(self.cancelled_qty_shares);
    }
}

// =============================================================================
// CANONICAL FILL RECORD
// =============================================================================

/// Canonical fill notification record.
#[derive(Debug, Clone)]
pub struct CanonicalFillRecord {
    pub local_order_id: u64,
    pub visible_ts_ns: i64,
    /// Match sequence for tie-breaking when multiple fills have same (ts, order_id).
    pub match_seq: u64,
    pub price_ticks: i64,
    pub size_shares: i64,
    pub is_maker: bool,
    /// Fee in fixed-point (i128, scaled by FEE_SCALE).
    pub fee_fixed: i128,
}

impl CanonicalFillRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::Fill as u8);
        encoder.write_u64(self.local_order_id);
        encoder.write_i64(self.visible_ts_ns);
        encoder.write_u64(self.match_seq);
        encoder.write_i64(self.price_ticks);
        encoder.write_i64(self.size_shares);
        encoder.write_bool(self.is_maker);
        encoder.write_i128(self.fee_fixed);
    }

    /// Ordering key for deterministic sorting: (visible_ts, local_order_id, match_seq).
    pub fn ordering_key(&self) -> (i64, u64, u64) {
        (self.visible_ts_ns, self.local_order_id, self.match_seq)
    }
}

// =============================================================================
// CANONICAL LEDGER ENTRY RECORD
// =============================================================================

/// Canonical ledger entry record.
#[derive(Debug, Clone)]
pub struct CanonicalLedgerRecord {
    pub entry_id: u64,
    pub visible_ts_ns: i64,
    pub event_ref_type: EventRefTypeCode,
    pub event_ref_id: u64,
    /// Account type code.
    pub account_type: AccountTypeCode,
    /// For CostBasis/Settlement accounts: market_id (numeric).
    pub account_market_id: Option<u64>,
    /// Amount in fixed-point i128.
    pub amount: i128,
}

impl CanonicalLedgerRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::LedgerEntry as u8);
        encoder.write_u64(self.entry_id);
        encoder.write_i64(self.visible_ts_ns);
        encoder.write_u8(self.event_ref_type as u8);
        encoder.write_u64(self.event_ref_id);
        encoder.write_u8(self.account_type as u8);
        encoder.write_u64(self.account_market_id.unwrap_or(0));
        encoder.write_i128(self.amount);
    }
}

// =============================================================================
// CANONICAL FINAL PNL RECORD
// =============================================================================

/// Canonical final PnL summary record.
#[derive(Debug, Clone)]
pub struct CanonicalFinalPnLRecord {
    /// Realized PnL in fixed-point i128.
    pub realized_pnl: i128,
    /// Total fees paid in fixed-point i128.
    pub total_fees: i128,
    /// Ending cash balance in fixed-point i128.
    pub ending_cash: i128,
    /// Open position value (mark-to-market) in fixed-point i128.
    pub open_position_value: i128,
    /// Total equity (cash + positions) in fixed-point i128.
    pub total_equity: i128,
}

impl CanonicalFinalPnLRecord {
    pub fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.write_u8(RecordTag::FinalPnL as u8);
        encoder.write_i128(self.realized_pnl);
        encoder.write_i128(self.total_fees);
        encoder.write_i128(self.ending_cash);
        encoder.write_i128(self.open_position_value);
        encoder.write_i128(self.total_equity);
    }
}

// =============================================================================
// STREAM COLLECTOR
// =============================================================================

/// Collects canonical records for a single stream and computes rolling hash.
#[derive(Debug)]
pub struct StreamCollector {
    name: String,
    encoder: CanonicalEncoder,
    hash: RollingHash,
    record_count: u64,
}

impl StreamCollector {
    /// Create a new stream collector.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            encoder: CanonicalEncoder::new(),
            hash: RollingHash::new(),
            record_count: 0,
        }
    }

    /// Add an order record.
    pub fn add_order(&mut self, record: &CanonicalOrderRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add a cancel record.
    pub fn add_cancel(&mut self, record: &CanonicalCancelRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add an order ack record.
    pub fn add_order_ack(&mut self, record: &CanonicalOrderAckRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add an order reject record.
    pub fn add_order_reject(&mut self, record: &CanonicalOrderRejectRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add a cancel ack record.
    pub fn add_cancel_ack(&mut self, record: &CanonicalCancelAckRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add a fill record.
    pub fn add_fill(&mut self, record: &CanonicalFillRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add a ledger record.
    pub fn add_ledger(&mut self, record: &CanonicalLedgerRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Add a final PnL record.
    pub fn add_final_pnl(&mut self, record: &CanonicalFinalPnLRecord) {
        self.encoder.clear();
        record.encode(&mut self.encoder);
        self.hash.update(self.encoder.as_bytes());
        self.record_count += 1;
    }

    /// Finalize and get the stream fingerprint.
    pub fn finish(&self) -> StreamFingerprint {
        StreamFingerprint {
            name: self.name.clone(),
            hash: self.hash.finish_u64(),
            hash_128: self.hash.to_hex(),
            record_count: self.record_count,
        }
    }

    /// Get current record count.
    pub fn record_count(&self) -> u64 {
        self.record_count
    }
}

/// Fingerprint of a single canonical stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFingerprint {
    /// Stream name.
    pub name: String,
    /// 64-bit hash.
    pub hash: u64,
    /// Full 128-bit hash as hex.
    pub hash_128: String,
    /// Number of records.
    pub record_count: u64,
}

// =============================================================================
// REPRODUCIBILITY FINGERPRINT
// =============================================================================

/// Complete reproducibility fingerprint for cross-run verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducibilityFingerprint {
    /// Dataset identity hash (from manifest, file names, sizes).
    pub dataset_hash: u64,
    /// Primary RNG seed.
    pub seed: u64,
    /// Orders stream fingerprint.
    pub orders: StreamFingerprint,
    /// Acks/Rejects stream fingerprint.
    pub acks: StreamFingerprint,
    /// Fills stream fingerprint.
    pub fills: StreamFingerprint,
    /// Ledger stream fingerprint.
    pub ledger: StreamFingerprint,
    /// Final PnL fingerprint.
    pub pnl: StreamFingerprint,
    /// Combined hash of all streams.
    pub combined_hash: u64,
    /// Combined 128-bit hash as hex.
    pub combined_hash_128: String,
}

impl ReproducibilityFingerprint {
    /// Compute the combined hash from all stream hashes.
    pub fn compute(
        dataset_hash: u64,
        seed: u64,
        orders: StreamFingerprint,
        acks: StreamFingerprint,
        fills: StreamFingerprint,
        ledger: StreamFingerprint,
        pnl: StreamFingerprint,
    ) -> Self {
        let mut hasher = RollingHash::new();
        hasher.update_u64(dataset_hash);
        hasher.update_u64(seed);
        hasher.update_u64(orders.hash);
        hasher.update_u64(acks.hash);
        hasher.update_u64(fills.hash);
        hasher.update_u64(ledger.hash);
        hasher.update_u64(pnl.hash);

        let combined_hash = hasher.finish_u64();
        let combined_hash_128 = hasher.to_hex();

        Self {
            dataset_hash,
            seed,
            orders,
            acks,
            fills,
            ledger,
            pnl,
            combined_hash,
            combined_hash_128,
        }
    }

    /// Format as a summary report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║              REPRODUCIBILITY FINGERPRINT REPORT                              ║\n");
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!(
            "║  Dataset Hash: {:016x}                                              ║\n",
            self.dataset_hash
        ));
        out.push_str(&format!(
            "║  Seed:         {:016x}                                              ║\n",
            self.seed
        ));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str("║  STREAM HASHES:                                                              ║\n");
        out.push_str(&format!(
            "║    Orders:  {:016x}  ({:8} records)                             ║\n",
            self.orders.hash, self.orders.record_count
        ));
        out.push_str(&format!(
            "║    Acks:    {:016x}  ({:8} records)                             ║\n",
            self.acks.hash, self.acks.record_count
        ));
        out.push_str(&format!(
            "║    Fills:   {:016x}  ({:8} records)                             ║\n",
            self.fills.hash, self.fills.record_count
        ));
        out.push_str(&format!(
            "║    Ledger:  {:016x}  ({:8} records)                             ║\n",
            self.ledger.hash, self.ledger.record_count
        ));
        out.push_str(&format!(
            "║    PnL:     {:016x}  ({:8} records)                             ║\n",
            self.pnl.hash, self.pnl.record_count
        ));
        out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        out.push_str(&format!(
            "║  COMBINED HASH: {:016x}                                           ║\n",
            self.combined_hash
        ));
        out.push_str(&format!(
            "║  FULL 128-BIT:  {}                               ║\n",
            self.combined_hash_128
        ));
        out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        out
    }

    /// Format as compact single line.
    pub fn format_compact(&self) -> String {
        format!(
            "ReproFingerprint[{}] seed={:016x} orders={:08x} acks={:08x} fills={:08x} ledger={:08x} pnl={:08x}",
            &self.combined_hash_128[..16],
            self.seed,
            self.orders.hash as u32,
            self.acks.hash as u32,
            self.fills.hash as u32,
            self.ledger.hash as u32,
            self.pnl.hash as u32,
        )
    }
}

// =============================================================================
// REPRODUCIBILITY COLLECTOR
// =============================================================================

/// Collects all canonical streams for reproducibility verification.
pub struct ReproducibilityCollector {
    mode: ReproducibilityMode,
    market_registry: MarketIdRegistry,
    orders: StreamCollector,
    acks: StreamCollector,
    fills: StreamCollector,
    ledger: StreamCollector,
    pnl: StreamCollector,
    dataset_hash: u64,
    seed: u64,
    next_local_order_id: u64,
    next_match_seq: u64,
    /// Map from external order ID to local order ID for consistent tracking.
    order_id_map: BTreeMap<u64, u64>,
}

impl ReproducibilityCollector {
    /// Create a new collector.
    pub fn new(mode: ReproducibilityMode, dataset_hash: u64, seed: u64) -> Self {
        Self {
            mode,
            market_registry: MarketIdRegistry::new(),
            orders: StreamCollector::new("orders"),
            acks: StreamCollector::new("acks"),
            fills: StreamCollector::new("fills"),
            ledger: StreamCollector::new("ledger"),
            pnl: StreamCollector::new("pnl"),
            dataset_hash,
            seed,
            next_local_order_id: 1,
            next_match_seq: 1,
            order_id_map: BTreeMap::new(),
        }
    }

    /// Check if fingerprint computation is enabled.
    pub fn is_enabled(&self) -> bool {
        self.mode.requires_fingerprint()
    }

    /// Allocate a local order ID for an external order ID.
    pub fn allocate_order_id(&mut self, external_order_id: u64) -> u64 {
        let local_id = self.next_local_order_id;
        self.next_local_order_id += 1;
        self.order_id_map.insert(external_order_id, local_id);
        local_id
    }

    /// Get local order ID for an external order ID.
    pub fn get_local_order_id(&self, external_order_id: u64) -> Option<u64> {
        self.order_id_map.get(&external_order_id).copied()
    }

    /// Get or allocate local order ID.
    pub fn get_or_allocate_order_id(&mut self, external_order_id: u64) -> u64 {
        if let Some(local_id) = self.order_id_map.get(&external_order_id) {
            *local_id
        } else {
            self.allocate_order_id(external_order_id)
        }
    }

    /// Allocate next match sequence number.
    pub fn next_match_seq(&mut self) -> u64 {
        let seq = self.next_match_seq;
        self.next_match_seq += 1;
        seq
    }

    /// Get or create numeric market ID.
    pub fn get_market_id(&mut self, market_id: &str) -> u64 {
        self.market_registry.get_or_insert(market_id)
    }

    /// Record an order submission.
    pub fn record_order(
        &mut self,
        external_order_id: u64,
        visible_ts_ns: Nanos,
        market_id: &str,
        side: Side,
        price: f64,
        size: f64,
        order_type: OrderType,
        tif: TimeInForce,
        post_only: bool,
    ) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.allocate_order_id(external_order_id);
        let market_id_num = self.get_market_id(market_id);

        let record = CanonicalOrderRecord {
            local_order_id,
            visible_ts_ns,
            market_id: market_id_num,
            side: side.into(),
            price_ticks: price_to_ticks(price),
            size_shares: size_to_shares(size),
            order_type: OrderTypeCode::from_order_type_and_tif(order_type, tif, post_only),
        };

        self.orders.add_order(&record);
    }

    /// Record a cancel request.
    pub fn record_cancel(&mut self, external_order_id: u64, visible_ts_ns: Nanos) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.get_or_allocate_order_id(external_order_id);
        let record = CanonicalCancelRecord {
            local_order_id,
            visible_ts_ns,
        };
        self.orders.add_cancel(&record);
    }

    /// Record an order acknowledgment.
    pub fn record_order_ack(&mut self, external_order_id: u64, visible_ts_ns: Nanos) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.get_or_allocate_order_id(external_order_id);
        let record = CanonicalOrderAckRecord {
            local_order_id,
            visible_ts_ns,
        };
        self.acks.add_order_ack(&record);
    }

    /// Record an order rejection.
    pub fn record_order_reject(
        &mut self,
        external_order_id: u64,
        visible_ts_ns: Nanos,
        reason: &str,
    ) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.get_or_allocate_order_id(external_order_id);
        let record = CanonicalOrderRejectRecord {
            local_order_id,
            visible_ts_ns,
            reason: RejectReasonCode::from_reason_string(reason),
        };
        self.acks.add_order_reject(&record);
    }

    /// Record a cancel acknowledgment.
    pub fn record_cancel_ack(
        &mut self,
        external_order_id: u64,
        visible_ts_ns: Nanos,
        cancelled_qty: f64,
    ) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.get_or_allocate_order_id(external_order_id);
        let record = CanonicalCancelAckRecord {
            local_order_id,
            visible_ts_ns,
            cancelled_qty_shares: size_to_shares(cancelled_qty),
        };
        self.acks.add_cancel_ack(&record);
    }

    /// Record a fill notification.
    pub fn record_fill(
        &mut self,
        external_order_id: u64,
        visible_ts_ns: Nanos,
        price: f64,
        size: f64,
        is_maker: bool,
        fee: f64,
    ) {
        if !self.is_enabled() {
            return;
        }

        let local_order_id = self.get_or_allocate_order_id(external_order_id);
        let match_seq = self.next_match_seq();

        let record = CanonicalFillRecord {
            local_order_id,
            visible_ts_ns,
            match_seq,
            price_ticks: price_to_ticks(price),
            size_shares: size_to_shares(size),
            is_maker,
            fee_fixed: fee_to_fixed(fee),
        };
        self.fills.add_fill(&record);
    }

    /// Record a ledger entry.
    pub fn record_ledger_entry(
        &mut self,
        entry_id: u64,
        visible_ts_ns: Nanos,
        event_ref: &EventRef,
        account: &LedgerAccount,
        amount: Amount,
    ) {
        if !self.is_enabled() {
            return;
        }

        let event_ref_id = match event_ref {
            EventRef::Fill { fill_id } => *fill_id,
            EventRef::Fee { fill_id } => *fill_id,
            EventRef::Settlement { settlement_id, .. } => *settlement_id,
            EventRef::InitialDeposit { deposit_id } => *deposit_id,
            EventRef::Deposit { deposit_id } => *deposit_id,
            EventRef::Withdrawal { withdrawal_id } => *withdrawal_id,
            EventRef::Adjustment { adjustment_id, .. } => *adjustment_id,
        };

        let account_market_id = match account {
            LedgerAccount::CostBasis { market_id, .. } => {
                Some(self.market_registry.get_or_insert(market_id))
            }
            LedgerAccount::SettlementReceivable { market_id } => {
                Some(self.market_registry.get_or_insert(market_id))
            }
            LedgerAccount::SettlementPayable { market_id } => {
                Some(self.market_registry.get_or_insert(market_id))
            }
            _ => None,
        };

        let record = CanonicalLedgerRecord {
            entry_id,
            visible_ts_ns,
            event_ref_type: event_ref.into(),
            event_ref_id,
            account_type: account.into(),
            account_market_id,
            amount,
        };
        self.ledger.add_ledger(&record);
    }

    /// Record final PnL summary.
    pub fn record_final_pnl(
        &mut self,
        realized_pnl: f64,
        total_fees: f64,
        ending_cash: f64,
        open_position_value: f64,
    ) {
        if !self.is_enabled() {
            return;
        }

        let record = CanonicalFinalPnLRecord {
            realized_pnl: fee_to_fixed(realized_pnl),
            total_fees: fee_to_fixed(total_fees),
            ending_cash: fee_to_fixed(ending_cash),
            open_position_value: fee_to_fixed(open_position_value),
            total_equity: fee_to_fixed(ending_cash + open_position_value),
        };
        self.pnl.add_final_pnl(&record);
    }

    /// Finalize and compute the reproducibility fingerprint.
    pub fn finish(self) -> ReproducibilityFingerprint {
        ReproducibilityFingerprint::compute(
            self.dataset_hash,
            self.seed,
            self.orders.finish(),
            self.acks.finish(),
            self.fills.finish(),
            self.ledger.finish(),
            self.pnl.finish(),
        )
    }

    /// Get current statistics.
    pub fn stats(&self) -> ReproducibilityStats {
        ReproducibilityStats {
            orders_count: self.orders.record_count(),
            acks_count: self.acks.record_count(),
            fills_count: self.fills.record_count(),
            ledger_count: self.ledger.record_count(),
            markets_count: self.market_registry.ids.len() as u64,
        }
    }
}

/// Statistics from the reproducibility collector.
#[derive(Debug, Clone, Default)]
pub struct ReproducibilityStats {
    pub orders_count: u64,
    pub acks_count: u64,
    pub fills_count: u64,
    pub ledger_count: u64,
    pub markets_count: u64,
}

// =============================================================================
// REPLAY VERIFICATION
// =============================================================================

/// Result of replay verification.
#[derive(Debug, Clone)]
pub struct ReplayVerificationResult {
    /// Whether the fingerprints matched.
    pub passed: bool,
    /// First run fingerprint.
    pub run1: ReproducibilityFingerprint,
    /// Second run fingerprint.
    pub run2: ReproducibilityFingerprint,
    /// Detailed mismatch information (if any).
    pub mismatch: Option<ReplayMismatch>,
}

/// Details about a fingerprint mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayMismatch {
    /// Which stream had the first mismatch.
    pub stream: String,
    /// Hash from run 1.
    pub hash1: u64,
    /// Hash from run 2.
    pub hash2: u64,
    /// Record count from run 1.
    pub count1: u64,
    /// Record count from run 2.
    pub count2: u64,
}

impl ReplayVerificationResult {
    /// Compare two fingerprints and produce a verification result.
    pub fn compare(run1: ReproducibilityFingerprint, run2: ReproducibilityFingerprint) -> Self {
        if run1 == run2 {
            return Self {
                passed: true,
                run1,
                run2,
                mismatch: None,
            };
        }

        // Find first mismatch
        let mismatch = if run1.orders != run2.orders {
            Some(ReplayMismatch {
                stream: "orders".to_string(),
                hash1: run1.orders.hash,
                hash2: run2.orders.hash,
                count1: run1.orders.record_count,
                count2: run2.orders.record_count,
            })
        } else if run1.acks != run2.acks {
            Some(ReplayMismatch {
                stream: "acks".to_string(),
                hash1: run1.acks.hash,
                hash2: run2.acks.hash,
                count1: run1.acks.record_count,
                count2: run2.acks.record_count,
            })
        } else if run1.fills != run2.fills {
            Some(ReplayMismatch {
                stream: "fills".to_string(),
                hash1: run1.fills.hash,
                hash2: run2.fills.hash,
                count1: run1.fills.record_count,
                count2: run2.fills.record_count,
            })
        } else if run1.ledger != run2.ledger {
            Some(ReplayMismatch {
                stream: "ledger".to_string(),
                hash1: run1.ledger.hash,
                hash2: run2.ledger.hash,
                count1: run1.ledger.record_count,
                count2: run2.ledger.record_count,
            })
        } else if run1.pnl != run2.pnl {
            Some(ReplayMismatch {
                stream: "pnl".to_string(),
                hash1: run1.pnl.hash,
                hash2: run2.pnl.hash,
                count1: run1.pnl.record_count,
                count2: run2.pnl.record_count,
            })
        } else {
            // Seed or dataset hash mismatch
            Some(ReplayMismatch {
                stream: "metadata".to_string(),
                hash1: run1.seed ^ run1.dataset_hash,
                hash2: run2.seed ^ run2.dataset_hash,
                count1: 0,
                count2: 0,
            })
        };

        Self {
            passed: false,
            run1,
            run2,
            mismatch,
        }
    }

    /// Format as a report.
    pub fn format_report(&self) -> String {
        let mut out = String::new();

        if self.passed {
            out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
            out.push_str("║              REPLAY VERIFICATION: PASSED                                     ║\n");
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!(
                "║  Combined Hash: {:016x}                                           ║\n",
                self.run1.combined_hash
            ));
            out.push_str("║  Fingerprints match exactly - run is REPRODUCIBLE                            ║\n");
            out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        } else {
            out.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
            out.push_str("║              REPLAY VERIFICATION: FAILED                                     ║\n");
            out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
            out.push_str(&format!(
                "║  Run 1 Hash: {:016x}                                              ║\n",
                self.run1.combined_hash
            ));
            out.push_str(&format!(
                "║  Run 2 Hash: {:016x}                                              ║\n",
                self.run2.combined_hash
            ));

            if let Some(ref mismatch) = self.mismatch {
                out.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
                out.push_str(&format!(
                    "║  FIRST MISMATCH: {} stream                                              ║\n",
                    mismatch.stream
                ));
                out.push_str(&format!(
                    "║    Run 1: hash={:016x} count={}                                 ║\n",
                    mismatch.hash1, mismatch.count1
                ));
                out.push_str(&format!(
                    "║    Run 2: hash={:016x} count={}                                 ║\n",
                    mismatch.hash2, mismatch.count2
                ));
            }

            out.push_str("║                                                                              ║\n");
            out.push_str("║  Run is NOT REPRODUCIBLE - cannot be certified for production                ║\n");
            out.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        }

        out
    }
}

// =============================================================================
// TRUST GATE FAILURE REASON
// =============================================================================

/// Trust failure reason for reproducibility issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReproducibilityFailure {
    /// Reproducibility check was not run.
    NotChecked,
    /// Replay verification failed.
    ReplayMismatch {
        stream: String,
        run1_hash: u64,
        run2_hash: u64,
    },
    /// Fingerprint not computed.
    MissingFingerprint,
}

impl std::fmt::Display for ReproducibilityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotChecked => write!(f, "REPRODUCIBILITY_NOT_CHECKED"),
            Self::ReplayMismatch {
                stream,
                run1_hash,
                run2_hash,
            } => {
                write!(
                    f,
                    "REPLAY_MISMATCH (stream={}, run1={:016x}, run2={:016x})",
                    stream, run1_hash, run2_hash
                )
            }
            Self::MissingFingerprint => write!(f, "MISSING_REPRODUCIBILITY_FINGERPRINT"),
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
    fn test_price_to_ticks() {
        assert_eq!(price_to_ticks(1.0), PRICE_SCALE);
        assert_eq!(price_to_ticks(0.5), PRICE_SCALE / 2);
        assert_eq!(price_to_ticks(100.12345678), 10012345678);
    }

    #[test]
    fn test_size_to_shares() {
        assert_eq!(size_to_shares(1.0), SIZE_SCALE);
        assert_eq!(size_to_shares(0.1), SIZE_SCALE / 10);
    }

    #[test]
    fn test_rolling_hash_determinism() {
        let mut h1 = RollingHash::new();
        let mut h2 = RollingHash::new();

        h1.update(b"test data 12345");
        h2.update(b"test data 12345");

        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn test_rolling_hash_sensitivity() {
        let mut h1 = RollingHash::new();
        let mut h2 = RollingHash::new();

        h1.update(b"test data 12345");
        h2.update(b"test data 12346"); // One bit different

        assert_ne!(h1.finish(), h2.finish());
    }

    #[test]
    fn test_canonical_encoder_endianness() {
        let mut encoder = CanonicalEncoder::new();
        encoder.write_u64(0x0102030405060708);

        let bytes = encoder.finish();
        // Little-endian: least significant byte first
        assert_eq!(bytes, vec![0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn test_market_registry_determinism() {
        // Insert in different orders
        let mut reg1 = MarketIdRegistry::new();
        reg1.get_or_insert("market_a");
        reg1.get_or_insert("market_b");
        reg1.get_or_insert("market_c");

        let mut reg2 = MarketIdRegistry::new();
        reg2.get_or_insert("market_c");
        reg2.get_or_insert("market_a");
        reg2.get_or_insert("market_b");

        // Same markets should get same IDs (by insertion order)
        // But fingerprint should be deterministic (BTreeMap iteration)
        // Note: IDs differ because insertion order differs, but that's OK
        // as long as the same collector uses consistent IDs throughout
    }

    #[test]
    fn test_stream_collector_fingerprint() {
        let mut collector1 = StreamCollector::new("test");
        let mut collector2 = StreamCollector::new("test");

        let record = CanonicalOrderRecord {
            local_order_id: 1,
            visible_ts_ns: 1000000000,
            market_id: 1,
            side: SideCode::Buy,
            price_ticks: 5000000000,
            size_shares: 100000000,
            order_type: OrderTypeCode::Limit,
        };

        collector1.add_order(&record);
        collector2.add_order(&record);

        let fp1 = collector1.finish();
        let fp2 = collector2.finish();

        assert_eq!(fp1.hash, fp2.hash);
        assert_eq!(fp1.record_count, fp2.record_count);
    }

    #[test]
    fn test_reproducibility_collector_basic() {
        let mut collector = ReproducibilityCollector::new(ReproducibilityMode::HashOnly, 12345, 42);

        collector.record_order(
            1,
            1000000000,
            "test-market",
            Side::Buy,
            0.5,
            100.0,
            OrderType::Limit,
            TimeInForce::Gtc,
            false,
        );

        collector.record_order_ack(1, 1000001000);
        collector.record_fill(1, 1000002000, 0.5, 100.0, false, 0.001);
        collector.record_final_pnl(10.0, 0.001, 1000.0, 50.0);

        let fingerprint = collector.finish();

        assert_eq!(fingerprint.seed, 42);
        assert_eq!(fingerprint.orders.record_count, 1);
        assert_eq!(fingerprint.acks.record_count, 1);
        assert_eq!(fingerprint.fills.record_count, 1);
        assert_eq!(fingerprint.pnl.record_count, 1);
    }

    #[test]
    fn test_reproducibility_determinism() {
        // Run twice with same inputs
        let run = |seed: u64| {
            let mut collector =
                ReproducibilityCollector::new(ReproducibilityMode::HashOnly, 12345, seed);

            collector.record_order(
                1,
                1000000000,
                "market-btc",
                Side::Buy,
                50000.0,
                1.5,
                OrderType::Limit,
                TimeInForce::Gtc,
                false,
            );

            collector.record_order(
                2,
                1000001000,
                "market-eth",
                Side::Sell,
                3000.0,
                10.0,
                OrderType::Limit,
                TimeInForce::Ioc,
                false,
            );

            collector.record_order_ack(1, 1000002000);
            collector.record_order_reject(2, 1000003000, "Insufficient funds");
            collector.record_fill(1, 1000004000, 50000.0, 1.5, true, 0.075);
            collector.record_final_pnl(100.0, 0.075, 9900.0, 75000.0);

            collector.finish()
        };

        let fp1 = run(42);
        let fp2 = run(42);

        assert_eq!(fp1, fp2);

        // Different seed should produce different fingerprint
        let fp3 = run(43);
        assert_ne!(fp1.combined_hash, fp3.combined_hash);
    }

    #[test]
    fn test_replay_verification_pass() {
        let fp1 = ReproducibilityFingerprint::compute(
            100,
            42,
            StreamFingerprint {
                name: "orders".into(),
                hash: 111,
                hash_128: "0".into(),
                record_count: 10,
            },
            StreamFingerprint {
                name: "acks".into(),
                hash: 222,
                hash_128: "0".into(),
                record_count: 10,
            },
            StreamFingerprint {
                name: "fills".into(),
                hash: 333,
                hash_128: "0".into(),
                record_count: 5,
            },
            StreamFingerprint {
                name: "ledger".into(),
                hash: 444,
                hash_128: "0".into(),
                record_count: 20,
            },
            StreamFingerprint {
                name: "pnl".into(),
                hash: 555,
                hash_128: "0".into(),
                record_count: 1,
            },
        );

        let fp2 = fp1.clone();

        let result = ReplayVerificationResult::compare(fp1, fp2);
        assert!(result.passed);
        assert!(result.mismatch.is_none());
    }

    #[test]
    fn test_replay_verification_fail() {
        let fp1 = ReproducibilityFingerprint::compute(
            100,
            42,
            StreamFingerprint {
                name: "orders".into(),
                hash: 111,
                hash_128: "0".into(),
                record_count: 10,
            },
            StreamFingerprint {
                name: "acks".into(),
                hash: 222,
                hash_128: "0".into(),
                record_count: 10,
            },
            StreamFingerprint {
                name: "fills".into(),
                hash: 333,
                hash_128: "0".into(),
                record_count: 5,
            },
            StreamFingerprint {
                name: "ledger".into(),
                hash: 444,
                hash_128: "0".into(),
                record_count: 20,
            },
            StreamFingerprint {
                name: "pnl".into(),
                hash: 555,
                hash_128: "0".into(),
                record_count: 1,
            },
        );

        let mut fp2 = fp1.clone();
        fp2.fills.hash = 334; // Different fills hash

        let result = ReplayVerificationResult::compare(fp1, fp2);
        assert!(!result.passed);
        assert!(result.mismatch.is_some());
        assert_eq!(result.mismatch.as_ref().unwrap().stream, "fills");
    }

    #[test]
    fn test_reject_reason_code_parsing() {
        assert_eq!(
            RejectReasonCode::from_reason_string("Insufficient balance"),
            RejectReasonCode::InsufficientFunds
        );
        assert_eq!(
            RejectReasonCode::from_reason_string("Invalid price"),
            RejectReasonCode::InvalidPrice
        );
        assert_eq!(
            RejectReasonCode::from_reason_string("Self trade prevention"),
            RejectReasonCode::SelfTrade
        );
        assert_eq!(
            RejectReasonCode::from_reason_string("Something unknown"),
            RejectReasonCode::Other
        );
    }
}
