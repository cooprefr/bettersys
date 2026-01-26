/**
 * Certified Run Cache
 * 
 * IMPORTANT: This module implements immutable artifact caching for certified backtest runs.
 * 
 * Design principles:
 * 1. Published runs are IMMUTABLE by contract - they never change once finalized.
 * 2. Cache aggressively: keyed by {run_id, etag} to ensure identical renders.
 * 3. Never background-refresh or auto-refetch - the user must explicitly reload.
 * 4. Treat different ETags as different artifacts, not updates.
 * 5. All timestamps and hashes come from the backend - no client-side computation.
 * 
 * This aggressive caching is intentional for institutional and audit-facing use.
 */

import type {
  CertifiedBacktestResults,
  RunSummaryResponse,
  RunEquitySeriesResponse,
  RunDrawdownSeriesResponse,
  RunWindowPnLSeriesResponse,
  RunWindowPnLDistributionResponse,
  RunManifestResponse,
  TrustLevel,
} from '../types/backtest';

// =============================================================================
// TYPES
// =============================================================================

/** Metadata extracted from HTTP response headers and body */
export interface ArtifactMeta {
  /** ETag from response header (for conditional requests) */
  etag: string | null;
  /** Manifest hash from response body (for display and verification) */
  manifestHash: string;
  /** Publish/creation timestamp from backend (unix seconds) */
  publishTimestamp: number;
  /** Trust level at time of fetch */
  trustLevel: TrustLevel;
  /** When this artifact was fetched (client time, for cache age display) */
  fetchedAt: number;
}

/** A cached artifact with its metadata */
export interface CachedArtifact<T> {
  data: T;
  meta: ArtifactMeta;
}

/** Cache key components (used for documentation) */
// interface CacheKey {
//   runId: string;
//   endpoint: string;
//   etag?: string;
// }

/** Result of a fetch operation */
export interface FetchResult<T> {
  data: T;
  meta: ArtifactMeta;
  fromCache: boolean;
  /** True if this is a 304 Not Modified response */
  notModified: boolean;
}

/** Mismatch detection result */
export interface MismatchInfo {
  type: 'manifest_hash' | 'trust_level' | 'etag';
  previous: string;
  current: string;
  runId: string;
}

// =============================================================================
// IN-MEMORY CACHE
// =============================================================================

/**
 * In-memory cache for certified run artifacts.
 * 
 * Keys are formatted as: `${runId}:${endpoint}` (etag is stored in the value)
 * This allows us to detect when an artifact has changed (different etag).
 */
const artifactCache = new Map<string, CachedArtifact<unknown>>();

/** Track the last known metadata per run for mismatch detection */
const runMetaCache = new Map<string, ArtifactMeta>();

function makeCacheKey(runId: string, endpoint: string): string {
  return `${runId}:${endpoint}`;
}

// =============================================================================
// CACHE OPERATIONS
// =============================================================================

export function getCached<T>(runId: string, endpoint: string): CachedArtifact<T> | null {
  const key = makeCacheKey(runId, endpoint);
  const cached = artifactCache.get(key);
  return cached as CachedArtifact<T> | null;
}

export function setCached<T>(
  runId: string,
  endpoint: string,
  data: T,
  meta: ArtifactMeta
): void {
  const key = makeCacheKey(runId, endpoint);
  artifactCache.set(key, { data, meta });
  
  // Also update the run-level meta cache
  const existingRunMeta = runMetaCache.get(runId);
  if (!existingRunMeta || meta.fetchedAt > existingRunMeta.fetchedAt) {
    runMetaCache.set(runId, meta);
  }
}

export function getRunMeta(runId: string): ArtifactMeta | null {
  return runMetaCache.get(runId) ?? null;
}

export function clearRunCache(runId: string): void {
  // Clear all endpoints for this run
  const keysToDelete: string[] = [];
  for (const key of artifactCache.keys()) {
    if (key.startsWith(`${runId}:`)) {
      keysToDelete.push(key);
    }
  }
  keysToDelete.forEach((k) => artifactCache.delete(k));
  runMetaCache.delete(runId);
}

export function clearAllCache(): void {
  artifactCache.clear();
  runMetaCache.clear();
}

// =============================================================================
// MISMATCH DETECTION
// =============================================================================

/**
 * Check if the new metadata differs from previously cached metadata for this run.
 * Returns mismatch info if there's a change, null otherwise.
 */
export function detectMismatch(
  runId: string,
  newMeta: ArtifactMeta
): MismatchInfo | null {
  const existing = runMetaCache.get(runId);
  if (!existing) {
    return null; // First fetch, no mismatch possible
  }

  // Check manifest hash mismatch (most critical)
  if (existing.manifestHash !== newMeta.manifestHash) {
    return {
      type: 'manifest_hash',
      previous: existing.manifestHash,
      current: newMeta.manifestHash,
      runId,
    };
  }

  // Check trust level change
  if (existing.trustLevel !== newMeta.trustLevel) {
    return {
      type: 'trust_level',
      previous: existing.trustLevel,
      current: newMeta.trustLevel,
      runId,
    };
  }

  // Check ETag change (indicates server-side change)
  if (existing.etag && newMeta.etag && existing.etag !== newMeta.etag) {
    return {
      type: 'etag',
      previous: existing.etag,
      current: newMeta.etag,
      runId,
    };
  }

  return null;
}

// =============================================================================
// HTTP FETCH WITH ETAG SUPPORT
// =============================================================================

const API_URL = import.meta.env.VITE_API_URL || '';

/**
 * Fetch a certified run endpoint with ETag caching support.
 * 
 * - Sends If-None-Match header if we have a cached ETag
 * - Handles 304 Not Modified by returning cached data
 * - Logs warnings if ETag is missing from response
 * - Never background-refreshes or auto-refetches
 */
