# Edge Receiver Architecture: Two-Tier Binance Market Data

**Status:** Proposal  
**Author:** Architecture Team  
**Date:** 2026-01-31

---

## 1. Overview

A geographically split architecture where a minimal **edge receiver** in ap-southeast-1 (Singapore, ~2-5ms to Binance) normalizes and forwards market data to the main trading engine in eu-west-1 (Ireland).

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TWO-TIER ARCHITECTURE                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌─────────────────┐         UDP/QUIC          ┌─────────────────────┐    │
│   │  EDGE RECEIVER  │ ========================> │   TRADING ENGINE    │    │
│   │  ap-southeast-1 │     ~120ms one-way        │     eu-west-1       │    │
│   │  (Singapore)    │                           │     (Ireland)       │    │
│   └────────┬────────┘                           └──────────┬──────────┘    │
│            │                                               │               │
│            │ ~2-5ms                                         │               │
│            ▼                                               ▼               │
│   ┌─────────────────┐                           ┌─────────────────────┐    │
│   │     Binance     │                           │   Strategy + Risk   │    │
│   │   WebSocket     │                           │   Kelly Sizing      │    │
│   │  (Singapore)    │                           │   Order Execution   │    │
│   └─────────────────┘                           └─────────────────────┘    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Why This Beats Single-Region

| Metric | Single Region (eu-west-1 direct) | Two-Tier (edge + forward) |
|--------|----------------------------------|---------------------------|
| Binance → First byte | 30-50ms p99 | 2-5ms p99 (at edge) |
| Binance → Strategy | 30-50ms p99 | 125-130ms p99 |
| **Jitter** | 15-30ms (long-haul variance) | 3-8ms (edge) + 2-5ms (tunnel) |
| **Gap detection** | Delayed (30ms+) | Immediate (at edge) |
| **Recovery** | Slow (must re-request from EU) | Fast (edge has L1 cache) |

**Key insight:** The edge receiver provides:
1. **Immediate gap detection** - knows within 5ms if Binance skipped a sequence
2. **Local L1 cache** - can serve stale-but-recent data if upstream hiccups
3. **Pre-normalized data** - EU engine receives fixed-size binary, no JSON parsing
4. **Reduced jitter** - Singapore→Ireland path is more predictable than Singapore→random-Binance-PoP→Ireland

---

## 2. Edge Receiver Design

### 2.1 Responsibilities (Minimal)

The edge receiver is intentionally thin:

1. **Connect** to Binance WebSocket (bookTicker streams)
2. **Parse** JSON → extract (symbol, bid, ask, bid_qty, ask_qty, exchange_ts)
3. **Detect gaps** via Binance update ID sequences
4. **Serialize** to compact binary wire format
5. **Forward** via UDP (primary) or QUIC (fallback)
6. **Heartbeat** every 100ms to prove liveness

**NOT responsible for:**
- Strategy decisions
- Position tracking
- Risk calculations
- Order execution
- Any state beyond L1 orderbook snapshots

### 2.2 Implementation (Rust)

```rust
// edge_receiver/src/main.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::net::UdpSocket;
use tokio_tungstenite::connect_async;

/// Compact wire format for a single symbol update
#[repr(C, packed)]
pub struct EdgeTick {
    pub magic: u16,           // 0xED6E ("edge")
    pub version: u8,          // Protocol version (1)
    pub flags: u8,            // Bit flags (gap_detected, heartbeat, etc.)
    pub symbol_id: u8,        // 0=BTC, 1=ETH, 2=SOL, 3=XRP
    pub _pad: [u8; 3],        // Alignment padding
    pub seq: u64,             // Edge-assigned monotonic sequence
    pub exchange_ts_ns: i64,  // Binance timestamp (ns since epoch)
    pub edge_ts_ns: i64,      // Edge receive timestamp (ns since epoch)
    pub bid: i64,             // Price × 1e8 (fixed-point)
    pub ask: i64,             // Price × 1e8 (fixed-point)
    pub bid_qty: i64,         // Quantity × 1e8
    pub ask_qty: i64,         // Quantity × 1e8
    pub binance_update_id: u64, // For gap detection
    pub checksum: u32,        // CRC32 of payload
}
// Total: 76 bytes fixed

const EDGE_TICK_SIZE: usize = std::mem::size_of::<EdgeTick>(); // 76 bytes

/// Flags byte layout
pub mod flags {
    pub const GAP_DETECTED: u8 = 0x01;   // Binance sequence gap
    pub const HEARTBEAT: u8 = 0x02;      // No data, just liveness
    pub const STALE: u8 = 0x04;          // Data older than 100ms
    pub const RECONNECTING: u8 = 0x08;   // WS reconnect in progress
}
```

