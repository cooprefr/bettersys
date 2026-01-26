use anyhow::Result;
use chrono::Utc;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{info, warn};

use crate::{
    scrapers::oracle_comparison::{global_oracle_tracker, SUPPORTED_ASSETS},
    signals::DbSignalStorage,
};

/// Background task that continuously tracks 15m Up/Down windows and persists
/// (start price, end price, outcome) to SQLite.
///
/// This runs independently of the frontend and survives backend restarts via the DB.
pub async fn spawn_updown_15m_history_collector(storage: Arc<DbSignalStorage>) -> Result<()> {
    let tracker = global_oracle_tracker();

    let mut last_window_end: HashMap<String, i64> = HashMap::new();

    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    info!("🗄️  UpDown15m history collector started (persisting to SQLite)");

    loop {
        tick.tick().await;

        let now = Utc::now().timestamp();
        let current_start = (now / 900) * 900;
        let current_end = current_start + 900;

        for asset in SUPPORTED_ASSETS {
            let prev_end = last_window_end.get(*asset).copied().unwrap_or(0);

            // First tick: start tracking the current window.
            if prev_end == 0 {
                tracker.start_window(asset, current_start, current_end);
                last_window_end.insert((*asset).to_string(), current_end);
                continue;
            }

            // Window rolled.
            if current_end != prev_end {
                if let Some(resolution) = tracker.finalize_window(asset) {
                    let has_chainlink =
                        resolution.chainlink_start.is_some() && resolution.chainlink_end.is_some();
                    let has_binance =
                        resolution.binance_start.is_some() && resolution.binance_end.is_some();

                    if has_chainlink || has_binance {
                        if let Err(e) = storage.upsert_updown_15m_window(&resolution) {
                            warn!(asset = %asset, error = %e, "failed to persist updown 15m window");
                        }
                    } else {
                        warn!(asset = %asset, "finalized 15m window but no start/end prices were recorded");
                    }
                }

                tracker.start_window(asset, current_start, current_end);
                last_window_end.insert((*asset).to_string(), current_end);
            }
        }
    }
}
