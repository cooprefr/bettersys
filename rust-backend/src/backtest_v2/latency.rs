//! Execution Latency and Microstructure Realism
//!
//! Configurable latency distributions with jitter, tail spikes, and queue position modeling.
//! Supports deterministic replay via seeded RNG.

use crate::backtest_v2::clock::Nanos;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Nanoseconds per microsecond.
pub const NS_PER_US: i64 = 1_000;
/// Nanoseconds per millisecond.
pub const NS_PER_MS: i64 = 1_000_000;

/// Latency distribution types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LatencyDistribution {
    /// Fixed latency (deterministic).
    Fixed { latency_ns: Nanos },

    /// Uniform distribution between min and max.
    Uniform { min_ns: Nanos, max_ns: Nanos },

    /// Normal (Gaussian) distribution.
    /// Clamped to [0, max_ns] to prevent negative latencies.
    Normal {
        mean_ns: Nanos,
        std_ns: Nanos,
        max_ns: Nanos,
    },

    /// Log-normal distribution (realistic for network latencies).
    /// mu/sigma are for the underlying normal distribution.
    LogNormal { mu: f64, sigma: f64, max_ns: Nanos },

    /// Exponential distribution (memoryless, good for queue times).
    Exponential { mean_ns: Nanos, max_ns: Nanos },

    /// Mixture distribution with occasional tail spikes.
    WithTailSpikes {
        base: Box<LatencyDistribution>,
        spike_prob: f64,
        spike: Box<LatencyDistribution>,
    },

    /// Gamma distribution (flexible shape for realistic latencies).
    Gamma {
        shape: f64,
        scale_ns: f64,
        max_ns: Nanos,
    },
}

impl Default for LatencyDistribution {
    fn default() -> Self {
        // Default: 1ms mean with 200us std, realistic for co-located systems
        Self::Normal {
            mean_ns: 1 * NS_PER_MS,
            std_ns: 200 * NS_PER_US,
            max_ns: 10 * NS_PER_MS,
        }
    }
}

impl LatencyDistribution {
    /// Sample a latency from this distribution.
    pub fn sample(&self, rng: &mut StdRng) -> Nanos {
        match self {
            Self::Fixed { latency_ns } => *latency_ns,

            Self::Uniform { min_ns, max_ns } => rng.gen_range(*min_ns..=*max_ns),

            Self::Normal {
                mean_ns,
                std_ns,
                max_ns,
            } => {
                let sample = sample_normal(rng, *mean_ns as f64, *std_ns as f64);
                (sample as Nanos).clamp(0, *max_ns)
            }

            Self::LogNormal { mu, sigma, max_ns } => {
                let normal = sample_normal(rng, *mu, *sigma);
                let sample = normal.exp() * NS_PER_US as f64;
                (sample as Nanos).clamp(0, *max_ns)
            }

            Self::Exponential { mean_ns, max_ns } => {
                let u: f64 = rng.gen();
                let sample = -(*mean_ns as f64) * (1.0 - u).ln();
                (sample as Nanos).clamp(0, *max_ns)
            }

            Self::WithTailSpikes {
                base,
                spike_prob,
                spike,
            } => {
                if rng.gen::<f64>() < *spike_prob {
                    spike.sample(rng)
                } else {
                    base.sample(rng)
                }
            }

            Self::Gamma {
                shape,
                scale_ns,
                max_ns,
            } => {
                let sample = sample_gamma(rng, *shape, *scale_ns);
                (sample as Nanos).clamp(0, *max_ns)
            }
        }
    }

    /// Create a realistic market data latency distribution.
    /// Typical: 50-500us with occasional 5ms spikes.
    pub fn market_data_realistic() -> Self {
        Self::WithTailSpikes {
            base: Box::new(Self::LogNormal {
                mu: 5.0, // ~150us median
                sigma: 0.5,
                max_ns: 2 * NS_PER_MS,
            }),
            spike_prob: 0.01, // 1% chance of spike
            spike: Box::new(Self::Uniform {
                min_ns: 2 * NS_PER_MS,
                max_ns: 10 * NS_PER_MS,
            }),
        }
    }

    /// Create a realistic order-to-ack latency distribution.
    /// Typical: 200us-2ms with occasional 20ms spikes.
    pub fn order_ack_realistic() -> Self {
        Self::WithTailSpikes {
            base: Box::new(Self::LogNormal {
                mu: 6.0, // ~400us median
                sigma: 0.6,
                max_ns: 5 * NS_PER_MS,
            }),
            spike_prob: 0.005, // 0.5% chance of spike
            spike: Box::new(Self::Uniform {
                min_ns: 10 * NS_PER_MS,
                max_ns: 50 * NS_PER_MS,
            }),
        }
    }