### 2.3 Resource Requirements

| Resource | Specification | Rationale |
|----------|---------------|-----------|
| Instance | c6in.medium (1 vCPU, 2GB) | Network-optimized, minimal |
| Network | ENA Express enabled | Lower jitter |
| CPU pinning | Core 0 for ingest | Deterministic latency |
| Memory | 512MB working set | L1 cache + buffers |

---

## 3. Wire Protocol Specification

### 3.1 Transport: UDP with QUIC Fallback

**Primary: UDP**
- Port 19876 (configurable)
- MTU-safe (76 bytes << 1500)
- No connection overhead
- Fire-and-forget semantics

**Fallback: QUIC**
- Activated if UDP loss > 1% over 60s window
- Provides reliable delivery with low overhead
- Same binary payload format

### 3.2 Packet Format

```
┌────────────────────────────────────────────────────────────────────┐
│                         EdgeTick (76 bytes)                         │
├──────┬─────────┬───────┬────────┬───────────┬─────────────────────┤
│ magic│ version │ flags │sym_id  │  padding  │       seq (8)       │
│ (2)  │   (1)   │  (1)  │  (1)   │    (3)    │                     │
├──────┴─────────┴───────┴────────┴───────────┼─────────────────────┤
│              exchange_ts_ns (8)             │   edge_ts_ns (8)    │
├─────────────────────────────────────────────┼─────────────────────┤
│                  bid (8)                    │       ask (8)       │
├─────────────────────────────────────────────┼─────────────────────┤
│                bid_qty (8)                  │    ask_qty (8)      │
├─────────────────────────────────────────────┼─────────────────────┤
│           binance_update_id (8)             │    checksum (4)     │
└─────────────────────────────────────────────┴─────────────────────┘
```

### 3.3 Sequence Numbers

The edge assigns a **monotonic 64-bit sequence** to every packet:

```rust
static EDGE_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    EDGE_SEQ.fetch_add(1, Ordering::Relaxed)
}
```

**Guarantees:**
- Strictly increasing (no gaps unless edge restarts)
- Per-edge-instance (not global)
- Wraps at u64::MAX (heat death of universe)

**Engine-side detection:**
```rust
struct SequenceTracker {
    last_seq: u64,
    gaps: Vec<(u64, u64)>,  // (expected, received)
    dup_count: u64,
}

impl SequenceTracker {
    fn check(&mut self, seq: u64) -> SequenceStatus {
        if seq == self.last_seq + 1 {
            self.last_seq = seq;
            SequenceStatus::Ok
        } else if seq <= self.last_seq {
            self.dup_count += 1;
            SequenceStatus::Duplicate
        } else {
            // Gap: expected last_seq+1, got seq
            self.gaps.push((self.last_seq + 1, seq));
            self.last_seq = seq;
            SequenceStatus::Gap { missing: seq - self.last_seq - 1 }
        }
    }
}
```

### 3.4 Heartbeats

Edge sends heartbeat every **100ms** if no data:

```rust
struct HeartbeatTick {
    magic: u16,           // 0xED6E
    version: u8,          // 1
    flags: u8,            // flags::HEARTBEAT
    symbol_id: u8,        // 0xFF (all symbols)
    _pad: [u8; 3],
    seq: u64,             // Increments normally
    edge_ts_ns: i64,      // Current edge time
    // ... rest zeroed
}
```

**Engine heartbeat detection:**
```rust
const HEARTBEAT_TIMEOUT_MS: u64 = 500;  // 5 missed heartbeats

fn check_liveness(last_recv: Instant) -> bool {
    last_recv.elapsed() < Duration::from_millis(HEARTBEAT_TIMEOUT_MS)
}
```

---

## 4. Loss and Reorder Handling

### 4.1 At the Engine (eu-west-1)

