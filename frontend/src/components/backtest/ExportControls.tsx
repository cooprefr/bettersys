/**
 * ExportControls Component
 * 
 * Provides institutional-grade export functionality for certified backtest runs.
 * All exports reflect exactly the certified backend outputs without client-side
 * recomputation, normalization, or transformation.
 * 
 * Export Types:
 * - CSV: Per-window PnL series, Equity curve time series
 * - JSON: Full run manifest (byte-for-byte identical to backend response)
 * 
 * CSV Column Documentation:
 * 
 * Window PnL CSV (run_{run_id}_window_pnl.csv):
 * - window_start_utc: ISO-8601 UTC timestamp of window start
 * - window_end_utc: ISO-8601 UTC timestamp of window end
 * - market_id: Market identifier string
 * - gross_pnl_usd: Gross PnL in USD before fees
 * - fees_usd: Total fees in USD
 * - settlement_transfer_usd: Settlement transfer amount in USD
 * - net_pnl_usd: Net PnL in USD after fees
 * - trades_count: Number of trades in window
 * - maker_fills_count: Number of maker fills
 * - taker_fills_count: Number of taker fills
 * - total_volume_usd: Total traded volume in USD
 * - start_price: Price at window start (if available)
 * - end_price: Price at window end (if available)
 * - outcome: Settlement outcome (if available)
 * - is_finalized: Whether window is finalized (true/false)
 * 
 * Equity Curve CSV (run_{run_id}_equity.csv):
 * - timestamp_utc: ISO-8601 UTC timestamp
 * - equity_usd: Total equity value in USD
 * - cash_balance_usd: Cash balance in USD
 * - position_value_usd: Position value in USD
 * - drawdown_usd: Drawdown value in USD
 * - drawdown_bps: Drawdown in basis points
 */

import React, { useState, useCallback, useMemo } from 'react';
import type {
  CertifiedBacktestResults,
  TrustLevel,
  EquityPoint,
  WindowPnL,
} from '../../types/backtest';
import { api } from '../../services/api';

// =============================================================================
// TYPES
// =============================================================================

interface ExportControlsProps {
  runId: string;
  results: CertifiedBacktestResults | null;
  trustLevel: TrustLevel | undefined;
  manifestHash?: string;
  className?: string;
}

type ExportType = 'window_pnl_csv' | 'equity_csv' | 'manifest_json';

interface ExportState {
  type: ExportType;
  status: 'idle' | 'loading' | 'success' | 'error';
  error?: string;
}

// =============================================================================
// CSV GENERATION UTILITIES
// =============================================================================

/**
 * Convert nanoseconds timestamp to ISO-8601 UTC string.
 * Preserves exact precision without client-side rounding.
 */
function nsToIso8601(ns: number): string {
  if (!Number.isFinite(ns) || ns <= 0) return '';
  try {
    const ms = ns / 1_000_000;
    return new Date(ms).toISOString();
  } catch {
    return '';
  }
}

/**
 * Escape a CSV field value according to RFC 4180.
 * - Fields containing commas, quotes, or newlines are quoted
 * - Quotes within fields are escaped by doubling
 */
function escapeCsvField(value: string | number | boolean | null | undefined): string {
  if (value === null || value === undefined) return '';
  const str = String(value);
  if (str.includes(',') || str.includes('"') || str.includes('\n') || str.includes('\r')) {
    return `"${str.replace(/"/g, '""')}"`;
  }
  return str;
}

/**
 * Generate CSV content from an array of objects with explicit headers.
 * Preserves exact values from backend without transformation.
 */
function generateCsv<T>(
  headers: Array<{ key: keyof T; label: string }>,
  rows: T[]
): string {
  const headerLine = headers.map((h) => escapeCsvField(h.label)).join(',');
  const dataLines = rows.map((row) =>
    headers.map((h) => escapeCsvField(row[h.key] as string | number | boolean | null | undefined)).join(',')
  );
  return [headerLine, ...dataLines].join('\r\n');
}

// =============================================================================
// WINDOW PNL CSV GENERATION
// =============================================================================

/**
 * CSV headers for per-window PnL export.
 * Labels include explicit units for clarity.
 */
