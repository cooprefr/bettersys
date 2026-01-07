import { useEffect, useRef, useState, useMemo, useCallback } from 'react';
import { Signal, SignalStats } from '../../types/signal';
import { SignalCardCompact } from './SignalCardCompact';
import { SignalFilters, FilterState } from './SignalFilters';
import { InspectorTab, SignalInspectorDrawer } from './SignalInspectorDrawer';
import { api } from '../../services/api';
import { useSignalStore } from '../../stores/signalStore';

const HISTORY_WINDOW_MS = 24 * 60 * 60 * 1000;
const PAGE_SIZE = 500;
const UPDOWN_NEEDLES = ['updown', 'up-or-down', 'up/down', 'up or down', 'up-down'];
const WALLET_PREFETCH_MAX = 25;
const WALLET_PREFETCH_CONCURRENCY = 3;

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

  // Use stats.total_signals if available, otherwise show current signals length
  const totalSignals = stats?.total_signals ?? signals.length;

  const hasActiveFilters = filters.hideUpDown || filters.whaleOnly || filters.minConfidence > 0;

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

  // Apply filters to signals
  const filteredSignals = useMemo(() => {
    return signals.filter((signal) => {
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
  }, [signals, filters, isUpDownMarket]);

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
  ]);

  useEffect(() => {
    if (autoScroll && feedRef.current && signals.length > previousSignalCount.current) {
      const isAtTop = feedRef.current.scrollTop <= 5;
      if (isAtTop) {
        feedRef.current.scrollTo({ top: 0, behavior: 'smooth' });
      }
    }
    previousSignalCount.current = signals.length;
  }, [signals.length, autoScroll]);

  const inspectorSignal = useMemo(() => {
    if (!inspectorSignalId) return null;
    return signals.find((s) => s.id === inspectorSignalId) || null;
  }, [signals, inspectorSignalId]);

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
      <div className="border-b border-grey/20 px-4 py-4 flex justify-between items-center">
        <div className="flex items-center gap-4">
          <h2 className="text-[14px] font-mono text-white font-semibold">
            SIGNAL FEED
          </h2>
          <div className="text-better-blue-lavender font-mono text-[13px]">
            LIVE ({totalSignals.toLocaleString()} TOTAL)
          </div>
          {(filters.hideUpDown || filters.whaleOnly || filters.minConfidence > 0) && (
            <div className="text-grey/80 font-mono text-[13px]">
              ({filteredSignals.length} SHOWN)
            </div>
          )}
        </div>

        <button
          onClick={() => setAutoScroll(!autoScroll)}
          className={`border border-grey/30 px-3 py-1 text-[11px] font-mono transition-colors duration-150 ${
            autoScroll ? 'bg-white text-black' : 'text-grey/80 hover:text-white'
          }`}
        >
          AUTO-SCROLL: {autoScroll ? 'ON' : 'OFF'}
        </button>
      </div>

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
              loadOlderSignals();
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
              <div className="px-4 py-3 text-center text-[11px] font-mono text-grey/60">
                {historyComplete
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
                <div className="text-xl font-mono text-grey/80 mb-2">
                  {signals.length > 0
                    ? hasActiveFilters && !historyComplete
                      ? 'SEARCHING 24H HISTORY...'
                      : historyComplete
                        ? 'NO MATCHING SIGNALS (24H)'
                        : 'NO MATCHING SIGNALS'
                    : error
                      ? 'ERROR'
                      : 'SCANNING...'}
                </div>
                <div className="text-xs font-mono text-grey/60 max-w-md whitespace-pre-wrap">
                  {signals.length > 0 ? (
                    hasActiveFilters && !historyComplete
                      ? 'No matches in the currently loaded window. Fetching older signals (up to 24h)...'
                      : historyComplete
                        ? 'No signals match the selected filters in the last 24 hours.'
                        : filters.hideUpDown
                          ? 'Most recent signals are Up/Down markets. Disable filter to see them.'
                          : filters.whaleOnly
                            ? 'No $1,000+ trades in recent signals. Disable filter to see smaller trades.'
                            : 'Adjust filters to see more'
                  ) : error ? error : 'Waiting for signals'}
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
