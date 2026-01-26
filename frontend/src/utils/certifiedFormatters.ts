/**
 * Certified Formatters for Backtest Result Display
 * 
 * These formatters are designed for institutional-grade correctness:
 * 1. Explicit rounding rules documented and consistent
 * 2. Never round before aggregation (backend does aggregation)
 * 3. Units are ALWAYS explicit in the output
 * 4. No silent truncation or floating-point recomputation
 * 5. Safe handling of edge cases (NaN, Infinity, null, undefined)
 * 
 * ROUNDING RULES:
 * - Currency (USD): 2 decimal places for display
 * - Percentages: 2 decimal places for display
 * - Ratios (Sharpe, PF): 2 decimal places
 * - Basis points: 0 decimal places (integers)
 * - Counts: 0 decimal places (integers)
 */

// =============================================================================
// TYPES
// =============================================================================

export type NumericInput = number | null | undefined;

export interface FormatOptions {
  /** Number of decimal places (default varies by format type) */
  decimals?: number;
  /** Whether to include the unit suffix (default: true) */
  includeUnit?: boolean;
  /** Whether to show explicit sign for positive values (default: false) */
  explicitSign?: boolean;
  /** Fallback string for invalid values (default: '---') */
  fallback?: string;
}

export type PnLContext = 'per_window' | 'cumulative' | 'gross' | 'net' | 'fees';

// =============================================================================
// VALIDATION
// =============================================================================

/**
 * Check if a value is a finite number.
 */
export function isValidNumber(value: NumericInput): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * Safely coerce a value to a number, returning null if invalid.
 */
export function toNumber(value: NumericInput): number | null {
  if (value === null || value === undefined) return null;
  const n = Number(value);
  return isValidNumber(n) ? n : null;
}

// =============================================================================
// CORE FORMATTERS
// =============================================================================

/**
 * Format a currency value in USD.
 * 
 * Rounding rule: 2 decimal places, half-to-even (banker's rounding)
 * Unit: Always includes '$' prefix
 */
