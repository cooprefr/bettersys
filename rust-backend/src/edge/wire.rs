//! Wire Protocol for Edge Receiver
//!
//! Fixed 72-byte binary format for minimal parsing overhead.
//! Uses fixed-point arithmetic for prices/quantities.

use std::io::{self, Write};

/// Magic bytes: 0xED6E ("edge")
pub const EDGE_MAGIC: u16 = 0xED6E;

/// Current protocol version
pub const EDGE_VERSION: u8 = 1;

/// Total packet size in bytes
/// 2+1+1+1+3+8+8+8+8+8+8+8+8+4 = 76 bytes
pub const EDGE_TICK_SIZE: usize = 76;

/// Price/quantity multiplier for fixed-point (8 decimal places)
pub const FIXED_POINT_SCALE: f64 = 100_000_000.0;

/// Symbol identifiers (fits in u8)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SymbolId {
    BtcUsdt = 0,
    EthUsdt = 1,
    SolUsdt = 2,
    XrpUsdt = 3,
    Unknown = 255,
}

impl SymbolId {
    pub fn from_str(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "BTCUSDT" => Self::BtcUsdt,
            "ETHUSDT" => Self::EthUsdt,
            "SOLUSDT" => Self::SolUsdt,
            "XRPUSDT" => Self::XrpUsdt,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BtcUsdt => "BTCUSDT",
            Self::EthUsdt => "ETHUSDT",
            Self::SolUsdt => "SOLUSDT",
            Self::XrpUsdt => "XRPUSDT",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::BtcUsdt,
            1 => Self::EthUsdt,
            2 => Self::SolUsdt,
            3 => Self::XrpUsdt,
            _ => Self::Unknown,
        }
    }
}

/// Flags byte constants for edge tick
#[allow(non_snake_case)]
pub mod EdgeFlags {
    /// Binance sequence gap detected at edge
    pub const GAP_DETECTED: u8 = 0x01;
    /// Heartbeat packet (no price data)
    pub const HEARTBEAT: u8 = 0x02;
    /// Data is stale (>100ms old at edge)
    pub const STALE: u8 = 0x04;
    /// WebSocket reconnect in progress
    pub const RECONNECTING: u8 = 0x08;
    /// QUIC transport active (not UDP)
    pub const QUIC_ACTIVE: u8 = 0x10;
}

/// Compact wire format for a single symbol update (76 bytes)
///
/// Layout (all fields little-endian):
/// ```text
/// Offset  Size  Field
/// 0       2     magic (0xED6E)
/// 2       1     version
/// 3       1     flags
/// 4       1     symbol_id
/// 5       3     padding
/// 8       8     seq (edge monotonic)
/// 16      8     exchange_ts_ns
/// 24      8     edge_ts_ns
/// 32      8     bid (fixed-point)
/// 40      8     ask (fixed-point)
/// 48      8     bid_qty (fixed-point)
/// 56      8     ask_qty (fixed-point)
/// 64      8     binance_update_id
/// 72      4     checksum (CRC32)
/// Total: 76 bytes
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct EdgeTick {
    pub magic: u16,
    pub version: u8,
    pub flags: u8,
    pub symbol_id: u8,
    pub _pad: [u8; 3],
    pub seq: u64,
    pub exchange_ts_ns: i64,
    pub edge_ts_ns: i64,
    pub bid: i64,
    pub ask: i64,
    pub bid_qty: i64,
    pub ask_qty: i64,
    pub binance_update_id: u64,
    pub checksum: u32,
}

// Verify size at compile time
const _: () = assert!(std::mem::size_of::<EdgeTick>() == EDGE_TICK_SIZE);

impl Default for EdgeTick {
    fn default() -> Self {
        Self {
            magic: EDGE_MAGIC,
            version: EDGE_VERSION,
            flags: 0,
            symbol_id: SymbolId::Unknown as u8,
            _pad: [0; 3],
            seq: 0,
            exchange_ts_ns: 0,
            edge_ts_ns: 0,
            bid: 0,
            ask: 0,
            bid_qty: 0,
            ask_qty: 0,
            binance_update_id: 0,
            checksum: 0,
        }
    }
}

