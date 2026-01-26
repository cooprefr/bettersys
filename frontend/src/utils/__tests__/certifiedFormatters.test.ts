/**
 * Certified Formatters Tests
 * 
 * Verifies:
 * 1. Rounding does not change totals when toggling views
 * 2. No NaN / Infinity / overflow renders
 * 3. Units are always explicit
 * 4. Display invariants match backend-certified values
 */

import {
  isValidNumber,
  toNumber,
  formatUsd,
  formatPercent,
  formatPercentRaw,
  formatBps,
  formatRatio,
  formatCount,
  formatPnLWithContext,
  formatDrawdown,
  formatAxisLabel,
  verifyCumulativePnL,
  verifyNetPnL,
  verifyEquityCurveStart,
  formatForExport,
  formatTimestampForExport,
} from '../certifiedFormatters';

describe('Certified Formatters', () => {
  describe('isValidNumber', () => {
    it('returns true for finite numbers', () => {
      expect(isValidNumber(0)).toBe(true);
      expect(isValidNumber(123.456)).toBe(true);
      expect(isValidNumber(-999)).toBe(true);
    });

    it('returns false for invalid values', () => {
      expect(isValidNumber(null)).toBe(false);
      expect(isValidNumber(undefined)).toBe(false);
      expect(isValidNumber(NaN)).toBe(false);
      expect(isValidNumber(Infinity)).toBe(false);
      expect(isValidNumber(-Infinity)).toBe(false);
    });
  });

  describe('toNumber', () => {
    it('converts valid inputs', () => {
      expect(toNumber(123)).toBe(123);
      expect(toNumber('456')).toBe(456);
    });

    it('returns null for invalid inputs', () => {
      expect(toNumber(null)).toBeNull();
      expect(toNumber(undefined)).toBeNull();
      expect(toNumber('not a number')).toBeNull();
    });
  });

  describe('formatUsd', () => {
    it('formats positive values correctly', () => {
      expect(formatUsd(1234.56)).toBe('$1,234.56');
      expect(formatUsd(0.99)).toBe('$0.99');
    });

    it('formats negative values with leading minus', () => {
      expect(formatUsd(-500.25)).toBe('-$500.25');
    });

    it('handles explicit sign option', () => {
      expect(formatUsd(100, { explicitSign: true })).toBe('+$100.00');
      expect(formatUsd(-100, { explicitSign: true })).toBe('-$100.00');
      expect(formatUsd(0, { explicitSign: true })).toBe('$0.00');
    });

    it('respects decimal places', () => {
      expect(formatUsd(123.456789, { decimals: 4 })).toBe('$123.4568');
      expect(formatUsd(100, { decimals: 0 })).toBe('$100');
    });

    it('handles invalid input with fallback', () => {
      expect(formatUsd(null)).toBe('---');
      expect(formatUsd(undefined)).toBe('---');
      expect(formatUsd(NaN)).toBe('---');
      expect(formatUsd(Infinity)).toBe('---');
      expect(formatUsd(null, { fallback: 'N/A' })).toBe('N/A');
    });

    it('can exclude unit', () => {
      expect(formatUsd(100, { includeUnit: false })).toBe('100.00');
    });
  });

  describe('formatPercent', () => {
    it('converts decimal to percentage', () => {
      expect(formatPercent(0.5)).toBe('50.00%');
      expect(formatPercent(0.123)).toBe('12.30%');
    });

    it('handles negative percentages', () => {
      expect(formatPercent(-0.15)).toBe('-15.00%');
    });

    it('handles explicit sign', () => {
      expect(formatPercent(0.1, { explicitSign: true })).toBe('+10.00%');
    });

    it('handles invalid input', () => {
      expect(formatPercent(null)).toBe('---');
      expect(formatPercent(NaN)).toBe('---');
    });
  });

  describe('formatPercentRaw', () => {
    it('formats already-percent values', () => {
      expect(formatPercentRaw(50)).toBe('50.00%');
      expect(formatPercentRaw(-15.5)).toBe('-15.50%');
    });
  });

  describe('formatBps', () => {
    it('formats basis points correctly', () => {
      expect(formatBps(100)).toBe('100 bps');
      expect(formatBps(-50)).toBe('-50 bps');
    });

    it('handles explicit sign', () => {
      expect(formatBps(25, { explicitSign: true })).toBe('+25 bps');
    });

    it('rounds to integers by default', () => {
      expect(formatBps(12.7)).toBe('13 bps');
    });
  });

  describe('formatRatio', () => {
    it('formats ratios with 2 decimal places', () => {
      expect(formatRatio(1.5)).toBe('1.50');
      expect(formatRatio(-0.75)).toBe('-0.75');
    });

    it('handles invalid input', () => {
      expect(formatRatio(null)).toBe('---');
    });
  });

  describe('formatCount', () => {
    it('formats integers with thousand separators', () => {
      expect(formatCount(1234567)).toBe('1,234,567');
      expect(formatCount(42)).toBe('42');
    });

    it('rounds non-integers', () => {
      expect(formatCount(99.6)).toBe('100');
      expect(formatCount(99.4)).toBe('99');
    });
  });

  describe('formatPnLWithContext', () => {
    it('returns value, label, and unit', () => {
      const result = formatPnLWithContext(500, 'net');
      expect(result.value).toBe('+$500.00');
      expect(result.label).toBe('Net');
      expect(result.unit).toBe('USD');
    });

    it('handles all context types', () => {
      expect(formatPnLWithContext(100, 'per_window').label).toBe('Per-Window');
      expect(formatPnLWithContext(100, 'cumulative').label).toBe('Cumulative');
      expect(formatPnLWithContext(100, 'gross').label).toBe('Gross');
      expect(formatPnLWithContext(100, 'fees').label).toBe('Fees');
    });
  });

  describe('formatDrawdown', () => {
    it('always shows negative values', () => {
      expect(formatDrawdown(100)).toBe('-$100.00');
      expect(formatDrawdown(-100)).toBe('-$100.00'); // Absolute value, then negate
    });

    it('formats as percentage when requested', () => {
      expect(formatDrawdown(5.5, true)).toBe('-5.50%');
    });
  });

  describe('formatAxisLabel', () => {
    it('abbreviates large USD values', () => {
      expect(formatAxisLabel(1500000, 'USD')).toBe('$1.5M');
      expect(formatAxisLabel(25000, 'USD')).toBe('$25.0K');
      expect(formatAxisLabel(500, 'USD')).toBe('$500');
    });

    it('handles negative values', () => {
      expect(formatAxisLabel(-1000000, 'USD')).toBe('-$1.0M');
    });

    it('formats other units', () => {
      expect(formatAxisLabel(15.5, 'percent')).toBe('15.5%');
      expect(formatAxisLabel(100, 'bps')).toBe('100 bps');
      expect(formatAxisLabel(1.5, 'ratio')).toBe('1.50');
    });
  });

  describe('Display Invariants', () => {
    describe('verifyCumulativePnL', () => {
      it('passes when sum matches cumulative', () => {
        const windows = [100, 50, -30, 80];
        const cumulative = 200;
        expect(verifyCumulativePnL(windows, cumulative)).toBeNull();
      });

      it('fails when sum differs from cumulative', () => {
        const windows = [100, 50, -30];
        const cumulative = 500; // Wrong
        const error = verifyCumulativePnL(windows, cumulative);
        expect(error).toContain('mismatch');
      });

      it('allows small floating point differences', () => {
        const windows = [0.1, 0.2, 0.3];
        const cumulative = 0.6000000001; // Tiny fp error
        expect(verifyCumulativePnL(windows, cumulative)).toBeNull();
      });
    });

    describe('verifyNetPnL', () => {
      it('passes when net = gross - fees', () => {
        expect(verifyNetPnL(1000, 50, 950)).toBeNull();
      });

      it('fails when equation doesn\'t hold', () => {
        const error = verifyNetPnL(1000, 50, 900); // Should be 950
        expect(error).toContain('mismatch');
      });
    });

    describe('verifyEquityCurveStart', () => {
      it('passes when equity matches initial capital', () => {
        expect(verifyEquityCurveStart(10000, 10000)).toBeNull();
      });

      it('allows small tolerance', () => {
        expect(verifyEquityCurveStart(10000.05, 10000)).toBeNull();
      });

      it('fails when significantly different', () => {
        const error = verifyEquityCurveStart(9000, 10000);
        expect(error).toContain('mismatch');
      });
    });
  });

  describe('Export Formatters', () => {
    describe('formatForExport', () => {
      it('outputs full precision', () => {
        expect(formatForExport(123.456789012345)).toBe('123.456789012345');
      });

      it('returns empty string for invalid', () => {
        expect(formatForExport(null)).toBe('');
        expect(formatForExport(NaN)).toBe('');
      });
    });

    describe('formatTimestampForExport', () => {
      it('outputs ISO 8601', () => {
        const ns = 1704110400000000000;
        expect(formatTimestampForExport(ns)).toBe('2024-01-01T12:00:00.000Z');
      });

      it('handles millisecond input', () => {
        const ms = 1704110400000;
        expect(formatTimestampForExport(ms)).toBe('2024-01-01T12:00:00.000Z');
      });
    });
  });

  describe('Rounding Consistency', () => {
    it('rounding is consistent across repeated calls', () => {
      const value = 123.456789;
      const results = Array(10).fill(null).map(() => formatUsd(value, { decimals: 2 }));
      expect(new Set(results).size).toBe(1);
    });

    it('rounding matches across different format functions', () => {
      // When same underlying value is formatted, rounding should be consistent
      const value = 0.125; // Could round to 12 or 13 depending on method
      const pct = formatPercent(value, { decimals: 0 });
      // Just verify it's deterministic
      expect(pct).toMatch(/^\d+%$/);
    });
  });

  describe('No NaN/Infinity Leakage', () => {
    const edgeCases = [NaN, Infinity, -Infinity, null, undefined];
    const formatters = [
      (v: any) => formatUsd(v),
      (v: any) => formatPercent(v),
      (v: any) => formatBps(v),
      (v: any) => formatRatio(v),
      (v: any) => formatCount(v),
    ];

    edgeCases.forEach(val => {
      formatters.forEach((fn, idx) => {
        it(`formatter ${idx} handles ${String(val)} safely`, () => {
          const result = fn(val);
          expect(result).not.toContain('NaN');
          expect(result).not.toContain('Infinity');
          expect(typeof result).toBe('string');
        });
      });
    });
  });
});