```rust
pub struct EdgeReceiver {
    socket: UdpSocket,
    seq_tracker: SequenceTracker,
    reorder_buffer: BoundedBuffer<EdgeTick>,  // 16 slots
    last_per_symbol: [Option<EdgeTick>; 4],   // BTC/ETH/SOL/XRP
    metrics: ReceiverMetrics,
}

impl EdgeReceiver {
    /// Process incoming packet with reorder tolerance
    pub fn recv(&mut self) -> Option<EdgeTick> {
        let mut buf = [0u8; EDGE_TICK_SIZE];
        let n = self.socket.recv(&mut buf).ok()?;
        
        if n != EDGE_TICK_SIZE {
            self.metrics.malformed += 1;
            return None;
        }
        
        let tick: EdgeTick = unsafe { std::ptr::read(buf.as_ptr() as *const _) };
        
        // Verify checksum
        if !verify_checksum(&tick) {
            self.metrics.checksum_errors += 1;
            return None;
        }
        
        // Sequence check with reorder tolerance
        match self.seq_tracker.check(tick.seq) {
            SequenceStatus::Ok => {
                self.deliver(tick);
            }
            SequenceStatus::Duplicate => {
                self.metrics.duplicates += 1;
            }
            SequenceStatus::Gap { missing } => {
                // Buffer this tick, wait briefly for missing
                self.reorder_buffer.insert(tick);
                self.metrics.gaps += 1;
                self.metrics.missing_count += missing;
                
                // After 5ms, give up and deliver buffered
                // (handled by timeout in main loop)
            }
        }
        
        None
    }
    
    /// Deliver tick to strategy (updates last-value cache)
    fn deliver(&mut self, tick: EdgeTick) {
        let idx = tick.symbol_id as usize;
        if idx < 4 {
            self.last_per_symbol[idx] = Some(tick);
        }
        // Broadcast to strategy...
    }
}
```

### 4.2 Reorder Buffer

```rust
/// Bounded reorder buffer with timeout
struct BoundedBuffer<T> {
    items: [(u64, T, Instant); 16],  // (seq, item, recv_time)
    head: usize,
    count: usize,
}

impl<T: Copy> BoundedBuffer<T> {
    const REORDER_TIMEOUT_MS: u64 = 5;
    
    fn insert(&mut self, seq: u64, item: T) {
        if self.count < 16 {
            self.items[self.count] = (seq, item, Instant::now());
            self.count += 1;
        }
        // Sort by seq for ordered delivery
        self.items[..self.count].sort_by_key(|(s, _, _)| *s);
    }
    
    fn drain_ready(&mut self, expected_seq: u64) -> Vec<T> {
        let now = Instant::now();
        let mut ready = Vec::new();
        
        // Deliver in-order items
        while self.count > 0 && self.items[0].0 == expected_seq {
            ready.push(self.items[0].1);
            self.shift_left();
            expected_seq += 1;
        }
        
        // Deliver timed-out items (accept gaps)
        while self.count > 0 {
            let (_, _, recv_time) = self.items[0];
            if now.duration_since(recv_time).as_millis() > Self::REORDER_TIMEOUT_MS as u128 {
                ready.push(self.items[0].1);
                self.shift_left();
            } else {
                break;
            }
        }
        
        ready
    }
}
```

### 4.3 Gap Recovery Strategy

| Gap Size | Action | Rationale |
|----------|--------|-----------|
| 1-3 packets | Wait 5ms in reorder buffer | Network reorder is common |
| 4-10 packets | Log warning, continue | L1 data is fungible |
| 10+ packets | Alert, check edge health | Likely edge issue |
| >100 in 1min | Trigger QUIC fallback | Sustained loss |

**No retransmission:** L1 orderbook data is ephemeral. By the time we'd retransmit, the data is stale. Instead, we:
1. Accept the gap
2. Use the next valid tick
3. Trust that the SeqLock last-value semantics handle staleness

---

## 5. Latency Budget Analysis

### 5.1 Breakdown

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        LATENCY BUDGET (p99)                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Binance exchange event generated                                           │
│           │                                                                 │
│           ├── 1-3ms   Binance internal (matching → WS push)                │
│           │                                                                 │
│  Binance WS server sends frame                                              │
│           │                                                                 │
│           ├── 2-5ms   Binance → Edge (Singapore local)                      │
│           │                                                                 │
│  Edge receives frame (T_edge_recv)                                          │
│           │                                                                 │
│           ├── 10-30μs  JSON parse (manual, no serde)                        │
│           │                                                                 │
│           ├── 1-5μs   Binary serialize                                      │
│           │                                                                 │
│           ├── 1-5μs   UDP sendto()                                          │
│           │                                                                 │
│  Edge sends UDP packet                                                      │
│           │                                                                 │
│           ├── 115-125ms  Singapore → Ireland (submarine cable)              │
│           │                                                                 │
│  Engine receives packet (T_engine_recv)                                     │
│           │                                                                 │
│           ├── 1-5μs   Deserialize + checksum                                │
│           │                                                                 │
│           ├── 1-5μs   SeqLock write                                         │
│           │                                                                 │
│  Strategy sees update (T_strategy)                                          │
│                                                                             │
│  TOTAL: ~125-135ms p99 (edge-to-strategy)                                   │
│  TOTAL: ~5-10ms p99 (binance-to-edge)                                       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 5.2 Why This Beats Direct Connection

