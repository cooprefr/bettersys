# Binance Latency Measurement Harness

## Overview

Production-grade instrumentation for quantifying network and processing latency to Binance market-data endpoints from AWS eu-west-1.

## Design Principles

### Monotonic vs Wall-Clock Time

| Time Source | Rust Type | Use Case | Properties |
|-------------|-----------|----------|------------|
| **Monotonic** | `std::time::Instant` | All latency deltas | Immune to NTP drift, leap seconds, clock adjustments |
| **Wall-clock** | `chrono::Utc` | Correlation with Binance `E` field, CSV timestamps | Subject to clock sync quality |

**Rule**: Never subtract wall-clock timestamps for latency. Always use monotonic.

## Instrumentation Points

### Connection Establishment (one-time per connect)

```
T0: DNS lookup start ─────────────────────────────────────┐
    │                                                      │
    ├── L_dns: DNS resolution (μs-ms)                     │
    │   Measured: lookup() call → first record received   │
    │                                                      │
T1: TCP SYN sent ─────────────────────────────────────────┤
    │                                                      │
    ├── L_tcp: TCP 3-way handshake (1-50ms typical)       │ Total
    │   Measured: connect() → socket writable             │ Connect
    │                                                      │ Time
T2: TCP connected ────────────────────────────────────────┤
    │                                                      │
    ├── L_tls: TLS handshake (10-100ms typical)           │
    │   Measured: ClientHello → Finished                   │
    │                                                      │
T3: TLS established ──────────────────────────────────────┤
    │                                                      │
    ├── L_ws: WebSocket upgrade (5-20ms typical)          │
    │   Measured: HTTP Upgrade → 101 Switching Protocols   │
    │                                                      │
T4: WebSocket ready ──────────────────────────────────────┘
```

### Per-Message Latency (steady state)

```
T_exchange ──────────────────────────────────────────────────────────┐
│   Binance generates message (from 'E' field in JSON)              │
│                                                                    │
├── L_wire: Network transit (one-way)                               │
│   = T_kernel_rx - T_exchange                                      │
│   WARNING: Requires clock sync. Accuracy = NTP quality (±1-5ms)   │
│                                                                    │
T_kernel_rx ─────────────────────────────────────────────────────────┤
│   Kernel receives frame (estimated, no SO_TIMESTAMPING in tokio)  │
│                                                                    │
├── L_userspace: Kernel → userspace delivery                        │
│   Typically < 100μs on modern Linux                               │
│                                                                    │ Total
T_recv ──────────────────────────────────────────────────────────────┤ Internal
│   tokio-tungstenite returns frame (FIRST MONOTONIC TIMESTAMP)     │ Latency
│                                                                    │
├── L_decode: JSON parse + validation (10-100μs typical)            │
│   Measured: recv return → struct populated                        │
│                                                                    │
T_decoded ───────────────────────────────────────────────────────────┤
│   Structured data available                                       │
│                                                                    │
├── L_handoff: Channel send (1-10μs typical)                        │
│   Measured: before channel.send() → after send returns            │
│                                                                    │
T_handoff ───────────────────────────────────────────────────────────┤
│   Message in strategy channel                                     │
│                                                                    │
├── L_strategy: Queue wait + callback entry (variable)              │
│   Measured: channel recv → callback entry                         │
│                                                                    │
T_strategy ──────────────────────────────────────────────────────────┘
    Strategy processes event
```

## Exact Timestamp Definitions

| Timestamp | Source | Meaning |
|-----------|--------|---------|
| `T_exchange` | `market_event.time_exchange` | When Binance generated the message (from `E` JSON field) |
| `T_recv` | `MonotonicInstant::now()` at `recv().await` return | First userspace timestamp after frame arrival |
| `T_decoded` | `MonotonicInstant::now()` after JSON parse | Structured data ready |
| `T_handoff` | `MonotonicInstant::now()` after `channel.send()` | Message queued for strategy |
| `T_strategy` | `MonotonicInstant::now()` at callback entry | Strategy starts processing |

## CSV Schema

### Message Latency Samples (`binance_message_latency.csv`)

```csv
sample_id,symbol,wall_clock_iso,exchange_ts_ms,mono_recv_ns,mono_decoded_ns,mono_handoff_ns,mono_strategy_ns,wire_latency_ns,decode_latency_ns,handoff_latency_ns,strategy_latency_ns,total_internal_latency_ns,message_size_bytes,sequence,best_bid,best_ask
1,BTCUSDT,2026-01-31T12:00:00.123456Z,1738324800123,1000000,1010000,1015000,1020000,5000000,10000,5000,5000,20000,256,,50000.00000000,50001.00000000
```

