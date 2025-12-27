//! Polymarket CLOB WebSocket (market channel) cache.
//!
//! Goal: HFT-grade orderbook snapshots.
//! - Maintain a single WS connection to Polymarket market channel
//! - Subscribe on-demand to token_ids (asset_ids)
//! - Cache latest L2 book per token_id for ultra-fast `/api/market/snapshot`

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::scrapers::polymarket::OrderBook;

const POLYMARKET_MARKET_WSS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

#[derive(Debug)]
enum WsCommand {
    Subscribe(String),
}

#[derive(Clone)]
pub struct PolymarketMarketWsCache {
    cmd_tx: mpsc::Sender<WsCommand>,
    books: Arc<RwLock<HashMap<String, CachedOrderBook>>>,
}

#[derive(Clone)]
struct CachedOrderBook {
    orderbook: Arc<OrderBook>,
    updated_at_ms: i64,
}

#[derive(Debug, Deserialize)]
struct WsBookMsg {
    pub event_type: String,
    #[serde(rename = "asset_id")]
    pub asset_id: String,
    #[serde(default)]
    pub bids: Vec<crate::scrapers::polymarket::Order>,
    #[serde(default)]
    pub asks: Vec<crate::scrapers::polymarket::Order>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

impl PolymarketMarketWsCache {
    /// Spawn the WS cache worker and return a handle that API routes can use.
    pub fn spawn() -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WsCommand>(1024);
        let cache = Arc::new(Self {
            cmd_tx,
            books: Arc::new(RwLock::new(HashMap::with_capacity(256))),
        });

        let worker_cache = cache.clone();
        tokio::spawn(async move {
            if let Err(e) = worker_cache.run(cmd_rx).await {
                warn!(error = %e, "Polymarket market WS cache worker exited");
            }
        });

        cache
    }

    /// Request subscription to a token_id (asset_id). Non-blocking.
    pub fn request_subscribe(&self, token_id: &str) {
        if token_id.trim().is_empty() {
            return;
        }
        let _ = self
            .cmd_tx
            .try_send(WsCommand::Subscribe(token_id.trim().to_string()));
    }

    /// Get latest cached orderbook if it is fresh.
    pub fn get_orderbook(&self, token_id: &str, max_age_ms: i64) -> Option<Arc<OrderBook>> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let books = self.books.read();
        let cached = books.get(token_id)?;
        if max_age_ms > 0 && now_ms - cached.updated_at_ms > max_age_ms {
            return None;
        }
        Some(cached.orderbook.clone())
    }

    async fn run(self: Arc<Self>, mut cmd_rx: mpsc::Receiver<WsCommand>) -> Result<()> {
        let mut desired_assets: HashSet<String> = HashSet::with_capacity(256);
        let mut reconnect_delay = Duration::from_secs(1);
        let max_reconnect_delay = Duration::from_secs(30);

        loop {
            // Wait for at least one subscription request.
            while desired_assets.is_empty() {
                match cmd_rx.recv().await {
                    Some(WsCommand::Subscribe(token)) => {
                        desired_assets.insert(token);
                    }
                    None => return Ok(()),
                }
            }

            match self
                .connect_and_stream(&mut cmd_rx, &mut desired_assets)
                .await
            {
                Ok(_) => {
                    reconnect_delay = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!(error = %e, "Polymarket market WS disconnected; reconnecting");
                    sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
                }
            }
        }
    }

    async fn connect_and_stream(
        &self,
        cmd_rx: &mut mpsc::Receiver<WsCommand>,
        desired_assets: &mut HashSet<String>,
    ) -> Result<()> {
        info!("🔌 Connecting to Polymarket market WS");
        let (ws_stream, resp) = connect_async(POLYMARKET_MARKET_WSS_URL)
            .await
            .context("connect_async market ws")?;

        info!(
            "✅ Polymarket market WS connected (status={})",
            resp.status()
        );

        let (mut write, mut read) = ws_stream.split();

        // Initial subscription.
        let initial_assets: Vec<String> = desired_assets.iter().cloned().collect();
        let sub_msg = serde_json::json!({
            "type": "market",
            "assets_ids": initial_assets,
        });
        write
            .send(Message::Text(sub_msg.to_string()))
            .await
            .context("send initial market subscription")?;

        let mut ping = interval(Duration::from_secs(5));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ping.tick() => {
                    // Polymarket docs expect "PING" text frames.
                    let _ = write.send(Message::Text("PING".to_string())).await;
                }
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        return Ok(());
                    };
                    match cmd {
                        WsCommand::Subscribe(token) => {
                            if desired_assets.insert(token.clone()) {
                                let msg = serde_json::json!({
                                    "assets_ids": [token],
                                    "operation": "subscribe",
                                });
                                let _ = write.send(Message::Text(msg.to_string())).await;
                            }
                        }
                    }
                }
                ws_msg = read.next() => {
                    let Some(ws_msg) = ws_msg else {
                        return Err(anyhow::anyhow!("market ws stream ended"));
                    };

                    match ws_msg {
                        Ok(Message::Text(text)) => {
                            self.handle_text_message(&text);
                        }
                        Ok(Message::Ping(payload)) => {
                            let _ = write.send(Message::Pong(payload)).await;
                        }
                        Ok(Message::Close(frame)) => {
                            debug!(?frame, "market ws close");
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(anyhow::anyhow!("market ws error: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn handle_text_message(&self, text: &str) {
        // Ignore PONG control frames or non-JSON messages.
        if text.eq_ignore_ascii_case("PONG") {
            return;
        }

        let json: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        // Fast-path: only cache full books.
        let event_type = json
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if event_type != "book" {
            return;
        }

        let msg: WsBookMsg = match serde_json::from_value(json) {
            Ok(v) => v,
            Err(e) => {
                debug!(error = %e, "failed to parse market ws book msg");
                return;
            }
        };

        let updated_at_ms = msg
            .timestamp
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

        let mut ob = OrderBook {
            bids: msg.bids,
            asks: msg.asks,
        };
        sort_orderbook(&mut ob);

        self.books.write().insert(
            msg.asset_id,
            CachedOrderBook {
                orderbook: Arc::new(ob),
                updated_at_ms,
            },
        );
    }
}

fn sort_orderbook(orderbook: &mut OrderBook) {
    orderbook.bids.sort_by(|a, b| {
        b.price
            .partial_cmp(&a.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    orderbook.asks.sort_by(|a, b| {
        a.price
            .partial_cmp(&b.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}
