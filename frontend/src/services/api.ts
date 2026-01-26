/**
 * REST API Client
 * 
 * Optimizations:
 * - Cached token to avoid repeated localStorage reads
 * - Connection reuse via keep-alive
 * - Minimal header construction
 */

import {
  Signal,
  SignalStats,
  CompositeSignal,
  MarketSnapshotResponse,
  SignalEnrichResponse,
  WalletAnalyticsResponse,
  WalletAnalyticsPrimeResponse,
} from '../types/signal';
import type {
  CertifiedBacktestResults,
  RunSummaryResponse,
  RunEquitySeriesResponse,
  RunDrawdownSeriesResponse,
  RunWindowPnLSeriesResponse,
  RunWindowPnLDistributionResponse,
  RunManifestResponse,
  AggregatedRunData,
  ManifestIntegrityError,
  WindowPnlHistogramResponse,
} from '../types/backtest';
import { validateHistogramResponse } from '../types/backtest';
import {
  VaultDepositRequest,
  VaultDepositResponse,
  VaultActivityResponse,
  VaultConfigResponse,
  VaultLlmDecisionsResponse,
  VaultLlmModelsResponse,
  VaultOverviewResponse,
  VaultPerformanceResponse,
  VaultPositionsResponse,
  VaultStateResponse,
  VaultWithdrawRequest,
  VaultWithdrawResponse,
} from '../types/vault';
import { LoginRequest, LoginResponse, PrivyLoginRequest } from '../types/auth';
import { TradeOrderRequest, TradeOrderResponse } from '../types/trade';

const API_URL = import.meta.env.VITE_API_URL || '';
const TOKEN_KEY = 'betterbot_token';

class ApiClient {
  private token: string | null = null;
  private cachedHeaders: Record<string, string> | null = null;

  setToken(token: string | null) {
    this.token = token;
    this.cachedHeaders = null; // Invalidate header cache
    if (token) {
      localStorage.setItem(TOKEN_KEY, token);
    } else {
      localStorage.removeItem(TOKEN_KEY);
    }
  }

  getToken(): string | null {
    if (this.token === null) {
      this.token = localStorage.getItem(TOKEN_KEY);
    }
    return this.token;
  }

  private getHeaders(): Record<string, string> {
    if (this.cachedHeaders) return this.cachedHeaders;
    
    const token = this.getToken();
    this.cachedHeaders = {
      'Content-Type': 'application/json',
      ...(token ? { 'Authorization': `Bearer ${token}` } : {}),
    };
    return this.cachedHeaders;
  }

  private async fetch<T>(
    endpoint: string,
    options: RequestInit = {},
    timeoutMs: number = 15_000
  ): Promise<T> {
    const controller = new AbortController();
    let timedOut = false;
    const t = window.setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);

    const externalSignal = options.signal;
    if (externalSignal) {
      if (externalSignal.aborted) {
        controller.abort();
      } else {
        externalSignal.addEventListener('abort', () => controller.abort(), { once: true });
      }
    }

    const { signal: _ignoredSignal, ...rest } = options;

