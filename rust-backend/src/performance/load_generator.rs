//! Synthetic load generator for validating performance dashboard
//!
//! Generates realistic latency patterns to test the metrics pipeline.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, sleep};

use crate::latency::{global_registry, LatencySpan, SpanType};
use crate::performance::venue::global_venue_tracker;

/// Load generator configuration
#[derive(Debug, Clone)]
pub struct LoadGenConfig {
    /// Target events per second
    pub events_per_sec: u32,
    /// Enable tick simulation
    pub simulate_ticks: bool,
    /// Enable signal simulation
    pub simulate_signals: bool,
    /// Enable order simulation
    pub simulate_orders: bool,
    /// Add artificial jitter (for testing tail latency alarms)
    pub inject_jitter: bool,
    /// Jitter spike probability (0.0-1.0)
    pub jitter_probability: f64,
    /// Base latency for ticks (μs)
    pub tick_base_us: u64,
    /// Base latency for signals (μs)
    pub signal_base_us: u64,
    /// Base latency for orders (μs)
    pub order_base_us: u64,
}

impl Default for LoadGenConfig {
    fn default() -> Self {
        Self {
            events_per_sec: 1000,
            simulate_ticks: true,
            simulate_signals: true,
            simulate_orders: true,
            inject_jitter: false,
            jitter_probability: 0.001, // 0.1% chance of spike
            tick_base_us: 50,
            signal_base_us: 100,
            order_base_us: 500,
        }
    }
}

/// Synthetic load generator
pub struct LoadGenerator {
    config: LoadGenConfig,
    running: Arc<AtomicBool>,
}

impl LoadGenerator {
    pub fn new(config: LoadGenConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the load generator
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let running = self.running.clone();
        running.store(true, Ordering::SeqCst);

        tokio::spawn(async move {
            let interval_us = 1_000_000 / config.events_per_sec as u64;
            let mut ticker = interval(Duration::from_micros(interval_us));
            let mut rng = StdRng::from_entropy();
            let registry = global_registry();
            let venues = global_venue_tracker();

            tracing::info!(
                "Load generator started: {} events/sec, interval={}μs",
                config.events_per_sec,
                interval_us
            );

            while running.load(Ordering::SeqCst) {
                ticker.tick().await;

                // Generate tick event
                if config.simulate_ticks {
                    let latency = generate_latency(&mut rng, config.tick_base_us, &config);
                    registry.record_span(LatencySpan::new(SpanType::BinanceWs, latency));
                }

                // Generate signal event (less frequent)
                if config.simulate_signals && rng.gen_ratio(1, 10) {
                    let latency = generate_latency(&mut rng, config.signal_base_us, &config);
                    registry.record_span(LatencySpan::new(SpanType::SignalDetection, latency));
                }

                // Generate order event (even less frequent)
                if config.simulate_orders && rng.gen_ratio(1, 100) {
                    let latency = generate_latency(&mut rng, config.order_base_us, &config);
                    registry.record_span(LatencySpan::new(SpanType::Fast15mOrder, latency));

                    // Venue metrics
                    let ack_latency = generate_latency(&mut rng, 10_000, &config); // ~10ms
                    venues.record_send_ack("polymarket", ack_latency);

                    // Simulate fill (~80% of orders)
                    if rng.gen_ratio(4, 5) {
                        let fill_latency = generate_latency(&mut rng, 50_000, &config); // ~50ms
                        venues.record_send_fill("polymarket", fill_latency);
                    }
                }
            }

            tracing::info!("Load generator stopped");
        })
    }

    /// Stop the load generator
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// Generate a latency value with optional jitter
fn generate_latency(rng: &mut impl Rng, base_us: u64, config: &LoadGenConfig) -> u64 {
    // Log-normal distribution for realistic latencies
    let multiplier: f64 = rng.gen_range(0.5..2.0);
    let mut latency = (base_us as f64 * multiplier) as u64;

    // Inject occasional spikes
    if config.inject_jitter && rng.gen_bool(config.jitter_probability) {
        latency *= rng.gen_range(10..100); // 10x-100x spike
    }

    latency.max(1)
}

/// Run a short burst for testing
pub async fn run_burst(events: u32, interval_ms: u64) {
    let registry = global_registry();
    let mut rng = StdRng::from_entropy();

    tracing::info!(
        "Running burst: {} events, {}ms interval",
        events,
        interval_ms
    );

    for i in 0..events {
        // Tick
        let tick_lat = rng.gen_range(10..200);
        registry.record_span(LatencySpan::new(SpanType::BinanceWs, tick_lat));

        // Signal (every 10th)
        if i % 10 == 0 {
            let sig_lat = rng.gen_range(50..500);
            registry.record_span(LatencySpan::new(SpanType::SignalDetection, sig_lat));
        }

        // Order (every 100th)
        if i % 100 == 0 {
            let order_lat = rng.gen_range(200..2000);
            registry.record_span(LatencySpan::new(SpanType::Fast15mOrder, order_lat));
        }

        sleep(Duration::from_millis(interval_ms)).await;
    }

    tracing::info!("Burst complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_burst() {
        run_burst(100, 1).await;

        let summary = global_registry().summary();
        assert!(summary.market_data.binance_ws.count >= 100);
    }
}
