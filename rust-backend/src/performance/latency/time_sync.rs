//! Time Synchronization and Clock Health Monitoring
//!
//! Provides trustworthy timestamping for latency measurement by:
//! 1. Using monotonic clocks for all internal measurements
//! 2. Detecting clock steps that invalidate one-way latency metrics
//! 3. Exposing clock health status for metric validity decisions
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────┐     ┌─────────────────────┐
//! │  MONOTONIC DOMAIN   │     │  WALL-CLOCK DOMAIN  │
//! │  (CLOCK_MONOTONIC)  │     │  (CLOCK_REALTIME)   │
//! │                     │     │                     │
//! │  • Internal latency │     │  • Exchange ts      │
//! │  • Processing time  │     │  • One-way latency  │
//! │  • Jitter tracking  │     │  • Log timestamps   │
//! │                     │     │                     │
//! │  IMMUNE TO:         │     │  AFFECTED BY:       │
//! │  - NTP steps        │     │  - NTP corrections  │
//! │  - Leap seconds     │     │  - Clock drift      │
//! └─────────────────────┘     └─────────────────────┘
//! ```

use std::{
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

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

/// Maximum reasonable one-way latency (microseconds) - anything beyond is likely clock error
const MAX_REASONABLE_ONE_WAY_US: i64 = 10_000_000; // 10 seconds

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
        let ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, ts.as_mut_ptr()) };

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

    /// Milliseconds since epoch
    #[inline]
    pub fn as_millis(&self) -> u64 {
        self.nanos / 1_000_000
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

    /// Elapsed in microseconds
    #[inline]
    pub fn elapsed_us(&self) -> u64 {
        self.elapsed().as_micros() as u64
    }
}

impl Default for MonoTs {
    fn default() -> Self {
        Self::now()
    }
}

// =============================================================================
// WALL-CLOCK TIMESTAMP
// =============================================================================

/// Wall-clock timestamp for correlation with exchange timestamps.
///
/// WARNING: Only use for one-way latency estimation when ClockHealth.synced == true.
/// Never use for internal latency calculations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

    /// From Unix nanoseconds
    #[inline]
    pub fn from_nanos(nanos: i64) -> Self {
        Self { unix_nanos: nanos }
    }

    /// Unix nanoseconds
    #[inline]
    pub fn as_nanos(&self) -> i64 {
        self.unix_nanos
    }

    /// Unix microseconds
    #[inline]
    pub fn as_micros(&self) -> i64 {
        self.unix_nanos / 1_000
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

    /// Signed difference in microseconds (self - other)
    #[inline]
    pub fn diff_micros(&self, other: Self) -> i64 {
        self.diff_nanos(other) / 1_000
    }

    /// ISO 8601 string representation
    pub fn to_iso8601(&self) -> String {
        let secs = self.unix_nanos / 1_000_000_000;
        let nanos = (self.unix_nanos % 1_000_000_000) as u32;
        chrono::DateTime::from_timestamp(secs, nanos)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
            .unwrap_or_else(|| "INVALID".to_string())
    }
}

impl Default for WallTs {
    fn default() -> Self {
        Self::now()
    }
}

// =============================================================================
// SYNCHRONIZED TIMESTAMP PAIR
// =============================================================================

/// Atomic capture of both monotonic and wall-clock timestamps.
///
/// Used to detect clock steps by comparing Δmono vs Δwall over time.
#[derive(Debug, Clone, Copy, Default)]
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
// CLOCK HEALTH STATUS
// =============================================================================

/// Clock health status for metric validity decisions
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClockHealth {
    /// Whether the clock is considered synchronized (offset < threshold)
    pub synced: bool,
    /// Estimated offset from NTP time (microseconds, positive = local clock ahead)
    pub offset_us: i64,
    /// Whether a clock step was detected recently
    pub step_detected: bool,
    /// Milliseconds since last clock step (u64::MAX if never)
    pub ms_since_step: u64,
    /// Number of clock steps detected since startup
    pub step_count: u64,
    /// Last health check timestamp (monotonic nanos)
    pub last_check_mono_ns: u64,
}