    try {
      const response = await fetch(`${API_URL}${endpoint}`, {
        ...rest,
        signal: controller.signal,
        headers: {
          ...this.getHeaders(),
          ...((rest.headers as Record<string, string>) || {}),
        },
      });

      if (!response.ok) {
        let bodyText = '';
        try {
          bodyText = await response.text();
        } catch {
          // ignore
        }

        const message = `API Error: ${response.status} ${response.statusText}${
          bodyText ? ` - ${bodyText}` : ''
        }`;
        const err: any = new Error(message);
        err.status = response.status;
        err.body = bodyText;
        throw err;
      }

      if (response.status === 204) {
        return null as unknown as T;
      }

      return response.json();
    } catch (err: any) {
      if (err?.name === 'AbortError') {
        if (timedOut) {
          throw new Error('Request timed out');
        }
        const e: any = new Error('Request aborted');
        e.aborted = true;
        throw e;
      }
      throw err;
    } finally {
      window.clearTimeout(t);
    }
  }

  // Auth endpoints
  async login(credentials: LoginRequest): Promise<LoginResponse> {
    const response = await this.fetch<LoginResponse>('/api/auth/login', {
      method: 'POST',
      body: JSON.stringify(credentials),
    });
    this.setToken(response.token);
    return response;
  }

  async loginWithPrivy(request: PrivyLoginRequest): Promise<LoginResponse> {
    const response = await this.fetch<LoginResponse>('/api/auth/privy', {
      method: 'POST',
      body: JSON.stringify(request),
    });
    this.setToken(response.token);
    return response;
  }

  async logout() {
    this.setToken(null);
  }

  async getCurrentUser(): Promise<LoginResponse> {
    return this.fetch<LoginResponse>('/api/auth/me');
  }

  // Signal endpoints
  async getSignals(params?: {
    limit?: number;
    min_confidence?: number;
    before?: string;
    before_id?: string;
    exclude_updown?: boolean;
  }): Promise<{ signals: Signal[]; count: number; timestamp: string }> {
    const query = new URLSearchParams();
    if (params?.limit) query.set('limit', params.limit.toString());
    if (params?.min_confidence) query.set('min_confidence', params.min_confidence.toString());
    if (params?.before) query.set('before', params.before);
    if (params?.before_id) query.set('before_id', params.before_id);
    if (params?.exclude_updown) query.set('exclude_updown', 'true');
    
    const queryString = query.toString();
    const endpoint = `/api/signals${queryString ? `?${queryString}` : ''}`;
    
    return this.fetch<{ signals: Signal[]; count: number; timestamp: string }>(endpoint);
  }

  async searchSignals(
    params: {
    q: string;
    limit?: number;
    before?: string;
    before_id?: string;
    exclude_updown?: boolean;
    min_confidence?: number;
    full_context?: boolean;
  },
    options?: { signal?: AbortSignal }
  ): Promise<{ signals: Signal[]; count: number; timestamp: string }> {
    const query = new URLSearchParams();
    query.set('q', params.q);
    if (params.limit) query.set('limit', params.limit.toString());
    if (params.before) query.set('before', params.before);
    if (params.before_id) query.set('before_id', params.before_id);
    if (params.exclude_updown) query.set('exclude_updown', 'true');
    if (params.min_confidence !== undefined)
      query.set('min_confidence', params.min_confidence.toString());
    if (params.full_context) query.set('full_context', 'true');

    return this.fetch<{ signals: Signal[]; count: number; timestamp: string }>(
      `/api/signals/search?${query.toString()}`,
      { signal: options?.signal }
    );
  }

  async searchSignalsStatus(options?: { signal?: AbortSignal }): Promise<{
    schema_ready: boolean;
    backfill_done: boolean;
    total_signals: number;
    indexed_rows: number;
    cursor_detected_at?: string | null;
    cursor_id?: string | null;
    timestamp: string;
  }> {
    return this.fetch(
      '/api/signals/search/status',
      { signal: options?.signal },
      2_500
    );
  }

  async getSignal(id: string): Promise<Signal> {
    return this.fetch<Signal>(`/api/signals/${id}`);
  }

  async getMarketSignals(slug: string): Promise<{ signals: Signal[]; total: number }> {
    return this.fetch<{ signals: Signal[]; total: number }>(`/api/signals/market/${slug}`);
  }

  async getCompositeSignals(): Promise<{
    composite_signals: CompositeSignal[];
    count: number;
    scan_time: string;
  }> {
    return this.fetch(`/api/signals/composite`);
  }

  async getSignalStats(): Promise<SignalStats> {
    return this.fetch<SignalStats>('/api/signals/stats');
  }

  async getSignalEnrich(
    signalId: string,
    levels: number = 10,
    fresh: boolean = false
  ): Promise<SignalEnrichResponse> {
    const query = new URLSearchParams({
      signal_id: signalId,
      levels: String(levels),
      fresh: fresh ? 'true' : 'false',
    });
    return this.fetch<SignalEnrichResponse>(`/api/signals/enrich?${query.toString()}`, {}, 2_000);
  }

  // Risk endpoints
  async getRiskStats(): Promise<any> {
    return this.fetch('/api/risk/stats');
  }

  // Market microstructure
  async getMarketSnapshot(tokenId: string, levels: number = 10): Promise<MarketSnapshotResponse> {
    const query = new URLSearchParams({ token_id: tokenId, levels: String(levels) });
    return this.fetch<MarketSnapshotResponse>(`/api/market/snapshot?${query.toString()}`, {}, 2_000);
  }

  async getMarketSnapshotBySlug(
    marketSlug: string,
    outcome: string,
    levels: number = 10
  ): Promise<MarketSnapshotResponse> {
    const query = new URLSearchParams({
      market_slug: marketSlug,
      outcome,
      levels: String(levels),
    });
    return this.fetch<MarketSnapshotResponse>(`/api/market/snapshot?${query.toString()}`, {}, 2_000);
  }

  async getWalletAnalytics(
    walletAddress: string,
    force: boolean = false,
    frictionMode: 'optimistic' | 'base' | 'pessimistic' = 'base',
    copyModel: 'scaled' | 'mtm' = 'scaled',
    cachedOnly: boolean = false
  ): Promise<WalletAnalyticsResponse> {
    const query = new URLSearchParams({
      wallet_address: walletAddress,
      force: String(force),
      friction_mode: frictionMode,
      copy_model: copyModel,
      cached_only: String(cachedOnly),
    });
    const timeoutMs = cachedOnly ? 2_000 : copyModel === 'mtm' ? 20_000 : 10_000;
    return this.fetch<WalletAnalyticsResponse>(`/api/wallet/analytics?${query.toString()}`, {}, timeoutMs);
  }

  async primeWalletAnalytics(
    wallets: string[],
    force: boolean = false,
    frictionMode: 'optimistic' | 'base' | 'pessimistic' = 'base',
    copyModel: 'scaled' | 'mtm' = 'scaled'
  ): Promise<WalletAnalyticsPrimeResponse> {
    return this.fetch<WalletAnalyticsPrimeResponse>(
      '/api/wallet/analytics/prime',
      {
        method: 'POST',
        body: JSON.stringify({
          wallets,
          force,
          friction_mode: frictionMode,
          copy_model: copyModel,
        }),
      },
      5_000
    );
  }

  // Vault endpoints (accounting-only; on-chain settlement TBD)
  async getVaultState(): Promise<VaultStateResponse> {
    return this.fetch<VaultStateResponse>('/api/vault/state');
  }

  async getVaultOverview(wallet?: string): Promise<VaultOverviewResponse> {
    const query = new URLSearchParams();
    if (wallet) query.set('wallet', wallet);
    const qs = query.toString();
    return this.fetch<VaultOverviewResponse>(`/api/vault/overview${qs ? `?${qs}` : ''}`);
  }

  async getVaultPerformance(range: '24h' | '7d' | '30d' | '90d' | 'all' = '7d'): Promise<VaultPerformanceResponse> {
    const query = new URLSearchParams({ range });
    return this.fetch<VaultPerformanceResponse>(`/api/vault/performance?${query.toString()}`);
  }

  async getVaultPositions(): Promise<VaultPositionsResponse> {
    return this.fetch<VaultPositionsResponse>('/api/vault/positions', {}, 5_000);
  }

  async getVaultActivity(limit: number = 200, wallet?: string): Promise<VaultActivityResponse> {
    const query = new URLSearchParams({ limit: String(limit) });
    if (wallet) query.set('wallet', wallet);
    return this.fetch<VaultActivityResponse>(`/api/vault/activity?${query.toString()}`, {}, 5_000);
  }

  async getVaultConfig(): Promise<VaultConfigResponse> {
    return this.fetch<VaultConfigResponse>('/api/vault/config');
  }

  async getVaultLlmDecisions(limit: number = 200, marketSlug?: string): Promise<VaultLlmDecisionsResponse> {
    const query = new URLSearchParams({ limit: String(limit) });
    if (marketSlug) query.set('market_slug', marketSlug);
    return this.fetch<VaultLlmDecisionsResponse>(`/api/vault/llm/decisions?${query.toString()}`, {}, 5_000);
  }

  async getVaultLlmModels(decisionId: string, limit: number = 20): Promise<VaultLlmModelsResponse> {
    const query = new URLSearchParams({ decision_id: decisionId, limit: String(limit) });
    return this.fetch<VaultLlmModelsResponse>(`/api/vault/llm/models?${query.toString()}`, {}, 5_000);
  }

  async vaultDeposit(request: VaultDepositRequest): Promise<VaultDepositResponse> {
    return this.fetch<VaultDepositResponse>(
      '/api/vault/deposit',
      {
        method: 'POST',
        body: JSON.stringify(request),
      },
      5_000
    );
  }

  async vaultWithdraw(request: VaultWithdrawRequest): Promise<VaultWithdrawResponse> {
    return this.fetch<VaultWithdrawResponse>(
      '/api/vault/withdraw',
      {
        method: 'POST',
        body: JSON.stringify(request),
      },
      5_000
    );
  }

  // Trading (feature-flagged server-side)
  async placeTradeOrder(request: TradeOrderRequest): Promise<TradeOrderResponse> {
    return this.fetch<TradeOrderResponse>(
      '/api/trade/order',
      {
        method: 'POST',
        body: JSON.stringify(request),
      },
      2_500
    );
  }

  // Health endpoint
  async getHealth(): Promise<{ status: string; timestamp: string }> {
    return this.fetch('/health');
  }

  // Performance dashboard
  async getPerformanceDashboard(): Promise<PerformanceDashboardResponse> {
    return this.fetch('/api/performance/dashboard', {}, 5_000);
  }

  // Trigger load test
  async runLoadTest(events?: number, intervalMs?: number): Promise<{ status: string; events: number; interval_ms: number }> {
    return this.fetch('/api/performance/load-test', {
      method: 'POST',
      body: JSON.stringify({ events, interval_ms: intervalMs }),
    }, 5_000);
  }

  // 15M Arbitrage monitoring
  async getArbitrage15m(asset?: string): Promise<Arb15mResponse> {
    const params = asset ? `?asset=${asset}` : '';
    return this.fetch(`/api/arbitrage/15m${params}`, {}, 5_000);
  }

  // Oracle Comparison (Chainlink vs Binance)
  async getOracleComparison(asset?: string): Promise<OracleComparisonResponse> {
    const params = asset ? `?asset=${asset}` : '';
    return this.fetch(`/api/oracle/comparison${params}`, {}, 10_000);
  }

  // Backtest (interactive config-and-run)
  async getBacktestResults(params: {
    asset?: string;
    bankroll?: number;
    min_edge?: number;
    kelly_fraction?: number;
    max_position_pct?: number;
    fee_rate?: number;
  } = {}): Promise<BacktestResults> {
    const query = new URLSearchParams();
    if (params.asset) query.set('asset', params.asset);
    if (params.bankroll) query.set('bankroll', String(params.bankroll));
    if (params.min_edge) query.set('min_edge', String(params.min_edge));
    if (params.kelly_fraction) query.set('kelly_fraction', String(params.kelly_fraction));
    if (params.max_position_pct) query.set('max_position_pct', String(params.max_position_pct));
    if (params.fee_rate) query.set('fee_rate', String(params.fee_rate));
    const qs = query.toString();
    return this.fetch(`/api/backtest/run${qs ? '?' + qs : ''}`, {}, 30_000);
  }

  // Certified Backtest Results (read-only, immutable)
  // Results are cached by run_id; ETag-based caching is handled by the browser.
  async getCertifiedBacktestResults(runId: string): Promise<CertifiedBacktestResults> {
    return this.fetch<CertifiedBacktestResults>(
      `/api/backtest/certified/${encodeURIComponent(runId)}`,
      {},
      30_000
    );
  }

  // Get immutable run manifest (for provenance/reproducibility verification)
  // Contains full run fingerprint, config fingerprint, dataset fingerprint, and trust decision.
  async getRunManifest(runId: string): Promise<unknown> {
    return this.fetch(
      `/api/runs/${encodeURIComponent(runId)}/manifest`,
      {},
      15_000
    );
  }

  // Get manifest URL for direct download
  getManifestUrl(runId: string): string {
    return `${API_URL}/api/v2/backtest/runs/${encodeURIComponent(runId)}/manifest`;
  }

  // ==========================================================================
  // PERMALINKED RUN ENDPOINTS (for /runs/{run_id} routes)
  // Backend API: /api/v2/backtest/runs/:run_id/*
  // All endpoints return manifest_hash for integrity verification.
  // ==========================================================================

  // GET /api/v2/backtest/runs/{run_id} - Run summary with trust status
  async getRunSummary(runId: string): Promise<RunSummaryResponse> {
    return this.fetch<RunSummaryResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}`,
      {},
      15_000
    );
  }

  // GET /api/v2/backtest/runs/{run_id}/equity - Time-indexed equity curve
  async getRunEquitySeries(runId: string): Promise<RunEquitySeriesResponse> {
    return this.fetch<RunEquitySeriesResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/equity`,
      {},
      15_000
    );
  }

  // GET /api/v2/backtest/runs/{run_id}/drawdown - Time-indexed drawdown curve
  async getRunDrawdownSeries(runId: string): Promise<RunDrawdownSeriesResponse> {
    return this.fetch<RunDrawdownSeriesResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/drawdown`,
      {},
      15_000
    );
  }

  // GET /api/v2/backtest/runs/{run_id}/window-pnl - Per-window PnL series
  async getRunWindowPnLSeries(runId: string): Promise<RunWindowPnLSeriesResponse> {
    return this.fetch<RunWindowPnLSeriesResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/window-pnl`,
      {},
      15_000
    );
  }

  // GET /api/v2/backtest/runs/{run_id}/distributions - PnL histogram bins
  async getRunWindowPnLDistribution(runId: string): Promise<RunWindowPnLDistributionResponse> {
    return this.fetch<RunWindowPnLDistributionResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/distributions`,
      {},
      15_000
    );
  }

  /**
   * GET /api/v2/backtest/runs/{run_id}/distributions - Certified histogram bins
   * 
   * Returns backend-computed, deterministic histogram bins for certified views.
   * The frontend MUST NOT recompute bins from raw samples.
   * 
   * @param runId - The run identifier
   * @param binCount - Optional bin count (default: 50, max: 1000)
   * @returns Validated histogram response or throws an error
   * @throws Error if schema version is unsupported or validation fails
   */
  async getWindowPnlHistogram(
    runId: string,
    binCount: number = 50
  ): Promise<WindowPnlHistogramResponse> {
    const query = new URLSearchParams({ bin_count: String(binCount) });
    const response = await this.fetch<WindowPnlHistogramResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/distributions?${query.toString()}`,
      {},
      15_000
    );

    // Validate the response before returning
    const validationError = validateHistogramResponse(response);
    if (validationError) {
      throw new Error(`Histogram validation failed: ${validationError}`);
    }

    return response;
  }

  /**
   * Download histogram JSON for a certified run.
   * Returns the raw JSON string for file download.
   */
  async downloadHistogramJson(runId: string, binCount: number = 50): Promise<string> {
    const histogram = await this.getWindowPnlHistogram(runId, binCount);
    return JSON.stringify(histogram, null, 2);
  }

  /**
   * Get the URL for downloading histogram JSON directly.
   */
  getHistogramDownloadUrl(runId: string, binCount: number = 50): string {
    return `${API_URL}/api/v2/backtest/runs/${encodeURIComponent(runId)}/distributions?bin_count=${binCount}`;
  }

  // GET /api/v2/backtest/runs/{run_id}/manifest - Full manifest JSON
  async getRunManifestTyped(runId: string): Promise<RunManifestResponse> {
    return this.fetch<RunManifestResponse>(
      `/api/v2/backtest/runs/${encodeURIComponent(runId)}/manifest`,
      {},
      15_000
    );
  }

  // Fetch all run data in parallel and verify manifest hash consistency
  async getAggregatedRunData(runId: string): Promise<AggregatedRunData | ManifestIntegrityError> {
    const [summary, equity, drawdown, windowPnL, distribution] = await Promise.all([
      this.getRunSummary(runId),
      this.getRunEquitySeries(runId),
      this.getRunDrawdownSeries(runId),
      this.getRunWindowPnLSeries(runId),
      this.getRunWindowPnLDistribution(runId),
    ]);

    // Verify all manifest hashes match
    const expectedHash = summary.manifest_hash;
    const hashes: Record<string, string> = {
      summary: summary.manifest_hash,
      equity: equity.manifest_hash,
      drawdown: drawdown.manifest_hash,
      windowPnL: windowPnL.manifest_hash,
      distribution: distribution.manifest_hash,
    };

    const mismatchedEndpoints = Object.entries(hashes)
      .filter(([_, hash]) => hash !== expectedHash)
      .map(([endpoint]) => endpoint);

    if (mismatchedEndpoints.length > 0) {
      return {
        type: 'manifest_mismatch',
        runId,
        expectedHash,
        actualHashes: hashes,
        message: `Artifact integrity error: manifest hash mismatch across endpoints (${mismatchedEndpoints.join(', ')})`,
      };
    }

    return { summary, equity, drawdown, windowPnL, distribution };
  }

  // List available certified backtest runs
  async listCertifiedBacktestRuns(params?: {
    limit?: number;
    strategy_name?: string;
  }): Promise<{ runs: Array<{ run_id: string; strategy_name?: string; trust_level: string; created_at: number }> }> {
    const query = new URLSearchParams();
    if (params?.limit) query.set('limit', String(params.limit));
    if (params?.strategy_name) query.set('strategy_name', params.strategy_name);
    const qs = query.toString();
    return this.fetch(`/api/backtest/certified${qs ? '?' + qs : ''}`, {}, 10_000);
  }

  // Paper Trading
  async getPaperTradingState(): Promise<PaperTradingState> {
    return this.fetch('/api/paper/state', {}, 5_000);
  }

  async startPaperTrading(req: PaperTradingStartRequest): Promise<{ success: boolean }> {
    return this.fetch('/api/paper/start', {
      method: 'POST',
      body: JSON.stringify(req),
    }, 5_000);
  }

  async stopPaperTrading(): Promise<{ success: boolean }> {
    return this.fetch('/api/paper/stop', { method: 'POST' }, 5_000);
  }

  async resetPaperTrading(): Promise<{ success: boolean }> {
    return this.fetch('/api/paper/reset', { method: 'POST' }, 5_000);
  }
}

