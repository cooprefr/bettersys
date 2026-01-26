import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import type { BacktestResults } from '../../types/vault';

function formatUsd(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v < 0 ? '-' : '';
  return `${sign}$${Math.abs(v).toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function formatPct(v: number | null | undefined, digits: number = 1): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `${(v * 100).toFixed(digits)}%`;
}

function formatNum(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}

function formatTs(tsSec: number | null | undefined): string {
  if (typeof tsSec !== 'number' || !Number.isFinite(tsSec)) return '---';
  try {
    return new Date(tsSec * 1000).toISOString().slice(0, 19).replace('T', ' ');
  } catch {
    return '---';
  }
}

const Panel: React.FC<{
  title: string;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}> = ({ title, right, children, className }) => {
  return (
    <div className={`bg-surface border border-grey/10 ${className || ''}`}>
      <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
        <div className="text-[10px] text-fg/90 tracking-widest">{title}</div>
        {right ? <div className="text-[10px] font-mono text-fg/80">{right}</div> : null}
      </div>
      <div className="p-4">{children}</div>
    </div>
  );
};

const KpiCard: React.FC<{ 
  label: string; 
  value: string; 
  sub?: string;
  tone?: 'default' | 'success' | 'danger';
}> = ({ label, value, sub, tone = 'default' }) => {
  const valueCls = tone === 'success' ? 'text-success' : tone === 'danger' ? 'text-danger' : 'text-fg';
  return (
    <div className="bg-surface border border-grey/10 p-4">
      <div className="text-[10px] text-fg/90 tracking-widest mb-2">{label}</div>
      <div className={`text-xl font-mono ${valueCls}`}>{value}</div>
      {sub ? <div className="text-[10px] font-mono text-fg/80 mt-1">{sub}</div> : null}
    </div>
  );
};

const Badge: React.FC<{
  tone?: 'info' | 'success' | 'danger' | 'warning';
  children: React.ReactNode;
}> = ({ tone = 'info', children }) => {
  const cls =
    tone === 'success'
      ? 'border-success/40 text-success'
      : tone === 'danger'
        ? 'border-danger/40 text-danger'
        : tone === 'warning'
          ? 'border-warning/40 text-warning'
          : 'border-grey/30 text-fg/90';
  return (
    <span className={`px-2 py-0.5 text-[10px] font-mono border ${cls} tracking-widest select-none`}>
      {children}
    </span>
  );
};

export const BacktestDashboard: React.FC = () => {
  const [results, setResults] = useState<BacktestResults | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Config state
  const [asset, setAsset] = useState<'btc' | 'eth' | 'sol' | 'xrp' | 'all'>('btc');
  const [bankroll, setBankroll] = useState('10000');
  const [minEdge, setMinEdge] = useState('0.05');
  const [kellyFraction, setKellyFraction] = useState('0.05');
  const [maxPosition, setMaxPosition] = useState('0.02');
  const [feeRate, setFeeRate] = useState('0.02');

  const runBacktest = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const resp = await api.getBacktestResults({
        asset,
        bankroll: parseFloat(bankroll),
        min_edge: parseFloat(minEdge),
        kelly_fraction: parseFloat(kellyFraction),
        max_position_pct: parseFloat(maxPosition),
        fee_rate: parseFloat(feeRate),
      });
      setResults(resp);
    } catch (e: any) {
      setError(e?.message ?? 'Failed to run backtest');
    } finally {
      setLoading(false);
    }
  }, [asset, bankroll, minEdge, kellyFraction, maxPosition, feeRate]);

  // Run on mount
  useEffect(() => {
    runBacktest();
  }, [runBacktest]);

  const pnlCurve = useMemo(() => results?.pnl_curve ?? [], [results]);
  const recentTrades = useMemo(() => results?.recent_trades ?? [], [results]);

  // Generate SVG path for PnL curve
  const pnlPath = useMemo(() => {
    if (pnlCurve.length < 2) return null;

    const pts = pnlCurve.map((p) => p.equity);
    const minV = Math.min(...pts);
    const maxV = Math.max(...pts);
    const span = Math.max(1e-9, maxV - minV);

    const w = 1000;
    const h = 200;
    const padding = 10;

    const d: string[] = [];
    for (let i = 0; i < pts.length; i++) {
      const x = (i / (pts.length - 1)) * w;
      const y = (1 - (pts[i] - minV) / span) * (h - 2 * padding) + padding;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }

    return { d: d.join(' '), w, h, minV, maxV };
  }, [pnlCurve]);

  // Calculate summary stats
  const summary = results?.summary;
  const roiTone = summary?.roi_pct ? (summary.roi_pct > 0 ? 'success' : 'danger') : 'default';
  const pnlTone = summary?.realized_pnl ? (summary.realized_pnl > 0 ? 'success' : 'danger') : 'default';
  const pfTone = summary?.profit_factor ? (summary.profit_factor >= 1.0 ? 'success' : 'danger') : 'default';

  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* Header */}
      <div className="flex items-end justify-between mb-5 border-b border-grey/20 pb-4">
        <div>
          <div className="flex items-center gap-3 flex-wrap">
            <h1 className="text-3xl font-semibold text-fg font-mono">LATENCY ARB BACKTEST</h1>
            <Badge tone="info">HISTORICAL</Badge>
            {results && <Badge tone={summary?.roi_pct && summary.roi_pct > 0 ? 'success' : 'danger'}>
              {summary?.roi_pct ? `${summary.roi_pct.toFixed(1)}% ROI` : '---'}
            </Badge>}
          </div>
          <div className="text-[10px] text-fg/90 tracking-widest mt-1">
            15M UP/DOWN MARKETS // REAL POLYMARKET DATA // FAIR-VALUE DEVIATION EDGE
          </div>
        </div>
        <div className="text-right">
          <div className="text-[10px] text-fg/90 tracking-widest mb-1">DATA RANGE</div>
          <div className="text-sm font-mono text-fg">
            {results?.date_range ? `${results.date_range.start} — ${results.date_range.end}` : '---'}
          </div>
        </div>
      </div>

      {/* Config Panel */}
      <Panel title="BACKTEST CONFIGURATION" right={loading ? 'RUNNING...' : 'READY'} className="mb-4">
        <div className="grid grid-cols-2 md:grid-cols-6 gap-3 mb-4">
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">ASSET</label>
            <select
              value={asset}
              onChange={(e) => setAsset(e.target.value as 'btc' | 'eth' | 'sol' | 'xrp' | 'all')}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            >
              <option value="btc">BTC</option>
              <option value="eth">ETH</option>
              <option value="sol">SOL</option>
              <option value="xrp">XRP</option>
              <option value="all">ALL</option>
            </select>
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">BANKROLL ($)</label>
            <input
              value={bankroll}
              onChange={(e) => setBankroll(e.target.value)}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">MIN EDGE</label>
            <input
              value={minEdge}
              onChange={(e) => setMinEdge(e.target.value)}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">KELLY FRAC</label>
            <input
              value={kellyFraction}
              onChange={(e) => setKellyFraction(e.target.value)}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">MAX POS %</label>
            <input
              value={maxPosition}
              onChange={(e) => setMaxPosition(e.target.value)}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">FEE RATE</label>
            <input
              value={feeRate}
              onChange={(e) => setFeeRate(e.target.value)}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg"
            />
          </div>
        </div>
        <button
          onClick={runBacktest}
          disabled={loading}
          className="bg-better-blue hover:bg-blue-700 disabled:opacity-50 text-white font-mono tracking-widest py-2 px-6 transition-colors duration-150"
        >
          {loading ? '[RUNNING...]' : '[RUN BACKTEST]'}
        </button>
      </Panel>

      {error ? (
        <div className="text-[12px] font-mono text-danger mb-4">{error}</div>
      ) : null}

      {/* KPI Cards */}
      <div className="grid grid-cols-2 md:grid-cols-6 gap-4 mb-4">
        <KpiCard 
          label="TOTAL PnL" 
          value={formatUsd(summary?.realized_pnl)} 
          tone={pnlTone}
        />
        <KpiCard 
          label="ROI %" 
          value={summary?.roi_pct != null ? `${summary.roi_pct.toFixed(1)}%` : '---'} 
          tone={roiTone}
        />
        <KpiCard 
          label="WIN RATE" 
          value={formatPct(summary?.win_rate)} 
          sub={`${summary?.wins ?? 0}W / ${summary?.losses ?? 0}L`}
        />
        <KpiCard 
          label="PROFIT FACTOR" 
          value={formatNum(summary?.profit_factor)} 
          tone={pfTone}
        />
        <KpiCard 
          label="MAX DRAWDOWN" 
          value={formatUsd(summary?.max_drawdown)} 
          tone="danger"
        />
        <KpiCard 
          label="TRADES" 
          value={String(summary?.trades_taken ?? 0)} 
          sub={`${summary?.opportunities ?? 0} opps`}
        />
      </div>

      {/* PnL Curve */}
      <Panel 
        title="EQUITY CURVE" 
        right={pnlCurve.length > 0 ? `${pnlCurve.length} POINTS` : 'NO DATA'}
        className="mb-4"
      >
        {pnlPath ? (
          <div>
            <svg viewBox={`0 0 ${pnlPath.w} ${pnlPath.h}`} className="w-full h-48">
              <defs>
                <linearGradient id="pnlGradient" x1="0%" y1="0%" x2="0%" y2="100%">
                  <stop offset="0%" stopColor="#3B82F6" stopOpacity="0.3" />
                  <stop offset="100%" stopColor="#3B82F6" stopOpacity="0" />
                </linearGradient>
              </defs>
              <path 
                d={`${pnlPath.d} L ${pnlPath.w} ${pnlPath.h} L 0 ${pnlPath.h} Z`} 
                fill="url(#pnlGradient)" 
              />
              <path d={pnlPath.d} fill="none" stroke="#3B82F6" strokeWidth="2" />
            </svg>
            <div className="flex justify-between text-[10px] font-mono text-fg/80 mt-2">
              <span>START: {formatUsd(pnlPath.minV)}</span>
              <span>PEAK: {formatUsd(pnlPath.maxV)}</span>
            </div>
          </div>
        ) : (
          <div className="h-48 flex items-center justify-center text-[11px] font-mono text-fg/80">
            {loading ? 'LOADING...' : 'NO EQUITY DATA AVAILABLE'}
          </div>
        )}
      </Panel>

      {/* Summary Stats */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mb-4">
        <Panel title="VOLUME & FEES">
          <div className="grid grid-cols-2 gap-3 text-[11px] font-mono">
            <div>
              <div className="text-fg/80">TOTAL VOLUME</div>
              <div className="text-fg">{formatUsd(summary?.total_volume)}</div>
            </div>
            <div>
              <div className="text-fg/80">TOTAL FEES</div>
              <div className="text-fg">{formatUsd(summary?.total_fees)}</div>
            </div>
            <div>
              <div className="text-fg/80">GROSS PROFIT</div>
              <div className="text-success">{formatUsd(summary?.gross_profit)}</div>
            </div>
            <div>
              <div className="text-fg/80">GROSS LOSS</div>
              <div className="text-danger">{formatUsd(summary?.gross_loss)}</div>
            </div>
          </div>
        </Panel>

        <Panel title="TRADE STATS">
          <div className="grid grid-cols-2 gap-3 text-[11px] font-mono">
            <div>
              <div className="text-fg/80">ORDERS SCANNED</div>
              <div className="text-fg">{(summary?.total_orders ?? 0).toLocaleString()}</div>
            </div>
            <div>
              <div className="text-fg/80">OPPORTUNITIES</div>
              <div className="text-fg">{(summary?.opportunities ?? 0).toLocaleString()}</div>
            </div>
            <div>
              <div className="text-fg/80">AVG PNL/TRADE</div>
              <div className={summary?.avg_pnl_per_trade && summary.avg_pnl_per_trade > 0 ? 'text-success' : 'text-danger'}>
                {formatUsd(summary?.avg_pnl_per_trade)}
              </div>
            </div>
            <div>
              <div className="text-fg/80">AVG TRADE SIZE</div>
              <div className="text-fg">{formatUsd(summary?.avg_trade_size)}</div>
            </div>
          </div>
        </Panel>
      </div>

      {/* Recent Trades */}
      <Panel title="RECENT TRADES" right={`SHOWING ${recentTrades.length}`}>
        {recentTrades.length === 0 ? (
          <div className="text-[11px] font-mono text-fg/80">NO TRADES AVAILABLE</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-[11px] font-mono">
              <thead>
                <tr className="text-fg/80 border-b border-grey/20">
                  <th className="text-left py-2 pr-3">TIME</th>
                  <th className="text-left py-2 pr-3">MARKET</th>
                  <th className="text-left py-2 pr-3">SIDE</th>
                  <th className="text-right py-2 pr-3">ENTRY</th>
                  <th className="text-right py-2 pr-3">EXIT</th>
                  <th className="text-right py-2 pr-3">EDGE</th>
                  <th className="text-right py-2">PnL</th>
                </tr>
              </thead>
              <tbody>
                {recentTrades.slice(0, 50).map((t, i) => {
                  const pnlCls = t.pnl >= 0 ? 'text-success' : 'text-danger';
                  return (
                    <tr key={i} className="border-b border-grey/10">
                      <td className="py-2 pr-3 text-fg/80">{formatTs(t.ts)}</td>
                      <td className="py-2 pr-3 text-fg max-w-[280px] truncate">{t.market_slug}</td>
                      <td className="py-2 pr-3 text-fg">{t.side} {t.outcome}</td>
                      <td className="py-2 pr-3 text-right text-fg">{formatNum(t.entry_price, 4)}</td>
                      <td className="py-2 pr-3 text-right text-fg">{formatNum(t.exit_price, 4)}</td>
                      <td className="py-2 pr-3 text-right text-fg">{formatPct(t.edge)}</td>
                      <td className={`py-2 text-right ${pnlCls}`}>{formatUsd(t.pnl)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Panel>

      <div className="text-[10px] text-fg/80 text-center font-mono mt-4">
        BACKTEST USES HISTORICAL DOME ORDER EVENTS FROM POLYMARKET 15M UP/DOWN MARKETS
      </div>
    </div>
  );
};