impl ClockHealth {
    /// Check if one-way latency metrics are trustworthy
    #[inline]
    pub fn one_way_metrics_valid(&self) -> bool {
        self.synced && !self.step_detected && self.ms_since_step >= CLOCK_STEP_COOLDOWN_MS
    }
}

impl Default for ClockHealth {
    fn default() -> Self {
        Self {
            synced: false,
            offset_us: 0,
            step_detected: false,
            ms_since_step: u64::MAX,
            step_count: 0,
            last_check_mono_ns: 0,
        }
    }
}

// =============================================================================
// CLOCK HEALTH MONITOR
// =============================================================================

/// Clock health monitor that detects clock steps and tracks sync status
pub struct ClockHealthMonitor {
    /// Last timestamp pair for step detection
    last_pair: RwLock<Option<TimestampPair>>,
    /// Whether a clock step was detected
    step_detected: AtomicBool,
    /// Monotonic timestamp of last clock step (nanos)
    last_step_mono_ns: AtomicU64,
    /// Total clock steps detected
    step_count: AtomicU64,
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
            step_count: AtomicU64::new(0),
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
            let wall_delta_ns = current.wall.as_nanos() - prev.wall.as_nanos();

            // Clock step if wall jumped more than mono + threshold
            let diff_us = (wall_delta_ns as i64 - mono_delta_ns as i64).abs() / 1_000;

