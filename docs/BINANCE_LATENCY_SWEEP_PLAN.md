# Binance Market Data Latency Sweep: Experiment Design & Implementation Plan

**Objective:** Minimize p99 WebSocket latency to Binance market data feeds by systematically evaluating AWS regions and instance types.

---

## 1. Experiment Design

### 1.1 Hypothesis

Binance operates primary matching engines in Tokyo (ap-northeast-1) and Singapore (ap-southeast-1), with European connectivity likely routing through Frankfurt or London. We hypothesize:

1. **ap-southeast-1** (Singapore) will have lowest p99 for Binance Spot due to geographic proximity to Binance infrastructure
2. **eu-central-1** (Frankfurt) or **eu-west-2** (London) will be best for European deployment
3. Network-optimized instances (c6in/c7gn) will show 10-20% improvement over general compute

### 1.2 Metrics to Collect

| Metric | Description | Priority |
|--------|-------------|----------|
| **p99_latency_us** | 99th percentile one-way latency (exchange timestamp → local receive) | Primary |
| **p999_latency_us** | 99.9th percentile for tail analysis | Primary |
| **p50_latency_us** | Median latency (baseline) | Secondary |
| **jitter_us** | Standard deviation of latency | Primary |
| **cv** | Coefficient of variation (jitter/mean) | Primary |
| **reconnect_count** | WebSocket reconnection frequency | Secondary |
| **message_loss_rate** | Estimated from sequence gaps | Secondary |
| **tick_rate_hz** | Messages per second received | Diagnostic |

### 1.3 Test Matrix

**Regions (Phase 1 - Europe focus):**
| Region | Code | Rationale |
|--------|------|-----------|
| Ireland | eu-west-1 | Major AWS hub, good peering |
| Frankfurt | eu-central-1 | Financial hub, DE-CIX |
| London | eu-west-2 | Financial hub, LINX |

**Regions (Phase 2 - Global):**
| Region | Code | Rationale |
|--------|------|-----------|
| Singapore | ap-southeast-1 | Likely closest to Binance infra |
| UAE | me-central-1 | Newer region, test latency profile |
| Tokyo | ap-northeast-1 | Alternative Asia PoP |

**Instance Types:**
| Family | Type | vCPU | Network | Rationale |
|--------|------|------|---------|-----------|
| Compute | c6i.large | 2 | Up to 12.5 Gbps | Baseline compute-optimized |
| Network | c6in.large | 2 | Up to 25 Gbps + ENA Express | Enhanced networking |
| Network | c7gn.medium | 1 | Up to 25 Gbps | Graviton3 + enhanced NW |
| Memory | r6i.large | 2 | Up to 12.5 Gbps | Buffer-heavy workload test |

### 1.4 Timing Parameters

```yaml
experiment:
  warmup_duration_sec: 300      # 5 minutes - TCP tuning, JIT, cache warmup
  measurement_duration_sec: 3600 # 1 hour minimum for statistical significance
  cooldown_sec: 60              # Between runs for instance normalization
  
sampling:
  tick_symbols: ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"]
  sample_every_n: 1             # Every tick (L1 orderbook ~100-200ms)
  histogram_buckets: 50         # Log-scale 1μs to 10s
  window_size_sec: 60           # Rolling window for real-time stats

statistical:
  min_samples: 10000            # Minimum for percentile accuracy
  confidence_level: 0.95        # For CI calculation
  outlier_cap_percentile: 99.99 # Cap extreme outliers
```

### 1.5 Time-of-Day Considerations

Run experiments at multiple times to capture volatility variance:
- **Low volatility:** 06:00-08:00 UTC (Asia close, Europe pre-market)
- **High volatility:** 14:00-16:00 UTC (US market open overlap)
- **Peak volume:** 00:00-02:00 UTC (Asia peak)

---

## 2. Acceptance Criteria & Decision Rule

### 2.1 Primary Decision Rule

```
SCORE(region, instance) = w1 * normalized_p99 + w2 * normalized_stability

Where:
  normalized_p99 = (p99 - min_p99) / (max_p99 - min_p99)
  normalized_stability = cv / max_cv  # coefficient of variation
  
Weights:
  w1 = 0.7  # p99 dominates
  w2 = 0.3  # stability matters for consistent execution
```

### 2.2 Hard Constraints (Disqualifying)

| Constraint | Threshold | Rationale |
|------------|-----------|-----------|
| p99 latency | < 50ms | Unusable for HFT if exceeded |
| Reconnect rate | < 1/hour | Stability requirement |
| Message loss | < 0.01% | Data integrity |
| p999/p99 ratio | < 3.0 | Tail behavior bounded |

### 2.3 Soft Preferences

| Factor | Weight | Notes |
|--------|--------|-------|
| Cost efficiency | 0.1 | $/latency-unit improvement |
| Region redundancy | 0.05 | Prefer regions with failover options |
| Graviton availability | 0.05 | ARM cost savings if latency equivalent |

### 2.4 Statistical Significance

Before declaring a winner:
1. **Mann-Whitney U test** between top candidates (p < 0.05)
2. **Bootstrap confidence intervals** for p99 difference
3. **Minimum 3 independent runs** per configuration
4. **Time-stratified analysis** to ensure winner is consistent across volatility regimes

---

## 3. Infrastructure (Terraform)

