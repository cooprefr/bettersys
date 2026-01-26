/**
 * CertifiedRunPage - Single Backtest Run View
 * 
 * This is the certified report view for a finalized backtest run.
 * It implements the exact layout spec for institutional-grade presentation.
 * 
 * Design principles:
 * - Certified report, not experiment
 * - Instrument panel aesthetic, not consumer dashboard
 * - No live updating - everything feels final, frozen, auditable
 * - No strategy logic exposure
 * - Strong emphasis on reproducibility
 * 
 * Sections:
 * 1. Trust & scope header
 * 2. Primary performance chart (equity curve + drawdown)
 * 3. Summary metrics strip
 * 4. Distribution & risk view
 * 5. Per-window PnL time series
 * 6. Provenance & methodology (collapsed)
 * 7. Disclaimers (conditional)
 * 8. Status footer (persistent)
 */

import React, { useState, useMemo, useCallback } from 'react';
import type {
  TrustLevel,
  Disclaimer,
  EquityPoint,
  WindowPnL,
  WindowPnLSeries,
  CertifiedSummary,
  ProvenanceSummary,
} from '../../types/backtest';
import {
  formatUsd,
} from '../../utils/certifiedFormatters';
import {
  formatUtcDatetime,
  formatDisplayDatetime,
  type DisplayTimezone,
} from '../../utils/timezone';
import { TrustStatusBadge } from './TrustStatusBadge';
import { TimezoneSelector } from './TimezoneSelector';
import {
  CurrencyDisplay,
  PnLDisplay,
  DrawdownDisplay,
  CountDisplay,
  WinRateDisplay,
  MetricCard,
} from './UnitSafeDisplay';

// =============================================================================
// TYPES
// =============================================================================

export interface CertifiedRunPageProps {
  /** Run identification */
  runId: string;
  /** Strategy name and version */
  strategyName: string;
  strategyVersion: string;
  /** Market identifier */
  market: string;
  /** Date range (ns timestamps) */
  dataRangeStartNs?: number;
  dataRangeEndNs?: number;
  /** Trust status */
  trustLevel: TrustLevel;
  trustReason?: string;
  /** Dataset info */
  datasetReadiness: string;
  settlementSource: string;
  productionGrade: boolean;
  /** Core metrics */
  summary: CertifiedSummary;
  /** Time series data */
  equityCurve: EquityPoint[];
  windowPnL: WindowPnLSeries;
  /** Provenance */
  provenance?: ProvenanceSummary;
  /** Disclaimers */
  disclaimers?: Disclaimer[];
  /** Manifest info */
  manifestHash: string;
  manifestUrl?: string;
  publishedAtNs?: number;
  schemaVersion: string;
  /** Callbacks */
  onBack?: () => void;
  onDownloadManifest?: () => void;
  onDownloadEquityCsv?: () => void;
  onDownloadWindowPnLCsv?: () => void;
}

// =============================================================================
// SECTION 1: TRUST & SCOPE HEADER
// =============================================================================

interface TrustHeaderProps {
  trustLevel: TrustLevel;
  trustReason?: string;
  datasetReadiness: string;
  settlementSource: string;
  productionGrade: boolean;
}

const TrustHeader: React.FC<TrustHeaderProps> = ({
  trustLevel,
  trustReason,
  datasetReadiness,
  settlementSource,
  productionGrade,
}) => (
  <div className="flex items-start justify-between gap-6 py-4 border-b border-grey/20">
    {/* Left: Trust badge */}
    <div className="flex flex-col gap-1">
      <TrustStatusBadge trustLevel={trustLevel} />
      {trustReason && trustLevel !== 'Trusted' && (
        <span className="text-[10px] font-mono text-fg/60 max-w-xs">
          {trustReason}
        </span>
      )}
    </div>
    
    {/* Right: Dataset info */}
    <div className="flex items-center gap-6 text-[10px] font-mono">
      <div className="text-right">
        <div className="text-fg/50 tracking-widest">DATASET</div>
        <div className="text-fg/90">{datasetReadiness}</div>
      </div>
      <div className="text-right">
        <div className="text-fg/50 tracking-widest">SETTLEMENT</div>
        <div className="text-fg/90">{settlementSource}</div>
      </div>
      <div className="text-right">
        <div className="text-fg/50 tracking-widest">PROD GRADE</div>
        <div className={productionGrade ? 'text-success' : 'text-fg/60'}>
          {productionGrade ? 'Yes' : 'No'}
        </div>
      </div>
    </div>
  </div>
);

