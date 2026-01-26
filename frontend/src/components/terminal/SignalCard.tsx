import React, { memo, useCallback, useEffect, useMemo, useState } from 'react';
import { Signal, MarketSnapshotResponse, WalletAnalyticsResponse } from '../../types/signal';
import { api } from '../../services/api';
import {
  getSignalLabel,
  formatConfidence,
  formatPrice,
  formatVolume,
  formatTimeAgo,
  formatTimestamp,
  cleanMarketName,
  formatPnL,
  formatVolumeCompact,
} from '../../utils/formatters';

const walletAnalyticsCache = new Map<string, WalletAnalyticsResponse>();
type InFlight<T> = { startedAtMs: number; promise: Promise<T> };
const walletAnalyticsInFlight = new Map<string, InFlight<WalletAnalyticsResponse>>();
const INFLIGHT_WALLET_TTL_MS = 35_000;

const bookCache = new Map<string, MarketSnapshotResponse>();
const bookInFlight = new Map<string, InFlight<MarketSnapshotResponse>>();
const INFLIGHT_BOOK_TTL_MS = 12_000;

function normalizeMarketTitle(raw: string): string {
  const s = (raw || '').replace(/\s+/g, ' ').trim();
  if (!s) return '';

  // Some backend paths historically stuffed a *headline* into `details.market_title` like:
  // "TRACKED WALLET ENTRY: ~$1 BUY on 'Bitcoin Up or Down - ...' by 0x..."
  // Extract the quoted market title to keep the UI stable.
  const quoted = s.match(/\bon\s+['"]([^'"]+)['"]/i);
  if (quoted?.[1]) return quoted[1].replace(/\s+/g, ' ').trim();

  return s;
}

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