const WINDOW_PNL_HEADERS: Array<{ key: keyof WindowPnLCsvRow; label: string }> = [
  { key: 'window_start_utc', label: 'window_start_utc' },
  { key: 'window_end_utc', label: 'window_end_utc' },
  { key: 'market_id', label: 'market_id' },
  { key: 'gross_pnl_usd', label: 'gross_pnl_usd' },
  { key: 'fees_usd', label: 'fees_usd' },
  { key: 'settlement_transfer_usd', label: 'settlement_transfer_usd' },
  { key: 'net_pnl_usd', label: 'net_pnl_usd' },
  { key: 'trades_count', label: 'trades_count' },
  { key: 'maker_fills_count', label: 'maker_fills_count' },
  { key: 'taker_fills_count', label: 'taker_fills_count' },
  { key: 'total_volume_usd', label: 'total_volume_usd' },
  { key: 'start_price', label: 'start_price' },
  { key: 'end_price', label: 'end_price' },
  { key: 'outcome', label: 'outcome' },
  { key: 'is_finalized', label: 'is_finalized' },
];

interface WindowPnLCsvRow {
  window_start_utc: string;
  window_end_utc: string;
  market_id: string;
  gross_pnl_usd: number;
  fees_usd: number;
  settlement_transfer_usd: number;
  net_pnl_usd: number;
  trades_count: number;
  maker_fills_count: number;
  taker_fills_count: number;
  total_volume_usd: number;
  start_price: number | string;
  end_price: number | string;
  outcome: string;
  is_finalized: boolean;
}

/**
 * Convert backend WindowPnL array to CSV rows.
 * Preserves exact ordering and values from backend.
 */
function windowPnLToCsvRows(windows: WindowPnL[]): WindowPnLCsvRow[] {
  return windows.map((w) => ({
    window_start_utc: nsToIso8601(w.window_start_ns),
    window_end_utc: nsToIso8601(w.window_end_ns),
    market_id: w.market_id,
    gross_pnl_usd: w.gross_pnl,
    fees_usd: w.fees,
    settlement_transfer_usd: w.settlement_transfer,
    net_pnl_usd: w.net_pnl,
    trades_count: w.trades_count,
    maker_fills_count: w.maker_fills_count,
    taker_fills_count: w.taker_fills_count,
    total_volume_usd: w.total_volume,
    start_price: w.start_price ?? '',
    end_price: w.end_price ?? '',
    outcome: w.outcome ?? '',
    is_finalized: w.is_finalized,
  }));
}

// =============================================================================
// EQUITY CURVE CSV GENERATION
// =============================================================================

/**
 * CSV headers for equity curve export.
 * Labels include explicit units for clarity.
 */
const EQUITY_CURVE_HEADERS: Array<{ key: keyof EquityCurveCsvRow; label: string }> = [
  { key: 'timestamp_utc', label: 'timestamp_utc' },
  { key: 'equity_usd', label: 'equity_usd' },
  { key: 'cash_balance_usd', label: 'cash_balance_usd' },
  { key: 'position_value_usd', label: 'position_value_usd' },
  { key: 'drawdown_usd', label: 'drawdown_usd' },
  { key: 'drawdown_bps', label: 'drawdown_bps' },
];

interface EquityCurveCsvRow {
  timestamp_utc: string;
  equity_usd: number;
  cash_balance_usd: number;
  position_value_usd: number;
  drawdown_usd: number;
  drawdown_bps: number;
}

/**
 * Convert backend EquityPoint array to CSV rows.
 * Preserves exact ordering and values from backend.
 */
function equityCurveToCsvRows(points: EquityPoint[]): EquityCurveCsvRow[] {
  return points.map((p) => ({
    timestamp_utc: nsToIso8601(p.time_ns),
    equity_usd: p.equity_value,
    cash_balance_usd: p.cash_balance,
    position_value_usd: p.position_value,
    drawdown_usd: p.drawdown_value,
    drawdown_bps: p.drawdown_bps,
  }));
}

// =============================================================================
// FILE DOWNLOAD UTILITY
// =============================================================================

/**
 * Trigger a file download in the browser.
 */