### 3.1 Directory Structure

```
infra/latency-sweep/
├── terraform/
│   ├── main.tf           # Provider, backend config
│   ├── variables.tf      # Input variables
│   ├── regions.tf        # Multi-region provider aliases
│   ├── modules/
│   │   └── probe/
│   │       ├── main.tf   # EC2 + security group + IAM
│   │       ├── variables.tf
│   │       └── outputs.tf
│   ├── outputs.tf        # Probe endpoints, IDs
│   └── terraform.tfvars.example
├── bootstrap/
│   ├── install.sh        # Rust toolchain + deps
│   └── systemd/
│       └── binance-probe.service
├── probe/
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
└── orchestrate/
    ├── run_sweep.py      # Orchestration script
    └── analyze.py        # Results analysis
```

### 3.2 Terraform Configuration

```hcl
# terraform/main.tf

terraform {
  required_version = ">= 1.5"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
  
  backend "s3" {
    bucket = "betterbot-terraform-state"
    key    = "latency-sweep/terraform.tfstate"
    region = "eu-west-1"
  }
}

locals {
  regions = {
    eu-west-1     = { name = "Ireland",   phase = 1 }
    eu-central-1  = { name = "Frankfurt", phase = 1 }
    eu-west-2     = { name = "London",    phase = 1 }
    ap-southeast-1 = { name = "Singapore", phase = 2 }
    me-central-1  = { name = "UAE",       phase = 2 }
    ap-northeast-1 = { name = "Tokyo",    phase = 2 }
  }
  
  instance_types = {
    c6i_large  = { type = "c6i.large",   family = "compute" }
    c6in_large = { type = "c6in.large",  family = "network" }
    c7gn_medium = { type = "c7gn.medium", family = "network_arm" }
    r6i_large  = { type = "r6i.large",   family = "memory" }
  }
  
  # Phase 1 only
  active_regions = { for k, v in local.regions : k => v if v.phase == var.phase }
  
  # Experiment ID for tagging
  experiment_id = "latency-sweep-${formatdate("YYYYMMDD-hhmm", timestamp())}"
}

variable "phase" {
  type    = number
  default = 1
}

variable "ssh_public_key" {
  type = string
}

variable "instance_types_filter" {
  type    = list(string)
  default = ["c6i_large", "c6in_large"]  # Start with these
}
```

```hcl
# terraform/regions.tf

provider "aws" {
  alias  = "eu-west-1"
  region = "eu-west-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "eu-central-1"
  region = "eu-central-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "eu-west-2"
  region = "eu-west-2"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "ap-southeast-1"
  region = "ap-southeast-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "me-central-1"
  region = "me-central-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "ap-northeast-1"
  region = "ap-northeast-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}
```

```hcl
# terraform/modules/probe/main.tf

variable "region" {
  type = string
}

variable "instance_type" {
  type = string
}

variable "instance_family" {
  type = string
}

variable "ssh_public_key" {
  type = string
}

variable "experiment_id" {
  type = string
}

data "aws_ami" "amazon_linux_2023" {
  most_recent = true
  owners      = ["amazon"]
  
  filter {
    name   = "name"
    values = ["al2023-ami-*-kernel-*"]
  }
  
  filter {
    name   = "architecture"
    values = var.instance_family == "network_arm" ? ["arm64"] : ["x86_64"]
  }
  
  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
}

resource "aws_key_pair" "probe" {
  key_name   = "latency-probe-${var.region}-${var.instance_family}"
  public_key = var.ssh_public_key
}

resource "aws_security_group" "probe" {
  name_prefix = "latency-probe-"
  description = "Latency probe - egress only for Binance WS"
  
  # SSH for management
  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]  # Restrict in production
  }
  
  # Metrics endpoint
  ingress {
    from_port   = 9090
    to_port     = 9090
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]  # Restrict in production
  }
  
  # All egress (Binance WS + package downloads)
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
  
  tags = {
    Name = "latency-probe-${var.region}-${var.instance_family}"
  }
}

resource "aws_instance" "probe" {
  ami           = data.aws_ami.amazon_linux_2023.id
  instance_type = var.instance_type
  key_name      = aws_key_pair.probe.key_name
  
  vpc_security_group_ids = [aws_security_group.probe.id]
  
  # Enable ENA (Enhanced Networking)
  ebs_optimized = true
  
  # Disable source/dest check for network monitoring
  source_dest_check = false
  
  root_block_device {
    volume_type = "gp3"
    volume_size = 20
    iops        = 3000
    throughput  = 125
  }
  
  # User data to install Rust and probe
  user_data = base64encode(templatefile("${path.module}/../../bootstrap/install.sh", {
    experiment_id   = var.experiment_id
    region          = var.region
    instance_family = var.instance_family
  }))
  
  tags = {
    Name           = "latency-probe-${var.region}-${var.instance_family}"
    Region         = var.region
    InstanceFamily = var.instance_family
    ExperimentId   = var.experiment_id
  }
  
  lifecycle {
    create_before_destroy = true
  }
}

# CloudWatch agent for system metrics
resource "aws_iam_role" "probe" {
  name_prefix = "latency-probe-"
  
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action = "sts:AssumeRole"
      Effect = "Allow"
      Principal = {
        Service = "ec2.amazonaws.com"
      }
    }]
  })
}

resource "aws_iam_role_policy_attachment" "cloudwatch" {
  role       = aws_iam_role.probe.name
  policy_arn = "arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy"
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.probe.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "probe" {
  name_prefix = "latency-probe-"
  role        = aws_iam_role.probe.name
}

output "instance_id" {
  value = aws_instance.probe.id
}

output "public_ip" {
  value = aws_instance.probe.public_ip
}

output "private_ip" {
  value = aws_instance.probe.private_ip
}
```

