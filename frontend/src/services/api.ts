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
}

export const api = new ApiClient();
