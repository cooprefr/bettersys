//! Binance WebSocket Session Management
//!
//! Production-grade fault-tolerant reconnect and session management:
//! - State machine with well-defined transitions
//! - Exponential backoff with jitter (thundering herd prevention)
//! - Endpoint rotation with circuit breakers
//! - Heartbeat monitoring (ping/pong + data staleness)
//! - Proactive reconnection before 24h hard limit
//! - State resync coordination post-reconnect
//!
//! Design principles:
//! - Minimize downtime through fast failover
//! - Never thundering-herd on mass reconnects
//! - Low-latency logging (hot path has zero logging)

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use tracing::{debug, info, warn};

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Production-tuned session configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    // Backoff parameters
    pub backoff_base_ms: u64,
    pub backoff_max_ms: u64,
    pub backoff_multiplier: f64,
    pub jitter_factor: f64,

    // Connection timeouts
    pub connect_timeout_ms: u64,
    pub subscribe_timeout_ms: u64,

    // Heartbeat parameters
    pub ping_interval_ms: u64,
    pub pong_timeout_ms: u64,
    pub stale_data_timeout_ms: u64,
    pub consecutive_stale_threshold: u32,

    // Proactive refresh
    pub proactive_refresh_secs: u64,
    pub hard_timeout_secs: u64,

    // Endpoint rotation
    pub circuit_breaker_threshold: u32,
    pub circuit_breaker_cooldown_secs: u64,

    // Resync
    pub resync_grace_period_ms: u64,
    pub updates_to_sync: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            // Backoff: 100ms base, 2x multiplier, 30s cap, ±30% jitter
            backoff_base_ms: 100,
            backoff_max_ms: 30_000,
            backoff_multiplier: 2.0,
            jitter_factor: 0.3,

            // Timeouts
            connect_timeout_ms: 10_000,
            subscribe_timeout_ms: 5_000,

            // Heartbeat
            ping_interval_ms: 30_000,
            pong_timeout_ms: 10_000,
            stale_data_timeout_ms: 5_000,
            consecutive_stale_threshold: 3,

            // Proactive refresh at 23h (Binance hard-closes at 24h)
            proactive_refresh_secs: 23 * 3600,
            hard_timeout_secs: 24 * 3600,

            // Circuit breaker
            circuit_breaker_threshold: 3,
            circuit_breaker_cooldown_secs: 60,

            // Resync
            resync_grace_period_ms: 500,
            updates_to_sync: 3,
        }
    }
}

impl SessionConfig {
    /// Load from environment with defaults
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(v) = std::env::var("BINANCE_BACKOFF_BASE_MS") {
            config.backoff_base_ms = v.parse().unwrap_or(config.backoff_base_ms);
        }
        if let Ok(v) = std::env::var("BINANCE_BACKOFF_MAX_MS") {
            config.backoff_max_ms = v.parse().unwrap_or(config.backoff_max_ms);
        }
        if let Ok(v) = std::env::var("BINANCE_CONNECT_TIMEOUT_MS") {
            config.connect_timeout_ms = v.parse().unwrap_or(config.connect_timeout_ms);
        }
        if let Ok(v) = std::env::var("BINANCE_PING_INTERVAL_MS") {
            config.ping_interval_ms = v.parse().unwrap_or(config.ping_interval_ms);
        }
        if let Ok(v) = std::env::var("BINANCE_STALE_DATA_TIMEOUT_MS") {
            config.stale_data_timeout_ms = v.parse().unwrap_or(config.stale_data_timeout_ms);
        }

        config
    }
}

// =============================================================================
// STATE MACHINE
// =============================================================================

