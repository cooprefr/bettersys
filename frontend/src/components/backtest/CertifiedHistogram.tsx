/**
 * CertifiedHistogram - Deterministic PnL Distribution Renderer
 * 
 * This component renders histogram bins EXACTLY as provided by the backend.
 * It does NOT compute bins from raw samples - all binning is done server-side
 * to ensure deterministic, reproducible, and certified results.
 * 
 * Non-negotiables:
 * - Bar positions are computed from left/right edges, not re-binned
 * - No locale or timezone-dependent formatting affects the chart
 * - Provenance (manifest_hash) is always displayed for verification
 * - If backend bins are unavailable, shows error state (no fallback)
 */

import React, { useCallback, useEffect, useMemo, useState } from 'react';
import type {
  WindowPnlHistogramResponse,
  CertifiedHistogramBin,
} from '../../types/backtest';
import { validateHistogramResponse } from '../../types/backtest';
import { api } from '../../services/api';

// =============================================================================
// TYPES
// =============================================================================

export interface CertifiedHistogramProps {
  /** Run ID to fetch histogram for */
  runId: string;
  /** Number of bins (default: 50) */
  binCount?: number;
  /** Optional pre-fetched histogram data */
  data?: WindowPnlHistogramResponse | null;
  /** Callback when download is requested */
  onDownload?: (json: string) => void;
  /** Additional CSS classes */
  className?: string;
}

type LoadingState = 
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'success'; data: WindowPnlHistogramResponse }
  | { status: 'error'; message: string };

// =============================================================================
// FORMATTING HELPERS (deterministic, locale-independent)
// =============================================================================

/**
 * Format a number as USD with fixed precision.
 * Uses explicit formatting to avoid locale-dependent variations.
 */
function formatUsd(value: number, decimals: number = 2): string {
  const sign = value < 0 ? '-' : '';
  const abs = Math.abs(value);
  const fixed = abs.toFixed(decimals);
  return `${sign}$${fixed}`;
}

/**
 * Format a count as an integer string.
 */
function formatCount(value: number): string {
  return Math.round(value).toString();
}

/**
 * Truncate a hash for display (first 8 chars).
 */
function truncateHash(hash: string): string {
  return hash.slice(0, 8);
}

// =============================================================================
// SUB-COMPONENTS
// =============================================================================

interface HistogramBarProps {
  bin: CertifiedHistogramBin;
  maxCount: number;
  chartWidth: number;
  chartHeight: number;
  minEdge: number;
  maxEdge: number;
  index: number;
}

/**
 * Single histogram bar - position and width computed from bin edges.
 */
const HistogramBar: React.FC<HistogramBarProps> = ({
  bin,
  maxCount,
  chartWidth,
  chartHeight,
  minEdge,
  maxEdge,
  index,
}) => {
  const range = maxEdge - minEdge;
  if (range <= 0) return null;

  // X position and width from bin edges (deterministic)
  const xStart = ((bin.left - minEdge) / range) * chartWidth;
  const xEnd = ((bin.right - minEdge) / range) * chartWidth;
  const width = Math.max(1, xEnd - xStart - 1); // 1px gap between bars

  // Y height from count
  const height = maxCount > 0 ? (bin.count / maxCount) * (chartHeight - 20) : 0;
  const y = chartHeight - height;

  // Color based on whether bin center is positive or negative
  const binCenter = (bin.left + bin.right) / 2;
  const fillColor = binCenter >= 0 ? '#22c55e' : '#ef4444'; // green/red

  return (
    <rect
      x={xStart}
      y={y}
      width={width}
      height={height}
      fill={fillColor}
      opacity={0.8}
      data-testid={`histogram-bar-${index}`}
      data-left={bin.left}
      data-right={bin.right}
      data-count={bin.count}
    />
  );
};

interface ProvenanceBadgeProps {
  runId: string;
  manifestHash: string;
  isTrusted: boolean;
}

/**
 * Provenance badge showing certification status.
 */
const ProvenanceBadge: React.FC<ProvenanceBadgeProps> = ({
  runId,
  manifestHash,
  isTrusted,
}) => {
  const badgeColor = isTrusted
    ? 'bg-success/20 border-success text-success'
    : 'bg-danger/20 border-danger text-danger';

  return (
    <div className={`flex items-center gap-2 px-3 py-1.5 border ${badgeColor} text-[10px] font-mono`}>
      <span className="tracking-widest">
        {isTrusted ? 'CERTIFIED' : 'UNCERTIFIED'}
      </span>
      <span className="text-fg/60">|</span>
      <span className="text-fg/80" title={`Run ID: ${runId}`}>
        {truncateHash(runId.replace('run_', ''))}
      </span>
      <span className="text-fg/60">|</span>
      <span className="text-fg/80" title={`Manifest Hash: ${manifestHash}`}>
        {truncateHash(manifestHash)}
      </span>
    </div>
  );
};