export function formatUsd(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { decimals = 2, includeUnit = true, explicitSign = false, fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  const sign = value < 0 ? '-' : (explicitSign && value > 0 ? '+' : '');
  const absValue = Math.abs(value);
  
  // Use toFixed for consistent rounding (JavaScript's default is round-half-away-from-zero)
  const formatted = absValue.toLocaleString('en-US', {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  });
  
  return includeUnit ? `${sign}$${formatted}` : `${sign}${formatted}`;
}

/**
 * Format a percentage value.
 * Input is expected as a decimal (0.15 = 15%).
 * 
 * Rounding rule: 2 decimal places by default
 * Unit: Always includes '%' suffix
 */
export function formatPercent(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { decimals = 2, includeUnit = true, explicitSign = false, fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  const pctValue = value * 100;
  const sign = pctValue < 0 ? '-' : (explicitSign && pctValue > 0 ? '+' : '');
  const formatted = Math.abs(pctValue).toFixed(decimals);
  
  return includeUnit ? `${sign}${formatted}%` : `${sign}${formatted}`;
}

/**
 * Format a percentage value that is already in percent form (e.g., 15 = 15%).
 */
export function formatPercentRaw(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { decimals = 2, includeUnit = true, explicitSign = false, fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  const sign = value < 0 ? '-' : (explicitSign && value > 0 ? '+' : '');
  const formatted = Math.abs(value).toFixed(decimals);
  
  return includeUnit ? `${sign}${formatted}%` : `${sign}${formatted}`;
}

/**
 * Format basis points.
 * Input is expected as basis points (100 = 1%).
 * 
 * Rounding rule: 0 decimal places (integers)
 * Unit: Always includes 'bps' suffix
 */
export function formatBps(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { decimals = 0, includeUnit = true, explicitSign = false, fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  const sign = value < 0 ? '-' : (explicitSign && value > 0 ? '+' : '');
  const formatted = Math.abs(value).toFixed(decimals);
  
  return includeUnit ? `${sign}${formatted} bps` : `${sign}${formatted}`;
}

/**
 * Format a ratio (Sharpe, profit factor, etc.).
 * 
 * Rounding rule: 2 decimal places
 * Unit: No unit (dimensionless)
 */
export function formatRatio(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { decimals = 2, explicitSign = false, fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  const sign = value < 0 ? '-' : (explicitSign && value > 0 ? '+' : '');
  const formatted = Math.abs(value).toFixed(decimals);
  
  return `${sign}${formatted}`;
}

/**
 * Format a count (number of trades, windows, etc.).
 * 
 * Rounding rule: 0 decimal places (integers)
 * Unit: No unit
 */
export function formatCount(
  value: NumericInput,
  options: FormatOptions = {}
): string {
  const { fallback = '---' } = options;
  
  if (!isValidNumber(value)) return fallback;
  
  return Math.round(value).toLocaleString('en-US');
}

// =============================================================================
// CONTEXT-AWARE FORMATTERS
// =============================================================================

/**
 * Format PnL with explicit context label.
 * This ensures the UI never silently switches between per-window and cumulative.
 */
export function formatPnLWithContext(
  value: NumericInput,
  context: PnLContext,
  options: FormatOptions = {}
): { value: string; label: string; unit: string } {
  const formatted = formatUsd(value, { ...options, explicitSign: true });
  
  const labels: Record<PnLContext, string> = {
    per_window: 'Per-Window',
    cumulative: 'Cumulative',
    gross: 'Gross',
    net: 'Net',
    fees: 'Fees',
  };
  
  return {
    value: formatted,
    label: labels[context],
    unit: 'USD',
  };
}

/**
 * Format equity value with explicit label.
 */
export function formatEquity(
  value: NumericInput,
  options: FormatOptions = {}
): { value: string; unit: string } {
  return {
    value: formatUsd(value, options),
    unit: 'USD',
  };
}

/**
 * Format drawdown with explicit sign (always negative or zero).
 */
export function formatDrawdown(
  value: NumericInput,
  asPercent: boolean = false,
  options: FormatOptions = {}
): string {
  if (!isValidNumber(value)) return options.fallback || '---';
  
  // Drawdown should always be shown as negative (or zero)
  const absValue = Math.abs(value);
  
  if (asPercent) {
    return `-${absValue.toFixed(options.decimals ?? 2)}%`;
  }
  return formatUsd(-absValue, { ...options, explicitSign: false });
}

// =============================================================================
// AXIS LABEL FORMATTERS (for charts)
// =============================================================================

/**
 * Format a value for chart axis label with appropriate abbreviation.
 * Does NOT silently rescale - abbreviation is explicit in the output.
 */
export function formatAxisLabel(
  value: NumericInput,
  unit: 'USD' | 'percent' | 'bps' | 'ratio'
): string {
  if (!isValidNumber(value)) return '---';
  
  const absValue = Math.abs(value);
  const sign = value < 0 ? '-' : '';
  
  switch (unit) {
    case 'USD': {
      if (absValue >= 1_000_000) {
        return `${sign}$${(absValue / 1_000_000).toFixed(1)}M`;
      }
      if (absValue >= 1_000) {
        return `${sign}$${(absValue / 1_000).toFixed(1)}K`;
      }
      return `${sign}$${absValue.toFixed(0)}`;
    }
    case 'percent':
      return `${sign}${absValue.toFixed(1)}%`;
    case 'bps':
      return `${sign}${Math.round(absValue)} bps`;
    case 'ratio':
      return `${sign}${absValue.toFixed(2)}`;
  }
}

/**
 * Get the unit label for chart axes.
 */
export function getAxisUnitLabel(unit: 'USD' | 'percent' | 'bps' | 'ratio'): string {
  switch (unit) {
    case 'USD': return 'USD';
    case 'percent': return '%';
    case 'bps': return 'bps';
    case 'ratio': return '';
  }
}

// =============================================================================
// VALIDATION / INVARIANT CHECKS
// =============================================================================

/**
 * Verify that cumulative PnL equals sum of per-window PnL.
 * Returns null if valid, or an error message if mismatch detected.
 */
export function verifyCumulativePnL(
  perWindowPnLs: number[],
  reportedCumulative: number,
  toleranceBps: number = 1 // Allow 0.01% tolerance for floating point
): string | null {
  const sum = perWindowPnLs.reduce((acc, v) => acc + v, 0);
  const diff = Math.abs(sum - reportedCumulative);
  const tolerance = Math.abs(reportedCumulative) * (toleranceBps / 10000);
  
  if (diff > Math.max(tolerance, 0.01)) { // At least 1 cent tolerance
    return `Cumulative PnL mismatch: sum of windows (${formatUsd(sum)}) != reported (${formatUsd(reportedCumulative)})`;
  }
  return null;
}

/**
 * Verify that net PnL = gross PnL - fees.
 */
export function verifyNetPnL(
  grossPnL: number,
  fees: number,
  reportedNetPnL: number,
  toleranceBps: number = 1
): string | null {
  const expected = grossPnL - fees;
  const diff = Math.abs(expected - reportedNetPnL);
  const tolerance = Math.abs(reportedNetPnL) * (toleranceBps / 10000);
  
  if (diff > Math.max(tolerance, 0.01)) {
    return `Net PnL mismatch: gross (${formatUsd(grossPnL)}) - fees (${formatUsd(fees)}) = ${formatUsd(expected)} != reported (${formatUsd(reportedNetPnL)})`;
  }
  return null;
}

/**
 * Verify that equity curve starts at expected initial capital.
 */
export function verifyEquityCurveStart(
  firstEquity: number,
  initialCapital: number,
  tolerancePct: number = 0.01 // Allow 0.01% tolerance
): string | null {
  const diff = Math.abs(firstEquity - initialCapital);
  const tolerance = initialCapital * (tolerancePct / 100);
  
  if (diff > Math.max(tolerance, 0.01)) {
    return `Equity curve start mismatch: first point (${formatUsd(firstEquity)}) != initial capital (${formatUsd(initialCapital)})`;
  }
  return null;
}

// =============================================================================
// EXPORT FORMATTERS
// =============================================================================

/**
 * Format a value for CSV/JSON export (full precision, no abbreviation).
 */
export function formatForExport(value: NumericInput): string {
  if (!isValidNumber(value)) return '';
  return value.toString();
}

/**
 * Format timestamp for export (ISO 8601).
 */
export function formatTimestampForExport(ns: number): string {
  try {
    const ms = ns > 1e15 ? Math.floor(ns / 1_000_000) : ns;
    return new Date(ms).toISOString();
  } catch {
    return '';
  }
}