            if diff_us > CLOCK_STEP_THRESHOLD_US {
                self.step_detected.store(true, Ordering::Release);
                self.last_step_mono_ns
                    .store(current.mono.as_nanos(), Ordering::Release);
                self.step_count.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    diff_us = diff_us,
                    mono_delta_us = mono_delta_ns / 1000,
                    wall_delta_us = wall_delta_ns / 1000,
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
            if elapsed_ms >= CLOCK_STEP_COOLDOWN_MS {
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

        if !synced {
            tracing::warn!(
                offset_us = offset_us,
                threshold_us = MAX_ACCEPTABLE_OFFSET_US,
                "Clock offset exceeds acceptable threshold"
            );
        }
    }

    /// Force sync status (e.g., when chrony is unavailable but we trust the clock)
    pub fn force_synced(&self, synced: bool) {
        self.synced.store(synced, Ordering::Release);
    }

    /// Get current clock health status
    pub fn health(&self) -> ClockHealth {
        let now_mono_ns = MonoTs::now().as_nanos();
        let last_step = self.last_step_mono_ns.load(Ordering::Acquire);

        let ms_since_step = if last_step > 0 {
            (now_mono_ns.saturating_sub(last_step)) / 1_000_000
        } else {
            u64::MAX // Never had a step
        };

        ClockHealth {
            synced: self.synced.load(Ordering::Acquire),
            offset_us: self.offset_us.load(Ordering::Acquire),
            step_detected: self.step_detected.load(Ordering::Acquire),
            ms_since_step,
            step_count: self.step_count.load(Ordering::Relaxed),
            last_check_mono_ns: now_mono_ns,
        }
    }

    /// Check if one-way metrics are currently valid
    #[inline]
    pub fn one_way_valid(&self) -> bool {
        self.health().one_way_metrics_valid()
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

/// Parsed chrony tracking output
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChronyTracking {
    /// Current offset from NTP time (microseconds, negative = local behind NTP)
    pub offset_us: i64,
    /// RMS offset (microseconds)
    pub rms_offset_us: i64,
    /// Root delay (microseconds)
    pub root_delay_us: i64,
    /// Root dispersion (microseconds)
    pub root_dispersion_us: i64,
    /// Stratum level
    pub stratum: u8,
    /// Frequency error (ppm)
    pub frequency_ppm: f64,
    /// Leap second status
    pub leap_status: String,
    /// Reference ID (NTP server)
    pub reference_id: String,
}

/// Parse chrony tracking output to extract offset and other metrics
pub fn parse_chrony_tracking(output: &str) -> Option<ChronyTracking> {
    let mut tracking = ChronyTracking::default();

    for line in output.lines() {
        let line = line.trim();

        if line.starts_with("Reference ID") {
            if let Some(idx) = line.find(':') {
                tracking.reference_id = line[idx + 1..].trim().to_string();
            }
        } else if line.starts_with("Stratum") {
            if let Some(idx) = line.find(':') {
                let value_str = line[idx + 1..].trim();
                if let Ok(stratum) = value_str.parse::<u8>() {
                    tracking.stratum = stratum;
                }
            }
        } else if line.starts_with("System time") {
            // "System time     : 0.000000123 seconds fast of NTP time"
            // "System time     : 0.000000456 seconds slow of NTP time"
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        // "fast" means local clock is ahead of NTP (positive offset)
                        // "slow" means local clock is behind NTP (negative offset)
                        let sign = if value_part.contains("slow") {
                            -1.0
                        } else {
                            1.0
                        };
                        tracking.offset_us = (secs * sign * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("RMS offset") {
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
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        tracking.root_delay_us = (secs * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("Root dispersion") {
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(secs_idx) = value_part.find(" seconds") {
                    let secs_str = value_part[..secs_idx].trim();
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        tracking.root_dispersion_us = (secs * 1_000_000.0) as i64;
                    }
                }
            }
        } else if line.starts_with("Frequency") {
            // "Frequency       : 12.345 ppm slow"
            if let Some(idx) = line.find(':') {
                let value_part = line[idx + 1..].trim();
                if let Some(ppm_idx) = value_part.find(" ppm") {
                    let ppm_str = value_part[..ppm_idx].trim();
                    if let Ok(ppm) = ppm_str.parse::<f64>() {
                        let sign = if value_part.contains("slow") {
                            -1.0
                        } else {
                            1.0
                        };
                        tracking.frequency_ppm = ppm * sign;
                    }
                }
            }
        } else if line.starts_with("Leap status") {
            if let Some(idx) = line.find(':') {
                tracking.leap_status = line[idx + 1..].trim().to_string();
            }
        }
    }

    Some(tracking)
}

/// Read chrony tracking data asynchronously
#[cfg(unix)]
pub async fn read_chrony_tracking() -> Option<ChronyTracking> {
    use tokio::process::Command;

    let output = Command::new("chronyc")
        .arg("tracking")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        tracing::debug!(
            "chronyc tracking failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_chrony_tracking(&stdout)
}

#[cfg(not(unix))]
pub async fn read_chrony_tracking() -> Option<ChronyTracking> {
    None
}

/// Synchronously read chrony tracking (for initialization)
#[cfg(unix)]
pub fn read_chrony_tracking_sync() -> Option<ChronyTracking> {
    use std::process::Command;

    let output = Command::new("chronyc").arg("tracking").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_chrony_tracking(&stdout)
}

#[cfg(not(unix))]
pub fn read_chrony_tracking_sync() -> Option<ChronyTracking> {
    None
}

// =============================================================================
// LATENCY CALCULATION HELPERS
// =============================================================================

/// Calculate one-way latency from exchange timestamp to local receipt.
///
/// Returns `None` if clock health indicates metrics are untrustworthy,
/// or if the calculated latency is unreasonable (negative or > 10s).
#[inline]
pub fn one_way_latency_us(exchange_ts_ms: i64, receipt_wall: WallTs) -> Option<i64> {
    let health = clock_health().health();

    if !health.one_way_metrics_valid() {
        return None;
    }

    let exchange_wall = WallTs::from_millis(exchange_ts_ms);
    let latency_us = receipt_wall.diff_micros(exchange_wall);

    // Sanity check: latency should be positive and reasonable
    if latency_us > 0 && latency_us < MAX_REASONABLE_ONE_WAY_US {
        Some(latency_us)
    } else {
        None
    }
}

/// Calculate one-way latency without clock health check (use when you've already validated)
#[inline]
pub fn one_way_latency_us_unchecked(exchange_ts_ms: i64, receipt_wall: WallTs) -> i64 {
    let exchange_wall = WallTs::from_millis(exchange_ts_ms);
    receipt_wall.diff_micros(exchange_wall)
}

/// Calculate internal processing latency (always valid, uses monotonic)
#[inline]
pub fn internal_latency_us(start: MonoTs, end: MonoTs) -> u64 {
    end.duration_since(start).as_micros() as u64
}

/// Calculate internal processing latency in nanoseconds
#[inline]
pub fn internal_latency_ns(start: MonoTs, end: MonoTs) -> u64 {
    end.duration_since(start).as_nanos() as u64
}

// =============================================================================
// CLOCK HEALTH POLLING TASK
// =============================================================================

/// Start a background task that periodically updates clock health
pub fn start_clock_health_task() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let mut chrony_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    clock_health().update();
                }
                _ = chrony_interval.tick() => {
                    if let Some(tracking) = read_chrony_tracking().await {
                        clock_health().update_chrony_offset(tracking.offset_us);
                        tracing::debug!(
                            offset_us = tracking.offset_us,
                            rms_us = tracking.rms_offset_us,
                            stratum = tracking.stratum,
                            "Chrony tracking update"
                        );
                    }
                }
            }
        }
    })
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
    fn test_mono_ts_elapsed() {
        let t = MonoTs::now();
        std::thread::sleep(Duration::from_millis(10));
        assert!(t.elapsed_us() >= 10_000);
    }