interface DownloadButtonProps {
  onClick: () => void;
  disabled?: boolean;
}

/**
 * Download button for histogram JSON.
 */
const DownloadButton: React.FC<DownloadButtonProps> = ({ onClick, disabled }) => (
  <button
    onClick={onClick}
    disabled={disabled}
    className="px-3 py-1.5 bg-surface border border-grey/20 hover:border-grey/40 
               text-[10px] font-mono tracking-widest text-fg/80 hover:text-fg
               disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
    title="Download histogram JSON for verification"
  >
    [DOWNLOAD JSON]
  </button>
);

// =============================================================================
// ERROR STATES
// =============================================================================

interface ErrorStateProps {
  message: string;
  onRetry?: () => void;
}

const ErrorState: React.FC<ErrorStateProps> = ({ message, onRetry }) => (
  <div className="flex flex-col items-center justify-center h-48 bg-danger/5 border-2 border-danger/30">
    <div className="text-danger text-lg mb-2" aria-hidden="true">!</div>
    <h3 className="text-danger font-mono text-[10px] tracking-widest mb-2">
      DISTRIBUTION UNAVAILABLE
    </h3>
    <p className="text-fg/60 font-mono text-[10px] text-center max-w-xs px-4">
      {message}
    </p>
    {onRetry && (
      <button
        onClick={onRetry}
        className="mt-3 px-3 py-1 bg-danger/20 hover:bg-danger/30 border border-danger 
                   text-danger font-mono text-[10px] tracking-widest"
      >
        [RETRY]
      </button>
    )}
  </div>
);

const UnavailableState: React.FC = () => (
  <div className="flex flex-col items-center justify-center h-48 bg-surface border border-grey/20">
    <div className="text-fg/40 text-lg mb-2" aria-hidden="true">-</div>
    <h3 className="text-fg/60 font-mono text-[10px] tracking-widest mb-2">
      DISTRIBUTION UNAVAILABLE FOR CERTIFIED RUNS
    </h3>
    <p className="text-fg/40 font-mono text-[10px] text-center max-w-xs px-4">
      Backend histogram bins are required for certified views.
      Client-side binning is not permitted.
    </p>
  </div>
);

const LoadingState: React.FC = () => (
  <div className="flex items-center justify-center h-48 bg-surface border border-grey/20">
    <span className="text-fg/60 font-mono text-[10px] tracking-widest animate-pulse">
      LOADING CERTIFIED HISTOGRAM...
    </span>
  </div>
);

// =============================================================================
// MAIN COMPONENT
// =============================================================================