| Column | Type | Description |
|--------|------|-------------|
| `sample_id` | u64 | Unique monotonically increasing ID |
| `symbol` | string | Trading pair (e.g., "BTCUSDT") |
| `wall_clock_iso` | string | ISO 8601 timestamp at recv() |
| `exchange_ts_ms` | i64 | Binance exchange timestamp (ms) |
| `mono_recv_ns` | u64 | Monotonic ns at recv() |
| `mono_decoded_ns` | u64 | Monotonic ns after decode |
| `mono_handoff_ns` | u64 | Monotonic ns after channel send |
| `mono_strategy_ns` | u64? | Monotonic ns at strategy entry |
| `wire_latency_ns` | i64 | Estimated one-way (wall-clock based) |
| `decode_latency_ns` | u64 | JSON parse time (monotonic) |
| `handoff_latency_ns` | u64 | Channel send time (monotonic) |
| `strategy_latency_ns` | u64? | Queue wait time (monotonic) |
| `total_internal_latency_ns` | u64 | recv → strategy (monotonic) |
| `message_size_bytes` | usize | Raw message size |
| `sequence` | u64? | Message sequence if available |
| `best_bid` | f64? | Best bid price |
| `best_ask` | f64? | Best ask price |

### Connection Latency Samples (`binance_connection_latency.csv`)

```csv
wall_clock_iso,dns_latency_ns,tcp_connect_latency_ns,tls_handshake_latency_ns,ws_upgrade_latency_ns,subscribe_latency_ns,total_connect_latency_ns,success,remote_addr,tls_version,tls_cipher,error
2026-01-31T12:00:00.000000Z,1500000,25000000,45000000,12000000,8000000,91500000,true,54.239.28.85:9443,TLSv1.3,TLS_AES_256_GCM_SHA384,""
```

## Integration Guide

### 1. Instrument `barter-data` Event Handler

```rust
use crate::performance::latency::binance_harness::{
    global_harness, MessageLatencyBuilder,
};

async fn consume(streams: Streams<...>) -> Result<()> {
    while let Some(event) = joined.next().await {
        match event {
            ReconnectEvent::Item(Ok(market_event)) => {
                // T_recv: FIRST monotonic timestamp
                let mut builder = MessageLatencyBuilder::start(
                    to_symbol(&market_event.instrument),
                    market_event.time_exchange.timestamp_millis(),
                    0, // size unknown at this point
                );

                // Parse mid-price
                let mid = market_event.kind.mid_price()...;

                // T_decoded: after parsing
                builder.mark_decoded();

                // Update internal state
                self.update_symbol(&symbol, ts, mid);

                // Prepare broadcast event
                let update_event = PriceUpdateEvent { ... };

                // T_handoff: before channel send
                builder.mark_handoff();

                // Send to strategy
                let _ = self.update_tx.send(update_event);

                // Record sample
                let sample = builder
                    .book(bid.unwrap_or(0.0), ask.unwrap_or(0.0))
                    .build();
                global_harness().record_message(sample);
            }
            // ...
        }
    }
}
```

### 2. Instrument Strategy Callback

```rust
async fn on_price_update(&mut self, event: PriceUpdateEvent) {
    // T_strategy: callback entry
    let strategy_entry_ns = MonotonicInstant::now().as_nanos();

    // ... strategy logic ...

    // Optional: record strategy entry timestamp separately
    // (requires passing builder through channel or using sample_id correlation)
}
```

### 3. Export CSV

```rust
use std::fs::File;

let harness = global_harness();
let mut file = File::create("binance_message_latency.csv")?;
harness.export_message_csv(&mut file)?;

let mut file = File::create("binance_connection_latency.csv")?;
harness.export_connection_csv(&mut file)?;
```

### 4. Query Statistics

```rust
let summary = global_harness().summary();

println!("Wire latency: {}", summary.wire_latency.to_us_string());
println!("Decode latency: {}", summary.decode_latency.to_us_string());
println!("Total internal: {}", summary.total_internal_latency.to_us_string());
println!("Wire jitter (mean): {:.1}μs", summary.wire_jitter.mean_jitter_ns / 1000.0);
```

## Dashboard Spec (Grafana)

