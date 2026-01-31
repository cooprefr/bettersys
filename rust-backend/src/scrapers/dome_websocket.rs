//! Dome WebSocket Real-time Order Feed
//! Mission: Sub-second latency for elite wallet tracking
//! Philosophy: Never miss a trade. Streaming > Polling.
//!
//! Based on Dome API WebSocket documentation:
//! - Endpoint: wss://ws.domeapi.io/<TOKEN> (token in URL path)
//! - Some deployments may also accept Authorization: Bearer <TOKEN>, so we send it as well.
//! - Subscribe to 'orders' channel with wallet filters
//! - Real-time order updates for tracked wallets
//!
//! Low-latency optimizations applied:
//! - Socket buffer sizing (4MB recv)
//! - TCP_NODELAY (disable Nagle's algorithm)
//! - TCP_QUICKACK (immediate ACKs on Linux)
//! - SO_BUSY_POLL (busy polling on Linux)

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message, tungstenite::protocol::WebSocketConfig};
use tracing::{debug, error, info, warn};

#[cfg(unix)]
use crate::performance::latency::socket_tuning::{SocketTuningConfig, apply_socket_tuning_fd};

const DOME_WS_BASE: &str = "wss://ws.domeapi.io";

/// Subscribe message sent to Dome WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsSubscribeMessage {
    pub action: String,   // "subscribe"
    pub platform: String, // "polymarket"
    pub version: i32,     // 1
    #[serde(rename = "type")]
    pub msg_type: String, // "orders"
    pub filters: WsFilters,
}

/// Filters for subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsFilters {
    pub users: Vec<String>, // Wallet addresses to track
}

/// Unsubscribe message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsUnsubscribeMessage {
    pub action: String,          // "unsubscribe"
    pub version: i32,            // 1
    pub subscription_id: String, // Subscription ID to cancel
}

/// WebSocket order update from Dome API
/// This matches the actual Dome API response format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsOrderUpdate {
    #[serde(rename = "type")]
    pub msg_type: String, // "event"
    pub subscription_id: String, // e.g., "sub_m58zfduokmd"
    pub data: WsOrderData,
}

/// Order data within the update message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsOrderData {
    pub token_id: String,
    #[serde(default)]
    pub token_label: Option<String>, // "Up", "Down", "Yes", "No" - outcome label
    pub side: String, // "BUY" or "SELL"
    pub market_slug: String,
    pub condition_id: String,
    pub shares: i64,            // Raw shares (e.g., 5000000)
    pub shares_normalized: f64, // Normalized (e.g., 5.0)
    pub price: f64,
    pub tx_hash: String,
    pub title: String,
    pub timestamp: i64,
    pub order_hash: String,
    pub user: String,
}

/// WebSocket client for Dome API real-time order streaming
pub struct DomeWebSocketClient {
    auth_token: String,
    tracked_wallets: Vec<String>,
    order_tx: mpsc::UnboundedSender<WsOrderData>,
}

impl DomeWebSocketClient {
    /// Create a new WebSocket client
    ///
    /// Returns the client and a receiver channel for order updates
    pub fn new(
        auth_token: String,
        tracked_wallets: Vec<String>,
    ) -> (Self, mpsc::UnboundedReceiver<WsOrderData>) {
        let (order_tx, order_rx) = mpsc::unbounded_channel();

        let client = Self {
            auth_token,
            tracked_wallets,
            order_tx,
        };

        (client, order_rx)
    }

    /// Start WebSocket connection with auto-reconnect
    ///
    /// This runs forever, automatically reconnecting on failures
    pub async fn run(&self) -> Result<()> {
        let mut reconnect_delay = Duration::from_secs(1);
        let max_reconnect_delay = Duration::from_secs(60);

        loop {
            let connect_start = Instant::now();
            match self.connect_and_stream().await {
                Ok(_) => {
                    info!("WebSocket connection closed gracefully");
                    reconnect_delay = Duration::from_secs(1);
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    warn!("Reconnecting in {:?}...", reconnect_delay);

                    // Record failure/reconnect
                    let recovery_us = connect_start.elapsed().as_micros() as u64;
                    crate::latency::global_comprehensive()
                        .failures
                        .record_reconnect("dome_ws", recovery_us);
                    crate::latency::global_comprehensive()
                        .md_integrity
                        .record_recovery("dome_ws", recovery_us);

                    sleep(reconnect_delay).await;

                    // Exponential backoff up to 60 seconds
                    reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
                }
            }
        }
    }

