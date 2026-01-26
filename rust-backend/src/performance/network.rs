//! Network statistics collection
//!
//! Monitor NIC drops, errors, socket queue sizes, and retransmits.

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Network statistics collector
pub struct NetworkStats {
    inner: RwLock<NetworkStatsInner>,
    start_time: Instant,
}

struct NetworkStatsInner {
    interfaces: HashMap<String, InterfaceStats>,
    tcp_stats: TcpStats,
    socket_stats: HashMap<String, SocketStats>,
}

impl Default for NetworkStats {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkStats {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(NetworkStatsInner {
                interfaces: HashMap::new(),
                tcp_stats: TcpStats::default(),
                socket_stats: HashMap::new(),
            }),
            start_time: Instant::now(),
        }
    }

    /// Register an interface for monitoring
    pub fn register_interface(&self, name: impl Into<String>) {
        let name = name.into();
        let mut inner = self.inner.write();
        inner
            .interfaces
            .entry(name.clone())
            .or_insert_with(|| InterfaceStats::new(name));
    }

    /// Update interface stats (call periodically from /proc/net/dev reader)
    pub fn update_interface(
        &self,
        name: &str,
        rx_packets: u64,
        tx_packets: u64,
        rx_bytes: u64,
        tx_bytes: u64,
        rx_dropped: u64,
        tx_dropped: u64,
        rx_errors: u64,
        tx_errors: u64,
    ) {
        let mut inner = self.inner.write();
        if let Some(iface) = inner.interfaces.get_mut(name) {
            iface.update(
                rx_packets, tx_packets, rx_bytes, tx_bytes, rx_dropped, tx_dropped, rx_errors,
                tx_errors,
            );
        }
    }

    /// Register a socket for monitoring
    pub fn register_socket(&self, name: impl Into<String>, socket_type: SocketType) {
        let name = name.into();
        let mut inner = self.inner.write();
        inner
            .socket_stats
            .entry(name.clone())
            .or_insert_with(|| SocketStats::new(name, socket_type));
    }

    /// Update socket queue sizes
    pub fn update_socket(&self, name: &str, send_queue: usize, recv_queue: usize) {
        let mut inner = self.inner.write();
        if let Some(sock) = inner.socket_stats.get_mut(name) {
            sock.update_queues(send_queue, recv_queue);
        }
    }

    /// Update TCP stats (from /proc/net/snmp or equivalent)
    pub fn update_tcp(&self, retransmits: u64, in_segs: u64, out_segs: u64) {
        let mut inner = self.inner.write();
        inner.tcp_stats.update(retransmits, in_segs, out_segs);
    }

    /// Get snapshot of all network stats
    pub fn snapshot(&self) -> NetworkSnapshot {
        let inner = self.inner.read();
        let uptime = self.start_time.elapsed().as_secs_f64();

        NetworkSnapshot {
            uptime_secs: uptime,
            interfaces: inner
                .interfaces
                .values()
                .map(|i| i.snapshot(uptime))
                .collect(),
            tcp: inner.tcp_stats.snapshot(uptime),
            sockets: inner.socket_stats.values().map(|s| s.snapshot()).collect(),
        }
    }

    /// Refresh stats from system (Linux-specific)
    #[cfg(target_os = "linux")]
    pub fn refresh_from_system(&self) {
        self.refresh_interfaces();
        self.refresh_tcp();
    }

    #[cfg(target_os = "linux")]
    fn refresh_interfaces(&self) {
        use std::fs::read_to_string;
        if let Ok(contents) = read_to_string("/proc/net/dev") {
            for line in contents.lines().skip(2) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 17 {
                    let name = parts[0].trim_end_matches(':');
                    let rx_bytes: u64 = parts[1].parse().unwrap_or(0);
                    let rx_packets: u64 = parts[2].parse().unwrap_or(0);
                    let rx_errors: u64 = parts[3].parse().unwrap_or(0);
                    let rx_dropped: u64 = parts[4].parse().unwrap_or(0);
                    let tx_bytes: u64 = parts[9].parse().unwrap_or(0);
                    let tx_packets: u64 = parts[10].parse().unwrap_or(0);
                    let tx_errors: u64 = parts[11].parse().unwrap_or(0);
                    let tx_dropped: u64 = parts[12].parse().unwrap_or(0);

                    self.register_interface(name);
                    self.update_interface(
                        name, rx_packets, tx_packets, rx_bytes, tx_bytes, rx_dropped, tx_dropped,
                        rx_errors, tx_errors,
                    );
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn refresh_tcp(&self) {
        use std::fs::read_to_string;
        if let Ok(contents) = read_to_string("/proc/net/snmp") {
            let lines: Vec<&str> = contents.lines().collect();
            for i in 0..lines.len() {
                if lines[i].starts_with("Tcp:") && i + 1 < lines.len() {
                    let values: Vec<&str> = lines[i + 1].split_whitespace().collect();
                    if values.len() >= 15 {
                        let in_segs: u64 = values[10].parse().unwrap_or(0);
                        let out_segs: u64 = values[11].parse().unwrap_or(0);
                        let retrans: u64 = values[12].parse().unwrap_or(0);
                        self.update_tcp(retrans, in_segs, out_segs);
                    }
                    break;
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn refresh_from_system(&self) {
        // macOS/Windows: Use sysinfo or platform-specific APIs
        // For now, stats remain at 0
    }
}

/// Per-interface statistics
struct InterfaceStats {
    name: String,
    rx_packets: AtomicU64,
    tx_packets: AtomicU64,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    rx_dropped: AtomicU64,
    tx_dropped: AtomicU64,
    rx_errors: AtomicU64,
    tx_errors: AtomicU64,
    last_rx_packets: AtomicU64,
    last_tx_packets: AtomicU64,
}

impl InterfaceStats {
    fn new(name: String) -> Self {
        Self {
            name,
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            rx_dropped: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
            rx_errors: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            last_rx_packets: AtomicU64::new(0),
            last_tx_packets: AtomicU64::new(0),
        }
    }

    fn update(
        &self,
        rx_packets: u64,
        tx_packets: u64,
        rx_bytes: u64,
        tx_bytes: u64,
        rx_dropped: u64,
        tx_dropped: u64,
        rx_errors: u64,
        tx_errors: u64,
    ) {
        self.last_rx_packets
            .store(self.rx_packets.load(Ordering::Relaxed), Ordering::Relaxed);
        self.last_tx_packets
            .store(self.tx_packets.load(Ordering::Relaxed), Ordering::Relaxed);
        self.rx_packets.store(rx_packets, Ordering::Relaxed);
        self.tx_packets.store(tx_packets, Ordering::Relaxed);
        self.rx_bytes.store(rx_bytes, Ordering::Relaxed);
        self.tx_bytes.store(tx_bytes, Ordering::Relaxed);
        self.rx_dropped.store(rx_dropped, Ordering::Relaxed);
        self.tx_dropped.store(tx_dropped, Ordering::Relaxed);
        self.rx_errors.store(rx_errors, Ordering::Relaxed);
        self.tx_errors.store(tx_errors, Ordering::Relaxed);
    }

    fn snapshot(&self, uptime: f64) -> InterfaceSnapshot {
        let rx_packets = self.rx_packets.load(Ordering::Relaxed);
        let tx_packets = self.tx_packets.load(Ordering::Relaxed);
        let rx_dropped = self.rx_dropped.load(Ordering::Relaxed);
        let tx_dropped = self.tx_dropped.load(Ordering::Relaxed);

        InterfaceSnapshot {
            name: self.name.clone(),
            rx_packets,
            tx_packets,
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_dropped,
            tx_dropped,
            rx_errors: self.rx_errors.load(Ordering::Relaxed),
            tx_errors: self.tx_errors.load(Ordering::Relaxed),
            rx_drop_rate_pct: if rx_packets > 0 {
                (rx_dropped as f64 / rx_packets as f64) * 100.0
            } else {
                0.0
            },
            tx_drop_rate_pct: if tx_packets > 0 {
                (tx_dropped as f64 / tx_packets as f64) * 100.0
            } else {
                0.0
            },
            rx_pps: rx_packets as f64 / uptime,
            tx_pps: tx_packets as f64 / uptime,
        }
    }
}

/// TCP statistics
#[derive(Default)]
struct TcpStats {
    retransmits: AtomicU64,
    in_segs: AtomicU64,
    out_segs: AtomicU64,
    last_retransmits: AtomicU64,
}

impl TcpStats {
    fn update(&self, retransmits: u64, in_segs: u64, out_segs: u64) {
        self.last_retransmits
            .store(self.retransmits.load(Ordering::Relaxed), Ordering::Relaxed);
        self.retransmits.store(retransmits, Ordering::Relaxed);
        self.in_segs.store(in_segs, Ordering::Relaxed);
        self.out_segs.store(out_segs, Ordering::Relaxed);
    }

    fn snapshot(&self, uptime: f64) -> TcpSnapshot {
        let retransmits = self.retransmits.load(Ordering::Relaxed);
        let out_segs = self.out_segs.load(Ordering::Relaxed);

        TcpSnapshot {
            retransmits,
            in_segs: self.in_segs.load(Ordering::Relaxed),
            out_segs,
            retransmit_rate_pct: if out_segs > 0 {
                (retransmits as f64 / out_segs as f64) * 100.0
            } else {
                0.0
            },
            segs_per_sec: out_segs as f64 / uptime,
        }
    }
}

/// Socket type
#[derive(Debug, Clone, Copy, Serialize)]
pub enum SocketType {
    Tcp,
    WebSocket,
    Udp,
}

/// Per-socket statistics
struct SocketStats {
    name: String,
    socket_type: SocketType,
    send_queue_max: AtomicU64,
    recv_queue_max: AtomicU64,
    send_queue_current: AtomicU64,
    recv_queue_current: AtomicU64,
}

impl SocketStats {
    fn new(name: String, socket_type: SocketType) -> Self {
        Self {
            name,
            socket_type,
            send_queue_max: AtomicU64::new(0),
            recv_queue_max: AtomicU64::new(0),
            send_queue_current: AtomicU64::new(0),
            recv_queue_current: AtomicU64::new(0),
        }
    }

    fn update_queues(&self, send_queue: usize, recv_queue: usize) {
        let send = send_queue as u64;
        let recv = recv_queue as u64;
        self.send_queue_current.store(send, Ordering::Relaxed);
        self.recv_queue_current.store(recv, Ordering::Relaxed);
        self.send_queue_max.fetch_max(send, Ordering::Relaxed);
        self.recv_queue_max.fetch_max(recv, Ordering::Relaxed);
    }

    fn snapshot(&self) -> SocketSnapshot {
        SocketSnapshot {
            name: self.name.clone(),
            socket_type: self.socket_type,
            send_queue: self.send_queue_current.load(Ordering::Relaxed),
            recv_queue: self.recv_queue_current.load(Ordering::Relaxed),
            send_queue_max: self.send_queue_max.load(Ordering::Relaxed),
            recv_queue_max: self.recv_queue_max.load(Ordering::Relaxed),
        }
    }
}

/// Network stats snapshot
#[derive(Debug, Clone, Serialize)]
pub struct NetworkSnapshot {
    pub uptime_secs: f64,
    pub interfaces: Vec<InterfaceSnapshot>,
    pub tcp: TcpSnapshot,
    pub sockets: Vec<SocketSnapshot>,
}

/// Interface stats snapshot
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceSnapshot {
    pub name: String,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_drop_rate_pct: f64,
    pub tx_drop_rate_pct: f64,
    pub rx_pps: f64,
    pub tx_pps: f64,
}

/// TCP stats snapshot
#[derive(Debug, Clone, Serialize)]
pub struct TcpSnapshot {
    pub retransmits: u64,
    pub in_segs: u64,
    pub out_segs: u64,
    pub retransmit_rate_pct: f64,
    pub segs_per_sec: f64,
}

/// Socket stats snapshot
#[derive(Debug, Clone, Serialize)]
pub struct SocketSnapshot {
    pub name: String,
    pub socket_type: SocketType,
    pub send_queue: u64,
    pub recv_queue: u64,
    pub send_queue_max: u64,
    pub recv_queue_max: u64,
}

/// Global network stats instance
pub fn global_network_stats() -> &'static NetworkStats {
    static STATS: std::sync::OnceLock<NetworkStats> = std::sync::OnceLock::new();
    STATS.get_or_init(NetworkStats::new)
}
