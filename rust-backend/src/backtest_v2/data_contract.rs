//! Historical data contract + enforcement.

use crate::backtest_v2::clock::Nanos;
use crate::backtest_v2::events::{Event, TimestampedEvent};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderBookHistory {
    /// Full incremental L2 deltas with exchange sequence numbers.
    FullIncrementalL2DeltasWithExchangeSeq,
    /// Periodic full L2 snapshots (no deltas).
    PeriodicL2Snapshots,
    /// Top-of-book polling (best bid/ask only).
    TopOfBookPolling { interval_ns: Nanos },
    /// No order book history.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeHistory {
    /// Public trade prints.
    TradePrints,
    /// No trade history.
    None,
}

/// Arrival time semantics for backtesting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ArrivalTimeSemantics {
    /// Arrival time was recorded at ingest with nanosecond precision.
    /// Enables `RecordedArrival` policy in visibility enforcement.
    RecordedArrival,
    /// Only exchange source timestamps available; arrival must be simulated.
    #[default]
    SimulatedLatency,
    /// No reliable timestamps available.
    Unusable,
}

/// Explicit declaration of the historical market data shape consumed by the backtester.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalDataContract {
    pub venue: String,
    pub market: String,
    pub orderbook: OrderBookHistory,
    pub trades: TradeHistory,
    /// Arrival time semantics for the dataset.
    #[serde(default)]
    pub arrival_time: ArrivalTimeSemantics,
}

impl HistoricalDataContract {
    pub fn polymarket_15m_updown_hybrid_snapshots_and_trades() -> Self {
        Self {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::SimulatedLatency,
        }
    }

    /// Data contract with recorded arrival times (from historical book snapshots database).
    pub fn polymarket_15m_updown_with_recorded_arrival() -> Self {
        Self {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints, // Now available via last_trade_price WS
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        }
    }

    /// Data contract with recorded snapshots AND trade prints (best available without deltas).
    pub fn polymarket_15m_updown_snapshots_and_trades() -> Self {
        Self {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        }
    }

    /// Production-grade data contract requiring full incremental deltas.
    pub fn polymarket_15m_updown_full_deltas() -> Self {
        Self {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        }
    }

    /// Check if this contract is production-grade.
    pub fn is_production_grade(&self) -> bool {
        matches!(
            self.orderbook,
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
        )
    }

    /// Check if this contract has recorded arrival times.
    /// When true, backtests can use `RecordedArrival` policy instead of simulated latency.
    pub fn has_recorded_arrival(&self) -> bool {
        matches!(self.arrival_time, ArrivalTimeSemantics::RecordedArrival)
    }

    /// Check if this contract supports explicit queue modeling for maker fills.
    /// 
    /// Queue modeling requires:
    /// - Full incremental L2 deltas with exchange sequence numbers (to track queue position)
    /// - Trade prints (to observe queue consumption)
    /// 
    /// Without both, we cannot determine when a passive order would have been filled
    /// because we don't know the actual order flow at each price level.
    pub fn supports_queue_modeling(&self) -> bool {
        matches!(
            (&self.orderbook, &self.trades),
            (
                OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
                TradeHistory::TradePrints
            )
        )
    }

    /// Check if this contract is snapshot-only (no incremental updates).
    /// Snapshot-only data cannot support realistic maker fill modeling.
    pub fn is_snapshot_only(&self) -> bool {
        matches!(
            self.orderbook,
            OrderBookHistory::PeriodicL2Snapshots | OrderBookHistory::TopOfBookPolling { .. }
        )
    }

    /// Get the reason why queue modeling is not supported (if applicable).
    pub fn queue_modeling_unsupported_reason(&self) -> Option<String> {
        if self.supports_queue_modeling() {
            return None;
        }

        let mut reasons = Vec::new();

        match &self.orderbook {
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq => {}
            OrderBookHistory::PeriodicL2Snapshots => {
                reasons.push("orderbook is periodic snapshots (cannot track queue position)");
            }
            OrderBookHistory::TopOfBookPolling { .. } => {
                reasons.push("orderbook is top-of-book polling only (no depth information)");
            }
            OrderBookHistory::None => {
                reasons.push("no orderbook history available");
            }
        }

        match &self.trades {
            TradeHistory::TradePrints => {}
            TradeHistory::None => {
                reasons.push("no trade prints available (cannot observe queue consumption)");
            }
        }

        if reasons.is_empty() {
            None
        } else {
            Some(reasons.join("; "))
        }
    }
}

// =============================================================================
// DATASET CLASSIFICATION
// =============================================================================

/// Dataset classification based on data fidelity.
/// 
/// This classification determines what types of strategies can be validly backtested
/// and whether the results can be trusted for production deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetClassification {
    /// Full incremental L2 deltas with exchange sequence numbers + trade prints.
    /// 
    /// This is the highest fidelity classification:
    /// - Can track queue position precisely
    /// - Supports maker (passive) strategy validation
    /// - Suitable for production-grade backtests
    FullIncremental,
    
    /// Periodic snapshots or top-of-book polling.
    /// 
    /// Limitations:
    /// - Cannot track queue position (no order flow between snapshots)
    /// - Maker fills are unrealistic (cannot validate queue consumption)
    /// - Only suitable for taker (aggressive) strategy validation
    SnapshotOnly,
    
    /// Incomplete data - missing orderbook OR missing trade prints.
    /// 
    /// Limitations:
    /// - Cannot validate any fill behavior reliably
    /// - NOT suitable for production-grade backtests
    /// - Results are indicative only
    Incomplete,
}

impl DatasetClassification {
    /// Human-readable description of this classification.
    pub fn description(&self) -> &'static str {
        match self {
            Self::FullIncremental => "Full incremental L2 deltas with exchange sequence numbers + trade prints",
            Self::SnapshotOnly => "Periodic snapshots or top-of-book polling (no incremental updates)",
            Self::Incomplete => "Incomplete data (missing orderbook or trade prints)",
        }
    }
    
    /// Whether this classification supports maker (passive) strategy validation.
    pub fn supports_maker_strategies(&self) -> bool {
        matches!(self, Self::FullIncremental)
    }
    
    /// Whether this classification is suitable for production-grade backtests.
    pub fn is_production_suitable(&self) -> bool {
        matches!(self, Self::FullIncremental)
    }
    
    /// Whether this classification should be rejected outright for production.
    pub fn is_rejected_for_production(&self) -> bool {
        matches!(self, Self::Incomplete)
    }
    
    /// Get the rejection reason for production-grade mode (if applicable).
    pub fn production_rejection_reason(&self) -> Option<String> {
        match self {
            Self::FullIncremental => None,
            Self::SnapshotOnly => Some(
                "SnapshotOnly data cannot support production-grade maker strategy validation. \
                 Use FullIncremental data or switch to taker-only strategy.".to_string()
            ),
            Self::Incomplete => Some(
                "Incomplete data is rejected for production-grade backtests. \
                 Both orderbook history and trade prints are required.".to_string()
            ),
        }
    }
}

impl std::fmt::Display for DatasetClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FullIncremental => write!(f, "FULL_INCREMENTAL"),
            Self::SnapshotOnly => write!(f, "SNAPSHOT_ONLY"),
            Self::Incomplete => write!(f, "INCOMPLETE"),
        }
    }
}

// =============================================================================
// DATASET READINESS (Automatic Classification for Execution Mode Gating)
// =============================================================================

/// Dataset readiness level - determines what execution modes are allowed.
/// 
/// This is the AUTHORITATIVE classification that GATES execution modes.
/// Unlike DatasetClassification (which describes data fidelity), DatasetReadiness
/// determines what a strategy is ALLOWED to do with this data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetReadiness {
    /// Maker-viable: Full queue modeling possible.
    /// 
    /// Requirements:
    /// - FullIncremental orderbook (L2 deltas with exchange sequence)
    /// - Trade prints (for queue consumption)
    /// - RecordedArrival timestamps (for visibility enforcement)
    /// 
    /// Allowed modes: Maker strategies, Taker strategies
    MakerViable,
    
    /// Taker-only: Can execute aggressive orders, cannot make markets.
    /// 
    /// Requirements:
    /// - At least snapshots OR deltas (some orderbook visibility)
    /// - Trade prints (for queue consumption tracking even if not maker)
    /// - RecordedArrival OR SimulatedLatency (usable timestamps)
    /// 
    /// Allowed modes: Taker strategies ONLY
    TakerOnly,
    
    /// Non-representative: Data is insufficient for reliable backtesting.
    /// 
    /// This classification REJECTS the backtest run entirely.
    /// 
    /// Triggered by:
    /// - No orderbook history
    /// - No trade prints
    /// - Unusable timestamps
    /// 
    /// Allowed modes: NONE (backtest must abort)
    NonRepresentative,
}