// =============================================================================
// SECTION 2: EQUITY CURVE CHART
// =============================================================================

interface EquityCurveChartProps {
  points: EquityPoint[];
  displayTz: DisplayTimezone;
  onHover?: (index: number | null) => void;
  hoveredIndex?: number | null;
}

const EquityCurveChart: React.FC<EquityCurveChartProps> = ({
  points,
  displayTz,
  onHover,
  hoveredIndex,
}) => {
  const chartData = useMemo(() => {
    if (points.length < 2) return null;
    
    const equities = points.map(p => p.equity_value);
    const minV = Math.min(...equities);
    const maxV = Math.max(...equities);
    const span = Math.max(1e-9, maxV - minV);
    
    const w = 1000;
    const h = 280;
    const pad = { top: 20, right: 60, bottom: 40, left: 80 };
    const innerW = w - pad.left - pad.right;
    const innerH = h - pad.top - pad.bottom;
    
    const pathD: string[] = [];
    const pointCoords: Array<{ x: number; y: number }> = [];
    
    for (let i = 0; i < points.length; i++) {
      const x = pad.left + (i / (points.length - 1)) * innerW;
      const y = pad.top + (1 - (equities[i] - minV) / span) * innerH;
      pathD.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
      pointCoords.push({ x, y });
    }
    
    return { pathD: pathD.join(' '), w, h, pad, innerW, innerH, minV, maxV, pointCoords };
  }, [points]);
  
  const config = useMemo(() => ({ displayTz, showUtcInTooltips: true }), [displayTz]);
  
  if (!chartData || points.length < 2) {
    return (
      <div className="h-72 flex items-center justify-center text-fg/50 font-mono text-[11px]">
        INSUFFICIENT EQUITY DATA
      </div>
    );
  }
  
  const { w, h, pad, minV, maxV, pointCoords } = chartData;
  const hoveredPoint = hoveredIndex !== null && hoveredIndex !== undefined ? points[hoveredIndex] : null;
  const hoveredCoord = hoveredIndex !== null && hoveredIndex !== undefined ? pointCoords[hoveredIndex] : null;
  
  // Y-axis ticks
  const yTicks = useMemo(() => {
    const ticks: number[] = [];
    const range = maxV - minV;
    const step = Math.pow(10, Math.floor(Math.log10(range))) / 2;
    for (let v = Math.ceil(minV / step) * step; v <= maxV; v += step) {
      ticks.push(v);
    }
    return ticks.slice(0, 6);
  }, [minV, maxV]);
  
  return (
    <div className="relative">
      <svg
        viewBox={`0 0 ${w} ${h}`}
        className="w-full"
        style={{ maxHeight: '320px' }}
        onMouseLeave={() => onHover?.(null)}
        role="img"
        aria-label="Equity curve chart"
      >
        <defs>
          <linearGradient id="eqFill" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="#3B82F6" stopOpacity="0.2" />
            <stop offset="100%" stopColor="#3B82F6" stopOpacity="0" />
          </linearGradient>
        </defs>
        
        {/* Y-axis gridlines and labels */}
        {yTicks.map((v, i) => {
          const y = pad.top + (1 - (v - minV) / (maxV - minV)) * (h - pad.top - pad.bottom);
          return (
            <g key={i}>
              <line
                x1={pad.left}
                y1={y}
                x2={w - pad.right}
                y2={y}
                stroke="#333"
                strokeWidth="1"
                strokeDasharray="4,4"
              />
              <text
                x={pad.left - 8}
                y={y + 3}
                className="text-[9px] fill-fg/50"
                textAnchor="end"
              >
                {formatUsd(v, { decimals: 0, includeUnit: false })}
              </text>
            </g>
          );
        })}
        
        {/* Y-axis label */}
        <text
          x={15}
          y={h / 2}
          className="text-[9px] fill-fg/50"
          textAnchor="middle"
          transform={`rotate(-90, 15, ${h / 2})`}
        >
          EQUITY (USD)
        </text>
        
        {/* Area fill */}
        <path
          d={`${chartData.pathD} L ${w - pad.right} ${h - pad.bottom} L ${pad.left} ${h - pad.bottom} Z`}
          fill="url(#eqFill)"
        />
        
        {/* Line */}
        <path
          d={chartData.pathD}
          fill="none"
          stroke="#3B82F6"
          strokeWidth="2"
        />
        
        {/* Hover indicator */}
        {hoveredCoord && (
          <>
            <line
              x1={hoveredCoord.x}
              y1={pad.top}
              x2={hoveredCoord.x}
              y2={h - pad.bottom}
              stroke="#666"
              strokeWidth="1"
              strokeDasharray="2,2"
            />
            <circle
              cx={hoveredCoord.x}
              cy={hoveredCoord.y}
              r="5"
              fill="#3B82F6"
              stroke="#fff"
              strokeWidth="2"
            />
          </>
        )}
        
        {/* Invisible hover targets */}
        {pointCoords.map((coord, i) => (
          <rect
            key={i}
            x={coord.x - 5}
            y={pad.top}
            width={10}
            height={h - pad.top - pad.bottom}
            fill="transparent"
            onMouseEnter={() => onHover?.(i)}
          />
        ))}
        
        {/* X-axis label */}
        <text
          x={w / 2}
          y={h - 5}
          className="text-[9px] fill-fg/50"
          textAnchor="middle"
        >
          TIME (UTC)
        </text>
      </svg>
      
      {/* Tooltip */}
      {hoveredPoint && hoveredCoord && (
        <div
          className="absolute bg-void/95 border border-grey/30 p-3 text-[10px] font-mono z-10 pointer-events-none"
          style={{
            left: `${(hoveredCoord.x / w) * 100}%`,
            top: '20px',
            transform: 'translateX(-50%)',
          }}
        >
          <div className="text-fg/60 mb-1">
            {formatUtcDatetime(hoveredPoint.time_ns)} UTC
          </div>
          <div className="text-fg/50 text-[9px] mb-2">
            {formatDisplayDatetime(hoveredPoint.time_ns, config)}
          </div>
          <div className="grid grid-cols-2 gap-x-4 gap-y-1">
            <span className="text-fg/50">Equity:</span>
            <span className="text-fg">{formatUsd(hoveredPoint.equity_value)}</span>
            <span className="text-fg/50">Drawdown:</span>
            <span className="text-danger">{formatUsd(-hoveredPoint.drawdown_value)}</span>
          </div>
        </div>
      )}
    </div>
  );
};

