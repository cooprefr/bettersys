//! Backtesting Framework
//!
//! Deterministic HFT backtesting engine for Polymarket-style CLOB markets.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     BacktestOrchestrator                        │
//! │  (owns Clock, drives event loop, enforces determinism)          │
//! └─────────────────────────────────────────────────────────────────┘
//!                                │
//!        ┌───────────────────────┼───────────────────────┐
//!        ▼                       ▼                       ▼
//! ┌─────────────┐        ┌─────────────┐        ┌─────────────┐
//! │ DataFeed    │        │  SimClock   │        │  Metrics    │
//! │ (replay)    │        │ (nanos)     │        │  Recorder   │
//! └─────────────┘        └─────────────┘        └─────────────┘
//!        │                       │
//!        └───────────┬───────────┘
//!                    ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        EventQueue                               │
//! │  BinaryHeap<(time, priority, source, seq)> - deterministic      │
//! └─────────────────────────────────────────────────────────────────┘
//!                    │
//!                    ▼
//! ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
//! │  Strategy   │───▶│ RiskEngine  │───▶│ OrderMgr    │
//! │  (trait)    │    │ (checks)    │    │ (lifecycle) │
//! └─────────────┘    └─────────────┘    └──────┬──────┘
//!                                              │
//!                                              ▼
//!                                       ┌─────────────┐
//!                                       │ Matching    │
//!                                       │ Simulator   │
//!                                       └──────┬──────┘
//!                                              │
//!                                              ▼
//!                                       ┌─────────────┐
//!                                       │ Portfolio   │
//!                                       │ (positions) │
//!                                       └─────────────┘
//! ```
//!
//! # Determinism Guarantees
//!
//! - **Clock**: Never calls system time; all time from `SimClock`
//! - **EventQueue**: `(time, priority, source, seq)` ordering
//! - **RNG**: Seeded `ChaCha8Rng` only
//! - **Data replay**: Pre-sorted by `(timestamp, exchange_seq)`

pub mod accounting_enforcer;
pub mod benchmark;
pub mod book;
pub mod book_recorder;
pub mod basis_signal;
pub mod clock;
// HFT-grade L2 delta model, storage, and replay
pub mod l2_delta;
pub mod l2_storage;
pub mod l2_replay;
// HFT-grade trade print recording and replay for slippage/impact attribution
pub mod trade_print;
pub mod trade_print_storage;
pub mod trade_print_attribution;
// Event-time standard for 15M strategy (three-timestamp model)
pub mod event_time;
pub mod unified_feed;
// Explicit dual-feed merge layer for 15M strategy (Binance + Polymarket alignment)
pub mod merge;
// Deterministic 15-minute window semantics (single source of truth for window boundaries)
pub mod time_windows;
// Taker slippage and fill model for realistic execution modeling
pub mod taker_slippage;
pub mod data_contract;
pub mod data_pipeline;
pub mod disclaimers;
pub mod equity_curve;
pub mod maker_validation;
// Pre-resolved market registry for hermetic backtesting
pub mod market_registry;
pub mod delta_recorder;
pub mod events;
pub mod example_strategy;
pub mod feed;
pub mod hermetic;
pub mod pre_trade_risk;
pub mod strategy_certification;
pub mod gate_suite;
pub mod integrity;
pub mod invariants;
pub mod ledger;
pub mod latency;
// First-class latency visibility model with order lifecycle scheduling for 15M Up/Down
pub mod latency_visibility;
// Trade span instrumentation for backtest (mirrors live TradeSpan model)
pub mod latency_spans;
pub mod matching;
pub mod metrics;
pub mod normalize;
pub mod oms;
pub mod oracle;
pub mod maker_fill_gate;
// Polymarket-compliant backtest execution adapter
pub mod polymarket_execution;
pub mod orchestrator;
pub mod strategy_factory;
pub mod perf;
pub mod portfolio;
pub mod production_grade;
pub mod queue;
pub mod queue_model;
pub mod risk;
pub mod sensitivity;
pub mod sim_adapter;
pub mod snapshot_sufficiency;
pub mod strategy;
pub mod settlement;
pub mod settlement_integration;
// Deterministic settlement reference replay for 15M Up/Down product
pub mod settlement_reference;
// Explicit, versioned settlement reference mapping (Binance → 15M reference price)
pub mod settlement_reference_mapping;
pub mod strict_accounting;
pub mod fingerprint;
// Cross-run reproducibility verification for HFT-grade certification
pub mod reproducibility;
pub mod trust_gate;
pub mod shadow_maker;
pub mod trade_recorder;
pub mod publication;
pub mod run_artifact;
pub mod artifact_store;
pub mod unified_recorder;
pub mod validation;
pub mod window_pnl;
pub mod visibility;
pub mod honesty;
#[cfg(test)]
mod gate_suite_tests;
#[cfg(test)]
mod integrity_tests;
#[cfg(test)]
mod invariant_tests;
#[cfg(test)]
mod ledger_tests;
#[cfg(test)]
mod oms_parity_tests;
#[cfg(test)]
mod queue_model_tests;
#[cfg(test)]
mod sensitivity_tests;
#[cfg(test)]
mod settlement_tests;
#[cfg(test)]
mod visibility_tests;
#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod accounting_tests;
#[cfg(test)]
mod maker_validation_tests;
#[cfg(test)]
mod settlement_integration_tests;
#[cfg(test)]
mod strict_accounting_tests;
#[cfg(test)]
mod production_grade_tests;
#[cfg(test)]
mod fingerprint_tests;
#[cfg(test)]
mod hermetic_tests;
#[cfg(test)]
mod trust_gate_tests;
#[cfg(test)]
mod equity_curve_tests;
#[cfg(test)]
mod window_pnl_tests;
#[cfg(test)]
mod event_time_tests;
#[cfg(test)]
mod trade_print_tests;
#[cfg(test)]
mod latency_visibility_tests;
#[cfg(test)]
mod settlement_reference_tests;