/// Connection state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Initial state before any connection attempt
    Init,
    /// TCP + TLS + WebSocket upgrade in progress
    Connecting,
    /// WebSocket connected, waiting for subscription ACK
    Subscribing,
    /// Actively receiving market data
    Streaming,
    /// Connection lost, waiting for backoff timer
    Reconnecting,
    /// Graceful shutdown requested
    Shutdown,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init => write!(f, "INIT"),
            Self::Connecting => write!(f, "CONNECTING"),
            Self::Subscribing => write!(f, "SUBSCRIBING"),
            Self::Streaming => write!(f, "STREAMING"),
            Self::Reconnecting => write!(f, "RECONNECTING"),
            Self::Shutdown => write!(f, "SHUTDOWN"),
        }
    }
}

/// Reason for state transition (for logging/metrics)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    Started,
    ConnectSuccess,
    SubscribeSuccess,
    ConnectTimeout,
    SubscribeTimeout,
    PongTimeout,
    DataStale,
    ServerClose,
    NetworkError,
    ProactiveRefresh,
    HardTimeout,
    ShutdownRequested,
}

impl std::fmt::Display for TransitionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Started => write!(f, "started"),
            Self::ConnectSuccess => write!(f, "connect_ok"),
            Self::SubscribeSuccess => write!(f, "subscribe_ok"),
            Self::ConnectTimeout => write!(f, "connect_timeout"),
            Self::SubscribeTimeout => write!(f, "subscribe_timeout"),
            Self::PongTimeout => write!(f, "pong_timeout"),
            Self::DataStale => write!(f, "data_stale"),
            Self::ServerClose => write!(f, "server_close"),
            Self::NetworkError => write!(f, "network_error"),
            Self::ProactiveRefresh => write!(f, "proactive_refresh"),
            Self::HardTimeout => write!(f, "hard_timeout"),
            Self::ShutdownRequested => write!(f, "shutdown"),
        }
    }
}

// =============================================================================
// EXPONENTIAL BACKOFF WITH JITTER
// =============================================================================

/// Backoff calculator with jitter for thundering herd prevention
#[derive(Debug)]
pub struct BackoffCalculator {
    config: SessionConfig,
    attempt: u32,
    rng_state: u64,
}

impl BackoffCalculator {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            attempt: 0,
            rng_state: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(12345),
        }
    }

    /// Fast PRNG for jitter (xorshift64)
    #[inline]
    fn next_random(&mut self) -> f64 {
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as f64) / (u64::MAX as f64)
    }

    /// Compute next backoff duration with jitter
    pub fn next_backoff(&mut self) -> Duration {
        let base = (self.config.backoff_base_ms as f64)
            * self.config.backoff_multiplier.powi(self.attempt as i32);
        let capped = base.min(self.config.backoff_max_ms as f64);

        // Jitter: ±jitter_factor (e.g., ±30%)
        let jitter_range = capped * self.config.jitter_factor;
        let jitter = (self.next_random() * 2.0 - 1.0) * jitter_range;
        let final_ms = (capped + jitter).max(self.config.backoff_base_ms as f64);

        self.attempt += 1;

        Duration::from_millis(final_ms as u64)
    }

    /// Reset on successful connection
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Current attempt number
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

// =============================================================================
// ENDPOINT ROTATION WITH CIRCUIT BREAKERS
// =============================================================================

/// Binance WebSocket endpoints
pub const BINANCE_ENDPOINTS: &[&str] = &[
    "wss://stream.binance.com:9443/ws",      // Primary
    "wss://stream.binance.com:443/ws",       // Firewall-friendly
    "wss://data-stream.binance.com:9443/ws", // Alternative
];

/// Per-endpoint circuit breaker state
#[derive(Debug, Clone, Copy)]
struct EndpointState {
    consecutive_failures: u32,
    circuit_open_until: Option<Instant>,
}

impl Default for EndpointState {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            circuit_open_until: None,
        }
    }
}

/// Endpoint rotator with circuit breakers
#[derive(Debug)]
pub struct EndpointRotator {
    endpoints: Vec<String>,
    current_idx: usize,
    states: Vec<EndpointState>,
    config: SessionConfig,
}