impl DatasetReadiness {
    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::MakerViable => "Full maker viability: L2 deltas + trades + recorded arrival",
            Self::TakerOnly => "Taker-only: orderbook snapshots/deltas + trades (no queue modeling)",
            Self::NonRepresentative => "Non-representative: insufficient data for reliable backtesting",
        }
    }
    
    /// Get the short label for logging.
    pub fn label(&self) -> &'static str {
        match self {
            Self::MakerViable => "MAKER_VIABLE",
            Self::TakerOnly => "TAKER_ONLY",
            Self::NonRepresentative => "NON_REPRESENTATIVE",
        }
    }
    
    /// Whether maker (passive) strategies are allowed.
    pub fn allows_maker(&self) -> bool {
        matches!(self, Self::MakerViable)
    }
    
    /// Whether taker (aggressive) strategies are allowed.
    pub fn allows_taker(&self) -> bool {
        matches!(self, Self::MakerViable | Self::TakerOnly)
    }
    
    /// Whether the backtest should be allowed to run at all.
    pub fn allows_backtest(&self) -> bool {
        !matches!(self, Self::NonRepresentative)
    }
    
    /// Get the rejection reason if backtest is not allowed.
    pub fn rejection_reason(&self) -> Option<&'static str> {
        match self {
            Self::NonRepresentative => Some(
                "Dataset classified as NON_REPRESENTATIVE: insufficient data for reliable backtesting. \
                 Ensure orderbook history, trade prints, and valid timestamps are available."
            ),
            _ => None,
        }
    }
}

impl std::fmt::Display for DatasetReadiness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Detailed breakdown of why a dataset received its readiness classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetReadinessReport {
    /// The final readiness classification.
    pub readiness: DatasetReadiness,
    /// The underlying data classification.
    pub data_classification: DatasetClassification,
    /// Individual stream availability.
    pub streams: StreamAvailability,
    /// Reasons that led to the classification.
    pub reasons: Vec<String>,
    /// Execution modes that are gated (blocked) by this classification.
    pub gated_modes: Vec<String>,
}

/// Stream availability breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAvailability {
    /// Orderbook stream type.
    pub orderbook: OrderBookStreamStatus,
    /// Trade prints available.
    pub trade_prints: bool,
    /// Arrival time semantics.
    pub arrival_time: ArrivalTimeStatus,
}

/// Orderbook stream status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderBookStreamStatus {
    /// Full L2 deltas with exchange sequence.
    FullDeltas,
    /// Periodic snapshots.
    Snapshots,
    /// Top-of-book only.
    TopOfBook,
    /// No orderbook.
    None,
}

impl From<&OrderBookHistory> for OrderBookStreamStatus {
    fn from(history: &OrderBookHistory) -> Self {
        match history {
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq => Self::FullDeltas,
            OrderBookHistory::PeriodicL2Snapshots => Self::Snapshots,
            OrderBookHistory::TopOfBookPolling { .. } => Self::TopOfBook,
            OrderBookHistory::None => Self::None,
        }
    }
}

/// Arrival time status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrivalTimeStatus {
    /// High-resolution recorded arrival times.
    Recorded,
    /// Simulated latency (usable but less accurate).
    Simulated,
    /// Unusable timestamps.
    Unusable,
}

impl From<&ArrivalTimeSemantics> for ArrivalTimeStatus {
    fn from(semantics: &ArrivalTimeSemantics) -> Self {
        match semantics {
            ArrivalTimeSemantics::RecordedArrival => Self::Recorded,
            ArrivalTimeSemantics::SimulatedLatency => Self::Simulated,
            ArrivalTimeSemantics::Unusable => Self::Unusable,
        }
    }
}

/// Automatic dataset readiness classifier.
/// 
/// This classifier analyzes a HistoricalDataContract and produces a DatasetReadiness
/// classification that gates allowed execution modes.
#[derive(Debug, Clone, Default)]
pub struct DatasetReadinessClassifier {
    /// Whether to require RecordedArrival for MakerViable.
    /// Default: true (recommended for production)
    pub require_recorded_arrival_for_maker: bool,
    /// Whether to allow SimulatedLatency for TakerOnly.
    /// Default: true
    pub allow_simulated_latency_for_taker: bool,
}

impl DatasetReadinessClassifier {
    /// Create a new classifier with default settings.
    pub fn new() -> Self {
        Self {
            require_recorded_arrival_for_maker: true,
            allow_simulated_latency_for_taker: true,
        }
    }
    
    /// Create a strict classifier that requires RecordedArrival for all modes.
    pub fn strict() -> Self {
        Self {
            require_recorded_arrival_for_maker: true,
            allow_simulated_latency_for_taker: false,
        }
    }
    
    /// Classify a data contract and produce a readiness report.
    pub fn classify(&self, contract: &HistoricalDataContract) -> DatasetReadinessReport {
        let mut reasons = Vec::new();
        let mut gated_modes = Vec::new();
        
        // Analyze streams
        let orderbook_status = OrderBookStreamStatus::from(&contract.orderbook);
        let has_trade_prints = matches!(contract.trades, TradeHistory::TradePrints);
        let arrival_status = ArrivalTimeStatus::from(&contract.arrival_time);
        
        let streams = StreamAvailability {
            orderbook: orderbook_status,
            trade_prints: has_trade_prints,
            arrival_time: arrival_status,
        };
        
        // Get underlying data classification
        let data_classification = contract.classify();
        
        // === CLASSIFICATION LOGIC ===
        
        // Check for NonRepresentative conditions (hard failures)
        if matches!(orderbook_status, OrderBookStreamStatus::None) {
            reasons.push("No orderbook history available".to_string());
        }
        if !has_trade_prints {
            reasons.push("No trade prints available (cannot observe queue consumption)".to_string());
        }
        if matches!(arrival_status, ArrivalTimeStatus::Unusable) {
            reasons.push("Arrival timestamps are unusable".to_string());
        }
        
        // NonRepresentative if any critical stream is missing
        if matches!(orderbook_status, OrderBookStreamStatus::None) 
            || !has_trade_prints 
            || matches!(arrival_status, ArrivalTimeStatus::Unusable) 
        {
            gated_modes.push("MakerStrategies".to_string());
            gated_modes.push("TakerStrategies".to_string());
            return DatasetReadinessReport {
                readiness: DatasetReadiness::NonRepresentative,
                data_classification,
                streams,
                reasons,
                gated_modes,
            };
        }
        
        // Check for MakerViable conditions
        let has_full_deltas = matches!(orderbook_status, OrderBookStreamStatus::FullDeltas);
        let has_recorded_arrival = matches!(arrival_status, ArrivalTimeStatus::Recorded);
        
        // MakerViable requires: full deltas + trade prints + (recorded arrival if required)
        let maker_viable = has_full_deltas 
            && has_trade_prints 
            && (!self.require_recorded_arrival_for_maker || has_recorded_arrival);
        
        if maker_viable {
            reasons.push("Full L2 deltas with exchange sequence".to_string());
            reasons.push("Trade prints available for queue consumption".to_string());
            if has_recorded_arrival {
                reasons.push("Recorded arrival timestamps for visibility enforcement".to_string());
            }
            return DatasetReadinessReport {
                readiness: DatasetReadiness::MakerViable,
                data_classification,
                streams,
                reasons,
                gated_modes,
            };
        }
        
        // TakerOnly: has some orderbook + trade prints + usable timestamps
        let taker_viable = matches!(orderbook_status, OrderBookStreamStatus::FullDeltas | OrderBookStreamStatus::Snapshots | OrderBookStreamStatus::TopOfBook)
            && has_trade_prints
            && (has_recorded_arrival || (self.allow_simulated_latency_for_taker && matches!(arrival_status, ArrivalTimeStatus::Simulated)));
        
        if taker_viable {
            // Gate maker strategies
            gated_modes.push("MakerStrategies".to_string());
            
            // Explain why maker is gated
            if !has_full_deltas {
                reasons.push(format!(
                    "Orderbook is {:?} (not FullDeltas) - cannot track queue position",
                    orderbook_status
                ));
            }
            if self.require_recorded_arrival_for_maker && !has_recorded_arrival {
                reasons.push("RecordedArrival required for maker strategies but only SimulatedLatency available".to_string());
            }
            
            return DatasetReadinessReport {
                readiness: DatasetReadiness::TakerOnly,
                data_classification,
                streams,
                reasons,
                gated_modes,
            };
        }
        
        // Fallback to NonRepresentative
        reasons.push("Insufficient data for any reliable backtesting mode".to_string());
        gated_modes.push("MakerStrategies".to_string());
        gated_modes.push("TakerStrategies".to_string());
        DatasetReadinessReport {
            readiness: DatasetReadiness::NonRepresentative,
            data_classification,
            streams,
            reasons,
            gated_modes,
        }
    }
    
    /// Quick classification without full report.
    pub fn classify_quick(&self, contract: &HistoricalDataContract) -> DatasetReadiness {
        self.classify(contract).readiness
    }
}

