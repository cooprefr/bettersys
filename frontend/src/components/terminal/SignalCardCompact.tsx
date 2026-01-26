import { memo, useMemo, useState, useCallback, useEffect } from 'react';
import { Signal, MarketSnapshotResponse } from '../../types/signal';
import { api } from '../../services/api';
import {
  getSignalLabel,
  formatConfidence,
  formatPrice,
  formatTimestamp,
  formatPnL,
  formatDelta,
  metricColorClass,
} from '../../utils/formatters';
import type { InspectorTab } from './SignalInspectorDrawer';

export interface SignalCardCompactProps {
  signal: Signal;
  onOpenInspector?: (tab: InspectorTab) => void;
}

function normalizeMarketTitle(raw: string): string {
  const s = (raw || '').replace(/\s+/g, ' ').trim();
  if (!s) return '';
  const quoted = s.match(/\bon\s+['"]([^'"]+)['"]/i);
  if (quoted?.[1]) return quoted[1].replace(/\s+/g, ' ').trim();
  return s;
}

function formatMaybeNum(v: number | null | undefined, digits: number = 0): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}

function tabButtonClass(active: boolean): string {
  return [
    'px-2 py-1 text-[11px] font-mono border transition-colors',
    active
      ? 'border-fg bg-fg text-void'
      : 'border-grey/30 text-fg/90 hover:text-fg hover:border-grey/50',
  ].join(' ');
}