    /// Create a decision/tick-to-trade latency distribution.
    /// This is internal strategy compute time.
    pub fn decision_realistic() -> Self {
        Self::LogNormal {
            mu: 4.0, // ~50us median
            sigma: 0.8,
            max_ns: 1 * NS_PER_MS,
        }
    }
}

/// Sample from normal distribution using Box-Muller transform.
fn sample_normal(rng: &mut StdRng, mean: f64, std: f64) -> f64 {
    let u1: f64 = rng.gen();
    let u2: f64 = rng.gen();
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos();
    mean + std * z
}

/// Sample from gamma distribution using Marsaglia and Tsang's method.
fn sample_gamma(rng: &mut StdRng, shape: f64, scale: f64) -> f64 {
    if shape < 1.0 {
        // Use Ahrens-Dieter method for shape < 1
        let u: f64 = rng.gen();
        sample_gamma(rng, shape + 1.0, scale) * u.powf(1.0 / shape)
    } else {
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();

        loop {
            let x = sample_normal(rng, 0.0, 1.0);
            let v = (1.0 + c * x).powi(3);

            if v > 0.0 {
                let u: f64 = rng.gen();
                if u < 1.0 - 0.0331 * x.powi(4) || u.ln() < 0.5 * x.powi(2) + d * (1.0 - v + v.ln())
                {
                    return d * v * scale;
                }
            }
        }
    }
}

/// Complete latency model configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyConfig {
    /// Market data feed latency (exchange -> strategy).
    pub market_data: LatencyDistribution,
    /// Strategy decision latency (receive data -> decide to trade).
    pub decision: LatencyDistribution,
    /// Order send latency (strategy -> gateway).
    pub order_send: LatencyDistribution,
    /// Venue processing latency (gateway -> exchange matching).
    pub venue_process: LatencyDistribution,
    /// Cancel processing latency.
    pub cancel_process: LatencyDistribution,
    /// Fill report latency (exchange -> strategy).
    pub fill_report: LatencyDistribution,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            market_data: LatencyDistribution::Fixed {
                latency_ns: 100 * NS_PER_US,
            },
            decision: LatencyDistribution::Fixed {
                latency_ns: 50 * NS_PER_US,
            },
            order_send: LatencyDistribution::Fixed {
                latency_ns: 200 * NS_PER_US,
            },
            venue_process: LatencyDistribution::Fixed {
                latency_ns: 100 * NS_PER_US,
            },
            cancel_process: LatencyDistribution::Fixed {
                latency_ns: 150 * NS_PER_US,
            },
            fill_report: LatencyDistribution::Fixed {
                latency_ns: 100 * NS_PER_US,
            },
        }
    }
}

impl LatencyConfig {
    /// Create a realistic latency configuration.
    pub fn realistic() -> Self {
        Self {
            market_data: LatencyDistribution::market_data_realistic(),
            decision: LatencyDistribution::decision_realistic(),
            order_send: LatencyDistribution::order_ack_realistic(),
            venue_process: LatencyDistribution::LogNormal {
                mu: 5.5,
                sigma: 0.4,
                max_ns: 2 * NS_PER_MS,
            },
            cancel_process: LatencyDistribution::LogNormal {
                mu: 5.8,
                sigma: 0.5,
                max_ns: 3 * NS_PER_MS,
            },
            fill_report: LatencyDistribution::market_data_realistic(),
        }
    }

    /// Create a zero-latency configuration (for debugging).
    pub fn zero() -> Self {
        Self {
            market_data: LatencyDistribution::Fixed { latency_ns: 0 },
            decision: LatencyDistribution::Fixed { latency_ns: 0 },
            order_send: LatencyDistribution::Fixed { latency_ns: 0 },
            venue_process: LatencyDistribution::Fixed { latency_ns: 0 },
            cancel_process: LatencyDistribution::Fixed { latency_ns: 0 },
            fill_report: LatencyDistribution::Fixed { latency_ns: 0 },
        }
    }