impl DatasetReadinessReport {
    /// Generate a formatted report string for logging.
    pub fn format_report(&self) -> String {
        let mut report = String::new();
        report.push_str("╔══════════════════════════════════════════════════════════════════╗\n");
        report.push_str("║           DATASET READINESS CLASSIFICATION REPORT                ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════════╣\n");
        report.push_str(&format!("║ Readiness:        {:<48} ║\n", self.readiness.label()));
        report.push_str(&format!("║ Data Class:       {:<48} ║\n", format!("{}", self.data_classification)));
        report.push_str("╠══════════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ STREAM AVAILABILITY                                              ║\n");
        report.push_str(&format!("║   Orderbook:      {:<48} ║\n", format!("{:?}", self.streams.orderbook)));
        report.push_str(&format!("║   Trade Prints:   {:<48} ║\n", if self.streams.trade_prints { "YES" } else { "NO" }));
        report.push_str(&format!("║   Arrival Time:   {:<48} ║\n", format!("{:?}", self.streams.arrival_time)));
        report.push_str("╠══════════════════════════════════════════════════════════════════╣\n");
        report.push_str(&format!("║ Allows Maker:     {:<48} ║\n", if self.readiness.allows_maker() { "YES" } else { "NO" }));
        report.push_str(&format!("║ Allows Taker:     {:<48} ║\n", if self.readiness.allows_taker() { "YES" } else { "NO" }));
        report.push_str(&format!("║ Allows Backtest:  {:<48} ║\n", if self.readiness.allows_backtest() { "YES" } else { "NO" }));
        
        if !self.gated_modes.is_empty() {
            report.push_str("╠══════════════════════════════════════════════════════════════════╣\n");
            report.push_str("║ GATED (BLOCKED) MODES                                            ║\n");
            for mode in &self.gated_modes {
                report.push_str(&format!("║   - {:<61} ║\n", mode));
            }
        }
        
        if !self.reasons.is_empty() {
            report.push_str("╠══════════════════════════════════════════════════════════════════╣\n");
            report.push_str("║ CLASSIFICATION REASONS                                           ║\n");
            for reason in &self.reasons {
                // Wrap long reasons
                for line in textwrap_simple(reason, 60) {
                    report.push_str(&format!("║   {:<63} ║\n", line));
                }
            }
        }
        
        report.push_str("╚══════════════════════════════════════════════════════════════════╝\n");
        report
    }
}

/// Simple text wrapping helper.
fn textwrap_simple(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    
    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }
    
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    
    if lines.is_empty() {
        lines.push(String::new());
    }
    
    lines
}

// Add method to HistoricalDataContract
impl HistoricalDataContract {
    /// Classify this data contract for execution mode gating.
    pub fn readiness(&self) -> DatasetReadiness {
        DatasetReadinessClassifier::new().classify_quick(self)
    }
    
    /// Get full readiness report.
    pub fn readiness_report(&self) -> DatasetReadinessReport {
        DatasetReadinessClassifier::new().classify(self)
    }
}

impl HistoricalDataContract {
    /// Classify this data contract based on its fidelity.
    /// 
    /// Classification rules:
    /// - FullIncremental: Full L2 deltas with seq + trade prints
    /// - SnapshotOnly: Periodic snapshots or top-of-book polling (with some data)
    /// - Incomplete: Missing orderbook OR missing trade prints
    pub fn classify(&self) -> DatasetClassification {
        let has_full_orderbook = matches!(
            self.orderbook,
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
        );
        let has_snapshot_orderbook = matches!(
            self.orderbook,
            OrderBookHistory::PeriodicL2Snapshots | OrderBookHistory::TopOfBookPolling { .. }
        );
        let has_trade_prints = matches!(self.trades, TradeHistory::TradePrints);
        let has_no_orderbook = matches!(self.orderbook, OrderBookHistory::None);
        let has_no_trades = matches!(self.trades, TradeHistory::None);
        
        if has_full_orderbook && has_trade_prints {
            DatasetClassification::FullIncremental
        } else if (has_snapshot_orderbook || has_full_orderbook) && has_trade_prints {
            // Has orderbook (even if snapshots) + trade prints = SnapshotOnly
            // Note: Full orderbook without trade prints is also SnapshotOnly (can't observe queue consumption)
            if has_full_orderbook && !has_trade_prints {
                DatasetClassification::Incomplete // Full orderbook but no trades = incomplete
            } else {
                DatasetClassification::SnapshotOnly
            }
        } else if has_no_orderbook || has_no_trades {
            DatasetClassification::Incomplete
        } else {
            // Snapshot orderbook without trade prints
            DatasetClassification::Incomplete
        }
    }
    
    /// Get a detailed classification report for logging.
    pub fn classification_report(&self) -> String {
        let classification = self.classify();
        let mut report = String::new();
        
        report.push_str(&format!("=== DATASET CLASSIFICATION: {} ===\n", classification));
        report.push_str(&format!("Venue: {}\n", self.venue));
        report.push_str(&format!("Market: {}\n", self.market));
        report.push_str(&format!("Orderbook: {:?}\n", self.orderbook));
        report.push_str(&format!("Trades: {:?}\n", self.trades));
        report.push_str(&format!("Description: {}\n", classification.description()));
        report.push_str(&format!("Supports Maker Strategies: {}\n", classification.supports_maker_strategies()));
        report.push_str(&format!("Production Suitable: {}\n", classification.is_production_suitable()));
        
        if let Some(reason) = classification.production_rejection_reason() {
            report.push_str(&format!("Production Rejection: {}\n", reason));
        }
        
        report.push_str("=====================================\n");
        report
    }
}

// =============================================================================
// BACKTEST MODE
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacktestMode {
    Deterministic,
    Approximate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQualitySummary {
    pub contract: HistoricalDataContract,
    pub classification: DatasetClassification,
    pub mode: BacktestMode,
    pub is_production_grade: bool,
    pub reasons: Vec<String>,
}

impl DataQualitySummary {
    pub fn new(contract: HistoricalDataContract) -> Self {
        let classification = contract.classify();
        let mut s = Self {
            contract,
            classification,
            mode: BacktestMode::Deterministic,
            is_production_grade: classification.is_production_suitable(),
            reasons: Vec::new(),
        };

        // Set mode based on classification
        match classification {
            DatasetClassification::FullIncremental => {
                // Full incremental is deterministic
            }
            DatasetClassification::SnapshotOnly => {
                s.downgrade(format!(
                    "dataset classified as {} (periodic snapshots cannot track queue position)",
                    classification
                ));
            }
            DatasetClassification::Incomplete => {
                s.downgrade(format!(
                    "dataset classified as {} (missing orderbook or trade prints)",
                    classification
                ));
            }
        }

        s
    }

    pub fn downgrade(&mut self, reason: impl Into<String>) {
        self.mode = BacktestMode::Approximate;
        self.is_production_grade = false;
        self.reasons.push(reason.into());
    }
    
    /// Get a log-friendly summary string.
    pub fn log_summary(&self) -> String {
        format!(
            "Dataset: {} | Mode: {:?} | Production: {} | Reasons: {}",
            self.classification,
            self.mode,
            self.is_production_grade,
            if self.reasons.is_empty() { "none".to_string() } else { self.reasons.join("; ") }
        )
    }
}

#[derive(Debug)]
pub struct DataContractValidator {
    contract: HistoricalDataContract,
    summary: DataQualitySummary,
    last_exchange_seq: HashMap<String, u64>,
}

impl DataContractValidator {
    pub fn new(contract: HistoricalDataContract) -> Self {
        let summary = DataQualitySummary::new(contract.clone());
        Self {
            contract,
            summary,
            last_exchange_seq: HashMap::new(),
        }
    }

    pub fn observe(&mut self, event: &TimestampedEvent) -> Result<()> {
        // Required ingestion metadata.
        ensure!(
            event.source_time >= 0,
            "missing/invalid source_timestamp_ns (source_time={})",
            event.source_time
        );
        ensure!(
            event.time >= 0,
            "missing/invalid arrival_timestamp_ns (time={})",
            event.time
        );
        ensure!(
            event.time >= event.source_time,
            "arrival_timestamp_ns < source_timestamp_ns (arrival={}, source={})",
            event.time,
            event.source_time
        );

        match &event.event {
            Event::L2Delta {
                token_id,
                exchange_seq,
                ..
            } => {
                match self.contract.orderbook {
                    OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq => {}
                    _ => {
                        bail!(
                            "data contract violation: saw L2Delta but contract.orderbook={:?}",
                            self.contract.orderbook
                        );
                    }
                }

                if let Some(prev) = self.last_exchange_seq.get(token_id) {
                    if *exchange_seq <= *prev {
                        self.summary.downgrade(format!(
                            "non-monotonic exchange_seq for {}: prev={}, now={}",
                            token_id, prev, exchange_seq
                        ));
                    } else if *exchange_seq > *prev + 1 {
                        self.summary.downgrade(format!(
                            "exchange_seq gap for {}: prev={}, now={} (missing {})",
                            token_id,
                            prev,
                            exchange_seq,
                            exchange_seq - prev - 1
                        ));
                    }
                }

                self.last_exchange_seq
                    .insert(token_id.clone(), *exchange_seq);
            }

            Event::L2BookSnapshot {
                token_id,
                exchange_seq,
                ..
            } => {
                if matches!(self.contract.orderbook, OrderBookHistory::None) {
                    bail!(
                        "data contract violation: saw L2BookSnapshot but contract.orderbook=None"
                    );
                }

                if *exchange_seq == 0 {
                    self.summary
                        .downgrade(format!("snapshot missing exchange_seq for {}", token_id));
                }

                if let Some(prev) = self.last_exchange_seq.get(token_id) {
                    if *exchange_seq > 0 && *exchange_seq < *prev {
                        self.summary.downgrade(format!(
                            "snapshot exchange_seq regressed for {}: prev={}, now={}",
                            token_id, prev, exchange_seq
                        ));
                    }
                }

                self.last_exchange_seq
                    .insert(token_id.clone(), *exchange_seq);
            }

            Event::TradePrint { .. } => {
                if matches!(self.contract.trades, TradeHistory::None) {
                    bail!("data contract violation: saw TradePrint but contract.trades=None");
                }
            }

            _ => {}
        }

        Ok(())
    }

    pub fn summary(&self) -> &DataQualitySummary {
        &self.summary
    }

    pub fn finalize(self) -> DataQualitySummary {
        self.summary
    }
}

// =============================================================================
// RUN GRADE - Explicit Classification for Reports
// =============================================================================

/// Explicit run grade for backtest results.
/// 
/// This is the LOUD label that appears in all reports. It cannot be faked
/// or upgraded - it is derived deterministically from dataset capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunGrade {
    /// Full production-grade backtest.
    /// 
    /// Requirements:
    /// - FullIncremental orderbook (L2 deltas with exchange seq)
    /// - Trade prints available
    /// - RecordedArrival timestamps
    /// - All trust gates passed
    /// 
    /// Claim: "Execution-realistic simulation suitable for production deployment"
    ProductionGrade,
    
    /// Exploratory-grade backtest with limited data.
    /// 
    /// Characteristics:
    /// - Snapshot-only orderbook OR simulated latency
    /// - Cannot model queue position
    /// - Taker-only execution valid
    /// 
    /// Claim: "Indicative results only - maker fills NOT validated"
    ExploratoryGrade,
    
    /// Simulation-only - insufficient data for reliable backtesting.
    /// 
    /// Characteristics:
    /// - Missing orderbook OR trade prints
    /// - Unusable timestamps
    /// 
    /// Claim: "NOT execution-realistic - for exploration only"
    SimulationOnly,
}