import type { BacktestResults, PaperTradingState, PaperTradingStartRequest } from '../types/vault';

// Performance types
export interface PerformanceDashboardResponse {
  timestamp: number;
  uptime_secs: number;
  latency: SystemLatencySummary;
  pipeline: PipelineSnapshot;
  memory: MemorySnapshot;
  cpu: CpuSnapshot;
  io: IoSnapshot;
  throughput: ThroughputSnapshot;
  comprehensive: ComprehensiveSnapshot;
}

export interface ComprehensiveSnapshot {
  timestamp: number;
  t2t: T2TSnapshot;
  venue_rt: VenueRTSnapshot[];
  throughput: ComprehensiveThroughput;
  queues: QueueStatsSnapshot[];
  md_integrity: SourceIntegritySnapshot[];
  order_lifecycle: OrderLifecycleSnapshot;
  failures: FailureSnapshot;
  serialization: SerializationSnapshot;
}

export interface T2TSnapshot {
  md_receive: HistogramSummary;
  md_decode: HistogramSummary;
  signal_compute: HistogramSummary;
  risk_check: HistogramSummary;
  order_build: HistogramSummary;
  wire_send: HistogramSummary;
  total: HistogramSummary;
  jitter: JitterMetrics;
}

export interface JitterMetrics {
  stddev_us: number;
  variance_us: number;
  spike_count: number;
  spike_rate_pct: number;
  sample_count: number;
}