---

## 4. Rust Latency Probe

### 4.1 Cargo.toml

```toml
# infra/latency-sweep/probe/Cargo.toml

[package]
name = "binance-latency-probe"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1.35", features = ["full", "rt-multi-thread"] }

# WebSocket
tokio-tungstenite = { version = "0.21", features = ["rustls-tls-webpki-roots"] }
futures-util = "0.3"

# Binance data (same as main project for consistency)
barter-data = "0.10.2"
barter-instrument = "0.3.1"

# HTTP for metrics endpoint
axum = "0.7"
tower = "0.4"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Time
chrono = { version = "0.4", features = ["serde"] }

# High-precision timing
quanta = "0.12"

# Statistics
statrs = "0.16"

# Fast locks
parking_lot = "0.12"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# CLI
clap = { version = "4.4", features = ["derive"] }

# Error handling
anyhow = "1.0"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
panic = 'abort'
```

### 4.2 Probe Implementation

```rust
// infra/latency-sweep/probe/src/main.rs

use anyhow::{Context, Result};
use axum::{routing::get, Json, Router};
use barter_data::{
    exchange::binance::spot::BinanceSpot,
    streams::{reconnect::Event as ReconnectEvent, Streams},
    subscription::book::OrderBooksL1,
};
use barter_instrument::instrument::market_data::{
    kind::MarketDataInstrumentKind, MarketDataInstrument,
};
use chrono::{DateTime, Utc};
use clap::Parser;
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tracing::{info, warn};

/// Binance Latency Probe for region/instance benchmarking
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Region identifier (for tagging)
    #[arg(long, env = "PROBE_REGION")]
    region: String,
    
    /// Instance family (for tagging)
    #[arg(long, env = "PROBE_INSTANCE_FAMILY")]
    instance_family: String,
    
    /// Experiment ID
    #[arg(long, env = "PROBE_EXPERIMENT_ID")]
    experiment_id: String,
    
    /// Warmup duration in seconds
    #[arg(long, default_value = "300")]
    warmup_sec: u64,
    
    /// Metrics HTTP port
    #[arg(long, default_value = "9090")]
    metrics_port: u16,
    
    /// Symbols to track (comma-separated)
    #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
    symbols: String,
}

// === Latency Histogram (matching existing codebase style) ===

static BUCKET_BOUNDS_US: &[u64] = &[
    1, 2, 5, 10, 20, 50, 100, 200, 500,
    1_000, 2_000, 5_000, 10_000,
    20_000, 50_000, 100_000,
    200_000, 500_000, 1_000_000,
    2_000_000, 5_000_000, 10_000_000,
    u64::MAX,
];

#[derive(Debug)]
struct LatencyHistogram {
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum_us: AtomicU64,
    min_us: AtomicU64,
    max_us: AtomicU64,
    // For variance calculation (Welford's online algorithm)
    m2: RwLock<f64>,
    mean: RwLock<f64>,
}

impl LatencyHistogram {
    fn new() -> Self {
        Self {
            buckets: (0..BUCKET_BOUNDS_US.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
            min_us: AtomicU64::new(u64::MAX),
            max_us: AtomicU64::new(0),
            m2: RwLock::new(0.0),
            mean: RwLock::new(0.0),
        }
    }
    
    #[inline]
    fn record(&self, latency_us: u64) {
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        self.sum_us.fetch_add(latency_us, Ordering::Relaxed);
        
        // Update min/max atomically
        loop {
            let current_min = self.min_us.load(Ordering::Relaxed);
            if latency_us >= current_min {
                break;
            }
            if self.min_us.compare_exchange_weak(
                current_min, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ).is_ok() {
                break;
            }
        }
        
        loop {
            let current_max = self.max_us.load(Ordering::Relaxed);
            if latency_us <= current_max {
                break;
            }
            if self.max_us.compare_exchange_weak(
                current_max, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ).is_ok() {
                break;
            }
        }
        
        // Bucket update
        let idx = BUCKET_BOUNDS_US
            .iter()
            .position(|&bound| latency_us <= bound)
            .unwrap_or(BUCKET_BOUNDS_US.len() - 1);
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        
        // Welford's online variance (under lock for accuracy)
        let x = latency_us as f64;
        let mut mean = self.mean.write();
        let mut m2 = self.m2.write();
        let delta = x - *mean;
        *mean += delta / n as f64;
        let delta2 = x - *mean;
        *m2 += delta * delta2;
    }
    
    fn percentile(&self, p: f64) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }
        
        let target = ((p / 100.0) * count as f64).ceil() as u64;
        let mut cumulative = 0u64;
        
        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return BUCKET_BOUNDS_US[i];
            }
        }
        
        self.max_us.load(Ordering::Relaxed)
    }
    
    fn variance(&self) -> f64 {
        let n = self.count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        *self.m2.read() / (n - 1) as f64
    }
    
    fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }
    
    fn cv(&self) -> f64 {
        let mean = *self.mean.read();
        if mean == 0.0 {
            return 0.0;
        }
        self.std_dev() / mean
    }
    
    fn summary(&self) -> HistogramSummary {
        let count = self.count.load(Ordering::Relaxed);
        HistogramSummary {
            count,
            min_us: if count == 0 { 0 } else { self.min_us.load(Ordering::Relaxed) },
            max_us: self.max_us.load(Ordering::Relaxed),
            mean_us: *self.mean.read(),
            std_dev_us: self.std_dev(),
            cv: self.cv(),
            p50_us: self.percentile(50.0),
            p90_us: self.percentile(90.0),
            p95_us: self.percentile(95.0),
            p99_us: self.percentile(99.0),
            p999_us: self.percentile(99.9),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct HistogramSummary {
    count: u64,
    min_us: u64,
    max_us: u64,
    mean_us: f64,
    std_dev_us: f64,
    cv: f64,  // Coefficient of variation
    p50_us: u64,
    p90_us: u64,
    p95_us: u64,
    p99_us: u64,
    p999_us: u64,
}

// === Probe State ===

struct ProbeState {
    args: Args,
    start_time: Instant,
    warmup_complete: AtomicU64,  // Unix timestamp when warmup completed
    
    // Per-symbol histograms
    histograms: HashMap<String, Arc<LatencyHistogram>>,
    
    // Aggregate histogram
    aggregate: Arc<LatencyHistogram>,
    
    // Counters
    reconnect_count: AtomicU64,
    message_count: AtomicU64,
    error_count: AtomicU64,
    
    // Recent samples for debugging
    recent_samples: RwLock<Vec<LatencySample>>,
}

#[derive(Debug, Clone, Serialize)]
struct LatencySample {
    timestamp: DateTime<Utc>,
    symbol: String,
    exchange_ts_ms: i64,
    receive_ts_ms: i64,
    latency_us: u64,
}

impl ProbeState {
    fn new(args: Args) -> Self {
        let symbols: Vec<String> = args.symbols.split(',').map(|s| s.trim().to_uppercase()).collect();
        let histograms: HashMap<String, Arc<LatencyHistogram>> = symbols
            .iter()
            .map(|s| (s.clone(), Arc::new(LatencyHistogram::new())))
            .collect();
        
        Self {
            args,
            start_time: Instant::now(),
            warmup_complete: AtomicU64::new(0),
            histograms,
            aggregate: Arc::new(LatencyHistogram::new()),
            reconnect_count: AtomicU64::new(0),
            message_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            recent_samples: RwLock::new(Vec::with_capacity(1000)),
        }
    }
    
    fn is_warmed_up(&self) -> bool {
        self.warmup_complete.load(Ordering::Relaxed) > 0
    }
    
    fn mark_warmup_complete(&self) {
        let now = Utc::now().timestamp() as u64;
        self.warmup_complete.store(now, Ordering::Relaxed);
        info!("Warmup complete at {}", now);
    }
    
    fn record_latency(&self, symbol: &str, exchange_ts_ms: i64, receive_ts_ms: i64) {
        self.message_count.fetch_add(1, Ordering::Relaxed);
        
        // Skip recording until warmup complete
        if !self.is_warmed_up() {
            return;
        }
        
        // Calculate one-way latency (exchange → local)
        let latency_us = if receive_ts_ms > exchange_ts_ms {
            ((receive_ts_ms - exchange_ts_ms) * 1000) as u64
        } else {
            // Clock skew - record as 0 but flag
            0
        };
        
        // Record to symbol histogram
        if let Some(hist) = self.histograms.get(symbol) {
            hist.record(latency_us);
        }
        
        // Record to aggregate
        self.aggregate.record(latency_us);
        
        // Store recent sample (capped)
        let sample = LatencySample {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            exchange_ts_ms,
            receive_ts_ms,
            latency_us,
        };
        
        let mut recent = self.recent_samples.write();
        if recent.len() >= 1000 {
            recent.remove(0);
        }
        recent.push(sample);
    }
}

// === Metrics Endpoint ===

#[derive(Serialize)]
struct MetricsResponse {
    probe_info: ProbeInfo,
    aggregate: HistogramSummary,
    per_symbol: HashMap<String, HistogramSummary>,
    counters: Counters,
    recent_samples: Vec<LatencySample>,
    uptime_sec: u64,
    warmup_complete: bool,
}

#[derive(Serialize)]
struct ProbeInfo {
    region: String,
    instance_family: String,
    experiment_id: String,
    timestamp: DateTime<Utc>,
}

#[derive(Serialize)]
struct Counters {
    messages: u64,
    reconnects: u64,
    errors: u64,
}

async fn get_metrics(
    axum::extract::State(state): axum::extract::State<Arc<ProbeState>>,
) -> Json<MetricsResponse> {
    let per_symbol: HashMap<String, HistogramSummary> = state
        .histograms
        .iter()
        .map(|(k, v)| (k.clone(), v.summary()))
        .collect();
    
    let response = MetricsResponse {
        probe_info: ProbeInfo {
            region: state.args.region.clone(),
            instance_family: state.args.instance_family.clone(),
            experiment_id: state.args.experiment_id.clone(),
            timestamp: Utc::now(),
        },
        aggregate: state.aggregate.summary(),
        per_symbol,
        counters: Counters {
            messages: state.message_count.load(Ordering::Relaxed),
            reconnects: state.reconnect_count.load(Ordering::Relaxed),
            errors: state.error_count.load(Ordering::Relaxed),
        },
        recent_samples: state.recent_samples.read().clone(),
        uptime_sec: state.start_time.elapsed().as_secs(),
        warmup_complete: state.is_warmed_up(),
    };
    
    Json(response)
}

async fn health() -> &'static str {
    "OK"
}

// === Main ===

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with JSON format for structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("binance_latency_probe=info".parse().unwrap())
        )
        .json()
        .init();
    
    let args = Args::parse();
    info!(?args, "Starting Binance latency probe");
    
    let state = Arc::new(ProbeState::new(args.clone()));
    
    // Start metrics server
    let metrics_state = state.clone();
    let metrics_port = args.metrics_port;
    tokio::spawn(async move {
        let app = Router::new()
            .route("/metrics", get(get_metrics))
            .route("/health", get(health))
            .with_state(metrics_state);
        
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", metrics_port))
            .await
            .expect("Failed to bind metrics port");
        
        info!("Metrics server listening on port {}", metrics_port);
        axum::serve(listener, app).await.expect("Metrics server failed");
    });
    
    // Warmup timer
    let warmup_state = state.clone();
    let warmup_sec = args.warmup_sec;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(warmup_sec)).await;
        warmup_state.mark_warmup_complete();
    });
    
    // Initialize Binance streams (same pattern as main codebase)
    let streams = init_streams().await?;
    
    // Consume stream
    let mut joined = streams.select_all();
    
    while let Some(event) = joined.next().await {
        match event {
            ReconnectEvent::Reconnecting(exchange) => {
                warn!(?exchange, "WebSocket reconnecting");
                state.reconnect_count.fetch_add(1, Ordering::Relaxed);
            }
            ReconnectEvent::Item(result) => match result {
                Ok(market_event) => {
                    let receive_ts_ms = Utc::now().timestamp_millis();
                    let exchange_ts_ms = market_event.time_exchange.timestamp_millis();
                    
                    let symbol = format!(
                        "{}{}",
                        market_event.instrument.base,
                        market_event.instrument.quote
                    ).to_uppercase();
                    
                    state.record_latency(&symbol, exchange_ts_ms, receive_ts_ms);
                }
                Err(e) => {
                    warn!(error = %e, "Stream error");
                    state.error_count.fetch_add(1, Ordering::Relaxed);
                }
            },
        }
    }
    
    Ok(())
}

async fn init_streams() -> Result<
    Streams<
        barter_data::streams::consumer::MarketStreamResult<
            MarketDataInstrument,
            barter_data::subscription::book::OrderBookL1,
        >,
    >,
> {
    Streams::<OrderBooksL1>::builder()
        .subscribe([
            (BinanceSpot::default(), "btc", "usdt", MarketDataInstrumentKind::Spot, OrderBooksL1),
            (BinanceSpot::default(), "eth", "usdt", MarketDataInstrumentKind::Spot, OrderBooksL1),
            (BinanceSpot::default(), "sol", "usdt", MarketDataInstrumentKind::Spot, OrderBooksL1),
            (BinanceSpot::default(), "xrp", "usdt", MarketDataInstrumentKind::Spot, OrderBooksL1),
        ])
        .init()
        .await
        .context("Failed to init Binance streams")
}
```