impl RunGrade {
    /// Get the human-readable label (used in report headers).
    pub fn label(&self) -> &'static str {
        match self {
            Self::ProductionGrade => "PRODUCTION_GRADE",
            Self::ExploratoryGrade => "EXPLORATORY_GRADE",
            Self::SimulationOnly => "SIMULATION_ONLY",
        }
    }
    
    /// Get the loud warning message for non-production runs.
    pub fn warning_message(&self) -> Option<&'static str> {
        match self {
            Self::ProductionGrade => None,
            Self::ExploratoryGrade => Some(
                "⚠️  EXPLORATORY GRADE: Snapshot-only data cannot validate maker fills. \
                 Results are INDICATIVE only. Do NOT use for production deployment decisions."
            ),
            Self::SimulationOnly => Some(
                "🚫 SIMULATION ONLY: Dataset lacks required streams for reliable backtesting. \
                 Results are NOT execution-realistic. For exploration purposes only."
            ),
        }
    }
    
    /// Get the banner for report output (80-char wide).
    pub fn format_banner(&self) -> String {
        let mut banner = String::new();
        banner.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        
        match self {
            Self::ProductionGrade => {
                banner.push_str("║  ✓ PRODUCTION GRADE                                                          ║\n");
                banner.push_str("║  Execution-realistic simulation suitable for production deployment           ║\n");
            }
            Self::ExploratoryGrade => {
                banner.push_str("║  ⚠️  EXPLORATORY GRADE - NOT EXECUTION-REALISTIC                              ║\n");
                banner.push_str("║                                                                              ║\n");
                banner.push_str("║  Snapshot-only data CANNOT validate:                                        ║\n");
                banner.push_str("║    • Queue position tracking                                                 ║\n");
                banner.push_str("║    • Maker (passive) fill timing                                             ║\n");
                banner.push_str("║    • Cancel-fill race resolution                                             ║\n");
                banner.push_str("║                                                                              ║\n");
                banner.push_str("║  Results are INDICATIVE only. Do NOT deploy based on these results.         ║\n");
            }
            Self::SimulationOnly => {
                banner.push_str("║  🚫 SIMULATION ONLY - RESULTS NOT RELIABLE                                   ║\n");
                banner.push_str("║                                                                              ║\n");
                banner.push_str("║  Missing required data streams:                                              ║\n");
                banner.push_str("║    • Orderbook history may be absent                                         ║\n");
                banner.push_str("║    • Trade prints may be absent                                              ║\n");
                banner.push_str("║    • Timestamps may be unusable                                              ║\n");
                banner.push_str("║                                                                              ║\n");
                banner.push_str("║  For EXPLORATION ONLY. Never use for production decisions.                  ║\n");
            }
        }
        
        banner.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        banner
    }
    
    /// Derive run grade from dataset readiness.
    pub fn from_readiness(readiness: DatasetReadiness) -> Self {
        match readiness {
            DatasetReadiness::MakerViable => Self::ProductionGrade,
            DatasetReadiness::TakerOnly => Self::ExploratoryGrade,
            DatasetReadiness::NonRepresentative => Self::SimulationOnly,
        }
    }
    
    /// Whether this grade allows production deployment claims.
    pub fn allows_production_claims(&self) -> bool {
        matches!(self, Self::ProductionGrade)
    }
    
    /// Whether this grade should abort by default.
    pub fn should_abort_by_default(&self) -> bool {
        matches!(self, Self::SimulationOnly)
    }
}

impl std::fmt::Display for RunGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// =============================================================================
// DATASET CAPABILITIES - Granular Capability Flags
// =============================================================================

/// Granular capability flags for a dataset.
/// 
/// These flags are derived from the data contract and cannot be manually set.
/// They represent what the dataset CAN and CANNOT support.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetCapabilities {
    // === ORDERBOOK CAPABILITIES ===
    /// Full L2 deltas with exchange sequence numbers.
    pub has_full_incremental_l2_deltas: bool,
    /// Exchange sequence numbers available for gap detection.
    pub has_exchange_sequence_numbers: bool,
    /// Periodic L2 snapshots available.
    pub has_periodic_snapshots: bool,
    /// Top-of-book (BBO) available.
    pub has_top_of_book: bool,
    /// Any orderbook data available.
    pub has_any_orderbook: bool,
    
    // === TRADE CAPABILITIES ===
    /// Trade prints (public tape) available.
    pub has_trade_prints: bool,
    /// Trade aggressor side available (needed for queue consumption).
    pub has_aggressor_side: bool,
    
    // === TIMESTAMP CAPABILITIES ===
    /// Recorded arrival timestamps (nanosecond precision).
    pub has_recorded_arrival: bool,
    /// Simulated latency timestamps (usable but less accurate).
    pub has_simulated_latency: bool,
    /// Any usable timestamps.
    pub has_usable_timestamps: bool,
    
    // === DERIVED CAPABILITIES ===
    /// Can track queue position for maker fills.
    pub can_track_queue_position: bool,
    /// Can validate maker fill timing.
    pub can_validate_maker_fills: bool,
    /// Can resolve cancel-fill races.
    pub can_resolve_cancel_fill_races: bool,
    /// Can enforce arrival-time visibility.
    pub can_enforce_visibility: bool,
}

impl DatasetCapabilities {
    /// Scan a data contract and extract capabilities.
    pub fn from_contract(contract: &HistoricalDataContract) -> Self {
        let has_full_deltas = matches!(
            contract.orderbook,
            OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
        );
        let has_snapshots = matches!(contract.orderbook, OrderBookHistory::PeriodicL2Snapshots);
        let has_tob = matches!(contract.orderbook, OrderBookHistory::TopOfBookPolling { .. });
        let has_any_book = !matches!(contract.orderbook, OrderBookHistory::None);
        let has_trades = matches!(contract.trades, TradeHistory::TradePrints);
        let has_recorded = matches!(contract.arrival_time, ArrivalTimeSemantics::RecordedArrival);
        let has_simulated = matches!(contract.arrival_time, ArrivalTimeSemantics::SimulatedLatency);
        let has_usable = !matches!(contract.arrival_time, ArrivalTimeSemantics::Unusable);
        
        // Derived capabilities
        let can_track_queue = has_full_deltas && has_trades;
        let can_validate_maker = can_track_queue && has_recorded;
        let can_resolve_races = has_full_deltas && has_trades;
        let can_enforce_vis = has_usable;
        
        Self {
            has_full_incremental_l2_deltas: has_full_deltas,
            has_exchange_sequence_numbers: has_full_deltas,
            has_periodic_snapshots: has_snapshots,
            has_top_of_book: has_tob,
            has_any_orderbook: has_any_book,
            has_trade_prints: has_trades,
            has_aggressor_side: has_trades, // Assume aggressor is available with trades
            has_recorded_arrival: has_recorded,
            has_simulated_latency: has_simulated,
            has_usable_timestamps: has_usable,
            can_track_queue_position: can_track_queue,
            can_validate_maker_fills: can_validate_maker,
            can_resolve_cancel_fill_races: can_resolve_races,
            can_enforce_visibility: can_enforce_vis,
        }
    }
    
    /// Get missing capabilities compared to a requirement set.
    pub fn missing_for(&self, requirements: &StrategyRequirements) -> Vec<String> {
        let mut missing = Vec::new();
        
        if requirements.requires_full_incremental_book && !self.has_full_incremental_l2_deltas {
            missing.push("Full incremental L2 deltas".to_string());
        }
        if requirements.requires_exchange_sequence && !self.has_exchange_sequence_numbers {
            missing.push("Exchange sequence numbers".to_string());
        }
        if requirements.requires_trade_prints && !self.has_trade_prints {
            missing.push("Trade prints".to_string());
        }
        if requirements.requires_recorded_arrival && !self.has_recorded_arrival {
            missing.push("Recorded arrival timestamps".to_string());
        }
        if requirements.requires_queue_tracking && !self.can_track_queue_position {
            missing.push("Queue position tracking".to_string());
        }
        
        missing
    }
    