const Sparkline: React.FC<{ values: number[]; width?: number; height?: number }> = ({
  values,
  width = 140,
  height = 32,
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

interface SignalCardProps {
  signal: Signal;
}

const SignalCardComponent: React.FC<SignalCardProps> = ({ signal }) => {
  const [showDetails, setShowDetails] = useState(false);
  const [showMarketPanel, setShowMarketPanel] = useState(false);
  const [showWalletPanel, setShowWalletPanel] = useState(false);
  const [showChart, setShowChart] = useState(false);
  const [showBook, setShowBook] = useState(false);
  const [bookData, setBookData] = useState<MarketSnapshotResponse | null>(null);
  const [bookLoading, setBookLoading] = useState(false);
  const [bookError, setBookError] = useState<string | null>(null);

  const [walletAnalytics, setWalletAnalytics] = useState<WalletAnalyticsResponse | null>(null);
  const [analyticsLoading, setAnalyticsLoading] = useState(false);
  const [analyticsError, setAnalyticsError] = useState<string | null>(null);

  const signalLabel = getSignalLabel(signal.signal_type.type);
  const timestamp = formatTimestamp(signal.detected_at);
  const timeAgo = formatTimeAgo(signal.detected_at);
  
  const getWalletForLink = () => {
    if (signal.signal_type.type === 'TrackedWalletEntry') return signal.signal_type.wallet_address;
    if (signal.signal_type.type === 'WhaleFollowing') return signal.signal_type.whale_address;
    if (signal.signal_type.type === 'EliteWallet') return signal.signal_type.wallet_address;
    if (signal.signal_type.type === 'InsiderWallet') return signal.signal_type.wallet_address;
    return null;
  };
  
  const walletForLink = getWalletForLink();

  let positionDisplay = '---';
  if (signal.signal_type.type === 'TrackedWalletEntry') {
    if (signal.signal_type.position_value_usd > 0) {
      positionDisplay = formatVolume(signal.signal_type.position_value_usd);
    }
  } else if (signal.details.recommended_size) {
    positionDisplay = formatVolume(signal.details.recommended_size);
  } else if (signal.signal_type.type === 'WhaleFollowing') {
    positionDisplay = formatVolume(signal.signal_type.position_size);
  } else if (signal.signal_type.type === 'EliteWallet' || signal.signal_type.type === 'InsiderWallet') {
    positionDisplay = formatVolume(signal.signal_type.position_size);
  }

  const actionText = signal.details.recommended_action.toUpperCase();
  const isBuy = actionText.includes('BUY');
  const isSell = actionText.includes('SELL');
  const price = signal.details.current_price;
  
  const getOutcomeText = () => {
    if (signal.signal_type.type === 'TrackedWalletEntry' && signal.signal_type.token_label) {
      const label = signal.signal_type.token_label.toUpperCase();
      if (label === 'UP' || label === 'DOWN') return label;
      if (label === 'YES' || label === 'NO') return label;
      return label;
    }
    
    const slug = signal.market_slug?.toLowerCase() || '';
    const isUpDownMarket = slug.includes('updown') || 
                           slug.includes('up-or-down') || 
                           slug.includes('up-down');
    
    if (isUpDownMarket) {
      return price >= 0.5 ? 'UP' : 'DOWN';
    }
    
    return price >= 0.5 ? 'YES' : 'NO';
  };
  
  const outcomeText = getOutcomeText();
  const walletAddress = walletForLink;

  // Enrichment context
  const ctx = signal.context;
  const ctxStatus = signal.context_status;
  const derived = ctx?.derived;
  const hasContext = ctx && (ctxStatus === 'ok' || ctxStatus === 'partial');

  // Price context
  const ctxDeltaBps = derived?.price_delta_bps;
  const ctxDeltaAbs = derived?.price_delta_abs;
  const ctxSpread = derived?.spread_at_entry ?? ctx?.orderbook?.spread;
  const ctxLatestPrice = ctx?.price?.latest?.price;

  // PnL context
  const pnl7d = derived?.pnl_7d;
  const pnl14d = derived?.pnl_14d;
  const pnl30d = derived?.pnl_30d;
  const pnl90d = derived?.pnl_90d;
  const hasPnL = pnl7d !== undefined || pnl14d !== undefined || pnl30d !== undefined || pnl90d !== undefined;

  // Sharpe-like ratios
  const sharpe7d = derived?.sharpe_7d;
  const sharpe14d = derived?.sharpe_14d;
  const sharpe30d = derived?.sharpe_30d;
  const sharpe90d = derived?.sharpe_90d;
  const hasSharpe =
    sharpe7d !== undefined || sharpe14d !== undefined || sharpe30d !== undefined || sharpe90d !== undefined;

  // Wallet trade stats
  const avgTrade24h = derived?.avg_trade_value_24h;
  const tradeCount24h = derived?.trade_count_24h;
  const hasTradeStats = avgTrade24h !== undefined || tradeCount24h !== undefined;

  // Market context
  const marketMeta = ctx?.market;
  const marketVolume = marketMeta?.volume_total ?? marketMeta?.volume_1_week;

  // Orderbook context
  const orderbook = ctx?.orderbook;
  const bestBid = orderbook?.best_bid;
  const bestAsk = orderbook?.best_ask;

  const marketTitle = useMemo(() => {
    // Prefer signal.details.market_title for stability (ctx arrives async and can cause title flicker).
    const rawTitle = normalizeMarketTitle(signal.details.market_title || ctx?.order?.title || '');
    const slug = (signal.market_slug || '').toLowerCase();

    const isUpDown =
      slug.includes('updown') || slug.includes('up-or-down') || slug.includes('up-down') || rawTitle.includes('Up or Down');

    if (!rawTitle) {
      if (!signal.market_slug) return 'Unknown Market';

      if (isUpDown) {
        // Example: btc-updown-15m-1765860300 -> "BTC Up/Down 15m"
        const parts = signal.market_slug.split('-').filter(Boolean);
        const asset = (parts[0] || 'MARKET').toUpperCase();
        const duration = parts.find((p) => /^(\d+)(m|h|d)$/.test(p)) || '';
        return `${asset} Up/Down${duration ? ` ${duration}` : ''}`;
      }

      return cleanMarketName(signal.market_slug);
    }

    if (!isUpDown) return rawTitle;

    // Example: "Bitcoin Up or Down - December 15, 11:45PM-12:00AM ET" -> "BTC Up/Down 11:45PM-12:00AM ET"
    const parts = rawTitle.split(' Up or Down - ');
    if (parts.length >= 2) {
      const assetRaw = parts[0].trim();
      const rest = parts.slice(1).join(' Up or Down - ').trim();
      const asset =
        assetRaw.toLowerCase() === 'bitcoin'
          ? 'BTC'
          : assetRaw.toLowerCase() === 'ethereum'
            ? 'ETH'
            : assetRaw;
      const timePart = (rest.includes(',') ? rest.split(',').pop() : rest)?.trim();
      return `${asset} Up/Down ${timePart || ''}`.trim();
    }

    // Fallback to slug-derived cleaned title
    return rawTitle;
  }, [ctx?.order?.title, signal.details.market_title, signal.market_slug]);

  const pnlColor = (val: number | undefined) => {
    if (val === undefined) return 'text-fg/90';
    if (val > 0) return 'text-success';
    if (val < 0) return 'text-danger';
    return 'text-fg';
  };

  // CLOB /book expects an outcome-level `clobTokenId` (a large integer string).
  // If we already have a numeric token id, use it directly (fast path). Otherwise fall back to
  // (market_slug, outcome) so the backend can resolve clobTokenId via Gamma.
  const marketSlugForBook = signal.market_slug || ctx?.order?.market_slug;
  const outcomeForBook =
    (ctx?.order?.token_label as string | undefined) ||
    ((signal.signal_type as any)?.token_label as string | undefined);

  const conditionId = ctx?.order?.condition_id;
  const clobTokenIdForBook = (ctx?.order?.token_id as string | undefined) || (signal.details?.market_id as string);
  const hasNumericClobTokenId = typeof clobTokenIdForBook === 'string' && /^[0-9]+$/.test(clobTokenIdForBook);
  const canFetchBook = Boolean(hasNumericClobTokenId || (marketSlugForBook && outcomeForBook));

  const pnlSeries = useMemo(() => {
    const arr = ctx?.wallet_pnl?.pnl_over_time;
    if (!Array.isArray(arr)) return [] as number[];
    return arr
      .map((p: any) => p?.pnl_to_date)
      .filter((v: any) => typeof v === 'number' && Number.isFinite(v))
      .slice(-90);
  }, [ctx?.wallet_pnl]);

  const walletCurvePoints = useMemo(() => {
    const points = walletAnalytics?.wallet_realized_curve;
    if (Array.isArray(points) && points.length >= 2) return points.slice(-90);

    const arr = ctx?.wallet_pnl?.pnl_over_time;
    if (!Array.isArray(arr) || arr.length < 2) return [] as { timestamp: number; value: number }[];

    return arr
      .map((p: any) => ({ timestamp: p?.timestamp, value: p?.pnl_to_date }))
      .filter((p: any) => typeof p.timestamp === 'number' && typeof p.value === 'number')
      .slice(-90);
  }, [walletAnalytics?.wallet_realized_curve, ctx?.wallet_pnl]);

  const walletCurveSeries = useMemo(() => {
    if (walletCurvePoints.length < 2) return pnlSeries;
    return walletCurvePoints.map((p) => p.value).filter((v) => Number.isFinite(v));
  }, [walletCurvePoints, pnlSeries]);

  const copyCurvePoints = useMemo(() => {
    const points = walletAnalytics?.copy_trade_curve;
    if (!Array.isArray(points) || points.length < 2) return [] as { timestamp: number; value: number }[];
    return points.slice(-90);
  }, [walletAnalytics?.copy_trade_curve]);

  const copyCurveSeries = useMemo(() => {
    if (copyCurvePoints.length < 2) return [] as number[];
    return copyCurvePoints.map((p) => p.value).filter((v) => Number.isFinite(v));
  }, [copyCurvePoints]);

  const walletCurveMeta = useMemo(() => {
    if (walletCurvePoints.length < 2) return null;
    const mm = curveMinMax(walletCurvePoints);
    const start = walletCurvePoints[0]?.timestamp;
    const end = walletCurvePoints[walletCurvePoints.length - 1]?.timestamp;
    const last = walletCurvePoints[walletCurvePoints.length - 1]?.value;
    return { ...mm, start, end, last };
  }, [walletCurvePoints]);

  const copyCurveMeta = useMemo(() => {
    if (copyCurvePoints.length < 2) return null;
    const mm = curveMinMax(copyCurvePoints);
    const start = copyCurvePoints[0]?.timestamp;
    const end = copyCurvePoints[copyCurvePoints.length - 1]?.timestamp;
    const last = copyCurvePoints[copyCurvePoints.length - 1]?.value;
    return { ...mm, start, end, last };
  }, [copyCurvePoints]);

  const bookCumulative = useMemo(() => {
    if (!bookData) return null;

    const bidCum: number[] = [];
    const askCum: number[] = [];

    let running = 0;
    for (const l of bookData.bids) {
      running += l.price * l.size;
      bidCum.push(running);
    }

    running = 0;
    for (const l of bookData.asks) {
      running += l.price * l.size;
      askCum.push(running);
    }

    const max = Math.max(1, ...bidCum, ...askCum);
    return { bidCum, askCum, maxNotional: max };
  }, [bookData]);

  const canFetchAnalytics = Boolean(walletAddress);

  const fetchWalletAnalytics = useCallback(
    async (force: boolean = false) => {
      if (!walletAddress) return;

      const key = walletAddress.toLowerCase();
      if (!force) {
        const cached = walletAnalyticsCache.get(key);
        if (cached) {
          setWalletAnalytics(cached);
          return;
        }
      }

      setAnalyticsLoading(true);
      setAnalyticsError(null);
      try {
        const nowMs = Date.now();
        const existing = walletAnalyticsInFlight.get(key);
        if (!existing || force || nowMs - existing.startedAtMs > INFLIGHT_WALLET_TTL_MS) {
          const promise = api.getWalletAnalytics(walletAddress, force);
          walletAnalyticsInFlight.set(key, { startedAtMs: nowMs, promise });
        }
        const resp = await walletAnalyticsInFlight.get(key)!.promise;
        walletAnalyticsCache.set(key, resp);
        walletAnalyticsInFlight.delete(key);
        setWalletAnalytics(resp);
      } catch (e: any) {
        walletAnalyticsInFlight.delete(key);
        setAnalyticsError(e?.message ?? 'Failed to fetch wallet analytics');
      } finally {
        setAnalyticsLoading(false);
      }
    },
    [walletAddress]
  );

  const fetchBook = useCallback(async () => {
    if (!canFetchBook) return;

    const key = hasNumericClobTokenId
      ? `token:${clobTokenIdForBook}:10`
      : `${marketSlugForBook!.toLowerCase()}:${outcomeForBook!.toLowerCase()}:10`;
    const cached = bookCache.get(key);
    if (cached) {
      const nowSec = Math.floor(Date.now() / 1000);
      if (nowSec - cached.fetched_at <= 2) {
        setBookData(cached);
        return;
      }
    }

    setBookLoading(true);
    setBookError(null);
    try {
      const nowMs = Date.now();
      const existing = bookInFlight.get(key);
      if (!existing || nowMs - existing.startedAtMs > INFLIGHT_BOOK_TTL_MS) {
        const promise = hasNumericClobTokenId
          ? api.getMarketSnapshot(clobTokenIdForBook, 10)
          : api.getMarketSnapshotBySlug(marketSlugForBook!, outcomeForBook!, 10);
        bookInFlight.set(key, { startedAtMs: nowMs, promise });
      }
      const snapshot = await bookInFlight.get(key)!.promise;
      bookCache.set(key, snapshot);
      bookInFlight.delete(key);
      setBookData(snapshot);
    } catch (e: any) {
      bookInFlight.delete(key);
      setBookError(e?.message ?? 'Failed to fetch orderbook');
    } finally {
      setBookLoading(false);
    }
  }, [canFetchBook, hasNumericClobTokenId, clobTokenIdForBook, marketSlugForBook, outcomeForBook]);

  useEffect(() => {
    if (!showChart) return;
    if (!canFetchAnalytics) return;
    if (walletAnalytics || analyticsLoading) return;
    fetchWalletAnalytics(false);
  }, [showChart, canFetchAnalytics, walletAnalytics, analyticsLoading, fetchWalletAnalytics]);

  useEffect(() => {
    if (!showBook) return;
    if (!canFetchBook) return;
    if (bookData || bookLoading) return;
    fetchBook();
  }, [showBook, canFetchBook, bookData, bookLoading, fetchBook]);

  const copyToClipboard = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Ignore
    }
  }, []);

  return (
    <div
      className="block bg-void border border-grey/20 m-2 p-3 transition-colors duration-150 hover:border-better-blue"
    >
      {/* Compact Header */}
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <span className="text-[10px] font-mono text-better-blue-lavender font-bold">[{signalLabel}]</span>
          <span className="text-[10px] font-mono text-better-blue-lavender">{timestamp}</span>
          {walletAddress && (
            <span className="text-[10px] font-mono text-better-blue-lavender truncate max-w-[120px]">
              {walletAddress.slice(0, 6)}...{walletAddress.slice(-4)}
            </span>
          )}
        </div>
        <span className="text-sm font-mono font-bold text-fg">{formatConfidence(signal.confidence)}</span>
      </div>

      {/* Market Title */}
      <div className="mb-3 font-mono text-fg text-sm">
        {marketTitle}
      </div>

      {/* Core Metrics - Always Shown */}
      <div className="grid grid-cols-4 gap-3 p-2 bg-fg/5 rounded">
        <div>
          <div className="text-[10px] text-better-blue-light/70 uppercase tracking-wide">Price</div>
          <div className="font-mono text-fg text-sm mt-0.5">{formatPrice(price)}</div>
        </div>
        <div>
          <div className="text-[10px] text-better-blue-light/70 uppercase tracking-wide">Size</div>
          <div className="font-mono text-fg text-sm mt-0.5">{positionDisplay}</div>
        </div>
        <div>
          <div className="text-[10px] text-better-blue-light/70 uppercase tracking-wide">Side</div>
          <div className="font-mono text-better-blue-light text-sm font-semibold mt-0.5">{outcomeText}</div>
        </div>
        <div>
          <div className="text-[10px] text-better-blue-light/70 uppercase tracking-wide">Action</div>
          <div className={`font-mono text-sm font-semibold mt-0.5 ${isBuy ? 'text-success' : isSell ? 'text-danger' : 'text-fg'}`}>
            {isBuy ? 'BUY' : isSell ? 'SELL' : actionText}
          </div>
        </div>
      </div>

      {/* Context Section - Only show when data is available */}
      {hasContext && (
        <div className="border-t border-grey/10 pt-2 mt-2">
          <div className="grid grid-cols-4 gap-2">
            {/* Price Delta */}
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase">Δ Price</div>
              <div className={`font-mono text-xs ${typeof ctxDeltaBps === 'number' ? (ctxDeltaBps > 0 ? 'text-success' : ctxDeltaBps < 0 ? 'text-danger' : 'text-fg') : 'text-fg/80'}`}>
                {typeof ctxDeltaBps === 'number' ? `${ctxDeltaBps > 0 ? '+' : ''}${ctxDeltaBps.toFixed(0)} bps` : '---'}
              </div>
              <div className="font-mono text-[10px] text-better-blue-lavender">
                {typeof ctxDeltaAbs === 'number' && Number.isFinite(ctxDeltaAbs)
                  ? `${ctxDeltaAbs > 0 ? '+' : ''}${(ctxDeltaAbs * 100).toFixed(2)}¢`
                  : ''}
              </div>
            </div>
            {/* Current Price */}
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase">Now</div>
              <div className="font-mono text-xs text-fg">
                {typeof ctxLatestPrice === 'number' ? formatPrice(ctxLatestPrice) : '---'}
              </div>
            </div>
            {/* Spread */}
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase">Spread</div>
              <div className="font-mono text-xs text-fg">
                {typeof ctxSpread === 'number' ? `${(ctxSpread * 100).toFixed(1)}%` : '---'}
              </div>
            </div>
            {/* Volume */}
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase">Volume</div>
              <div className="font-mono text-xs text-fg">
                {typeof marketVolume === 'number' ? formatVolumeCompact(marketVolume) : '---'}
              </div>
            </div>
          </div>

          {/* Bid/Ask Row */}
          {(bestBid !== undefined || bestAsk !== undefined) && (
            <div className="grid grid-cols-2 gap-2 mt-2">
              <div className="flex items-center gap-2">
                <span className="text-[9px] text-better-blue-light/70 uppercase">Bid</span>
                <span className="font-mono text-xs text-success">{typeof bestBid === 'number' ? formatPrice(bestBid) : '---'}</span>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[9px] text-better-blue-light/70 uppercase">Ask</span>
                <span className="font-mono text-xs text-danger">{typeof bestAsk === 'number' ? formatPrice(bestAsk) : '---'}</span>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Wallet PnL Section - Only show when available */}
      {hasPnL && (
        <div className="border-t border-grey/10 pt-2 mt-2">
          <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">Wallet PnL (Realized)</div>
          <div className="grid grid-cols-4 gap-2">
            <div>
              <div className="text-[9px] text-better-blue-light/60">7D</div>
              <div className={`font-mono text-xs font-semibold ${pnlColor(pnl7d)}`}>
                {typeof pnl7d === 'number' ? formatPnL(pnl7d) : '---'}
              </div>
            </div>
            <div>
              <div className="text-[9px] text-better-blue-light/60">14D</div>
              <div className={`font-mono text-xs font-semibold ${pnlColor(pnl14d)}`}>
                {typeof pnl14d === 'number' ? formatPnL(pnl14d) : '---'}
              </div>
            </div>
            <div>
              <div className="text-[9px] text-better-blue-light/60">30D</div>
              <div className={`font-mono text-xs font-semibold ${pnlColor(pnl30d)}`}>
                {typeof pnl30d === 'number' ? formatPnL(pnl30d) : '---'}
              </div>
            </div>
            <div>
              <div className="text-[9px] text-better-blue-light/60">90D</div>
              <div className={`font-mono text-xs font-semibold ${pnlColor(pnl90d)}`}>
                {typeof pnl90d === 'number' ? formatPnL(pnl90d) : '---'}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Details (progressive disclosure) */}
      {showDetails && (
        <div className="border-t border-grey/10 pt-2 mt-2 space-y-2">
          {!hasContext && (
            <div className="text-[10px] font-mono text-better-blue-lavender">
              Enrichment pending…
            </div>
          )}

          {showMarketPanel && (
            <div className="bg-fg/5 rounded p-2">
              <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">Market</div>
              <div className="space-y-1">
                <div className="flex items-center justify-between gap-2">
                  <span className="font-mono text-[10px] text-fg truncate">{marketTitle}</span>
                </div>
                {signal.market_slug && (
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-mono text-[10px] text-better-blue-lavender truncate">{signal.market_slug}</span>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        copyToClipboard(signal.market_slug);
                      }}
                      className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                    >
                      [COPY]
                    </button>
                  </div>
                )}
                {outcomeForBook && (
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-mono text-[10px] text-better-blue-lavender truncate">outcome: {outcomeForBook}</span>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        copyToClipboard(outcomeForBook);
                      }}
                      className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                    >
                      [COPY]
                    </button>
                  </div>
                )}
                {conditionId && (
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-mono text-[10px] text-better-blue-lavender truncate">condition_id: {conditionId}</span>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        copyToClipboard(conditionId);
                      }}
                      className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                    >
                      [COPY]
                    </button>
                  </div>
                )}

                {clobTokenIdForBook && (
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-mono text-[10px] text-better-blue-lavender truncate">
                      clob_token_id: {clobTokenIdForBook}
                    </span>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        copyToClipboard(clobTokenIdForBook);
                      }}
                      className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                    >
                      [COPY]
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}

          {showWalletPanel && walletAddress && (
            <div className="bg-fg/5 rounded p-2">
              <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">Wallet</div>
              <div className="flex items-center justify-between gap-2">
                <span className="font-mono text-[10px] text-better-blue-lavender truncate">{walletAddress}</span>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    copyToClipboard(walletAddress);
                  }}
                  className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                >
                  [COPY]
                </button>
              </div>
            </div>
          )}

          {hasSharpe && (
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">PnL Sharpe (Realized)</div>
              <div className="grid grid-cols-4 gap-2">
                <div>
                  <div className="text-[9px] text-better-blue-light/60">7D</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof sharpe7d === 'number' ? sharpe7d.toFixed(2) : '---'}
                  </div>
                </div>
                <div>
                  <div className="text-[9px] text-better-blue-light/60">14D</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof sharpe14d === 'number' ? sharpe14d.toFixed(2) : '---'}
                  </div>
                </div>
                <div>
                  <div className="text-[9px] text-better-blue-light/60">30D</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof sharpe30d === 'number' ? sharpe30d.toFixed(2) : '---'}
                  </div>
                </div>
                <div>
                  <div className="text-[9px] text-better-blue-light/60">90D</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof sharpe90d === 'number' ? sharpe90d.toFixed(2) : '---'}
                  </div>
                </div>
              </div>
            </div>
          )}

          {hasTradeStats && (
            <div>
              <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">Wallet Trade Stats (24h)</div>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <div className="text-[9px] text-better-blue-light/60">Trades</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof tradeCount24h === 'number' ? tradeCount24h : '---'}
                  </div>
                </div>
                <div>
                  <div className="text-[9px] text-better-blue-light/60">Avg Size</div>
                  <div className="font-mono text-xs text-fg">
                    {typeof avgTrade24h === 'number' ? formatVolume(avgTrade24h) : '---'}
                  </div>
                </div>
              </div>
            </div>
          )}

          {showChart && (
            <div>
              <div className="flex items-center justify-between mb-1">
                <div className="text-[9px] text-better-blue-light/70 uppercase">Equity Curves</div>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    fetchWalletAnalytics(true);
                  }}
                  className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                  disabled={!canFetchAnalytics || analyticsLoading}
                >
                  [REFRESH]
                </button>
              </div>

              {!canFetchAnalytics && <div className="text-[10px] text-fg/80 font-mono">No wallet address</div>}
              {analyticsError && <div className="text-[10px] text-danger font-mono">{analyticsError}</div>}
              {analyticsLoading && <div className="text-[10px] text-fg/80 font-mono">Loading…</div>}

              <div className="space-y-2">
                <div className="bg-fg/5 rounded p-2 flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-[9px] text-better-blue-light/60 uppercase">Wallet (Realized) — USD PnL-to-date</div>
                    <div className="flex items-center gap-2">
                      <div className="w-[46px] text-right text-[9px] font-mono text-fg/90">
                        {typeof walletCurveMeta?.min === 'number' ? formatPnL(walletCurveMeta.min) : ''}
                      </div>
                      <Sparkline values={walletCurveSeries} width={170} height={32} />
                      <div className="w-[46px] text-left text-[9px] font-mono text-fg/90">
                        {typeof walletCurveMeta?.max === 'number' ? formatPnL(walletCurveMeta.max) : ''}
                      </div>
                    </div>
                    <div className="flex items-center justify-between mt-1 text-[9px] font-mono text-fg/90">
                      <span>{formatDayLabel(walletCurveMeta?.start)}</span>
                      <span>{formatDayLabel(walletCurveMeta?.end)}</span>
                    </div>
                  </div>
                  <div className="shrink-0 text-right">
                    <div className="text-[9px] font-mono text-better-blue-lavender">{walletAnalytics?.lookback_days ?? 90}D</div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">ROE</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatPct(walletAnalytics?.wallet_roe_pct, 1)}
                      </div>
                    </div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">WR</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatWinRate(walletAnalytics?.wallet_win_rate)}
                      </div>
                    </div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">Profit Factor</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatProfitFactor(walletAnalytics?.wallet_profit_factor)}
                      </div>
                    </div>
                  </div>
                </div>

                <div className="bg-fg/5 rounded p-2 flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-[9px] text-better-blue-light/60 uppercase">
                      Copy (Fixed ${walletAnalytics?.fixed_buy_notional_usd ?? 1}/ORDER) — USD PnL-to-date
                    </div>
                    <div className="flex items-center gap-2">
                      <div className="w-[46px] text-right text-[9px] font-mono text-fg/90">
                        {typeof copyCurveMeta?.min === 'number' ? formatPnL(copyCurveMeta.min) : ''}
                      </div>
                      <Sparkline values={copyCurveSeries} width={170} height={32} />
                      <div className="w-[46px] text-left text-[9px] font-mono text-fg/90">
                        {typeof copyCurveMeta?.max === 'number' ? formatPnL(copyCurveMeta.max) : ''}
                      </div>
                    </div>
                    <div className="flex items-center justify-between mt-1 text-[9px] font-mono text-fg/90">
                      <span>{formatDayLabel(copyCurveMeta?.start)}</span>
                      <span>{formatDayLabel(copyCurveMeta?.end)}</span>
                    </div>
                  </div>
                  <div className="shrink-0 text-right">
                    <div className="text-[9px] font-mono text-better-blue-lavender">{walletAnalytics?.lookback_days ?? 90}D</div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">ROE</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatPct(walletAnalytics?.copy_roe_pct, 1)}
                      </div>
                    </div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">WR</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatWinRate(walletAnalytics?.copy_win_rate)}
                      </div>
                    </div>
                    <div className="mt-1">
                      <div className="text-[9px] text-better-blue-light/60">Profit Factor</div>
                      <div className="text-[10px] font-mono text-fg">
                        {formatProfitFactor(walletAnalytics?.copy_profit_factor)}
                      </div>
                    </div>
                  </div>
                </div>

                {walletAnalytics?.copy_roe_denom_usd && (
                  <div className="text-[9px] font-mono text-fg/80">
                    ROE denom ≈ {formatVolumeCompact(walletAnalytics.copy_roe_denom_usd)} gross buys (copy)
                    {walletAnalytics.wallet_roe_denom_usd
                      ? ` • ${formatVolumeCompact(walletAnalytics.wallet_roe_denom_usd)} gross buys (wallet)`
                      : ''}
                  </div>
                )}

                {walletAnalytics && (
                  <div>
                    <div className="text-[9px] text-better-blue-light/70 uppercase mb-1">Copy Sharpe (Realized)</div>
                    <div className="grid grid-cols-4 gap-2">
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">7D</div>
                        <div className="font-mono text-xs text-fg">
                          {typeof walletAnalytics.copy_sharpe_7d === 'number'
                            ? walletAnalytics.copy_sharpe_7d.toFixed(2)
                            : '---'}
                        </div>
                      </div>
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">14D</div>
                        <div className="font-mono text-xs text-fg">
                          {typeof walletAnalytics.copy_sharpe_14d === 'number'
                            ? walletAnalytics.copy_sharpe_14d.toFixed(2)
                            : '---'}
                        </div>
                      </div>
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">30D</div>
                        <div className="font-mono text-xs text-fg">
                          {typeof walletAnalytics.copy_sharpe_30d === 'number'
                            ? walletAnalytics.copy_sharpe_30d.toFixed(2)
                            : '---'}
                        </div>
                      </div>
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">90D</div>
                        <div className="font-mono text-xs text-fg">
                          {typeof walletAnalytics.copy_sharpe_90d === 'number'
                            ? walletAnalytics.copy_sharpe_90d.toFixed(2)
                            : '---'}
                        </div>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            </div>
          )}

          {showBook && (
            <div>
              <div className="flex items-center justify-between mb-1">
                <div className="text-[9px] text-better-blue-light/70 uppercase">Orderbook (Now)</div>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    fetchBook();
                  }}
                  className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
                  disabled={!canFetchBook || bookLoading}
                >
                  [REFRESH]
                </button>
              </div>

              {!canFetchBook && (
                <div className="text-[10px] text-fg/80 font-mono">Missing token_id or (market_slug, outcome)</div>
              )}
              {bookError && <div className="text-[10px] text-danger font-mono">{bookError}</div>}
              {bookLoading && <div className="text-[10px] text-fg/80 font-mono">Loading…</div>}

              {bookData && (
                <div className="bg-fg/5 rounded p-2">
                  <div className="grid grid-cols-3 gap-2 mb-2">
                    <div>
                      <div className="text-[9px] text-better-blue-light/60">Bid</div>
                      <div className="font-mono text-xs text-success">
                        {typeof bookData.best_bid === 'number' ? formatPrice(bookData.best_bid) : '---'}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/60">Ask</div>
                      <div className="font-mono text-xs text-danger">
                        {typeof bookData.best_ask === 'number' ? formatPrice(bookData.best_ask) : '---'}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/60">Imb (10bps)</div>
                      <div className="font-mono text-xs text-fg">
                        {typeof bookData.imbalance_10bps === 'number' ? bookData.imbalance_10bps.toFixed(2) : '---'}
                      </div>
                    </div>
                  </div>

                  {bookData.depth && (
                    <div className="grid grid-cols-3 gap-2 mb-2">
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">Depth 10bps</div>
                      <div className="font-mono text-xs text-fg">{formatVolume(bookData.depth.bps_10)}</div>
                      </div>
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">Depth 25bps</div>
                      <div className="font-mono text-xs text-fg">{formatVolume(bookData.depth.bps_25)}</div>
                      </div>
                      <div>
                        <div className="text-[9px] text-better-blue-light/60">Depth 50bps</div>
                      <div className="font-mono text-xs text-fg">{formatVolume(bookData.depth.bps_50)}</div>
                      </div>
                    </div>
                  )}

                  <div className="grid grid-cols-2 gap-2">
                    <div>
                      <div className="text-[9px] text-better-blue-light/60 uppercase mb-1">Bids</div>
                      <div className="space-y-1">
                        {bookData.bids.slice(0, 5).map((l, idx) => {
                          const cum = bookCumulative?.bidCum[idx] ?? l.price * l.size;
                          const pct = Math.min(100, (cum / (bookCumulative?.maxNotional ?? 1)) * 100);

                          return (
                            <div key={`b-${idx}`} className="flex items-center justify-between gap-2">
                              <span className="font-mono text-[10px] text-success tabular-nums">{formatPrice(l.price)}</span>
                              <span className="relative font-mono text-[10px] text-better-blue-lavender tabular-nums min-w-[80px] text-right">
                                <span
                                  className="absolute inset-y-0 left-0 bg-success/15"
                                  style={{ width: `${pct.toFixed(2)}%` }}
                                  aria-hidden="true"
                                />
                                <span className="relative">{formatVolumeCompact(l.size)}</span>
                              </span>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                    <div>
                      <div className="text-[9px] text-better-blue-light/60 uppercase mb-1">Asks</div>
                      <div className="space-y-1">
                        {bookData.asks.slice(0, 5).map((l, idx) => {
                          const cum = bookCumulative?.askCum[idx] ?? l.price * l.size;
                          const pct = Math.min(100, (cum / (bookCumulative?.maxNotional ?? 1)) * 100);

                          return (
                            <div key={`a-${idx}`} className="flex items-center justify-between gap-2">
                              <span className="font-mono text-[10px] text-danger tabular-nums">{formatPrice(l.price)}</span>
                              <span className="relative font-mono text-[10px] text-better-blue-lavender tabular-nums min-w-[80px] text-right">
                                <span
                                  className="absolute inset-y-0 left-0 bg-danger/15"
                                  style={{ width: `${pct.toFixed(2)}%` }}
                                  aria-hidden="true"
                                />
                                <span className="relative">{formatVolumeCompact(l.size)}</span>
                              </span>
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
      )}

      {/* Footer */}
      <div className="flex items-center justify-between mt-3 pt-2 border-t border-grey/10">
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setShowDetails(true);
              setShowMarketPanel((v) => !v);
            }}
            className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
          >
            [{showMarketPanel ? 'MARKET-' : 'MARKET'}]
          </button>

          {walletAddress && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setShowDetails(true);
                setShowWalletPanel((v) => !v);
              }}
              className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
            >
              [{showWalletPanel ? 'WALLET-' : 'WALLET'}]
            </button>
          )}

          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setShowDetails((v) => {
                const next = !v;
                if (!next) {
                  setShowMarketPanel(false);
                  setShowWalletPanel(false);
                  setShowChart(false);
                  setShowBook(false);
                }
                return next;
              });
            }}
            className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
          >
            [{showDetails ? 'HIDE' : 'DETAILS'}]
          </button>

          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setShowDetails(true);
              setShowChart((v) => !v);
            }}
            className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
            disabled={!canFetchAnalytics}
          >
            [{showChart ? 'CHART-' : 'CHART'}]
          </button>

          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setShowDetails(true);
              setShowBook((v) => !v);
            }}
            className="text-[9px] font-mono text-better-blue-lavender hover:text-fg transition-colors"
          >
            [{showBook ? 'BOOK-' : 'BOOK'}]
          </button>
        </div>
        <span className="text-[9px] text-better-blue-lavender font-mono">{timeAgo}</span>
      </div>
    </div>
  );
};

export const SignalCard = memo(SignalCardComponent, (prevProps, nextProps) => {
  return prevProps.signal.id === nextProps.signal.id &&
         prevProps.signal.detected_at === nextProps.signal.detected_at &&
         prevProps.signal.context_version === nextProps.signal.context_version &&
         prevProps.signal.context_status === nextProps.signal.context_status;
});