```json
{
  "title": "Binance Market Data Latency (eu-west-1)",
  "panels": [
    {
      "title": "Connection Establishment",
      "type": "timeseries",
      "metrics": [
        "binance_dns_latency_p99_us",
        "binance_tcp_connect_p99_us",
        "binance_tls_handshake_p99_us",
        "binance_ws_upgrade_p99_us"
      ]
    },
    {
      "title": "Per-Message Latency (Internal)",
      "type": "timeseries",
      "metrics": [
        "binance_decode_latency_p50_us",
        "binance_decode_latency_p99_us",
        "binance_handoff_latency_p50_us",
        "binance_handoff_latency_p99_us",
        "binance_strategy_latency_p50_us",
        "binance_strategy_latency_p99_us"
      ]
    },
    {
      "title": "Wire Latency (One-Way Estimate)",
      "type": "timeseries",
      "metrics": [
        "binance_wire_latency_p50_ms",
        "binance_wire_latency_p95_ms",
        "binance_wire_latency_p99_ms"
      ]
    },
    {
      "title": "RTT (Ping/Pong)",
      "type": "timeseries",
      "metrics": [
        "binance_rtt_p50_ms",
        "binance_rtt_p99_ms"
      ]
    },
    {
      "title": "Jitter",
      "type": "timeseries",
      "metrics": [
        "binance_wire_jitter_mean_us",
        "binance_wire_jitter_max_us"
      ]
    },
    {
      "title": "Per-Symbol P99",
      "type": "bargauge",
      "metrics": [
        "binance_symbol_latency_p99_us{symbol=\"BTCUSDT\"}",
        "binance_symbol_latency_p99_us{symbol=\"ETHUSDT\"}",
        "binance_symbol_latency_p99_us{symbol=\"SOLUSDT\"}",
        "binance_symbol_latency_p99_us{symbol=\"XRPUSDT\"}"
      ]
    }
  ]
}
```

## Expected Latency Ranges (AWS eu-west-1 → Binance)

| Component | P50 | P95 | P99 | P99.9 |
|-----------|-----|-----|-----|-------|
| DNS | <1ms | 2ms | 5ms | 10ms |
| TCP Connect | 15-25ms | 30ms | 50ms | 100ms |
| TLS Handshake | 30-50ms | 60ms | 80ms | 150ms |
| WS Upgrade | 10-20ms | 25ms | 35ms | 50ms |
| **Total Connect** | **60-100ms** | **120ms** | **170ms** | **300ms** |
| Wire (one-way)* | 8-15ms | 20ms | 30ms | 50ms |
| Decode | 10-30μs | 50μs | 80μs | 150μs |
| Handoff | 1-5μs | 10μs | 20μs | 50μs |
| Strategy Queue | 5-20μs | 50μs | 100μs | 500μs |
| **Total Internal** | **20-60μs** | **120μs** | **200μs** | **700μs** |

*Wire latency estimates require NTP sync. Accuracy depends on NTP quality (±1-5ms without PTP).

## One-Way vs Round-Trip Measurement

### Round-Trip (Accurate)

WebSocket ping/pong provides accurate RTT without clock synchronization:

```rust
// Send ping, record monotonic timestamp
let ping_sent = MonotonicInstant::now();
ws.send(Message::Ping(vec![1,2,3,4])).await?;

// Wait for pong
let pong = ws.recv().await?;
let pong_recv = MonotonicInstant::now();

let rtt_ns = pong_recv.duration_since(ping_sent).as_nanos();
let estimated_one_way_ns = rtt_ns / 2; // Lower bound
```

### One-Way (Estimated)

Requires comparing Binance's `E` timestamp to local wall-clock:

```rust
let exchange_ts_ms = market_event.time_exchange.timestamp_millis();
let recv_wall_ms = chrono::Utc::now().timestamp_millis();
let one_way_ms = recv_wall_ms - exchange_ts_ms;
```

**Caveats**:
- Requires NTP synchronization on both ends
- Binance servers likely use GPS/PTP (sub-ms accuracy)
- AWS instances use NTP (typical accuracy: ±1-5ms)
- Negative values indicate clock skew (your clock is ahead)

## Jitter Calculation

Uses RFC 3550 exponential moving average:

```
J(i) = J(i-1) + (|D(i-1,i)| - J(i-1)) / 16
```

Where `D(i-1,i)` is the difference between consecutive latency samples.

## Best Practices

1. **Always use monotonic time for latency deltas**
2. **Capture timestamps as early as possible** (immediately at recv() return)
3. **Minimize work between timestamp captures** (parse after marking T_recv)
4. **Sample rate trade-off**: Full sampling for accuracy, 1:10 or 1:100 for production overhead
5. **Export CSV periodically** for offline analysis
6. **Monitor jitter** as indicator of network/system instability
7. **Compare RTT/2 to one-way estimates** to validate clock sync