export interface VenueRTSnapshot {
  venue: string;
  order_to_ack: HistogramSummary;
  order_to_fill: HistogramSummary;
  cancel_to_ack: HistogramSummary;
  rejects: number;
  reject_reasons: Record<string, number>;
}

export interface ComprehensiveThroughput {
  uptime_secs: number;
  md_messages_per_sec: number;
  md_bytes_per_sec: number;
  md_decode_per_sec: number;
  strategy_evals_per_sec: number;
  signals_per_sec: number;
  risk_checks_per_sec: number;
  orders_per_sec: number;
  cancels_per_sec: number;
  db_writes_per_sec: number;
  fill_rate_pct: number;
  reject_rate_pct: number;
  log_drop_rate_pct: number;
}

export interface QueueStatsSnapshot {
  name: string;
  capacity: number;
  current_depth: number;
  max_depth: number;
  utilization_pct: number;
  enqueues: number;
  dequeues: number;
  drops: number;
  drop_rate_pct: number;
  wait_time: HistogramSummary;
  blocked_time_us: number;
  blocked_count: number;
}

export interface SourceIntegritySnapshot {
  source: string;
  messages: number;
  gaps: number;
  out_of_order: number;
  duplicates: number;
  gap_rate_pct: number;
  ooo_rate_pct: number;
  dup_rate_pct: number;
  max_clock_skew_us: number;
  recoveries: number;
  recovery_time: HistogramSummary;
}

