# Time Synchronization & Timestamping Design

## Executive Summary

This document defines a production-grade time synchronization approach for trustworthy "exchange timestamp → receipt" latency metrics. The design uses:

1. **Monotonic clocks** for all internal latency measurements (immune to NTP adjustments)
2. **Wall-clock (NTP-synchronized)** only for correlation with exchange timestamps
3. **Clock-step detection** to invalidate metrics during synchronization events
4. **Chrony** as the NTP daemon with aggressive tuning for sub-millisecond accuracy

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TIMESTAMP DOMAINS                                    │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌──────────────────────┐     ┌──────────────────────┐                      │
│  │   MONOTONIC DOMAIN   │     │  WALL-CLOCK DOMAIN   │                      │
│  │   (CLOCK_MONOTONIC)  │     │   (CLOCK_REALTIME)   │                      │
│  │                      │     │                      │                      │
│  │  • Internal latency  │     │  • Exchange ts       │                      │
│  │  • Processing time   │     │  • One-way latency   │                      │
│  │  • Tick-to-trade     │     │  • Log timestamps    │                      │
│  │  • Jitter tracking   │     │  • CSV export        │                      │
│  │                      │     │                      │                      │
│  │  IMMUNE TO:          │     │  AFFECTED BY:        │                      │
│  │  - NTP steps         │     │  - NTP corrections   │                      │
│  │  - Leap seconds      │     │  - Clock drift       │                      │
│  │  - Time adjustments  │     │  - Leap seconds      │                      │
│  └──────────────────────┘     └──────────────────────┘                      │
│           │                            │                                     │
│           │                            │                                     │
│           ▼                            ▼                                     │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │                    CLOCK HEALTH MONITOR                          │       │
│  │                                                                  │       │
│  │  • Detects clock steps (|Δwall - Δmono| > threshold)             │       │
│  │  • Publishes ClockHealth { synced, offset_us, step_detected }    │       │
│  │  • Invalidates one-way metrics during clock instability          │       │
│  └──────────────────────────────────────────────────────────────────┘       │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. Chrony Configuration

### 1.1 Installation

```bash
# Ubuntu/Debian
sudo apt-get install chrony

# Amazon Linux / RHEL
sudo yum install chrony

# macOS (Homebrew)
brew install chrony
```

### 1.2 Configuration File (`/etc/chrony/chrony.conf`)

```conf
# =============================================================================
# CHRONY CONFIGURATION FOR HFT LATENCY MEASUREMENT
# Target: <1ms offset from UTC, <100μs jitter
# =============================================================================

# --- NTP Servers ---
# Use multiple stratum-1 servers close to your datacenter
# AWS eu-west-1: Use Amazon Time Sync Service (169.254.169.123) + public pools

# Amazon Time Sync (stratum 1, ~50μs accuracy in AWS)
server 169.254.169.123 prefer iburst minpoll 0 maxpoll 2

# Cloudflare (anycast, low latency)
server time.cloudflare.com iburst minpoll 1 maxpoll 4

# Google Public NTP (leap smear - be aware!)
server time.google.com iburst minpoll 1 maxpoll 4

# Regional pools (add servers close to your region)
pool 0.europe.pool.ntp.org iburst minpoll 1 maxpoll 4 maxsources 3
pool 1.europe.pool.ntp.org iburst minpoll 1 maxpoll 4 maxsources 3

# --- Synchronization Behavior ---

# Allow large initial offset correction (first sync only)
makestep 1.0 3

# After initial sync, use slewing only (max 500μs/sec adjustment)
# This prevents clock jumps that break latency metrics
maxslewrate 500

# Minimum samples before trusting a source
minsources 2

# --- Drift & Stability ---

# Store drift rate to survive reboots
driftfile /var/lib/chrony/drift

# RTC (real-time clock) synchronization
rtcsync

# --- Logging ---

# Log significant events
logdir /var/log/chrony
log tracking measurements statistics

# Log clock steps for debugging
logchange 0.001

# --- Access Control ---

# Allow chronyc from localhost only
bindcmdaddress 127.0.0.1
bindcmdaddress ::1

# Deny NTP client access (we're a client, not a server)
deny all

# --- Hardware Timestamping (if NIC supports it) ---
# Uncomment if your NIC supports hardware timestamps (e.g., Intel X710)
# hwtimestamp eth0

# --- Leap Second Handling ---
# Use kernel leap second handling (preferred)
leapsecmode system
maxchange 1000000000 1 2

# --- Temperature Compensation (optional, for precision) ---
# tempcomp /sys/class/hwmon/hwmon0/temp1_input 30 26000 0.0 0.0 0.0
```