    /// Get the expected cancel latency (for race condition checks).
    /// Returns the mean/fixed value depending on distribution type.
    pub fn cancel_latency_ns(&self) -> Nanos {
        match &self.cancel_process {
            LatencyDistribution::Fixed { latency_ns } => *latency_ns,
            LatencyDistribution::Uniform { min_ns, max_ns } => (min_ns + max_ns) / 2,
            LatencyDistribution::Normal { mean_ns, .. } => *mean_ns,
            LatencyDistribution::LogNormal { mu, .. } => (*mu * 1000.0) as Nanos, // Rough approximation
            LatencyDistribution::Exponential { mean_ns, .. } => *mean_ns,
            LatencyDistribution::WithTailSpikes { .. } => 150 * NS_PER_US, // Default estimate
            LatencyDistribution::Gamma { shape, scale_ns, .. } => (shape * scale_ns) as Nanos,
        }
    }
    
    /// Get the expected order latency (strategy decision -> venue arrival).
    /// This is used for QueueProof to estimate when an order would arrive at the venue.
    /// Returns the mean/fixed value for order_send + venue_process.
    pub fn order_latency_ns(&self) -> Nanos {
        let order_send = match &self.order_send {
            LatencyDistribution::Fixed { latency_ns } => *latency_ns,
            LatencyDistribution::Uniform { min_ns, max_ns } => (min_ns + max_ns) / 2,
            LatencyDistribution::Normal { mean_ns, .. } => *mean_ns,
            LatencyDistribution::LogNormal { mu, .. } => (*mu * 1000.0) as Nanos,
            LatencyDistribution::Exponential { mean_ns, .. } => *mean_ns,
            LatencyDistribution::WithTailSpikes { .. } => 200 * NS_PER_US,
            LatencyDistribution::Gamma { shape, scale_ns, .. } => (shape * scale_ns) as Nanos,
        };
        let venue_process = match &self.venue_process {
            LatencyDistribution::Fixed { latency_ns } => *latency_ns,
            LatencyDistribution::Uniform { min_ns, max_ns } => (min_ns + max_ns) / 2,
            LatencyDistribution::Normal { mean_ns, .. } => *mean_ns,
            LatencyDistribution::LogNormal { mu, .. } => (*mu * 1000.0) as Nanos,
            LatencyDistribution::Exponential { mean_ns, .. } => *mean_ns,
            LatencyDistribution::WithTailSpikes { .. } => 100 * NS_PER_US,
            LatencyDistribution::Gamma { shape, scale_ns, .. } => (shape * scale_ns) as Nanos,
        };
        order_send + venue_process
    }
}

/// Latency sampler with seeded RNG for deterministic replay.
pub struct LatencySampler {
    config: LatencyConfig,
    rng: StdRng,
    /// Statistics
    pub stats: LatencyStats,
}

/// Latency statistics.
#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    pub market_data_samples: u64,
    pub market_data_sum_ns: i64,
    pub market_data_max_ns: Nanos,
    pub decision_samples: u64,
    pub decision_sum_ns: i64,
    pub order_send_samples: u64,
    pub order_send_sum_ns: i64,
    pub venue_process_samples: u64,
    pub venue_process_sum_ns: i64,
    pub cancel_samples: u64,
    pub cancel_sum_ns: i64,
    pub fill_report_samples: u64,
    pub fill_report_sum_ns: i64,
}

impl LatencyStats {
    pub fn avg_market_data_ns(&self) -> f64 {
        if self.market_data_samples > 0 {
            self.market_data_sum_ns as f64 / self.market_data_samples as f64
        } else {
            0.0
        }
    }

    pub fn avg_tick_to_trade_ns(&self) -> f64 {
        let samples = self.venue_process_samples.max(1) as f64;
        (self.market_data_sum_ns
            + self.decision_sum_ns
            + self.order_send_sum_ns
            + self.venue_process_sum_ns) as f64
            / samples
    }
}

impl LatencySampler {
    pub fn new(config: LatencyConfig, seed: u64) -> Self {
        Self {
            config,
            rng: StdRng::seed_from_u64(seed),
            stats: LatencyStats::default(),
        }
    }

    /// Sample market data latency.
    pub fn sample_market_data(&mut self) -> Nanos {
        let latency = self.config.market_data.sample(&mut self.rng);
        self.stats.market_data_samples += 1;
        self.stats.market_data_sum_ns += latency;
        self.stats.market_data_max_ns = self.stats.market_data_max_ns.max(latency);
        latency
    }

    /// Sample decision latency.
    pub fn sample_decision(&mut self) -> Nanos {
        let latency = self.config.decision.sample(&mut self.rng);
        self.stats.decision_samples += 1;
        self.stats.decision_sum_ns += latency;
        latency
    }

    /// Sample order send latency.
    pub fn sample_order_send(&mut self) -> Nanos {
        let latency = self.config.order_send.sample(&mut self.rng);
        self.stats.order_send_samples += 1;
        self.stats.order_send_sum_ns += latency;
        latency
    }

