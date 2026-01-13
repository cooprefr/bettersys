import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import {
  MarketSnapshotResponse,
  Signal,
  SignalEnrichResponse,
  WalletAnalyticsResponse,
} from '../../types/signal';
import {
  formatConfidence,
  formatDelta,
  formatPnL,
  formatPrice,
  formatTimestamp,
  formatVolumeCompact,
  metricColorClass,
} from '../../utils/formatters';

export type InspectorTab = 'DETAILS' | 'PERFORMANCE' | 'BOOK' | 'TRADE';

type InFlight<T> = { startedAtMs: number; promise: Promise<T> };

const walletAnalyticsCache = new Map<string, WalletAnalyticsResponse>();
const walletAnalyticsInFlight = new Map<string, InFlight<WalletAnalyticsResponse>>();
const INFLIGHT_WALLET_TTL_MS = 35_000;

const bookCache = new Map<string, MarketSnapshotResponse>();
const bookInFlight = new Map<string, InFlight<MarketSnapshotResponse>>();
const INFLIGHT_BOOK_TTL_MS = 12_000;

const enrichCache = new Map<string, SignalEnrichResponse>();
const enrichInFlight = new Map<string, InFlight<SignalEnrichResponse>>();
const INFLIGHT_ENRICH_TTL_MS = 5_000;

function formatDayLabel(tsSeconds: number | undefined): string {
  if (!tsSeconds || !Number.isFinite(tsSeconds)) return '';
  try {
    const d = new Date(tsSeconds * 1000);
    return d.toLocaleDateString('en-US', { month: 'short', day: '2-digit' });
  } catch {
    return '';
  }
}

function curveMinMax(points: { value: number }[]): { min?: number; max?: number } {
  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (const p of points) {
    const v = p.value;
    if (!Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }
  if (min === Number.POSITIVE_INFINITY || max === Number.NEGATIVE_INFINITY) return {};
  return { min, max };
}

function formatPct(v: number | undefined, digits: number = 1): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v >= 0 ? '+' : '';
  return `${sign}${v.toFixed(digits)}%`;
}

function formatWinRate(v: number | undefined): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `${(v * 100).toFixed(0)}%`;
}

function formatProfitFactor(v: number | undefined): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  if (v >= 999) return '>999';
  return v.toFixed(2);
}

function normalizeMarketTitle(raw: string): string {
  const s = (raw || '').replace(/\s+/g, ' ').trim();
  if (!s) return '';
  const quoted = s.match(/\bon\s+['"]([^'"]+)['"]/i);
  if (quoted?.[1]) return quoted[1].replace(/\s+/g, ' ').trim();
  return s;
}

function tabButtonClass(active: boolean): string {
  return [
    'px-2.5 py-1.5 text-[11px] font-mono border transition-colors',
    active
      ? 'border-white bg-white text-black'
      : 'border-grey/30 text-grey/80 hover:text-white hover:border-grey/50',
  ].join(' ');
}