    /// Format as a capability report for logging.
    pub fn format_report(&self) -> String {
        let mut report = String::new();
        report.push_str("Dataset Capabilities:\n");
        report.push_str(&format!("  Orderbook:\n"));
        report.push_str(&format!("    Full L2 Deltas:      {}\n", self.has_full_incremental_l2_deltas));
        report.push_str(&format!("    Exchange Sequence:   {}\n", self.has_exchange_sequence_numbers));
        report.push_str(&format!("    Periodic Snapshots:  {}\n", self.has_periodic_snapshots));
        report.push_str(&format!("    Top-of-Book:         {}\n", self.has_top_of_book));
        report.push_str(&format!("  Trades:\n"));
        report.push_str(&format!("    Trade Prints:        {}\n", self.has_trade_prints));
        report.push_str(&format!("    Aggressor Side:      {}\n", self.has_aggressor_side));
        report.push_str(&format!("  Timestamps:\n"));
        report.push_str(&format!("    Recorded Arrival:    {}\n", self.has_recorded_arrival));
        report.push_str(&format!("    Simulated Latency:   {}\n", self.has_simulated_latency));
        report.push_str(&format!("  Derived:\n"));
        report.push_str(&format!("    Queue Tracking:      {}\n", self.can_track_queue_position));
        report.push_str(&format!("    Maker Fill Valid:    {}\n", self.can_validate_maker_fills));
        report.push_str(&format!("    Cancel-Fill Races:   {}\n", self.can_resolve_cancel_fill_races));
        report.push_str(&format!("    Visibility Enforce:  {}\n", self.can_enforce_visibility));
        report
    }
}

// =============================================================================
// STRATEGY REQUIREMENTS - What a Strategy Needs
// =============================================================================

/// Requirements declared by a strategy.
/// 
/// These requirements are compared against DatasetCapabilities to determine
/// if the dataset supports the strategy's execution model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyRequirements {
    /// Strategy name for logging.
    pub strategy_name: String,
    
    // === HARD REQUIREMENTS (will fail if not met) ===
    /// Requires full incremental L2 deltas.
    pub requires_full_incremental_book: bool,
    /// Requires exchange sequence numbers.
    pub requires_exchange_sequence: bool,
    /// Requires trade prints.
    pub requires_trade_prints: bool,
    /// Requires recorded arrival timestamps.
    pub requires_recorded_arrival: bool,
    /// Requires queue position tracking.
    pub requires_queue_tracking: bool,
    
    // === SOFT REQUIREMENTS (will warn if not met) ===
    /// Prefers full incremental book but can work with snapshots.
    pub prefers_full_incremental_book: bool,
    /// Prefers recorded arrival but can simulate.
    pub prefers_recorded_arrival: bool,
    
    /// Execution model (Maker, Taker, or Mixed).
    pub execution_model: ExecutionModel,
}

/// Execution model for a strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExecutionModel {
    /// Purely taker (aggressive) execution.
    #[default]
    Taker,
    /// Purely maker (passive) execution.
    Maker,
    /// Mixed taker and maker execution.
    Mixed,
}

impl StrategyRequirements {
    /// Create requirements for FAST15M strategy.
    /// 
    /// FAST15M is a taker-focused strategy that benefits from full incremental
    /// data but can operate with snapshot-only data in exploratory mode.
    pub fn fast15m() -> Self {
        Self {
            strategy_name: "FAST15M".to_string(),
            requires_full_incremental_book: false, // Can work with snapshots
            requires_exchange_sequence: false,
            requires_trade_prints: true, // Needs trade prints for fill timing
            requires_recorded_arrival: false, // Can simulate
            requires_queue_tracking: false, // Taker-focused
            prefers_full_incremental_book: true, // Better with deltas
            prefers_recorded_arrival: true, // Better with recorded
            execution_model: ExecutionModel::Taker,
        }
    }
    
    /// Create requirements for a production-grade maker strategy.
    pub fn production_maker() -> Self {
        Self {
            strategy_name: "ProductionMaker".to_string(),
            requires_full_incremental_book: true,
            requires_exchange_sequence: true,
            requires_trade_prints: true,
            requires_recorded_arrival: true,
            requires_queue_tracking: true,
            prefers_full_incremental_book: true,
            prefers_recorded_arrival: true,
            execution_model: ExecutionModel::Maker,
        }
    }
    
    /// Check if a dataset meets these requirements.
    pub fn check_compatibility(&self, capabilities: &DatasetCapabilities) -> StrategyCompatibility {
        let missing = capabilities.missing_for(self);
        
        if !missing.is_empty() {
            return StrategyCompatibility::Incompatible {
                missing_capabilities: missing,
            };
        }
        
        // Check soft requirements for warnings
        let mut warnings = Vec::new();
        
        if self.prefers_full_incremental_book && !capabilities.has_full_incremental_l2_deltas {
            warnings.push("Strategy prefers full incremental book but only snapshots available".to_string());
        }
        if self.prefers_recorded_arrival && !capabilities.has_recorded_arrival {
            warnings.push("Strategy prefers recorded arrival but using simulated latency".to_string());
        }
        
        // Check execution model compatibility
        match self.execution_model {
            ExecutionModel::Maker | ExecutionModel::Mixed => {
                if !capabilities.can_validate_maker_fills {
                    return StrategyCompatibility::Incompatible {
                        missing_capabilities: vec![
                            "Maker fill validation (requires full deltas + trades + recorded arrival)".to_string()
                        ],
                    };
                }
            }
            ExecutionModel::Taker => {}
        }
        
        if warnings.is_empty() {
            StrategyCompatibility::FullyCompatible
        } else {
            StrategyCompatibility::PartiallyCompatible { warnings }
        }
    }
}

/// Result of checking strategy compatibility with a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyCompatibility {
    /// Dataset fully meets all requirements.
    FullyCompatible,
    /// Dataset meets hard requirements but has soft requirement warnings.
    PartiallyCompatible { warnings: Vec<String> },
    /// Dataset does not meet hard requirements.
    Incompatible { missing_capabilities: Vec<String> },
}

impl StrategyCompatibility {
    /// Whether the strategy can run on this dataset.
    pub fn is_runnable(&self) -> bool {
        !matches!(self, Self::Incompatible { .. })
    }
    
    /// Get the warnings (empty for Incompatible).
    pub fn warnings(&self) -> &[String] {
        match self {
            Self::PartiallyCompatible { warnings } => warnings,
            _ => &[],
        }
    }
    
    /// Get the missing capabilities (empty for compatible).
    pub fn missing(&self) -> &[String] {
        match self {
            Self::Incompatible { missing_capabilities } => missing_capabilities,
            _ => &[],
        }
    }
}

// =============================================================================
// RUN CLASSIFICATION REPORT - Combined Report
// =============================================================================

/// Combined classification report for a backtest run.
/// 
/// This report combines all classification information into a single
/// structure for logging, reporting, and trust gating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunClassificationReport {
    /// The run grade (PRODUCTION_GRADE, EXPLORATORY_GRADE, SIMULATION_ONLY).
    pub grade: RunGrade,
    /// Dataset readiness level.
    pub readiness: DatasetReadiness,
    /// Dataset classification.
    pub classification: DatasetClassification,
    /// Detailed capabilities.
    pub capabilities: DatasetCapabilities,
    /// Strategy requirements (if checked).
    pub strategy_requirements: Option<StrategyRequirements>,
    /// Strategy compatibility (if checked).
    pub strategy_compatibility: Option<StrategyCompatibility>,
    /// Loud warnings for non-production runs.
    pub warnings: Vec<String>,
    /// Machine-readable fields for JSON output.
    pub machine_readable: MachineReadableClassification,
}

/// Machine-readable classification fields for JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineReadableClassification {
    /// ISO string for grade.
    pub grade_code: String,
    /// Whether production claims are allowed.
    pub production_claims_allowed: bool,
    /// Whether maker strategies are allowed.
    pub maker_allowed: bool,
    /// Whether taker strategies are allowed.
    pub taker_allowed: bool,
    /// Whether the backtest should be allowed to run.
    pub backtest_allowed: bool,
    /// List of unsupported capabilities.
    pub unsupported_capabilities: Vec<String>,
    /// List of warnings.
    pub warnings: Vec<String>,
}

impl RunClassificationReport {
    /// Create a classification report from a data contract.
    pub fn from_contract(contract: &HistoricalDataContract) -> Self {
        let readiness = contract.readiness();
        let classification = contract.classify();
        let capabilities = DatasetCapabilities::from_contract(contract);
        let grade = RunGrade::from_readiness(readiness);
        
        let mut warnings = Vec::new();
        if let Some(msg) = grade.warning_message() {
            warnings.push(msg.to_string());
        }
        
        let mut unsupported = Vec::new();
        if !capabilities.has_full_incremental_l2_deltas {
            unsupported.push("full_incremental_l2_deltas".to_string());
        }
        if !capabilities.can_track_queue_position {
            unsupported.push("queue_position_tracking".to_string());
        }
        if !capabilities.can_validate_maker_fills {
            unsupported.push("maker_fill_validation".to_string());
        }
        
        let machine_readable = MachineReadableClassification {
            grade_code: grade.label().to_string(),
            production_claims_allowed: grade.allows_production_claims(),
            maker_allowed: readiness.allows_maker(),
            taker_allowed: readiness.allows_taker(),
            backtest_allowed: readiness.allows_backtest(),
            unsupported_capabilities: unsupported,
            warnings: warnings.clone(),
        };
        
        Self {
            grade,
            readiness,
            classification,
            capabilities,
            strategy_requirements: None,
            strategy_compatibility: None,
            warnings,
            machine_readable,
        }
    }
    