impl EdgeTick {
    /// Create a new tick with prices
    pub fn new(
        symbol: SymbolId,
        seq: u64,
        exchange_ts_ns: i64,
        edge_ts_ns: i64,
        bid: f64,
        ask: f64,
        bid_qty: f64,
        ask_qty: f64,
        binance_update_id: u64,
    ) -> Self {
        let mut tick = Self {
            magic: EDGE_MAGIC,
            version: EDGE_VERSION,
            flags: 0,
            symbol_id: symbol as u8,
            _pad: [0; 3],
            seq,
            exchange_ts_ns,
            edge_ts_ns,
            bid: (bid * FIXED_POINT_SCALE) as i64,
            ask: (ask * FIXED_POINT_SCALE) as i64,
            bid_qty: (bid_qty * FIXED_POINT_SCALE) as i64,
            ask_qty: (ask_qty * FIXED_POINT_SCALE) as i64,
            binance_update_id,
            checksum: 0,
        };
        tick.checksum = tick.compute_checksum();
        tick
    }

    /// Create a heartbeat tick
    pub fn heartbeat(seq: u64, edge_ts_ns: i64) -> Self {
        let mut tick = Self {
            magic: EDGE_MAGIC,
            version: EDGE_VERSION,
            flags: EdgeFlags::HEARTBEAT,
            symbol_id: 0xFF,
            _pad: [0; 3],
            seq,
            exchange_ts_ns: 0,
            edge_ts_ns,
            bid: 0,
            ask: 0,
            bid_qty: 0,
            ask_qty: 0,
            binance_update_id: 0,
            checksum: 0,
        };
        tick.checksum = tick.compute_checksum();
        tick
    }

    /// Set a flag
    pub fn with_flag(mut self, flag: u8) -> Self {
        self.flags |= flag;
        self.checksum = self.compute_checksum();
        self
    }

    /// Get bid price as f64
    #[inline]
    pub fn bid_f64(&self) -> f64 {
        self.bid as f64 / FIXED_POINT_SCALE
    }

    /// Get ask price as f64
    #[inline]
    pub fn ask_f64(&self) -> f64 {
        self.ask as f64 / FIXED_POINT_SCALE
    }

    /// Get mid price as f64
    #[inline]
    pub fn mid_f64(&self) -> f64 {
        (self.bid_f64() + self.ask_f64()) / 2.0
    }

    /// Get bid quantity as f64
    #[inline]
    pub fn bid_qty_f64(&self) -> f64 {
        self.bid_qty as f64 / FIXED_POINT_SCALE
    }

    /// Get ask quantity as f64
    #[inline]
    pub fn ask_qty_f64(&self) -> f64 {
        self.ask_qty as f64 / FIXED_POINT_SCALE
    }

    /// Get symbol enum
    #[inline]
    pub fn symbol(&self) -> SymbolId {
        SymbolId::from_u8(self.symbol_id)
    }

    /// Check if this is a heartbeat
    #[inline]
    pub fn is_heartbeat(&self) -> bool {
        self.flags & EdgeFlags::HEARTBEAT != 0
    }

    /// Check if gap was detected at edge
    #[inline]
    pub fn has_gap(&self) -> bool {
        self.flags & EdgeFlags::GAP_DETECTED != 0
    }

    /// Check if data is stale
    #[inline]
    pub fn is_stale(&self) -> bool {
        self.flags & EdgeFlags::STALE != 0
    }

    /// Compute CRC32 checksum of payload (excluding checksum field)
    pub fn compute_checksum(&self) -> u32 {
        let bytes = self.as_bytes_without_checksum();
        crc32_fast(bytes)
    }

