/**
 * Unit-Safe Display Components for Backtest Results
 * 
 * These components enforce:
 * 1. Explicit units everywhere (no silent conversions)
 * 2. Accessible rendering with ARIA labels
 * 3. Tooltips showing full precision and units
 * 4. Color coding that doesn't rely solely on color (uses icons/labels)
 * 
 * ACCESSIBILITY:
 * - All values have sr-only descriptions
 * - Color + icon + sign for profit/loss
 * - High contrast ratios (WCAG AA minimum)
 */

import React from 'react';
import {
  formatUsd,
  formatPercent,
  formatBps,
  formatRatio,
  formatCount,
  formatDrawdown,
  isValidNumber,
  type NumericInput,
  type PnLContext,
} from '../../utils/certifiedFormatters';

// =============================================================================
// TYPES
// =============================================================================

export interface UnitSafeProps {
  /** Optional additional CSS classes */
  className?: string;
  /** Optional test ID */
  testId?: string;
}

export type ValueTone = 'positive' | 'negative' | 'neutral' | 'warning' | 'danger';

// =============================================================================
// UTILITY HOOKS/FUNCTIONS
// =============================================================================

function getToneClasses(tone: ValueTone): string {
  switch (tone) {
    case 'positive':
      return 'text-success';
    case 'negative':
    case 'danger':
      return 'text-danger';
    case 'warning':
      return 'text-warning';
    case 'neutral':
    default:
      return 'text-fg';
  }
}

function getToneIcon(tone: ValueTone): string {
  switch (tone) {
    case 'positive':
      return '▲';
    case 'negative':
    case 'danger':
      return '▼';
    case 'warning':
      return '⚠';
    default:
      return '';
  }
}

function getValueTone(value: NumericInput, zeroIsNeutral: boolean = true): ValueTone {
  if (!isValidNumber(value)) return 'neutral';
  if (value > 0) return 'positive';
  if (value < 0) return 'negative';
  return zeroIsNeutral ? 'neutral' : 'neutral';
}

// =============================================================================
// CURRENCY DISPLAY
// =============================================================================

export interface CurrencyDisplayProps extends UnitSafeProps {
  value: NumericInput;
  /** Label for screen readers and tooltips */
  label: string;
  /** Whether to show explicit +/- sign */
  showSign?: boolean;
  /** Whether to apply color based on value */
  colorize?: boolean;
  /** Override tone regardless of value */
  forceTone?: ValueTone;
  /** Show icon indicator alongside value */
  showIcon?: boolean;
  /** Number of decimal places */
  decimals?: number;
}

export const CurrencyDisplay: React.FC<CurrencyDisplayProps> = ({
  value,
  label,
  showSign = false,
  colorize = false,
  forceTone,
  showIcon = false,
  decimals = 2,
  className = '',
  testId,
}) => {
  const formatted = formatUsd(value, { decimals, explicitSign: showSign });
  const tone = forceTone || (colorize ? getValueTone(value) : 'neutral');
  const icon = showIcon ? getToneIcon(tone) : '';
  
  const ariaLabel = `${label}: ${formatted} USD`;
  
  return (
    <span
      className={`font-mono ${getToneClasses(tone)} ${className}`}
      aria-label={ariaLabel}
      title={`${label}: ${formatted} (USD)`}
      data-testid={testId}
    >
      {icon && <span aria-hidden="true" className="mr-1 text-[0.75em]">{icon}</span>}
      <span>{formatted}</span>
      <span className="sr-only"> USD</span>
    </span>
  );
};

// =============================================================================
// PNL DISPLAY (with explicit context)
// =============================================================================

export interface PnLDisplayProps extends UnitSafeProps {
  value: NumericInput;
  /** Context: per_window, cumulative, gross, net, fees */
  context: PnLContext;
  /** Whether to show the context label */
  showLabel?: boolean;
  /** Number of decimal places */
  decimals?: number;
}

const CONTEXT_LABELS: Record<PnLContext, string> = {
  per_window: 'Window PnL',
  cumulative: 'Cumulative PnL',
  gross: 'Gross PnL',
  net: 'Net PnL',
  fees: 'Fees',
};

export const PnLDisplay: React.FC<PnLDisplayProps> = ({
  value,
  context,
  showLabel = false,
  decimals = 2,
  className = '',
  testId,
}) => {
  const formatted = formatUsd(value, { decimals, explicitSign: true });
  const tone = context === 'fees' ? 'warning' : getValueTone(value);
  const label = CONTEXT_LABELS[context];
  const icon = context !== 'fees' ? getToneIcon(tone) : '';
  
  const ariaLabel = `${label}: ${formatted} USD`;
  
  return (
    <span
      className={`font-mono ${getToneClasses(tone)} ${className}`}
      aria-label={ariaLabel}
      title={`${label}: ${formatted} (USD, ${context.replace('_', ' ')})`}
      data-testid={testId}
    >
      {showLabel && (
        <span className="text-fg/60 mr-2 text-[0.85em]">{label}:</span>
      )}
      {icon && <span aria-hidden="true" className="mr-1 text-[0.75em]">{icon}</span>}
      <span>{formatted}</span>
      <span className="sr-only"> USD ({context.replace('_', ' ')})</span>
    </span>
  );
};