### 1.3 Chrony Service Setup

```bash
# Enable and start chrony
sudo systemctl enable chrony
sudo systemctl start chrony

# Verify synchronization
chronyc tracking
chronyc sources -v
chronyc sourcestats

# Expected output (good sync):
# Reference ID    : A9FEA97B (169.254.169.123)
# Stratum         : 4
# Ref time (UTC)  : Sat Jan 25 12:34:56 2025
# System time     : 0.000000123 seconds fast of NTP time
# Last offset     : +0.000000045 seconds
# RMS offset      : 0.000000234 seconds
# Frequency       : 12.345 ppm slow
# Root delay      : 0.000123456 seconds
# Root dispersion : 0.000012345 seconds
```

---

## 2. Rust Implementation

### 2.1 Core Types (`src/performance/latency/time_sync.rs`)

```rust
//! Time Synchronization and Clock Health Monitoring
//!
//! Provides trustworthy timestamping for latency measurement by:
//! 1. Using monotonic clocks for all internal measurements
//! 2. Detecting clock steps that invalidate one-way latency metrics
//! 3. Exposing clock health status for metric validity decisions

use std::{
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;

// =============================================================================
// CONSTANTS
// =============================================================================

/// Threshold for detecting a clock step (microseconds)
/// If |Δwall - Δmono| exceeds this, we've had a clock adjustment
const CLOCK_STEP_THRESHOLD_US: i64 = 1_000; // 1ms

/// Cooldown period after a clock step before trusting one-way metrics again
const CLOCK_STEP_COOLDOWN_MS: u64 = 5_000; // 5 seconds

/// Maximum acceptable clock offset for "synced" status (microseconds)
const MAX_ACCEPTABLE_OFFSET_US: i64 = 1_000; // 1ms

/// Sampling interval for clock health checks (milliseconds)
const HEALTH_CHECK_INTERVAL_MS: u64 = 100;

// =============================================================================
// MONOTONIC TIMESTAMP
// =============================================================================

/// High-resolution monotonic timestamp for internal latency measurement.
/// 
/// Uses `CLOCK_MONOTONIC_RAW` on Linux when available (immune to NTP slewing).
/// Falls back to `std::time::Instant` on other platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonoTs {
    /// Nanoseconds since process start (or arbitrary epoch)
    nanos: u64,
}

impl MonoTs {
    /// Capture current monotonic timestamp
    #[inline]
    pub fn now() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::now_linux()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::now_portable()
        }
    }

    #[cfg(target_os = "linux")]
    #[inline]
    fn now_linux() -> Self {
        use std::mem::MaybeUninit;
        
        let mut ts = MaybeUninit::<libc::timespec>::uninit();
        // CLOCK_MONOTONIC_RAW is immune to NTP adjustments
        let ret = unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, ts.as_mut_ptr())
        };
        
        if ret == 0 {
            let ts = unsafe { ts.assume_init() };
            Self {
                nanos: (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64),
            }
        } else {
            // Fallback to std::time::Instant
            Self::now_portable()
        }
    }

    #[inline]
    fn now_portable() -> Self {
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        Self {
            nanos: epoch.elapsed().as_nanos() as u64,
        }
    }

    /// Nanoseconds since epoch
    #[inline]
    pub fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// Microseconds since epoch
    #[inline]
    pub fn as_micros(&self) -> u64 {
        self.nanos / 1_000
    }

    /// Duration since another timestamp (saturating)
    #[inline]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        Duration::from_nanos(self.nanos.saturating_sub(earlier.nanos))
    }

    /// Elapsed since this timestamp
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }
}

// =============================================================================
// WALL-CLOCK TIMESTAMP
// =============================================================================

/// Wall-clock timestamp for correlation with exchange timestamps.
/// 
/// WARNING: Only use for one-way latency estimation when ClockHealth.synced == true.
/// Never use for internal latency calculations.
#[derive(Debug, Clone, Copy)]
pub struct WallTs {
    /// Unix timestamp in nanoseconds
    unix_nanos: i64,
}

impl WallTs {
    /// Capture current wall-clock timestamp
    #[inline]
    pub fn now() -> Self {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            unix_nanos: dur.as_nanos() as i64,
        }
    }

    /// From Unix milliseconds (Binance timestamp format)
    #[inline]
    pub fn from_millis(millis: i64) -> Self {
        Self {
            unix_nanos: millis * 1_000_000,
        }
    }

    /// Unix nanoseconds
    #[inline]
    pub fn as_nanos(&self) -> i64 {
        self.unix_nanos
    }

    /// Unix milliseconds
    #[inline]
    pub fn as_millis(&self) -> i64 {
        self.unix_nanos / 1_000_000
    }

    /// Signed difference in nanoseconds (self - other)
    #[inline]
    pub fn diff_nanos(&self, other: Self) -> i64 {
        self.unix_nanos - other.unix_nanos
    }
}

// =============================================================================
// SYNCHRONIZED TIMESTAMP PAIR
// =============================================================================

/// Atomic capture of both monotonic and wall-clock timestamps.
/// 
/// Used to detect clock steps by comparing Δmono vs Δwall over time.
#[derive(Debug, Clone, Copy)]
pub struct TimestampPair {
    pub mono: MonoTs,
    pub wall: WallTs,
}

impl TimestampPair {
    /// Capture both timestamps atomically (as close together as possible)
    #[inline]
    pub fn now() -> Self {
        // Capture mono first (faster), then wall
        // The gap between them is typically <1μs
        let mono = MonoTs::now();
        let wall = WallTs::now();
        Self { mono, wall }
    }
}

// =============================================================================
// CLOCK HEALTH MONITOR
// =============================================================================

/// Clock health status for metric validity decisions
#[derive(Debug, Clone, Copy)]
pub struct ClockHealth {
    /// Whether the clock is considered synchronized (offset < threshold)
    pub synced: bool,
    /// Estimated offset from NTP time (microseconds, positive = ahead)
    pub offset_us: i64,
    /// Whether a clock step was detected recently
    pub step_detected: bool,
    /// Time since last clock step (milliseconds, 0 if never)
    pub ms_since_step: u64,
    /// Last health check timestamp (monotonic nanos)
    pub last_check_mono_ns: u64,
}

impl ClockHealth {
    /// Check if one-way latency metrics are trustworthy
    #[inline]
    pub fn one_way_metrics_valid(&self) -> bool {
        self.synced && !self.step_detected && self.ms_since_step > CLOCK_STEP_COOLDOWN_MS
    }
}

/// Clock health monitor that detects clock steps and tracks sync status
pub struct ClockHealthMonitor {
    /// Last timestamp pair for step detection
    last_pair: RwLock<Option<TimestampPair>>,
    /// Whether a clock step was detected
    step_detected: AtomicBool,
    /// Monotonic timestamp of last clock step (nanos)
    last_step_mono_ns: AtomicU64,
    /// Current estimated offset (microseconds)
    offset_us: AtomicI64,
    /// Whether clock is considered synced
    synced: AtomicBool,
    /// Last chrony offset reading (microseconds)
    chrony_offset_us: AtomicI64,
}

impl Default for ClockHealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockHealthMonitor {
    pub fn new() -> Self {
        Self {
            last_pair: RwLock::new(None),
            step_detected: AtomicBool::new(false),
            last_step_mono_ns: AtomicU64::new(0),
            offset_us: AtomicI64::new(0),
            synced: AtomicBool::new(false),
            chrony_offset_us: AtomicI64::new(0),
        }
    }

    /// Update clock health (call periodically, e.g., every 100ms)
    pub fn update(&self) {
        let current = TimestampPair::now();
        
        // Check for clock step
        if let Some(prev) = *self.last_pair.read() {
            let mono_delta_ns = current.mono.as_nanos().saturating_sub(prev.mono.as_nanos());
            let wall_delta_ns = (current.wall.as_nanos() - prev.wall.as_nanos()) as u64;
            
            // Clock step if wall jumped more than mono + threshold
            let diff_us = (wall_delta_ns as i64 - mono_delta_ns as i64).abs() / 1_000;
            
            if diff_us > CLOCK_STEP_THRESHOLD_US {
                self.step_detected.store(true, Ordering::Release);
                self.last_step_mono_ns.store(current.mono.as_nanos(), Ordering::Release);
                tracing::warn!(
                    diff_us = diff_us,
                    "Clock step detected: wall jumped {}μs relative to monotonic",
                    diff_us
                );
            }
        }
        
        // Update last pair
        *self.last_pair.write() = Some(current);
        
        // Clear step flag after cooldown
        let last_step = self.last_step_mono_ns.load(Ordering::Acquire);
        if last_step > 0 {
            let elapsed_ms = (current.mono.as_nanos() - last_step) / 1_000_000;
            if elapsed_ms > CLOCK_STEP_COOLDOWN_MS {
                self.step_detected.store(false, Ordering::Release);
            }
        }
    }

    /// Update offset from chrony (call when reading chrony tracking)
    pub fn update_chrony_offset(&self, offset_us: i64) {
        self.chrony_offset_us.store(offset_us, Ordering::Release);
        self.offset_us.store(offset_us, Ordering::Release);
        
        let synced = offset_us.abs() < MAX_ACCEPTABLE_OFFSET_US;
        self.synced.store(synced, Ordering::Release);
    }

    /// Get current clock health status
    pub fn health(&self) -> ClockHealth {
        let now_mono_ns = MonoTs::now().as_nanos();
        let last_step = self.last_step_mono_ns.load(Ordering::Acquire);
        
        let ms_since_step = if last_step > 0 {
            (now_mono_ns - last_step) / 1_000_000
        } else {
            u64::MAX // Never had a step
        };
        
        ClockHealth {
            synced: self.synced.load(Ordering::Acquire),
            offset_us: self.offset_us.load(Ordering::Acquire),
            step_detected: self.step_detected.load(Ordering::Acquire),
            ms_since_step,
            last_check_mono_ns: now_mono_ns,
        }
    }
}

/// Global clock health monitor
pub fn clock_health() -> &'static ClockHealthMonitor {
    static MONITOR: std::sync::OnceLock<ClockHealthMonitor> = std::sync::OnceLock::new();
    MONITOR.get_or_init(ClockHealthMonitor::new)
}

// =============================================================================
// CHRONY INTEGRATION
// =============================================================================

/// Parse chrony tracking output to extract offset
pub fn parse_chrony_tracking(output: &str) -> Option<ChronyTracking> {
    let mut tracking = ChronyTracking::default();
    
    for line in output.lines() {
        let line = line.trim();
        
        if line.starts_with("System time") {
            // "System time     : 0.000000123 seconds fast of NTP time"
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        let sign = if value_part.contains("slow") { 1.0 } else { -1.0 };
                        tracking.offset_us = (secs * sign * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("RMS offset") {
            // "RMS offset      : 0.000000234 seconds"
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        tracking.rms_offset_us = (secs * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("Root delay") {
            // "Root delay      : 0.000123456 seconds"
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        tracking.root_delay_us = (secs * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("Stratum") {
            // "Stratum         : 4"
            if let Some(idx) = line.find(':') {
                let value_str = line[idx + 1..].trim();
                if let Ok(stratum) = value_str.parse::<u8>() {
                    tracking.stratum = stratum;
                }
            }
        } else if line.starts_with("Leap status") {
            // "Leap status     : Normal"
            if let Some(idx) = line.find(':') {
                tracking.leap_status = line[idx + 1..].trim().to_string();
            }
        }
    }
    
    Some(tracking)
}

#[derive(Debug, Clone, Default)]
pub struct ChronyTracking {
    /// Current offset from NTP time (microseconds, negative = behind)
    pub offset_us: i64,
    /// RMS offset (microseconds)
    pub rms_offset_us: i64,
    /// Root delay (microseconds)
    pub root_delay_us: i64,
    /// Stratum level
    pub stratum: u8,
    /// Leap second status
    pub leap_status: String,
}

/// Read chrony tracking data
#[cfg(unix)]
pub async fn read_chrony_tracking() -> Option<ChronyTracking> {
    use tokio::process::Command;
    
    let output = Command::new("chronyc")
        .arg("tracking")
        .output()
        .await
        .ok()?;
    
    if !output.status.success() {
        return None;
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_chrony_tracking(&stdout)
}

#[cfg(not(unix))]
pub async fn read_chrony_tracking() -> Option<ChronyTracking> {
    None
}

// =============================================================================
// LATENCY CALCULATION HELPERS
// =============================================================================

/// Calculate one-way latency from exchange timestamp to local receipt.
/// 
/// Returns `None` if clock health indicates metrics are untrustworthy.
#[inline]
pub fn one_way_latency_us(exchange_ts_ms: i64, receipt_wall: WallTs) -> Option<i64> {
    let health = clock_health().health();
    
    if !health.one_way_metrics_valid() {
        return None;
    }
    
    let exchange_wall = WallTs::from_millis(exchange_ts_ms);
    let latency_us = (receipt_wall.as_nanos() - exchange_wall.as_nanos()) / 1_000;
    
    // Sanity check: latency should be positive and reasonable (< 10s)
    if latency_us > 0 && latency_us < 10_000_000 {
        Some(latency_us)
    } else {
        None
    }
}

/// Calculate internal processing latency (always valid, uses monotonic)
#[inline]
pub fn internal_latency_us(start: MonoTs, end: MonoTs) -> u64 {
    end.duration_since(start).as_micros() as u64
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mono_ts_monotonic() {
        let t1 = MonoTs::now();
        std::thread::sleep(Duration::from_micros(100));
        let t2 = MonoTs::now();
        
        assert!(t2.as_nanos() > t1.as_nanos());
        assert!(t2.duration_since(t1).as_micros() >= 100);
    }

    #[test]
    fn test_wall_ts_conversion() {
        let ts = WallTs::from_millis(1700000000000);
        assert_eq!(ts.as_millis(), 1700000000000);
    }

    #[test]
    fn test_clock_health_monitor() {
        let monitor = ClockHealthMonitor::new();
        
        // Initial state: not synced, no step
        let health = monitor.health();
        assert!(!health.synced);
        assert!(!health.step_detected);
        
        // Update with good offset
        monitor.update_chrony_offset(100); // 100μs offset
        monitor.update();
        
        let health = monitor.health();
        assert!(health.synced);
    }

    #[test]
    fn test_parse_chrony_tracking() {
        let output = r#"
Reference ID    : A9FEA97B (169.254.169.123)
Stratum         : 4
Ref time (UTC)  : Sat Jan 25 12:34:56 2025
System time     : 0.000000123 seconds fast of NTP time
Last offset     : +0.000000045 seconds
RMS offset      : 0.000000234 seconds
Frequency       : 12.345 ppm slow
Root delay      : 0.000123456 seconds
Root dispersion : 0.000012345 seconds
Update interval : 16.0 seconds
Leap status     : Normal
"#;
        
        let tracking = parse_chrony_tracking(output).unwrap();
        assert_eq!(tracking.stratum, 4);
        assert!(tracking.offset_us < 0); // "fast" means negative offset
        assert_eq!(tracking.leap_status, "Normal");
    }
}
```