// =============================================================================
// DRAWDOWN CHART (synchronized)
// =============================================================================

interface DrawdownChartProps {
  points: EquityPoint[];
  hoveredIndex?: number | null;
  onHover?: (index: number | null) => void;
}

const DrawdownChart: React.FC<DrawdownChartProps> = ({
  points,
  hoveredIndex,
  onHover,
}) => {
  const chartData = useMemo(() => {
    if (points.length < 2) return null;
    
    const dds = points.map(p => p.drawdown_bps / 100);
    const maxDd = Math.max(...dds.map(Math.abs), 0.01);
    
    const w = 1000;
    const h = 80;
    const pad = { top: 5, right: 60, bottom: 20, left: 80 };
    const innerW = w - pad.left - pad.right;
    const innerH = h - pad.top - pad.bottom;
    
    const pathD: string[] = [];
    const pointCoords: Array<{ x: number; y: number }> = [];
    
    for (let i = 0; i < points.length; i++) {
      const x = pad.left + (i / (points.length - 1)) * innerW;
      const y = pad.top + (dds[i] / maxDd) * innerH;
      pathD.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
      pointCoords.push({ x, y });
    }
    
    return { pathD: pathD.join(' '), w, h, pad, maxDd, pointCoords };
  }, [points]);
  
  if (!chartData) return null;
  
  const { w, h, pad, maxDd, pointCoords } = chartData;
  const hoveredCoord = hoveredIndex !== null && hoveredIndex !== undefined ? pointCoords[hoveredIndex] : null;
  
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      className="w-full"
      style={{ maxHeight: '100px' }}
      onMouseLeave={() => onHover?.(null)}
      role="img"
      aria-label="Drawdown chart"
    >
      <defs>
        <linearGradient id="ddFill" x1="0%" y1="0%" x2="0%" y2="100%">
          <stop offset="0%" stopColor="#EF4444" stopOpacity="0.3" />
          <stop offset="100%" stopColor="#EF4444" stopOpacity="0" />
        </linearGradient>
      </defs>
      
      {/* Area */}
      <path
        d={`${chartData.pathD} L ${w - pad.right} ${h - pad.bottom} L ${pad.left} ${h - pad.bottom} Z`}
        fill="url(#ddFill)"
      />
      
      {/* Line */}
      <path
        d={chartData.pathD}
        fill="none"
        stroke="#EF4444"
        strokeWidth="1.5"
      />
      
      {/* Max DD label */}
      <text
        x={pad.left - 8}
        y={pad.top + 10}
        className="text-[9px] fill-fg/50"
        textAnchor="end"
      >
        -{maxDd.toFixed(1)}%
      </text>
      
      {/* Hover indicator */}
      {hoveredCoord && (
        <circle
          cx={hoveredCoord.x}
          cy={hoveredCoord.y}
          r="4"
          fill="#EF4444"
          stroke="#fff"
          strokeWidth="2"
        />
      )}
      
      {/* Invisible hover targets */}
      {pointCoords.map((coord, i) => (
        <rect
          key={i}
          x={coord.x - 5}
          y={pad.top}
          width={10}
          height={h - pad.top - pad.bottom}
          fill="transparent"
          onMouseEnter={() => onHover?.(i)}
        />
      ))}
    </svg>
  );
};

