/**
 * Display Invariants Test Suite
 * 
 * These tests verify that the UI maintains mathematical correctness
 * analogous to the backend invariants. The UI must never weaken the
 * auditability or trust guarantees of the backend.
 * 
 * INVARIANTS TESTED:
 * 1. Equity curve starts at initial capital
 * 2. Cumulative PnL equals sum of per-window PnL
 * 3. Net PnL = Gross PnL - Fees
 * 4. No silent unit conversions
 * 5. Timezone conversions are presentation-only
 * 6. Rounding is consistent across views
 */

import {
  verifyCumulativePnL,
  verifyNetPnL,
  verifyEquityCurveStart,
  formatUsd,
  formatPercent,
} from '../certifiedFormatters';
import {
  formatUtcDatetime,
  formatDisplayDatetime,
  nsToDate,
  setTimezoneConfig,
  resetTimezoneConfig,
} from '../timezone';

describe('Display Invariants', () => {
  beforeEach(() => {
    resetTimezoneConfig();
  });

  describe('Invariant 1: Equity curve starts at initial capital', () => {
    it('validates equity curve starting point', () => {
      // Valid case
      const validEquityCurve = [
        { time_ns: 1704067200000000000, equity_value: 10000 },
        { time_ns: 1704067900000000000, equity_value: 10050 },
        { time_ns: 1704068500000000000, equity_value: 10025 },
      ];
      
      expect(verifyEquityCurveStart(validEquityCurve[0].equity_value, 10000)).toBeNull();
    });

    it('detects mismatch in equity curve start', () => {
      const invalidStart = 9500; // Should be 10000
      const error = verifyEquityCurveStart(invalidStart, 10000);
      expect(error).not.toBeNull();
      expect(error).toContain('mismatch');
    });

    it('allows tolerance for floating point', () => {
      // Small floating point difference should be allowed
      expect(verifyEquityCurveStart(10000.001, 10000)).toBeNull();
    });
  });

  describe('Invariant 2: Cumulative PnL equals sum of per-window PnL', () => {
    it('validates cumulative matches sum', () => {
      const windowPnLs = [50.25, -20.10, 100.00, -30.50, 75.35];
      const expectedSum = windowPnLs.reduce((a, b) => a + b, 0); // 175.00
      
      expect(verifyCumulativePnL(windowPnLs, expectedSum)).toBeNull();
    });

    it('detects cumulative mismatch', () => {
      const windowPnLs = [100, 50, -30];
      const wrongCumulative = 200; // Should be 120
      
      const error = verifyCumulativePnL(windowPnLs, wrongCumulative);
      expect(error).not.toBeNull();
      expect(error).toContain('mismatch');
    });

    it('handles empty window list', () => {
      expect(verifyCumulativePnL([], 0)).toBeNull();
    });

    it('handles floating point accumulation', () => {
      // Classic floating point problem: 0.1 + 0.2 + 0.3 != 0.6
      const windowPnLs = [0.1, 0.2, 0.3];
      // Sum is not used - verifyCumulativePnL uses reduce internally with tolerance
      // Should still pass due to tolerance
      expect(verifyCumulativePnL(windowPnLs, 0.6)).toBeNull();
    });
  });

  describe('Invariant 3: Net PnL = Gross PnL - Fees', () => {
    it('validates net PnL equation', () => {
      const grossPnL = 1000;
      const fees = 25.50;
      const netPnL = 974.50;
      
      expect(verifyNetPnL(grossPnL, fees, netPnL)).toBeNull();
    });

    it('detects net PnL calculation error', () => {
      const grossPnL = 1000;
      const fees = 25;
      const wrongNetPnL = 1000; // Should be 975
      
      const error = verifyNetPnL(grossPnL, fees, wrongNetPnL);
      expect(error).not.toBeNull();
    });

    it('handles zero fees', () => {
      expect(verifyNetPnL(500, 0, 500)).toBeNull();
    });

    it('handles negative gross PnL', () => {
      expect(verifyNetPnL(-100, 10, -110)).toBeNull();
    });
  });

  describe('Invariant 4: No silent unit conversions', () => {
    it('USD formatting always includes dollar sign', () => {
      const formatted = formatUsd(1234.56);
      expect(formatted).toContain('$');
    });

    it('percentage formatting always includes percent sign', () => {
      const formatted = formatPercent(0.15);
      expect(formatted).toContain('%');
    });

    it('explicit sign option works correctly', () => {
      expect(formatUsd(100, { explicitSign: true })).toMatch(/^\+\$/);
      expect(formatUsd(-100, { explicitSign: true })).toMatch(/^-\$/);
    });

    it('unit can be excluded when explicitly requested', () => {
      const noUnit = formatUsd(100, { includeUnit: false });
      expect(noUnit).not.toContain('$');
    });
  });

  describe('Invariant 5: Timezone conversions are presentation-only', () => {
    it('UTC formatting is deterministic', () => {
      const ns = 1704110400000000000;
      
      // Multiple calls should always return same result
      const result1 = formatUtcDatetime(ns);
      const result2 = formatUtcDatetime(ns);
      expect(result1).toBe(result2);
    });

    it('changing display timezone does not affect underlying data', () => {
      const ns = 1704110400000000000;
      
      // Get UTC representation
      const utcResult = formatUtcDatetime(ns);
      
      // Change display timezone
      setTimezoneConfig({ displayTz: 'America/Los_Angeles' });
      
      // UTC formatting should be unchanged
      expect(formatUtcDatetime(ns)).toBe(utcResult);
      
      // Date object should be unchanged
      const date = nsToDate(ns);
      expect(date.toISOString()).toBe('2024-01-01T12:00:00.000Z');
    });

    it('display timezone explicitly labeled', () => {
      const ns = 1704110400000000000;
      
      // UTC should say "UTC"
      setTimezoneConfig({ displayTz: 'UTC' });
      expect(formatDisplayDatetime(ns)).toContain('UTC');
      
      // Non-UTC should include timezone abbreviation
      setTimezoneConfig({ displayTz: 'America/New_York' });
      const result = formatDisplayDatetime(ns);
      // Should have some timezone indicator (EST, EDT, or similar)
      expect(result).toMatch(/[A-Z]{2,4}$/);
    });
  });

  describe('Invariant 6: Rounding is consistent across views', () => {
    it('same value formats identically everywhere', () => {
      const value = 123.456;
      
      // Format with same options should give same result
      const r1 = formatUsd(value, { decimals: 2 });
      const r2 = formatUsd(value, { decimals: 2 });
      const r3 = formatUsd(value, { decimals: 2 });
      
      expect(r1).toBe(r2);
      expect(r2).toBe(r3);
    });

    it('rounding does not accumulate errors', () => {
      // Format a series of values and verify total
      const values = [33.33, 33.33, 33.34];
      const total = values.reduce((a, b) => a + b, 0);
      
      // Formatted total should equal 100.00
      expect(formatUsd(total)).toBe('$100.00');
    });

    it('negative zero displays correctly', () => {
      const negZero = -0;
      const result = formatUsd(negZero);
      expect(result).toBe('$0.00'); // Should not show -$0.00
    });
  });

  describe('Edge Cases', () => {
    it('handles very large numbers', () => {
      const largeNum = 999999999999.99;
      const result = formatUsd(largeNum);
      expect(result).toContain('$');
      expect(result).not.toContain('NaN');
      expect(result).not.toContain('Infinity');
    });

    it('handles very small numbers', () => {
      const smallNum = 0.0000001;
      const result = formatUsd(smallNum, { decimals: 8 });
      expect(result).toContain('$');
    });

    it('handles negative numbers correctly', () => {
      const negNum = -12345.67;
      const result = formatUsd(negNum);
      expect(result).toBe('-$12,345.67');
    });

    it('null values never cause runtime errors', () => {
      expect(() => formatUsd(null)).not.toThrow();
      expect(() => formatPercent(undefined)).not.toThrow();
      expect(() => verifyCumulativePnL([], NaN)).not.toThrow();
    });
  });

  describe('Comprehensive Verification Helper', () => {
    interface BacktestSummary {
      gross_pnl: number;
      fees: number;
      net_pnl: number;
      initial_capital: number;
      first_equity: number;
      per_window_pnls: number[];
      cumulative_pnl: number;
    }

    function verifyAllInvariants(summary: BacktestSummary): string[] {
      const errors: string[] = [];

      // Check equity start
      const equityError = verifyEquityCurveStart(summary.first_equity, summary.initial_capital);
      if (equityError) errors.push(equityError);

      // Check net = gross - fees
      const netError = verifyNetPnL(summary.gross_pnl, summary.fees, summary.net_pnl);
      if (netError) errors.push(netError);

      // Check cumulative = sum of windows
      const cumError = verifyCumulativePnL(summary.per_window_pnls, summary.cumulative_pnl);
      if (cumError) errors.push(cumError);

      return errors;
    }

    it('passes all invariants for valid data', () => {
      const validSummary: BacktestSummary = {
        gross_pnl: 1000,
        fees: 50,
        net_pnl: 950,
        initial_capital: 10000,
        first_equity: 10000,
        per_window_pnls: [200, -50, 100, 700],
        cumulative_pnl: 950,
      };

      const errors = verifyAllInvariants(validSummary);
      expect(errors).toHaveLength(0);
    });

    it('catches multiple invariant violations', () => {
      const invalidSummary: BacktestSummary = {
        gross_pnl: 1000,
        fees: 50,
        net_pnl: 900, // Wrong: should be 950
        initial_capital: 10000,
        first_equity: 9000, // Wrong: should be 10000
        per_window_pnls: [200, -50, 100],
        cumulative_pnl: 500, // Wrong: should be 250
      };

      const errors = verifyAllInvariants(invalidSummary);
      expect(errors.length).toBeGreaterThan(0);
    });
  });
});