### 2.2 Integration with Existing Code

Update `binance_hft_ingest.rs` to use the new timestamp types:

```rust
// In binance_hft_ingest.rs, update process_message:

use crate::performance::latency::time_sync::{MonoTs, WallTs, clock_health, one_way_latency_us};

fn process_message(&self, msg: &str, receive_mono: MonoTs) {
    let receive_wall = WallTs::now();
    let start_decode = MonoTs::now();
    
    let parsed = match self.parse_book_ticker(msg) {
        Some(p) => p,
        None => {
            self.stats.parse_errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    
    let end_decode = MonoTs::now();
    
    // Internal latency (always valid, monotonic-based)
    let decode_latency_us = end_decode.duration_since(start_decode).as_micros() as u64;
    
    // One-way latency (only valid if clock is synced)
    let wire_latency_us = one_way_latency_us(parsed.timestamp, receive_wall);
    
    // Record metrics
    self.stats.record_decode_latency(decode_latency_us);
    if let Some(wire_us) = wire_latency_us {
        self.stats.record_wire_latency(wire_us as u64);
    }
    
    // ... rest of processing
}
```

---

## 3. Validation Steps

### 3.1 Chrony Sync Verification

```bash
#!/bin/bash
# validate_time_sync.sh

echo "=== Chrony Tracking ==="
chronyc tracking

echo ""
echo "=== NTP Sources ==="
chronyc sources -v

echo ""
echo "=== Source Statistics ==="
chronyc sourcestats

echo ""
echo "=== Validation Checks ==="

# Check 1: Stratum <= 4
STRATUM=$(chronyc tracking | grep "Stratum" | awk '{print $3}')
if [ "$STRATUM" -le 4 ]; then
    echo "✓ Stratum check PASSED (stratum=$STRATUM)"
else
    echo "✗ Stratum check FAILED (stratum=$STRATUM, expected <= 4)"
fi

# Check 2: Offset < 1ms
OFFSET=$(chronyc tracking | grep "System time" | awk '{print $4}')
OFFSET_MS=$(echo "$OFFSET * 1000" | bc -l)
if (( $(echo "$OFFSET_MS < 1" | bc -l) )); then
    echo "✓ Offset check PASSED (offset=${OFFSET_MS}ms)"
else
    echo "✗ Offset check FAILED (offset=${OFFSET_MS}ms, expected < 1ms)"
fi

# Check 3: Root delay < 100ms
ROOT_DELAY=$(chronyc tracking | grep "Root delay" | awk '{print $4}')
ROOT_DELAY_MS=$(echo "$ROOT_DELAY * 1000" | bc -l)
if (( $(echo "$ROOT_DELAY_MS < 100" | bc -l) )); then
    echo "✓ Root delay check PASSED (delay=${ROOT_DELAY_MS}ms)"
else
    echo "✗ Root delay check FAILED (delay=${ROOT_DELAY_MS}ms, expected < 100ms)"
fi

# Check 4: Leap status is Normal
LEAP_STATUS=$(chronyc tracking | grep "Leap status" | awk '{print $4}')
if [ "$LEAP_STATUS" == "Normal" ]; then
    echo "✓ Leap status check PASSED"
else
    echo "⚠ Leap status: $LEAP_STATUS (may indicate leap second pending)"
fi
```

