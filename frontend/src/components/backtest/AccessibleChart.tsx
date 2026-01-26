/**
 * Accessible Chart Wrapper Components
 * 
 * Provides accessibility features for backtest charts:
 * 1. Keyboard navigation (arrow keys to move through data points)
 * 2. Screen reader summaries (sr-only text describing the chart)
 * 3. Focus indicators
 * 4. Tooltips accessible via keyboard
 * 5. High contrast color alternatives
 * 
 * COLOR ACCESSIBILITY:
 * - Never rely on color alone
 * - Use patterns/icons alongside colors
 * - Meet WCAG AA contrast ratios (4.5:1 for text)
 */

import React, { useState, useCallback, useRef, useMemo } from 'react';
import type { EquityPoint } from '../../types/backtest';
import {
  formatUsd,
  formatPercent,
} from '../../utils/certifiedFormatters';
import {
  formatDisplayDatetime,
  getTimezoneConfig,
} from '../../utils/timezone';

// =============================================================================
// TYPES
// =============================================================================

export interface ChartDataPoint {
  x: number; // timestamp in ns
  y: number; // value
  label?: string;
}

export interface AccessibleChartProps {
  /** Chart title for screen readers */
  title: string;
  /** Chart data points */
  data: ChartDataPoint[];
  /** Unit for Y-axis values */
  yUnit: 'USD' | 'percent' | 'bps' | 'ratio';
  /** SVG content (the actual chart) */
  children: React.ReactNode;
  /** Width of the chart */
  width?: number;
  /** Height of the chart */
  height?: number;
  /** Callback when a point is focused */
  onPointFocus?: (index: number | null) => void;
  /** Additional CSS classes */
  className?: string;
}

export interface ChartSummaryProps {
  /** Chart title */
  title: string;
  /** Summary statistics */
  stats: {
    min: number;
    max: number;
    start: number;
    end: number;
    count: number;
  };
  /** Unit for values */
  unit: 'USD' | 'percent' | 'bps' | 'ratio';
}

export interface EquityCurveAccessibleProps {
  points: EquityPoint[];
  initialCapital?: number;
  width?: number;
  height?: number;
  className?: string;
}

export interface DrawdownChartAccessibleProps {
  points: Array<{
    time_ns: number;
    drawdown_value: number;
    drawdown_bps: number;
  }>;
  width?: number;
  height?: number;
  className?: string;
}

// =============================================================================
// CHART SUMMARY (Screen Reader)
// =============================================================================

const formatValue = (value: number, unit: 'USD' | 'percent' | 'bps' | 'ratio'): string => {
  switch (unit) {
    case 'USD': return formatUsd(value);
    case 'percent': return formatPercent(value);
    case 'bps': return `${Math.round(value)} basis points`;
    case 'ratio': return value.toFixed(2);
  }
};

export const ChartSummary: React.FC<ChartSummaryProps> = ({
  title,
  stats,
  unit,
}) => (
  <div className="sr-only" role="status" aria-live="polite">
    <h3>{title} Summary</h3>
    <p>
      This chart shows {stats.count} data points.
      Starting value: {formatValue(stats.start, unit)}.
      Ending value: {formatValue(stats.end, unit)}.
      Minimum: {formatValue(stats.min, unit)}.
      Maximum: {formatValue(stats.max, unit)}.
    </p>
  </div>
);

// =============================================================================
// KEYBOARD NAVIGATION HOOK
// =============================================================================

function useChartKeyboardNav(
  dataLength: number,
  onFocusChange?: (index: number | null) => void
) {
  const [focusedIndex, setFocusedIndex] = useState<number | null>(null);
  
  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (dataLength === 0) return;
    
    let newIndex = focusedIndex;
    
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        e.preventDefault();
        newIndex = focusedIndex === null ? 0 : Math.min(focusedIndex + 1, dataLength - 1);
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        e.preventDefault();
        newIndex = focusedIndex === null ? dataLength - 1 : Math.max(focusedIndex - 1, 0);
        break;
      case 'Home':
        e.preventDefault();
        newIndex = 0;
        break;
      case 'End':
        e.preventDefault();
        newIndex = dataLength - 1;
        break;
      case 'Escape':
        e.preventDefault();
        newIndex = null;
        break;
      default:
        return;
    }
    
    setFocusedIndex(newIndex);
    onFocusChange?.(newIndex);
  }, [focusedIndex, dataLength, onFocusChange]);
  
  const handleBlur = useCallback(() => {
    setFocusedIndex(null);
    onFocusChange?.(null);
  }, [onFocusChange]);
  
  return {
    focusedIndex,
    setFocusedIndex,
    handleKeyDown,
    handleBlur,
  };
}

// =============================================================================
// ACCESSIBLE CHART WRAPPER
// =============================================================================