---

## 5. Bootstrap Script

```bash
#!/bin/bash
# infra/latency-sweep/bootstrap/install.sh

set -euo pipefail

EXPERIMENT_ID="${experiment_id}"
REGION="${region}"
INSTANCE_FAMILY="${instance_family}"

echo "=== Binance Latency Probe Bootstrap ==="
echo "Experiment: $EXPERIMENT_ID"
echo "Region: $REGION"
echo "Instance: $INSTANCE_FAMILY"

# Update system
dnf update -y

# Install build dependencies
dnf groupinstall -y "Development Tools"
dnf install -y gcc openssl-devel

# Install Rust (latest stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source "$HOME/.cargo/env"

# Clone or copy probe code (in real deployment, use S3 or git)
mkdir -p /opt/latency-probe
cd /opt/latency-probe

# Write Cargo.toml and src/main.rs (in production, fetch from S3)
cat > Cargo.toml << 'CARGO_EOF'
# ... (contents from section 4.1)
CARGO_EOF

mkdir -p src
cat > src/main.rs << 'RUST_EOF'
# ... (contents from section 4.2)
RUST_EOF

# Build release binary
cargo build --release

# Create systemd service
cat > /etc/systemd/system/binance-probe.service << EOF
[Unit]
Description=Binance Latency Probe
After=network.target

[Service]
Type=simple
User=root
Environment="PROBE_REGION=$REGION"
Environment="PROBE_INSTANCE_FAMILY=$INSTANCE_FAMILY"
Environment="PROBE_EXPERIMENT_ID=$EXPERIMENT_ID"
Environment="RUST_LOG=info"
ExecStart=/opt/latency-probe/target/release/binance-latency-probe
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

# Enable and start
systemctl daemon-reload
systemctl enable binance-probe
systemctl start binance-probe

echo "=== Bootstrap Complete ==="
```

