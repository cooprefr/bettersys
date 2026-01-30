//! Dome Replay Ingestion CLI
//!
//! Run with: cargo run --bin dome_replay_ingest

use anyhow::Result;
use betterbot_backend::scrapers::dome_replay_ingest::DomeReplayIngestor;

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("DOME_API_KEY")
        .expect("DOME_API_KEY must be set");
    
    // Authoritative window (epoch only)
    let start_ms: i64 = 1769413205000;
    let end_ms: i64 = 1769419076000;
    let margin_ms: i64 = 120000;
    
    let db_path = "dome_replay_data_v3.db";
    
    let mut ingestor = DomeReplayIngestor::new(
        api_key,
        db_path,
        start_ms,
        end_ms,
        margin_ms,
    )?;
    
    let receipt = ingestor.run().await?;
    
    // Output receipt JSON only
    println!("{}", serde_json::to_string(&receipt)?);
    
    Ok(())
}