impl EndpointRotator {
    pub fn new(config: SessionConfig) -> Self {
        let endpoints: Vec<String> = BINANCE_ENDPOINTS.iter().map(|s| s.to_string()).collect();
        let states = vec![EndpointState::default(); endpoints.len()];

        Self {
            endpoints,
            current_idx: 0,
            states,
            config,
        }
    }

    /// Get current endpoint URL
    pub fn current(&self) -> &str {
        &self.endpoints[self.current_idx]
    }

    /// Rotate to next available endpoint, respecting circuit breakers
    pub fn rotate(&mut self) -> &str {
        let now = Instant::now();

        for _ in 0..self.endpoints.len() {
            self.current_idx = (self.current_idx + 1) % self.endpoints.len();
            let state = &mut self.states[self.current_idx];

            // Check circuit breaker
            if let Some(open_until) = state.circuit_open_until {
                if now < open_until {
                    // Circuit still open, skip this endpoint
                    continue;
                }
                // Circuit half-open: allow one attempt
                state.circuit_open_until = None;
                debug!(
                    endpoint = self.endpoints[self.current_idx],
                    "circuit_half_open"
                );
            }

            return &self.endpoints[self.current_idx];
        }

        // All circuits open: force primary endpoint
        warn!("all_circuits_open, forcing primary endpoint");
        self.current_idx = 0;
        &self.endpoints[0]
    }

    /// Record failure for current endpoint
    pub fn record_failure(&mut self, reason: TransitionReason) {
        let idx = self.current_idx;
        let state = &mut self.states[idx];
        state.consecutive_failures += 1;

        if state.consecutive_failures >= self.config.circuit_breaker_threshold {
            let cooldown = Duration::from_secs(self.config.circuit_breaker_cooldown_secs);
            state.circuit_open_until = Some(Instant::now() + cooldown);
            warn!(
                endpoint = self.endpoints[idx],
                failures = state.consecutive_failures,
                cooldown_secs = cooldown.as_secs(),
                reason = %reason,
                "circuit_opened"
            );
        }
    }

    /// Record success for current endpoint
    pub fn record_success(&mut self) {
        let idx = self.current_idx;
        let state = &mut self.states[idx];

        if state.consecutive_failures > 0 {
            debug!(
                endpoint = self.endpoints[idx],
                prev_failures = state.consecutive_failures,
                "endpoint_recovered"
            );
        }

        state.consecutive_failures = 0;
        state.circuit_open_until = None;
    }

    /// Check if we should rotate (based on failure reason)
    pub fn should_rotate(&self, reason: TransitionReason) -> bool {
        matches!(
            reason,
            TransitionReason::ConnectTimeout
                | TransitionReason::SubscribeTimeout
                | TransitionReason::PongTimeout
                | TransitionReason::NetworkError
        )
    }
}

// =============================================================================
// HEARTBEAT MONITOR
// =============================================================================

/// Result of heartbeat check
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatAction {
    /// Everything OK, continue streaming
    Ok,
    /// Time to send a ping
    SendPing,
    /// Pong not received in time
    PongTimeout,
    /// No market data received recently
    DataStale,
}

/// Heartbeat monitoring for connection health
#[derive(Debug)]
pub struct HeartbeatMonitor {
    config: SessionConfig,
    last_ping_sent: Option<Instant>,
    awaiting_pong: bool,
    last_data_received: Instant,
    consecutive_stale_checks: u32,
}

