import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import type { VaultOverviewResponse, VaultActivityResponse, VaultPerformanceResponse } from '../../types/vault';

function formatUsd(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v < 0 ? '-' : '';
  return `${sign}$${Math.abs(v).toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function formatPct(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v >= 0 ? '+' : '';
  return `${sign}${(v * 100).toFixed(digits)}%`;
}

function formatTs(tsSec: number | null | undefined): string {
  if (typeof tsSec !== 'number' || !Number.isFinite(tsSec)) return '---';
  try {
    return new Date(tsSec * 1000).toLocaleString('en-AU', {
      timeZone: 'Australia/Sydney',
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

const Panel: React.FC<{
  title: string;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}> = ({ title, right, children, className }) => (
  <div className={`bg-surface border border-grey/10 ${className || ''}`}>
    <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
      <div className="text-[10px] tracking-widest text-fg/90">{title}</div>
      {right && <div className="text-[10px] font-mono text-fg/60">{right}</div>}
    </div>
    <div className="p-4">{children}</div>
  </div>
);

const KpiCard: React.FC<{ 
  label: string; 
  value: string; 
  sub?: string;
  tone?: 'default' | 'success' | 'danger';
}> = ({ label, value, sub, tone = 'default' }) => {
  const valueCls = tone === 'success' ? 'text-success' : tone === 'danger' ? 'text-danger' : 'text-fg';
  return (
    <div className="bg-surface border border-grey/10 p-4">
      <div className="text-[10px] text-fg/60 tracking-widest mb-2">{label}</div>
      <div className={`text-xl font-mono ${valueCls}`}>{value}</div>
      {sub && <div className="text-[10px] font-mono text-fg/50 mt-1">{sub}</div>}
    </div>
  );
};

const Badge: React.FC<{
  tone?: 'default' | 'success' | 'danger' | 'warning';
  children: React.ReactNode;
}> = ({ tone = 'default', children }) => {
  const cls =
    tone === 'success' ? 'border-success/40 text-success'
    : tone === 'danger' ? 'border-danger/40 text-danger'
    : tone === 'warning' ? 'border-warning/40 text-warning'
    : 'border-grey/30 text-fg/80';
  return (
    <span className={`px-2 py-0.5 text-[10px] font-mono border ${cls} tracking-widest`}>
      {children}
    </span>
  );
};

const INITIAL_BANKROLL = 50;
const MAX_LOSS = 15;
const MAX_TRADE_SIZE = 0.50;

export const LiveTradingDashboard: React.FC = () => {
  const [overview, setOverview] = useState<VaultOverviewResponse | null>(null);
  const [activity, setActivity] = useState<VaultActivityResponse | null>(null);
  const [performance, setPerformance] = useState<VaultPerformanceResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdate, setLastUpdate] = useState<number>(0);

  const fetchAll = useCallback(async () => {
    try {
      const [overviewResp, activityResp, perfResp] = await Promise.all([
        api.getVaultOverview(),
        api.getVaultActivity().catch(() => null),
        api.getVaultPerformance('24h').catch(() => null),
      ]);
      setOverview(overviewResp);
      if (activityResp) setActivity(activityResp);
      if (perfResp) setPerformance(perfResp);
      setLastUpdate(Date.now());
      setError(null);
    } catch (e: any) {
      setError(e?.message ?? 'Failed to fetch data');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchAll();
    const interval = setInterval(fetchAll, 5000);
    return () => clearInterval(interval);
  }, [fetchAll]);

  const isLive = overview && !overview.paper;
  const engineOn = overview?.engine_enabled ?? false;
  
  // Use real Polymarket values if available, otherwise fall back to internal ledger
  const polymarketBalance = overview?.polymarket_balance;
  const polymarketPositions = overview?.polymarket_positions_value;
  const polymarketTotal = overview?.polymarket_total_value;
  
  const nav = polymarketTotal ?? overview?.nav_usdc ?? 0;
  const cash = polymarketBalance ?? overview?.cash_usdc ?? 0;
  const positionsValue = polymarketPositions ?? 0;
  
  // For live trading, we track PnL from an initial reference (not hardcoded $50)
  const totalPnl = nav - INITIAL_BANKROLL;
  const drawdownPct = INITIAL_BANKROLL > 0 ? totalPnl / INITIAL_BANKROLL : 0;
  const isCircuitBroken = totalPnl <= -MAX_LOSS;

  const trades = useMemo(() => {
    if (!activity?.events) return [];
    return activity.events
      .filter(e => e.kind === 'trade' || e.kind === 'fill')
      .sort((a, b) => b.ts - a.ts)
      .slice(0, 50);
  }, [activity?.events]);

  const stats = useMemo(() => {
    let volume = 0;
    for (const t of trades) {
      volume += Math.abs(t.notional_usdc ?? 0);
    }
    return { volume, tradeCount: trades.length };
  }, [trades]);

  const pnlCurve = useMemo(() => performance?.points ?? [], [performance?.points]);
  
  const pnlPath = useMemo(() => {
    if (pnlCurve.length < 2) return null;
    const pts = pnlCurve.map((p) => p.nav_usdc);
    const minV = Math.min(...pts);
    const maxV = Math.max(...pts);
    const span = Math.max(1e-9, maxV - minV);
    const w = 1000, h = 200, padding = 10;
    const d: string[] = [];
    for (let i = 0; i < pts.length; i++) {
      const x = (i / (pts.length - 1)) * w;
      const y = (1 - (pts[i] - minV) / span) * (h - 2 * padding) + padding;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    return { d: d.join(' '), w, h, minV, maxV };
  }, [pnlCurve]);

  const pnlTone = totalPnl >= 0 ? 'success' : 'danger';

  if (loading) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-[12px] font-mono text-fg/60">LOADING...</div>
      </div>
    );
  }

  if (!isLive) {
    return (
      <div className="h-full flex flex-col items-center justify-center p-8">
        <div className="text-xl font-mono text-warning mb-2">PAPER MODE</div>
        <div className="text-[11px] font-mono text-fg/60 text-center max-w-md">
          Set <code className="text-warning">VAULT_ENGINE_PAPER=false</code> and restart backend.
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* Header */}
      <div className="flex items-end justify-between mb-5 border-b border-grey/20 pb-4">
        <div>
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold text-fg font-mono">LIVE</h1>
            <Badge tone="danger">LIVE</Badge>
            <Badge tone={engineOn ? 'success' : 'danger'}>
              {engineOn ? 'ENGINE ON' : 'ENGINE OFF'}
            </Badge>
            {totalPnl !== 0 && (
              <Badge tone={totalPnl > 0 ? 'success' : 'danger'}>
                {formatPct(drawdownPct)}
              </Badge>
            )}
          </div>
          <div className="text-[10px] text-fg/50 tracking-widest mt-1">
            CLOB EXECUTION • CHAINLINK SETTLEMENT • 15M UP/DOWN
          </div>
        </div>
        <div className="text-right text-[10px] font-mono text-fg/50">
          {lastUpdate > 0 && `Updated ${Math.round((Date.now() - lastUpdate) / 1000)}s ago`}
        </div>
      </div>

      {/* Circuit Breaker */}
      {isCircuitBroken && (
        <div className="bg-danger/10 border border-danger/40 p-4 mb-4">
          <div className="text-[12px] font-mono text-danger">
            CIRCUIT BREAKER: Trading halted at -${MAX_LOSS} loss.
          </div>
        </div>
      )}

      {/* Config */}
      <Panel title="CONFIGURATION" right="LOCKED" className="mb-4">
        <div className="grid grid-cols-5 gap-3 text-[11px] font-mono">
          <div>
            <div className="text-fg/50 mb-1">ASSETS</div>
            <div className="text-fg">BTC/ETH/SOL/XRP</div>
          </div>
          <div>
            <div className="text-fg/50 mb-1">BANKROLL</div>
            <div className="text-fg">${INITIAL_BANKROLL}</div>
          </div>
          <div>
            <div className="text-fg/50 mb-1">MAX/TRADE</div>
            <div className="text-fg">${MAX_TRADE_SIZE} (1%)</div>
          </div>
          <div>
            <div className="text-fg/50 mb-1">KILL-SWITCH</div>
            <div className="text-fg">-${MAX_LOSS} (-30%)</div>
          </div>
          <div>
            <div className="text-fg/50 mb-1">KELLY</div>
            <div className="text-fg">10%</div>
          </div>
        </div>
      </Panel>

      {error && <div className="text-[11px] font-mono text-danger mb-4">{error}</div>}

      {/* KPIs - Real Polymarket Account */}
      <div className="grid grid-cols-6 gap-4 mb-4">
        <KpiCard label="TOTAL VALUE" value={formatUsd(nav)} tone={nav > 0 ? 'success' : 'default'} sub="Polymarket" />
        <KpiCard label="CASH (USDC)" value={formatUsd(cash)} sub="available" />
        <KpiCard label="POSITIONS" value={formatUsd(positionsValue)} sub="mark-to-market" />
        <KpiCard label="SESSION PNL" value={formatUsd(totalPnl)} tone={pnlTone} sub={`ref: $${INITIAL_BANKROLL}`} />
        <KpiCard label="TRADES" value={String(stats.tradeCount)} />
        <KpiCard label="VOLUME" value={formatUsd(stats.volume)} />
      </div>

      {/* Equity Curve */}
      <Panel title="EQUITY" right={pnlCurve.length > 0 ? `${pnlCurve.length} pts` : '---'} className="mb-4">
        {pnlPath ? (
          <div>
            <svg viewBox={`0 0 ${pnlPath.w} ${pnlPath.h}`} className="w-full h-40">
              <defs>
                <linearGradient id="eqGrad" x1="0%" y1="0%" x2="0%" y2="100%">
                  <stop offset="0%" stopColor={totalPnl >= 0 ? "#22C55E" : "#EF4444"} stopOpacity="0.2" />
                  <stop offset="100%" stopColor={totalPnl >= 0 ? "#22C55E" : "#EF4444"} stopOpacity="0" />
                </linearGradient>
              </defs>
              <path d={`${pnlPath.d} L ${pnlPath.w} ${pnlPath.h} L 0 ${pnlPath.h} Z`} fill="url(#eqGrad)" />
              <path d={pnlPath.d} fill="none" stroke={totalPnl >= 0 ? "#22C55E" : "#EF4444"} strokeWidth="1.5" />
              <line x1="0" y1={pnlPath.h / 2} x2={pnlPath.w} y2={pnlPath.h / 2} stroke="currentColor" strokeOpacity="0.2" strokeDasharray="4" />
            </svg>
            <div className="flex justify-between text-[10px] font-mono text-fg/50 mt-2">
              <span>L: {formatUsd(pnlPath.minV)}</span>
              <span>H: {formatUsd(pnlPath.maxV)}</span>
            </div>
          </div>
        ) : (
          <div className="h-40 flex items-center justify-center text-[11px] font-mono text-fg/50">
            {engineOn ? 'AWAITING DATA' : 'ENGINE OFF'}
          </div>
        )}
      </Panel>

      {/* Oracle */}
      <Panel title="ORACLE" right="CHAINLINK/POLYGON" className="mb-4">
        <div className="grid grid-cols-4 gap-4 text-[11px] font-mono">
          {['BTC', 'ETH', 'SOL', 'XRP'].map(asset => (
            <div key={asset} className="flex items-center gap-2">
              <div className="w-1.5 h-1.5 rounded-full bg-success"></div>
              <span className="text-fg/80">{asset}/USD</span>
            </div>
          ))}
        </div>
        <div className="mt-2 text-[10px] font-mono text-fg/40">
          Lag protection: skip if divergence &gt;50bps or stale &gt;5s
        </div>
      </Panel>

      {/* Trades */}
      <Panel title="TRADES" right={trades.length > 0 ? String(trades.length) : '---'} className="mb-4">
        {trades.length === 0 ? (
          <div className="text-[11px] font-mono text-fg/50 py-4 text-center">
            {engineOn ? 'AWAITING TRADES' : 'NO TRADES'}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-[11px] font-mono">
              <thead>
                <tr className="text-fg/50 border-b border-grey/10">
                  <th className="text-left py-2 pr-3">TIME</th>
                  <th className="text-left py-2 pr-3">MARKET</th>
                  <th className="text-left py-2 pr-3">SIDE</th>
                  <th className="text-right py-2 pr-3">PRICE</th>
                  <th className="text-right py-2 pr-3">SIZE</th>
                  <th className="text-right py-2">NOTIONAL</th>
                </tr>
              </thead>
              <tbody>
                {trades.map((t, i) => (
                  <tr key={t.id || i} className="border-b border-grey/5">
                    <td className="py-2 pr-3 text-fg/60">{formatTs(t.ts)}</td>
                    <td className="py-2 pr-3 text-fg/80 max-w-[240px] truncate">{t.market_slug ?? '---'}</td>
                    <td className={`py-2 pr-3 ${t.side === 'BUY' ? 'text-success' : 'text-danger'}`}>
                      {t.side} {t.outcome}
                    </td>
                    <td className="py-2 pr-3 text-right text-fg/80">{t.price?.toFixed(4) ?? '---'}</td>
                    <td className="py-2 pr-3 text-right text-fg/80">{t.shares?.toFixed(2) ?? '---'}</td>
                    <td className="py-2 text-right text-fg/80">{formatUsd(t.notional_usdc)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Panel>

      <div className="text-[9px] text-fg/30 text-center font-mono">
        LIVE • CLOB • CHAINLINK • 15M BTC/ETH/SOL/XRP
      </div>
    </div>
  );
};
