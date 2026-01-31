//! Edge Receiver Client - Runs in eu-west-1 (Ireland)
//!
//! Receives binary packets from the edge receiver via UDP,
//! handles loss/reorder, and provides tick data to strategies.

use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};

use super::wire::{EdgeTick, EdgeWireError, SymbolId, EDGE_TICK_SIZE};

/// Configuration for the edge receiver client
#[derive(Debug, Clone)]
pub struct EdgeReceiverClientConfig {
    /// Address to bind to for receiving
    pub bind_addr: SocketAddr,
    /// Reorder buffer timeout
    pub reorder_timeout: Duration,
    /// Heartbeat timeout (triggers fallback)
    pub heartbeat_timeout: Duration,
    /// Max packets to buffer for reordering
    pub reorder_buffer_size: usize,
    /// QUIC fallback threshold (loss ratio over 60s)
    pub quic_fallback_threshold: f64,
}

impl Default for EdgeReceiverClientConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:19876".parse().unwrap(),
            reorder_timeout: Duration::from_millis(5),
            heartbeat_timeout: Duration::from_millis(500),
            reorder_buffer_size: 16,
            quic_fallback_threshold: 0.01,
        }
    }
}

/// Sequence tracking result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceStatus {
    Ok,
    Duplicate,
    Gap { missing: u64 },
}

/// Sequence tracker for detecting gaps and duplicates
pub struct SequenceTracker {
    last_seq: u64,
    initialized: bool,
    gaps: Vec<(u64, u64)>, // (expected, received)
    dup_count: u64,
    gap_count: u64,
    total_missing: u64,
}

impl SequenceTracker {
    pub fn new() -> Self {
        Self {
            last_seq: 0,
            initialized: false,
            gaps: Vec::new(),
            dup_count: 0,
            gap_count: 0,
            total_missing: 0,
        }
    }

    pub fn check(&mut self, seq: u64) -> SequenceStatus {
        if !self.initialized {
            self.initialized = true;
            self.last_seq = seq;
            return SequenceStatus::Ok;
        }

        if seq == self.last_seq + 1 {
            self.last_seq = seq;
            SequenceStatus::Ok
        } else if seq <= self.last_seq {
            self.dup_count += 1;
            SequenceStatus::Duplicate
        } else {
            let missing = seq - self.last_seq - 1;
            self.gaps.push((self.last_seq + 1, seq));
            self.gap_count += 1;
            self.total_missing += missing;
            self.last_seq = seq;
            SequenceStatus::Gap { missing }
        }
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        (self.gap_count, self.dup_count, self.total_missing)
    }
}

/// Buffered item for reordering
struct BufferedTick {
    tick: EdgeTick,
    recv_time: Instant,
}

/// Reorder buffer with timeout-based delivery
struct ReorderBuffer {
    items: Vec<BufferedTick>,
    max_size: usize,
    timeout: Duration,
}

impl ReorderBuffer {
    fn new(max_size: usize, timeout: Duration) -> Self {
        Self {
            items: Vec::with_capacity(max_size),
            max_size,
            timeout,
        }
    }

    fn insert(&mut self, tick: EdgeTick) {
        if self.items.len() >= self.max_size {
            // Buffer full, drop oldest
            self.items.remove(0);
        }
        self.items.push(BufferedTick {
            tick,
            recv_time: Instant::now(),
        });
        // Keep sorted by sequence
        self.items.sort_by_key(|b| b.tick.seq);
    }

