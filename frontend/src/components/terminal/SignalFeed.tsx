import { useEffect, useRef, useState, useMemo, useCallback, useDeferredValue } from 'react';
import { Signal, SignalStats } from '../../types/signal';
import { SignalCardCompact } from './SignalCardCompact';
import { SignalFilters, FilterState } from './SignalFilters';
import { SignalSearch } from './SignalSearch';
import { InspectorTab, SignalInspectorDrawer } from './SignalInspectorDrawer';
import { api } from '../../services/api';
import { useSignalStore } from '../../stores/signalStore';

const HISTORY_WINDOW_MS = 24 * 60 * 60 * 1000;
const PAGE_SIZE = 500;
const SEARCH_PAGE_SIZE = 200;
const SEARCH_DEBOUNCE_MS = 150;
const UPDOWN_NEEDLES = ['updown', 'up-or-down', 'up/down', 'up or down', 'up-down'];
const WALLET_PREFETCH_MAX = 25;
const WALLET_PREFETCH_CONCURRENCY = 3;

type SearchStatus = {
  schema_ready: boolean;
  backfill_done: boolean;
  total_signals: number;
  indexed_rows: number;
  cursor_detected_at?: string | null;
  cursor_id?: string | null;
  timestamp: string;
};

type SearchBanner = { tone: 'info' | 'warning' | 'error'; message: string };

interface SignalFeedProps {
  signals: Signal[];
  stats?: SignalStats | null;
  error?: string | null;
}