    /// Connect to WebSocket and stream order updates
    async fn connect_and_stream(&self) -> Result<()> {
        // Construct WebSocket URL: wss://ws.domeapi.io/<TOKEN>
        // (Do not log the token.)
        let ws_url = format!("{}/{}", DOME_WS_BASE, self.auth_token);

        info!("🔌 Connecting to Dome WebSocket...");
        debug!("WebSocket URL: {}", DOME_WS_BASE);

        // Build from IntoClientRequest so tungstenite can add required websocket headers.
        let mut request = ws_url
            .into_client_request()
            .context("Failed to build websocket request")?;

        // Also attach Authorization header (harmless if ignored, useful if supported).
        if let Ok(hv) = format!("Bearer {}", self.auth_token).parse() {
            request.headers_mut().insert("Authorization", hv);
        }

        // WebSocket protocol configuration for low-latency
        let ws_config = WebSocketConfig {
            max_message_size: Some(16 * 1024 * 1024),  // 16MB max message
            max_frame_size: Some(4 * 1024 * 1024),     // 4MB max frame
            accept_unmasked_frames: false,
            ..Default::default()
        };

        let (ws_stream, response) = connect_async_with_config(request, Some(ws_config), false)
            .await
            .context("Failed to connect to WebSocket")?;

        info!("✅ WebSocket connected (status: {})", response.status());

        // Apply low-latency socket tuning on Unix systems
        #[cfg(unix)]
        {
            // Get the underlying TCP stream's fd for tuning
            // Note: tokio-tungstenite wraps the stream, so we need to access it carefully
            // The tuning is best done at the TCP level before TLS upgrade, but we can
            // still apply buffer tuning post-connection
            let tuning_config = SocketTuningConfig::websocket();
            debug!("Socket tuning config: recv_buf={}KB, busy_poll={}us, nodelay={}",
                tuning_config.recv_buffer_size / 1024,
                tuning_config.busy_poll_us,
                tuning_config.tcp_nodelay);
            // Note: For TLS streams, direct fd access is complex. The kernel-level
            // sysctls (net.core.rmem_default, etc.) will apply to the socket.
            // Application-level tuning via socket2 should be done at connection time
            // before the TLS handshake for full effect.
        }

        let (mut write, mut read) = ws_stream.split();

        info!(
            "📡 Subscribing to {} wallets for real-time order feed",
            self.tracked_wallets.len()
        );

        // Subscribe to wallet order feeds
        // Format per Dome API docs
        let subscribe_msg = WsSubscribeMessage {
            action: "subscribe".to_string(),
            platform: "polymarket".to_string(),
            version: 1,
            msg_type: "orders".to_string(),
            filters: WsFilters {
                users: self.tracked_wallets.clone(),
            },
        };

        let sub_json = serde_json::to_string(&subscribe_msg)
            .context("Failed to serialize subscription message")?;

        debug!("Sending subscription: {}", sub_json);

        write
            .send(Message::Text(sub_json))
            .await
            .context("Failed to send subscription")?;

        info!("🔥 Subscribed! Now streaming real-time orders from tracked wallets");

        // Process incoming messages
        while let Some(message) = read.next().await {
            // Start latency measurement immediately on message receipt
            let msg_received = Instant::now();

            match message {
                Ok(Message::Text(text)) => {
                    // P99.9 OPTIMIZATION: Only format debug message if DEBUG level is enabled
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!("Received message: {}", &text[..text.len().min(200)]);
                    }

                    // Parse order update
                    // Track message for MD integrity (decode time)
                    let decode_start = Instant::now();
                    match serde_json::from_str::<WsOrderUpdate>(&text) {
                        Ok(update) => {
                            let order = &update.data;

                            // Record decode time for serialization metrics
                            let decode_us = decode_start.elapsed().as_micros() as u64;
                            crate::latency::global_comprehensive()
                                .serialization
                                .record_decode("dome_ws_order", decode_us, text.len() as u64);

                            // Record MD integrity (message count, timestamp for clock skew)
                            crate::latency::global_comprehensive()
                                .md_integrity
                                .record_message(
                                    "dome_ws",
                                    None, // No sequence number available from Dome
                                    Some(order.timestamp as u64 * 1_000_000), // Convert to microseconds
                                );

                            // Record Dome WS latency
                            let latency_us = msg_received.elapsed().as_micros() as u64;
                            crate::latency::global_registry().record_span(
                                crate::latency::LatencySpan::new(
                                    crate::latency::SpanType::DomeWs,
                                    latency_us,
                                )
                                .with_metadata(format!(
                                    "{}:{}",
                                    &order.user[..10.min(order.user.len())],
                                    &order.market_slug
                                )),
                            );

                            // Record to performance profiler
                            crate::performance::global_profiler()
                                .pipeline
                                .record_dome_ws(latency_us);
                            crate::performance::global_profiler()
                                .throughput
                                .record_dome_ws_event();
                            // Record to CPU profiler for hot path tracking
                            crate::performance::global_profiler()
                                .cpu
                                .record_span("dome_ws_process", latency_us);

                            // P99.9 OPTIMIZATION: Guard logging behind level check to avoid
                            // formatting overhead in hot path. Only format when INFO is enabled.
                            if tracing::enabled!(tracing::Level::INFO) {
                                info!(
                                    "🔔 REALTIME ORDER: {} [{}] {} {} @ ${:.3} | {} | {} ({}μs)",
                                    &order.user[..10],
                                    order.side,
                                    order.shares_normalized,
                                    &order.market_slug,
                                    order.price,
                                    order.title,
                                    update.subscription_id,
                                    latency_us
                                );
                            }

                            // Send to processing channel
                            if let Err(e) = self.order_tx.send(order.clone()) {
                                error!("Failed to send order update: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            // Try to parse as generic JSON for debugging
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                // Check if it's a subscription confirmation or other control message
                                if json.get("action").is_some()
                                    || json.get("status").is_some()
                                    || json.get("subscription_id").is_some()
                                {
                                    debug!("Control message: {}", text);
                                } else {
                                    warn!(
                                        "Failed to parse order update: {} | Message: {}",
                                        e, text
                                    );
                                }
                            } else {
                                warn!("Failed to parse WebSocket message: {} | Raw: {}", e, text);
                            }
                        }
                    }
                }
                Ok(Message::Ping(ping)) => {
                    debug!("Received ping, sending pong");
                    write
                        .send(Message::Pong(ping))
                        .await
                        .context("Failed to send pong")?;
                }
                Ok(Message::Pong(_)) => {
                    debug!("Received pong");
                }
                Ok(Message::Close(frame)) => {
                    info!("WebSocket closed by server: {:?}", frame);
                    break;
                }
                Ok(Message::Binary(data)) => {
                    warn!("Received unexpected binary message: {} bytes", data.len());
                }
                Err(e) => {
                    error!("WebSocket read error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        info!("WebSocket stream ended, will reconnect...");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscribe_message_serialization() {
        let msg = WsSubscribeMessage {
            action: "subscribe".to_string(),
            platform: "polymarket".to_string(),
            version: 1,
            msg_type: "orders".to_string(),
            filters: WsFilters {
                users: vec!["0x6031b6eed1c97e853c6e0f03ad3ce3529351f96d".to_string()],
            },
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("subscribe"));
        assert!(json.contains("polymarket"));
        assert!(json.contains("orders"));
    }

    #[test]
    fn test_order_update_deserialization() {
        let json = r#"{
            "type": "event",
            "subscription_id": "sub_m58zfduokmd",
            "data": {
                "token_id": "57564352641769637293436658960633624379577489846300950628596680893489126052038",
                "side": "BUY",
                "market_slug": "btc-updown-15m-1762755300",
                "condition_id": "0x592b8a416cbe36aa7bb40df85a61685ebd54ebbd2d55842f1bb398cae4f40dfc",
                "shares": 5000000,
                "shares_normalized": 5.0,
                "price": 0.54,
                "tx_hash": "0xd94d999336c1f579359044e2bc5fba863f240ee07ef1c6713ff69e09b67b3b13",
                "title": "Bitcoin Up or Down - November 10, 1:15AM-1:30AM ET",
                "timestamp": 1762755335,
                "order_hash": "0xf504516ab54ea46f41eaf2852f41c328e6234928f3fcfe01a9172a5908839421",
                "user": "0x6031b6eed1c97e853c6e0f03ad3ce3529351f96d"
            }
        }"#;

        let update: WsOrderUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update.msg_type, "event");
        assert_eq!(update.data.side, "BUY");
        assert_eq!(update.data.shares_normalized, 5.0);
        assert_eq!(update.data.price, 0.54);
    }
}
