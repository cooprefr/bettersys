/**
 * Settlement Window Display Component
 * 
 * Displays settlement windows with:
 * 1. Exact start/end boundaries (no snapping/rounding)
 * 2. UTC timestamps as canonical
 * 3. Display timezone conversion (user-selected)
 * 4. Tooltips showing both UTC and display timezone
 * 5. Visual indication when windows cross calendar day boundaries
 * 
 * INVARIANTS:
 * - Window boundaries are NEVER modified
 * - UTC is always shown as source of truth
 * - Timezone conversion is presentation-only
 */

import React, { useMemo } from 'react';
import type { WindowPnL } from '../../types/backtest';
import {
  formatUtcDatetime,
  formatUtcTime,
  formatWindowRange,
  getWindowTooltip,
  getTimezoneConfig,
  type DisplayTimezone,
} from '../../utils/timezone';
import {
  formatUsd,
  isValidNumber,
} from '../../utils/certifiedFormatters';
import { PnLDisplay, CountDisplay } from './UnitSafeDisplay';

// =============================================================================
// TYPES
// =============================================================================

export interface SettlementWindowDisplayProps {
  window: WindowPnL;
  /** Show compact inline view vs detailed view */
  compact?: boolean;
  /** Override display timezone */
  displayTz?: DisplayTimezone;
  /** Optional onClick handler */
  onClick?: () => void;
  className?: string;
  testId?: string;
}

export interface WindowTimeRangeProps {
  startNs: number;
  endNs: number;
  displayTz?: DisplayTimezone;
  showDate?: boolean;
  className?: string;
}

export interface WindowTableProps {
  windows: WindowPnL[];
  displayTz?: DisplayTimezone;
  onWindowClick?: (window: WindowPnL) => void;
  className?: string;
  /** Number of windows to show (pagination) */
  limit?: number;
  /** Starting offset for pagination */
  offset?: number;
}

// =============================================================================
// WINDOW TIME RANGE COMPONENT
// =============================================================================

export const WindowTimeRange: React.FC<WindowTimeRangeProps> = ({
  startNs,
  endNs,
  displayTz,
  showDate = false,
  className = '',
}) => {
  const config = useMemo(
    () => displayTz ? { displayTz, showUtcInTooltips: true } : getTimezoneConfig(),
    [displayTz]
  );
  
  const { utc, display, crossesMidnight } = useMemo(
    () => formatWindowRange(startNs, endNs, config),
    [startNs, endNs, config]
  );
  
  const tooltip = useMemo(
    () => getWindowTooltip(startNs, endNs, config),
    [startNs, endNs, config]
  );
  
  const utcTimeOnly = `${formatUtcTime(startNs)} - ${formatUtcTime(endNs)} UTC`;
  
  return (
    <span
      className={`font-mono text-[11px] ${className}`}
      title={tooltip}
      aria-label={tooltip}
    >
      {showDate ? (
        <span className="text-fg/90">{utc}</span>
      ) : (
        <span className="text-fg/90">{utcTimeOnly}</span>
      )}
      {display && config.displayTz !== 'UTC' && (
        <span className="text-fg/50 ml-2 text-[10px]">
          ({display})
          {crossesMidnight && (
            <span className="text-warning/70 ml-1" title="Window crosses midnight in display timezone">
              ⌀
            </span>
          )}
        </span>
      )}
    </span>
  );
};

// =============================================================================
// SINGLE WINDOW DISPLAY
// =============================================================================