impl HeartbeatMonitor {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            last_ping_sent: None,
            awaiting_pong: false,
            last_data_received: Instant::now(),
            consecutive_stale_checks: 0,
        }
    }

    /// Reset state for new connection
    pub fn reset(&mut self) {
        self.last_ping_sent = None;
        self.awaiting_pong = false;
        self.last_data_received = Instant::now();
        self.consecutive_stale_checks = 0;
    }

    /// Record that we received market data
    #[inline]
    pub fn record_data_received(&mut self) {
        self.last_data_received = Instant::now();
        self.consecutive_stale_checks = 0;
    }

    /// Record that we sent a ping
    pub fn record_ping_sent(&mut self) {
        self.last_ping_sent = Some(Instant::now());
        self.awaiting_pong = true;
    }

    /// Record that we received a pong
    pub fn record_pong_received(&mut self) {
        self.awaiting_pong = false;
    }

    /// Check heartbeat status and return required action
    pub fn check(&mut self) -> HeartbeatAction {
        let now = Instant::now();

        // Check pong timeout
        if self.awaiting_pong {
            if let Some(ping_time) = self.last_ping_sent {
                if now.duration_since(ping_time)
                    > Duration::from_millis(self.config.pong_timeout_ms)
                {
                    return HeartbeatAction::PongTimeout;
                }
            }
        }

        // Check data staleness
        let data_age = now.duration_since(self.last_data_received);
        if data_age > Duration::from_millis(self.config.stale_data_timeout_ms) {
            self.consecutive_stale_checks += 1;
            if self.consecutive_stale_checks >= self.config.consecutive_stale_threshold {
                return HeartbeatAction::DataStale;
            }
        }

        // Check if we need to send ping
        let should_ping = match self.last_ping_sent {
            None => true,
            Some(ping_time) => {
                now.duration_since(ping_time)
                    > Duration::from_millis(self.config.ping_interval_ms)
            }
        };

        if should_ping && !self.awaiting_pong {
            return HeartbeatAction::SendPing;
        }

        HeartbeatAction::Ok
    }

    /// Time until next required heartbeat check
    pub fn time_until_next_check(&self) -> Duration {
        let now = Instant::now();

        // If awaiting pong, check frequently
        if self.awaiting_pong {
            if let Some(ping_time) = self.last_ping_sent {
                let elapsed = now.duration_since(ping_time);
                let timeout = Duration::from_millis(self.config.pong_timeout_ms);
                if elapsed < timeout {
                    return timeout - elapsed;
                }
            }
            return Duration::from_millis(100);
        }

        // Otherwise, check based on ping interval and stale timeout
        let stale_check = Duration::from_millis(self.config.stale_data_timeout_ms / 2);
        let ping_check = match self.last_ping_sent {
            None => Duration::ZERO,
            Some(ping_time) => {
                let elapsed = now.duration_since(ping_time);
                let interval = Duration::from_millis(self.config.ping_interval_ms);
                if elapsed < interval {
                    interval - elapsed
                } else {
                    Duration::ZERO
                }
            }
        };

        stale_check.min(ping_check).max(Duration::from_millis(100))
    }
}

// =============================================================================
// RESYNC COORDINATOR
// =============================================================================

/// Per-symbol resync state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncState {
    /// Just reconnected, waiting for first update
    Resyncing,
    /// Received some data, not yet stable
    PartialSync,
    /// Normal operation
    Synced,
}

/// Per-symbol state tracking for resync
#[derive(Debug)]
pub struct SymbolResyncState {
    state: ResyncState,
    updates_since_reconnect: u32,
    updates_to_sync: u32,
}

impl SymbolResyncState {
    pub fn new(updates_to_sync: u32) -> Self {
        Self {
            state: ResyncState::Synced,
            updates_since_reconnect: 0,
            updates_to_sync,
        }
    }

    /// Mark symbol as resyncing (call on reconnect)
    pub fn mark_resyncing(&mut self) {
        self.state = ResyncState::Resyncing;
        self.updates_since_reconnect = 0;
    }

    /// Record an update for this symbol
    #[inline]
    pub fn record_update(&mut self) {
        self.updates_since_reconnect += 1;
        match self.state {
            ResyncState::Resyncing => {
                self.state = ResyncState::PartialSync;
            }
            ResyncState::PartialSync if self.updates_since_reconnect >= self.updates_to_sync => {
                self.state = ResyncState::Synced;
            }
            _ => {}
        }
    }

