import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import type { PaperTradingState } from '../../types/vault';

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
    // Format in AEST (Australia/Sydney timezone)
    return new Date(tsSec * 1000).toLocaleString('en-AU', {
      timeZone: 'Australia/Sydney',
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    });
  } catch {
    return '---';
  }
}

function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}h ${m}m`;
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
  tone?: 'info' | 'success' | 'danger' | 'warning' | 'paper';
  children: React.ReactNode;
}> = ({ tone = 'info', children }) => {
  const cls =
    tone === 'success'
      ? 'border-success/40 text-success'
      : tone === 'danger'
        ? 'border-danger/40 text-danger'
        : tone === 'warning'
          ? 'border-warning/40 text-warning'
          : tone === 'paper'
            ? 'border-warning/60 text-warning'
            : 'border-grey/30 text-fg/90';
  return (
    <span className={`px-2 py-0.5 text-[10px] font-mono border ${cls} tracking-widest select-none`}>
      {children}
    </span>
  );
};

export const PaperTradingDashboard: React.FC = () => {
  const [state, setState] = useState<PaperTradingState | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isRunning, setIsRunning] = useState(false);

  // Config
  const [asset, setAsset] = useState<'btc' | 'eth' | 'sol' | 'xrp' | 'all'>('btc');
  const [bankroll, setBankroll] = useState('10000');
  const [minEdge, setMinEdge] = useState('0.05');
  const [kellyFraction, setKellyFraction] = useState('0.05');
  const [maxPosition, setMaxPosition] = useState('0.02');

  const fetchState = useCallback(async () => {
    try {
      const resp = await api.getPaperTradingState();
      setState(resp);
      setIsRunning(resp.is_running);
    } catch (e: any) {
      // Might not be running yet
      if (!e?.message?.includes('404')) {
        setError(e?.message ?? 'Failed to fetch state');
      }
    }
  }, []);

  const startPaperTrading = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await api.startPaperTrading({
        asset,
        bankroll: parseFloat(bankroll),
        min_edge: parseFloat(minEdge),
        kelly_fraction: parseFloat(kellyFraction),
        max_position_pct: parseFloat(maxPosition),
      });
      setIsRunning(true);
      await fetchState();
    } catch (e: any) {
      setError(e?.message ?? 'Failed to start paper trading');
    } finally {
      setLoading(false);
    }
  }, [asset, bankroll, minEdge, kellyFraction, maxPosition, fetchState]);

  const stopPaperTrading = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await api.stopPaperTrading();
      setIsRunning(false);
      await fetchState();
    } catch (e: any) {
      setError(e?.message ?? 'Failed to stop paper trading');
    } finally {
      setLoading(false);
    }
  }, [fetchState]);

  const resetPaperTrading = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await api.resetPaperTrading();
      await fetchState();
    } catch (e: any) {
      setError(e?.message ?? 'Failed to reset');
    } finally {
      setLoading(false);
    }
  }, [fetchState]);

  // Poll for updates when running
  useEffect(() => {
    fetchState();
    const interval = setInterval(fetchState, isRunning ? 2000 : 10000);
    return () => clearInterval(interval);
  }, [fetchState, isRunning]);

  const summary = state?.summary;
  const recentTrades = state?.recent_trades ?? [];
  const pnlCurve = useMemo(() => state?.pnl_curve ?? [], [state?.pnl_curve]);

  // PnL curve for chart
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

  const roiTone = summary?.roi_pct ? (summary.roi_pct > 0 ? 'success' : 'danger') : 'default';
  const pnlTone = summary?.realized_pnl ? (summary.realized_pnl > 0 ? 'success' : 'danger') : 'default';
  const pfTone = summary?.profit_factor ? (summary.profit_factor >= 1.0 ? 'success' : 'danger') : 'default';

  const uptime = state?.uptime_secs ?? 0;

  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* Header */}
      <div className="flex items-end justify-between mb-5 border-b border-grey/20 pb-4">
        <div>
          <div className="flex items-center gap-3 flex-wrap">
            <h1 className="text-3xl font-semibold text-fg font-mono">PAPER TRADING</h1>
            <Badge tone="paper">PAPER</Badge>
            <Badge tone={isRunning ? 'success' : 'danger'}>
              {isRunning ? 'RUNNING' : 'STOPPED'}
            </Badge>
            {state && summary?.roi_pct != null && (
              <Badge tone={summary.roi_pct > 0 ? 'success' : 'danger'}>
                {summary.roi_pct.toFixed(1)}% ROI
              </Badge>
            )}
          </div>
          <div className="text-[10px] text-fg/90 tracking-widest mt-1">
            LIVE PAPER TRADING // 15M UP/DOWN MARKETS // NO REAL MONEY
          </div>
        </div>
        <div className="text-right">
          <div className="text-[10px] text-fg/90 tracking-widest mb-1">UPTIME</div>
          <div className="text-sm font-mono text-fg">
            {isRunning ? formatDuration(uptime) : '---'}
          </div>
        </div>
      </div>

      {/* Config & Controls */}
      <Panel title="PAPER TRADING CONFIGURATION" right={loading ? 'LOADING...' : isRunning ? 'ACTIVE' : 'STOPPED'} className="mb-4">
        <div className="grid grid-cols-2 md:grid-cols-5 gap-3 mb-4">
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">ASSET</label>
            <select
              value={asset}
              onChange={(e) => setAsset(e.target.value as 'btc' | 'eth' | 'sol' | 'xrp' | 'all')}
              disabled={isRunning}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg disabled:opacity-50"
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
              disabled={isRunning}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg disabled:opacity-50"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">MIN EDGE</label>
            <input
              value={minEdge}
              onChange={(e) => setMinEdge(e.target.value)}
              disabled={isRunning}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg disabled:opacity-50"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">KELLY FRAC</label>
            <input
              value={kellyFraction}
              onChange={(e) => setKellyFraction(e.target.value)}
              disabled={isRunning}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg disabled:opacity-50"
            />
          </div>
          <div>
            <label className="text-[10px] text-fg/80 block mb-1">MAX POS %</label>
            <input
              value={maxPosition}
              onChange={(e) => setMaxPosition(e.target.value)}
              disabled={isRunning}
              className="w-full bg-void/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-fg disabled:opacity-50"
            />
          </div>
        </div>
        <div className="flex gap-3">
          {!isRunning ? (
            <button
              onClick={startPaperTrading}
              disabled={loading}
              className="bg-success hover:bg-green-600 disabled:opacity-50 text-white font-mono tracking-widest py-2 px-6 transition-colors duration-150"
            >
              {loading ? '[STARTING...]' : '[START PAPER TRADING]'}
            </button>
          ) : (
            <button
              onClick={stopPaperTrading}
              disabled={loading}
              className="bg-danger hover:bg-red-600 disabled:opacity-50 text-white font-mono tracking-widest py-2 px-6 transition-colors duration-150"
            >
              {loading ? '[STOPPING...]' : '[STOP]'}
            </button>
          )}
          <button
            onClick={resetPaperTrading}
            disabled={loading || isRunning}
            className="bg-surface border border-grey/30 hover:border-grey/50 disabled:opacity-50 text-fg font-mono tracking-widest py-2 px-6 transition-colors duration-150"
          >
            [RESET]
          </button>
        </div>
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
          sub={`${summary?.signals_seen ?? 0} signals`}
        />
      </div>

      {/* Live Equity Curve */}
      <Panel 
        title="LIVE EQUITY CURVE" 
        right={pnlCurve.length > 0 ? `${pnlCurve.length} POINTS` : 'NO DATA'}
        className="mb-4"
      >
        {pnlPath ? (
          <div>
            <svg viewBox={`0 0 ${pnlPath.w} ${pnlPath.h}`} className="w-full h-48">
              <defs>
                <linearGradient id="livePnlGradient" x1="0%" y1="0%" x2="0%" y2="100%">
                  <stop offset="0%" stopColor="#22C55E" stopOpacity="0.3" />
                  <stop offset="100%" stopColor="#22C55E" stopOpacity="0" />
                </linearGradient>
              </defs>
              <path 
                d={`${pnlPath.d} L ${pnlPath.w} ${pnlPath.h} L 0 ${pnlPath.h} Z`} 
                fill="url(#livePnlGradient)" 
              />
              <path d={pnlPath.d} fill="none" stroke="#22C55E" strokeWidth="2" />
            </svg>
            <div className="flex justify-between text-[10px] font-mono text-fg/80 mt-2">
              <span>START: {formatUsd(pnlPath.minV)}</span>
              <span>CURRENT: {formatUsd(pnlPath.maxV)}</span>
            </div>
          </div>
        ) : (
          <div className="h-48 flex items-center justify-center text-[11px] font-mono text-fg/80">
            {isRunning ? 'WAITING FOR TRADES...' : 'START PAPER TRADING TO SEE EQUITY CURVE'}
          </div>
        )}
      </Panel>

      {/* Live Stats */}
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

        <Panel title="LIVE STATS">
          <div className="grid grid-cols-2 gap-3 text-[11px] font-mono">
            <div>
              <div className="text-fg/80">SIGNALS SEEN</div>
              <div className="text-fg">{(summary?.signals_seen ?? 0).toLocaleString()}</div>
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
      <Panel title="RECENT TRADES (LIVE)" right={`SHOWING ${recentTrades.length}`}>
        {recentTrades.length === 0 ? (
          <div className="text-[11px] font-mono text-fg/80">
            {isRunning ? 'WAITING FOR TRADES...' : 'NO TRADES YET'}
          </div>
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
                {recentTrades.slice(-50).reverse().map((t, i) => {
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
        PAPER TRADING SIMULATES TRADES WITHOUT REAL MONEY // USES LIVE MARKET DATA FROM POLYMARKET
      </div>
    </div>
  );
};