    /// Verify checksum
    pub fn verify_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    /// Get bytes without checksum (for checksum calculation)
    fn as_bytes_without_checksum(&self) -> &[u8] {
        let ptr = self as *const Self as *const u8;
        // Everything except last 4 bytes (checksum)
        unsafe { std::slice::from_raw_parts(ptr, EDGE_TICK_SIZE - 4) }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; EDGE_TICK_SIZE] {
        let mut buf = [0u8; EDGE_TICK_SIZE];
        let ptr = self as *const Self as *const u8;
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), EDGE_TICK_SIZE);
        }
        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(buf: &[u8; EDGE_TICK_SIZE]) -> Self {
        unsafe { std::ptr::read(buf.as_ptr() as *const Self) }
    }

    /// Try to deserialize from slice (validates magic and checksum)
    pub fn try_from_slice(buf: &[u8]) -> Result<Self, EdgeWireError> {
        if buf.len() != EDGE_TICK_SIZE {
            return Err(EdgeWireError::InvalidSize(buf.len()));
        }

        let tick = Self::from_bytes(buf.try_into().unwrap());

        if tick.magic != EDGE_MAGIC {
            return Err(EdgeWireError::InvalidMagic(tick.magic));
        }

        if tick.version != EDGE_VERSION {
            return Err(EdgeWireError::UnsupportedVersion(tick.version));
        }

        if !tick.verify_checksum() {
            return Err(EdgeWireError::ChecksumMismatch);
        }

        Ok(tick)
    }

    /// Write to a writer
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.to_bytes())
    }
}

/// Errors during wire protocol parsing
#[derive(Debug, Clone)]
pub enum EdgeWireError {
    InvalidSize(usize),
    InvalidMagic(u16),
    UnsupportedVersion(u8),
    ChecksumMismatch,
}

impl std::fmt::Display for EdgeWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSize(s) => write!(f, "invalid packet size: {} (expected {})", s, EDGE_TICK_SIZE),
            Self::InvalidMagic(m) => write!(f, "invalid magic: 0x{:04X} (expected 0x{:04X})", m, EDGE_MAGIC),
            Self::UnsupportedVersion(v) => write!(f, "unsupported version: {} (expected {})", v, EDGE_VERSION),
            Self::ChecksumMismatch => write!(f, "checksum mismatch"),
        }
    }
}

impl std::error::Error for EdgeWireError {}

/// Fast CRC32 implementation (IEEE polynomial)
fn crc32_fast(data: &[u8]) -> u32 {
    const CRC32_TABLE: [u32; 256] = generate_crc32_table();
    
    let mut crc = 0xFFFFFFFF_u32;
    for &byte in data {
        let idx = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[idx] ^ (crc >> 8);
    }
    !crc
}

/// Generate CRC32 lookup table at compile time
const fn generate_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = 0xEDB88320 ^ (crc >> 1);
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_roundtrip() {
        let tick = EdgeTick::new(
            SymbolId::BtcUsdt,
            12345,
            1700000000_000_000_000,
            1700000000_100_000_000,
            50000.12345678,
            50001.87654321,
            1.5,
            2.3,
            999888777,
        );

        let bytes = tick.to_bytes();
        assert_eq!(bytes.len(), EDGE_TICK_SIZE);

        let restored = EdgeTick::try_from_slice(&bytes).unwrap();
        assert_eq!(restored.seq, 12345);
        assert!((restored.bid_f64() - 50000.12345678).abs() < 1e-7);
        assert!((restored.ask_f64() - 50001.87654321).abs() < 1e-7);
        assert_eq!(restored.symbol(), SymbolId::BtcUsdt);
    }

    #[test]
    fn test_heartbeat() {
        let hb = EdgeTick::heartbeat(100, 1700000000_000_000_000);
        assert!(hb.is_heartbeat());
        assert!(hb.verify_checksum());
    }

    #[test]
    fn test_checksum_detects_corruption() {
        let tick = EdgeTick::new(
            SymbolId::EthUsdt,
            1,
            0,
            0,
            3000.0,
            3001.0,
            10.0,
            10.0,
            1,
        );

        let mut bytes = tick.to_bytes();
        bytes[10] ^= 0xFF; // Corrupt a byte

        assert!(EdgeTick::try_from_slice(&bytes).is_err());
    }

    #[test]
    fn test_symbol_conversion() {
        assert_eq!(SymbolId::from_str("btcusdt"), SymbolId::BtcUsdt);
        assert_eq!(SymbolId::from_str("ETHUSDT"), SymbolId::EthUsdt);
        assert_eq!(SymbolId::BtcUsdt.as_str(), "BTCUSDT");
    }
}
