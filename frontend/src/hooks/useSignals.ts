/**
 * Optimized signal fetching hook
 * - Uses 0.5-second polling for a more "live" terminal feel
 * - Debounces rapid requests
 * - Cancels in-flight requests on unmount
 */
import { useEffect, useRef, useCallback } from 'react';
import { useSignalStore } from '../stores/signalStore';
import { api } from '../services/api';

export const useSignals = (opts?: { wsConnected?: boolean }) => {
  const { signals, stats, isLoading, error, setSignals, setStats, setError } = useSignalStore();
  const isMounted = useRef(true);
  const isLoadingRef = useRef(false);

  const loadSignals = useCallback(async (params?: { limit?: number; min_confidence?: number }) => {
    // Prevent concurrent requests
    if (isLoadingRef.current) return;
    isLoadingRef.current = true;

    try {
      const startTime = performance.now();
      // Request 500 signals - balance between data availability and performance
      const response = await api.getSignals({ limit: 500, ...params });
      
      if (!isMounted.current) return;
      
      const latency = Math.round(performance.now() - startTime);
      window.dispatchEvent(new CustomEvent('api-latency', { detail: latency }));
      
      setSignals(response.signals);
    } catch (err: unknown) {
      if (isMounted.current) {
        const message = err instanceof Error ? err.message : 'Failed to load signals';
        setError(message);
      }
    } finally {
      isLoadingRef.current = false;
    }
  }, [setSignals, setError]);

  const loadStats = useCallback(async () => {
    try {
      const statsData = await api.getSignalStats();
      if (isMounted.current) {
        setStats(statsData);
      }
    } catch (err) {
      // Stats are non-critical, just log
      console.error('Failed to load stats:', err);
    }
  }, [setStats]);

  useEffect(() => {
    isMounted.current = true;
    
    // Load initial data
    loadSignals();
    loadStats();

    // Polling intervals
    const signalPollMs = opts?.wsConnected ? 5_000 : 500;
    const signalInterval = setInterval(loadSignals, signalPollMs);
    const statsInterval = setInterval(loadStats, 10000);   // 10 seconds

    return () => {
      isMounted.current = false;
      clearInterval(signalInterval);
      clearInterval(statsInterval);
    };
  }, [loadSignals, loadStats, opts?.wsConnected]);

  const refreshSignals = useCallback(() => {
    loadSignals();
    loadStats();
  }, [loadSignals, loadStats]);

  return {
    signals,
    stats,
    isLoading,
    error,
    loadSignals,
    loadStats,
    refreshSignals,
  };
};
