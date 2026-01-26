/**
 * Certified Backtest Results Types
 * 
 * These types represent the read-only, immutable results of a certified backtest run.
 * The UI consumes these directly from backend endpoints without recomputation.
 */

// Trust level from backend TrustGate evaluation
export type TrustLevel = 'Trusted' | 'Untrusted' | 'Unknown' | 'Bypassed';

// Disclaimer severity
export type DisclaimerSeverity = 'Info' | 'Warning' | 'Critical';

// Disclaimer category
export type DisclaimerCategory =
  | 'ProductionMode'
  | 'DatasetReadiness'
  | 'MakerValidity'
  | 'GateSuite'
  | 'Sensitivity'
  | 'SettlementReference'
  | 'IntegrityPolicy'
  | 'Reproducibility'
  | 'DataCoverage';

// A single disclaimer from the backend
export interface Disclaimer {
  id: string;
  severity: DisclaimerSeverity;
  category: DisclaimerCategory;
  message: string;
  evidence: string[];
}

// Disclaimers block from backend
export interface DisclaimersBlock {
  generated_at_ns: number;
  trust_level: TrustLevel;
  disclaimers: Disclaimer[];
}

// Equity curve point
export interface EquityPoint {
  time_ns: number;
  equity_value: number;
  cash_balance: number;
  position_value: number;
  drawdown_value: number;
  drawdown_bps: number;
}

// Per-window PnL record
export interface WindowPnL {
  window_start_ns: number;
  window_end_ns: number;
  market_id: string;
  gross_pnl: number;
  fees: number;
  settlement_transfer: number;
  net_pnl: number;
  trades_count: number;
  maker_fills_count: number;
  taker_fills_count: number;
  total_volume: number;
  start_price?: number;
  end_price?: number;
  outcome?: string;
  is_finalized: boolean;
}

// Window PnL series
export interface WindowPnLSeries {
  windows: WindowPnL[];
  total_net_pnl: number;
  total_gross_pnl: number;
  total_fees: number;
  total_settlement: number;
  total_trades: number;
  finalized_count: number;
  active_windows: number;
  series_hash: number;
}

// Strategy identification
export interface StrategyId {
  name: string;
  version: string;
  code_hash?: string;
}

// Dataset provenance
export interface DatasetProvenance {
  classification: string;
  readiness: string;
  orderbook_type: string;
  trade_type: string;
  arrival_semantics: string;
}

// Run fingerprint for reproducibility
export interface RunFingerprint {
  version: string;
  hash_hex: string;
  strategy_name?: string;
  strategy_version?: string;
  dataset_hash?: number;
  seed?: number;
}

// Summary metrics for the compact summary panel
export interface CertifiedSummary {
  net_pnl: number;
  gross_pnl: number;
  total_fees: number;
  max_drawdown: number;
  max_drawdown_pct: number;
  win_rate: number;
  windows_traded: number;
  total_windows: number;
  sharpe_ratio?: number;
  profit_factor?: number;
}

// Provenance summary for advanced disclosure
export interface ProvenanceSummary {
  strategy_id?: StrategyId;
  dataset: DatasetProvenance;
  run_fingerprint?: RunFingerprint;
  settlement_source: string;
  operating_mode: string;
  data_range_start_ns?: number;
  data_range_end_ns?: number;
}

// The main certified backtest results response
export interface CertifiedBacktestResults {
  // Run identification
  run_id: string;
  fetched_at: number;
  
  // Trust status
  trust_level: TrustLevel;
  disclaimers?: DisclaimersBlock;
  
  // Core metrics (compact summary)
  summary: CertifiedSummary;
  
  // Time-indexed series for charting
  equity_curve: EquityPoint[];
  window_pnl: WindowPnLSeries;
  
  // Provenance (for advanced disclosure)
  provenance: ProvenanceSummary;
  
  // Download URLs (optional)
  manifest_url?: string;
  csv_export_url?: string;
}

// Histogram bin for PnL distribution
export interface PnLHistogramBin {
  bin_start: number;
  bin_end: number;
  count: number;
  sum_pnl: number;
}

// API query params for fetching results
export interface CertifiedBacktestQuery {
  run_id: string;
}

// =============================================================================
// PERMALINKED RUN TYPES (for /runs/{run_id} routes)
// =============================================================================

// Run summary response from GET /api/runs/{run_id}
export interface RunSummaryResponse {
  run_id: string;
  manifest_hash: string;
  trust_level: TrustLevel;
  disclaimers?: DisclaimersBlock;
  strategy_id?: StrategyId;
  dataset_version: string;
  dataset_readiness: string;
  settlement_source: string;
  settlement_rule: string;
  operating_mode: string;
  data_range_start_ns?: number;
  data_range_end_ns?: number;
  summary: CertifiedSummary;
  created_at: number;
  // Provenance fields for CertifiedRunFooter (mandatory)
  schema_version: string;
  publish_timestamp: number; // Unix timestamp in seconds (UTC)
}

// Equity series response from GET /api/runs/{run_id}/series/equity
export interface RunEquitySeriesResponse {
  run_id: string;
  manifest_hash: string;
  points: EquityPoint[];
}

// Drawdown series response from GET /api/runs/{run_id}/series/drawdown
export interface RunDrawdownSeriesResponse {
  run_id: string;
  manifest_hash: string;
  points: Array<{
    time_ns: number;
    drawdown_value: number;
    drawdown_bps: number;
  }>;
}