export interface OrderLifecycleSnapshot {
  orders_created: number;
  orders_sent: number;
  orders_acked: number;
  orders_filled: number;
  orders_partial: number;
  orders_rejected: number;
  orders_cancelled: number;
  cancel_rejects: number;
  stale_quotes: number;
  crossed_blocked: number;
  invalid_blocked: number;
  reject_rate_pct: number;
  fill_rate_pct: number;
  partial_rate_pct: number;
  reject_reasons: Record<string, number>;
  queue_time: HistogramSummary;
}

export interface FailureSnapshot {
  reconnects: number;
  recovery_time: HistogramSummary;
  warmup_time: HistogramSummary;
  circuit_breaker_trips: number;
  watchdog_resets: number;
  crash_recoveries: number;
  degraded_mode_time_us: number;
  component_failures: Record<string, number>;
}

export interface SerializationSnapshot {
  encode_time: HistogramSummary;
  decode_time: HistogramSummary;
  bytes_encoded: number;
  bytes_decoded: number;
  encode_count: number;
  decode_count: number;
  avg_encode_bytes: number;
  avg_decode_bytes: number;
  zero_copy_rate_pct: number;
  by_message_type: Record<string, MessageTypeSnapshot>;
}

export interface MessageTypeSnapshot {
  encode_time: HistogramSummary;
  decode_time: HistogramSummary;
  avg_encode_bytes: number;
  avg_decode_bytes: number;
  encode_count: number;
  decode_count: number;
}

