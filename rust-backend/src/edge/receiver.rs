//! Edge Receiver - Runs in ap-southeast-1 (Singapore)
//!
//! Connects to Binance WebSocket, parses JSON, and forwards
//! normalized binary packets to the trading engine via UDP.

use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::wire::{EdgeFlags, EdgeTick, SymbolId, EDGE_TICK_SIZE};

/// Configuration for the edge receiver
#[derive(Debug, Clone)]
pub struct EdgeReceiverConfig {
    /// Symbols to subscribe to
    pub symbols: Vec<String>,
    /// Binance WebSocket URL
    pub binance_ws_url: String,
    /// Destination address for forwarding (engine)
    pub forward_addr: SocketAddr,
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    /// Stale threshold (mark data as stale if older than this)
    pub stale_threshold: Duration,
    /// CPU core to pin to (None = no pinning)
    pub pin_to_core: Option<usize>,
}

impl Default for EdgeReceiverConfig {
    fn default() -> Self {
        Self {
            symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
                "XRPUSDT".to_string(),
            ],
            binance_ws_url: "wss://stream.binance.com:9443/ws".to_string(),
            forward_addr: "127.0.0.1:19876".parse().unwrap(),
            heartbeat_interval: Duration::from_millis(100),
            stale_threshold: Duration::from_millis(100),
            pin_to_core: None,
        }
    }
}

/// Per-symbol state for gap detection
struct SymbolState {
    last_update_id: u64,
    last_exchange_ts_ms: i64,
    gap_count: u64,
}

/// Statistics for the edge receiver
#[derive(Debug, Default)]
pub struct EdgeReceiverStats {
    pub messages_received: AtomicU64,
    pub messages_forwarded: AtomicU64,
    pub heartbeats_sent: AtomicU64,
    pub gaps_detected: AtomicU64,
    pub parse_errors: AtomicU64,
    pub send_errors: AtomicU64,
    pub reconnects: AtomicU64,
    pub bytes_received: AtomicU64,
    pub bytes_sent: AtomicU64,
}