    #[test]
    fn test_wall_ts_conversion() {
        let ts = WallTs::from_millis(1700000000000);
        assert_eq!(ts.as_millis(), 1700000000000);
        assert_eq!(ts.as_micros(), 1700000000000000);
    }

    #[test]
    fn test_wall_ts_diff() {
        let t1 = WallTs::from_millis(1000);
        let t2 = WallTs::from_millis(2000);

        assert_eq!(t2.diff_micros(t1), 1_000_000);
        assert_eq!(t1.diff_micros(t2), -1_000_000);
    }

    #[test]
    fn test_timestamp_pair() {
        let pair = TimestampPair::now();
        assert!(pair.mono.as_nanos() > 0);
        assert!(pair.wall.as_nanos() > 0);
    }

    #[test]
    fn test_clock_health_monitor_initial() {
        let monitor = ClockHealthMonitor::new();

        let health = monitor.health();
        assert!(!health.synced);
        assert!(!health.step_detected);
        assert_eq!(health.step_count, 0);
    }

    #[test]
    fn test_clock_health_monitor_sync() {
        let monitor = ClockHealthMonitor::new();

        // Update with good offset (500μs)
        monitor.update_chrony_offset(500);
        monitor.update();

        let health = monitor.health();
        assert!(health.synced);
        assert_eq!(health.offset_us, 500);
    }

    #[test]
    fn test_clock_health_monitor_bad_offset() {
        let monitor = ClockHealthMonitor::new();

        // Update with bad offset (5ms)
        monitor.update_chrony_offset(5000);
        monitor.update();

        let health = monitor.health();
        assert!(!health.synced);
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
        assert!(tracking.offset_us > 0); // "fast" means positive offset
        assert_eq!(tracking.offset_us, 0); // 0.000000123 rounds to 0 at microsecond precision
        assert_eq!(tracking.leap_status, "Normal");
        assert!(tracking.frequency_ppm < 0.0); // "slow" means negative
    }

    #[test]
    fn test_parse_chrony_tracking_slow() {
        let output = "System time     : 0.001234567 seconds slow of NTP time";
        let tracking = parse_chrony_tracking(output).unwrap();
        assert!(tracking.offset_us < 0); // "slow" means negative offset
        assert_eq!(tracking.offset_us, -1234); // ~1.234ms
    }

    #[test]
    fn test_internal_latency() {
        let start = MonoTs::now();
        std::thread::sleep(Duration::from_micros(100));
        let end = MonoTs::now();

        let latency = internal_latency_us(start, end);
        assert!(latency >= 100);
    }

    #[test]
    fn test_one_way_metrics_validity() {
        let health = ClockHealth {
            synced: true,
            offset_us: 100,
            step_detected: false,
            ms_since_step: 10_000, // 10 seconds since step
            step_count: 1,
            last_check_mono_ns: 0,
        };
        assert!(health.one_way_metrics_valid());

        let health_not_synced = ClockHealth {
            synced: false,
            ..health
        };
        assert!(!health_not_synced.one_way_metrics_valid());

        let health_step_detected = ClockHealth {
            step_detected: true,
            ..health
        };
        assert!(!health_step_detected.one_way_metrics_valid());

        let health_recent_step = ClockHealth {
            ms_since_step: 1000, // Only 1 second since step
            ..health
        };
        assert!(!health_recent_step.one_way_metrics_valid());
    }
}
