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
  WalletAnalyticsResponse,
} from '../types/signal';
import { LoginRequest, LoginResponse } from '../types/auth';
import { TradeOrderRequest, TradeOrderResponse } from '../types/trade';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3000';
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
    const t = window.setTimeout(() => controller.abort(), timeoutMs);

    try {
      const response = await fetch(`${API_URL}${endpoint}`, {
        ...options,
        signal: controller.signal,
        headers: {
          ...this.getHeaders(),
          ...((options.headers as Record<string, string>) || {}),
        },
      });

      if (!response.ok) {
        throw new Error(`API Error: ${response.status} ${response.statusText}`);
      }

      return response.json();
    } catch (err: any) {
      if (err?.name === 'AbortError') {
        throw new Error('Request timed out');
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
    frictionMode: 'optimistic' | 'base' | 'pessimistic' = 'base'
  ): Promise<WalletAnalyticsResponse> {
    const query = new URLSearchParams({
      wallet_address: walletAddress,
      force: String(force),
      friction_mode: frictionMode,
    });
    return this.fetch<WalletAnalyticsResponse>(`/api/wallet/analytics?${query.toString()}`, {}, 2_500);
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