---

## 6. Orchestration & Analysis

```python
#!/usr/bin/env python3
# infra/latency-sweep/orchestrate/run_sweep.py

"""
Latency Sweep Orchestrator

Coordinates multi-region probe deployment, data collection, and analysis.
"""

import argparse
import json
import subprocess
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional

import requests

@dataclass
class ProbeConfig:
    region: str
    instance_family: str
    instance_ip: str
    metrics_port: int = 9090

@dataclass  
class SweepConfig:
    experiment_id: str
    warmup_sec: int = 300
    measurement_sec: int = 3600
    poll_interval_sec: int = 60
    output_dir: Path = Path("./results")

def collect_metrics(probe: ProbeConfig) -> Optional[dict]:
    """Fetch metrics from a probe instance."""
    try:
        resp = requests.get(
            f"http://{probe.instance_ip}:{probe.metrics_port}/metrics",
            timeout=10
        )
        resp.raise_for_status()
        return resp.json()
    except Exception as e:
        print(f"Error collecting from {probe.region}/{probe.instance_family}: {e}")
        return None

def run_terraform(action: str, phase: int = 1, extra_args: List[str] = None):
    """Run Terraform command."""
    cmd = ["terraform", action]
    if action in ("apply", "destroy"):
        cmd.append("-auto-approve")
    cmd.extend([f"-var=phase={phase}"])
    if extra_args:
        cmd.extend(extra_args)
    
    subprocess.run(cmd, cwd="terraform", check=True)

def get_probe_ips() -> Dict[str, Dict[str, str]]:
    """Get probe IPs from Terraform outputs."""
    result = subprocess.run(
        ["terraform", "output", "-json"],
        cwd="terraform",
        capture_output=True,
        text=True,
        check=True
    )
    outputs = json.loads(result.stdout)
    # Parse outputs into probe configs
    # Structure: {region: {instance_family: ip}}
    return outputs.get("probe_ips", {}).get("value", {})

def run_sweep(config: SweepConfig, probes: List[ProbeConfig]):
    """Execute the latency sweep experiment."""
    config.output_dir.mkdir(parents=True, exist_ok=True)
    
    results_file = config.output_dir / f"{config.experiment_id}_results.jsonl"
    
    print(f"Starting sweep: {config.experiment_id}")
    print(f"Probes: {len(probes)}")
    print(f"Warmup: {config.warmup_sec}s, Measurement: {config.measurement_sec}s")
    
    # Wait for warmup
    print(f"Waiting {config.warmup_sec}s for warmup...")
    time.sleep(config.warmup_sec)
    
    # Collect metrics periodically
    start_time = time.time()
    end_time = start_time + config.measurement_sec
    
    with open(results_file, "a") as f:
        while time.time() < end_time:
            elapsed = int(time.time() - start_time)
            remaining = int(end_time - time.time())
            print(f"[{elapsed}s elapsed, {remaining}s remaining]")
            
            for probe in probes:
                metrics = collect_metrics(probe)
                if metrics:
                    record = {
                        "timestamp": datetime.utcnow().isoformat(),
                        "elapsed_sec": elapsed,
                        "region": probe.region,
                        "instance_family": probe.instance_family,
                        "metrics": metrics
                    }
                    f.write(json.dumps(record) + "\n")
                    f.flush()
            
            time.sleep(config.poll_interval_sec)
    
    print(f"Sweep complete. Results: {results_file}")
    return results_file

def main():
    parser = argparse.ArgumentParser(description="Latency Sweep Orchestrator")
    parser.add_argument("--phase", type=int, default=1, help="Deployment phase (1=EU, 2=Global)")
    parser.add_argument("--warmup", type=int, default=300, help="Warmup seconds")
    parser.add_argument("--duration", type=int, default=3600, help="Measurement seconds")
    parser.add_argument("--deploy", action="store_true", help="Deploy infrastructure")
    parser.add_argument("--destroy", action="store_true", help="Destroy infrastructure")
    parser.add_argument("--collect", action="store_true", help="Collect metrics only")
    args = parser.parse_args()
    
    experiment_id = f"latency-sweep-{datetime.utcnow().strftime('%Y%m%d-%H%M')}"
    
    if args.deploy:
        print("Deploying infrastructure...")
        run_terraform("init")
        run_terraform("apply", phase=args.phase)
    
    if args.collect:
        probe_ips = get_probe_ips()
        probes = [
            ProbeConfig(region=region, instance_family=family, instance_ip=ip)
            for region, families in probe_ips.items()
            for family, ip in families.items()
        ]
        
        config = SweepConfig(
            experiment_id=experiment_id,
            warmup_sec=args.warmup,
            measurement_sec=args.duration
        )
        
        run_sweep(config, probes)
    
    if args.destroy:
        print("Destroying infrastructure...")
        run_terraform("destroy", phase=args.phase)

if __name__ == "__main__":
    main()
```