### 3.2 Clock Step Detection Test

```rust
#[cfg(test)]
mod clock_step_tests {
    use super::*;
    use std::process::Command;

    #[test]
    #[ignore] // Requires root privileges
    fn test_clock_step_detection() {
        let monitor = ClockHealthMonitor::new();
        
        // Warm up
        for _ in 0..10 {
            monitor.update();
            std::thread::sleep(Duration::from_millis(100));
        }
        
        let health_before = monitor.health();
        assert!(!health_before.step_detected, "No step should be detected initially");
        
        // Simulate clock step (requires root)
        // This would be done manually in testing:
        // sudo date -s "$(date -d '+1 second')"
        
        // After step, next update should detect it
        monitor.update();
        let health_after = monitor.health();
        
        // In real test with actual clock step:
        // assert!(health_after.step_detected);
    }
}
```

### 3.3 One-Way Latency Validation

```bash
# Compare one-way estimate with RTT/2

# 1. Measure RTT via WebSocket ping/pong
# 2. Measure one-way via exchange_ts - local_ts
# 3. Validate: one_way should be approximately RTT/2 ± NTP accuracy

# Example validation in Rust:
fn validate_one_way_estimate(rtt_us: u64, one_way_us: i64) -> bool {
    let estimated_one_way = rtt_us as i64 / 2;
    let tolerance_us = 1000; // 1ms tolerance for NTP accuracy
    
    (one_way_us - estimated_one_way).abs() < tolerance_us
}
```