    fn drain_ready(&mut self, expected_seq: u64) -> Vec<EdgeTick> {
        let now = Instant::now();
        let mut ready = Vec::new();
        let mut expected = expected_seq;

        // First, drain in-order items
        while !self.items.is_empty() && self.items[0].tick.seq == expected {
            ready.push(self.items.remove(0).tick);
            expected += 1;
        }

        // Then, drain timed-out items (accept gaps)
        while !self.items.is_empty() {
            let age = now.duration_since(self.items[0].recv_time);
            if age >= self.timeout {
                ready.push(self.items.remove(0).tick);
            } else {
                break;
            }
        }

        ready
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

/// Client-side statistics
#[derive(Debug, Default)]
pub struct EdgeClientStats {
    pub packets_received: AtomicU64,
    pub packets_delivered: AtomicU64,
    pub heartbeats_received: AtomicU64,
    pub gaps_detected: AtomicU64,
    pub duplicates: AtomicU64,
    pub checksum_errors: AtomicU64,
    pub malformed_packets: AtomicU64,
    pub reorder_events: AtomicU64,
    pub timeout_deliveries: AtomicU64,
    pub bytes_received: AtomicU64,
}

impl EdgeClientStats {
    pub fn snapshot(&self) -> EdgeClientStatsSnapshot {
        EdgeClientStatsSnapshot {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_delivered: self.packets_delivered.load(Ordering::Relaxed),
            heartbeats_received: self.heartbeats_received.load(Ordering::Relaxed),
            gaps_detected: self.gaps_detected.load(Ordering::Relaxed),
            duplicates: self.duplicates.load(Ordering::Relaxed),
            checksum_errors: self.checksum_errors.load(Ordering::Relaxed),
            malformed_packets: self.malformed_packets.load(Ordering::Relaxed),
            reorder_events: self.reorder_events.load(Ordering::Relaxed),
            timeout_deliveries: self.timeout_deliveries.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EdgeClientStatsSnapshot {
    pub packets_received: u64,
    pub packets_delivered: u64,
    pub heartbeats_received: u64,
    pub gaps_detected: u64,
    pub duplicates: u64,
    pub checksum_errors: u64,
    pub malformed_packets: u64,
    pub reorder_events: u64,
    pub timeout_deliveries: u64,
    pub bytes_received: u64,
}

/// Latest tick per symbol (lock-free reads via SeqLock pattern)
pub struct SymbolSnapshot {
    pub tick: EdgeTick,
    pub local_recv_ns: u64,
}

/// The edge receiver client
pub struct EdgeReceiverClient {
    config: EdgeReceiverClientConfig,
    running: Arc<AtomicBool>,
    stats: Arc<EdgeClientStats>,
    
    // Latest ticks per symbol (RwLock for simplicity, could use SeqLock)
    latest_ticks: Arc<RwLock<HashMap<SymbolId, SymbolSnapshot>>>,
    
    // Last heartbeat time
    last_heartbeat: Arc<RwLock<Instant>>,
    
    // Thread handle
    recv_thread: Mutex<Option<JoinHandle<()>>>,
    
    // Callback for new ticks
    tick_callback: Arc<RwLock<Option<Box<dyn Fn(EdgeTick) + Send + Sync>>>>,
    
    // Start time for monotonic timestamps
    start_instant: Instant,
}

impl EdgeReceiverClient {
    pub fn new(config: EdgeReceiverClientConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(EdgeClientStats::default()),
            latest_ticks: Arc::new(RwLock::new(HashMap::new())),
            last_heartbeat: Arc::new(RwLock::new(Instant::now())),
            recv_thread: Mutex::new(None),
            tick_callback: Arc::new(RwLock::new(None)),
            start_instant: Instant::now(),
        })
    }

    /// Get monotonic nanosecond timestamp
    #[inline]
    pub fn now_ns(&self) -> u64 {
        self.start_instant.elapsed().as_nanos() as u64
    }

    /// Set callback for new ticks
    pub fn set_callback<F>(&self, callback: F)
    where
        F: Fn(EdgeTick) + Send + Sync + 'static,
    {
        *self.tick_callback.write() = Some(Box::new(callback));
    }

    /// Get latest tick for a symbol
    pub fn latest(&self, symbol: SymbolId) -> Option<EdgeTick> {
        self.latest_ticks.read().get(&symbol).map(|s| s.tick)
    }

    /// Get latest mid price for a symbol
    pub fn mid(&self, symbol: &str) -> Option<f64> {
        let sym = SymbolId::from_str(symbol);
        self.latest(sym).map(|t| t.mid_f64())
    }

    /// Check if data is stale (no heartbeat recently)
    pub fn is_stale(&self) -> bool {
        let last = *self.last_heartbeat.read();
        last.elapsed() > self.config.heartbeat_timeout
    }

    /// Time since last heartbeat
    pub fn heartbeat_age(&self) -> Duration {
        self.last_heartbeat.read().elapsed()
    }

    /// Get stats
    pub fn stats(&self) -> &EdgeClientStats {
        &self.stats
    }

    /// Start the receiver thread
    pub fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut handle = self.recv_thread.lock();
        if handle.is_some() {
            return Ok(());
        }

        self.running.store(true, Ordering::SeqCst);

        let client = self.clone();
        let socket = UdpSocket::bind(client.config.bind_addr)?;
        socket.set_read_timeout(Some(Duration::from_millis(50)))?;

        info!("Edge client listening on {}", client.config.bind_addr);

        let thread = thread::Builder::new()
            .name("edge-receiver-client".to_string())
            .spawn(move || {
                client.recv_loop(socket);
            })?;

        *handle = Some(thread);
        Ok(())
    }

    /// Stop the receiver
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.recv_thread.lock().take() {
            let _ = handle.join();
        }
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Receive loop (runs on dedicated thread)
    fn recv_loop(self: Arc<Self>, socket: UdpSocket) {
        let mut buf = [0u8; EDGE_TICK_SIZE];
        let mut seq_tracker = SequenceTracker::new();
        let mut reorder_buffer = ReorderBuffer::new(
            self.config.reorder_buffer_size,
            self.config.reorder_timeout,
        );

        while self.running.load(Ordering::Relaxed) {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    let recv_ns = self.now_ns();
                    self.stats.bytes_received.fetch_add(n as u64, Ordering::Relaxed);

                    if n != EDGE_TICK_SIZE {
                        self.stats.malformed_packets.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }

                    match EdgeTick::try_from_slice(&buf) {
                        Ok(tick) => {
                            self.stats.packets_received.fetch_add(1, Ordering::Relaxed);

                            // Update heartbeat time
                            if tick.is_heartbeat() {
                                *self.last_heartbeat.write() = Instant::now();
                                self.stats.heartbeats_received.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }

                            // Also update heartbeat on any valid packet
                            *self.last_heartbeat.write() = Instant::now();

                            // Check sequence
                            match seq_tracker.check(tick.seq) {
                                SequenceStatus::Ok => {
                                    self.deliver_tick(tick, recv_ns);
                                }
                                SequenceStatus::Duplicate => {
                                    self.stats.duplicates.fetch_add(1, Ordering::Relaxed);
                                }
                                SequenceStatus::Gap { missing } => {
                                    self.stats.gaps_detected.fetch_add(1, Ordering::Relaxed);
                                    self.stats.reorder_events.fetch_add(1, Ordering::Relaxed);
                                    
                                    // Buffer for potential reorder
                                    reorder_buffer.insert(tick);
                                    
                                    debug!(
                                        "Gap detected: {} missing packets, buffer size: {}",
                                        missing,
                                        reorder_buffer.len()
                                    );
                                }
                            }

                            // Drain ready items from reorder buffer
                            let ready = reorder_buffer.drain_ready(seq_tracker.last_seq + 1);
                            for t in ready {
                                self.deliver_tick(t, recv_ns);
                                self.stats.timeout_deliveries.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(EdgeWireError::ChecksumMismatch) => {
                            self.stats.checksum_errors.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            self.stats.malformed_packets.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Timeout, check for stale reorder buffer entries
                    let ready = reorder_buffer.drain_ready(seq_tracker.last_seq + 1);
                    for t in ready {
                        self.deliver_tick(t, self.now_ns());
                        self.stats.timeout_deliveries.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    debug!("Recv error: {}", e);
                }
            }
        }

        info!("Edge receiver client stopped");
    }

    /// Deliver a tick to consumers
    fn deliver_tick(&self, tick: EdgeTick, local_recv_ns: u64) {
        let symbol = tick.symbol();

        // Update latest
        {
            let mut latest = self.latest_ticks.write();
            latest.insert(symbol, SymbolSnapshot { tick, local_recv_ns });
        }

        self.stats.packets_delivered.fetch_add(1, Ordering::Relaxed);

        // Call callback if set
        if let Some(callback) = self.tick_callback.read().as_ref() {
            callback(tick);
        }
    }
}

impl Drop for EdgeReceiverClient {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Fallback controller for switching between edge and direct Binance
pub struct EdgeFallbackController {
    client: Arc<EdgeReceiverClient>,
    fallback_active: AtomicBool,
    last_check: RwLock<Instant>,
    check_interval: Duration,
}

impl EdgeFallbackController {
    pub fn new(client: Arc<EdgeReceiverClient>) -> Self {
        Self {
            client,
            fallback_active: AtomicBool::new(false),
            last_check: RwLock::new(Instant::now()),
            check_interval: Duration::from_secs(1),
        }
    }

    /// Check if we should use fallback (direct Binance connection)
    pub fn should_fallback(&self) -> bool {
        let now = Instant::now();
        let mut last = self.last_check.write();

        if now.duration_since(*last) < self.check_interval {
            return self.fallback_active.load(Ordering::Relaxed);
        }

        *last = now;

        let should_fallback = self.client.is_stale();

        if should_fallback && !self.fallback_active.load(Ordering::Relaxed) {
            warn!(
                "Edge heartbeat timeout ({:?}), activating fallback",
                self.client.heartbeat_age()
            );
            self.fallback_active.store(true, Ordering::Relaxed);
        } else if !should_fallback && self.fallback_active.load(Ordering::Relaxed) {
            info!("Edge recovered, deactivating fallback");
            self.fallback_active.store(false, Ordering::Relaxed);
        }

        should_fallback
    }

    /// Is fallback currently active?
    pub fn is_fallback_active(&self) -> bool {
        self.fallback_active.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_tracker() {
        let mut tracker = SequenceTracker::new();

        assert_eq!(tracker.check(1), SequenceStatus::Ok);
        assert_eq!(tracker.check(2), SequenceStatus::Ok);
        assert_eq!(tracker.check(3), SequenceStatus::Ok);

        // Duplicate
        assert_eq!(tracker.check(2), SequenceStatus::Duplicate);

        // Gap
        assert_eq!(tracker.check(6), SequenceStatus::Gap { missing: 2 });

        let (gaps, dups, missing) = tracker.stats();
        assert_eq!(gaps, 1);
        assert_eq!(dups, 1);
        assert_eq!(missing, 2);
    }

    #[test]
    fn test_reorder_buffer() {
        let mut buffer = ReorderBuffer::new(16, Duration::from_millis(5));

        // Insert out of order
        let tick3 = EdgeTick::new(SymbolId::BtcUsdt, 3, 0, 0, 100.0, 101.0, 1.0, 1.0, 1);
        let tick1 = EdgeTick::new(SymbolId::BtcUsdt, 1, 0, 0, 100.0, 101.0, 1.0, 1.0, 1);
        let tick2 = EdgeTick::new(SymbolId::BtcUsdt, 2, 0, 0, 100.0, 101.0, 1.0, 1.0, 1);

        buffer.insert(tick3);
        buffer.insert(tick1);
        buffer.insert(tick2);

        // Should drain in order
        let ready = buffer.drain_ready(1);
        assert_eq!(ready.len(), 3);
        assert_eq!(ready[0].seq, 1);
        assert_eq!(ready[1].seq, 2);
        assert_eq!(ready[2].seq, 3);
    }
}