    /// Add strategy requirements check to the report.
    pub fn with_strategy_check(mut self, requirements: StrategyRequirements) -> Self {
        let compatibility = requirements.check_compatibility(&self.capabilities);
        
        // Add compatibility warnings
        for warning in compatibility.warnings() {
            self.warnings.push(warning.clone());
            self.machine_readable.warnings.push(warning.clone());
        }
        
        // If incompatible, add to unsupported
        for missing in compatibility.missing() {
            self.machine_readable.unsupported_capabilities.push(missing.clone());
        }
        
        self.strategy_requirements = Some(requirements);
        self.strategy_compatibility = Some(compatibility);
        self
    }
    
    /// Format the full classification report for logging.
    pub fn format_full_report(&self) -> String {
        let mut report = String::new();
        
        // Grade banner
        report.push_str(&self.grade.format_banner());
        report.push('\n');
        
        // Readiness section
        report.push_str("DATASET READINESS\n");
        report.push_str(&format!("  Level:          {}\n", self.readiness.label()));
        report.push_str(&format!("  Classification: {}\n", self.classification));
        report.push_str(&format!("  Allows Maker:   {}\n", self.readiness.allows_maker()));
        report.push_str(&format!("  Allows Taker:   {}\n", self.readiness.allows_taker()));
        report.push('\n');
        
        // Capabilities section
        report.push_str(&self.capabilities.format_report());
        report.push('\n');
        
        // Strategy compatibility (if checked)
        if let Some(ref compat) = self.strategy_compatibility {
            if let Some(ref reqs) = self.strategy_requirements {
                report.push_str(&format!("STRATEGY COMPATIBILITY: {}\n", reqs.strategy_name));
                match compat {
                    StrategyCompatibility::FullyCompatible => {
                        report.push_str("  Status: FULLY COMPATIBLE\n");
                    }
                    StrategyCompatibility::PartiallyCompatible { warnings } => {
                        report.push_str("  Status: PARTIALLY COMPATIBLE (with warnings)\n");
                        for w in warnings {
                            report.push_str(&format!("  Warning: {}\n", w));
                        }
                    }
                    StrategyCompatibility::Incompatible { missing_capabilities } => {
                        report.push_str("  Status: INCOMPATIBLE\n");
                        for m in missing_capabilities {
                            report.push_str(&format!("  Missing: {}\n", m));
                        }
                    }
                }
                report.push('\n');
            }
        }
        
        // Warnings section
        if !self.warnings.is_empty() {
            report.push_str("WARNINGS\n");
            for w in &self.warnings {
                report.push_str(&format!("  {}\n", w));
            }
        }
        
        report
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classification_full_incremental() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        assert_eq!(contract.classify(), DatasetClassification::FullIncremental);
        assert!(contract.classify().supports_maker_strategies());
        assert!(contract.classify().is_production_suitable());
        assert!(!contract.classify().is_rejected_for_production());
    }

    #[test]
    fn test_classification_snapshot_only() {
        let contract = HistoricalDataContract::polymarket_15m_updown_hybrid_snapshots_and_trades();
        assert_eq!(contract.classify(), DatasetClassification::SnapshotOnly);
        assert!(!contract.classify().supports_maker_strategies());
        assert!(!contract.classify().is_production_suitable());
        assert!(!contract.classify().is_rejected_for_production()); // Not rejected outright, but limited
    }