export interface IoSnapshot {
  read_ops: number;
  write_ops: number;
  read_bytes: number;
  write_bytes: number;
}

export interface SystemLatencySummary {
  timestamp: number;
  market_data: {
    binance_ws: HistogramSummary;
    dome_ws: HistogramSummary;
    dome_rest: HistogramSummary;
    polymarket_ws: HistogramSummary;
    polymarket_rest: HistogramSummary;
    gamma_api: HistogramSummary;
  };
  signal_pipeline: {
    detection: HistogramSummary;
    enrichment: HistogramSummary;
    broadcast: HistogramSummary;
    storage: HistogramSummary;
  };
  trading: {
    fast15m_t2t: HistogramSummary;
    fast15m_gamma: HistogramSummary;
    fast15m_book: HistogramSummary;
    fast15m_order: HistogramSummary;
    long_t2t: HistogramSummary;
    long_llm: HistogramSummary;
  };
  counters: {
    binance_updates: number;
    dome_ws_events: number;
    signals_detected: number;
    signals_stored: number;
    fast15m_evaluations: number;
    fast15m_trades: number;
    api_requests: number;
    cache_hits: number;
    cache_misses: number;
  };
}

export interface HistogramSummary {
  name: string;
  count: number;
  min_us: number;
  max_us: number;
  mean_us: number;
  p50_us: number;
  p90_us: number;
  p95_us: number;
  p99_us: number;
  p999_us: number;
}

