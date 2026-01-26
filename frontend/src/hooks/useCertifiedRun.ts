/**
 * useCertifiedRun Hook
 * 
 * IMPORTANT: This hook provides production-grade data fetching for certified backtest runs.
 * 
 * Key behaviors:
 * 1. NO auto-refresh on focus - published runs are immutable
 * 2. NO interval polling - data never changes after publication
 * 3. Aggressive caching - reloading a permalink yields identical UI without refetch
 * 4. ETag-based conditional requests - network inspector shows 304 on repeat visits
 * 5. Mismatch detection - different ETags/hashes force full re-render
 * 6. Fail-closed - must have manifest before rendering metrics
 * 
 * This is intentional for institutional and audit-facing use.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../services/api';
import {
  fetchCertifiedResults,
  getCached,
  getRunMeta,
  clearRunCache,
  type ArtifactMeta,
  type MismatchInfo,
  detectMismatch,
} from '../services/certifiedRunCache';
import type { CertifiedBacktestResults } from '../types/backtest';

// =============================================================================
// TYPES
// =============================================================================

export interface CertifiedRunState {
  /** The certified run data (null until loaded) */
  data: CertifiedBacktestResults | null;
  /** Artifact metadata (etag, manifest hash, publish timestamp) */
  meta: ArtifactMeta | null;
  /** Loading state */
  loading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Whether the data came from cache */
  fromCache: boolean;
  /** Whether this was a 304 Not Modified response */
  notModified: boolean;
  /** Mismatch info if a change was detected */
  mismatch: MismatchInfo | null;
}

export interface UseCertifiedRunOptions {
  /** If true, bypass cache and force a fresh fetch */
  forceRefresh?: boolean;
  /** Callback when a mismatch is detected */
  onMismatch?: (info: MismatchInfo) => void;
}

export interface UseCertifiedRunResult extends CertifiedRunState {
  /** Manually trigger a refresh (bypasses cache) */
  refresh: () => Promise<void>;
  /** Clear the cache for this run */
  clearCache: () => void;
}

// =============================================================================
// HOOK
// =============================================================================

export function useCertifiedRun(
  runId: string,
  options: UseCertifiedRunOptions = {}
): UseCertifiedRunResult {
  const { forceRefresh = false, onMismatch } = options;

  const [state, setState] = useState<CertifiedRunState>({
    data: null,
    meta: null,
    loading: true,
    error: null,
    fromCache: false,
    notModified: false,
    mismatch: null,
  });

  // Track if component is mounted to avoid state updates after unmount
  const isMountedRef = useRef(true);
  // Track the current runId to handle race conditions
  const currentRunIdRef = useRef(runId);
  // Prevent concurrent fetches
  const fetchingRef = useRef(false);

  // Get token function
  const getToken = useCallback(() => api.getToken(), []);

  // Fetch function
  const doFetch = useCallback(
    async (force: boolean = false) => {
      if (fetchingRef.current && !force) {
        return;
      }

      fetchingRef.current = true;
      currentRunIdRef.current = runId;

      // Check if we have cached data first (for instant display)
      const cachedMeta = getRunMeta(runId);
      const cached = getCached<CertifiedBacktestResults>(
        runId,
        `/api/backtest/certified/${encodeURIComponent(runId)}`
      );

      // If we have cached data and not forcing refresh, show it immediately
      if (cached && !force) {
        if (isMountedRef.current && currentRunIdRef.current === runId) {
          setState({
            data: cached.data,
            meta: cached.meta,
            loading: false,
            error: null,
            fromCache: true,
            notModified: false,
            mismatch: null,
          });
        }
        fetchingRef.current = false;
        return;
      }

      // Show loading state
      if (isMountedRef.current) {
        setState((prev) => ({
          ...prev,
          loading: true,
          error: null,
        }));
      }

      try {
        const result = await fetchCertifiedResults(runId, getToken, {
          forceRefresh: force,
        });

        // Check if we're still mounted and on the same runId
        if (!isMountedRef.current || currentRunIdRef.current !== runId) {
          return;
        }

        // Check for mismatch
        let mismatch: MismatchInfo | null = null;
        if (cachedMeta && result.meta) {
          mismatch = detectMismatch(runId, result.meta);
          if (mismatch && onMismatch) {
            onMismatch(mismatch);
          }
        }

        setState({
          data: result.data,
          meta: result.meta,
          loading: false,
          error: null,
          fromCache: result.fromCache,
          notModified: result.notModified,
          mismatch,
        });
      } catch (e: unknown) {
        if (!isMountedRef.current || currentRunIdRef.current !== runId) {
          return;
        }

        const msg = e instanceof Error ? e.message : 'Failed to load certified results';
        setState((prev) => ({
          ...prev,
          loading: false,
          error: msg,
        }));
      } finally {
        fetchingRef.current = false;
      }
    },
    [runId, getToken, onMismatch]
  );

  // Initial fetch on mount and runId change
  useEffect(() => {
    isMountedRef.current = true;
    doFetch(forceRefresh);

    return () => {
      isMountedRef.current = false;
    };
  }, [runId, doFetch, forceRefresh]);

  // Manual refresh function
  const refresh = useCallback(async () => {
    await doFetch(true);
  }, [doFetch]);

  // Clear cache function
  const clearCache = useCallback(() => {
    clearRunCache(runId);
  }, [runId]);

  return {
    ...state,
    refresh,
    clearCache,
  };
}

// =============================================================================
// UTILITY HOOKS
// =============================================================================

/**
 * Hook that returns only the artifact metadata for a run.
 * Useful for displaying the certified artifact indicator.
 */
export function useCertifiedRunMeta(runId: string): {
  meta: ArtifactMeta | null;
  loading: boolean;
} {
  const { meta, loading } = useCertifiedRun(runId);
  return { meta, loading };
}

/**
 * Hook that checks if we have a cached version of a run.
 * Useful for determining whether to show a loading skeleton.
 */
export function useHasCachedRun(runId: string): boolean {
  const [hasCached, setHasCached] = useState(false);

  useEffect(() => {
    const cached = getCached<CertifiedBacktestResults>(
      runId,
      `/api/backtest/certified/${encodeURIComponent(runId)}`
    );
    setHasCached(cached !== null);
  }, [runId]);

  return hasCached;
}