| Factor | Direct (eu-west-1 → Binance) | Two-Tier |
|--------|------------------------------|----------|
| **Wire latency** | 30-50ms (variable routing) | 120ms (fixed submarine) |
| **Jitter** | 15-30ms (internet weather) | 2-5ms (private backbone) |
| **Parse location** | In EU (blocking strategy) | At edge (async) |
| **Gap detection** | 30-50ms delayed | 5ms (immediate at edge) |
| **Recovery** | Re-request from EU | Edge L1 cache |

**The key win is jitter reduction.** While total latency is higher, the variance is dramatically lower. For HFT strategies, predictable 125ms beats unpredictable 30-50ms.

Additionally, the edge can detect gaps immediately and flag them in the packet, allowing the EU engine to make informed decisions without waiting.

---

## 6. Deployment Plan

### 6.1 Infrastructure (Terraform)

```hcl
# infra/edge-receiver/main.tf

module "edge_receiver" {
  source = "./modules/edge"
  
  providers = {
    aws = aws.ap-southeast-1
  }
  
  instance_type = "c6in.medium"
  ami           = data.aws_ami.al2023_arm64.id  # Graviton for cost
  
  # Network tuning
  ena_express = true
  
  # Security
  allowed_cidrs = [
    "0.0.0.0/0"  # Binance WS (egress only)
  ]
  allowed_destinations = [
    "${var.engine_ip}/32"  # EU engine only
  ]
  
  # Tags
  environment = var.environment
  component   = "edge-receiver"
}

module "trading_engine" {
  source = "./modules/engine"
  
  providers = {
    aws = aws.eu-west-1
  }
  
  instance_type = "c6in.xlarge"  # More compute for strategy
  
  # Accept UDP from edge
  edge_receiver_ip = module.edge_receiver.private_ip
  edge_port        = 19876
}
```

### 6.2 Systemd Services

```ini
# /etc/systemd/system/edge-receiver.service
[Unit]
Description=Binance Edge Receiver
After=network-online.target

[Service]
Type=simple
User=betterbot
ExecStart=/opt/betterbot/edge-receiver \
  --binance-symbols btcusdt,ethusdt,solusdt,xrpusdt \
  --forward-host ${ENGINE_HOST} \
  --forward-port 19876 \
  --heartbeat-ms 100

# CPU pinning
CPUAffinity=0
Nice=-20

# Restart policy
Restart=always
RestartSec=1

[Install]
WantedBy=multi-user.target
```

### 6.3 Deployment Sequence

1. **Provision edge in ap-southeast-1**
   ```bash
   cd infra/edge-receiver
   terraform apply -target=module.edge_receiver
   ```

2. **Deploy edge binary**
   ```bash
   cargo build --release --bin edge-receiver
   scp target/release/edge-receiver edge:/opt/betterbot/
   ssh edge 'systemctl restart edge-receiver'
   ```

3. **Update engine to receive from edge**
   ```bash
   # In eu-west-1
   export EDGE_RECEIVER_HOST=<edge-private-ip>
   export EDGE_RECEIVER_PORT=19876
   systemctl restart trading-engine
   ```

4. **Verify connectivity**
   ```bash
   # On engine
   nc -ul 19876 | xxd | head -20  # Should see 72-byte packets
   ```

---

## 7. Failover Plan

### 7.1 Edge Failure Scenarios

| Scenario | Detection | Action | RTO |
|----------|-----------|--------|-----|
| Edge process crash | Heartbeat timeout (500ms) | Engine falls back to direct WS | <1s |
| Edge instance failure | Heartbeat + AWS health check | Auto-replace via ASG | 60-90s |
| Singapore region failure | Route 53 health check | Failover to Tokyo edge | 30-60s |
| Network partition (Edge↔Engine) | Heartbeat timeout | Engine reconnects direct | <1s |

### 7.2 Fallback Architecture

```
                    PRIMARY                         FALLBACK
                    ────────                        ────────
                    
┌───────────────┐                           ┌───────────────┐
│ Edge Receiver │──────UDP────────┐         │   Binance WS  │
│ ap-southeast-1│                 │         │   (direct)    │
└───────────────┘                 ▼         └───────┬───────┘
                          ┌─────────────┐           │
                          │   Engine    │◄──────────┘
                          │  eu-west-1  │   (fallback on
                          └─────────────┘    heartbeat timeout)
```

**Engine fallback logic:**

