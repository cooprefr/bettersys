//! Throughput Tracking
//!
//! Measures requests per second, messages per second,
//! and other throughput metrics for the trading engine.

use parking_lot::RwLock;
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// Throughput tracker for measuring rates
#[derive(Debug)]
pub struct ThroughputTracker {
    start_time: Instant,

    // Event counters
    pub binance_updates: AtomicU64,
    pub dome_ws_events: AtomicU64,
    pub dome_rest_calls: AtomicU64,
    pub polymarket_book_updates: AtomicU64,
    pub signals_detected: AtomicU64,
    pub signals_stored: AtomicU64,
    pub api_requests: AtomicU64,
    pub ws_messages_sent: AtomicU64,
    pub trades_executed: AtomicU64,

    // Sliding window for recent throughput (last 60 seconds)
    pub recent_events: RwLock<VecDeque<TimestampedCount>>,
    window_size_secs: u64,
}

#[derive(Debug, Clone)]
pub struct TimestampedCount {
    pub timestamp: i64,
    pub binance: u64,
    pub dome_ws: u64,
    pub signals: u64,
    pub api: u64,
    pub trades: u64,
}

impl ThroughputTracker {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            binance_updates: AtomicU64::new(0),
            dome_ws_events: AtomicU64::new(0),
            dome_rest_calls: AtomicU64::new(0),
            polymarket_book_updates: AtomicU64::new(0),
            signals_detected: AtomicU64::new(0),
            signals_stored: AtomicU64::new(0),
            api_requests: AtomicU64::new(0),
            ws_messages_sent: AtomicU64::new(0),
            trades_executed: AtomicU64::new(0),
            recent_events: RwLock::new(VecDeque::with_capacity(60)),
            window_size_secs: 60,
        }
    }

    /// Record a Binance price update
    pub fn record_binance_update(&self) {
        self.binance_updates.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a Dome WebSocket event
    pub fn record_dome_ws_event(&self) {
        self.dome_ws_events.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a Dome REST call
    pub fn record_dome_rest_call(&self) {
        self.dome_rest_calls.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a Polymarket orderbook update
    pub fn record_polymarket_book(&self) {
        self.polymarket_book_updates.fetch_add(1, Ordering::Relaxed);
    }

    /// Record signal detection
    pub fn record_signal_detected(&self) {
        self.signals_detected.fetch_add(1, Ordering::Relaxed);
    }

    /// Record signal storage
    pub fn record_signal_stored(&self) {
        self.signals_stored.fetch_add(1, Ordering::Relaxed);
    }

    /// Record API request
    pub fn record_api_request(&self) {
        self.api_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Record WebSocket message sent
    pub fn record_ws_message(&self) {
        self.ws_messages_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Record trade execution
    pub fn record_trade(&self) {
        self.trades_executed.fetch_add(1, Ordering::Relaxed);
    }

    /// Take a snapshot for the sliding window (call every second)
    pub fn snapshot_window(&self) {
        let now = chrono::Utc::now().timestamp();
        let snapshot = TimestampedCount {
            timestamp: now,
            binance: self.binance_updates.load(Ordering::Relaxed),
            dome_ws: self.dome_ws_events.load(Ordering::Relaxed),
            signals: self.signals_detected.load(Ordering::Relaxed),
            api: self.api_requests.load(Ordering::Relaxed),
            trades: self.trades_executed.load(Ordering::Relaxed),
        };

        let mut recent = self.recent_events.write();
        recent.push_back(snapshot);

        // Keep only last window_size_secs entries
        while recent.len() > self.window_size_secs as usize {
            recent.pop_front();
        }
    }

    /// Calculate throughput rates (events per second)
    pub fn rates(&self) -> ThroughputRates {
        let elapsed_secs = self.start_time.elapsed().as_secs_f64().max(1.0);

        ThroughputRates {
            binance_per_sec: self.binance_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
            dome_ws_per_sec: self.dome_ws_events.load(Ordering::Relaxed) as f64 / elapsed_secs,
            dome_rest_per_sec: self.dome_rest_calls.load(Ordering::Relaxed) as f64 / elapsed_secs,
            polymarket_per_sec: self.polymarket_book_updates.load(Ordering::Relaxed) as f64
                / elapsed_secs,
            signals_per_sec: self.signals_detected.load(Ordering::Relaxed) as f64 / elapsed_secs,
            api_per_sec: self.api_requests.load(Ordering::Relaxed) as f64 / elapsed_secs,
            ws_messages_per_sec: self.ws_messages_sent.load(Ordering::Relaxed) as f64
                / elapsed_secs,
            trades_per_sec: self.trades_executed.load(Ordering::Relaxed) as f64 / elapsed_secs,
        }
    }

    /// Calculate recent throughput (last 60 seconds)
    pub fn recent_rates(&self) -> ThroughputRates {
        let recent = self.recent_events.read();
        if recent.len() < 2 {
            return ThroughputRates::default();
        }

        let first = recent.front().unwrap();
        let last = recent.back().unwrap();
        let elapsed_secs = (last.timestamp - first.timestamp).max(1) as f64;

        ThroughputRates {
            binance_per_sec: (last.binance - first.binance) as f64 / elapsed_secs,
            dome_ws_per_sec: (last.dome_ws - first.dome_ws) as f64 / elapsed_secs,
            dome_rest_per_sec: 0.0, // Not tracked in window
            polymarket_per_sec: 0.0,
            signals_per_sec: (last.signals - first.signals) as f64 / elapsed_secs,
            api_per_sec: (last.api - first.api) as f64 / elapsed_secs,
            ws_messages_per_sec: 0.0,
            trades_per_sec: (last.trades - first.trades) as f64 / elapsed_secs,
        }
    }

    /// Get snapshot
    pub fn snapshot(&self) -> ThroughputSnapshot {
        ThroughputSnapshot {
            uptime_secs: self.start_time.elapsed().as_secs_f64(),
            totals: ThroughputTotals {
                binance_updates: self.binance_updates.load(Ordering::Relaxed),
                dome_ws_events: self.dome_ws_events.load(Ordering::Relaxed),
                dome_rest_calls: self.dome_rest_calls.load(Ordering::Relaxed),
                polymarket_book_updates: self.polymarket_book_updates.load(Ordering::Relaxed),
                signals_detected: self.signals_detected.load(Ordering::Relaxed),
                signals_stored: self.signals_stored.load(Ordering::Relaxed),
                api_requests: self.api_requests.load(Ordering::Relaxed),
                ws_messages_sent: self.ws_messages_sent.load(Ordering::Relaxed),
                trades_executed: self.trades_executed.load(Ordering::Relaxed),
            },
            lifetime_rates: self.rates(),
            recent_rates: self.recent_rates(),
        }
    }
}

impl Default for ThroughputTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ThroughputRates {
    pub binance_per_sec: f64,
    pub dome_ws_per_sec: f64,
    pub dome_rest_per_sec: f64,
    pub polymarket_per_sec: f64,
    pub signals_per_sec: f64,
    pub api_per_sec: f64,
    pub ws_messages_per_sec: f64,
    pub trades_per_sec: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThroughputTotals {
    pub binance_updates: u64,
    pub dome_ws_events: u64,
    pub dome_rest_calls: u64,
    pub polymarket_book_updates: u64,
    pub signals_detected: u64,
    pub signals_stored: u64,
    pub api_requests: u64,
    pub ws_messages_sent: u64,
    pub trades_executed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThroughputSnapshot {
    pub uptime_secs: f64,
    pub totals: ThroughputTotals,
    pub lifetime_rates: ThroughputRates,
    pub recent_rates: ThroughputRates,
}
