# Phase 3: Dome WebSocket Real-time Engine - Implementation Plan

**Objective**: Replace 30-second polling with WebSocket streaming for sub-second latency  
**Priority**: HIGH (10x performance multiplier)  
**Estimated Duration**: 3-4 hours

---

## Current State Analysis

### Existing Implementation (dome_tracker.rs)
- ✅ REST API polling every 30 seconds
- ✅ Rate limiting (1 req/sec)
- ✅ Order fetching for tracked wallets
- ❌ High latency (30s+ delay)
- ❌ API rate limit concerns
- ❌ Miss fast-moving opportunities

### Dependencies Available
From `Cargo.toml`:
- ✅ `tokio-tungstenite = "0.20"` - WebSocket client
- ✅ `axum` with `ws` feature - WebSocket server (for API)
- ✅ `tokio` with full features
- ✅ `serde` + `serde_json`

---

## Phase 3 Implementation Strategy

### 3.1 WebSocket Client Architecture

**New File**: `rust-backend/src/scrapers/dome_websocket.rs`

```rust
//! Dome WebSocket Real-time Order Feed
//! Mission: Sub-second latency for elite wallet tracking
//! Philosophy: Never miss a trade. Streaming > Polling.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

const DOME_WS_URL: &str = "wss://api.domeapi.io/v1/polymarket/stream";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsSubscribeMessage {
    pub action: String,  // "subscribe"
    pub channel: String, // "orders"
    pub filters: WsFilters,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsFilters {
    pub users: Vec<String>,  // Wallet addresses to track
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsOrderUpdate {
    pub order_id: String,
    pub token_id: String,
    pub side: String,
    pub shares_normalized: f64,
    pub price: f64,
    pub timestamp: i64,
    pub market_slug: String,
    pub title: String,
    pub user: String,
}

pub struct DomeWebSocketClient {
    api_key: String,
    tracked_wallets: Vec<String>,
    order_tx: mpsc::UnboundedSender<WsOrderUpdate>,
}

impl DomeWebSocketClient {
    pub fn new(
        api_key: String,
        tracked_wallets: Vec<String>,
    ) -> (Self, mpsc::UnboundedReceiver<WsOrderUpdate>) {
        let (order_tx, order_rx) = mpsc::unbounded_channel();
        
        let client = Self {
            api_key,
            tracked_wallets,
            order_tx,
        };
        
        (client, order_rx)
    }
    
    /// Start WebSocket connection with auto-reconnect
    pub async fn run(&self) -> Result<()> {
        let mut reconnect_delay = Duration::from_secs(1);
        
        loop {
            match self.connect_and_stream().await {
                Ok(_) => {
                    info!("WebSocket connection closed gracefully");
                    reconnect_delay = Duration::from_secs(1);
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    warn!("Reconnecting in {:?}...", reconnect_delay);
                    sleep(reconnect_delay).await;
                    
                    // Exponential backoff up to 60 seconds
                    reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
    
    async fn connect_and_stream(&self) -> Result<()> {
        info!("🔌 Connecting to Dome WebSocket: {}", DOME_WS_URL);
        
        let url = format!("{}?api_key={}", DOME_WS_URL, self.api_key);
        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to WebSocket")?;
        
        let (mut write, mut read) = ws_stream.split();
        
        info!("✅ WebSocket connected, subscribing to {} wallets", 
              self.tracked_wallets.len());
        
        // Subscribe to wallet order feeds
        let subscribe_msg = WsSubscribeMessage {
            action: "subscribe".to_string(),
            channel: "orders".to_string(),
            filters: WsFilters {
                users: self.tracked_wallets.clone(),
            },
        };
        
        let sub_json = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(sub_json)).await
            .context("Failed to send subscription")?;
        
        info!("📡 Subscribed to real-time order feed");
        
        // Process incoming messages
        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<WsOrderUpdate>(&text) {
                        Ok(order) => {
                            debug!("🔔 Order update: {} {} {} @ {}",
                                   &order.user[..10],
                                   order.side,
                                   order.shares_normalized,
                                   order.price);
                            
                            // Send to processing channel
                            if let Err(e) = self.order_tx.send(order) {
                                error!("Failed to send order update: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse order update: {}", e);
                        }
                    }
                }
                Ok(Message::Ping(ping)) => {
                    write.send(Message::Pong(ping)).await?;
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("WebSocket read error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        
        Ok(())
    }
}
```

### 3.2 Update tracked_wallet_polling in main.rs

**Replace polling loop with WebSocket streaming**:

```rust
async fn tracked_wallet_polling(
    storage: Arc<DbSignalStorage>,
    signal_tx: broadcast::Sender<MarketSignal>,
) -> Result<()> {
    info!("👑 Starting tracked wallet STREAMING system (Phase 3: WebSockets)");
    
    let config = Config::from_env();
    
    let dome_api_key = match &config.dome_api_key {
        Some(key) if !key.is_empty() && key != "your_dome_api_key_here" => key.clone(),
        _ => {
            info!("⚠️  Dome API key not configured - wallet tracking disabled");
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    };
    
    // Extract wallet addresses
    let tracked_wallets: Vec<String> = config.tracked_wallets.keys()
        .cloned()
        .collect();
    
    info!("📊 Streaming {} wallets via WebSocket", tracked_wallets.len());
    
    // Create WebSocket client
    let (ws_client, mut order_rx) = DomeWebSocketClient::new(
        dome_api_key,
        tracked_wallets.clone(),
    );
    
    let detector = SignalDetector::new();
    
    // Spawn WebSocket connection task
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = ws_client.run().await {
            error!("WebSocket client error: {}", e);
        }
    });
    
    // Process incoming orders in real-time
    info!("🔥 WebSocket streaming active - real-time order flow enabled");
    
    while let Some(order) = order_rx.recv().await {
        // Find wallet label
        let wallet_label = config.tracked_wallets.get(&order.user)
            .map(|s| s.as_str())
            .unwrap_or("unknown");
        
        info!("💰 REALTIME: {} [{}] {} {} @ {}",
              &order.user[..10],
              wallet_label,
              order.side,
              order.shares_normalized,
              order.price);
        
        // Convert to DomeOrder format for detector
        let dome_order = vec![DomeOrder {
            token_id: order.token_id,
            side: order.side,
            shares_normalized: order.shares_normalized,
            price: order.price,
            timestamp: order.timestamp,
            market_slug: order.market_slug,
            title: order.title,
            user: order.user.clone(),
        }];
        
        // Detect signals
        let signals = detector.detect_trader_entry(
            &dome_order,
            &order.user,
            wallet_label,
        );
        
        // Store and broadcast immediately
        for signal in signals {
            if let Err(e) = storage.store(&signal).await {
                warn!("Failed to store signal {}: {}", signal.id, e);
            }
            let _ = signal_tx.send(signal);
        }
    }
    
    // If channel closes, wait for WebSocket task
    ws_handle.await?;
    
    Ok(())
}
```

### 3.3 Update scrapers/mod.rs

```rust
pub mod dome;
pub mod dome_tracker;      // Keep for REST API fallback
pub mod dome_websocket;    // NEW: WebSocket client
pub mod hashdive;
pub mod hashdive_api;
pub mod mock_generator;
pub mod polymarket;
pub mod polymarket_api;
```

---

## Implementation Steps

### Step 1: Create WebSocket Client (1 hour)
1. Create `rust-backend/src/scrapers/dome_websocket.rs`
2. Implement `DomeWebSocketClient` with:
   - WebSocket connection
   - Subscription management
   - Message parsing
   - Auto-reconnection with exponential backoff
3. Add comprehensive logging

### Step 2: Integrate with Main (45 min)
1. Update `tracked_wallet_polling()` in main.rs
2. Replace REST polling with WebSocket streaming
3. Maintain detector integration
4. Keep error handling intact

### Step 3: Update Module Exports (5 min)
1. Add `pub mod dome_websocket;` to scrapers/mod.rs
2. Export necessary types

### Step 4: Testing & Verification (1 hour)
1. Test WebSocket connection
2. Verify order updates received
3. Test auto-reconnection
4. Measure latency improvement
5. Stress test with all 45 wallets

### Step 5: Documentation (30 min)
1. Create PHASE_3_COMPLETE.md
2. Document WebSocket endpoints
3. Add troubleshooting guide
4. Update README

---

## Success Metrics

### Before (Polling)
- ❌ Latency: 30-90 seconds (polling interval + detection)
- ❌ API calls: 45 wallets × 20 polls/hour = 900 requests/hour
- ❌ Rate limit risk
- ❌ Missed fast trades

### After (WebSocket)
- ✅ Latency: <1 second (real-time streaming)
- ✅ API calls: 1 persistent connection
- ✅ No rate limit concerns
- ✅ Zero missed trades
- ✅ 30-90x latency improvement

---

## Fallback Strategy

If WebSocket fails:
1. Log error with details
2. Attempt reconnection with exponential backoff
3. After 5 failed reconnects, fall back to REST polling
4. Alert user of degraded mode

Keep `dome_tracker.rs` as fallback:
```rust
// In main.rs
let use_websocket = env::var("DOME_USE_WEBSOCKET")
    .unwrap_or_else(|_| "true".to_string())
    .parse::<bool>()
    .unwrap_or(true);

if use_websocket {
    // Phase 3: WebSocket streaming
    tokio::spawn(tracked_wallet_polling_websocket(...));
} else {
    // Fallback: REST polling
    tokio::spawn(tracked_wallet_polling_rest(...));
}
```

---

## Risk Mitigation

### Potential Issues
1. **WebSocket disconnects**: Auto-reconnect with exponential backoff
2. **Message parsing errors**: Log and skip, don't crash
3. **Channel overflow**: Use unbounded channel for now, monitor
4. **Authentication failures**: Clear error messages, fall back to polling

### Monitoring
- Log connection status
- Track reconnection attempts
- Measure message processing latency
- Alert on repeated failures

---

## Dependencies

**Already installed**:
- `tokio-tungstenite = "0.20"` ✅
- `futures-util` (included with tokio) ✅
- `serde` + `serde_json` ✅

**No new dependencies needed!**

---

## Phase 3 Deliverables

1. ✅ `rust-backend/src/scrapers/dome_websocket.rs` (300+ lines)
2. ✅ Updated `rust-backend/src/main.rs` (tracked_wallet_polling)
3. ✅ Updated `rust-backend/src/scrapers/mod.rs`
4. ✅ `PHASE_3_COMPLETE.md` documentation
5. ✅ Clean compilation
6. ✅ Latency tests
7. ✅ Connection resilience tests

---

## Next Phase

**Phase 4: Arbitrage Detection System**
- Cross-platform price monitoring
- Arbitrage opportunity detection
- Profitability calculations
- Risk-adjusted position sizing

---

## Notes

- Dome API WebSocket endpoint is **hypothetical** - actual endpoint may differ
- Must verify Dome API documentation for correct WebSocket URL
- May need to adjust authentication (Bearer token vs API key param)
- Message format may vary from REST API responses