function downloadFile(content: string | Blob, filename: string, mimeType: string): void {
  const blob = content instanceof Blob ? content : new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// =============================================================================
// EXPORT BUTTON COMPONENT
// =============================================================================

interface ExportButtonProps {
  label: string;
  onClick: () => void;
  disabled: boolean;
  loading: boolean;
  success?: boolean;
  error?: string;
}

const ExportButton: React.FC<ExportButtonProps> = ({
  label,
  onClick,
  disabled,
  loading,
  success,
  error,
}) => {
  const baseClasses =
    'flex items-center gap-2 px-3 py-2 border font-mono text-[11px] tracking-widest transition-colors';
  
  let stateClasses: string;
  let icon: string;
  
  if (loading) {
    stateClasses = 'border-grey/30 bg-grey/5 text-fg/50 cursor-wait';
    icon = '⟳';
  } else if (error) {
    stateClasses = 'border-danger/50 bg-danger/5 text-danger cursor-pointer hover:bg-danger/10';
    icon = '✗';
  } else if (success) {
    stateClasses = 'border-success/50 bg-success/5 text-success';
    icon = '✓';
  } else if (disabled) {
    stateClasses = 'border-grey/20 bg-grey/5 text-fg/30 cursor-not-allowed';
    icon = '↓';
  } else {
    stateClasses = 'border-accent/50 bg-accent/5 text-accent hover:bg-accent/10 cursor-pointer';
    icon = '↓';
  }

  return (
    <button
      onClick={onClick}
      disabled={disabled || loading}
      className={`${baseClasses} ${stateClasses}`}
      title={error || undefined}
    >
      <span className="text-sm" aria-hidden="true">
        {icon}
      </span>
      <span>{loading ? 'EXPORTING...' : label}</span>
    </button>
  );
};

// =============================================================================
// MAIN COMPONENT
// =============================================================================

export const ExportControls: React.FC<ExportControlsProps> = ({
  runId,
  results,
  trustLevel,
  manifestHash,
  className,
}) => {
  const [exportStates, setExportStates] = useState<Record<ExportType, ExportState>>({
    window_pnl_csv: { type: 'window_pnl_csv', status: 'idle' },
    equity_csv: { type: 'equity_csv', status: 'idle' },
    manifest_json: { type: 'manifest_json', status: 'idle' },
  });

  // Determine if exports are allowed
  const canExport = useMemo(() => {
    // Only allow exports for trusted runs with loaded data
    if (!results) return false;
    if (trustLevel === 'Untrusted' || trustLevel === 'Unknown') return false;
    return true;
  }, [results, trustLevel]);

  const hasWindowPnL = useMemo(
    () => (results?.window_pnl?.windows?.length ?? 0) > 0,
    [results?.window_pnl?.windows?.length]
  );

  const hasEquityCurve = useMemo(
    () => (results?.equity_curve?.length ?? 0) > 0,
    [results?.equity_curve?.length]
  );

  // Update export state helper
  const setExportState = useCallback((type: ExportType, state: Partial<ExportState>) => {
    setExportStates((prev) => ({
      ...prev,
      [type]: { ...prev[type], ...state },
    }));
  }, []);

  // Reset success state after a delay
  const resetSuccessState = useCallback((type: ExportType) => {
    setTimeout(() => {
      setExportState(type, { status: 'idle' });
    }, 2000);
  }, [setExportState]);

  // Export per-window PnL as CSV
  const handleExportWindowPnL = useCallback(async () => {
    if (!results?.window_pnl?.windows || !canExport) return;

    setExportState('window_pnl_csv', { status: 'loading', error: undefined });

    try {
      const rows = windowPnLToCsvRows(results.window_pnl.windows);
      const csv = generateCsv(WINDOW_PNL_HEADERS, rows);
      const filename = `run_${runId}_window_pnl.csv`;
      downloadFile(csv, filename, 'text/csv;charset=utf-8');
      setExportState('window_pnl_csv', { status: 'success' });
      resetSuccessState('window_pnl_csv');
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Export failed';
      setExportState('window_pnl_csv', { status: 'error', error: msg });
    }
  }, [results?.window_pnl?.windows, canExport, runId, setExportState, resetSuccessState]);

  // Export equity curve as CSV
  const handleExportEquityCurve = useCallback(async () => {
    if (!results?.equity_curve || !canExport) return;

    setExportState('equity_csv', { status: 'loading', error: undefined });

    try {
      const rows = equityCurveToCsvRows(results.equity_curve);
      const csv = generateCsv(EQUITY_CURVE_HEADERS, rows);
      const filename = `run_${runId}_equity.csv`;
      downloadFile(csv, filename, 'text/csv;charset=utf-8');
      setExportState('equity_csv', { status: 'success' });
      resetSuccessState('equity_csv');
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Export failed';
      setExportState('equity_csv', { status: 'error', error: msg });
    }
  }, [results?.equity_curve, canExport, runId, setExportState, resetSuccessState]);

  // Export manifest as JSON (byte-for-byte identical to backend)
  const handleExportManifest = useCallback(async () => {
    if (!canExport) return;

    setExportState('manifest_json', { status: 'loading', error: undefined });

    try {
      // Fetch raw manifest from backend to ensure byte-for-byte identical export
      const response = await fetch(api.getManifestUrl(runId));
      if (!response.ok) {
        throw new Error(`Failed to fetch manifest: ${response.status}`);
      }
      
      // Get the raw text to preserve exact formatting
      const manifestText = await response.text();
      const filename = `run_${runId}_manifest.json`;
      downloadFile(manifestText, filename, 'application/json;charset=utf-8');
      setExportState('manifest_json', { status: 'success' });
      resetSuccessState('manifest_json');
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Export failed';
      setExportState('manifest_json', { status: 'error', error: msg });
    }
  }, [canExport, runId, setExportState, resetSuccessState]);

  // Don't render if no results
  if (!results) {
    return null;
  }

  const windowPnLState = exportStates.window_pnl_csv;
  const equityState = exportStates.equity_csv;
  const manifestState = exportStates.manifest_json;

  return (
    <div
      className={`export-controls bg-surface border border-grey/20 ${className ?? ''}`}
      data-testid="export-controls"
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-grey/10">
        <div className="flex items-center gap-2">
          <span className="text-[11px] font-mono text-fg/90 tracking-widest">
            EXPORT ARTIFACTS
          </span>
          {manifestHash && (
            <span className="text-[9px] font-mono text-fg/50 tracking-wider">
              [{manifestHash.slice(0, 8)}]
            </span>
          )}
        </div>
        {!canExport && (
          <span className="text-[9px] font-mono text-warning tracking-wider">
            EXPORTS DISABLED
          </span>
        )}
      </div>

      {/* Export Buttons */}
      <div className="p-4 space-y-3">
        <div className="flex flex-wrap gap-3">
          <ExportButton
            label="Download per-window PnL (CSV)"
            onClick={handleExportWindowPnL}
            disabled={!canExport || !hasWindowPnL}
            loading={windowPnLState.status === 'loading'}
            success={windowPnLState.status === 'success'}
            error={windowPnLState.error}
          />
          <ExportButton
            label="Download equity curve (CSV)"
            onClick={handleExportEquityCurve}
            disabled={!canExport || !hasEquityCurve}
            loading={equityState.status === 'loading'}
            success={equityState.status === 'success'}
            error={equityState.error}
          />
          <ExportButton
            label="Download run manifest (JSON)"
            onClick={handleExportManifest}
            disabled={!canExport}
            loading={manifestState.status === 'loading'}
            success={manifestState.status === 'success'}
            error={manifestState.error}
          />
        </div>

        {/* Immutability Notice */}
        <p className="text-[9px] text-fg/50 font-mono leading-relaxed mt-3">
          Exports reflect the certified backtest artifact and are immutable.
          {manifestHash && (
            <span className="ml-1">
              Manifest hash: <span className="text-fg/70">{manifestHash}</span>
            </span>
          )}
        </p>

        {/* Trust Warning */}
        {!canExport && trustLevel && (
          <div className="mt-2 px-3 py-2 bg-warning/10 border border-warning/30 text-[10px] font-mono text-warning">
            Exports are only available for trusted, published runs.
            {trustLevel === 'Untrusted' && ' This run is marked as untrusted.'}
            {trustLevel === 'Unknown' && ' Trust status is unknown.'}
          </div>
        )}
      </div>
    </div>
  );
};

export default ExportControls;