// Re-exports for convenience
pub use book::{BookManager, DeltaResult, OrderBook};
pub use book_recorder::{
    AsyncBookRecorder, BookRecorderStats, BookSnapshotStorage, PriceLevel as RecordedPriceLevel,
    RecordedBookFeed, RecordedBookSnapshot, RecorderMessage, now_ns,
};
pub use clock::{Nanos, SimClock, NANOS_PER_MILLI, NANOS_PER_SEC};
pub use data_contract::{
    ArrivalTimeSemantics, ArrivalTimeStatus, BacktestMode, DataContractValidator,
    DataQualitySummary, DatasetCapabilities, DatasetClassification, DatasetReadiness,
    DatasetReadinessClassifier, DatasetReadinessReport, ExecutionModel, HistoricalDataContract,
    MachineReadableClassification, OrderBookHistory, OrderBookStreamStatus, RunClassificationReport,
    RunGrade, StreamAvailability, StrategyCompatibility, StrategyRequirements, TradeHistory,
};
pub use events::{
    Event, EventPriority, Level, MarketStatus, OrderId, OrderType, Price, RejectReason, Resolution,
    Side, Size, TimeInForce, TimestampedEvent, TokenId,
};
pub use example_strategy::{MarketMakerStrategy, MomentumStrategy};
pub use feed::{MarketDataFeed, MarketDataFeedExt, VecFeed};
pub use gate_suite::{
    DoNothingStrategy, GateFailureReason, GateMetrics, GateMode, GateSuite, GateSuiteConfig, 
    GateSuiteReport, GateTestResult, GateTolerances, RandomTakerStrategy, SignalInverter, 
    SyntheticPriceGenerator, TrustLevel, ZeroEdgeWrapper,
};
pub use integrity::{
    DropReason, DuplicatePolicy, GapPolicy, HaltReason, HaltType, IntegrityResult,
    OutOfOrderPolicy, PathologyCounters, PathologyPolicy, StreamIntegrityGuard, SyncState,
};
pub use invariants::{
    CategoryFlags, CausalDump, EventSummary, InvariantAbort, InvariantCategory, InvariantConfig,
    InvariantCounters, InvariantEnforcer, InvariantMode, InvariantResult, InvariantViolation,
    LedgerEntrySummary, OmsTransition, ProductionGradeRequirements, StateSnapshot,
    ViolationContext, ViolationType as InvariantViolationType,
};
pub use accounting_enforcer::{
    AccountingAbort, AccountingEnforcer, AccountingEnforcerStats, AccountingStateSnapshot,
    AccountingTrigger,
};
pub use ledger::{
    AccountingMode, AccountingViolation, Amount, CausalTrace, EventRef, Ledger, LedgerAccount,
    LedgerConfig, LedgerEntry, LedgerMetadata, LedgerPosting, LedgerStats, ViolationType,
    from_amount, to_amount, AMOUNT_SCALE,
};
pub use latency::{LatencyConfig, LatencyDistribution, LatencySampler, LatencyStats};
// First-class latency visibility model with order lifecycle scheduling
pub use latency_visibility::{
    JitterCategory, LatencyVisibilityApplier, LatencyVisibilityModel, LatencyVisibilityStats,
    OrderLifecycleEvent, OrderLifecycleScheduler, ScheduledLifecycleEvent,
    ms as latency_ms, us as latency_us, sec as latency_sec,
    validate_no_negative_latency, validate_visible_monotone,
    NANOS_PER_MS as LATENCY_NANOS_PER_MS, NANOS_PER_SEC as LATENCY_NANOS_PER_SEC,
    NANOS_PER_US as LATENCY_NANOS_PER_US,
};
pub use matching::{
    CancelRequest, FeeConfig, LimitOrderBook, MatchingConfig, MatchingEngine, MatchingStats,
    OrderRequest, SelfTradeMode,
};
pub use metrics::{
    AdverseSelectionAtHorizon, AdverseSelectionMetrics, BacktestReport, FillMetrics,
    FillMetricsSummary, FillRecord, LatencyMetrics, LatencyPercentiles, LatencyTracker,
    MarketMetrics, MetricsCollector, SlippageMetrics, SlippageSummary, TailRiskMetrics,
    TailRiskSummary,
};
pub use normalize::{
    BatchNormalizer, DataNormalizer, IntegrityStats, NormalizerConfig, RawOrderBookDelta,
    RawOrderBookSnapshot, RawPriceLevel, RawResolution, RawTrade,
};
pub use oms::{
    MarketStatus as OmsMarketStatus, OmsOrder, OmsStats, OrderManagementSystem, OrderState,
    RateLimiter, TerminalReason, ValidationError, VenueConstraints,
};
pub use orchestrator::{
    BacktestConfig, BacktestOperatingMode, BacktestOrchestrator, BacktestResults, 
    MakerFillModel, ProductionGradeViolation, determine_operating_mode, format_operating_mode_banner,
};
pub use perf::{
    BenchmarkConfig, BenchmarkResult, BenchmarkRunner, EventPool, LevelPool, MarketEvent,
    MarketState, MemoryUsage, ParallelConfig, ParallelProcessor, ProcessingResult, ProfileReport,
    ProfileStage, Profiler, StringArena, TickLookup,
};
pub use portfolio::{
    MarketPosition, Outcome, Portfolio, PortfolioSummary, RiskConstraints, RiskViolation,
    TokenId as PortfolioTokenId, TokenPosition,
};
pub use queue::{EventQueue, StreamMerger, StreamSource};
pub use queue_model::{
    InFlightOrder, OurFill, QueuePosition, QueuePositionModel, QueueStats, RaceResult,
};
pub use maker_fill_gate::{
    AdmissibleFill, CancelRaceProof, CancelRaceRationale, MakerFillCandidate, MakerFillGate,
    MakerFillGateConfig, MakerFillGateStats, QueueProof, RejectionCounts, RejectionReason,
};
pub use risk::{
    BlockReason, BlockedOrder, KellyParams, KellyResult, KellySizer, RiskCheckResult, RiskLimits,
    RiskManager, RiskManagerBuilder, RiskState, RiskStats,
};
pub use sensitivity::{
    CancelLatencyAssumption, ExecutionSweepConfig, ExecutionSweepResults, FragilityDetector,
    FragilityFlags, FragilityThresholds, LatencyComponent, LatencySweepConfig,
    LatencySweepResults, QueueModelAssumption, SamplingRegime, SamplingSweepConfig,
    SamplingSweepResults, SensitivityConfig, SensitivityReport, SweepPointMetrics,
    TrustRecommendation,
};
pub use maker_validation::{
    ConservativeConfig, MakerExecutionProfile, MakerFragilityFlags, MakerProfileConfigs,
    MakerSurvivalCriteria, MakerSurvivalStatus, MakerValidationConfig, MakerValidationResult,
    MakerValidationRunner, MeasuredLiveConfig, NeutralConfig, ProfileMetrics,
};
pub use settlement::{
    OutcomeKnowableRule, ReferencePriceRule, Representativeness, RoundingRule,
    SettlementEngine, SettlementEvent, SettlementModel, SettlementOutcome, SettlementSpec,
    SettlementState, SettlementStats, TieRule, WindowStartRule,
};
pub use settlement_integration::{
    ChainlinkSettlementCoordinator, SettlementConfig, SettlementDelayStats, SettlementError,
    SettlementFingerprint, SettlementMetadata, WindowSettlementRecord,
};
// Deterministic settlement reference replay for 15M Up/Down product
pub use settlement_reference::{
    PriceFixed, ReferenceSettlementEngine, ReferenceStreamMetadata, RecordedReferenceStreamProvider,
    ReferencePriceType, RoundingRule as SettlementRefRoundingRule, SampledReference, SamplingRule,
    SettlementAuditRecord, SettlementOutcomeResult, SettlementReferenceCoverage,
    SettlementReferenceFailure, SettlementReferenceProvider, SettlementReferenceSpec,
    SettlementReferenceTick, SettlementTieRule, ReferenceSettlementStats,
    classify_settlement_coverage, NANOS_15_MIN as SETTLEMENT_REF_NANOS_15_MIN, PRICE_SCALE,
};
// Explicit, versioned settlement reference mapping (Binance → 15M reference price)
pub use settlement_reference_mapping::{
    BinanceBookUpdate, BinanceMarkPrice, BinanceTrade, FallbackReason, FallbackStep,
    InputStreamKind, MappingValidationResult, OutlierBounds, PriceKind, ReferenceTickTransformer,
    ReferenceVenue, RoundingRule as MappingRoundingRule, SettlementReferenceMapping15m,
    SettlementReferenceTick as MappingReferenceTick, StalenessConfig, SymbolMapping,
    TerminalFallbackAction, TransformationError, TransformerStats, UpDownAsset,
    compute_mid_fp, float_to_fp, fp_to_float, FP_SCALE, NS_PER_SEC as MAPPING_NS_PER_SEC,
    SETTLEMENT_REFERENCE_MAPPING_VERSION,
};
pub use snapshot_sufficiency::{
    ConservativeQueueBound, InterArrivalStats, QueueModelingCapability, SnapshotFrequencyAnalyzer,
    SnapshotSufficiencyReport, SufficiencyThresholds, TokenSnapshotAnalysis,
};
pub use trade_recorder::{
    AsyncTradeRecorder, QueueConsumptionTracker, RecordedTradePrint, TradeRecorderMessage,
    TradeRecorderStats, TradePrintFeed, TradePrintStorage,
};
// HFT-grade trade print recording for slippage/impact/adverse selection
pub use trade_print::{
    AggressorSideSource, PolymarketTradePrint, TradePrintBuilder, TradePrintDeduplicator,
    TradeIdSource, TradeSequenceTracker, TradeStreamMetadata, TradePrintError,
    PRICE_SCALE as TRADE_PRINT_PRICE_SCALE, SIZE_SCALE as TRADE_PRINT_SIZE_SCALE,
    DEFAULT_TICK_SIZE as TRADE_PRINT_DEFAULT_TICK_SIZE,
};
pub use trade_print_storage::{
    TradePrintFullStorage, TradePrintRecordingStats, TradePrintReplayFeed,
};
pub use trade_print_attribution::{
    AttributionConfig, AttributionEngine, AttributionStats, FillAttributionReport,
    MidMoveAtHorizon, MidMoveMetrics, MidPriceTracker, NearbyPrintContext, TradePrintBuffer,
};
pub use unified_recorder::{
    RecorderIntegrity, RecorderStats, UnifiedRecorder, UnifiedRecorderConfig, UnifiedReplayFeed,
    UnifiedStorage,
};
pub use data_pipeline::{
    BackfillConfig, DatasetStore, DatasetTrustLevel, DatasetVersion, 
    DuplicatePolicy as PipelineDuplicatePolicy, FieldDefinition, FieldType, GapInfo, 
    GapPolicy as PipelineGapPolicy, IntegrityIssue, IntegrityReport, IntegrityStatus, 
    IssueSeverity, LiveRecorder, LiveRecorderConfig, NightlyBackfill, 
    OutOfOrderPolicy as PipelineOutOfOrderPolicy, RawDataStream, RawEventRecord, RawPayload, 
    RecorderStats as PipelineRecorderStats, ReplayValidation, ReplayValidationConfig, 
    ReplayValidationResult, SequenceSemantics, StreamSchema, TimeRange, classify_dataset_trust,
};
pub use oracle::{
    BasisDiagnostics, BasisStats, ChainlinkFeedConfig, ChainlinkIngestor, ChainlinkReplayFeed,
    ChainlinkRound, ChainlinkSettlementSource, OraclePricePoint, OracleRoundStorage,
    OracleStorageConfig, SettlementReferenceRule, SettlementReferenceSource, WindowBasis,
};
pub use sim_adapter::{OmsParityMode, OmsParityStats, SimulatedOrderSender};
pub use strategy::{
    BookSnapshot, CancelAck, FillNotification, OpenOrder, OrderAck, OrderReject, OrderSender,
    Position, Strategy, StrategyCancel, StrategyContext, StrategyFactory, StrategyOrder,
    StrategyParams, TimerEvent, TradePrint,
};
pub use validation::{
    Checkpoint, DeterministicSeed, EventTracer, InvariantChecker, InvariantSummary,
    InvariantViolation as LegacyInvariantViolation, OrderTraceEvent, ReplayTestCase,
    ReplayTestResult, ReplayTestRunner, ReproducibilityValidator, StateFingerprint,
    StrategyAction, TracedEvent, ValidationHarness, ValidationHarnessSummary, ValidationResult,
};
pub use visibility::{
    disable_strict_mode, enable_strict_mode, is_strict_mode, ArrivalTimeMapper, DecisionProof,
    DecisionProofBuffer, InputEventRecord, SimArrivalPolicy, VisibilityViolation,
    VisibilityWatermark,
};
// Event-time standard (three-timestamp model for 15M strategy)
pub use event_time::{
    BacktestLatencyModel, BookSide, EventTime, EventTimeError, FeedEvent, FeedEventPayload,
    FeedEventPriority, FeedSource, IngestTimestampQuality, LatencyApplierStats,
    LatencyModelApplier, MarketStatus as EventTimeMarketStatus, PriceLevel as EventTimePriceLevel,
    ResolutionOutcome as EventTimeResolutionOutcome, VisibleNanos, Window15M,
    NS_PER_MS as EVENT_TIME_NS_PER_MS, NS_PER_SEC as EVENT_TIME_NS_PER_SEC,
    NS_PER_US as EVENT_TIME_NS_PER_US, NANOS_15_MIN,
    check_no_negative_delay, check_visible_monotone,
};
pub use unified_feed::{
    StrategyEventView, UnifiedFeedQueue, UnifiedFeedQueueStats, VisibleTimeContext,
};
// Explicit dual-feed merge layer for 15M strategy
pub use merge::{
    BinanceAdapter, BinanceRawRecord, EventLogEntry, FeedAdapter, FeedMerger, FeedMergerConfig,
    FeedMergerStats, OmsAdapter, OrderingKey, PolymarketAdapter, PolymarketEventType,
    PolymarketRawRecord, PriorityClass, RunFingerprint as MergeRunFingerprint, SourceId,
    TimerAdapter, ordering_key,
};
// Deterministic 15-minute window semantics (single source of truth)
pub use time_windows::{
    PStartConfig, StartPriceState, WindowBounds, WindowContext, WindowState,
    WINDOW_DURATION_NS, WINDOW_DURATION_SECS, NS_PER_MIN, NS_PER_SEC as TIME_WINDOWS_NS_PER_SEC,
    align_to_window_end, align_to_window_start as tw_align_to_window_start,
    is_different_window, is_later_window, remaining_time_ns, remaining_time_secs,
    window_bounds_15m, window_bounds_from_visible, window_end_from_index, window_index,
    window_start_from_index,
};
pub use fingerprint::{
    BehaviorEvent, BehaviorFingerprint, BehaviorFingerprintBuilder, CodeFingerprint,
    ConfigFingerprint, DatasetFingerprint, FingerprintCollector, RunFingerprint, 
    SeedFingerprint, StreamFingerprint, StreamFingerprintBuilder, StrategyFingerprint,
    StrategyId, FINGERPRINT_VERSION,
};
// Cross-run reproducibility verification for HFT-grade certification
pub use reproducibility::{
    CanonicalCancelAckRecord, CanonicalCancelRecord, CanonicalEncoder, CanonicalFillRecord,
    CanonicalFinalPnLRecord, CanonicalLedgerRecord, CanonicalOrderAckRecord,
    CanonicalOrderRejectRecord, CanonicalOrderRecord, MarketIdRegistry, RecordTag,
    ReplayMismatch, ReplayVerificationResult, ReproducibilityCollector, ReproducibilityFailure,
    ReproducibilityFingerprint, ReproducibilityMode, ReproducibilityStats, RollingHash,
    StreamCollector, StreamFingerprint as ReprodStreamFingerprint,
    fee_to_fixed, price_to_ticks as reprod_price_to_ticks, size_to_shares,
    AccountTypeCode, EventRefTypeCode, OrderTypeCode, RejectReasonCode, SideCode,
    FEE_SCALE, PRICE_SCALE as REPROD_PRICE_SCALE, SIZE_SCALE,
};
pub use hermetic::{
    CallbackType, DecisionAction, HermeticAbort, HermeticClock, HermeticConfig,
    HermeticDecisionProof, HermeticDecisionProofBuilder, HermeticEnforcer, HermeticRng,
    HermeticStrategy, HermeticViolation, HermeticViolationType, InputEventId,
    disable_hermetic_mode, enable_hermetic_mode, guard_async_spawn, guard_env_access,
    guard_filesystem_io, guard_network_io, guard_thread_spawn, guard_wall_clock,
    hermetic_guard, is_hermetic_mode, FORBIDDEN_API_PATTERNS,
};
pub use benchmark::{
    BenchmarkResults, BenchmarkScenario, BenchmarkStage, BenchmarkSuiteConfig,
    BenchmarkSuiteReport, DeterminismResult, GrowthReport, MemoryProfiler, MemorySample,
    MemoryStats, PerformanceTargets, ScenarioSize, StageBreakdown, StageProfiler, StageTiming,
    SuiteSummary, SyntheticDataGenerator, TargetComparison,
};
pub use shadow_maker::{
    BookSnapshotContext, DataIntegrityContext, DiscrepancyClass, FailureMode, OrderDiscrepancy,
    QueueModelThresholds, QueueModelTrustLevel, QueueModelValidationFlag,
    QueueModelValidationReport, ShadowMakerPreconditions, ShadowMakerValidator, ShadowOrder,
    ShadowOrderTerminalReason, ShadowPrediction, ShadowValidationStats, UnsupportedRegime,
};
pub use trust_gate::{
    TrustDecision, TrustFailureReason, TrustGate, TrustGateConfig, TrustGateError,
};
pub use strategy_factory::{available_strategies, make_strategy};
pub use window_pnl::{
    WindowAccountingEngine, WindowAccountingError, WindowId, WindowPnL, WindowPnLSeries,
    align_to_window_start, parse_window_start_from_slug,
};
pub use equity_curve::{
    EquityCurve, EquityCurveSummary, EquityObservationTrigger, EquityPoint, EquityRecorder,
    EquityRecorderStats,
};
pub use honesty::{
    DistributionStats, HonestyMetrics, HonestyMetricsError, PerWindowValue, RatioValue,
    RATIO_SCALE,
};
pub use disclaimers::{
    Category as DisclaimerCategory, Disclaimer, DisclaimerContext, DisclaimersBlock,
    Severity as DisclaimerSeverity, TrustLevelSnapshot, generate_disclaimers,
};
pub use run_artifact::{
    ArtifactResponse, BinningConfig, BinningMethod, ConfigSummary, DatasetMetadata, 
    DatasetReadinessDto, DistributionBin, DrawdownPoint, EquityPoint as ArtifactEquityPoint, 
    HistogramBin, ListRunsFilter, ListRunsResponse, MethodologyCapsule, ProvenanceBlock, 
    RunArtifact, RunDistributions, RunId, RunManifest, RunSortField, RunSummary, 
    RunTimeSeries, SortOrder, StrategyIdDto, StrategyIdentity, TimeRangeSummary, 
    TrustDecisionSummary, TrustLevelDto, TrustStatus, WindowPnLPoint, WindowPnlHistogramResponse, 
    RUN_ARTIFACT_API_VERSION, RUN_ARTIFACT_STORAGE_VERSION, WINDOW_PNL_HISTOGRAM_SCHEMA_VERSION,
};
pub use artifact_store::{ArtifactStore, ArtifactStoreError, ArtifactStoreStats};
pub use publication::{
    PublicationDecision, PublicationError, PublicationGate, PublicationGateError,
    PublicationStatus,
};
// HFT-grade L2 delta model, storage, and replay
pub use l2_delta::{
    BookError, BookFingerprint, DeterministicBook, EventTime as L2EventTime, 
    GapPolicy as L2GapPolicy, L2DatasetMetadata, L2DeltaContractRequirement, 
    L2DeltaContractResult, PolymarketL2Delta, PolymarketL2Snapshot, SequenceOrigin, 
    SequenceScope, TickPriceLevel, POLYMARKET_TICK_SIZE, price_to_ticks, ticks_to_price,
};
pub use l2_storage::{
    AsyncL2Recorder, L2RecorderMessage, L2Storage, L2StorageStats,
};
pub use l2_replay::{
    L2BookManager, L2ContractVerifier, L2DatasetClassification, L2Event, L2ReplayFeed,
    L2TrustGateExt,
};
// Pre-resolved market registry for hermetic backtesting
pub use market_registry::{
    ExtractedRegistryParams, FeeSchedule, MarketFlags, MarketKey, MarketMeta, MarketRegistry,
    MarketRegistryError, RegistryDatasetValidation, RegistryFingerprint, RegistryHandle,
    SettlementRule, TokenIds, extract_registry_params, inject_registry_params,
    make_registry_handle, strategy_param_keys, REGISTRY_VERSION,
    TRUST_FAILURE_DATASET_INCOMPATIBLE, TRUST_FAILURE_INVALID_REGISTRY, TRUST_FAILURE_MISSING_REGISTRY,
};
// Taker slippage and fill model for realistic execution modeling
pub use taker_slippage::{
    compare_execution_models, price_to_tick, tick_to_price,
    ExecutionMetrics, ExecutionOutcome, LevelFill, LiquiditySource, PriceTick,
    SimulatedL2Book, SimulatedPriceLevel, SlippageComparison, TakerFillModel, TakerFillResult,
    TakerFillStats, TakerOrderRequest, TakerSlippageConfig,
    DEFAULT_TICK_SIZE as TAKER_DEFAULT_TICK_SIZE, MAX_PRICE as TAKER_MAX_PRICE,
    MIN_ORDER_SIZE as TAKER_MIN_ORDER_SIZE, MIN_PRICE as TAKER_MIN_PRICE,
};
// Trade span instrumentation (mirrors live TradeSpan model)
pub use latency_spans::{
    DecisionSpan, EventKind, LatencyPercentiles as SpanLatencyPercentiles, LatencySummary, OrderSpan,
    SpanArtifact, SpanCollector, SpanCollectorStats, SPAN_SCHEMA_VERSION,
};

#[cfg(test)]
mod disclaimers_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exports() {
        let clock = SimClock::new(0);
        assert_eq!(clock.now(), 0);

        let mut queue = EventQueue::new();
        assert!(queue.is_empty());
    }
}