export const AccessibleChart: React.FC<AccessibleChartProps> = ({
  title,
  data,
  yUnit,
  children,
  width: _width = 600,
  height: _height = 200,
  onPointFocus,
  className = '',
}) => {
  // width and height are part of the interface for consistency but unused here
  void _width;
  void _height;
  const containerRef = useRef<HTMLDivElement>(null);
  const { focusedIndex, handleKeyDown, handleBlur } = useChartKeyboardNav(
    data.length,
    onPointFocus
  );
  
  // Compute summary stats
  const stats = useMemo(() => {
    if (data.length === 0) {
      return { min: 0, max: 0, start: 0, end: 0, count: 0 };
    }
    const values = data.map(d => d.y);
    return {
      min: Math.min(...values),
      max: Math.max(...values),
      start: data[0].y,
      end: data[data.length - 1].y,
      count: data.length,
    };
  }, [data]);
  
  // Get focused point info
  const focusedPoint = focusedIndex !== null ? data[focusedIndex] : null;
  const config = getTimezoneConfig();
  
  return (
    <div
      ref={containerRef}
      className={`relative ${className}`}
      role="img"
      aria-label={title}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      onBlur={handleBlur}
    >
      {/* Screen reader summary */}
      <ChartSummary title={title} stats={stats} unit={yUnit} />
      
      {/* Chart content */}
      <div aria-hidden="true">
        {children}
      </div>
      
      {/* Keyboard navigation tooltip */}
      {focusedPoint && (
        <div
          className="absolute top-2 right-2 bg-void/95 border border-grey/30 p-2 
                     text-[10px] font-mono z-10 shadow-lg"
          role="tooltip"
          aria-live="polite"
        >
          <div className="text-fg/60 mb-1">
            Point {(focusedIndex ?? 0) + 1} of {data.length}
          </div>
          <div className="text-fg">
            {formatValue(focusedPoint.y, yUnit)}
          </div>
          <div className="text-fg/50 text-[9px] mt-1">
            {formatDisplayDatetime(focusedPoint.x, config)}
          </div>
          {focusedPoint.label && (
            <div className="text-fg/70 mt-1">{focusedPoint.label}</div>
          )}
        </div>
      )}
      
      {/* Keyboard hint */}
      <div className="sr-only">
        Use arrow keys to navigate through data points. Press Escape to exit.
      </div>
      
      {/* Visible keyboard hint (focus indicator) */}
      {containerRef.current === document.activeElement && (
        <div className="absolute bottom-1 left-1 text-[9px] text-fg/40 font-mono">
          ← → Navigate | Esc Exit
        </div>
      )}
    </div>
  );
};

// =============================================================================
// ACCESSIBLE EQUITY CURVE
// =============================================================================

