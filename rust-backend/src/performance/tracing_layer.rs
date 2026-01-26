//! Tracing Layer for Performance Analysis
//!
//! Integrates with the tracing ecosystem to enable:
//! - Flamegraph generation (via tracing-flame)
//! - Span timing for hot path detection
//! - Structured performance logging

use std::time::Instant;
use tracing::{span, Level, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

/// Performance-focused tracing layer
/// Records span timings to the global profiler
pub struct PerformanceLayer {
    min_duration_us: u64,
}

impl PerformanceLayer {
    pub fn new() -> Self {
        Self {
            min_duration_us: 100, // Only record spans > 100μs
        }
    }

    pub fn with_min_duration_us(mut self, us: u64) -> Self {
        self.min_duration_us = us;
        self
    }
}

impl Default for PerformanceLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for PerformanceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanTiming {
                entered_at: Instant::now(),
            });
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let extensions = span.extensions();
            if let Some(timing) = extensions.get::<SpanTiming>() {
                let duration_us = timing.entered_at.elapsed().as_micros() as u64;

                if duration_us >= self.min_duration_us {
                    // Record to profiler
                    let name = span.name();
                    crate::performance::global_profiler()
                        .cpu
                        .record_span(name, duration_us);
                }
            }
        }
    }
}

struct SpanTiming {
    entered_at: Instant,
}

/// Helper to create a performance-tracked span
#[macro_export]
macro_rules! perf_span {
    ($level:expr, $name:expr) => {
        tracing::span!($level, $name)
    };
    ($name:expr) => {
        tracing::span!(tracing::Level::INFO, $name)
    };
}

/// Configuration for tracing with performance tracking
pub struct TracingConfig {
    /// Enable performance layer
    pub performance_layer: bool,
    /// Enable flame layer for flamegraph generation
    pub flame_layer: bool,
    /// Flame output path
    pub flame_path: Option<String>,
    /// Minimum span duration to record (microseconds)
    pub min_span_duration_us: u64,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            performance_layer: true,
            flame_layer: false,
            flame_path: None,
            min_span_duration_us: 100,
        }
    }
}

/// Dummy guard for tracing shutdown
pub struct TracingGuard;

impl Drop for TracingGuard {
    fn drop(&mut self) {
        // Cleanup if needed
    }
}

/// Initialize tracing with performance tracking
///
/// Returns a guard that must be kept alive for flame output
pub fn init_tracing(config: TracingConfig) -> Option<TracingGuard> {
    use tracing_subscriber::prelude::*;

    let base_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    let perf_layer = if config.performance_layer {
        Some(PerformanceLayer::new().with_min_duration_us(config.min_span_duration_us))
    } else {
        None
    };

    // Note: For flame layer, add tracing-flame to Cargo.toml:
    // tracing-flame = "0.2"
    //
    // Then enable with:
    // let (flame_layer, guard) = tracing_flame::FlameLayer::with_file("./flame.folded").unwrap();
    //
    // The returned guard writes output on drop.

    tracing_subscriber::registry()
        .with(base_layer)
        .with(perf_layer)
        .init();

    None // Return flame guard when enabled
}

/// Instrumentation helpers for common patterns
pub mod instrument {
    use std::time::Instant;

    /// Time a sync function and record to profiler
    pub fn timed<F, R>(name: &str, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = Instant::now();
        let result = f();
        let duration_us = start.elapsed().as_micros() as u64;
        crate::performance::global_profiler()
            .cpu
            .record_span(name, duration_us);
        result
    }

    /// Time an async function
    pub async fn timed_async<F, Fut, R>(name: &str, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let start = Instant::now();
        let result = f().await;
        let duration_us = start.elapsed().as_micros() as u64;
        crate::performance::global_profiler()
            .cpu
            .record_span(name, duration_us);
        result
    }

    /// RAII guard for timing a scope
    pub struct TimedScope {
        name: String,
        start: Instant,
    }

    impl TimedScope {
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                start: Instant::now(),
            }
        }
    }

    impl Drop for TimedScope {
        fn drop(&mut self) {
            let duration_us = self.start.elapsed().as_micros() as u64;
            crate::performance::global_profiler()
                .cpu
                .record_span(&self.name, duration_us);
        }
    }
}

/// Convenience macro for timed scope
#[macro_export]
macro_rules! timed_scope {
    ($name:expr) => {
        let _guard = $crate::performance::tracing_layer::instrument::TimedScope::new($name);
    };
}