```rust
enum DataSource {
    Edge { last_heartbeat: Instant },
    Direct { ws: WebSocketStream },
}

impl TradingEngine {
    async fn recv_tick(&mut self) -> Option<PriceTick> {
        match &mut self.source {
            DataSource::Edge { last_heartbeat } => {
                tokio::select! {
                    tick = self.edge_socket.recv() => {
                        *last_heartbeat = Instant::now();
                        Some(tick)
                    }
                    _ = tokio::time::sleep(HEARTBEAT_TIMEOUT) => {
                        warn!("Edge heartbeat timeout, falling back to direct WS");
                        self.source = DataSource::Direct {
                            ws: connect_binance_direct().await
                        };
                        None
                    }
                }
            }
            DataSource::Direct { ws } => {
                // Direct Binance WebSocket (existing code path)
                self.recv_from_ws(ws).await
            }
        }
    }
}
```

### 7.3 Multi-Region Edge (Optional)

For higher availability, deploy edges in multiple regions:

```
┌─────────────────┐     ┌─────────────────┐
│ Edge (Primary)  │     │ Edge (Secondary)│
│ ap-southeast-1  │     │ ap-northeast-1  │
│   Singapore     │     │     Tokyo       │
└────────┬────────┘     └────────┬────────┘
         │                       │
         └───────────┬───────────┘
                     │
                     ▼
              ┌─────────────┐
              │   Engine    │
              │  eu-west-1  │
              └─────────────┘
```

Engine selects based on:
1. Lower sequence number wins (first to arrive)
2. Deduplicate via `binance_update_id`
3. Failover to secondary if primary heartbeat dies

---

## 8. Monitoring & Alerting

### 8.1 Metrics to Export

**Edge metrics (Prometheus):**
```
# HELP edge_binance_latency_ns One-way latency from Binance to edge
edge_binance_latency_ns{symbol="BTCUSDT", quantile="0.99"} 3500000

# HELP edge_forward_latency_ns Time to serialize and send UDP
edge_forward_latency_ns{quantile="0.99"} 15000

# HELP edge_gaps_total Binance sequence gaps detected
edge_gaps_total{symbol="BTCUSDT"} 12

# HELP edge_reconnects_total WebSocket reconnection count
edge_reconnects_total 3
```

**Engine metrics:**
```
# HELP engine_edge_latency_ns Edge-to-engine one-way latency
engine_edge_latency_ns{quantile="0.99"} 122000000

# HELP engine_packet_loss_ratio UDP packet loss ratio (0-1)
engine_packet_loss_ratio 0.0001

# HELP engine_reorder_events_total Packets received out of order
engine_reorder_events_total 45

# HELP engine_heartbeat_age_ms Time since last edge heartbeat
engine_heartbeat_age_ms 87
```

### 8.2 Alerts

| Alert | Condition | Severity | Action |
|-------|-----------|----------|--------|
| `EdgeHeartbeatStale` | `engine_heartbeat_age_ms > 500` | Critical | Page on-call |
| `EdgeHighLatency` | `edge_binance_latency_ns{q=0.99} > 10ms` | Warning | Investigate |
| `EngineHighLoss` | `engine_packet_loss_ratio > 0.01` | Warning | Check network |
| `EdgeGapSpike` | `rate(edge_gaps_total[5m]) > 10` | Warning | Check Binance |

---

## 9. Cost Estimate

| Component | Specification | Monthly Cost |
|-----------|---------------|--------------|
| Edge (ap-southeast-1) | c6in.medium, reserved 1yr | ~$25 |
| Engine (eu-west-1) | c6in.xlarge, reserved 1yr | ~$95 |
| Data transfer (Edge→Engine) | ~100GB/month | ~$9 |
| Route 53 health checks | 2 endpoints | ~$1 |
| **Total** | | **~$130/month** |

Compared to running everything in ap-southeast-1 (closer to Binance but far from Polymarket), this architecture provides:
- Lower latency to Polymarket execution (EU-based)
- Predictable Binance data with edge caching
- Isolation of market data concerns

---

## 10. Implementation Checklist

- [ ] Create `edge-receiver` crate in `rust-backend/src/bin/`
- [ ] Implement `EdgeTick` wire format with CRC32
- [ ] Add UDP sender with configurable destination
- [ ] Add heartbeat timer (100ms)
- [ ] Add Binance gap detection (track `lastUpdateId`)
- [ ] Create `EdgeReceiverClient` in trading engine
- [ ] Add sequence tracker with reorder buffer
- [ ] Add fallback to direct WebSocket on timeout
- [ ] Terraform modules for edge and engine
- [ ] Prometheus metrics exporter
- [ ] Grafana dashboard
- [ ] Runbook for failover scenarios
- [ ] Load test: verify 10k msg/sec throughput
- [ ] Chaos test: kill edge, verify fallback <1s