export const SignalCardCompact = memo<SignalCardCompactProps>(({ signal, onOpenInspector }) => {
  const ctx = signal.context;
  const derived = ctx?.derived;
  const order = ctx?.order;

  // Inline trade panel state
  const [isTradeExpanded, setIsTradeExpanded] = useState(false);
  const [tradeSide, setTradeSide] = useState<'BUY' | 'SELL'>('BUY');
  const [tradeNotionalUsd, setTradeNotionalUsd] = useState<number>(25);
  const [tradeOrderType, setTradeOrderType] = useState<'GTC' | 'FAK' | 'FOK'>('GTC');
  const [tradePriceMode, setTradePriceMode] = useState<'JOIN' | 'CROSS' | 'CUSTOM'>('JOIN');
  const [tradeCustomPrice, setTradeCustomPrice] = useState<string>('');
  const [tradeArmed, setTradeArmed] = useState(false);
  const [tradeStatus, setTradeStatus] = useState<string | null>(null);
  const [bookData, setBookData] = useState<MarketSnapshotResponse | null>(null);
  const [bookLoading, setBookLoading] = useState(false);

  const tradingEnabled =
    String(import.meta.env.VITE_ENABLE_TRADING || '').toLowerCase() === 'true' ||
    String(import.meta.env.VITE_ENABLE_TRADING || '').toLowerCase() === '1';

  const marketTitle = useMemo(() => {
    const m: any = (ctx as any)?.market;
    const fromCtx = m ? String(m.question || m.title || '') : '';
    return normalizeMarketTitle(fromCtx || signal.details.market_title || order?.title || signal.market_slug || '');
  }, [ctx, signal.details.market_title, order?.title, signal.market_slug]);

  const walletShort = useMemo(() => {
    const st: any = signal.signal_type as any;
    const addr: string | undefined =
      signal.signal_type.type === 'TrackedWalletEntry'
        ? st.wallet_address
        : signal.signal_type.type === 'WhaleFollowing'
          ? st.whale_address
          : signal.signal_type.type === 'EliteWallet'
            ? st.wallet_address
            : signal.signal_type.type === 'InsiderWallet'
              ? st.wallet_address
              : order?.user;
    if (!addr) return '---';
    return `${addr.slice(0, 6)}…${addr.slice(-4)}`;
  }, [signal.signal_type, order?.user]);

  const marketSlugForBook = signal.market_slug || order?.market_slug;
  const outcomeForBook = (order?.token_label as string | undefined) || ((signal.signal_type as any)?.token_label as string | undefined);
  const clobTokenIdForBook = (order?.token_id as string | undefined) || (signal.details?.market_id as string | undefined);
  const hasNumericClobTokenId = typeof clobTokenIdForBook === 'string' && /^[0-9]+$/.test(clobTokenIdForBook);
  const canFetchBook = Boolean(hasNumericClobTokenId || (marketSlugForBook && outcomeForBook));

  const fetchBook = useCallback(async () => {
    if (!canFetchBook) return;
    setBookLoading(true);
    try {
      const snapshot = hasNumericClobTokenId
        ? await api.getMarketSnapshot(clobTokenIdForBook!, 6)
        : await api.getMarketSnapshotBySlug(marketSlugForBook!, outcomeForBook!, 6);
      setBookData(snapshot);
    } catch {
      // ignore
    } finally {
      setBookLoading(false);
    }
  }, [canFetchBook, hasNumericClobTokenId, clobTokenIdForBook, marketSlugForBook, outcomeForBook]);

  // Fetch book when trade panel expands
  useEffect(() => {
    if (isTradeExpanded && canFetchBook && !bookData && !bookLoading) {
      fetchBook();
    }
  }, [isTradeExpanded, canFetchBook, bookData, bookLoading, fetchBook]);

  // Reset trade state when panel collapses
  useEffect(() => {
    if (!isTradeExpanded) {
      setTradeArmed(false);
      setTradeStatus(null);
    }
  }, [isTradeExpanded]);

  const open = (tab: InspectorTab) => {
    if (tab === 'TRADE') {
      setIsTradeExpanded((v) => !v);
    } else {
      onOpenInspector?.(tab);
    }
  };

  // BUY or SELL action from the order (fallback to `recommended_action` when context isn't loaded yet).
  const orderSide = useMemo(() => {
    if (order?.side) return order.side.toUpperCase();
    const act = (signal.details?.recommended_action || '').toUpperCase();
    if (act.includes('SELL')) return 'SELL';
    if (act.includes('BUY')) return 'BUY';
    return '---';
  }, [order?.side, signal.details?.recommended_action]);

  const displayPrice =
    typeof order?.price === 'number'
      ? order.price
      : typeof signal.details?.current_price === 'number'
        ? signal.details.current_price
        : null;
  // Outcome label (Up, Down, Yes, No)
  const outcome = (order?.token_label as string | undefined) || ((signal.signal_type as any)?.token_label as string | undefined) || '---';
  const sizeUsd = useMemo(() => {
    if (!order) return null;
    const shares = order.shares_normalized;
    const px = order.price;
    if (typeof shares !== 'number' || typeof px !== 'number') return null;
    const v = shares * px;
    return Number.isFinite(v) ? v : null;
  }, [order]);

  const displaySizeUsd = useMemo(() => {
    if (typeof sizeUsd === 'number') return sizeUsd;
    const st: any = signal.signal_type as any;
    if (signal.signal_type.type === 'TrackedWalletEntry' && typeof st.position_value_usd === 'number') {
      return st.position_value_usd;
    }
    if (typeof signal.details?.recommended_size === 'number') return signal.details.recommended_size;
    if (typeof signal.details?.volume_24h === 'number') return signal.details.volume_24h;
    return null;
  }, [sizeUsd, signal.signal_type, signal.details?.recommended_size, signal.details?.volume_24h]);

  const deltaSignValue =
    typeof derived?.price_delta_bps === 'number'
      ? derived.price_delta_bps
      : typeof derived?.price_delta_abs === 'number'
        ? derived.price_delta_abs
        : null;

  const handleTradeSubmit = async () => {
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
  };

  return (
    <div className="border-b border-grey/10 bg-surface hover:bg-fg/5 transition-colors">
      <div className="px-4 md:px-6 py-4">
        {/* Row 1 */}
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-3 min-w-0">
            <div className="text-[11px] font-mono text-fg/90">[{getSignalLabel(signal.signal_type.type)}]</div>
            <div className="text-[11px] font-mono text-fg/90 truncate">{formatTimestamp(signal.detected_at)}</div>
            <div className="text-[11px] font-mono text-fg/90 truncate">{walletShort}</div>
            <div className="text-[11px] font-mono text-better-blue-lavender truncate">{formatConfidence(signal.confidence)}</div>
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => open('TRADE')}
              className={`border px-2 py-1 text-[11px] font-mono transition-colors ${
                isTradeExpanded
                  ? 'border-success bg-success/20 text-success'
                  : 'border-fg bg-fg text-void hover:bg-fg/90'
              }`}
            >
              {isTradeExpanded ? '[CLOSE]' : '[TRADE]'}
            </button>
            <button
              type="button"
              onClick={() => open('DETAILS')}
              className="border border-grey/30 px-2 py-1 text-[11px] font-mono text-fg/90 hover:text-fg hover:border-grey/50"
            >
              [OPEN]
            </button>
          </div>
        </div>

        {/* Row 2 */}
        <div className="mt-1 flex items-center justify-between gap-3">
          <div
            className="text-[13px] font-mono font-semibold text-fg truncate cursor-pointer"
            title={marketTitle}
            onClick={() => open('DETAILS')}
          >
            {marketTitle || '---'}
          </div>
          <div className="flex items-center gap-2">
            {(['DETAILS', 'PERFORMANCE', 'BOOK', 'TRADE'] as InspectorTab[]).map((t) => (
              <button
                key={t}
                type="button"
                onClick={() => open(t)}
                className="text-[11px] font-mono text-fg/90 hover:text-fg"
              >
                [{t}]
              </button>
            ))}
          </div>
        </div>

        {/* Row 3: dense horizontal metrics */}
        <div className="mt-3 grid grid-cols-9 gap-2">
          <div>
            <div className="text-[10px] text-better-blue-light/90">PRICE</div>
            <div className="text-[12px] font-mono text-fg tabular-nums">
              {typeof displayPrice === 'number' ? formatPrice(displayPrice) : '---'}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">SIZE</div>
            <div className="text-[12px] font-mono text-fg tabular-nums">
              {typeof displaySizeUsd === 'number' ? `$${displaySizeUsd.toFixed(2)}` : '---'}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">ACTION</div>
            <div className={`text-[12px] font-mono font-semibold truncate ${
              orderSide === 'BUY' ? 'text-success' : orderSide === 'SELL' ? 'text-danger' : 'text-fg'
            }`}>
              {orderSide}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">OUTCOME</div>
            <div className="text-[12px] font-mono text-fg truncate">{outcome}</div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">Δ</div>
            <div className={`text-[12px] font-mono tabular-nums ${metricColorClass(deltaSignValue)}`}>
              {formatDelta(derived?.price_delta_bps, derived?.price_delta_abs)}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">SPREAD</div>
            <div className="text-[12px] font-mono text-fg tabular-nums">
              {typeof derived?.spread_at_entry === 'number' ? formatPrice(derived.spread_at_entry) : '---'}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">PnL 30D</div>
            <div className={`text-[12px] font-mono tabular-nums ${metricColorClass(derived?.pnl_30d)}`}>
              {typeof derived?.pnl_30d === 'number' ? formatPnL(derived.pnl_30d) : '---'}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">Realized Sharpe 30D</div>
            <div className={`text-[12px] font-mono tabular-nums ${metricColorClass(derived?.sharpe_30d)}`}>
              {formatMaybeNum(derived?.sharpe_30d, 2)}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-better-blue-light/90">METADATA</div>
            <div className="text-[12px] font-mono text-fg truncate">{signal.context_status ?? '---'}</div>
          </div>
        </div>
      </div>

      {/* Inline Trade Panel - Expands Below Card */}
      <div
        className={`overflow-hidden transition-all duration-300 ease-in-out ${
          isTradeExpanded ? 'max-h-[400px] opacity-100' : 'max-h-0 opacity-0'
        }`}
      >
        <div className="px-4 pb-4 border-t border-grey/20 bg-void/50">
          <div className="pt-3 space-y-3">
            {/* Mini orderbook preview */}
            {canFetchBook && (
              <div className="grid grid-cols-4 gap-2">
                <div className="bg-fg/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Bid</div>
                  <div className="text-[13px] font-mono text-success tabular-nums">
                    {bookLoading ? '...' : typeof bookData?.best_bid === 'number' ? formatPrice(bookData.best_bid) : '---'}
                  </div>
                </div>
                <div className="bg-fg/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Ask</div>
                  <div className="text-[13px] font-mono text-danger tabular-nums">
                    {bookLoading ? '...' : typeof bookData?.best_ask === 'number' ? formatPrice(bookData.best_ask) : '---'}
                  </div>
                </div>
                <div className="bg-fg/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Spread</div>
                  <div className="text-[13px] font-mono text-fg tabular-nums">
                    {bookLoading ? '...' : typeof bookData?.spread === 'number' ? formatPrice(bookData.spread) : '---'}
                  </div>
                </div>
                <div className="bg-fg/5 rounded p-2">
                  <div className="text-[10px] text-better-blue-light/90 uppercase">Imb</div>
                  <div className="text-[13px] font-mono text-fg tabular-nums">
                    {bookLoading ? '...' : typeof bookData?.imbalance_10bps === 'number' ? bookData.imbalance_10bps.toFixed(2) : '---'}
                  </div>
                </div>
              </div>
            )}

            {/* Trade controls */}
            <div className="bg-fg/5 rounded p-3 space-y-3">
              <div className="flex items-center justify-between">
                <div className="text-[11px] font-mono text-fg/90">One-click trade (Polymarket)</div>
                <div className={`text-[10px] font-mono ${tradingEnabled ? 'text-success' : 'text-warning'}`}>
                  {tradingEnabled ? 'ENABLED' : 'DISABLED'}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3">
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
                    className="ml-auto w-[100px] bg-void/50 border border-grey/30 px-2 py-1 text-[12px] font-mono text-fg tabular-nums"
                    inputMode="decimal"
                  />
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3">
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
                    className="mt-1 w-full bg-void/50 border border-grey/30 px-2 py-1 text-[12px] font-mono text-fg tabular-nums disabled:opacity-40"
                  />
                </div>
              </div>
            </div>

            {/* ARM / SUBMIT */}
            <div className="flex items-center justify-between">
              <div className="text-[10px] font-mono text-fg/80 truncate">
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
                  onClick={handleTradeSubmit}
                  className="border border-grey/30 px-3 py-1 text-[11px] font-mono text-fg/90 hover:text-fg hover:border-grey/50 disabled:opacity-40"
                >
                  [SUBMIT]
                </button>
              </div>
            </div>

            {tradeStatus && <div className="text-[11px] font-mono text-fg/90">{tradeStatus}</div>}
          </div>
        </div>
      </div>
    </div>
  );
});

SignalCardCompact.displayName = 'SignalCardCompact';