export const CertifiedHistogram: React.FC<CertifiedHistogramProps> = ({
  runId,
  binCount = 50,
  data: preloadedData,
  onDownload,
  className = '',
}) => {
  const [state, setState] = useState<LoadingState>(
    preloadedData
      ? { status: 'success', data: preloadedData }
      : { status: 'idle' }
  );

  // Fetch histogram data
  const fetchHistogram = useCallback(async () => {
    setState({ status: 'loading' });
    try {
      const response = await api.getWindowPnlHistogram(runId, binCount);
      setState({ status: 'success', data: response });
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to load histogram';
      setState({ status: 'error', message });
    }
  }, [runId, binCount]);

  // Initial fetch
  useEffect(() => {
    if (!preloadedData && state.status === 'idle') {
      fetchHistogram();
    }
  }, [preloadedData, state.status, fetchHistogram]);

  // Update if preloaded data changes
  useEffect(() => {
    if (preloadedData) {
      const validationError = validateHistogramResponse(preloadedData);
      if (validationError) {
        setState({ status: 'error', message: validationError });
      } else {
        setState({ status: 'success', data: preloadedData });
      }
    }
  }, [preloadedData]);

  // Compute chart dimensions and data
  const chartData = useMemo(() => {
    if (state.status !== 'success') return null;
    
    const { data } = state;
    // Defensive: check bins exists and is an array
    if (!data || !data.bins || !Array.isArray(data.bins) || data.bins.length === 0) {
      return null;
    }

    const { bins, binning, total_samples } = data;
    const maxCount = Math.max(...bins.map(b => b.count), 1);
    const { min: minEdge, max: maxEdge } = binning;

    return {
      bins,
      maxCount,
      minEdge,
      maxEdge,
      totalSamples: total_samples,
    };
  }, [state]);

  // Handle download
  const handleDownload = useCallback(() => {
    if (state.status !== 'success') return;
    const json = JSON.stringify(state.data, null, 2);
    
    if (onDownload) {
      onDownload(json);
    } else {
      // Default: trigger browser download
      const blob = new Blob([json], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `histogram_${runId}_${state.data.manifest_hash.slice(0, 8)}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    }
  }, [state, runId, onDownload]);

  // Render loading state
  if (state.status === 'loading' || state.status === 'idle') {
    return <LoadingState />;
  }

  // Render error state
  if (state.status === 'error') {
    // Check if it's a "bins unavailable" error
    if (state.message.includes('schema_version') || state.message.includes('Missing')) {
      return <UnavailableState />;
    }
    return <ErrorState message={state.message} onRetry={fetchHistogram} />;
  }

  // No chart data (empty bins)
  if (!chartData) {
    return <UnavailableState />;
  }

  const { data } = state;
  const chartWidth = 600;
  const chartHeight = 200;
  const padding = { top: 10, right: 10, bottom: 30, left: 10 };
  const innerWidth = chartWidth - padding.left - padding.right;
  const innerHeight = chartHeight - padding.top - padding.bottom;

  return (
    <div className={`bg-surface border border-grey/10 ${className}`}>
      {/* Header */}
      <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
        <div className="text-[10px] text-fg/90 tracking-widest">
          PNL DISTRIBUTION
        </div>
        <div className="flex items-center gap-3">
          <ProvenanceBadge
            runId={data.run_id}
            manifestHash={data.manifest_hash}
            isTrusted={data.is_trusted}
          />
          <DownloadButton onClick={handleDownload} />
        </div>
      </div>

      {/* Chart */}
      <div className="p-4">
        <svg
          viewBox={`0 0 ${chartWidth} ${chartHeight}`}
          className="w-full h-auto"
          style={{ maxHeight: '250px' }}
          data-testid="certified-histogram-svg"
        >
          {/* Background */}
          <rect
            x={padding.left}
            y={padding.top}
            width={innerWidth}
            height={innerHeight}
            fill="transparent"
          />

          {/* Zero line if range crosses zero */}
          {chartData.minEdge < 0 && chartData.maxEdge > 0 && (
            <line
              x1={padding.left + ((0 - chartData.minEdge) / (chartData.maxEdge - chartData.minEdge)) * innerWidth}
              y1={padding.top}
              x2={padding.left + ((0 - chartData.minEdge) / (chartData.maxEdge - chartData.minEdge)) * innerWidth}
              y2={padding.top + innerHeight}
              stroke="#666"
              strokeWidth={1}
              strokeDasharray="4,4"
            />
          )}

          {/* Bars */}
          <g transform={`translate(${padding.left}, ${padding.top})`}>
            {chartData.bins.map((bin, idx) => (
              <HistogramBar
                key={idx}
                bin={bin}
                maxCount={chartData.maxCount}
                chartWidth={innerWidth}
                chartHeight={innerHeight}
                minEdge={chartData.minEdge}
                maxEdge={chartData.maxEdge}
                index={idx}
              />
            ))}
          </g>

          {/* X-axis labels */}
          <text
            x={padding.left}
            y={chartHeight - 5}
            className="text-[10px] fill-fg/60"
            textAnchor="start"
          >
            {formatUsd(chartData.minEdge)}
          </text>
          <text
            x={chartWidth - padding.right}
            y={chartHeight - 5}
            className="text-[10px] fill-fg/60"
            textAnchor="end"
          >
            {formatUsd(chartData.maxEdge)}
          </text>
          {chartData.minEdge < 0 && chartData.maxEdge > 0 && (
            <text
              x={padding.left + ((0 - chartData.minEdge) / (chartData.maxEdge - chartData.minEdge)) * innerWidth}
              y={chartHeight - 5}
              className="text-[10px] fill-fg/80"
              textAnchor="middle"
            >
              $0
            </text>
          )}
        </svg>

        {/* Stats footer */}
        <div className="flex justify-between items-center mt-3 pt-3 border-t border-grey/10">
          <div className="text-[10px] font-mono text-fg/60">
            {formatCount(data.total_samples)} WINDOWS | {data.binning.bin_count} BINS | {data.unit}
          </div>
          <div className="text-[10px] font-mono text-fg/40">
            Schema: {data.schema_version}
          </div>
        </div>

        {/* Overflow/underflow warning */}
        {(data.underflow_count > 0 || data.overflow_count > 0) && (
          <div className="mt-2 text-[9px] font-mono text-warning/80">
            Note: {data.underflow_count} underflow, {data.overflow_count} overflow samples not shown
          </div>
        )}
      </div>
    </div>
  );
};

export default CertifiedHistogram;