    /// Sample venue processing latency.
    pub fn sample_venue_process(&mut self) -> Nanos {
        let latency = self.config.venue_process.sample(&mut self.rng);
        self.stats.venue_process_samples += 1;
        self.stats.venue_process_sum_ns += latency;
        latency
    }

    /// Sample cancel processing latency.
    pub fn sample_cancel(&mut self) -> Nanos {
        let latency = self.config.cancel_process.sample(&mut self.rng);
        self.stats.cancel_samples += 1;
        self.stats.cancel_sum_ns += latency;
        latency
    }

    /// Sample fill report latency.
    pub fn sample_fill_report(&mut self) -> Nanos {
        let latency = self.config.fill_report.sample(&mut self.rng);
        self.stats.fill_report_samples += 1;
        self.stats.fill_report_sum_ns += latency;
        latency
    }

    /// Total tick-to-trade latency (all components).
    pub fn sample_tick_to_trade(&mut self) -> Nanos {
        self.sample_market_data()
            + self.sample_decision()
            + self.sample_order_send()
            + self.sample_venue_process()
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.stats = LatencyStats::default();
    }

    /// Reseed RNG (for reproducibility).
    pub fn reseed(&mut self, seed: u64) {
        self.rng = StdRng::seed_from_u64(seed);
    }

    /// Access to RNG for other uses.
    pub fn rng(&mut self) -> &mut StdRng {
        &mut self.rng
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_latency() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = LatencyDistribution::Fixed { latency_ns: 1000 };

        for _ in 0..100 {
            assert_eq!(dist.sample(&mut rng), 1000);
        }
    }

    #[test]
    fn test_uniform_latency() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = LatencyDistribution::Uniform {
            min_ns: 100,
            max_ns: 200,
        };

        for _ in 0..100 {
            let sample = dist.sample(&mut rng);
            assert!(sample >= 100 && sample <= 200);
        }
    }

    #[test]
    fn test_normal_latency_clamped() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = LatencyDistribution::Normal {
            mean_ns: 1000,
            std_ns: 500,
            max_ns: 5000,
        };

        for _ in 0..100 {
            let sample = dist.sample(&mut rng);
            assert!(sample >= 0 && sample <= 5000);
        }
    }

    #[test]
    fn test_tail_spikes() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = LatencyDistribution::WithTailSpikes {
            base: Box::new(LatencyDistribution::Fixed { latency_ns: 100 }),
            spike_prob: 0.5, // 50% for testing
            spike: Box::new(LatencyDistribution::Fixed { latency_ns: 10000 }),
        };

        let mut saw_base = false;
        let mut saw_spike = false;

        for _ in 0..100 {
            let sample = dist.sample(&mut rng);
            if sample == 100 {
                saw_base = true;
            }
            if sample == 10000 {
                saw_spike = true;
            }
        }

        assert!(saw_base, "Should see base latency");
        assert!(saw_spike, "Should see spike latency");
    }

    #[test]
    fn test_latency_sampler_deterministic() {
        let config = LatencyConfig::default();

        let mut sampler1 = LatencySampler::new(config.clone(), 42);
        let mut sampler2 = LatencySampler::new(config, 42);

        for _ in 0..100 {
            assert_eq!(
                sampler1.sample_tick_to_trade(),
                sampler2.sample_tick_to_trade()
            );
        }
    }

    #[test]
    fn test_realistic_config() {
        let config = LatencyConfig::realistic();
        let mut sampler = LatencySampler::new(config, 42);

        for _ in 0..1000 {
            let t2t = sampler.sample_tick_to_trade();
            assert!(t2t >= 0);
            assert!(t2t < 100 * NS_PER_MS);
        }

        let avg = sampler.stats.avg_tick_to_trade_ns();
        assert!(avg > 100.0 * NS_PER_US as f64);
        assert!(avg < 50.0 * NS_PER_MS as f64);
    }

    #[test]
    fn test_exponential_distribution() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = LatencyDistribution::Exponential {
            mean_ns: 1000,
            max_ns: 10000,
        };

        let mut sum = 0i64;
        let n = 10000;

        for _ in 0..n {
            let sample = dist.sample(&mut rng);
            assert!(sample >= 0 && sample <= 10000);
            sum += sample;
        }

        // Mean should be close to 1000
        let avg = sum as f64 / n as f64;
        assert!((avg - 1000.0).abs() < 200.0);
    }
}