export async function fetchCertifiedEndpoint<T extends { manifest_hash?: string; trust_level?: TrustLevel }>(
  runId: string,
  endpoint: string,
  getToken: () => string | null,
  options: {
    /** If true, bypass cache and force a fresh fetch */
    forceRefresh?: boolean;
    /** Timeout in milliseconds */
    timeoutMs?: number;
  } = {}
): Promise<FetchResult<T>> {
  const { forceRefresh = false, timeoutMs = 30_000 } = options;
  const cached = getCached<T>(runId, endpoint);

  // Build headers
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
  };

  const token = getToken();
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  // Add If-None-Match if we have a cached ETag and not forcing refresh
  if (cached?.meta.etag && !forceRefresh) {
    headers['If-None-Match'] = cached.meta.etag;
  }

  // Setup timeout
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetch(`${API_URL}${endpoint}`, {
      method: 'GET',
      headers,
      signal: controller.signal,
      // Ensure browser doesn't bypass our ETag handling
      cache: 'no-store',
    });

    // Handle 304 Not Modified - return cached data
    if (response.status === 304 && cached) {
      return {
        data: cached.data,
        meta: cached.meta,
        fromCache: true,
        notModified: true,
      };
    }

    if (!response.ok) {
      let bodyText = '';
      try {
        bodyText = await response.text();
      } catch {
        // ignore
      }
      throw new Error(
        `API Error: ${response.status} ${response.statusText}${bodyText ? ` - ${bodyText}` : ''}`
      );
    }

    const data: T = await response.json();

    // Extract ETag from response headers
    const etag = response.headers.get('ETag');
    if (!etag) {
      console.warn(
        `[CertifiedRunCache] Missing ETag header for ${endpoint}. ` +
        `Long-term caching disabled for this response.`
      );
    }

    // Extract metadata from response body
    const manifestHash = data.manifest_hash ?? '';
    const trustLevel = data.trust_level ?? 'Unknown';

    // For CertifiedBacktestResults, try to get the creation timestamp
    let publishTimestamp = Date.now() / 1000;
    if ('fetched_at' in data && typeof (data as any).fetched_at === 'number') {
      publishTimestamp = (data as any).fetched_at;
    }
    if ('created_at' in data && typeof (data as any).created_at === 'number') {
      publishTimestamp = (data as any).created_at;
    }

    const meta: ArtifactMeta = {
      etag,
      manifestHash,
      publishTimestamp,
      trustLevel,
      fetchedAt: Date.now(),
    };

    // Check for mismatch before updating cache
    const mismatch = detectMismatch(runId, meta);
    if (mismatch) {
      console.warn(
        `[CertifiedRunCache] Artifact mismatch detected for run ${runId}:`,
        mismatch
      );
      // Clear old cache to force full re-render
      clearRunCache(runId);
    }

    // Cache the result (only if we have an ETag)
    if (etag) {
      setCached(runId, endpoint, data, meta);
    }

    return {
      data,
      meta,
      fromCache: false,
      notModified: false,
    };
  } finally {
    clearTimeout(timeoutId);
  }
}

// =============================================================================
// SPECIALIZED FETCH FUNCTIONS
// =============================================================================

/** Fetch certified backtest results */
export async function fetchCertifiedResults(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<CertifiedBacktestResults>> {
  return fetchCertifiedEndpoint<CertifiedBacktestResults>(
    runId,
    `/api/backtest/certified/${encodeURIComponent(runId)}`,
    getToken,
    { ...options, timeoutMs: 30_000 }
  );
}

/** Fetch run summary */
export async function fetchRunSummary(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunSummaryResponse>> {
  return fetchCertifiedEndpoint<RunSummaryResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

/** Fetch equity series */
export async function fetchEquitySeries(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunEquitySeriesResponse>> {
  return fetchCertifiedEndpoint<RunEquitySeriesResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}/series/equity`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

/** Fetch drawdown series */
export async function fetchDrawdownSeries(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunDrawdownSeriesResponse>> {
  return fetchCertifiedEndpoint<RunDrawdownSeriesResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}/series/drawdown`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

/** Fetch window PnL series */
export async function fetchWindowPnLSeries(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunWindowPnLSeriesResponse>> {
  return fetchCertifiedEndpoint<RunWindowPnLSeriesResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}/series/window_pnl`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

/** Fetch window PnL distribution */
export async function fetchWindowPnLDistribution(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunWindowPnLDistributionResponse>> {
  return fetchCertifiedEndpoint<RunWindowPnLDistributionResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}/distribution/window_pnl`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

/** Fetch run manifest */
export async function fetchRunManifest(
  runId: string,
  getToken: () => string | null,
  options?: { forceRefresh?: boolean }
): Promise<FetchResult<RunManifestResponse>> {
  return fetchCertifiedEndpoint<RunManifestResponse>(
    runId,
    `/api/runs/${encodeURIComponent(runId)}/manifest`,
    getToken,
    { ...options, timeoutMs: 15_000 }
  );
}

// =============================================================================
// UTILITY FUNCTIONS
// =============================================================================

/** Format a manifest hash for display (first 12 characters) */
export function formatManifestHash(hash: string): string {
  if (!hash || hash.length === 0) return '---';
  return hash.slice(0, 12);
}

/** Format a publish timestamp as UTC string */
export function formatPublishTimestamp(ts: number): string {
  if (!ts || ts <= 0) return '---';
  try {
    const date = new Date(ts * 1000);
    return date.toISOString().replace('T', ' ').slice(0, 19) + ' UTC';
  } catch {
    return '---';
  }
}