```python
#!/usr/bin/env python3
# infra/latency-sweep/orchestrate/analyze.py

"""
Latency Sweep Analyzer

Processes collected metrics and produces decision recommendations.
"""

import argparse
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Tuple

import numpy as np
from scipy import stats

@dataclass
class ConfigMetrics:
    region: str
    instance_family: str
    p50_samples: List[int]
    p99_samples: List[int]
    p999_samples: List[int]
    cv_samples: List[float]
    reconnects: int
    errors: int
    message_count: int

def load_results(results_file: Path) -> Dict[Tuple[str, str], ConfigMetrics]:
    """Load and aggregate results by (region, instance_family)."""
    metrics: Dict[Tuple[str, str], ConfigMetrics] = {}
    
    with open(results_file) as f:
        for line in f:
            record = json.loads(line)
            key = (record["region"], record["instance_family"])
            
            if key not in metrics:
                metrics[key] = ConfigMetrics(
                    region=record["region"],
                    instance_family=record["instance_family"],
                    p50_samples=[],
                    p99_samples=[],
                    p999_samples=[],
                    cv_samples=[],
                    reconnects=0,
                    errors=0,
                    message_count=0
                )
            
            m = metrics[key]
            agg = record["metrics"]["aggregate"]
            
            m.p50_samples.append(agg["p50_us"])
            m.p99_samples.append(agg["p99_us"])
            m.p999_samples.append(agg["p999_us"])
            m.cv_samples.append(agg["cv"])
            
            counters = record["metrics"]["counters"]
            m.reconnects = max(m.reconnects, counters["reconnects"])
            m.errors = max(m.errors, counters["errors"])
            m.message_count = max(m.message_count, counters["messages"])
    
    return metrics

def compute_score(m: ConfigMetrics, all_metrics: List[ConfigMetrics]) -> Tuple[float, dict]:
    """
    Compute weighted score for a configuration.
    
    SCORE = 0.7 * normalized_p99 + 0.3 * normalized_cv
    
    Lower is better.
    """
    # Get all p99 and CV values for normalization
    all_p99 = [np.median(x.p99_samples) for x in all_metrics]
    all_cv = [np.median(x.cv_samples) for x in all_metrics]
    
    min_p99, max_p99 = min(all_p99), max(all_p99)
    max_cv = max(all_cv)
    
    # This config's values
    p99 = np.median(m.p99_samples)
    cv = np.median(m.cv_samples)
    
    # Normalize (0 = best, 1 = worst)
    norm_p99 = (p99 - min_p99) / (max_p99 - min_p99) if max_p99 > min_p99 else 0
    norm_cv = cv / max_cv if max_cv > 0 else 0
    
    # Weighted score
    score = 0.7 * norm_p99 + 0.3 * norm_cv
    
    details = {
        "p99_median_us": int(p99),
        "p99_p10": int(np.percentile(m.p99_samples, 10)),
        "p99_p90": int(np.percentile(m.p99_samples, 90)),
        "cv_median": round(cv, 4),
        "p999_median_us": int(np.median(m.p999_samples)),
        "p999_p99_ratio": round(np.median(m.p999_samples) / p99, 2) if p99 > 0 else 0,
        "normalized_p99": round(norm_p99, 4),
        "normalized_cv": round(norm_cv, 4),
        "reconnects": m.reconnects,
        "errors": m.errors,
        "samples": len(m.p99_samples)
    }
    
    return score, details

def check_hard_constraints(m: ConfigMetrics) -> Tuple[bool, List[str]]:
    """Check if configuration meets hard constraints."""
    violations = []
    
    p99 = np.median(m.p99_samples)
    p999 = np.median(m.p999_samples)
    
    # p99 < 50ms
    if p99 > 50_000:
        violations.append(f"p99 {p99/1000:.1f}ms > 50ms limit")
    
    # Reconnect rate < 1/hour (assuming 1-hour measurement)
    if m.reconnects > 1:
        violations.append(f"Reconnects {m.reconnects} > 1/hour limit")
    
    # p999/p99 ratio < 3.0
    ratio = p999 / p99 if p99 > 0 else 0
    if ratio > 3.0:
        violations.append(f"p999/p99 ratio {ratio:.2f} > 3.0 limit")
    
    return len(violations) == 0, violations

def statistical_comparison(m1: ConfigMetrics, m2: ConfigMetrics) -> dict:
    """Compare two configurations statistically."""
    # Mann-Whitney U test (non-parametric)
    stat, p_value = stats.mannwhitneyu(
        m1.p99_samples, m2.p99_samples, alternative='less'
    )
    
    # Bootstrap confidence interval for difference
    n_bootstrap = 1000
    diffs = []
    for _ in range(n_bootstrap):
        s1 = np.random.choice(m1.p99_samples, size=len(m1.p99_samples), replace=True)
        s2 = np.random.choice(m2.p99_samples, size=len(m2.p99_samples), replace=True)
        diffs.append(np.median(s2) - np.median(s1))
    
    ci_low, ci_high = np.percentile(diffs, [2.5, 97.5])
    
    return {
        "mann_whitney_p": round(p_value, 4),
        "significant": p_value < 0.05,
        "p99_diff_median_us": int(np.median(diffs)),
        "p99_diff_ci_95": [int(ci_low), int(ci_high)]
    }

def analyze(results_file: Path):
    """Main analysis function."""
    print(f"Analyzing: {results_file}")
    
    metrics = load_results(results_file)
    all_metrics = list(metrics.values())
    
    print(f"\nLoaded {len(all_metrics)} configurations")
    print("=" * 80)
    
    # Score all configurations
    scored = []
    for m in all_metrics:
        passes, violations = check_hard_constraints(m)
        score, details = compute_score(m, all_metrics)
        
        scored.append({
            "region": m.region,
            "instance_family": m.instance_family,
            "score": score,
            "passes_constraints": passes,
            "violations": violations,
            "details": details,
            "metrics": m
        })
    
    # Sort by score (lower is better)
    scored.sort(key=lambda x: (not x["passes_constraints"], x["score"]))
    
    print("\n=== RANKING (lower score = better) ===\n")
    for i, s in enumerate(scored, 1):
        status = "PASS" if s["passes_constraints"] else "FAIL"
        print(f"{i}. [{status}] {s['region']} / {s['instance_family']}")
        print(f"   Score: {s['score']:.4f}")
        print(f"   p99: {s['details']['p99_median_us']/1000:.2f}ms "
              f"(range: {s['details']['p99_p10']/1000:.2f}-{s['details']['p99_p90']/1000:.2f}ms)")
        print(f"   CV: {s['details']['cv_median']:.4f}")
        print(f"   p999/p99 ratio: {s['details']['p999_p99_ratio']:.2f}")
        if s["violations"]:
            print(f"   Violations: {', '.join(s['violations'])}")
        print()
    
    # Statistical comparison between top 2
    if len(scored) >= 2 and scored[0]["passes_constraints"] and scored[1]["passes_constraints"]:
        print("\n=== STATISTICAL COMPARISON (Top 2) ===\n")
        comparison = statistical_comparison(scored[0]["metrics"], scored[1]["metrics"])
        print(f"Comparing: {scored[0]['region']}/{scored[0]['instance_family']} vs "
              f"{scored[1]['region']}/{scored[1]['instance_family']}")
        print(f"Mann-Whitney U p-value: {comparison['mann_whitney_p']}")
        print(f"Significant difference: {comparison['significant']}")
        print(f"p99 difference: {comparison['p99_diff_median_us']/1000:.2f}ms "
              f"(95% CI: [{comparison['p99_diff_ci_95'][0]/1000:.2f}, "
              f"{comparison['p99_diff_ci_95'][1]/1000:.2f}]ms)")
    
    # Final recommendation
    print("\n=== RECOMMENDATION ===\n")
    winner = scored[0] if scored[0]["passes_constraints"] else None
    if winner:
        print(f"RECOMMENDED: {winner['region']} with {winner['instance_family']}")
        print(f"Expected p99: {winner['details']['p99_median_us']/1000:.2f}ms")
        print(f"Stability (CV): {winner['details']['cv_median']:.4f}")
    else:
        print("WARNING: No configuration passes all hard constraints!")
        print("Consider expanding the test matrix or relaxing constraints.")

def main():
    parser = argparse.ArgumentParser(description="Latency Sweep Analyzer")
    parser.add_argument("results_file", type=Path, help="JSONL results file")
    args = parser.parse_args()
    
    analyze(args.results_file)

if __name__ == "__main__":
    main()
```