const Sparkline: React.FC<{ values: number[]; width?: number; height?: number }> = ({
  values,
  width = 210,
  height = 36,
}) => {
  if (values.length < 2) return null;

  const min = Math.min(...values);
  const max = Math.max(...values);
  const range = max - min || 1;

  const points = values
    .map((v, i) => {
      const x = (i / (values.length - 1)) * width;
      const y = height - ((v - min) / range) * height;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(' ');

  const isUp = values[values.length - 1] >= values[0];
  const stroke = isUp ? '#22c55e' : '#ef4444';

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true">
      <polyline
        fill="none"
        stroke={stroke}
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
        points={points}
        opacity={0.9}
      />
    </svg>
  );
};

export interface SignalInspectorDrawerProps {
  open: boolean;
  signal: Signal | null;
  activeTab: InspectorTab;
  onClose: () => void;
  onTabChange: (tab: InspectorTab) => void;
}

export const SignalInspectorDrawer: React.FC<SignalInspectorDrawerProps> = ({
  open,
  signal,
  activeTab,
  onClose,
  onTabChange,
}) => {
  const [walletAnalytics, setWalletAnalytics] = useState<WalletAnalyticsResponse | null>(null);
  const [analyticsLoading, setAnalyticsLoading] = useState(false);
  const [analyticsError, setAnalyticsError] = useState<string | null>(null);
  const [analyticsSource, setAnalyticsSource] = useState<'cache' | 'network' | null>(null);
  const [frictionMode, setFrictionMode] = useState<'optimistic' | 'base' | 'pessimistic'>('base');
  const [copyModel, setCopyModel] = useState<'scaled' | 'mtm'>('scaled');

  const analyticsRetryTimerRef = React.useRef<number | null>(null);
  const analyticsRetryCountRef = React.useRef<number>(0);
  const analyticsRetryKeyRef = React.useRef<string | null>(null);

  const [bookData, setBookData] = useState<MarketSnapshotResponse | null>(null);
  const [bookLoading, setBookLoading] = useState(false);
  const [bookError, setBookError] = useState<string | null>(null);

  const [enrich15m, setEnrich15m] = useState<SignalEnrichResponse | null>(null);
  const [enrich15mLoading, setEnrich15mLoading] = useState(false);
  const [enrich15mError, setEnrich15mError] = useState<string | null>(null);

  const [tradeSide, setTradeSide] = useState<'BUY' | 'SELL'>('BUY');
  const [tradeNotionalUsd, setTradeNotionalUsd] = useState<number>(25);
  const [tradeOrderType, setTradeOrderType] = useState<'GTC' | 'FAK' | 'FOK'>('GTC');
  const [tradePriceMode, setTradePriceMode] = useState<'JOIN' | 'CROSS' | 'CUSTOM'>('JOIN');
  const [tradeCustomPrice, setTradeCustomPrice] = useState<string>('');
  const [tradeArmed, setTradeArmed] = useState(false);
  const [tradeStatus, setTradeStatus] = useState<string | null>(null);

  const tradingEnabled =
    String(import.meta.env.VITE_ENABLE_TRADING || '').toLowerCase() === 'true' ||
    String(import.meta.env.VITE_ENABLE_TRADING || '').toLowerCase() === '1';

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  // Reset transient fetch state when changing signals.
  useEffect(() => {
    setAnalyticsError(null);
    setBookError(null);
    setEnrich15mError(null);
    setAnalyticsLoading(false);
    setBookLoading(false);
    setBookData(null);
    setWalletAnalytics(null);
    setAnalyticsSource(null);
    setEnrich15m(null);
    setEnrich15mLoading(false);
    setTradeArmed(false);
    setTradeStatus(null);

    if (analyticsRetryTimerRef.current) {
      window.clearTimeout(analyticsRetryTimerRef.current);
      analyticsRetryTimerRef.current = null;
    }
    analyticsRetryCountRef.current = 0;
    analyticsRetryKeyRef.current = null;
  }, [signal?.id]);

  useEffect(() => {
    if (open) return;
    if (analyticsRetryTimerRef.current) {
      window.clearTimeout(analyticsRetryTimerRef.current);
      analyticsRetryTimerRef.current = null;
    }
    analyticsRetryCountRef.current = 0;
    analyticsRetryKeyRef.current = null;
  }, [open]);

  const ctx = signal?.context;
  const derived = ctx?.derived;
  const order = ctx?.order;

  const signalId = signal?.id ?? null;
  const marketSlug = signal?.market_slug ?? '';
  const isUpDown15m = useMemo(() => {
    const slug = marketSlug.toLowerCase();
    return /^(btc|eth|sol|xrp)-updown-15m-\d+/.test(slug);
  }, [marketSlug]);

  const walletAddress = useMemo(() => {
    if (!signal) return null;
    const st: any = signal.signal_type as any;
    if (signal.signal_type.type === 'TrackedWalletEntry') return st.wallet_address as string;
    if (signal.signal_type.type === 'WhaleFollowing') return st.whale_address as string;
    if (signal.signal_type.type === 'EliteWallet') return st.wallet_address as string;
    if (signal.signal_type.type === 'InsiderWallet') return st.wallet_address as string;
    return order?.user ?? null;
  }, [signal, order?.user]);

  const marketTitle = useMemo(() => {
    if (!signal) return '';
    const m: any = (ctx as any)?.market;
    const fromCtx = m ? String(m.question || m.title || '') : '';
    return normalizeMarketTitle(fromCtx || signal.details.market_title || order?.title || signal.market_slug || '');
  }, [signal, order?.title, ctx]);

  const canFetchAnalytics = Boolean(walletAddress);

  const marketSlugForBook = signal?.market_slug || order?.market_slug;
  const outcomeForBook = (order?.token_label as string | undefined) || ((signal?.signal_type as any)?.token_label as string | undefined);

  const clobTokenIdForBook = (order?.token_id as string | undefined) || (signal?.details?.market_id as string | undefined);
  const hasNumericClobTokenId = typeof clobTokenIdForBook === 'string' && /^[0-9]+$/.test(clobTokenIdForBook);
  const canFetchBook = Boolean(hasNumericClobTokenId || (marketSlugForBook && outcomeForBook));

  const fetchWalletAnalytics = useCallback(
    async (force: boolean = false) => {
      if (!walletAddress) return;
      const key = `${walletAddress.toLowerCase()}:${frictionMode}:${copyModel}`;
      setAnalyticsError(null);

      if (force || analyticsRetryKeyRef.current !== key) {
        analyticsRetryKeyRef.current = key;
        analyticsRetryCountRef.current = 0;
      }

      let keepLoading = false;

      try {
        if (!force && walletAnalyticsCache.has(key)) {
          setWalletAnalytics(walletAnalyticsCache.get(key)!);
          setAnalyticsSource('cache');
          return;
        }

        setAnalyticsLoading(true);

        const nowMs = Date.now();
        const existing = walletAnalyticsInFlight.get(key);
        if (!existing || force || nowMs - existing.startedAtMs > INFLIGHT_WALLET_TTL_MS) {
          const promise = api.getWalletAnalytics(walletAddress, force, frictionMode, copyModel);
          walletAnalyticsInFlight.set(key, { startedAtMs: nowMs, promise });
        }
        const data = await walletAnalyticsInFlight.get(key)!.promise;
        walletAnalyticsInFlight.delete(key);
        walletAnalyticsCache.set(key, data);
        setWalletAnalytics(data);
        setAnalyticsSource('network');
      } catch (e: any) {
        walletAnalyticsInFlight.delete(key);
        const msg = String(e?.message || 'Failed to fetch wallet analytics');
        if (msg.toLowerCase().includes('timed out') && analyticsRetryCountRef.current < 6) {
          analyticsRetryCountRef.current += 1;
          setAnalyticsError('Still computing… retrying');
          keepLoading = true;
          if (analyticsRetryTimerRef.current) {
            window.clearTimeout(analyticsRetryTimerRef.current);
          }
          analyticsRetryTimerRef.current = window.setTimeout(() => {
            analyticsRetryTimerRef.current = null;
            fetchWalletAnalytics(false);
          }, 900);
          return;
        }
        setAnalyticsError(msg);
      } finally {
        if (!keepLoading) {
          setAnalyticsLoading(false);
        }
      }
    },
    [walletAddress, frictionMode, copyModel]
  );

  const fetchBook = useCallback(
    async (force: boolean = false) => {
      if (!canFetchBook) return;
      const key = hasNumericClobTokenId
        ? `token:${clobTokenIdForBook}`
        : `slug:${marketSlugForBook}:${outcomeForBook}`;

      setBookError(null);
      setBookLoading(true);
      try {
        if (!force && bookCache.has(key)) {
          setBookData(bookCache.get(key)!);
          return;
        }

        const nowMs = Date.now();
        const existing = bookInFlight.get(key);
        if (!existing || force || nowMs - existing.startedAtMs > INFLIGHT_BOOK_TTL_MS) {
          const promise = hasNumericClobTokenId
            ? api.getMarketSnapshot(clobTokenIdForBook!, 12)
            : api.getMarketSnapshotBySlug(marketSlugForBook!, outcomeForBook!, 12);
          bookInFlight.set(key, { startedAtMs: nowMs, promise });
        }

        const snapshot = await bookInFlight.get(key)!.promise;
        bookInFlight.delete(key);
        bookCache.set(key, snapshot);
        setBookData(snapshot);
      } catch (e: any) {
        bookInFlight.delete(key);
        setBookError(e?.message ?? 'Failed to fetch orderbook');
      } finally {
        setBookLoading(false);
      }
    },
    [canFetchBook, hasNumericClobTokenId, clobTokenIdForBook, marketSlugForBook, outcomeForBook]
  );

  const fetchUpDown15mEnrich = useCallback(
    async (fresh: boolean = false) => {
      if (!signalId || !isUpDown15m) return;
      const levels = 10;
      const key = `${signalId}:${levels}`;

      setEnrich15mError(null);

      const cached = enrichCache.get(key);
      if (!fresh && cached) {
        const nowSec = Math.floor(Date.now() / 1000);
        if (nowSec - cached.fetched_at <= 2) {
          setEnrich15m(cached);
          return;
        }
      }

      setEnrich15mLoading(true);
      try {
        const nowMs = Date.now();
        const existing = enrichInFlight.get(key);
        if (!existing || fresh || nowMs - existing.startedAtMs > INFLIGHT_ENRICH_TTL_MS) {
          const promise = api.getSignalEnrich(signalId, levels, fresh);
          enrichInFlight.set(key, { startedAtMs: nowMs, promise });
        }

        const resp = await enrichInFlight.get(key)!.promise;
        enrichInFlight.delete(key);
        enrichCache.set(key, resp);
        setEnrich15m(resp);
      } catch (e: any) {
        enrichInFlight.delete(key);
        setEnrich15mError(e?.message ?? 'Failed to enrich');
      } finally {
        setEnrich15mLoading(false);
      }
    },
    [signalId, isUpDown15m]
  );

  const bookDepth = useMemo(() => {
    if (!bookData) return null;

    const levelCount = 12;

    let bidCum = 0;
    const bids = bookData.bids.slice(0, levelCount).map((b) => {
      bidCum += b.size;
      return { ...b, cum: bidCum };
    });

    let askCum = 0;
    const asks = bookData.asks.slice(0, levelCount).map((a) => {
      askCum += a.size;
      return { ...a, cum: askCum };
    });

    const maxCum = Math.max(bids[bids.length - 1]?.cum ?? 0, asks[asks.length - 1]?.cum ?? 0, 1);
    return { bids, asks, maxCum };
  }, [bookData]);

  useEffect(() => {
    if (!open) return;
    if (!canFetchAnalytics) return;
    if (walletAnalytics || analyticsLoading) return;
    fetchWalletAnalytics(false);
  }, [open, canFetchAnalytics, walletAnalytics, analyticsLoading, fetchWalletAnalytics]);

  // Refetch when friction mode changes - force fresh data from backend
  const prevFrictionModeRef = React.useRef(frictionMode);
  useEffect(() => {
    if (!open || !canFetchAnalytics) return;
    // Only refetch if friction mode actually changed (not on initial mount)
    if (prevFrictionModeRef.current !== frictionMode) {
      prevFrictionModeRef.current = frictionMode;
      // Clear displayed analytics and fetch for new mode (prefer cache for speed).
      setWalletAnalytics(null);
      fetchWalletAnalytics(false);
    }
  }, [frictionMode, open, canFetchAnalytics, fetchWalletAnalytics]);

  const prevCopyModelRef = React.useRef(copyModel);
  useEffect(() => {
    if (!open || !canFetchAnalytics) return;
    if (prevCopyModelRef.current !== copyModel) {
      prevCopyModelRef.current = copyModel;
      setWalletAnalytics(null);
      fetchWalletAnalytics(false);
    }
  }, [copyModel, open, canFetchAnalytics, fetchWalletAnalytics]);

  useEffect(() => {
    if (!open) return;
    if (activeTab !== 'BOOK' && activeTab !== 'TRADE') return;
    if (!canFetchBook) return;
    if (bookData || bookLoading) return;
    fetchBook(false);
  }, [open, activeTab, canFetchBook, bookData, bookLoading, fetchBook]);

  useEffect(() => {
    if (!open) return;
    if (activeTab !== 'DETAILS') return;
    if (!signalId || !isUpDown15m) return;
    if (enrich15m || enrich15mLoading) return;
    fetchUpDown15mEnrich(false);
  }, [open, activeTab, signalId, isUpDown15m, enrich15m, enrich15mLoading, fetchUpDown15mEnrich]);

  useEffect(() => {
    if (!open) return;
    if (activeTab !== 'DETAILS') return;
    if (!signalId || !isUpDown15m) return;
    const t = window.setInterval(() => {
      fetchUpDown15mEnrich(false);
    }, 5_000);
    return () => window.clearInterval(t);
  }, [open, activeTab, signalId, isUpDown15m, fetchUpDown15mEnrich]);

  const walletCurvePoints = walletAnalytics?.wallet_realized_curve || [];
  const copyCurvePoints = walletAnalytics?.copy_trade_curve || [];
  const walletSeries = walletCurvePoints.map((p) => p.value).filter((v) => Number.isFinite(v));
  const copySeries = copyCurvePoints.map((p) => p.value).filter((v) => Number.isFinite(v));
  const walletMM = curveMinMax(walletCurvePoints);
  const copyMM = curveMinMax(copyCurvePoints);

  const analyticsUpdatedAtMs = useMemo(() => {
    if (!walletAnalytics?.updated_at) return null;
    const raw = walletAnalytics.updated_at;
    if (raw > 1_000_000_000_000) return raw;
    return raw * 1000;
  }, [walletAnalytics?.updated_at]);

  const analyticsAgeMs = analyticsUpdatedAtMs ? Date.now() - analyticsUpdatedAtMs : null;
  const analyticsIsStale = typeof analyticsAgeMs === 'number' && analyticsAgeMs > 5 * 60 * 1000;

  if (!open || !signal) return null;

  return (
    <div className="w-[460px] shrink-0 border-l border-grey/20 bg-void h-full flex flex-col">
      {/* Header */}
      <div className="border-b border-grey/20 px-4 py-4 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-[11px] font-mono text-grey/80">
            {formatTimestamp(signal.detected_at)} • {formatConfidence(signal.confidence)}
          </div>
          <div className="mt-1 text-[14px] font-mono text-white truncate">{marketTitle}</div>
          {walletAddress && (
            <div className="mt-1 text-[11px] font-mono text-grey/80 truncate">
              wallet: {walletAddress}
            </div>
          )}
        </div>
        <button
          type="button"
          onClick={onClose}
          className="border border-grey/30 px-2 py-1 text-[11px] font-mono text-grey/80 hover:text-white hover:border-grey/50"
        >
          [CLOSE]
        </button>
      </div>

      {/* Tabs */}
      <div className="px-4 py-2 border-b border-grey/10 flex flex-wrap gap-2">
        {(['DETAILS', 'PERFORMANCE', 'BOOK', 'TRADE'] as InspectorTab[]).map((t) => (
          <button
            key={t}
            type="button"
            onClick={() => onTabChange(t)}
            className={tabButtonClass(activeTab === t)}
          >
            {t}
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-4 py-3">
        {activeTab === 'DETAILS' && (
          <div className="space-y-3">
            {isUpDown15m && (
              <div className="bg-white/5 rounded p-3 space-y-2">
                <div className="flex items-center justify-between">
                  <div className="text-[11px] font-mono text-grey/80">15m live (Binance + CLOB)</div>
                  <button
                    type="button"
                    className="text-[11px] font-mono border border-grey/20 px-2 py-1 text-grey/80 hover:text-white hover:border-grey/40"
                    disabled={enrich15mLoading}
                    onClick={() => fetchUpDown15mEnrich(true)}
                  >
                    [REFRESH]
                  </button>
                </div>

                {enrich15mError && <div className="text-[11px] text-danger font-mono">{enrich15mError}</div>}
                {enrich15mLoading && <div className="text-[11px] text-grey/80 font-mono">Loading…</div>}

                {enrich15m && (
                  <div className="space-y-2">
                    <div className="grid grid-cols-4 gap-2">
                      <div className="bg-black/40 rounded p-2">
                        <div className="text-[9px] text-better-blue-light/90">Binance</div>
                        <div className="text-[12px] font-mono text-white tabular-nums">
                          {typeof enrich15m.binance?.mid === 'number' ? formatPrice(enrich15m.binance.mid) : '---'}
                        </div>
                      </div>
                      <div className="bg-black/40 rounded p-2">
                        <div className="text-[9px] text-better-blue-light/90">σ/√s</div>
                        <div className="text-[12px] font-mono text-white tabular-nums">
                          {typeof enrich15m.binance?.sigma_per_sqrt_s === 'number'
                            ? enrich15m.binance.sigma_per_sqrt_s.toExponential(2)
                            : '---'}
                        </div>
                      </div>
                      <div className="bg-black/40 rounded p-2">
                        <div className="text-[9px] text-better-blue-light/90">p(up)</div>
                        <div className="text-[12px] font-mono text-white tabular-nums">
                          {typeof enrich15m.binance?.p_up_shrunk === 'number'
                            ? `${(enrich15m.binance.p_up_shrunk * 100).toFixed(1)}%`
                            : '---'}
                        </div>
                      </div>
                      <div className="bg-black/40 rounded p-2">
                        <div className="text-[9px] text-better-blue-light/90">t_rem</div>
                        <div className="text-[12px] font-mono text-white tabular-nums">
                          {typeof enrich15m.binance?.t_rem_sec === 'number' ? `${Math.round(enrich15m.binance.t_rem_sec)}s` : '---'}
                        </div>
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-2">
                      {(() => {
                        const pUp = enrich15m.binance?.p_up_shrunk;
                        const pDown = typeof pUp === 'number' ? 1 - pUp : null;

                        const upAsk = enrich15m.up?.best_ask;
                        const downAsk = enrich15m.down?.best_ask;

                        const edgeUp =
                          typeof pUp === 'number' && typeof upAsk === 'number' ? pUp - upAsk : null;
                        const edgeDown =
                          typeof pDown === 'number' && typeof downAsk === 'number' ? pDown - downAsk : null;

                        const fmtEdge = (v: number | null) => {
                          if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
                          const cents = v * 100;
                          const sign = cents >= 0 ? '+' : '';
                          return `${sign}${cents.toFixed(2)}¢`;
                        };

                        return (
                          <>
                            <div className="bg-black/40 rounded p-2">
                              <div className="flex items-center justify-between">
                                <div className="text-[9px] text-better-blue-light/90">UP</div>
                                <div className={`text-[9px] font-mono tabular-nums ${metricColorClass(edgeUp)}`}>
                                  edge {fmtEdge(edgeUp)}
                                </div>
                              </div>
                              <div className="mt-1 grid grid-cols-2 gap-2">
                                <div>
                                  <div className="text-[9px] text-grey/70">Bid</div>
                                  <div className="text-[12px] font-mono text-success tabular-nums">
                                    {typeof enrich15m.up?.best_bid === 'number' ? formatPrice(enrich15m.up.best_bid) : '---'}
                                  </div>
                                </div>
                                <div>
                                  <div className="text-[9px] text-grey/70">Ask</div>
                                  <div className="text-[12px] font-mono text-danger tabular-nums">
                                    {typeof enrich15m.up?.best_ask === 'number' ? formatPrice(enrich15m.up.best_ask) : '---'}
                                  </div>
                                </div>
                              </div>
                            </div>

                            <div className="bg-black/40 rounded p-2">
                              <div className="flex items-center justify-between">
                                <div className="text-[9px] text-better-blue-light/90">DOWN</div>
                                <div className={`text-[9px] font-mono tabular-nums ${metricColorClass(edgeDown)}`}>
                                  edge {fmtEdge(edgeDown)}
                                </div>
                              </div>
                              <div className="mt-1 grid grid-cols-2 gap-2">
                                <div>
                                  <div className="text-[9px] text-grey/70">Bid</div>
                                  <div className="text-[12px] font-mono text-success tabular-nums">
                                    {typeof enrich15m.down?.best_bid === 'number' ? formatPrice(enrich15m.down.best_bid) : '---'}
                                  </div>
                                </div>
                                <div>
                                  <div className="text-[9px] text-grey/70">Ask</div>
                                  <div className="text-[12px] font-mono text-danger tabular-nums">
                                    {typeof enrich15m.down?.best_ask === 'number' ? formatPrice(enrich15m.down.best_ask) : '---'}
                                  </div>
                                </div>
                              </div>
                            </div>
                          </>
                        );
                      })()}
                    </div>
                  </div>
                )}
              </div>
            )}

            <div className="grid grid-cols-3 gap-2">
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90 uppercase">Δ</div>
                <div
                  className={`text-[13px] font-mono tabular-nums ${metricColorClass(
                    typeof derived?.price_delta_bps === 'number'
                      ? derived.price_delta_bps
                      : typeof derived?.price_delta_abs === 'number'
                        ? derived.price_delta_abs
                        : null
                  )}`}
                >
                  {formatDelta(derived?.price_delta_bps, derived?.price_delta_abs)}
                </div>
              </div>
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90 uppercase">Spread</div>
                <div className="text-[13px] font-mono text-white tabular-nums">
                  {typeof derived?.spread_at_entry === 'number' ? formatPrice(derived.spread_at_entry) : '---'}
                </div>
              </div>
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90 uppercase">Now</div>
                <div className="text-[13px] font-mono text-white tabular-nums">
                  {typeof ctx?.price?.latest?.price === 'number' ? formatPrice(ctx.price.latest.price) : '---'}
                </div>
              </div>
            </div>

            <div className="grid grid-cols-4 gap-2">
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90">PnL 7D</div>
                <div className={`text-[13px] font-mono tabular-nums ${metricColorClass(derived?.pnl_7d)}`}>
                  {typeof derived?.pnl_7d === 'number' ? formatPnL(derived.pnl_7d) : '---'}
                </div>
              </div>
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90">PnL 30D</div>
                <div className={`text-[13px] font-mono tabular-nums ${metricColorClass(derived?.pnl_30d)}`}>
                  {typeof derived?.pnl_30d === 'number' ? formatPnL(derived.pnl_30d) : '---'}
                </div>
              </div>
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90">Sh 30D</div>
                <div className={`text-[13px] font-mono tabular-nums ${metricColorClass(derived?.sharpe_30d)}`}>
                  {typeof derived?.sharpe_30d === 'number' ? derived.sharpe_30d.toFixed(2) : '---'}
                </div>
              </div>
              <div className="bg-white/5 rounded p-2">
                <div className="text-[10px] text-better-blue-light/90">Trades 24h</div>
                <div className="text-[13px] font-mono text-white tabular-nums">
                  {typeof derived?.trade_count_24h === 'number' ? derived.trade_count_24h : '---'}
                </div>
              </div>
            </div>

            <div className="text-[11px] font-mono text-grey/80">
              context: {signal.context_status ?? '---'}
              {signal.context_enriched_at ? ` • enriched_at=${signal.context_enriched_at}` : ''}
            </div>
            {Array.isArray(ctx?.errors) && ctx?.errors.length > 0 && (
              <div className="text-[11px] font-mono text-danger whitespace-pre-wrap">
                {ctx.errors.join('\n')}
              </div>
            )}
          </div>
        )}

        {activeTab === 'TRADE' && (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="text-[11px] font-mono text-grey/80">One-click trade (Polymarket, Polygon)</div>
              <div className={`text-[10px] font-mono ${tradingEnabled ? 'text-success' : 'text-warning'}`}>
                {tradingEnabled ? 'ENABLED' : 'DISABLED'}
              </div>
            </div>

            {!tradingEnabled && (
              <div className="text-[11px] font-mono text-warning">
                Trading is feature-flagged off. Set <span className="text-white">VITE_ENABLE_TRADING=true</span> to enable.
              </div>
            )}

            {canFetchBook && (
              <div className="grid grid-cols-3 gap-2">
                <div className="bg-white/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Bid</div>
                  <div className="text-[13px] font-mono text-success tabular-nums">
                    {typeof bookData?.best_bid === 'number' ? formatPrice(bookData.best_bid) : '---'}
                  </div>
                </div>
                <div className="bg-white/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Ask</div>
                  <div className="text-[13px] font-mono text-danger tabular-nums">
                    {typeof bookData?.best_ask === 'number' ? formatPrice(bookData.best_ask) : '---'}
                  </div>
                </div>
                <div className="bg-white/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Spr</div>
                  <div className="text-[13px] font-mono text-white tabular-nums">
                    {typeof bookData?.spread === 'number' ? formatPrice(bookData.spread) : '---'}
                  </div>
                </div>
              </div>
            )}

            <div className="bg-white/5 rounded p-3 space-y-3">
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Side</div>
                  <div className="mt-1 flex gap-2">
                    {(['BUY', 'SELL'] as const).map((s) => (
                      <button
                        key={s}
                        type="button"
                        onClick={() => setTradeSide(s)}
                        className={tabButtonClass(tradeSide === s)}
                      >
                        {s}
                      </button>
                    ))}
                  </div>
                </div>

                <div>
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Order type</div>
                  <div className="mt-1 flex gap-2">
                    {(['GTC', 'FAK', 'FOK'] as const).map((t) => (
                      <button
                        key={t}
                        type="button"
                        onClick={() => setTradeOrderType(t)}
                        className={tabButtonClass(tradeOrderType === t)}
                      >
                        {t}
                      </button>
                    ))}
                  </div>
                </div>
              </div>

              <div>
                <div className="text-[10px] text-better-blue-light/90 uppercase">Notional (USDC)</div>
                <div className="mt-1 flex items-center gap-2">
                  {[10, 25, 50, 100].map((v) => (
                    <button
                      key={v}
                      type="button"
                      onClick={() => setTradeNotionalUsd(v)}
                      className={tabButtonClass(tradeNotionalUsd === v)}
                    >
                      {v}
                    </button>
                  ))}
                  <input
                    value={String(tradeNotionalUsd)}
                    onChange={(e) => setTradeNotionalUsd(Number(e.target.value) || 0)}
                    className="ml-auto w-[120px] bg-black/50 border border-grey/20 px-2 py-1 text-[12px] font-mono text-white tabular-nums"
                    inputMode="decimal"
                  />
                </div>
              </div>

              <div className="grid grid-cols-2 gap-2">
                <div>
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Price mode</div>
                  <div className="mt-1 flex gap-2">
                    {(['JOIN', 'CROSS', 'CUSTOM'] as const).map((m) => (
                      <button
                        key={m}
                        type="button"
                        onClick={() => setTradePriceMode(m)}
                        className={tabButtonClass(tradePriceMode === m)}
                      >
                        {m}
                      </button>
                    ))}
                  </div>
                </div>
                <div>
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Custom price</div>
                  <input
                    value={tradeCustomPrice}
                    onChange={(e) => setTradeCustomPrice(e.target.value)}
                    disabled={tradePriceMode !== 'CUSTOM'}
                    placeholder="$0.50"
                    className="mt-1 w-full bg-black/50 border border-grey/20 px-2 py-1 text-[12px] font-mono text-white tabular-nums disabled:opacity-40"
                  />
                </div>
              </div>
            </div>

            <div className="flex items-center justify-between">
              <div className="text-[10px] font-mono text-grey/80 truncate">
                {marketSlugForBook ? `market=${marketSlugForBook}` : ''}
                {outcomeForBook ? ` • outcome=${outcomeForBook}` : ''}
              </div>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => {
                    setTradeArmed((v) => !v);
                    setTradeStatus(null);
                  }}
                  className={tabButtonClass(tradeArmed)}
                >
                  {tradeArmed ? 'ARMED' : 'ARM'}
                </button>
                <button
                  type="button"
                  disabled={!tradingEnabled || !tradeArmed}
                  onClick={async () => {
                    setTradeStatus(null);
                    setTradeArmed(false);

                    const parsePrice = (raw: string): number | undefined => {
                      const s = (raw || '').trim();
                      if (!s) return undefined;
                      const cleaned = s.replace(/[$,\s]/g, '');
                      const v = Number(cleaned);
                      return Number.isFinite(v) ? v : undefined;
                    };

                    try {
                      const resp = await api.placeTradeOrder({
                        signal_id: signal.id,
                        market_slug: marketSlugForBook ?? undefined,
                        outcome: outcomeForBook ?? undefined,
                        side: tradeSide,
                        notional_usd: tradeNotionalUsd,
                        order_type: tradeOrderType,
                        price_mode: tradePriceMode,
                        limit_price: tradePriceMode === 'CUSTOM' ? parsePrice(tradeCustomPrice) : undefined,
                      });
                      setTradeStatus(resp.message);
                    } catch (e: any) {
                      setTradeStatus(e?.message ?? 'Trade request failed');
                    }
                  }}
                  className="border border-grey/20 px-3 py-1 text-[11px] font-mono text-grey/80 hover:text-white hover:border-grey/40 disabled:opacity-40"
                >
                  [SUBMIT]
                </button>
              </div>
            </div>

            {tradeStatus && <div className="text-[11px] font-mono text-grey/80">{tradeStatus}</div>}
          </div>
        )}

        {activeTab === 'PERFORMANCE' && (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="min-w-0">
                <div className="text-[11px] font-mono text-grey/80">Equity curves (realized PnL-to-date, USD)</div>
                {walletAnalytics && (
                  <div className="mt-1 text-[10px] font-mono text-grey/80">
                    updated {analyticsAgeMs !== null ? `${Math.max(0, Math.round(analyticsAgeMs / 1000))}s` : '—'} ago
                    {analyticsSource ? ` • ${analyticsSource}` : ''}
                    {analyticsIsStale ? ' • stale' : ''}
                    {analyticsError ? ' • degraded' : ''}
                  </div>
                )}
              </div>
              <button
                type="button"
                className="text-[11px] font-mono border border-grey/20 px-2 py-1 text-grey/80 hover:text-white hover:border-grey/40"
                disabled={!canFetchAnalytics || analyticsLoading}
                onClick={() => fetchWalletAnalytics(true)}
              >
                [{analyticsError ? 'RETRY' : 'REFRESH'}]
              </button>
            </div>

            {/* Friction mode selector */}
            <div className="flex items-center gap-2">
              <div className="text-[10px] font-mono text-grey/80">FRICTION:</div>
              {(['optimistic', 'base', 'pessimistic'] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setFrictionMode(mode)}
                  className={`px-2 py-0.5 text-[10px] font-mono transition-colors ${
                    frictionMode === mode
                      ? 'bg-white text-black'
                      : 'text-grey/80 hover:text-white'
                  }`}
                >
                  {mode.toUpperCase()}
                </button>
              ))}
            </div>

            {/* Copy curve model selector */}
            <div className="flex items-center gap-2">
              <div className="text-[10px] font-mono text-grey/80">COPY CURVE:</div>
              {(['scaled', 'mtm'] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setCopyModel(mode)}
                  className={`px-2 py-0.5 text-[10px] font-mono transition-colors ${
                    copyModel === mode
                      ? 'bg-white text-black'
                      : 'text-grey/80 hover:text-white'
                  }`}
                >
                  {mode.toUpperCase()}
                </button>
              ))}
              {copyModel === 'mtm' && (
                <div className="text-[10px] font-mono text-grey/60">(slower)</div>
              )}
            </div>

            {!canFetchAnalytics && <div className="text-[11px] text-grey/80 font-mono">No wallet address</div>}
            {analyticsError && (
              <div
                className={`text-[11px] font-mono ${
                  analyticsError.toLowerCase().includes('still computing')
                    ? 'text-grey/80'
                    : 'text-danger'
                }`}
              >
                {analyticsError}
              </div>
            )}

            {/* Never blank: show a fixed skeleton container while loading/no-data */}
            {!walletAnalytics && (
              <div className="space-y-2">
                <div className="bg-white/5 rounded p-2">
                  <div className="h-[10px] w-24 bg-white/10 rounded animate-pulse" />
                  <div className="mt-2 h-[36px] w-[210px] bg-white/10 rounded animate-pulse" />
                  <div className="mt-2 h-[10px] w-full bg-white/10 rounded animate-pulse" />
                </div>
                <div className="bg-white/5 rounded p-2">
                  <div className="h-[10px] w-40 bg-white/10 rounded animate-pulse" />
                  <div className="mt-2 h-[36px] w-[210px] bg-white/10 rounded animate-pulse" />
                  <div className="mt-2 h-[10px] w-full bg-white/10 rounded animate-pulse" />
                </div>
                {analyticsLoading && <div className="text-[11px] text-grey/80 font-mono">Loading…</div>}
              </div>
            )}

            {walletAnalytics && (
              <div className="space-y-2">
                {/* Wallet curve */}
                <div className="bg-white/5 rounded p-2">
                  <div className="flex items-center justify-between">
                    <div className="text-[9px] text-better-blue-light/90 uppercase">Wallet</div>
                    <div className="text-[9px] font-mono text-grey/80">{walletAnalytics.lookback_days}D</div>
                  </div>
                  <div className="mt-1 flex items-center gap-2">
                    <div className="w-[54px] text-right text-[9px] font-mono text-grey/80">
                      {typeof walletMM.min === 'number' ? formatPnL(walletMM.min) : ''}
                    </div>
                    {walletSeries.length >= 2 ? (
                      <Sparkline values={walletSeries} />
                    ) : (
                      <div className="h-[36px] w-[210px] bg-white/10 rounded" />
                    )}
                    <div className="w-[54px] text-left text-[9px] font-mono text-grey/80">
                      {typeof walletMM.max === 'number' ? formatPnL(walletMM.max) : ''}
                    </div>
                  </div>
                  <div className="mt-1 flex items-center justify-between text-[9px] font-mono text-grey/80">
                    <span>{formatDayLabel(walletCurvePoints[0]?.timestamp)}</span>
                    <span>{formatDayLabel(walletCurvePoints[walletCurvePoints.length - 1]?.timestamp)}</span>
                  </div>
                  <div className="mt-2 grid grid-cols-3 gap-2">
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">ROE</div>
                      <div
                        className={`text-[11px] font-mono tabular-nums ${metricColorClass(walletAnalytics.wallet_roe_pct)}`}
                      >
                        {formatPct(walletAnalytics.wallet_roe_pct, 1)}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">WR</div>
                      <div className="text-[11px] font-mono text-white tabular-nums">{formatWinRate(walletAnalytics.wallet_win_rate)}</div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">Profit Factor</div>
                      <div className="text-[11px] font-mono text-white tabular-nums">{formatProfitFactor(walletAnalytics.wallet_profit_factor)}</div>
                    </div>
                  </div>
                </div>

                {/* Copy curve */}
                <div className="bg-white/5 rounded p-2">
                  <div className="flex items-center justify-between">
                    <div className="text-[9px] text-better-blue-light/90 uppercase">
                      Copy {copyModel.toUpperCase()} (${walletAnalytics.fixed_buy_notional_usd}/order)
                    </div>
                    <div className="text-[9px] font-mono text-grey/80">{walletAnalytics.lookback_days}D</div>
                  </div>
                  <div className="mt-1 flex items-center gap-2">
                    <div className="w-[54px] text-right text-[9px] font-mono text-grey/80">
                      {typeof copyMM.min === 'number' ? formatPnL(copyMM.min) : ''}
                    </div>
                    {copySeries.length >= 2 ? (
                      <Sparkline values={copySeries} />
                    ) : (
                      <div className="h-[36px] w-[210px] bg-white/10 rounded" />
                    )}
                    <div className="w-[54px] text-left text-[9px] font-mono text-grey/80">
                      {typeof copyMM.max === 'number' ? formatPnL(copyMM.max) : ''}
                    </div>
                  </div>
                  <div className="mt-1 flex items-center justify-between text-[9px] font-mono text-grey/80">
                    <span>{formatDayLabel(copyCurvePoints[0]?.timestamp)}</span>
                    <span>{formatDayLabel(copyCurvePoints[copyCurvePoints.length - 1]?.timestamp)}</span>
                  </div>
                  <div className="mt-2 grid grid-cols-3 gap-2">
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">ROE</div>
                      <div className={`text-[11px] font-mono tabular-nums ${metricColorClass(walletAnalytics.copy_roe_pct)}`}>
                        {formatPct(walletAnalytics.copy_roe_pct, 1)}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">WR</div>
                      <div className="text-[11px] font-mono text-white tabular-nums">{formatWinRate(walletAnalytics.copy_win_rate)}</div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">Profit Factor</div>
                      <div className="text-[11px] font-mono text-white tabular-nums">{formatProfitFactor(walletAnalytics.copy_profit_factor)}</div>
                    </div>
                  </div>
                </div>

                {/* Friction stats */}
                <div className="bg-white/5 rounded p-2 mt-2">
                  <div className="flex items-center justify-between">
                    <div className="text-[9px] text-better-blue-light/90 uppercase">
                      Execution Costs (assumed)
                    </div>
                    <div className="text-[9px] font-mono text-grey/80">
                      {(walletAnalytics.copy_friction_mode?.toUpperCase() || 'BASE') + ' • '}
                      {walletAnalytics.copy_friction_pct_per_trade?.toFixed(2) || '1.00'}% per fill
                    </div>
                  </div>
                  <div className="mt-1 grid grid-cols-2 gap-2">
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">Total Costs</div>
                      <div className="text-[11px] font-mono text-warning tabular-nums">
                        {typeof walletAnalytics.copy_total_friction_usd === 'number'
                          ? `-$${walletAnalytics.copy_total_friction_usd.toFixed(2)}`
                          : '---'}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/90">Fills</div>
                      <div className="text-[11px] font-mono text-white tabular-nums">
                        {walletAnalytics.copy_trade_count ?? '---'}
                      </div>
                    </div>
                  </div>
                  <div className="mt-1 text-[9px] font-mono text-grey/70">
                    Net copy curve includes these costs.
                  </div>
                </div>

                <div className="text-[9px] font-mono text-grey/80">
                  ROE denom ≈ {walletAnalytics.copy_roe_denom_usd ? formatVolumeCompact(walletAnalytics.copy_roe_denom_usd) : '---'} gross buys
                </div>
              </div>
            )}
          </div>
        )}

        {activeTab === 'BOOK' && (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="text-[11px] font-mono text-grey/80">Orderbook snapshot (now)</div>
              <button
                type="button"
                className="text-[11px] font-mono border border-grey/20 px-2 py-1 text-grey/80 hover:text-white hover:border-grey/40"
                disabled={!canFetchBook || bookLoading}
                onClick={() => fetchBook(true)}
              >
                [REFRESH]
              </button>
            </div>

            {!canFetchBook && (
              <div className="text-[11px] text-grey/80 font-mono">Missing token_id or (market_slug, outcome)</div>
            )}
            {bookError && <div className="text-[11px] text-danger font-mono">{bookError}</div>}
            {bookLoading && <div className="text-[11px] text-grey/80 font-mono">Loading…</div>}

            {bookData && (
              <div className="space-y-2">
                <div className="grid grid-cols-4 gap-2">
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90">Bid</div>
                    <div className="text-[13px] font-mono text-success tabular-nums">
                      {typeof bookData.best_bid === 'number' ? formatPrice(bookData.best_bid) : '---'}
                    </div>
                  </div>
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90">Ask</div>
                    <div className="text-[13px] font-mono text-danger tabular-nums">
                      {typeof bookData.best_ask === 'number' ? formatPrice(bookData.best_ask) : '---'}
                    </div>
                  </div>
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90">Spr</div>
                    <div className="text-[13px] font-mono text-white tabular-nums">
                      {typeof bookData.spread === 'number' ? formatPrice(bookData.spread) : '---'}
                    </div>
                  </div>
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90">Imb</div>
                    <div className="text-[13px] font-mono text-white tabular-nums">
                      {typeof bookData.imbalance_10bps === 'number' ? bookData.imbalance_10bps.toFixed(2) : '---'}
                    </div>
                  </div>
                </div>

                <div className="grid grid-cols-2 gap-2">
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90 uppercase mb-1">Top bids</div>
                    <div className="space-y-1">
                      {(bookDepth?.bids ?? bookData.bids.slice(0, 12).map((b) => ({ ...b, cum: b.size }))).map((b, idx) => {
                        const pct = bookDepth ? Math.min(100, (b.cum / bookDepth.maxCum) * 100) : 0;
                        return (
                          <div
                            key={idx}
                            className="relative flex justify-between text-[11px] font-mono tabular-nums overflow-hidden rounded px-1"
                          >
                            <div className="absolute inset-y-0 left-0 bg-success/10" style={{ width: `${pct}%` }} />
                            <span className="relative z-10 text-success">{formatPrice(b.price)}</span>
                            <span className="relative z-10 text-grey/80">{formatVolumeCompact(b.size)}</span>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                  <div className="bg-white/5 rounded p-2">
                    <div className="text-[9px] text-better-blue-light/90 uppercase mb-1">Top asks</div>
                    <div className="space-y-1">
                      {(bookDepth?.asks ?? bookData.asks.slice(0, 12).map((a) => ({ ...a, cum: a.size }))).map((a, idx) => {
                        const pct = bookDepth ? Math.min(100, (a.cum / bookDepth.maxCum) * 100) : 0;
                        return (
                          <div
                            key={idx}
                            className="relative flex justify-between text-[11px] font-mono tabular-nums overflow-hidden rounded px-1"
                          >
                            <div className="absolute inset-y-0 right-0 bg-danger/10" style={{ width: `${pct}%` }} />
                            <span className="relative z-10 text-danger">{formatPrice(a.price)}</span>
                            <span className="relative z-10 text-grey/80">{formatVolumeCompact(a.size)}</span>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
};