export interface QueueSnapshot {
  name: string;
  capacity: number;
  current_depth: number;
  max_depth: number;
  utilization_pct: number;
  enqueue_wait_p99_us: number;
  dequeue_wait_p99_us: number;
}

export interface NetworkSnapshot {
  uptime_secs: number;
  interfaces: {
    name: string;
    rx_packets: number;
    tx_packets: number;
    rx_dropped: number;
    tx_dropped: number;
    rx_drop_rate_pct: number;
  }[];
  tcp: {
    retransmits: number;
    retransmit_rate_pct: number;
  };
}

export interface VenueSnapshot {
  name: string;
  total_orders: number;
  total_fills: number;
  fill_rate_pct: number;
  send_ack_p50_us: number;
  send_ack_p99_us: number;
  send_fill_p99_us: number;
  cancel_ack_p99_us: number;
}

export interface AggregateVenueStats {
  total_orders: number;
  total_fills: number;
  fill_rate_pct: number;
  avg_send_ack_p99_us: number;
}

export interface PipelineSnapshot {
  binance_feed: ComponentMetrics;
  dome_ws: ComponentMetrics;
  signal_detection: ComponentMetrics;
  fast15m_engine: ComponentMetrics;
}

export interface ComponentMetrics {
  name: string;
  events_processed: number;
  errors: number;
  latency_count: number;
  latency_min_us: number;
  latency_max_us: number;
}

export interface MemorySnapshot {
  heap_used_bytes?: number;
  heap_total_bytes?: number;
  // New fields from backend
  heap_bytes?: number;
  peak_heap_bytes?: number;
  total_allocations?: number;
  total_deallocations?: number;
  total_bytes_allocated?: number;
  total_bytes_deallocated?: number;
  large_allocations?: number;
  allocation_rate?: number;
  components?: ComponentMemory[];
  system?: SystemMemory;
}

export interface ComponentMemory {
  name: string;
  estimated_bytes: number;
  item_count: number;
  description: string;
}

export interface SystemMemory {
  total_bytes: number;
  available_bytes: number;
  used_bytes: number;
  process_resident_bytes: number;
  process_virtual_bytes: number;
}

export interface CpuSnapshot {
  // Old fields (for backwards compat)
  user_pct?: number;
  system_pct?: number;
  cores?: number;
  // New fields from backend
  total_cpu_us?: number;
  cpu_utilization_pct?: number;
  span_count?: number;
  top_spans?: TopSpan[];
  hot_paths?: HotPath[];
  uptime_us?: number;
}

export interface TopSpan {
  name: string;
  total_us?: number;
  total_time_us?: number;
  count?: number;
  invocations?: number;
  avg_us?: number;
  min_time_us?: number;
  max_time_us?: number;
  last_time_us?: number;
  pct_of_total?: number;
}

export interface HotPath {
  path: string;
  total_us: number;
  count: number;
  avg_us: number;
  pct_of_total: number;
}

export interface ThroughputSnapshot {
  // Old fields (for backwards compat)
  requests_per_sec?: number;
  signals_per_sec?: number;
  // New fields from backend
  uptime_secs?: number;
  totals?: ThroughputTotals;
  lifetime_rates?: ThroughputRates;
  recent_rates?: ThroughputRates;
}