---

## 4. Monitoring Dashboard

### 4.1 Key Metrics to Track

| Metric | Source | Alert Threshold |
|--------|--------|-----------------|
| `chrony_offset_us` | chrony tracking | > 1000 μs |
| `chrony_root_delay_us` | chrony tracking | > 100000 μs |
| `chrony_stratum` | chrony tracking | > 4 |
| `clock_step_count` | ClockHealthMonitor | > 0 in 1h window |
| `one_way_latency_p99_us` | Latency harness | > 5000 μs |
| `internal_latency_p99_us` | Latency harness | > 100 μs |

### 4.2 Grafana Queries (Prometheus format)

```promql
# Chrony offset (gauge)
chrony_system_clock_offset_seconds * 1000000

# Clock step events (counter)
increase(clock_step_events_total[1h])

# One-way latency P99 (when valid)
histogram_quantile(0.99, 
  rate(binance_one_way_latency_us_bucket{valid="true"}[5m])
)

# Internal latency P99
histogram_quantile(0.99, 
  rate(binance_internal_latency_us_bucket[5m])
)
```

---

## 5. Summary of Best Practices

1. **ALWAYS use MonoTs (CLOCK_MONOTONIC_RAW) for internal latency measurements**
2. **ONLY use WallTs for correlation with exchange timestamps**
3. **CHECK ClockHealth.one_way_metrics_valid() before trusting one-way latency**
4. **RUN chrony with aggressive polling (minpoll 0-1) for sub-ms accuracy**
5. **MONITOR for clock steps and invalidate metrics during cooldown**
6. **VALIDATE RTT/2 ≈ one-way estimate to confirm NTP accuracy**
7. **LOG clock steps for debugging and post-incident analysis**

---

## 6. Failure Modes & Recovery

| Failure | Detection | Recovery |
|---------|-----------|----------|
| NTP server unreachable | `chronyc sources` shows offline | Fallback to secondary pools |
| Clock step | `|Δwall - Δmono| > threshold` | Invalidate one-way metrics for 5s |
| Large offset | `offset_us > 1ms` | Alert, increase poll rate |
| Leap second | `Leap status != Normal` | Use kernel leap handling, warn |
| Hardware clock drift | RMS offset increasing | Check tempcomp, battery |