    #[test]
    fn test_classification_incomplete_no_trades() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
            trades: TradeHistory::None,
            arrival_time: ArrivalTimeSemantics::default(),
        };
        assert_eq!(contract.classify(), DatasetClassification::Incomplete);
        assert!(!contract.classify().supports_maker_strategies());
        assert!(!contract.classify().is_production_suitable());
        assert!(contract.classify().is_rejected_for_production());
    }

    #[test]
    fn test_classification_incomplete_no_orderbook() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::None,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::default(),
        };
        assert_eq!(contract.classify(), DatasetClassification::Incomplete);
        assert!(contract.classify().is_rejected_for_production());
    }

    #[test]
    fn test_classification_incomplete_no_data() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::None,
            trades: TradeHistory::None,
            arrival_time: ArrivalTimeSemantics::default(),
        };
        assert_eq!(contract.classify(), DatasetClassification::Incomplete);
    }

    #[test]
    fn test_classification_top_of_book_with_trades() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::TopOfBookPolling { interval_ns: 1_000_000_000 },
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::default(),
        };
        assert_eq!(contract.classify(), DatasetClassification::SnapshotOnly);
    }

    #[test]
    fn test_data_quality_summary_full_incremental() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let summary = DataQualitySummary::new(contract);
        
        assert_eq!(summary.classification, DatasetClassification::FullIncremental);
        assert_eq!(summary.mode, BacktestMode::Deterministic);
        assert!(summary.is_production_grade);
        assert!(summary.reasons.is_empty());
    }

    #[test]
    fn test_data_quality_summary_snapshot_only() {
        let contract = HistoricalDataContract::polymarket_15m_updown_hybrid_snapshots_and_trades();
        let summary = DataQualitySummary::new(contract);
        
        assert_eq!(summary.classification, DatasetClassification::SnapshotOnly);
        assert_eq!(summary.mode, BacktestMode::Approximate);
        assert!(!summary.is_production_grade);
        assert!(!summary.reasons.is_empty());
        assert!(summary.reasons[0].contains("SNAPSHOT_ONLY"));
    }

    #[test]
    fn test_classification_report_format() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let report = contract.classification_report();
        
        assert!(report.contains("FULL_INCREMENTAL"));
        assert!(report.contains("Polymarket"));
        assert!(report.contains("Production Suitable: true"));
    }

    #[test]
    fn test_production_rejection_reasons() {
        assert!(DatasetClassification::FullIncremental.production_rejection_reason().is_none());
        assert!(DatasetClassification::SnapshotOnly.production_rejection_reason().is_some());
        assert!(DatasetClassification::Incomplete.production_rejection_reason().is_some());
    }
    
    // =========================================================================
    // DATASET READINESS TESTS
    // =========================================================================
    
    #[test]
    fn test_readiness_maker_viable_full_deltas_with_recorded_arrival() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let report = contract.readiness_report();
        
        assert_eq!(report.readiness, DatasetReadiness::MakerViable);
        assert!(report.readiness.allows_maker());
        assert!(report.readiness.allows_taker());
        assert!(report.readiness.allows_backtest());
        assert!(report.gated_modes.is_empty());
    }
    
    #[test]
    fn test_readiness_taker_only_snapshots_with_trades() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let report = contract.readiness_report();
        
        // Snapshots + trades + recorded arrival = TakerOnly
        assert_eq!(report.readiness, DatasetReadiness::TakerOnly);
        assert!(!report.readiness.allows_maker());
        assert!(report.readiness.allows_taker());
        assert!(report.readiness.allows_backtest());
        assert!(report.gated_modes.contains(&"MakerStrategies".to_string()));
    }
    
    #[test]
    fn test_readiness_non_representative_no_trades() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::None,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        };
        let report = contract.readiness_report();
        
        assert_eq!(report.readiness, DatasetReadiness::NonRepresentative);
        assert!(!report.readiness.allows_maker());
        assert!(!report.readiness.allows_taker());
        assert!(!report.readiness.allows_backtest());
    }
    
    #[test]
    fn test_readiness_non_representative_no_orderbook() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::None,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        };
        let report = contract.readiness_report();
        
        assert_eq!(report.readiness, DatasetReadiness::NonRepresentative);
        assert!(!report.readiness.allows_backtest());
    }
    
    #[test]
    fn test_readiness_non_representative_unusable_timestamps() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::Unusable,
        };
        let report = contract.readiness_report();
        
        assert_eq!(report.readiness, DatasetReadiness::NonRepresentative);
        assert!(!report.readiness.allows_backtest());
    }
    
    #[test]
    fn test_readiness_taker_with_simulated_latency() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::SimulatedLatency,
        };
        let classifier = DatasetReadinessClassifier::new();
        let report = classifier.classify(&contract);
        
        // Default classifier allows simulated latency for taker
        assert_eq!(report.readiness, DatasetReadiness::TakerOnly);
        assert!(report.readiness.allows_taker());
    }
    
    #[test]
    fn test_readiness_strict_classifier_rejects_simulated_latency() {
        let contract = HistoricalDataContract {
            venue: "Test".to_string(),
            market: "Test".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::SimulatedLatency,
        };
        let classifier = DatasetReadinessClassifier::strict();
        let report = classifier.classify(&contract);
        
        // Strict classifier requires recorded arrival for taker too
        assert_eq!(report.readiness, DatasetReadiness::NonRepresentative);
    }
    
    #[test]
    fn test_readiness_report_format() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let report = contract.readiness_report();
        let formatted = report.format_report();
        
        assert!(formatted.contains("MAKER_VIABLE"));
        assert!(formatted.contains("Allows Maker:"));
        assert!(formatted.contains("YES"));
    }
    
    #[test]
    fn test_readiness_description() {
        assert!(DatasetReadiness::MakerViable.description().contains("maker"));
        assert!(DatasetReadiness::TakerOnly.description().contains("Taker"));
        assert!(DatasetReadiness::NonRepresentative.description().contains("insufficient"));
    }
    
    #[test]
    fn test_readiness_labels() {
        assert_eq!(DatasetReadiness::MakerViable.label(), "MAKER_VIABLE");
        assert_eq!(DatasetReadiness::TakerOnly.label(), "TAKER_ONLY");
        assert_eq!(DatasetReadiness::NonRepresentative.label(), "NON_REPRESENTATIVE");
    }
    
    #[test]
    fn test_readiness_rejection_reason() {
        assert!(DatasetReadiness::MakerViable.rejection_reason().is_none());
        assert!(DatasetReadiness::TakerOnly.rejection_reason().is_none());
        assert!(DatasetReadiness::NonRepresentative.rejection_reason().is_some());
    }
    
    #[test]
    fn test_stream_availability_from_orderbook_history() {
        assert_eq!(
            OrderBookStreamStatus::from(&OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq),
            OrderBookStreamStatus::FullDeltas
        );
        assert_eq!(
            OrderBookStreamStatus::from(&OrderBookHistory::PeriodicL2Snapshots),
            OrderBookStreamStatus::Snapshots
        );
        assert_eq!(
            OrderBookStreamStatus::from(&OrderBookHistory::TopOfBookPolling { interval_ns: 1000 }),
            OrderBookStreamStatus::TopOfBook
        );
        assert_eq!(
            OrderBookStreamStatus::from(&OrderBookHistory::None),
            OrderBookStreamStatus::None
        );
    }
    
    #[test]
    fn test_arrival_time_status_from_semantics() {
        assert_eq!(
            ArrivalTimeStatus::from(&ArrivalTimeSemantics::RecordedArrival),
            ArrivalTimeStatus::Recorded
        );
        assert_eq!(
            ArrivalTimeStatus::from(&ArrivalTimeSemantics::SimulatedLatency),
            ArrivalTimeStatus::Simulated
        );
        assert_eq!(
            ArrivalTimeStatus::from(&ArrivalTimeSemantics::Unusable),
            ArrivalTimeStatus::Unusable
        );
    }
    
    // =========================================================================
    // READINESS VERDICT TESTS (Prompt 10)
    // =========================================================================
    
    #[test]
    fn test_current_contract_is_taker_only() {
        // The current best-available contract with recorded arrival
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        
        // Verify data contract fields
        assert_eq!(contract.venue, "Polymarket");
        assert_eq!(contract.market, "15m up/down");
        assert_eq!(contract.orderbook, OrderBookHistory::PeriodicL2Snapshots);
        assert_eq!(contract.trades, TradeHistory::TradePrints);
        assert_eq!(contract.arrival_time, ArrivalTimeSemantics::RecordedArrival);
        
        // Classification should be SnapshotOnly
        let classification = contract.classify();
        assert_eq!(classification, DatasetClassification::SnapshotOnly);
        
        // Readiness should be TakerOnly
        let report = contract.readiness_report();
        assert_eq!(report.readiness, DatasetReadiness::TakerOnly);
        
        // Maker should be gated
        assert!(!report.readiness.allows_maker());
        assert!(report.readiness.allows_taker());
        assert!(report.readiness.allows_backtest());
        
        // Gated modes should include MakerStrategies
        assert!(report.gated_modes.contains(&"MakerStrategies".to_string()));
    }
    
    #[test]
    fn test_full_deltas_contract_is_maker_viable() {
        // The production-grade contract with full deltas
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        
        // Verify data contract fields
        assert_eq!(contract.orderbook, OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq);
        assert_eq!(contract.trades, TradeHistory::TradePrints);
        assert_eq!(contract.arrival_time, ArrivalTimeSemantics::RecordedArrival);
        
        // Classification should be FullIncremental
        let classification = contract.classify();
        assert_eq!(classification, DatasetClassification::FullIncremental);
        
        // Readiness should be MakerViable
        let report = contract.readiness_report();
        assert_eq!(report.readiness, DatasetReadiness::MakerViable);
        
        // All modes allowed
        assert!(report.readiness.allows_maker());
        assert!(report.readiness.allows_taker());
        assert!(report.readiness.allows_backtest());
        
        // No gated modes
        assert!(report.gated_modes.is_empty());
    }
    
    #[test]
    fn test_readiness_verdict_upgrade_path() {
        // Current contract: PeriodicL2Snapshots → TakerOnly
        let current = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let current_readiness = current.readiness();
        assert_eq!(current_readiness, DatasetReadiness::TakerOnly);
        
        // Upgraded contract: FullIncrementalL2DeltasWithExchangeSeq → MakerViable
        let upgraded = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let upgraded_readiness = upgraded.readiness();
        assert_eq!(upgraded_readiness, DatasetReadiness::MakerViable);
        
        // Verify the ONLY difference is orderbook type
        assert_eq!(current.venue, upgraded.venue);
        assert_eq!(current.market, upgraded.market);
        assert_eq!(current.trades, upgraded.trades);
        assert_eq!(current.arrival_time, upgraded.arrival_time);
        assert_ne!(current.orderbook, upgraded.orderbook);
        
        // The single upgrade required:
        // OrderBookHistory::PeriodicL2Snapshots → OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq
    }
    
    #[test]
    fn test_supported_claims_taker_only() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let readiness = contract.readiness();
        
        assert_eq!(readiness, DatasetReadiness::TakerOnly);
        
        // Supported claims (what TakerOnly CAN do):
        // - Execute aggressive (taker) orders ✅
        // - Track arrival-time visibility ✅
        // - Validate fill timing ✅
        // - Calculate execution costs ✅
        assert!(readiness.allows_taker());
        assert!(readiness.allows_backtest());
        
        // Unsupported claims (what TakerOnly CANNOT do):
        // - Track queue position ❌
        // - Credit maker fills ❌
        // - Resolve cancel-fill races ❌
        assert!(!readiness.allows_maker());
    }
    
    #[test]
    fn test_supported_claims_maker_viable() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let readiness = contract.readiness();
        
        assert_eq!(readiness, DatasetReadiness::MakerViable);
        
        // All claims supported:
        // - Execute aggressive (taker) orders ✅
        // - Execute passive (maker) orders ✅
        // - Track queue position ✅
        // - Credit maker fills (when queue_ahead <= 0) ✅
        // - Resolve cancel-fill races ✅
        // - Track arrival-time visibility ✅
        assert!(readiness.allows_maker());
        assert!(readiness.allows_taker());
        assert!(readiness.allows_backtest());
    }
    
    #[test]
    fn test_verdict_derived_from_code_not_flags() {
        // This test verifies that readiness is derived from data contract
        // structure, not from configuration flags
        
        // Same venue/market, different orderbook type → different readiness
        let snapshots = HistoricalDataContract {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::PeriodicL2Snapshots,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        };
        
        let deltas = HistoricalDataContract {
            venue: "Polymarket".to_string(),
            market: "15m up/down".to_string(),
            orderbook: OrderBookHistory::FullIncrementalL2DeltasWithExchangeSeq,
            trades: TradeHistory::TradePrints,
            arrival_time: ArrivalTimeSemantics::RecordedArrival,
        };
        
        // The ONLY difference is orderbook type
        // This difference drives readiness classification
        assert_eq!(snapshots.readiness(), DatasetReadiness::TakerOnly);
        assert_eq!(deltas.readiness(), DatasetReadiness::MakerViable);
        
        // No flags involved - purely structural
    }
    
    // =========================================================================
    // RUN GRADE TESTS
    // =========================================================================
    
    #[test]
    fn test_run_grade_from_readiness() {
        assert_eq!(RunGrade::from_readiness(DatasetReadiness::MakerViable), RunGrade::ProductionGrade);
        assert_eq!(RunGrade::from_readiness(DatasetReadiness::TakerOnly), RunGrade::ExploratoryGrade);
        assert_eq!(RunGrade::from_readiness(DatasetReadiness::NonRepresentative), RunGrade::SimulationOnly);
    }
    
    #[test]
    fn test_run_grade_labels() {
        assert_eq!(RunGrade::ProductionGrade.label(), "PRODUCTION_GRADE");
        assert_eq!(RunGrade::ExploratoryGrade.label(), "EXPLORATORY_GRADE");
        assert_eq!(RunGrade::SimulationOnly.label(), "SIMULATION_ONLY");
    }
    
    #[test]
    fn test_run_grade_warning_messages() {
        assert!(RunGrade::ProductionGrade.warning_message().is_none());
        assert!(RunGrade::ExploratoryGrade.warning_message().is_some());
        assert!(RunGrade::SimulationOnly.warning_message().is_some());
        
        // Warning should mention key terms
        let exploratory_msg = RunGrade::ExploratoryGrade.warning_message().unwrap();
        assert!(exploratory_msg.contains("EXPLORATORY"));
        assert!(exploratory_msg.contains("INDICATIVE"));
        
        let simulation_msg = RunGrade::SimulationOnly.warning_message().unwrap();
        assert!(simulation_msg.contains("SIMULATION"));
        assert!(simulation_msg.contains("NOT execution-realistic"));
    }
    
    #[test]
    fn test_run_grade_production_claims() {
        assert!(RunGrade::ProductionGrade.allows_production_claims());
        assert!(!RunGrade::ExploratoryGrade.allows_production_claims());
        assert!(!RunGrade::SimulationOnly.allows_production_claims());
    }
    
    #[test]
    fn test_run_grade_abort_by_default() {
        assert!(!RunGrade::ProductionGrade.should_abort_by_default());
        assert!(!RunGrade::ExploratoryGrade.should_abort_by_default());
        assert!(RunGrade::SimulationOnly.should_abort_by_default());
    }
    
    #[test]
    fn test_run_grade_banner_format() {
        let banner = RunGrade::ProductionGrade.format_banner();
        assert!(banner.contains("PRODUCTION GRADE"));
        assert!(banner.contains("╔"));
        assert!(banner.contains("╚"));
        
        let banner = RunGrade::ExploratoryGrade.format_banner();
        assert!(banner.contains("EXPLORATORY GRADE"));
        assert!(banner.contains("Queue position tracking"));
        
        let banner = RunGrade::SimulationOnly.format_banner();
        assert!(banner.contains("SIMULATION ONLY"));
        assert!(banner.contains("EXPLORATION ONLY"));
    }
    
    // =========================================================================
    // DATASET CAPABILITIES TESTS
    // =========================================================================
    
    #[test]
    fn test_capabilities_from_full_deltas_contract() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let caps = DatasetCapabilities::from_contract(&contract);
        
        assert!(caps.has_full_incremental_l2_deltas);
        assert!(caps.has_exchange_sequence_numbers);
        assert!(!caps.has_periodic_snapshots);
        assert!(caps.has_trade_prints);
        assert!(caps.has_recorded_arrival);
        assert!(caps.has_usable_timestamps);
        
        // Derived
        assert!(caps.can_track_queue_position);
        assert!(caps.can_validate_maker_fills);
        assert!(caps.can_resolve_cancel_fill_races);
        assert!(caps.can_enforce_visibility);
    }
    
    #[test]
    fn test_capabilities_from_snapshot_contract() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let caps = DatasetCapabilities::from_contract(&contract);
        
        assert!(!caps.has_full_incremental_l2_deltas);
        assert!(!caps.has_exchange_sequence_numbers);
        assert!(caps.has_periodic_snapshots);
        assert!(caps.has_trade_prints);
        assert!(caps.has_recorded_arrival);
        
        // Derived - snapshot cannot track queue
        assert!(!caps.can_track_queue_position);
        assert!(!caps.can_validate_maker_fills);
        assert!(!caps.can_resolve_cancel_fill_races);
        assert!(caps.can_enforce_visibility);
    }
    
    #[test]
    fn test_capabilities_format_report() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let caps = DatasetCapabilities::from_contract(&contract);
        let report = caps.format_report();
        
        assert!(report.contains("Dataset Capabilities"));
        assert!(report.contains("Full L2 Deltas"));
        assert!(report.contains("Queue Tracking"));
    }
    
    // =========================================================================
    // STRATEGY REQUIREMENTS TESTS
    // =========================================================================
    
    #[test]
    fn test_fast15m_requirements() {
        let reqs = StrategyRequirements::fast15m();
        
        assert_eq!(reqs.strategy_name, "FAST15M");
        assert!(!reqs.requires_full_incremental_book);
        assert!(reqs.requires_trade_prints);
        assert!(!reqs.requires_queue_tracking);
        assert!(reqs.prefers_full_incremental_book);
        assert_eq!(reqs.execution_model, ExecutionModel::Taker);
    }
    
    #[test]
    fn test_production_maker_requirements() {
        let reqs = StrategyRequirements::production_maker();
        
        assert!(reqs.requires_full_incremental_book);
        assert!(reqs.requires_exchange_sequence);
        assert!(reqs.requires_trade_prints);
        assert!(reqs.requires_recorded_arrival);
        assert!(reqs.requires_queue_tracking);
        assert_eq!(reqs.execution_model, ExecutionModel::Maker);
    }
    
    #[test]
    fn test_fast15m_compatible_with_snapshots() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let caps = DatasetCapabilities::from_contract(&contract);
        let reqs = StrategyRequirements::fast15m();
        
        let compat = reqs.check_compatibility(&caps);
        assert!(compat.is_runnable());
        
        // Should have warnings about preference for full deltas
        match compat {
            StrategyCompatibility::PartiallyCompatible { warnings } => {
                assert!(!warnings.is_empty());
            }
            StrategyCompatibility::FullyCompatible => {
                // This is also acceptable if preferences match
            }
            StrategyCompatibility::Incompatible { .. } => {
                panic!("FAST15M should be compatible with snapshot data");
            }
        }
    }
    
    #[test]
    fn test_fast15m_fully_compatible_with_full_deltas() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let caps = DatasetCapabilities::from_contract(&contract);
        let reqs = StrategyRequirements::fast15m();
        
        let compat = reqs.check_compatibility(&caps);
        assert!(compat.is_runnable());
        assert!(matches!(compat, StrategyCompatibility::FullyCompatible));
    }
    
    #[test]
    fn test_maker_strategy_incompatible_with_snapshots() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let caps = DatasetCapabilities::from_contract(&contract);
        let reqs = StrategyRequirements::production_maker();
        
        let compat = reqs.check_compatibility(&caps);
        assert!(!compat.is_runnable());
        assert!(!compat.missing().is_empty());
    }
    
    #[test]
    fn test_maker_strategy_compatible_with_full_deltas() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let caps = DatasetCapabilities::from_contract(&contract);
        let reqs = StrategyRequirements::production_maker();
        
        let compat = reqs.check_compatibility(&caps);
        assert!(compat.is_runnable());
    }
    
    // =========================================================================
    // RUN CLASSIFICATION REPORT TESTS
    // =========================================================================
    
    #[test]
    fn test_classification_report_from_full_deltas() {
        let contract = HistoricalDataContract::polymarket_15m_updown_full_deltas();
        let report = RunClassificationReport::from_contract(&contract);
        
        assert_eq!(report.grade, RunGrade::ProductionGrade);
        assert_eq!(report.readiness, DatasetReadiness::MakerViable);
        assert_eq!(report.classification, DatasetClassification::FullIncremental);
        assert!(report.warnings.is_empty());
        
        // Machine-readable
        assert_eq!(report.machine_readable.grade_code, "PRODUCTION_GRADE");
        assert!(report.machine_readable.production_claims_allowed);
        assert!(report.machine_readable.maker_allowed);
        assert!(report.machine_readable.taker_allowed);
    }
    
    #[test]
    fn test_classification_report_from_snapshots() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let report = RunClassificationReport::from_contract(&contract);
        
        assert_eq!(report.grade, RunGrade::ExploratoryGrade);
        assert_eq!(report.readiness, DatasetReadiness::TakerOnly);
        assert!(!report.warnings.is_empty());
        
        // Machine-readable
        assert_eq!(report.machine_readable.grade_code, "EXPLORATORY_GRADE");
        assert!(!report.machine_readable.production_claims_allowed);
        assert!(!report.machine_readable.maker_allowed);
        assert!(report.machine_readable.taker_allowed);
    }
    
    #[test]
    fn test_classification_report_with_strategy_check() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let report = RunClassificationReport::from_contract(&contract)
            .with_strategy_check(StrategyRequirements::fast15m());
        
        assert!(report.strategy_requirements.is_some());
        assert!(report.strategy_compatibility.is_some());
        
        // FAST15M should be runnable
        if let Some(compat) = &report.strategy_compatibility {
            assert!(compat.is_runnable());
        }
    }
    
    #[test]
    fn test_classification_report_format_with_strategy() {
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let report = RunClassificationReport::from_contract(&contract)
            .with_strategy_check(StrategyRequirements::fast15m());
        
        let formatted = report.format_full_report();
        
        assert!(formatted.contains("EXPLORATORY GRADE"));
        assert!(formatted.contains("DATASET READINESS"));
        assert!(formatted.contains("Dataset Capabilities"));
        assert!(formatted.contains("FAST15M"));
    }
    
    #[test]
    fn test_snapshot_only_cannot_claim_production() {
        // This is the key test: snapshot-only data cannot be labeled production-grade
        let contract = HistoricalDataContract::polymarket_15m_updown_with_recorded_arrival();
        let report = RunClassificationReport::from_contract(&contract);
        
        // Grade should be EXPLORATORY, not PRODUCTION
        assert_eq!(report.grade, RunGrade::ExploratoryGrade);
        assert!(!report.grade.allows_production_claims());
        
        // Machine-readable should reflect this
        assert!(!report.machine_readable.production_claims_allowed);
        
        // Warnings should be loud about this
        assert!(!report.warnings.is_empty());
        
        // Banner should warn about limitations
        let banner = report.grade.format_banner();
        assert!(banner.contains("NOT EXECUTION-REALISTIC"));
        assert!(banner.contains("INDICATIVE"));
    }
}