    /// Check if symbol data is tradeable
    #[inline]
    pub fn is_synced(&self) -> bool {
        matches!(self.state, ResyncState::Synced)
    }

    pub fn state(&self) -> ResyncState {
        self.state
    }
}

/// Coordinates resync across all symbols
#[derive(Debug)]
pub struct ResyncCoordinator {
    config: SessionConfig,
    symbols: Vec<String>,
    symbol_states: RwLock<Vec<SymbolResyncState>>,
    connection_start: RwLock<Option<Instant>>,
    trading_enabled_at: RwLock<Option<Instant>>,
}

impl ResyncCoordinator {
    pub fn new(config: SessionConfig, symbols: Vec<String>) -> Self {
        let symbol_states = symbols
            .iter()
            .map(|_| SymbolResyncState::new(config.updates_to_sync))
            .collect();

        Self {
            config,
            symbols,
            symbol_states: RwLock::new(symbol_states),
            connection_start: RwLock::new(None),
            trading_enabled_at: RwLock::new(None),
        }
    }

    /// Call when connection is established
    pub fn on_connect(&self) {
        let now = Instant::now();
        *self.connection_start.write() = Some(now);
        *self.trading_enabled_at.write() =
            Some(now + Duration::from_millis(self.config.resync_grace_period_ms));

        // Mark all symbols as resyncing
        let mut states = self.symbol_states.write();
        for state in states.iter_mut() {
            state.mark_resyncing();
        }

        info!(
            grace_period_ms = self.config.resync_grace_period_ms,
            "resync_started"
        );
    }

    /// Record an update for a symbol
    #[inline]
    pub fn record_update(&self, symbol_idx: usize) {
        let mut states = self.symbol_states.write();
        if let Some(state) = states.get_mut(symbol_idx) {
            state.record_update();
        }
    }

    /// Check if trading is allowed (grace period passed)
    #[inline]
    pub fn is_trading_enabled(&self) -> bool {
        match *self.trading_enabled_at.read() {
            Some(enabled_at) => Instant::now() >= enabled_at,
            None => false,
        }
    }

    /// Check if a specific symbol is synced
    #[inline]
    pub fn is_symbol_synced(&self, symbol_idx: usize) -> bool {
        let states = self.symbol_states.read();
        states
            .get(symbol_idx)
            .map(|s| s.is_synced())
            .unwrap_or(false)
    }

    /// Check if symbol is tradeable (grace period passed + symbol synced)
    #[inline]
    pub fn is_symbol_tradeable(&self, symbol_idx: usize) -> bool {
        self.is_trading_enabled() && self.is_symbol_synced(symbol_idx)
    }

    /// Get overall sync status
    pub fn sync_status(&self) -> (usize, usize) {
        let states = self.symbol_states.read();
        let synced = states.iter().filter(|s| s.is_synced()).count();
        (synced, states.len())
    }

    /// Check if proactive refresh is needed
    pub fn needs_proactive_refresh(&self) -> bool {
        match *self.connection_start.read() {
            Some(start) => {
                start.elapsed() > Duration::from_secs(self.config.proactive_refresh_secs)
            }
            None => false,
        }
    }

    /// Check if hard timeout exceeded
    pub fn is_hard_timeout(&self) -> bool {
        match *self.connection_start.read() {
            Some(start) => start.elapsed() > Duration::from_secs(self.config.hard_timeout_secs),
            None => false,
        }
    }

    /// Connection duration
    pub fn connection_duration(&self) -> Option<Duration> {
        self.connection_start.read().map(|s| s.elapsed())
    }
}

// =============================================================================
// SESSION METRICS
// =============================================================================