export const SettlementWindowDisplay: React.FC<SettlementWindowDisplayProps> = ({
  window: w,
  compact = false,
  displayTz,
  onClick,
  className = '',
  testId,
}) => {
  const tooltip = useMemo(
    () => getWindowTooltip(
      w.window_start_ns,
      w.window_end_ns,
      displayTz ? { displayTz, showUtcInTooltips: true } : undefined
    ),
    [w.window_start_ns, w.window_end_ns, displayTz]
  );
  
  const isProfit = w.net_pnl >= 0;
  const pnlIcon = isProfit ? '▲' : '▼';
  const pnlColor = isProfit ? 'text-success' : 'text-danger';
  
  if (compact) {
    return (
      <div
        className={`flex items-center gap-3 py-2 px-3 bg-surface border border-grey/10 
                    hover:border-grey/20 transition-colors cursor-pointer ${className}`}
        onClick={onClick}
        title={tooltip}
        data-testid={testId}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => e.key === 'Enter' && onClick?.()}
        aria-label={`Settlement window ${formatUtcDatetime(w.window_start_ns)} to ${formatUtcDatetime(w.window_end_ns)}, Net PnL: ${formatUsd(w.net_pnl)}`}
      >
        <WindowTimeRange
          startNs={w.window_start_ns}
          endNs={w.window_end_ns}
          displayTz={displayTz}
        />
        <span className="flex-grow" />
        <span className={`font-mono ${pnlColor}`}>
          <span aria-hidden="true" className="text-[0.8em] mr-1">{pnlIcon}</span>
          {formatUsd(w.net_pnl, { explicitSign: true })}
        </span>
        {!w.is_finalized && (
          <span className="text-warning/70 text-[9px] tracking-widest" title="Window not yet finalized">
            PENDING
          </span>
        )}
      </div>
    );
  }
  
  // Detailed view
  return (
    <div
      className={`bg-surface border border-grey/10 p-4 ${className}`}
      data-testid={testId}
      role="article"
      aria-label={`Settlement window details`}
    >
      {/* Header with time range */}
      <div className="flex items-center justify-between mb-3 pb-2 border-b border-grey/10">
        <WindowTimeRange
          startNs={w.window_start_ns}
          endNs={w.window_end_ns}
          displayTz={displayTz}
          showDate
        />
        <div className="flex items-center gap-2">
          {w.outcome && (
            <span className={`text-[10px] tracking-widest px-2 py-0.5 border ${
              w.outcome === 'Up' ? 'border-success/40 text-success' : 'border-danger/40 text-danger'
            }`}>
              {w.outcome.toUpperCase()}
            </span>
          )}
          {!w.is_finalized && (
            <span className="text-[10px] tracking-widest px-2 py-0.5 border border-warning/40 text-warning">
              PENDING
            </span>
          )}
        </div>
      </div>
      
      {/* Market info */}
      <div className="text-[11px] font-mono text-fg/70 mb-3">
        {w.market_id}
      </div>
      
      {/* PnL breakdown */}
      <div className="grid grid-cols-2 gap-3 text-[11px] font-mono">
        <div>
          <div className="text-fg/50 mb-1">GROSS PnL (USD)</div>
          <PnLDisplay value={w.gross_pnl} context="gross" />
        </div>
        <div>
          <div className="text-fg/50 mb-1">FEES (USD)</div>
          <PnLDisplay value={w.fees} context="fees" />
        </div>
        <div>
          <div className="text-fg/50 mb-1">SETTLEMENT (USD)</div>
          <span className="text-fg">{formatUsd(w.settlement_transfer)}</span>
        </div>
        <div>
          <div className="text-fg/50 mb-1">NET PnL (USD)</div>
          <PnLDisplay value={w.net_pnl} context="net" />
        </div>
      </div>
      
      {/* Trade details */}
      <div className="grid grid-cols-3 gap-3 text-[11px] font-mono mt-3 pt-3 border-t border-grey/10">
        <div>
          <div className="text-fg/50 mb-1">TRADES</div>
          <CountDisplay value={w.trades_count} label="Total trades" />
        </div>
        <div>
          <div className="text-fg/50 mb-1">MAKER FILLS</div>
          <CountDisplay value={w.maker_fills_count} label="Maker fills" />
        </div>
        <div>
          <div className="text-fg/50 mb-1">TAKER FILLS</div>
          <CountDisplay value={w.taker_fills_count} label="Taker fills" />
        </div>
      </div>
      
      {/* Volume and prices */}
      <div className="grid grid-cols-3 gap-3 text-[11px] font-mono mt-3 pt-3 border-t border-grey/10">
        <div>
          <div className="text-fg/50 mb-1">VOLUME (USD)</div>
          <span className="text-fg">{formatUsd(w.total_volume)}</span>
        </div>
        {isValidNumber(w.start_price) && (
          <div>
            <div className="text-fg/50 mb-1">START PRICE</div>
            <span className="text-fg">{formatUsd(w.start_price, { decimals: 4 })}</span>
          </div>
        )}
        {isValidNumber(w.end_price) && (
          <div>
            <div className="text-fg/50 mb-1">END PRICE</div>
            <span className="text-fg">{formatUsd(w.end_price, { decimals: 4 })}</span>
          </div>
        )}
      </div>
    </div>
  );
};

// =============================================================================
// WINDOW TABLE COMPONENT
// =============================================================================