// =============================================================================
// SECTION 3: SUMMARY METRICS STRIP
// =============================================================================

interface SummaryStripProps {
  summary: CertifiedSummary;
}

const SummaryStrip: React.FC<SummaryStripProps> = ({ summary }) => (
  <div className="grid grid-cols-2 md:grid-cols-6 gap-3 py-4 border-b border-grey/20">
    <MetricCard label="NET PnL" unit="USD">
      <PnLDisplay value={summary.net_pnl} context="net" />
    </MetricCard>
    <MetricCard label="GROSS PnL" unit="USD">
      <PnLDisplay value={summary.gross_pnl} context="gross" />
    </MetricCard>
    <MetricCard label="TOTAL FEES" unit="USD">
      <CurrencyDisplay value={summary.total_fees} label="Total fees" colorize forceTone="warning" />
    </MetricCard>
    <MetricCard label="MAX DRAWDOWN">
      <DrawdownDisplay value={summary.max_drawdown} percentValue={summary.max_drawdown_pct} />
    </MetricCard>
    <MetricCard label="WIN RATE">
      <WinRateDisplay
        winRate={summary.win_rate}
        wins={summary.windows_traded}
        total={summary.total_windows}
      />
    </MetricCard>
    <MetricCard label="WINDOWS TRADED">
      <CountDisplay value={summary.windows_traded} label="Windows traded" total={summary.total_windows} />
    </MetricCard>
  </div>
);

// =============================================================================
// SECTION 4: DISTRIBUTION & RISK VIEW
// =============================================================================

interface DistributionPanelProps {
  windows: WindowPnL[];
}