export const SignalFeed: React.FC<SignalFeedProps> = ({ signals, stats, error }) => {
  const feedRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  const previousSignalCount = useRef(signals.length);
  const historyCursorRef = useRef<string | null>(null);

  const addSignals = useSignalStore((s) => s.addSignals);
  const [isLoadingHistory, setIsLoadingHistory] = useState(false);
  const [historyComplete, setHistoryComplete] = useState(false);
  const [historyStalled, setHistoryStalled] = useState(false);

  const [inspectorSignalId, setInspectorSignalId] = useState<string | null>(null);
  const [inspectorTab, setInspectorTab] = useState<InspectorTab>('DETAILS');

  const prefetchedWalletsRef = useRef<Set<string>>(new Set());
  
  // Filter state
  const [filters, setFilters] = useState<FilterState>({
    hideUpDown: false,
    minConfidence: 50,
    whaleOnly: false,
  });

  const [searchQuery, setSearchQuery] = useState('');
  const deferredSearchQuery = useDeferredValue(searchQuery);
  const [searchMode, setSearchMode] = useState<'server' | 'local'>('server');
  const [searchStatus, setSearchStatus] = useState<SearchStatus | null>(null);
  const [searchNotice, setSearchNotice] = useState<string | null>(null);
  const [searchResults, setSearchResults] = useState<Signal[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [searchComplete, setSearchComplete] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const searchRequestIdRef = useRef(0);
  const serverSearchFailuresRef = useRef(0);
  const searchAbortRef = useRef<AbortController | null>(null);
  const searchStatusAbortRef = useRef<AbortController | null>(null);

  // Use stats.total_signals if available, otherwise show current signals length
  const totalSignals = stats?.total_signals ?? signals.length;

  const hasActiveFilters = filters.hideUpDown || filters.whaleOnly || filters.minConfidence > 0;

  const trimmedSearchQuery = searchQuery.trim();
  const deferredTrimmedSearchQuery = deferredSearchQuery.trim();
  const isSearchActive = trimmedSearchQuery.length > 0;

  const isServerSearchActive = isSearchActive && searchMode === 'server';
  const isLocalSearchActive = isSearchActive && searchMode === 'local';

  const localSearchResults = useMemo(() => {
    if (!isLocalSearchActive) return [];
    if (!deferredTrimmedSearchQuery) return [];

    const terms = deferredTrimmedSearchQuery
      .toLowerCase()
      .split(/\s+/)
      .map((t) => t.trim())
      .filter(Boolean);
    if (!terms.length) return [];

    const getHaystack = (s: Signal) => {
      const st: any = s.signal_type as any;
      const walletAddr =
        s.signal_type.type === 'TrackedWalletEntry'
          ? st.wallet_address
          : s.signal_type.type === 'WhaleFollowing'
            ? st.whale_address
            : s.signal_type.type === 'EliteWallet'
              ? st.wallet_address
              : s.signal_type.type === 'InsiderWallet'
                ? st.wallet_address
                : '';

      return [
        s.market_slug,
        s.details?.market_title,
        (s.details as any)?.market_question,
        s.source,
        s.signal_type?.type,
        walletAddr,
        s.context?.order?.title,
        s.context?.order?.market_slug,
        s.context?.order?.user,
        s.context?.order?.token_label,
      ]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
    };

    return signals.filter((s) => {
      const hay = getHaystack(s);
      return terms.every((t) => hay.includes(t));
    });
  }, [deferredTrimmedSearchQuery, isLocalSearchActive, signals]);

  const activeSignals = useMemo(() => {
    if (!isSearchActive) return signals;
    return searchMode === 'server' ? searchResults : localSearchResults;
  }, [isSearchActive, localSearchResults, searchMode, searchResults, signals]);

  const searchBanner: SearchBanner | null = useMemo(() => {
    if (!isSearchActive) return null;

    if (isLocalSearchActive) {
      return {
        tone: 'warning',
        message:
          searchNotice ||
          'Local search is limited to the loaded feed (up to 24h). Restart backend to enable full-history search.',
      };
    }

    if (searchError) {
      return { tone: 'error', message: searchError };
    }

    if (searchNotice) {
      return { tone: 'warning', message: searchNotice };
    }

    if (searchStatus) {
      if (!searchStatus.schema_ready) {
        return {
          tone: 'warning',
          message: 'Server search index not ready yet (schema missing). Restart the backend to apply DB schema.',
        };
      }

      if (!searchStatus.backfill_done && searchStatus.total_signals > 0) {
        return {
          tone: 'info',
          message: `Indexing full history: ${searchStatus.indexed_rows.toLocaleString()}/${searchStatus.total_signals.toLocaleString()} indexed (results may be partial).`,
        };
      }
    }

    return null;
  }, [isLocalSearchActive, isSearchActive, searchError, searchNotice, searchStatus]);

  const isUpDownMarket = useCallback((signal: Signal) => {
    const matches = (raw: unknown) => {
      if (typeof raw !== 'string') return false;
      const s = raw.toLowerCase();
      return UPDOWN_NEEDLES.some((n) => s.includes(n));
    };

    if (matches(signal.market_slug)) return true;
    if (matches(signal.details?.market_title)) return true;
    if (matches(signal.context?.order?.market_slug)) return true;

    const m: any = signal.context?.market;
    if (matches(m?.question)) return true;
    if (matches(m?.title)) return true;

    return false;
  }, []);

  // Apply filters to active signals
  const filteredSignals = useMemo(() => {
    return activeSignals.filter((signal) => {
      // Filter by confidence (convert to percentage)
      const confidencePct = signal.confidence * 100;
      if (confidencePct < filters.minConfidence) {
        return false;
      }

      // Filter out up/down markets
      if (filters.hideUpDown) {
        if (isUpDownMarket(signal)) {
          return false;
        }
      }

      // Filter for whale trades ($1000+)
      if (filters.whaleOnly) {
        const positionValue = signal.signal_type.type === 'TrackedWalletEntry'
          ? signal.signal_type.position_value_usd
          : signal.details.volume_24h || 0;
        if (positionValue < 1000) {
          return false;
        }
      }

      return true;
    });
  }, [activeSignals, filters, isUpDownMarket]);

  // Search: reset state immediately when the user edits the query.
  useEffect(() => {
    searchRequestIdRef.current += 1;
    serverSearchFailuresRef.current = 0;

    searchAbortRef.current?.abort();
    searchStatusAbortRef.current?.abort();
    setSearchError(null);
    setSearchNotice(null);
    setSearchComplete(false);
    if (!trimmedSearchQuery) {
      setSearchResults([]);
      setIsSearching(false);
      setSearchMode('server');
      setSearchStatus(null);
      return;
    }
    setSearchResults([]);
  }, [trimmedSearchQuery]);

  // Search: fetch results via backend FTS endpoint (debounced + deferred typing).
  useEffect(() => {
    if (!isServerSearchActive) return;
    if (!trimmedSearchQuery) return;
    if (!deferredTrimmedSearchQuery) return;

    const reqId = searchRequestIdRef.current + 1;
    searchRequestIdRef.current = reqId;
    let controller: AbortController | null = null;

    const timer = window.setTimeout(() => {
      controller = new AbortController();
      searchAbortRef.current?.abort();
      searchAbortRef.current = controller;
      const signal = controller.signal;

      (async () => {
        setIsSearching(true);
        setSearchError(null);
        setSearchNotice(null);
        try {
          const resp = await api.searchSignals(
            {
              q: deferredTrimmedSearchQuery,
              limit: SEARCH_PAGE_SIZE,
              exclude_updown: filters.hideUpDown || undefined,
              min_confidence: filters.minConfidence > 0 ? filters.minConfidence / 100 : undefined,
            },
            { signal }
          );

          if (reqId !== searchRequestIdRef.current) return;

          serverSearchFailuresRef.current = 0;
          const batch = resp.signals || [];
          setSearchResults(batch);
          setSearchComplete(batch.length < SEARCH_PAGE_SIZE);
        } catch (e: any) {
          if (e?.aborted) return;
          if (reqId !== searchRequestIdRef.current) return;

          const status = e?.status;
          const message = e?.message ?? 'Search failed';

          if (status === 404 || status === 501) {
            setSearchMode('local');
            setIsSearching(false);
            setSearchResults([]);
            setSearchComplete(true);
            setSearchError(null);
            setSearchNotice(
              'Server search unavailable. Falling back to local search (loaded feed only).'
            );
            return;
          }

          if (typeof status === 'number' && status >= 500) {
            const failures = serverSearchFailuresRef.current + 1;
            serverSearchFailuresRef.current = failures;
            if (failures >= 2) {
              setSearchMode('local');
              setIsSearching(false);
              setSearchResults([]);
              setSearchComplete(true);
              setSearchError(null);
              setSearchNotice(
                `Server search unstable (${status}). Falling back to local search (loaded feed only).`
              );
              return;
            }
          }

          setSearchResults([]);
          setSearchComplete(true);
          setSearchError(message);
        } finally {
          if (reqId === searchRequestIdRef.current) {
            setIsSearching(false);
          }
        }
      })();
    }, SEARCH_DEBOUNCE_MS);

    return () => {
      window.clearTimeout(timer);
      controller?.abort();
    };
  }, [deferredTrimmedSearchQuery, filters.hideUpDown, filters.minConfidence, isServerSearchActive, trimmedSearchQuery]);

  const loadMoreSearchResults = useCallback(async () => {
    if (!isSearchActive) return;
    if (searchMode !== 'server') return;
    if (!deferredTrimmedSearchQuery) return;
    if (isSearching || searchComplete) return;

    const reqId = searchRequestIdRef.current;

    const last = searchResults[searchResults.length - 1];
    if (!last) return;

    setIsSearching(true);
    try {
      const resp = await api.searchSignals({
        q: deferredTrimmedSearchQuery,
        limit: SEARCH_PAGE_SIZE,
        before: last.detected_at,
        before_id: last.id,
        exclude_updown: filters.hideUpDown || undefined,
        min_confidence: filters.minConfidence > 0 ? filters.minConfidence / 100 : undefined,
      });

      if (reqId !== searchRequestIdRef.current) return;

      const batch = resp.signals || [];
      if (batch.length === 0) {
        setSearchComplete(true);
        return;
      }

      setSearchResults((prev) => {
        const seen = new Set(prev.map((s) => s.id));
        const next = [...prev];
        for (const s of batch) {
          if (!seen.has(s.id)) next.push(s);
        }
        return next;
      });

      if (batch.length < SEARCH_PAGE_SIZE) {
        setSearchComplete(true);
      }
    } catch (e: any) {
      if (reqId !== searchRequestIdRef.current) return;
      const status = e?.status;
      if (status === 404 || status === 501) {
        setSearchMode('local');
        setSearchNotice('Server search unavailable. Falling back to local search (loaded feed only).');
        setSearchError(null);
        return;
      }
      setSearchError(e?.message ?? 'Search failed');
      setSearchComplete(true);
    } finally {
      if (reqId === searchRequestIdRef.current) {
        setIsSearching(false);
      }
    }
  }, [deferredTrimmedSearchQuery, filters.hideUpDown, filters.minConfidence, isSearchActive, isSearching, searchComplete, searchMode, searchResults]);

  // Search: poll backend status (indexing/backfill progress) and auto-heal back to server search.
  useEffect(() => {
    if (!isSearchActive) return;

    let cancelled = false;
    let timeoutId: number | null = null;

    const tick = async () => {
      if (cancelled) return;

      const controller = new AbortController();
      searchStatusAbortRef.current?.abort();
      searchStatusAbortRef.current = controller;

      try {
        const status = await api.searchSignalsStatus({ signal: controller.signal });
        if (cancelled) return;
        setSearchStatus(status);

        if (searchMode === 'local' && status.schema_ready) {
          setSearchMode('server');
          setSearchNotice(null);
          setSearchError(null);
        }

        const nextMs =
          searchMode === 'server'
            ? status.backfill_done
              ? 10_000
              : 2_000
            : 5_000;
        timeoutId = window.setTimeout(tick, nextMs);
      } catch (e: any) {
        if (e?.aborted) return;
        if (cancelled) return;

        const code = e?.status;
        if (searchMode === 'server' && (code === 404 || code === 501)) {
          setSearchMode('local');
          setSearchNotice(
            'Server search unavailable. Falling back to local search (loaded feed only).'
          );
          setSearchError(null);
        }

        timeoutId = window.setTimeout(tick, searchMode === 'local' ? 7_000 : 5_000);
      }
    };

    tick();

    return () => {
      cancelled = true;
      if (timeoutId) window.clearTimeout(timeoutId);
      searchStatusAbortRef.current?.abort();
    };
  }, [isSearchActive, searchMode]);

  // Warm wallet analytics cache for the most visible wallets so PERFORMANCE opens instantly.
  useEffect(() => {
    if (!filteredSignals.length) return;

    const wallets: string[] = [];
    for (const s of filteredSignals) {
      const st: any = s.signal_type as any;
      const addr: string | null =
        s.signal_type.type === 'TrackedWalletEntry'
          ? st.wallet_address
          : s.signal_type.type === 'WhaleFollowing'
            ? st.whale_address
            : s.signal_type.type === 'EliteWallet'
              ? st.wallet_address
              : s.signal_type.type === 'InsiderWallet'
                ? st.wallet_address
                : s.context?.order?.user || null;
      if (typeof addr === 'string' && addr) wallets.push(addr.toLowerCase());
      if (wallets.length >= WALLET_PREFETCH_MAX * 2) break;
    }

    const unique: string[] = [];
    const seen = new Set<string>();
    for (const w of wallets) {
      if (seen.has(w)) continue;
      seen.add(w);
      unique.push(w);
    }

    const toFetch = unique
      .filter((w) => !prefetchedWalletsRef.current.has(w))
      .slice(0, WALLET_PREFETCH_MAX);
    if (!toFetch.length) return;

    let cancelled = false;
    const queue = [...toFetch];
    for (const w of toFetch) prefetchedWalletsRef.current.add(w);

    (async () => {
      const workers = Array.from({ length: WALLET_PREFETCH_CONCURRENCY }, async () => {
        while (!cancelled) {
          const next = queue.pop();
          if (!next) return;
          try {
            await api.getWalletAnalytics(next, false, 'base', 'scaled', true);
          } catch {
            // Best-effort.
          }
        }
      });
      await Promise.all(workers);
    })();

    return () => {
      cancelled = true;
    };
  }, [filteredSignals]);

  const loadOlderSignals = useCallback(async () => {
    if (isLoadingHistory || historyComplete) return;
    if (!signals.length) return;

    const cutoffMs = Date.now() - HISTORY_WINDOW_MS;
    const oldest = signals[signals.length - 1];
    if (!oldest) return;

    const cursorKey = `${oldest.detected_at}|${oldest.id}|${filters.hideUpDown}`;
    if (historyCursorRef.current === cursorKey) {
      setHistoryStalled(true);
      setHistoryComplete(true);
      return;
    }
    historyCursorRef.current = cursorKey;

    const oldestMs = Date.parse(oldest.detected_at);
    if (Number.isFinite(oldestMs) && oldestMs <= cutoffMs) {
      setHistoryComplete(true);
      return;
    }

    setIsLoadingHistory(true);
    try {
      // Use server-side filtering when hideUpDown is enabled
      // This is much more efficient than client-side filtering
      const resp = await api.getSignals({
        limit: PAGE_SIZE,
        before: oldest.detected_at,
        before_id: oldest.id,
        exclude_updown: filters.hideUpDown || undefined,
      });

      const batch = resp.signals || [];
      if (batch.length === 0) {
        setHistoryComplete(true);
        return;
      }

      // Keep only the last 24h in the store (store also enforces this, but we filter here so
      // history completion can be detected reliably).
      const withinWindow = batch.filter((s) => {
        const t = Date.parse(s.detected_at);
        return Number.isNaN(t) || t >= cutoffMs;
      });
      if (withinWindow.length > 0) {
        addSignals(withinWindow);
      }

      const oldestFetched = batch[batch.length - 1];
      const oldestFetchedMs = Date.parse(oldestFetched.detected_at);
      if (batch.length < PAGE_SIZE || (Number.isFinite(oldestFetchedMs) && oldestFetchedMs < cutoffMs)) {
        setHistoryComplete(true);
      }
    } catch {
      // Keep terminal usable even if history paging fails.
      setHistoryComplete(true);
    } finally {
      setIsLoadingHistory(false);
    }
  }, [addSignals, filters.hideUpDown, historyComplete, isLoadingHistory, signals]);

  // Reset history paging state when hideUpDown filter changes
  // This allows the server-side filter to fetch fresh results
  useEffect(() => {
    historyCursorRef.current = null;
    setHistoryComplete(false);
    setHistoryStalled(false);
  }, [filters.hideUpDown]);

  // If filters eliminate everything in the currently loaded window, automatically page back
  // through the last-24h history until we find matches (or hit the cutoff).
  // Minimum threshold: keep paging until we have at least 10 matching signals
  const MIN_FILTERED_SIGNALS = 10;
  useEffect(() => {
    if (isSearchActive) return;
    if (!hasActiveFilters) return;
    if (!signals.length) return;
    if (filteredSignals.length >= MIN_FILTERED_SIGNALS) return;
    if (historyComplete || isLoadingHistory) return;

    loadOlderSignals();
  }, [
    filteredSignals.length,
    hasActiveFilters,
    historyComplete,
    isLoadingHistory,
    loadOlderSignals,
    signals.length,
    isSearchActive,
  ]);

  // Local-search safety net: if we have no matches, keep paging back through the last-24h window.
  useEffect(() => {
    if (!isLocalSearchActive) return;
    if (!signals.length) return;
    if (filteredSignals.length > 0) return;
    if (historyComplete || isLoadingHistory) return;

    loadOlderSignals();
  }, [
    filteredSignals.length,
    historyComplete,
    isLoadingHistory,
    isLocalSearchActive,
    loadOlderSignals,
    signals.length,
  ]);

  useEffect(() => {
    if (isSearchActive) return;
    if (autoScroll && feedRef.current && signals.length > previousSignalCount.current) {
      const isAtTop = feedRef.current.scrollTop <= 5;
      if (isAtTop) {
        feedRef.current.scrollTo({ top: 0, behavior: 'smooth' });
      }
    }
    previousSignalCount.current = signals.length;
  }, [signals.length, autoScroll, isSearchActive]);

  const inspectorSignal = useMemo(() => {
    if (!inspectorSignalId) return null;
    return activeSignals.find((s) => s.id === inspectorSignalId) || null;
  }, [activeSignals, inspectorSignalId]);

  useEffect(() => {
    if (inspectorSignalId && !inspectorSignal) {
      setInspectorSignalId(null);
      setInspectorTab('DETAILS');
    }
  }, [inspectorSignalId, inspectorSignal]);

  const openInspector = (signalId: string, tab: InspectorTab) => {
    setInspectorSignalId(signalId);
    setInspectorTab(tab);
  };

  return (
    <div className="relative h-full bg-void flex flex-col overflow-hidden">
      {/* Header */}
      <div className="border-b border-grey/20 bg-surface px-4 md:px-6 py-5 flex justify-between items-center">
        <div className="flex items-center gap-4">
          <h2 className="text-[16px] font-mono text-fg font-semibold">
            SIGNAL FEED
          </h2>
          <div className="text-better-blue-lavender font-mono text-[13px]">
            {isSearchActive
              ? searchMode === 'local'
                ? 'SEARCH (LOCAL)'
                : 'SEARCH'
              : `LIVE (${totalSignals.toLocaleString()} TOTAL)`}
          </div>
          {(filters.hideUpDown || filters.whaleOnly || filters.minConfidence > 0) && (
            <div className="text-fg/90 font-mono text-[13px]">
              ({filteredSignals.length} SHOWN)
            </div>
          )}
        </div>

        {!isSearchActive && (
          <button
            onClick={() => setAutoScroll(!autoScroll)}
            className={`border border-grey/30 px-3 py-1 text-[11px] font-mono transition-colors duration-150 ${
              autoScroll ? 'bg-fg text-void' : 'text-fg/90 hover:text-fg'
            }`}
          >
            AUTO-SCROLL: {autoScroll ? 'ON' : 'OFF'}
          </button>
        )}
      </div>

      {/* Search (always visible) */}
      <SignalSearch
        value={searchQuery}
        onChange={setSearchQuery}
        resultsCount={filteredSignals.length}
        isSearching={isSearching}
        mode={searchMode}
        banner={searchBanner}
      />

      {/* Filters */}
      <SignalFilters filters={filters} onFiltersChange={setFilters} />

      {/* Feed */}
      <div className="flex-1 overflow-hidden flex">
        {/* Left: list */}
        <div
          ref={feedRef}
          className="flex-1 overflow-y-auto"
          onScroll={(e) => {
            const el = e.currentTarget;
            const nearBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 300;
            if (nearBottom) {
              if (isSearchActive) {
                if (searchMode === 'server') {
                  loadMoreSearchResults();
                } else {
                  loadOlderSignals();
                }
              } else {
                loadOlderSignals();
              }
            }
          }}
        >
          {filteredSignals.length > 0 ? (
            <>
              {filteredSignals.map((signal) => (
                <SignalCardCompact
                  key={signal.id}
                  signal={signal}
                  onOpenInspector={(tab) => openInspector(signal.id, tab)}
                />
              ))}
              <div className="px-4 md:px-6 py-3 text-center text-[11px] font-mono text-fg/70">
                {isSearchActive
                  ? searchMode === 'server'
                    ? searchComplete
                      ? 'END (SEARCH)'
                      : isSearching
                        ? 'SEARCHING...'
                        : 'SCROLL FOR MORE'
                    : historyComplete
                      ? historyStalled
                        ? 'HISTORY UNAVAILABLE'
                        : 'END (LOCAL 24H)'
                      : isLoadingHistory
                        ? 'LOADING 24H HISTORY...'
                        : 'SCROLL FOR MORE (24H)'
                  : historyComplete
                    ? historyStalled
                      ? 'HISTORY UNAVAILABLE'
                      : 'END (24H)'
                    : isLoadingHistory
                      ? 'LOADING 24H HISTORY...'
                      : 'SCROLL FOR 24H HISTORY'}
              </div>
            </>
          ) : (
            <div className="flex items-center justify-center h-full">
              <div className="text-center">
                <div className="text-xl font-mono text-fg/90 mb-2">
                  {isSearchActive
                    ? searchMode === 'server' && isSearching
                      ? 'SEARCHING...'
                      : searchError
                        ? 'ERROR'
                        : searchMode === 'local'
                          ? 'NO MATCHING SIGNALS (LOCAL)'
                          : 'NO MATCHING SIGNALS'
                    : signals.length > 0
                      ? hasActiveFilters && !historyComplete
                        ? 'SEARCHING 24H HISTORY...'
                        : historyComplete
                          ? 'NO MATCHING SIGNALS (24H)'
                          : 'NO MATCHING SIGNALS'
                      : error
                        ? 'ERROR'
                        : 'SCANNING...'}
                </div>
                <div className="text-xs font-mono text-fg/70 max-w-md whitespace-pre-wrap">
                  {isSearchActive
                    ? searchError
                      ? searchError
                      : searchMode === 'local'
                        ? 'Searching within the loaded feed (up to 24h). If you expect older matches, restart the backend to enable full-history search.'
                        : 'Try different keywords, quotes for phrases, or clear search.'
                    : signals.length > 0
                      ? hasActiveFilters && !historyComplete
                        ? 'No matches in the currently loaded window. Fetching older signals (up to 24h)...'
                        : historyComplete
                          ? 'No signals match the selected filters in the last 24 hours.'
                          : filters.hideUpDown
                            ? 'Most recent signals are Up/Down markets. Disable filter to see them.'
                            : filters.whaleOnly
                              ? 'No $1,000+ trades in recent signals. Disable filter to see smaller trades.'
                              : 'Adjust filters to see more'
                      : error
                        ? error
                        : 'Waiting for signals'}
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Right: inspector */}
        <SignalInspectorDrawer
          open={Boolean(inspectorSignal)}
          signal={inspectorSignal}
          activeTab={inspectorTab}
          onTabChange={setInspectorTab}
          onClose={() => setInspectorSignalId(null)}
        />
      </div>
    </div>
  );
};