---

## 7. Execution Checklist

### Pre-flight
- [ ] AWS credentials configured for all target regions
- [ ] S3 bucket for Terraform state exists
- [ ] SSH key pair generated
- [ ] Cost estimate reviewed (~$0.10-0.20/hr per probe)

### Phase 1 (EU)
```bash
cd infra/latency-sweep/orchestrate
python run_sweep.py --phase 1 --deploy
python run_sweep.py --phase 1 --collect --warmup 300 --duration 3600
python analyze.py results/latency-sweep-*.jsonl
python run_sweep.py --phase 1 --destroy
```

### Phase 2 (Global) - if EU results inconclusive
```bash
python run_sweep.py --phase 2 --deploy
python run_sweep.py --phase 2 --collect --warmup 300 --duration 3600
python analyze.py results/latency-sweep-*.jsonl
python run_sweep.py --phase 2 --destroy
```

### Post-analysis
- [ ] Review p99 winner vs runner-up statistical significance
- [ ] Check time-of-day consistency
- [ ] Calculate cost/latency tradeoff
- [ ] Update `rust-backend` deployment config with winning region

---

## 8. Expected Outcomes

Based on prior Binance latency research:

| Region | Expected p99 | Notes |
|--------|-------------|-------|
| ap-southeast-1 | 5-15ms | Likely best due to Binance Singapore |
| eu-central-1 | 20-40ms | Good European connectivity |
| eu-west-2 | 25-45ms | Financial hub, decent peering |
| eu-west-1 | 30-50ms | Good AWS hub, farther from Binance |

Network-optimized instances (c6in) typically show 10-20% improvement in p99 over standard compute due to:
- ENA Express for lower jitter
- Higher PPS (packets per second) limits
- Better NUMA locality

---

## 9. Cost Estimate

| Component | Cost/hr | Phase 1 (3 regions × 2 instances) | Phase 2 (+3 regions) |
|-----------|---------|-----------------------------------|---------------------|
| c6i.large | $0.085 | $0.51/hr | $1.02/hr |
| c6in.large | $0.113 | $0.68/hr | $1.36/hr |
| Data transfer | ~$0.02 | $0.12/hr | $0.24/hr |
| **Total** | | **~$1.31/hr** | **~$2.62/hr** |

4-hour experiment (warmup + measurement + buffer): **~$5-10** per phase.