const DistributionPanel: React.FC<DistributionPanelProps> = ({ windows }) => {
  // Compute histogram
  const histogramData = useMemo(() => {
    if (windows.length === 0) return null;
    
    const pnls = windows.map(w => w.net_pnl);
    const min = Math.min(...pnls);
    const max = Math.max(...pnls);
    const range = max - min || 1;
    const binCount = Math.min(30, Math.max(10, Math.floor(windows.length / 5)));
    const binWidth = range / binCount;
    
    const bins: Array<{ left: number; right: number; count: number }> = [];
    for (let i = 0; i < binCount; i++) {
      bins.push({
        left: min + i * binWidth,
        right: min + (i + 1) * binWidth,
        count: 0,
      });
    }
    
    pnls.forEach(pnl => {
      const idx = Math.min(Math.floor((pnl - min) / binWidth), binCount - 1);
      if (idx >= 0) bins[idx].count++;
    });
    
    const mean = pnls.reduce((a, b) => a + b, 0) / pnls.length;
    const sorted = [...pnls].sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)];
    const maxCount = Math.max(...bins.map(b => b.count), 1);
    
    return { bins, min, max, mean, median, maxCount, binWidth };
  }, [windows]);
  
  // Compute risk stats
  const riskStats = useMemo(() => {
    if (windows.length === 0) return null;
    
    const sorted = [...windows].sort((a, b) => a.net_pnl - b.net_pnl);
    const worst = sorted[0];
    const best = sorted[sorted.length - 1];
    
    const pnls = windows.map(w => w.net_pnl);
    const mean = pnls.reduce((a, b) => a + b, 0) / pnls.length;
    const variance = pnls.reduce((sum, p) => sum + Math.pow(p - mean, 2), 0) / pnls.length;
    const stdDev = Math.sqrt(variance);
    
    // Tail loss: worst 5% of windows
    const tailCount = Math.max(1, Math.floor(windows.length * 0.05));
    const tailLoss = sorted.slice(0, tailCount).reduce((sum, w) => sum + w.net_pnl, 0);
    
    return { worst, best, stdDev, tailLoss, tailCount };
  }, [windows]);
  
  if (!histogramData || !riskStats) {
    return (
      <div className="text-fg/50 font-mono text-[11px] p-4">
        INSUFFICIENT DATA FOR DISTRIBUTION
      </div>
    );
  }
  
  const { bins, min, max, mean, median, maxCount } = histogramData;
  
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-4 py-4 border-b border-grey/20">
      {/* Left: Histogram */}
      <div className="bg-surface border border-grey/10 p-4">
        <div className="text-[10px] text-fg/90 tracking-widest mb-3">
          PER-WINDOW PnL DISTRIBUTION
        </div>
        <div className="h-32 flex items-end gap-[1px]">
          {bins.map((bin, i) => {
            const height = (bin.count / maxCount) * 100;
            const isNegative = bin.right <= 0;
            const isMeanBin = mean >= bin.left && mean < bin.right;
            const isMedianBin = median >= bin.left && median < bin.right;
            
            return (
              <div
                key={i}
                className={`flex-1 transition-all ${
                  isNegative ? 'bg-danger/70' : 'bg-success/70'
                } ${isMeanBin || isMedianBin ? 'ring-1 ring-fg/50' : ''}`}
                style={{ height: `${Math.max(2, height)}%` }}
                title={`${formatUsd(bin.left)} to ${formatUsd(bin.right)}: ${bin.count} windows`}
              />
            );
          })}
        </div>
        <div className="flex justify-between text-[9px] font-mono text-fg/50 mt-2">
          <span>{formatUsd(min)}</span>
          <span>MEAN: {formatUsd(mean)} | MED: {formatUsd(median)}</span>
          <span>{formatUsd(max)}</span>
        </div>
      </div>
      
      {/* Right: Risk stats */}
      <div className="bg-surface border border-grey/10 p-4">
        <div className="text-[10px] text-fg/90 tracking-widest mb-3">
          RISK SUMMARY
        </div>
        <div className="space-y-3 text-[11px] font-mono">
          <div className="flex justify-between">
            <span className="text-fg/60">Worst window:</span>
            <span className="text-danger">{formatUsd(riskStats.worst.net_pnl)}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-fg/60">Best window:</span>
            <span className="text-success">{formatUsd(riskStats.best.net_pnl)}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-fg/60">Std dev (per-window):</span>
            <span className="text-fg/90">{formatUsd(riskStats.stdDev)}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-fg/60">Tail loss (worst {riskStats.tailCount}):</span>
            <span className="text-danger">{formatUsd(riskStats.tailLoss)}</span>
          </div>
        </div>
      </div>
    </div>
  );
};

// =============================================================================
// SECTION 5: PER-WINDOW PnL TIME SERIES
// =============================================================================

interface WindowPnLTimeSeriesProps {
  windows: WindowPnL[];
  displayTz: DisplayTimezone;
}

