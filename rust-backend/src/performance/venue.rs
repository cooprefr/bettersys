//! Venue round-trip latency tracking
//!
//! Track per-venue latencies: send->ack, send->fill, cancel->ack

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;

use crate::latency::LatencyHistogram;

/// Venue latency tracker
pub struct VenueLatencyTracker {
    venues: RwLock<HashMap<String, VenueMetrics>>,
}

impl Default for VenueLatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl VenueLatencyTracker {
    pub fn new() -> Self {
        Self {
            venues: RwLock::new(HashMap::new()),
        }
    }

    /// Register a venue for tracking
    pub fn register_venue(&self, venue: impl Into<String>) {
        let venue = venue.into();
        let mut venues = self.venues.write();
        venues
            .entry(venue.clone())
            .or_insert_with(|| VenueMetrics::new(venue));
    }

    /// Record order send->ack latency
    pub fn record_send_ack(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(metrics) = self.venues.write().get_mut(venue) {
            metrics.send_to_ack.record(latency_us);
            metrics.total_orders += 1;
        }
    }

    /// Record order send->fill latency
    pub fn record_send_fill(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(metrics) = self.venues.write().get_mut(venue) {
            metrics.send_to_fill.record(latency_us);
            metrics.total_fills += 1;
        }
    }

    /// Record cancel->ack latency
    pub fn record_cancel_ack(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(metrics) = self.venues.write().get_mut(venue) {
            metrics.cancel_to_ack.record(latency_us);
            metrics.total_cancels += 1;
        }
    }

    /// Record a rejected order
    pub fn record_reject(&self, venue: &str) {
        self.ensure_venue(venue);
        if let Some(metrics) = self.venues.write().get_mut(venue) {
            metrics.total_rejects += 1;
        }
    }

    /// Record connection latency (connect time)
    pub fn record_connect(&self, venue: &str, latency_us: u64) {
        self.ensure_venue(venue);
        if let Some(metrics) = self.venues.write().get_mut(venue) {
            metrics.connect_latency.record(latency_us);
        }
    }

    fn ensure_venue(&self, venue: &str) {
        let mut venues = self.venues.write();
        if !venues.contains_key(venue) {
            venues.insert(venue.to_string(), VenueMetrics::new(venue.to_string()));
        }
    }

    /// Get snapshot of all venue metrics
    pub fn snapshot(&self) -> Vec<VenueSnapshot> {
        self.venues.read().values().map(|m| m.snapshot()).collect()
    }

    /// Get snapshot for a specific venue
    pub fn get(&self, venue: &str) -> Option<VenueSnapshot> {
        self.venues.read().get(venue).map(|m| m.snapshot())
    }

    /// Get aggregated stats across all venues
    pub fn aggregate(&self) -> AggregateVenueStats {
        let venues = self.venues.read();
        let snapshots: Vec<VenueSnapshot> = venues.values().map(|m| m.snapshot()).collect();

        if snapshots.is_empty() {
            return AggregateVenueStats::default();
        }

        AggregateVenueStats {
            total_orders: snapshots.iter().map(|s| s.total_orders).sum(),
            total_fills: snapshots.iter().map(|s| s.total_fills).sum(),
            total_cancels: snapshots.iter().map(|s| s.total_cancels).sum(),
            total_rejects: snapshots.iter().map(|s| s.total_rejects).sum(),
            avg_send_ack_p99_us: snapshots.iter().map(|s| s.send_ack_p99_us).sum::<u64>()
                / snapshots.len() as u64,
            avg_send_fill_p99_us: snapshots.iter().map(|s| s.send_fill_p99_us).sum::<u64>()
                / snapshots.len() as u64,
            avg_cancel_ack_p99_us: snapshots.iter().map(|s| s.cancel_ack_p99_us).sum::<u64>()
                / snapshots.len() as u64,
            fill_rate_pct: {
                let total_orders: u64 = snapshots.iter().map(|s| s.total_orders).sum();
                let total_fills: u64 = snapshots.iter().map(|s| s.total_fills).sum();
                if total_orders > 0 {
                    (total_fills as f64 / total_orders as f64) * 100.0
                } else {
                    0.0
                }
            },
            venue_count: snapshots.len(),
        }
    }
}