// =============================================================================
// PERCENTAGE DISPLAY
// =============================================================================

export interface PercentDisplayProps extends UnitSafeProps {
  /** Value as decimal (0.15 = 15%) */
  value: NumericInput;
  /** Label for screen readers */
  label: string;
  /** Whether to apply color based on value */
  colorize?: boolean;
  /** Override tone */
  forceTone?: ValueTone;
  /** Number of decimal places */
  decimals?: number;
  /** Whether to show explicit +/- sign */
  showSign?: boolean;
}

export const PercentDisplay: React.FC<PercentDisplayProps> = ({
  value,
  label,
  colorize = false,
  forceTone,
  decimals = 2,
  showSign = false,
  className = '',
  testId,
}) => {
  const formatted = formatPercent(value, { decimals, explicitSign: showSign });
  const tone = forceTone || (colorize ? getValueTone(value) : 'neutral');
  
  const ariaLabel = `${label}: ${formatted}`;
  
  return (
    <span
      className={`font-mono ${getToneClasses(tone)} ${className}`}
      aria-label={ariaLabel}
      title={`${label}: ${formatted}`}
      data-testid={testId}
    >
      {formatted}
    </span>
  );
};

// =============================================================================
// DRAWDOWN DISPLAY
// =============================================================================

export interface DrawdownDisplayProps extends UnitSafeProps {
  /** Absolute drawdown value (positive number will be shown as negative) */
  value: NumericInput;
  /** Optional percentage representation */
  percentValue?: NumericInput;
  /** Number of decimal places */
  decimals?: number;
}

export const DrawdownDisplay: React.FC<DrawdownDisplayProps> = ({
  value,
  percentValue,
  decimals = 2,
  className = '',
  testId,
}) => {
  const formattedUsd = formatDrawdown(value, false, { decimals });
  const formattedPct = percentValue !== undefined
    ? formatDrawdown(percentValue, true, { decimals })
    : null;
  
  const ariaLabel = `Maximum drawdown: ${formattedUsd} USD${formattedPct ? ` (${formattedPct})` : ''}`;
  
  return (
    <span
      className={`font-mono text-danger ${className}`}
      aria-label={ariaLabel}
      title={ariaLabel}
      data-testid={testId}
    >
      <span aria-hidden="true" className="mr-1 text-[0.75em]">▼</span>
      <span>{formattedUsd}</span>
      {formattedPct && (
        <span className="text-fg/60 ml-1 text-[0.85em]">({formattedPct})</span>
      )}
      <span className="sr-only"> USD drawdown</span>
    </span>
  );
};

// =============================================================================
// BASIS POINTS DISPLAY
// =============================================================================

export interface BpsDisplayProps extends UnitSafeProps {
  /** Value in basis points */
  value: NumericInput;
  /** Label for screen readers */
  label: string;
  /** Whether to apply color based on value */
  colorize?: boolean;
  /** Whether to show explicit +/- sign */
  showSign?: boolean;
}

export const BpsDisplay: React.FC<BpsDisplayProps> = ({
  value,
  label,
  colorize = false,
  showSign = false,
  className = '',
  testId,
}) => {
  const formatted = formatBps(value, { explicitSign: showSign });
  const tone = colorize ? getValueTone(value) : 'neutral';
  
  const ariaLabel = `${label}: ${formatted}`;
  
  return (
    <span
      className={`font-mono ${getToneClasses(tone)} ${className}`}
      aria-label={ariaLabel}
      title={`${label}: ${formatted}`}
      data-testid={testId}
    >
      {formatted}
    </span>
  );
};

// =============================================================================
// RATIO DISPLAY (Sharpe, Profit Factor)
// =============================================================================

export interface RatioDisplayProps extends UnitSafeProps {
  value: NumericInput;
  /** Label for the ratio (e.g., "Sharpe Ratio", "Profit Factor") */
  label: string;
  /** Reference value for coloring (e.g., 1.0 for profit factor) */
  neutralAt?: number;
  /** Number of decimal places */
  decimals?: number;
}