export interface ThroughputTotals {
  binance_updates: number;
  dome_ws_events: number;
  dome_rest_calls: number;
  polymarket_book_updates: number;
  signals_detected: number;
  signals_stored: number;
  api_requests: number;
  ws_messages_sent: number;
  trades_executed: number;
}

export interface ThroughputRates {
  binance_per_sec: number;
  dome_ws_per_sec: number;
  dome_rest_per_sec: number;
  polymarket_per_sec: number;
  signals_per_sec: number;
  api_per_sec: number;
  ws_messages_per_sec: number;
  trades_per_sec: number;
}

// ============================================================================
// 15M ARBITRAGE TYPES
// ============================================================================

export interface Arb15mResponse {
  timestamp: number;
  asset: string;
  binance: Arb15mBinanceData;
  polymarket: Arb15mPolymarketData;
  edge: Arb15mEdgeData;
}

export interface Arb15mBinanceData {
  symbol: string;
  mid_price: number | null;
  start_price: number | null;
  best_bid: number | null;
  best_ask: number | null;
  spread_bps: number | null;
  last_update_ts: number;
  recent_trades: TradeTick[];
  ohlc_history: OhlcPoint[];
  latency_history: LatencySample[];
}

export interface TradeTick {
  ts_ms: number;
  price: number;
  size: number;
  is_buyer_maker: boolean;
  receive_latency_us: number;
}

export interface OhlcPoint {
  ts: number;
  open: number;
  high: number;
  low: number;
  close: number;
  bid: number;
  ask: number;
}

export interface LatencySample {
  ts_ms: number;
  /** Total: barter_received → handler_complete */
  receive_us: number;
  /** Component 1: barter_received → handler_entry (internal propagation) */
  propagate_us: number;
  /** Component 2: handler_entry → state_update_complete (our processing) */
  process_us: number;
  /** Component 3: exchange_timestamp → barter_received (network + decode) */
  network_us: number;
}

export interface Arb15mPolymarketData {
  market_slug: string | null;
  time_remaining_sec: number | null;
  up_token_id: string | null;
  up_best_bid: number | null;
  up_best_ask: number | null;
  up_depth: OrderbookLevel[];
  down_token_id: string | null;
  down_best_bid: number | null;
  down_best_ask: number | null;
  down_depth: OrderbookLevel[];
}

export interface OrderbookLevel {
  price: number;
  size: number;
}

export interface Arb15mEdgeData {
  model_p_up: number | null;
  market_p_up: number | null;
  edge_up: number | null;
  edge_up_bps: number | null;
  recommended_side: string;
}

// ============================================================================
// ORACLE COMPARISON TYPES (Chainlink vs Binance)
// ============================================================================

export interface OracleComparisonResponse {
  fetched_at: number;
  assets: Record<string, AssetOracleComparisonData>;
  total_rolling_stats: AgreementStats;
  total_all_time_stats: AgreementStats;
}

export interface AssetOracleComparisonData {
  asset: string;
  rolling_window: WindowResolution[];
  price_ticks: PriceTick[];
  current_divergence_bps: number | null;
  current_divergence_aligned_bps: number | null;
  current_chainlink_staleness_ms: number | null;
  current_binance_staleness_ms: number | null;
  rolling_stats: AgreementStats;
  all_time_stats: AgreementStats;
  avg_chainlink_interval_us: number | null;
  avg_binance_interval_us: number | null;
}

export interface WindowResolution {
  asset: string;
  window_start_ts: number;
  window_end_ts: number;
  chainlink_start: number | null;
  chainlink_end: number | null;
  binance_start: number | null;
  binance_end: number | null;
  chainlink_outcome: boolean | null;
  binance_outcome: boolean | null;
  agreed: boolean | null;
  divergence_usd: number | null;
  divergence_bps: number | null;
  recorded_at: number;
}

export interface PriceTick {
  ts: number;
  chainlink_price: number | null;
  binance_price: number | null;
  divergence_bps: number | null;
  chainlink_ts: number | null;
  binance_ts: number | null;
  chainlink_staleness_ms: number | null;
  binance_staleness_ms: number | null;
  binance_price_at_chainlink_ts: number | null;
  binance_chainlink_skew_sec: number | null;
  divergence_aligned_bps: number | null;
  chainlink_latency_us: number | null;
  binance_latency_us: number | null;
}

export interface AgreementStats {
  total_windows: number;
  windows_with_data: number;
  agreed_count: number;
  disagreed_count: number;
  agreement_rate: number | null;
  avg_divergence_bps: number | null;
  max_divergence_bps: number | null;
  min_divergence_bps: number | null;
}

export const api = new ApiClient();