export const EquityCurveAccessible: React.FC<EquityCurveAccessibleProps> = ({
  points,
  initialCapital,
  width = 1000,
  height = 200,
  className = '',
}) => {
  const [focusedIdx, setFocusedIdx] = useState<number | null>(null);
  
  // Convert to chart data format
  const chartData = useMemo<ChartDataPoint[]>(() => 
    points.map(p => ({
      x: p.time_ns,
      y: p.equity_value,
      label: `Cash: ${formatUsd(p.cash_balance)}, Position: ${formatUsd(p.position_value)}`,
    })),
    [points]
  );
  
  // Compute path
  const pathData = useMemo(() => {
    if (points.length < 2) return null;
    
    const values = points.map(p => p.equity_value);
    const minV = Math.min(...values);
    const maxV = Math.max(...values);
    const span = Math.max(1e-9, maxV - minV);
    const padding = 10;
    
    const d: string[] = [];
    for (let i = 0; i < points.length; i++) {
      const x = (i / (points.length - 1)) * width;
      const y = (1 - (values[i] - minV) / span) * (height - 2 * padding) + padding;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    
    return { d: d.join(' '), minV, maxV };
  }, [points, width, height]);
  
  // Verify equity curve starts at initial capital
  const startMismatch = useMemo(() => {
    if (!initialCapital || points.length === 0) return null;
    const diff = Math.abs(points[0].equity_value - initialCapital);
    if (diff > initialCapital * 0.001) { // 0.1% tolerance
      return `Warning: Equity curve starts at ${formatUsd(points[0].equity_value)}, expected ${formatUsd(initialCapital)}`;
    }
    return null;
  }, [points, initialCapital]);
  
  if (points.length < 2 || !pathData) {
    return (
      <div className="flex items-center justify-center h-48 text-fg/60 text-[11px] font-mono">
        INSUFFICIENT EQUITY DATA
      </div>
    );
  }
  
  return (
    <AccessibleChart
      title="Equity Curve"
      data={chartData}
      yUnit="USD"
      width={width}
      height={height}
      onPointFocus={setFocusedIdx}
      className={className}
    >
      <svg viewBox={`0 0 ${width} ${height}`} className="w-full h-48">
        <defs>
          <linearGradient id="equityGradAccessible" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="#3B82F6" stopOpacity="0.3" />
            <stop offset="100%" stopColor="#3B82F6" stopOpacity="0" />
          </linearGradient>
        </defs>
        
        {/* Area fill */}
        <path
          d={`${pathData.d} L ${width} ${height} L 0 ${height} Z`}
          fill="url(#equityGradAccessible)"
        />
        
        {/* Line */}
        <path d={pathData.d} fill="none" stroke="#3B82F6" strokeWidth="2" />
        
        {/* Focus indicator */}
        {focusedIdx !== null && points[focusedIdx] && (
          <>
            <circle
              cx={(focusedIdx / (points.length - 1)) * width}
              cy={
                (1 - (points[focusedIdx].equity_value - pathData.minV) / 
                  Math.max(1e-9, pathData.maxV - pathData.minV)) * 
                (height - 20) + 10
              }
              r="6"
              fill="#3B82F6"
              stroke="#fff"
              strokeWidth="2"
            />
          </>
        )}
      </svg>
      
      {/* Axis labels with units */}
      <div className="flex justify-between text-[10px] font-mono text-fg/60 mt-2">
        <span>MIN: {formatUsd(pathData.minV)} (USD)</span>
        <span>MAX: {formatUsd(pathData.maxV)} (USD)</span>
      </div>
      
      {/* Warning for start mismatch */}
      {startMismatch && (
        <div className="mt-2 text-[10px] text-warning font-mono" role="alert">
          ⚠ {startMismatch}
        </div>
      )}
    </AccessibleChart>
  );
};

// =============================================================================
// ACCESSIBLE DRAWDOWN CHART
// =============================================================================

export const DrawdownChartAccessible: React.FC<DrawdownChartAccessibleProps> = ({
  points,
  width = 1000,
  height = 100,
  className = '',
}) => {
  const [focusedIdx, setFocusedIdx] = useState<number | null>(null);
  
  // Convert to chart data
  const chartData = useMemo<ChartDataPoint[]>(() =>
    points.map(p => ({
      x: p.time_ns,
      y: -p.drawdown_bps / 100, // Convert to percentage, keep negative
    })),
    [points]
  );
  
  // Compute path
  const pathData = useMemo(() => {
    if (points.length < 2) return null;
    
    const dds = points.map(p => p.drawdown_bps / 100);
    const maxDd = Math.max(...dds.map(Math.abs));
    const span = Math.max(0.01, maxDd);
    const padding = 5;
    
    const d: string[] = [];
    for (let i = 0; i < points.length; i++) {
      const x = (i / (points.length - 1)) * width;
      const y = (dds[i] / span) * (height - 2 * padding) + padding;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    
    return { d: d.join(' '), maxDd };
  }, [points, width, height]);
  
  if (points.length < 2 || !pathData) {
    return (
      <div className="flex items-center justify-center h-24 text-fg/60 text-[11px] font-mono">
        INSUFFICIENT DRAWDOWN DATA
      </div>
    );
  }
  
  return (
    <AccessibleChart
      title="Drawdown Chart"
      data={chartData}
      yUnit="percent"
      width={width}
      height={height}
      onPointFocus={setFocusedIdx}
      className={className}
    >
      <svg viewBox={`0 0 ${width} ${height}`} className="w-full h-24">
        <defs>
          <linearGradient id="ddGradAccessible" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="#EF4444" stopOpacity="0.3" />
            <stop offset="100%" stopColor="#EF4444" stopOpacity="0" />
          </linearGradient>
          {/* Pattern for accessibility (not relying on color alone) */}
          <pattern id="ddPattern" patternUnits="userSpaceOnUse" width="4" height="4">
            <path d="M-1,1 l2,-2 M0,4 l4,-4 M3,5 l2,-2" stroke="#EF4444" strokeWidth="0.5" opacity="0.5"/>
          </pattern>
        </defs>
        
        {/* Area fill with pattern */}
        <path
          d={`${pathData.d} L ${width} ${height} L 0 ${height} Z`}
          fill="url(#ddGradAccessible)"
        />
        <path
          d={`${pathData.d} L ${width} ${height} L 0 ${height} Z`}
          fill="url(#ddPattern)"
        />
        
        {/* Line */}
        <path d={pathData.d} fill="none" stroke="#EF4444" strokeWidth="1.5" />
        
        {/* Focus indicator */}
        {focusedIdx !== null && points[focusedIdx] && (
          <circle
            cx={(focusedIdx / (points.length - 1)) * width}
            cy={
              (points[focusedIdx].drawdown_bps / 100 / Math.max(0.01, pathData.maxDd)) * 
              (height - 10) + 5
            }
            r="4"
            fill="#EF4444"
            stroke="#fff"
            strokeWidth="2"
          />
        )}
      </svg>
      
      {/* Axis labels */}
      <div className="flex justify-between text-[10px] font-mono text-fg/60 mt-1">
        <span>0%</span>
        <span>MAX DD: -{pathData.maxDd.toFixed(2)}% (drawdown)</span>
      </div>
    </AccessibleChart>
  );
};