impl EdgeReceiverStats {
    pub fn snapshot(&self) -> EdgeReceiverStatsSnapshot {
        EdgeReceiverStatsSnapshot {
            messages_received: self.messages_received.load(Ordering::Relaxed),
            messages_forwarded: self.messages_forwarded.load(Ordering::Relaxed),
            heartbeats_sent: self.heartbeats_sent.load(Ordering::Relaxed),
            gaps_detected: self.gaps_detected.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EdgeReceiverStatsSnapshot {
    pub messages_received: u64,
    pub messages_forwarded: u64,
    pub heartbeats_sent: u64,
    pub gaps_detected: u64,
    pub parse_errors: u64,
    pub send_errors: u64,
    pub reconnects: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// The edge receiver that forwards Binance data
pub struct EdgeReceiver {
    config: EdgeReceiverConfig,
    running: Arc<AtomicBool>,
    seq: AtomicU64,
    stats: Arc<EdgeReceiverStats>,
    symbol_states: RwLock<HashMap<String, SymbolState>>,
    start_instant: Instant,
}

impl EdgeReceiver {
    pub fn new(config: EdgeReceiverConfig) -> Arc<Self> {
        let symbol_states: HashMap<String, SymbolState> = config
            .symbols
            .iter()
            .map(|s| {
                (
                    s.to_uppercase(),
                    SymbolState {
                        last_update_id: 0,
                        last_exchange_ts_ms: 0,
                        gap_count: 0,
                    },
                )
            })
            .collect();

        Arc::new(Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            seq: AtomicU64::new(1),
            stats: Arc::new(EdgeReceiverStats::default()),
            symbol_states: RwLock::new(symbol_states),
            start_instant: Instant::now(),
        })
    }

    /// Get monotonic nanosecond timestamp
    #[inline]
    fn now_ns(&self) -> i64 {
        self.start_instant.elapsed().as_nanos() as i64
    }

    /// Get next sequence number
    #[inline]
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Get stats reference
    pub fn stats(&self) -> &EdgeReceiverStats {
        &self.stats
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Stop the receiver
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Start the receiver (blocking)
    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        self.running.store(true, Ordering::SeqCst);

        // Pin to core if configured
        #[cfg(target_os = "linux")]
        if let Some(core) = self.config.pin_to_core {
            if let Some(core_ids) = core_affinity::get_core_ids() {
                if core < core_ids.len() {
                    core_affinity::set_for_current(core_ids[core]);
                    info!("Pinned to core {}", core);
                }
            }
        }

        // Create UDP socket for forwarding
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_nonblocking(false)?;
        socket.connect(self.config.forward_addr)?;
        info!("Forwarding to {}", self.config.forward_addr);

        // Build subscription URL
        let streams: Vec<String> = self
            .config
            .symbols
            .iter()
            .map(|s| format!("{}@bookTicker", s.to_lowercase()))
            .collect();
        let url = format!(
            "{}/stream?streams={}",
            self.config.binance_ws_url,
            streams.join("/")
        );

        // Reconnect loop
        let mut reconnect_delay = Duration::from_millis(100);

        while self.running.load(Ordering::Relaxed) {
            info!("Connecting to {}", url);

            match connect_async(&url).await {
                Ok((ws_stream, _)) => {
                    reconnect_delay = Duration::from_millis(100);
                    let (mut write, mut read) = ws_stream.split();

                    // Heartbeat task - clone Arc<Self> for the spawned task
                    let heartbeat_self = self.clone();
                    let socket_clone = socket.try_clone()?;

                    let heartbeat_handle = tokio::spawn(async move {
                        let mut interval = tokio::time::interval(heartbeat_self.config.heartbeat_interval);
                        while heartbeat_self.running.load(Ordering::Relaxed) {
                            interval.tick().await;

                            let edge_ts_ns = heartbeat_self.now_ns();
                            let seq = heartbeat_self.next_seq();
                            let hb = EdgeTick::heartbeat(seq, edge_ts_ns);

                            match socket_clone.send(&hb.to_bytes()) {
                                Ok(_) => {
                                    heartbeat_self.stats.heartbeats_sent.fetch_add(1, Ordering::Relaxed);
                                    heartbeat_self.stats
                                        .bytes_sent
                                        .fetch_add(EDGE_TICK_SIZE as u64, Ordering::Relaxed);
                                }
                                Err(e) => {
                                    warn!("Heartbeat send error: {}", e);
                                    heartbeat_self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    });

                    // Message processing loop
                    while self.running.load(Ordering::Relaxed) {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        self.stats.messages_received.fetch_add(1, Ordering::Relaxed);
                                        self.stats.bytes_received.fetch_add(text.len() as u64, Ordering::Relaxed);

                                        if let Some(tick) = self.parse_and_build_tick(&text) {
                                            match socket.send(&tick.to_bytes()) {
                                                Ok(_) => {
                                                    self.stats.messages_forwarded.fetch_add(1, Ordering::Relaxed);
                                                    self.stats.bytes_sent.fetch_add(EDGE_TICK_SIZE as u64, Ordering::Relaxed);
                                                }
                                                Err(e) => {
                                                    debug!("Send error: {}", e);
                                                    self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
                                                }
                                            }
                                        }
                                    }
                                    Some(Ok(Message::Ping(payload))) => {
                                        let _ = write.send(Message::Pong(payload)).await;
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        info!("WebSocket closed by server");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        warn!("WebSocket error: {}", e);
                                        break;
                                    }
                                    None => {
                                        info!("WebSocket stream ended");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    heartbeat_handle.abort();
                }
                Err(e) => {
                    error!("Connection failed: {}", e);
                    self.stats.reconnects.fetch_add(1, Ordering::Relaxed);
                }
            }

            if self.running.load(Ordering::Relaxed) {
                info!("Reconnecting in {:?}...", reconnect_delay);
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(30));
                self.stats.reconnects.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(())
    }

    /// Parse Binance JSON and build EdgeTick
    fn parse_and_build_tick(&self, msg: &str) -> Option<EdgeTick> {
        // Fast manual JSON parsing (no serde in hot path)
        // Format: {"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.00","B":"1.5","a":"50001.00","A":"2.0","u":12345,"T":1234567890123}}

        let data_start = msg.find("\"data\":")?;
        let data_content = &msg[data_start + 7..];

        // Extract symbol
        let s_start = data_content.find("\"s\":\"")?;
        let s_value_start = s_start + 5;
        let s_end = data_content[s_value_start..].find('"')?;
        let symbol_str = &data_content[s_value_start..s_value_start + s_end];
        let symbol = SymbolId::from_str(symbol_str);

        if symbol == SymbolId::Unknown {
            return None;
        }

        // Extract bid price
        let bid = self.extract_quoted_f64(data_content, "\"b\":\"")?;
        let bid_qty = self.extract_quoted_f64(data_content, "\"B\":\"")?;
        let ask = self.extract_quoted_f64(data_content, "\"a\":\"")?;
        let ask_qty = self.extract_quoted_f64(data_content, "\"A\":\"")?;

        // Extract update ID
        let update_id = self.extract_u64(data_content, "\"u\":")?;

        // Extract timestamp (T field, or use E field)
        let timestamp_ms = self
            .extract_i64(data_content, "\"T\":")
            .or_else(|| self.extract_i64(data_content, "\"E\":"))
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

        let edge_ts_ns = self.now_ns();
        let exchange_ts_ns = timestamp_ms * 1_000_000;

        // Check for gaps
        let mut flags = 0u8;
        {
            let mut states = self.symbol_states.write();
            if let Some(state) = states.get_mut(symbol_str) {
                if state.last_update_id > 0 && update_id > state.last_update_id + 1 {
                    flags |= EdgeFlags::GAP_DETECTED;
                    state.gap_count += 1;
                    self.stats.gaps_detected.fetch_add(1, Ordering::Relaxed);
                    debug!(
                        "Gap detected for {}: {} -> {}",
                        symbol_str, state.last_update_id, update_id
                    );
                }
                state.last_update_id = update_id;
                state.last_exchange_ts_ms = timestamp_ms;
            }
        }

        // Check staleness
        let age_ms = (edge_ts_ns / 1_000_000) - timestamp_ms;
        if age_ms > self.config.stale_threshold.as_millis() as i64 {
            flags |= EdgeFlags::STALE;
        }

        let seq = self.next_seq();

        let mut tick = EdgeTick::new(
            symbol,
            seq,
            exchange_ts_ns,
            edge_ts_ns,
            bid,
            ask,
            bid_qty,
            ask_qty,
            update_id,
        );

        if flags != 0 {
            tick.flags = flags;
            tick.checksum = tick.compute_checksum();
        }

        Some(tick)
    }

    /// Extract quoted f64 value from JSON
    #[inline]
    fn extract_quoted_f64(&self, data: &str, prefix: &str) -> Option<f64> {
        let start = data.find(prefix)?;
        let value_start = start + prefix.len();
        let end = data[value_start..].find('"')?;
        data[value_start..value_start + end].parse().ok()
    }

    /// Extract u64 value from JSON
    #[inline]
    fn extract_u64(&self, data: &str, prefix: &str) -> Option<u64> {
        let start = data.find(prefix)?;
        let value_start = start + prefix.len();
        let end = data[value_start..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(data.len() - value_start);
        data[value_start..value_start + end].parse().ok()
    }

    /// Extract i64 value from JSON
    #[inline]
    fn extract_i64(&self, data: &str, prefix: &str) -> Option<i64> {
        let start = data.find(prefix)?;
        let value_start = start + prefix.len();
        let end = data[value_start..]
            .find(|c: char| !c.is_ascii_digit() && c != '-')
            .unwrap_or(data.len() - value_start);
        data[value_start..value_start + end].parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_binance_message() {
        let config = EdgeReceiverConfig::default();
        let receiver = EdgeReceiver::new(config);

        let msg = r#"{"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"50000.12","B":"1.5","a":"50001.34","A":"2.3","u":12345678,"T":1700000000123}}"#;

        let tick = receiver.parse_and_build_tick(msg).unwrap();

        assert_eq!(tick.symbol(), SymbolId::BtcUsdt);
        assert!((tick.bid_f64() - 50000.12).abs() < 0.01);
        assert!((tick.ask_f64() - 50001.34).abs() < 0.01);
        assert!((tick.bid_qty_f64() - 1.5).abs() < 0.01);
        assert_eq!(tick.binance_update_id, 12345678);
    }
}