export const WindowTable: React.FC<WindowTableProps> = ({
  windows,
  displayTz,
  onWindowClick,
  className = '',
  limit = 50,
  offset = 0,
}) => {
  const visibleWindows = useMemo(
    () => windows.slice(offset, offset + limit),
    [windows, offset, limit]
  );
  
  if (windows.length === 0) {
    return (
      <div className="text-[11px] font-mono text-fg/60 p-4 text-center">
        NO SETTLEMENT WINDOWS
      </div>
    );
  }
  
  return (
    <div className={className}>
      {/* Table header */}
      <div
        className="grid grid-cols-[1fr_100px_100px_100px_100px_80px] gap-2 px-3 py-2 
                   text-[10px] font-mono text-fg/60 tracking-widest border-b border-grey/20"
        role="row"
        aria-label="Column headers"
      >
        <div>TIME (UTC)</div>
        <div className="text-right">GROSS</div>
        <div className="text-right">FEES</div>
        <div className="text-right">NET</div>
        <div className="text-right">VOLUME</div>
        <div className="text-right">STATUS</div>
      </div>
      
      {/* Table rows */}
      <div role="rowgroup">
        {visibleWindows.map((w, idx) => {
          const isProfit = w.net_pnl >= 0;
          const pnlColor = isProfit ? 'text-success' : 'text-danger';
          
          return (
            <div
              key={`${w.window_start_ns}-${w.market_id}`}
              className={`grid grid-cols-[1fr_100px_100px_100px_100px_80px] gap-2 px-3 py-2 
                         text-[11px] font-mono border-b border-grey/10 hover:bg-grey/5 
                         transition-colors ${onWindowClick ? 'cursor-pointer' : ''}`}
              onClick={() => onWindowClick?.(w)}
              role="row"
              tabIndex={onWindowClick ? 0 : undefined}
              onKeyDown={(e) => e.key === 'Enter' && onWindowClick?.(w)}
              aria-label={`Window ${idx + offset + 1}: ${formatUtcDatetime(w.window_start_ns)}, Net PnL ${formatUsd(w.net_pnl)}`}
            >
              <div className="text-fg/80 truncate">
                <WindowTimeRange
                  startNs={w.window_start_ns}
                  endNs={w.window_end_ns}
                  displayTz={displayTz}
                />
              </div>
              <div className="text-right text-fg/80">
                {formatUsd(w.gross_pnl, { explicitSign: true })}
              </div>
              <div className="text-right text-warning/80">
                {formatUsd(w.fees)}
              </div>
              <div className={`text-right ${pnlColor}`}>
                {formatUsd(w.net_pnl, { explicitSign: true })}
              </div>
              <div className="text-right text-fg/60">
                {formatUsd(w.total_volume)}
              </div>
              <div className="text-right">
                {w.is_finalized ? (
                  <span className="text-success/70">FINAL</span>
                ) : (
                  <span className="text-warning/70">PEND</span>
                )}
              </div>
            </div>
          );
        })}
      </div>
      
      {/* Pagination info */}
      {windows.length > limit && (
        <div className="text-[10px] font-mono text-fg/50 p-3 text-center">
          Showing {offset + 1}–{Math.min(offset + limit, windows.length)} of {windows.length} windows
        </div>
      )}
    </div>
  );
};

// =============================================================================
// WINDOW SUMMARY STATS
// =============================================================================

export interface WindowSummaryStatsProps {
  windows: WindowPnL[];
  className?: string;
}

export const WindowSummaryStats: React.FC<WindowSummaryStatsProps> = ({
  windows,
  className = '',
}) => {
  const stats = useMemo(() => {
    if (windows.length === 0) {
      return null;
    }
    
    const finalized = windows.filter(w => w.is_finalized);
    const pending = windows.length - finalized.length;
    
    const totalGross = windows.reduce((sum, w) => sum + w.gross_pnl, 0);
    const totalFees = windows.reduce((sum, w) => sum + w.fees, 0);
    const totalNet = windows.reduce((sum, w) => sum + w.net_pnl, 0);
    const totalVolume = windows.reduce((sum, w) => sum + w.total_volume, 0);
    
    const winning = windows.filter(w => w.net_pnl > 0).length;
    const losing = windows.filter(w => w.net_pnl < 0).length;
    const breakeven = windows.length - winning - losing;
    
    return {
      total: windows.length,
      finalized: finalized.length,
      pending,
      totalGross,
      totalFees,
      totalNet,
      totalVolume,
      winning,
      losing,
      breakeven,
      winRate: windows.length > 0 ? winning / windows.length : 0,
    };
  }, [windows]);
  
  if (!stats) {
    return null;
  }
  
  return (
    <div className={`grid grid-cols-2 md:grid-cols-4 gap-4 ${className}`}>
      <div className="bg-surface border border-grey/10 p-3">
        <div className="text-[10px] text-fg/60 tracking-widest mb-1">TOTAL WINDOWS</div>
        <div className="text-lg font-mono text-fg">
          <CountDisplay value={stats.total} label="Total windows" />
        </div>
        <div className="text-[10px] font-mono text-fg/50 mt-1">
          {stats.finalized} final / {stats.pending} pending
        </div>
      </div>
      
      <div className="bg-surface border border-grey/10 p-3">
        <div className="text-[10px] text-fg/60 tracking-widest mb-1">NET PnL (USD)</div>
        <PnLDisplay value={stats.totalNet} context="cumulative" className="text-lg" />
        <div className="text-[10px] font-mono text-fg/50 mt-1">
          Gross: {formatUsd(stats.totalGross)} / Fees: {formatUsd(stats.totalFees)}
        </div>
      </div>
      
      <div className="bg-surface border border-grey/10 p-3">
        <div className="text-[10px] text-fg/60 tracking-widest mb-1">WIN RATE</div>
        <div className={`text-lg font-mono ${stats.winRate >= 0.5 ? 'text-success' : 'text-danger'}`}>
          {(stats.winRate * 100).toFixed(1)}%
        </div>
        <div className="text-[10px] font-mono text-fg/50 mt-1">
          {stats.winning}W / {stats.losing}L / {stats.breakeven}BE
        </div>
      </div>
      
      <div className="bg-surface border border-grey/10 p-3">
        <div className="text-[10px] text-fg/60 tracking-widest mb-1">TOTAL VOLUME (USD)</div>
        <div className="text-lg font-mono text-fg">
          {formatUsd(stats.totalVolume)}
        </div>
      </div>
    </div>
  );
};