const WindowPnLTimeSeries: React.FC<WindowPnLTimeSeriesProps> = ({
  windows,
  displayTz,
}) => {
  const [hoveredIdx, setHoveredIdx] = useState<number | null>(null);
  
  const chartData = useMemo(() => {
    if (windows.length === 0) return null;
    
    const pnls = windows.map(w => w.net_pnl);
    const maxAbs = Math.max(...pnls.map(Math.abs), 0.01);
    
    const w = 1000;
    const h = 120;
    const pad = { top: 10, right: 60, bottom: 25, left: 80 };
    const innerW = w - pad.left - pad.right;
    const innerH = h - pad.top - pad.bottom;
    const barW = Math.max(1, (innerW / windows.length) - 1);
    const zeroY = pad.top + innerH / 2;
    
    return { w, h, pad, innerW, innerH, barW, zeroY, maxAbs };
  }, [windows]);
  
  // displayTz is available for future tooltip enhancements
  void displayTz;
  
  if (!chartData || windows.length === 0) {
    return null;
  }
  
  const { w, h, pad, innerW, barW, zeroY, maxAbs } = chartData;
  const hoveredWindow = hoveredIdx !== null ? windows[hoveredIdx] : null;
  
  return (
    <div className="py-4 border-b border-grey/20">
      <div className="text-[10px] text-fg/90 tracking-widest mb-3">
        PER-WINDOW PnL OVER TIME
      </div>
      <div className="relative">
        <svg
          viewBox={`0 0 ${w} ${h}`}
          className="w-full"
          style={{ maxHeight: '140px' }}
          onMouseLeave={() => setHoveredIdx(null)}
          role="img"
          aria-label="Per-window PnL time series"
        >
          {/* Zero line */}
          <line
            x1={pad.left}
            y1={zeroY}
            x2={w - pad.right}
            y2={zeroY}
            stroke="#444"
            strokeWidth="1"
          />
          
          {/* Bars */}
          {windows.map((win, i) => {
            const x = pad.left + (i / windows.length) * innerW;
            const barH = (Math.abs(win.net_pnl) / maxAbs) * (zeroY - pad.top);
            const y = win.net_pnl >= 0 ? zeroY - barH : zeroY;
            const fill = win.net_pnl >= 0 ? '#22c55e' : '#ef4444';
            const opacity = hoveredIdx === i ? 1 : 0.7;
            
            return (
              <rect
                key={i}
                x={x}
                y={y}
                width={barW}
                height={barH}
                fill={fill}
                opacity={opacity}
                onMouseEnter={() => setHoveredIdx(i)}
              />
            );
          })}
          
          {/* Y-axis labels */}
          <text x={pad.left - 8} y={pad.top + 5} className="text-[9px] fill-fg/50" textAnchor="end">
            +{formatUsd(maxAbs, { decimals: 0, includeUnit: false })}
          </text>
          <text x={pad.left - 8} y={zeroY + 3} className="text-[9px] fill-fg/50" textAnchor="end">
            0
          </text>
          <text x={pad.left - 8} y={h - pad.bottom} className="text-[9px] fill-fg/50" textAnchor="end">
            -{formatUsd(maxAbs, { decimals: 0, includeUnit: false })}
          </text>
          
          {/* Y-axis unit */}
          <text x={10} y={h / 2} className="text-[8px] fill-fg/40" transform={`rotate(-90, 10, ${h / 2})`} textAnchor="middle">
            USD
          </text>
        </svg>
        
        {/* Tooltip */}
        {hoveredWindow && hoveredIdx !== null && (
          <div
            className="absolute bg-void/95 border border-grey/30 p-2 text-[10px] font-mono z-10 pointer-events-none"
            style={{
              left: `${((hoveredIdx + 0.5) / windows.length) * 100}%`,
              top: '10px',
              transform: 'translateX(-50%)',
            }}
          >
            <div className="text-fg/60 mb-1">
              {formatUtcDatetime(hoveredWindow.window_start_ns)} UTC
            </div>
            <div className={hoveredWindow.net_pnl >= 0 ? 'text-success' : 'text-danger'}>
              {formatUsd(hoveredWindow.net_pnl, { explicitSign: true })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
};

// =============================================================================
// SECTION 6: PROVENANCE PANEL
// =============================================================================

interface ProvenancePanelSectionProps {
  provenance?: ProvenanceSummary;
  strategyName: string;
  strategyVersion: string;
  datasetReadiness: string;
  settlementSource: string;
  productionGrade: boolean;
  onDownloadManifest?: () => void;
  onDownloadEquityCsv?: () => void;
  onDownloadWindowPnLCsv?: () => void;
}

const ProvenancePanelSection: React.FC<ProvenancePanelSectionProps> = ({
  provenance,
  strategyName,
  strategyVersion,
  datasetReadiness,
  settlementSource,
  productionGrade,
  onDownloadManifest,
  onDownloadEquityCsv,
  onDownloadWindowPnLCsv,
}) => {
  const [expanded, setExpanded] = useState(false);
  
  return (
    <div className="border border-grey/20 my-4">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between px-4 py-3 text-[10px] font-mono 
                   text-fg/80 tracking-widest hover:bg-grey/5 transition-colors"
        aria-expanded={expanded}
      >
        <span>METHODOLOGY & PROVENANCE</span>
        <span>{expanded ? '[-]' : '[+]'}</span>
      </button>
      
      {expanded && (
        <div className="px-4 py-4 border-t border-grey/20 text-[11px] font-mono space-y-4">
          {/* Info grid */}
          <div className="grid grid-cols-2 gap-x-8 gap-y-2 text-fg/80">
            <div>
              <span className="text-fg/50">Strategy:</span> {strategyName} v{strategyVersion}
            </div>
            <div>
              <span className="text-fg/50">Dataset readiness:</span> {datasetReadiness}
            </div>
            <div>
              <span className="text-fg/50">Settlement:</span> {settlementSource}
            </div>
            <div>
              <span className="text-fg/50">Window size:</span> 15-minute
            </div>
            <div>
              <span className="text-fg/50">Production grade:</span> {productionGrade ? 'Yes' : 'No'}
            </div>
            <div>
              <span className="text-fg/50">Deterministic:</span> Yes
            </div>
            {provenance?.run_fingerprint && (
              <>
                <div>
                  <span className="text-fg/50">Fingerprint:</span> {provenance.run_fingerprint.hash_hex?.slice(0, 16)}...
                </div>
                <div>
                  <span className="text-fg/50">Seed:</span> {provenance.run_fingerprint.seed ?? 'N/A'}
                </div>
              </>
            )}
          </div>
          
          {/* Download buttons */}
          <div className="flex gap-3 pt-3 border-t border-grey/10">
            {onDownloadManifest && (
              <button
                onClick={onDownloadManifest}
                className="px-3 py-1.5 border border-grey/30 text-fg/80 hover:border-grey/50 
                           hover:text-fg transition-colors tracking-widest text-[10px]"
              >
                [DOWNLOAD MANIFEST]
              </button>
            )}
            {onDownloadEquityCsv && (
              <button
                onClick={onDownloadEquityCsv}
                className="px-3 py-1.5 border border-grey/30 text-fg/80 hover:border-grey/50 
                           hover:text-fg transition-colors tracking-widest text-[10px]"
              >
                [EQUITY CSV]
              </button>
            )}
            {onDownloadWindowPnLCsv && (
              <button
                onClick={onDownloadWindowPnLCsv}
                className="px-3 py-1.5 border border-grey/30 text-fg/80 hover:border-grey/50 
                           hover:text-fg transition-colors tracking-widest text-[10px]"
              >
                [WINDOW PnL CSV]
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
};

// =============================================================================
// SECTION 7: DISCLAIMERS
// =============================================================================

interface DisclaimersSectionProps {
  disclaimers?: Disclaimer[];
}

const DisclaimersSection: React.FC<DisclaimersSectionProps> = ({ disclaimers }) => {
  if (!disclaimers || disclaimers.length === 0) return null;
  
  return (
    <div className="bg-warning/5 border border-warning/30 p-4 my-4">
      <div className="text-[10px] font-mono text-warning/90 tracking-widest mb-2">
        DISCLAIMERS ({disclaimers.length})
      </div>
      <ul className="space-y-1 text-[11px] font-mono text-fg/80">
        {disclaimers.map((d, i) => (
          <li key={d.id || i} className="flex items-start gap-2">
            <span className="text-warning/70">[{d.severity}]</span>
            <span>{d.message}</span>
          </li>
        ))}
      </ul>
    </div>
  );
};

// =============================================================================
// SECTION 8: STATUS FOOTER
// =============================================================================

interface StatusFooterProps {
  runId: string;
  publishedAtNs?: number;
  schemaVersion: string;
  manifestHash: string;
}

const StatusFooter: React.FC<StatusFooterProps> = ({
  runId,
  publishedAtNs,
  schemaVersion,
  manifestHash,
}) => {
  const [showFullHash, setShowFullHash] = useState(false);
  const [copied, setCopied] = useState(false);
  
  const handleCopyHash = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(manifestHash);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback
    }
  }, [manifestHash]);
  
  const truncatedHash = manifestHash.slice(0, 12);
  const publishedAt = publishedAtNs ? formatUtcDatetime(publishedAtNs) : '---';
  
  return (
    <footer
      className="fixed bottom-0 left-0 right-0 bg-void border-t border-grey/20 px-4 py-2 z-50"
      role="contentinfo"
      aria-label="Run provenance footer"
    >
      <div className="max-w-6xl mx-auto flex items-center justify-between text-[9px] font-mono text-fg/50">
        <div className="flex items-center gap-6">
          <span>
            <span className="text-fg/30">RUN:</span> {runId.slice(0, 16)}...
          </span>
          <span>
            <span className="text-fg/30">PUBLISHED:</span> {publishedAt} UTC
          </span>
          <span>
            <span className="text-fg/30">SCHEMA:</span> {schemaVersion}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-fg/30">MANIFEST:</span>
          <span
            onClick={() => setShowFullHash(!showFullHash)}
            onKeyDown={(e) => e.key === 'Enter' && setShowFullHash(!showFullHash)}
            className="cursor-pointer hover:text-fg/70 transition-colors underline decoration-dotted"
            tabIndex={0}
            role="button"
            aria-expanded={showFullHash}
            aria-label={`Manifest hash: ${showFullHash ? manifestHash : truncatedHash}`}
          >
            {showFullHash ? manifestHash : truncatedHash}
          </span>
          <button
            onClick={handleCopyHash}
            className="text-[8px] px-1 py-0.5 border border-grey/30 hover:border-grey/50 transition-colors"
            aria-label="Copy manifest hash"
          >
            {copied ? 'COPIED' : 'COPY'}
          </button>
        </div>
      </div>
    </footer>
  );
};

// =============================================================================
// MAIN PAGE COMPONENT
// =============================================================================

export const CertifiedRunPage: React.FC<CertifiedRunPageProps> = ({
  runId,
  strategyName,
  strategyVersion,
  market,
  dataRangeStartNs,
  dataRangeEndNs,
  trustLevel,
  trustReason,
  datasetReadiness,
  settlementSource,
  productionGrade,
  summary,
  equityCurve,
  windowPnL,
  provenance,
  disclaimers,
  manifestHash,
  manifestUrl,
  publishedAtNs,
  schemaVersion,
  onBack,
  onDownloadManifest,
  onDownloadEquityCsv,
  onDownloadWindowPnLCsv,
}) => {
  const [displayTz, setDisplayTz] = useState<DisplayTimezone>('UTC');
  const [hoveredEqIdx, setHoveredEqIdx] = useState<number | null>(null);
  
  // manifestUrl is available for direct linking but downloads use callbacks
  void manifestUrl;
  
  // Date range formatting
  const dateRange = useMemo(() => {
    if (!dataRangeStartNs || !dataRangeEndNs) return '---';
    const start = formatUtcDatetime(dataRangeStartNs).slice(0, 10);
    const end = formatUtcDatetime(dataRangeEndNs).slice(0, 10);
    return `${start} to ${end}`;
  }, [dataRangeStartNs, dataRangeEndNs]);
  
  return (
    <div className="min-h-screen bg-void text-fg pb-16">
      {/* Page header */}
      <header className="border-b border-grey/20 px-4 py-4">
        <div className="max-w-6xl mx-auto">
          <div className="flex items-center justify-between mb-2">
            {onBack && (
              <button
                onClick={onBack}
                className="text-[10px] font-mono text-fg/60 hover:text-fg tracking-widest"
              >
                ← ALL RUNS
              </button>
            )}
            <TimezoneSelector value={displayTz} onChange={setDisplayTz} compact />
          </div>
          <h1 className="text-lg font-mono text-fg">
            Backtest Results — {market}
          </h1>
          <div className="text-[11px] font-mono text-fg/60 mt-1">
            {strategyName} v{strategyVersion} | {dateRange}
          </div>
        </div>
      </header>
      
      <main className="max-w-6xl mx-auto px-4">
        {/* Section 1: Trust header */}
        <TrustHeader
          trustLevel={trustLevel}
          trustReason={trustReason}
          datasetReadiness={datasetReadiness}
          settlementSource={settlementSource}
          productionGrade={productionGrade}
        />
        
        {/* Section 2: Equity curve + Drawdown */}
        <div className="py-4 border-b border-grey/20">
          <EquityCurveChart
            points={equityCurve}
            displayTz={displayTz}
            hoveredIndex={hoveredEqIdx}
            onHover={setHoveredEqIdx}
          />
          <DrawdownChart
            points={equityCurve}
            hoveredIndex={hoveredEqIdx}
            onHover={setHoveredEqIdx}
          />
        </div>
        
        {/* Section 3: Summary metrics */}
        <SummaryStrip summary={summary} />
        
        {/* Section 4: Distribution & risk */}
        <DistributionPanel windows={windowPnL.windows} />
        
        {/* Section 5: Per-window time series */}
        <WindowPnLTimeSeries windows={windowPnL.windows} displayTz={displayTz} />
        
        {/* Section 6: Provenance */}
        <ProvenancePanelSection
          provenance={provenance}
          strategyName={strategyName}
          strategyVersion={strategyVersion}
          datasetReadiness={datasetReadiness}
          settlementSource={settlementSource}
          productionGrade={productionGrade}
          onDownloadManifest={onDownloadManifest}
          onDownloadEquityCsv={onDownloadEquityCsv}
          onDownloadWindowPnLCsv={onDownloadWindowPnLCsv}
        />
        
        {/* Section 7: Disclaimers */}
        <DisclaimersSection disclaimers={disclaimers} />
      </main>
      
      {/* Section 8: Status footer */}
      <StatusFooter
        runId={runId}
        publishedAtNs={publishedAtNs}
        schemaVersion={schemaVersion}
        manifestHash={manifestHash}
      />
    </div>
  );
};

export default CertifiedRunPage;