/// Session metrics for monitoring
#[derive(Debug, Default)]
pub struct SessionMetrics {
    pub connections_attempted: AtomicU64,
    pub connections_succeeded: AtomicU64,
    pub connections_failed: AtomicU64,
    pub reconnections: AtomicU64,
    pub endpoint_rotations: AtomicU64,
    pub circuit_breaker_trips: AtomicU64,
    pub pong_timeouts: AtomicU64,
    pub data_stale_events: AtomicU64,
    pub proactive_refreshes: AtomicU64,
    pub hard_timeouts: AtomicU64,
    pub total_downtime_ms: AtomicU64,
}

impl SessionMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn record_connect_attempt(&self) {
        self.connections_attempted.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_connect_success(&self) {
        self.connections_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_connect_failure(&self) {
        self.connections_failed.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_reconnection(&self) {
        self.reconnections.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_endpoint_rotation(&self) {
        self.endpoint_rotations.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_circuit_breaker_trip(&self) {
        self.circuit_breaker_trips.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_pong_timeout(&self) {
        self.pong_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_data_stale(&self) {
        self.data_stale_events.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_proactive_refresh(&self) {
        self.proactive_refreshes.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_hard_timeout(&self) {
        self.hard_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_downtime(&self, duration: Duration) {
        self.total_downtime_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    /// Summary string for logging
    pub fn summary(&self) -> String {
        format!(
            "connects={}/{} reconnects={} rotations={} pong_timeouts={} stale={} proactive={} hard_timeout={} downtime_ms={}",
            self.connections_succeeded.load(Ordering::Relaxed),
            self.connections_attempted.load(Ordering::Relaxed),
            self.reconnections.load(Ordering::Relaxed),
            self.endpoint_rotations.load(Ordering::Relaxed),
            self.pong_timeouts.load(Ordering::Relaxed),
            self.data_stale_events.load(Ordering::Relaxed),
            self.proactive_refreshes.load(Ordering::Relaxed),
            self.hard_timeouts.load(Ordering::Relaxed),
            self.total_downtime_ms.load(Ordering::Relaxed),
        )
    }
}

// =============================================================================
// SESSION MANAGER
// =============================================================================

/// Complete session manager coordinating all components
#[derive(Debug)]
pub struct SessionManager {
    config: SessionConfig,
    state: RwLock<SessionState>,
    backoff: RwLock<BackoffCalculator>,
    endpoints: RwLock<EndpointRotator>,
    heartbeat: RwLock<HeartbeatMonitor>,
    resync: ResyncCoordinator,
    metrics: SessionMetrics,
    disconnect_time: RwLock<Option<Instant>>,
}

impl SessionManager {
    pub fn new(config: SessionConfig, symbols: Vec<String>) -> Self {
        let backoff = BackoffCalculator::new(config.clone());
        let endpoints = EndpointRotator::new(config.clone());
        let heartbeat = HeartbeatMonitor::new(config.clone());
        let resync = ResyncCoordinator::new(config.clone(), symbols);

        Self {
            config,
            state: RwLock::new(SessionState::Init),
            backoff: RwLock::new(backoff),
            endpoints: RwLock::new(endpoints),
            heartbeat: RwLock::new(heartbeat),
            resync,
            metrics: SessionMetrics::new(),
            disconnect_time: RwLock::new(None),
        }
    }

    /// Current state
    pub fn state(&self) -> SessionState {
        *self.state.read()
    }

    /// Transition to new state with reason
    pub fn transition(&self, new_state: SessionState, reason: TransitionReason) {
        let old_state = {
            let mut state = self.state.write();
            let old = *state;
            *state = new_state;
            old
        };

        // State-specific actions
        match (old_state, new_state) {
            (_, SessionState::Connecting) => {
                self.metrics.record_connect_attempt();
                if old_state == SessionState::Reconnecting {
                    // Track downtime
                    if let Some(disc_time) = *self.disconnect_time.read() {
                        self.metrics.add_downtime(disc_time.elapsed());
                    }
                }
            }
            (_, SessionState::Streaming) => {
                self.metrics.record_connect_success();
                self.backoff.write().reset();
                self.endpoints.write().record_success();
                self.heartbeat.write().reset();
                self.resync.on_connect();
            }
            (_, SessionState::Reconnecting) => {
                self.metrics.record_reconnection();
                *self.disconnect_time.write() = Some(Instant::now());

                // Record failure and maybe rotate endpoint
                let should_rotate = self.endpoints.read().should_rotate(reason);
                self.endpoints.write().record_failure(reason);

                if should_rotate {
                    self.endpoints.write().rotate();
                    self.metrics.record_endpoint_rotation();
                }

                // Update metrics by reason
                match reason {
                    TransitionReason::PongTimeout => self.metrics.record_pong_timeout(),
                    TransitionReason::DataStale => self.metrics.record_data_stale(),
                    TransitionReason::ProactiveRefresh => self.metrics.record_proactive_refresh(),
                    TransitionReason::HardTimeout => self.metrics.record_hard_timeout(),
                    _ => self.metrics.record_connect_failure(),
                }
            }
            _ => {}
        }

        // Log transition (cold path, OK to allocate)
        info!(
            from = %old_state,
            to = %new_state,
            reason = %reason,
            endpoint = self.endpoints.read().current(),
            "session_transition"
        );
    }

    /// Get current endpoint URL
    pub fn current_endpoint(&self) -> String {
        self.endpoints.read().current().to_string()
    }

    /// Get next backoff duration
    pub fn next_backoff(&self) -> Duration {
        self.backoff.write().next_backoff()
    }

    /// Current backoff attempt number
    pub fn backoff_attempt(&self) -> u32 {
        self.backoff.read().attempt()
    }

    /// Record that market data was received (hot path)
    #[inline]
    pub fn record_data_received(&self, symbol_idx: usize) {
        self.heartbeat.write().record_data_received();
        self.resync.record_update(symbol_idx);
    }

    /// Record ping sent
    pub fn record_ping_sent(&self) {
        self.heartbeat.write().record_ping_sent();
    }

    /// Record pong received
    pub fn record_pong_received(&self) {
        self.heartbeat.write().record_pong_received();
    }

    /// Check heartbeat status
    pub fn check_heartbeat(&self) -> HeartbeatAction {
        self.heartbeat.write().check()
    }

    /// Time until next heartbeat check
    pub fn heartbeat_check_interval(&self) -> Duration {
        self.heartbeat.read().time_until_next_check()
    }

    /// Check if proactive refresh is needed
    pub fn needs_proactive_refresh(&self) -> bool {
        self.resync.needs_proactive_refresh()
    }

    /// Check if hard timeout exceeded
    pub fn is_hard_timeout(&self) -> bool {
        self.resync.is_hard_timeout()
    }

    /// Check if trading is enabled
    #[inline]
    pub fn is_trading_enabled(&self) -> bool {
        self.resync.is_trading_enabled()
    }

    /// Check if symbol is tradeable
    #[inline]
    pub fn is_symbol_tradeable(&self, symbol_idx: usize) -> bool {
        self.resync.is_symbol_tradeable(symbol_idx)
    }

    /// Get sync status
    pub fn sync_status(&self) -> (usize, usize) {
        self.resync.sync_status()
    }

    /// Connection timeout duration
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.config.connect_timeout_ms)
    }

    /// Subscribe timeout duration
    pub fn subscribe_timeout(&self) -> Duration {
        Duration::from_millis(self.config.subscribe_timeout_ms)
    }

    /// Get metrics reference
    pub fn metrics(&self) -> &SessionMetrics {
        &self.metrics
    }

    /// Get config reference
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_calculator() {
        let config = SessionConfig::default();
        let mut backoff = BackoffCalculator::new(config);

        // First backoff should be around 100ms (with jitter)
        let d1 = backoff.next_backoff();
        assert!(d1.as_millis() >= 70 && d1.as_millis() <= 130);

        // Second should be around 200ms
        let d2 = backoff.next_backoff();
        assert!(d2.as_millis() >= 140 && d2.as_millis() <= 260);

        // After reset, should be back to ~100ms
        backoff.reset();
        let d3 = backoff.next_backoff();
        assert!(d3.as_millis() >= 70 && d3.as_millis() <= 130);
    }

    #[test]
    fn test_backoff_cap() {
        let config = SessionConfig::default();
        let mut backoff = BackoffCalculator::new(config);

        // Run many iterations
        for _ in 0..20 {
            let d = backoff.next_backoff();
            // Should never exceed max + jitter
            assert!(d.as_millis() <= 39_000); // 30000 * 1.3
        }
    }

    #[test]
    fn test_endpoint_rotation() {
        let config = SessionConfig::default();
        let mut rotator = EndpointRotator::new(config);

        let e1 = rotator.current().to_string();
        let e2 = rotator.rotate().to_string();
        let e3 = rotator.rotate().to_string();

        // Should cycle through endpoints
        assert_ne!(e1, e2);
        assert_ne!(e2, e3);

        // Eventually wraps
        let e4 = rotator.rotate().to_string();
        assert_eq!(e1, e4);
    }

    #[test]
    fn test_circuit_breaker() {
        let mut config = SessionConfig::default();
        config.circuit_breaker_threshold = 2;
        let mut rotator = EndpointRotator::new(config);

        // Record failures to trip circuit
        rotator.record_failure(TransitionReason::ConnectTimeout);
        rotator.record_failure(TransitionReason::ConnectTimeout);

        // Current endpoint should now be circuit-open
        let idx_before = rotator.current_idx;
        rotator.rotate();

        // Should have skipped the circuit-open endpoint
        assert_ne!(rotator.current_idx, idx_before);
    }

    #[test]
    fn test_heartbeat_monitor() {
        let mut config = SessionConfig::default();
        config.ping_interval_ms = 100;
        config.stale_data_timeout_ms = 50;
        config.consecutive_stale_threshold = 2;

        let mut monitor = HeartbeatMonitor::new(config);

        // Initially should want to send ping
        assert_eq!(monitor.check(), HeartbeatAction::SendPing);

        monitor.record_ping_sent();

        // Now should be OK (waiting for pong)
        monitor.record_data_received();
        assert_eq!(monitor.check(), HeartbeatAction::Ok);

        monitor.record_pong_received();
    }

    #[test]
    fn test_resync_state() {
        let mut state = SymbolResyncState::new(3);

        state.mark_resyncing();
        assert!(!state.is_synced());

        state.record_update();
        assert_eq!(state.state(), ResyncState::PartialSync);
        assert!(!state.is_synced());

        state.record_update();
        state.record_update();
        assert_eq!(state.state(), ResyncState::Synced);
        assert!(state.is_synced());
    }

    #[test]
    fn test_session_manager_transitions() {
        let config = SessionConfig::default();
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let manager = SessionManager::new(config, symbols);

        assert_eq!(manager.state(), SessionState::Init);

        manager.transition(SessionState::Connecting, TransitionReason::Started);
        assert_eq!(manager.state(), SessionState::Connecting);

        manager.transition(SessionState::Subscribing, TransitionReason::ConnectSuccess);
        assert_eq!(manager.state(), SessionState::Subscribing);

        manager.transition(SessionState::Streaming, TransitionReason::SubscribeSuccess);
        assert_eq!(manager.state(), SessionState::Streaming);

        // Check metrics
        assert_eq!(
            manager.metrics.connections_attempted.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            manager.metrics.connections_succeeded.load(Ordering::Relaxed),
            1
        );
    }
}