/// Metrics for a single venue
struct VenueMetrics {
    name: String,
    send_to_ack: LatencyHistogram,
    send_to_fill: LatencyHistogram,
    cancel_to_ack: LatencyHistogram,
    connect_latency: LatencyHistogram,
    total_orders: u64,
    total_fills: u64,
    total_cancels: u64,
    total_rejects: u64,
}

impl VenueMetrics {
    fn new(name: String) -> Self {
        Self {
            name,
            send_to_ack: LatencyHistogram::new(),
            send_to_fill: LatencyHistogram::new(),
            cancel_to_ack: LatencyHistogram::new(),
            connect_latency: LatencyHistogram::new(),
            total_orders: 0,
            total_fills: 0,
            total_cancels: 0,
            total_rejects: 0,
        }
    }

    fn snapshot(&self) -> VenueSnapshot {
        VenueSnapshot {
            name: self.name.clone(),
            total_orders: self.total_orders,
            total_fills: self.total_fills,
            total_cancels: self.total_cancels,
            total_rejects: self.total_rejects,
            fill_rate_pct: if self.total_orders > 0 {
                (self.total_fills as f64 / self.total_orders as f64) * 100.0
            } else {
                0.0
            },
            reject_rate_pct: if self.total_orders > 0 {
                (self.total_rejects as f64 / self.total_orders as f64) * 100.0
            } else {
                0.0
            },
            send_ack_p50_us: self.send_to_ack.p50(),
            send_ack_p90_us: self.send_to_ack.p90(),
            send_ack_p99_us: self.send_to_ack.p99(),
            send_ack_p999_us: self.send_to_ack.p999(),
            send_fill_p50_us: self.send_to_fill.p50(),
            send_fill_p90_us: self.send_to_fill.p90(),
            send_fill_p99_us: self.send_to_fill.p99(),
            send_fill_p999_us: self.send_to_fill.p999(),
            cancel_ack_p50_us: self.cancel_to_ack.p50(),
            cancel_ack_p90_us: self.cancel_to_ack.p90(),
            cancel_ack_p99_us: self.cancel_to_ack.p99(),
            cancel_ack_p999_us: self.cancel_to_ack.p999(),
            connect_p50_us: self.connect_latency.p50(),
            connect_p99_us: self.connect_latency.p99(),
        }
    }
}

/// Venue stats snapshot
#[derive(Debug, Clone, Serialize)]
pub struct VenueSnapshot {
    pub name: String,
    pub total_orders: u64,
    pub total_fills: u64,
    pub total_cancels: u64,
    pub total_rejects: u64,
    pub fill_rate_pct: f64,
    pub reject_rate_pct: f64,
    // Send->Ack latencies
    pub send_ack_p50_us: u64,
    pub send_ack_p90_us: u64,
    pub send_ack_p99_us: u64,
    pub send_ack_p999_us: u64,
    // Send->Fill latencies
    pub send_fill_p50_us: u64,
    pub send_fill_p90_us: u64,
    pub send_fill_p99_us: u64,
    pub send_fill_p999_us: u64,
    // Cancel->Ack latencies
    pub cancel_ack_p50_us: u64,
    pub cancel_ack_p90_us: u64,
    pub cancel_ack_p99_us: u64,
    pub cancel_ack_p999_us: u64,
    // Connection latency
    pub connect_p50_us: u64,
    pub connect_p99_us: u64,
}

/// Aggregated stats across all venues
#[derive(Debug, Clone, Default, Serialize)]
pub struct AggregateVenueStats {
    pub total_orders: u64,
    pub total_fills: u64,
    pub total_cancels: u64,
    pub total_rejects: u64,
    pub avg_send_ack_p99_us: u64,
    pub avg_send_fill_p99_us: u64,
    pub avg_cancel_ack_p99_us: u64,
    pub fill_rate_pct: f64,
    pub venue_count: usize,
}

/// Global venue tracker
pub fn global_venue_tracker() -> &'static VenueLatencyTracker {
    static TRACKER: std::sync::OnceLock<VenueLatencyTracker> = std::sync::OnceLock::new();
    TRACKER.get_or_init(|| {
        let tracker = VenueLatencyTracker::new();
        // Pre-register known venues
        tracker.register_venue("polymarket");
        tracker.register_venue("binance");
        tracker
    })
}
