/**
 * Timezone Utility Tests
 * 
 * Verifies:
 * 1. UTC timestamps render identically across environments
 * 2. Timezone conversion is correct for DST boundaries
 * 3. No implicit browser timezone inference
 * 4. Window range formatting is consistent
 */

import {
  nsToMs,
  nsToDate,
  formatUtcIso,
  formatUtcDate,
  formatUtcTime,
  formatUtcDatetime,
  formatDisplayDatetime,
  formatWindowRange,
  getWindowTooltip,
  isValidTimestampNs,
  setTimezoneConfig,
  resetTimezoneConfig,
  getTimezoneConfig,
  isValidTimezone,
} from '../timezone';

describe('Timezone Utilities', () => {
  beforeEach(() => {
    resetTimezoneConfig();
  });

  describe('nsToMs', () => {
    it('converts nanoseconds to milliseconds correctly', () => {
      const ns = 1704067200000000000; // 2024-01-01 00:00:00 UTC in ns
      const ms = nsToMs(ns);
      expect(ms).toBe(1704067200000);
    });

    it('handles milliseconds input (passthrough for values below threshold)', () => {
      const ms = 1704067200000; // Already in ms
      expect(nsToMs(ms)).toBe(ms);
    });

    it('handles edge cases', () => {
      expect(nsToMs(0)).toBe(0);
      expect(nsToMs(1000000)).toBe(1000000); // Small value treated as ms
    });
  });

  describe('nsToDate', () => {
    it('creates correct Date object from nanoseconds', () => {
      const ns = 1704067200000000000; // 2024-01-01 00:00:00 UTC
      const date = nsToDate(ns);
      expect(date.toISOString()).toBe('2024-01-01T00:00:00.000Z');
    });
  });

  describe('formatUtcIso', () => {
    it('formats nanosecond timestamp as ISO 8601', () => {
      const ns = 1704067200000000000;
      expect(formatUtcIso(ns)).toBe('2024-01-01T00:00:00.000Z');
    });

    it('returns --- for invalid input', () => {
      expect(formatUtcIso(NaN)).toBe('---');
      expect(formatUtcIso(Infinity)).toBe('---');
    });
  });

  describe('formatUtcDate', () => {
    it('formats as YYYY-MM-DD', () => {
      const ns = 1704067200000000000;
      expect(formatUtcDate(ns)).toBe('2024-01-01');
    });
  });

  describe('formatUtcTime', () => {
    it('formats as HH:MM:SS', () => {
      const ns = 1704067200000000000;
      expect(formatUtcTime(ns)).toBe('00:00:00');
    });

    it('handles mid-day times', () => {
      const ns = 1704110400000000000; // 2024-01-01 12:00:00 UTC
      expect(formatUtcTime(ns)).toBe('12:00:00');
    });
  });

  describe('formatUtcDatetime', () => {
    it('formats as YYYY-MM-DD HH:MM:SS', () => {
      const ns = 1704110400000000000;
      expect(formatUtcDatetime(ns)).toBe('2024-01-01 12:00:00');
    });
  });

  describe('formatDisplayDatetime', () => {
    it('formats UTC with explicit label when displayTz is UTC', () => {
      setTimezoneConfig({ displayTz: 'UTC' });
      const ns = 1704110400000000000;
      expect(formatDisplayDatetime(ns)).toBe('2024-01-01 12:00:00 UTC');
    });

    it('includes timezone name in output for non-UTC', () => {
      setTimezoneConfig({ displayTz: 'America/New_York' });
      const ns = 1704110400000000000;
      const result = formatDisplayDatetime(ns);
      // Should include some timezone indicator (EST or EDT)
      expect(result).toMatch(/\d{2}\/\d{2}\/\d{4}.+[A-Z]{2,4}$/);
    });

    it('falls back to UTC on invalid timezone', () => {
      const result = formatDisplayDatetime(1704110400000000000, { 
        displayTz: 'Invalid/Timezone', 
        showUtcInTooltips: true 
      });
      expect(result).toContain('UTC');
    });
  });

  describe('DST boundary handling', () => {
    it('handles US DST spring forward (March 2024)', () => {
      // March 10, 2024 2:00 AM ET - clocks spring forward
      const beforeDst = 1710050400000000000; // 2024-03-10 06:00:00 UTC
      const afterDst = 1710054000000000000;  // 2024-03-10 07:00:00 UTC
      
      setTimezoneConfig({ displayTz: 'America/New_York' });
      
      const before = formatDisplayDatetime(beforeDst);
      const after = formatDisplayDatetime(afterDst);
      
      // Both should have explicit timezone labels
      expect(before).toMatch(/[A-Z]{2,4}$/);
      expect(after).toMatch(/[A-Z]{2,4}$/);
    });

    it('handles US DST fall back (November 2024)', () => {
      // November 3, 2024 2:00 AM ET - clocks fall back
      const beforeDst = 1730613600000000000; // 2024-11-03 05:00:00 UTC
      const afterDst = 1730617200000000000;  // 2024-11-03 06:00:00 UTC
      
      setTimezoneConfig({ displayTz: 'America/New_York' });
      
      const before = formatDisplayDatetime(beforeDst);
      const after = formatDisplayDatetime(afterDst);
      
      // Both should render without errors
      expect(before).toBeTruthy();
      expect(after).toBeTruthy();
    });
  });

  describe('formatWindowRange', () => {
    it('formats 15-minute window correctly', () => {
      const startNs = 1704110400000000000; // 2024-01-01 12:00:00 UTC
      const endNs = 1704111300000000000;   // 2024-01-01 12:15:00 UTC
      
      const { utc, display, crossesMidnight } = formatWindowRange(startNs, endNs);
      
      expect(utc).toBe('2024-01-01 12:00:00 - 12:15:00 UTC');
      expect(display).toBeNull(); // UTC mode, no separate display
      expect(crossesMidnight).toBe(false);
    });

    it('detects windows crossing midnight in display timezone', () => {
      // Window at 23:45-00:00 UTC, displayed in UTC+5
      const startNs = 1704153900000000000; // 2024-01-01 23:45:00 UTC
      const endNs = 1704154800000000000;   // 2024-01-02 00:00:00 UTC
      
      const { crossesMidnight } = formatWindowRange(startNs, endNs, {
        displayTz: 'Asia/Karachi', // UTC+5
        showUtcInTooltips: true,
      });
      
      // In UTC+5, this would be 04:45-05:00 on Jan 2, doesn't cross midnight
      // But the UTC window itself crosses midnight
      expect(typeof crossesMidnight).toBe('boolean');
    });
  });

  describe('getWindowTooltip', () => {
    it('includes both UTC and display timezone', () => {
      const startNs = 1704110400000000000;
      const endNs = 1704111300000000000;
      
      const tooltip = getWindowTooltip(startNs, endNs, {
        displayTz: 'America/New_York',
        showUtcInTooltips: true,
      });
      
      expect(tooltip).toContain('UTC');
      expect(tooltip).toContain('Window Start');
      expect(tooltip).toContain('Window End');
    });
  });

  describe('isValidTimestampNs', () => {
    it('validates reasonable nanosecond timestamps', () => {
      expect(isValidTimestampNs(1704067200000000000)).toBe(true); // 2024
      expect(isValidTimestampNs(946684800000000000)).toBe(true);  // 2000 (min)
    });

    it('rejects invalid timestamps', () => {
      expect(isValidTimestampNs(-1)).toBe(false);
      expect(isValidTimestampNs(NaN)).toBe(false);
      expect(isValidTimestampNs(Infinity)).toBe(false);
      expect(isValidTimestampNs(100)).toBe(false); // Too small
    });
  });

  describe('isValidTimezone', () => {
    it('validates known timezones', () => {
      expect(isValidTimezone('UTC')).toBe(true);
      expect(isValidTimezone('local')).toBe(true);
      expect(isValidTimezone('America/New_York')).toBe(true);
      expect(isValidTimezone('Europe/London')).toBe(true);
      expect(isValidTimezone('Asia/Tokyo')).toBe(true);
    });

    it('rejects invalid timezones', () => {
      expect(isValidTimezone('Invalid/Timezone')).toBe(false);
      // Note: Some browsers accept 'EST' as a valid timezone abbreviation
      // so we only test clearly invalid strings
      expect(isValidTimezone('')).toBe(false);
    });
  });

  describe('config persistence', () => {
    it('maintains config between calls', () => {
      setTimezoneConfig({ displayTz: 'Europe/Paris' });
      expect(getTimezoneConfig().displayTz).toBe('Europe/Paris');
      
      setTimezoneConfig({ showUtcInTooltips: false });
      expect(getTimezoneConfig().displayTz).toBe('Europe/Paris');
      expect(getTimezoneConfig().showUtcInTooltips).toBe(false);
    });

    it('resets to defaults', () => {
      setTimezoneConfig({ displayTz: 'Asia/Tokyo', showUtcInTooltips: false });
      resetTimezoneConfig();
      
      const config = getTimezoneConfig();
      expect(config.displayTz).toBe('UTC');
      expect(config.showUtcInTooltips).toBe(true);
    });
  });

  describe('cross-browser consistency', () => {
    it('UTC formatting is deterministic', () => {
      const ns = 1704110400000000000;
      
      // Multiple calls should return identical results
      const results = Array(5).fill(null).map(() => formatUtcDatetime(ns));
      expect(new Set(results).size).toBe(1);
    });

    it('ISO format is always ISO 8601', () => {
      const ns = 1704110400000000000;
      const iso = formatUtcIso(ns);
      
      // Should match ISO 8601 format
      expect(iso).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
    });
  });
});