export const RatioDisplay: React.FC<RatioDisplayProps> = ({
  value,
  label,
  neutralAt = 0,
  decimals = 2,
  className = '',
  testId,
}) => {
  const formatted = formatRatio(value, { decimals });
  
  let tone: ValueTone = 'neutral';
  if (isValidNumber(value)) {
    if (value > neutralAt) tone = 'positive';
    else if (value < neutralAt) tone = 'negative';
  }
  
  const ariaLabel = `${label}: ${formatted}`;
  
  return (
    <span
      className={`font-mono ${getToneClasses(tone)} ${className}`}
      aria-label={ariaLabel}
      title={`${label}: ${formatted}`}
      data-testid={testId}
    >
      {formatted}
    </span>
  );
};

// =============================================================================
// COUNT DISPLAY
// =============================================================================

export interface CountDisplayProps extends UnitSafeProps {
  value: NumericInput;
  /** Label for screen readers */
  label: string;
  /** Optional "of total" value */
  total?: NumericInput;
}

export const CountDisplay: React.FC<CountDisplayProps> = ({
  value,
  label,
  total,
  className = '',
  testId,
}) => {
  const formatted = formatCount(value);
  const totalFormatted = total !== undefined ? formatCount(total) : null;
  
  const ariaLabel = totalFormatted
    ? `${label}: ${formatted} of ${totalFormatted}`
    : `${label}: ${formatted}`;
  
  return (
    <span
      className={`font-mono text-fg ${className}`}
      aria-label={ariaLabel}
      title={ariaLabel}
      data-testid={testId}
    >
      <span>{formatted}</span>
      {totalFormatted && (
        <span className="text-fg/60 ml-1">/ {totalFormatted}</span>
      )}
    </span>
  );
};

// =============================================================================
// WIN RATE DISPLAY
// =============================================================================

export interface WinRateDisplayProps extends UnitSafeProps {
  /** Win rate as decimal (0-1) */
  winRate: NumericInput;
  /** Number of winning periods */
  wins?: NumericInput;
  /** Total number of periods */
  total?: NumericInput;
  /** Threshold for "good" win rate (default 0.5) */
  goodThreshold?: number;
}

export const WinRateDisplay: React.FC<WinRateDisplayProps> = ({
  winRate,
  wins,
  total,
  goodThreshold = 0.5,
  className = '',
  testId,
}) => {
  const formatted = formatPercent(winRate, { decimals: 1 });
  const tone: ValueTone = isValidNumber(winRate)
    ? (winRate >= goodThreshold ? 'positive' : 'negative')
    : 'neutral';
  
  const subtext = wins !== undefined && total !== undefined
    ? `${formatCount(wins)}W / ${formatCount(total)}`
    : null;
  
  const ariaLabel = `Win rate: ${formatted}${subtext ? ` (${subtext} total)` : ''}`;
  
  return (
    <span
      className={`font-mono ${className}`}
      aria-label={ariaLabel}
      title={ariaLabel}
      data-testid={testId}
    >
      <span className={getToneClasses(tone)}>{formatted}</span>
      {subtext && (
        <span className="text-fg/60 text-[0.85em] ml-2">{subtext}</span>
      )}
    </span>
  );
};

// =============================================================================
// UNIT LABEL COMPONENT
// =============================================================================

export interface UnitLabelProps extends UnitSafeProps {
  /** The unit text (e.g., "USD", "bps", "%") */
  unit: string;
  /** Whether this is for an axis */
  isAxis?: boolean;
}

export const UnitLabel: React.FC<UnitLabelProps> = ({
  unit,
  isAxis = false,
  className = '',
  testId,
}) => (
  <span
    className={`text-fg/60 text-[0.75em] ${isAxis ? 'tracking-widest' : ''} ${className}`}
    aria-hidden="true"
    data-testid={testId}
  >
    ({unit})
  </span>
);

// =============================================================================
// METRIC CARD WITH UNIT
// =============================================================================

export interface MetricCardProps extends UnitSafeProps {
  /** Metric label */
  label: string;
  /** Formatted value (use one of the display components) */
  children: React.ReactNode;
  /** Optional subtitle */
  subtitle?: React.ReactNode;
  /** Optional unit label */
  unit?: string;
}

export const MetricCard: React.FC<MetricCardProps> = ({
  label,
  children,
  subtitle,
  unit,
  className = '',
  testId,
}) => (
  <div
    className={`bg-surface border border-grey/10 p-4 ${className}`}
    data-testid={testId}
    role="group"
    aria-label={label}
  >
    <div className="flex items-baseline gap-2 mb-2">
      <span className="text-[10px] text-fg/90 tracking-widest">{label}</span>
      {unit && <UnitLabel unit={unit} />}
    </div>
    <div className="text-xl">{children}</div>
    {subtitle && (
      <div className="text-[10px] font-mono text-fg/60 mt-1">{subtitle}</div>
    )}
  </div>
);