// Window PnL series response from GET /api/runs/{run_id}/series/window_pnl
export interface RunWindowPnLSeriesResponse {
  run_id: string;
  manifest_hash: string;
  series: WindowPnLSeries;
}

// Window PnL distribution response from GET /api/runs/{run_id}/distribution/window_pnl
export interface RunWindowPnLDistributionResponse {
  run_id: string;
  manifest_hash: string;
  bins: PnLHistogramBin[];
  total_windows: number;
  winning_windows: number;
  losing_windows: number;
}

// Full manifest response from GET /api/runs/{run_id}/manifest
export interface RunManifestResponse {
  run_id: string;
  manifest_hash: string;
  version: string;
  generated_at_ns: number;
  strategy: {
    name: string;
    version: string;
    code_hash: string;
  };
  config: {
    hash: number;
    settlement_rule: string;
    arrival_policy: string;
    maker_fill_model: string;
    strict_accounting: boolean;
    production_grade: boolean;
  };
  dataset: {
    hash: number;
    classification: string;
    readiness: string;
    orderbook_type: string;
  };
  seed: {
    primary_seed: number;
    hash: number;
  };
  behavior: {
    event_count: number;
    hash: number;
  };
  trust_decision: {
    trust_level: TrustLevel;
    reasons?: string[];
  };
}

// Aggregated run data for the RunPage (all endpoints combined)
export interface AggregatedRunData {
  summary: RunSummaryResponse;
  equity: RunEquitySeriesResponse;
  drawdown: RunDrawdownSeriesResponse;
  windowPnL: RunWindowPnLSeriesResponse;
  distribution: RunWindowPnLDistributionResponse;
}

// Integrity error when manifest hashes don't match
export interface ManifestIntegrityError {
  type: 'manifest_mismatch';
  runId: string;
  expectedHash: string;
  actualHashes: Record<string, string>;
  message: string;
}

// =============================================================================
// CERTIFIED WINDOW PNL HISTOGRAM (DETERMINISTIC)
// =============================================================================

/** Schema version for histogram responses - used for forwards compatibility */
export const HISTOGRAM_SCHEMA_VERSION = 'v1' as const;

/** Binning method used by the backend */
export type BinningMethod = 'fixed_edges' | 'backend_v1';

/** Binning configuration for the histogram */
export interface BinningConfig {
  /** Method used for binning */
  method: BinningMethod;
  /** Number of bins */
  bin_count: number;
  /** Minimum value (left edge of first bin) */
  min: number;
  /** Maximum value (right edge of last bin) */
  max: number;
}

/** A single histogram bin with explicit edges (backend-computed) */
export interface CertifiedHistogramBin {
  /** Left edge of the bin (inclusive) */
  left: number;
  /** Right edge of the bin (exclusive, except for last bin) */
  right: number;
  /** Number of samples in this bin */
  count: number;
}

/**
 * Response for GET /api/runs/{run_id}/distribution/window_pnl
 * 
 * This response contains deterministic, backend-computed histogram bins.
 * The frontend MUST NOT recompute bins from raw samples for certified views.
 */
export interface WindowPnlHistogramResponse {
  /** Schema version for forwards compatibility */
  schema_version: string;
  /** Run identifier */
  run_id: string;
  /** Manifest hash for verification (used for provenance display) */
  manifest_hash: string;
  /** Unit of the PnL values (e.g., "USD") */
  unit: string;
  /** Binning configuration */
  binning: BinningConfig;
  /** Histogram bins (backend-computed, deterministic) */
  bins: CertifiedHistogramBin[];
  /** Count of samples below the minimum bin edge */
  underflow_count: number;
  /** Count of samples above the maximum bin edge */
  overflow_count: number;
  /** Total number of windows included */
  total_samples: number;
  /** Trust level of the run */
  trust_level: TrustLevel;
  /** Whether the run is trusted */
  is_trusted: boolean;
}

/**
 * Validate a histogram response from the backend.
 * Returns null if valid, or an error message if invalid.
 */
export function validateHistogramResponse(
  response: WindowPnlHistogramResponse
): string | null {
  // Check schema version
  if (response.schema_version !== HISTOGRAM_SCHEMA_VERSION) {
    return `Unsupported schema version: ${response.schema_version} (expected ${HISTOGRAM_SCHEMA_VERSION})`;
  }

  // Check bins array exists and matches bin_count
  if (!Array.isArray(response.bins)) {
    return 'Missing bins array';
  }

  if (response.bins.length !== response.binning.bin_count) {
    return `Bin count mismatch: ${response.bins.length} bins but binning.bin_count = ${response.binning.bin_count}`;
  }

  // Check bins are contiguous
  for (let i = 0; i < response.bins.length - 1; i++) {
    const current = response.bins[i];
    const next = response.bins[i + 1];
    if (Math.abs(current.right - next.left) > 1e-10) {
      return `Bins are not contiguous at index ${i}: ${current.right} != ${next.left}`;
    }
  }

  // Check total count
  const binSum = response.bins.reduce((sum, b) => sum + b.count, 0);
  const expectedTotal = binSum + response.underflow_count + response.overflow_count;
  if (expectedTotal !== response.total_samples) {
    return `Count mismatch: bins(${binSum}) + underflow(${response.underflow_count}) + overflow(${response.overflow_count}) = ${expectedTotal} != total_samples(${response.total_samples})`;
  }

  // Check manifest_hash is present
  if (!response.manifest_hash || response.manifest_hash.length === 0) {
    return 'Missing manifest_hash for provenance verification';
  }

  return null;
}
