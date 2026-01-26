/**
 * Timezone Utilities for Backtest Result Display
 * 
 * CANONICAL RULE: All timestamps from backend are UTC nanoseconds.
 * This module provides explicit, auditable timezone conversion.
 * 
 * Key invariants:
 * - Backend timestamps are NEVER modified in storage
 * - Conversion happens at render time only
 * - Both UTC and display timezone are shown in tooltips
 * - No implicit browser timezone inference
 */

export type DisplayTimezone = 'UTC' | 'local' | string; // IANA timezone string e.g. 'America/New_York'

export interface TimezoneConfig {
  /** The display timezone (default: 'UTC') */
  displayTz: DisplayTimezone;
  /** Whether to show UTC in tooltips alongside display timezone */
  showUtcInTooltips: boolean;
}

const DEFAULT_CONFIG: TimezoneConfig = {
  displayTz: 'UTC',
  showUtcInTooltips: true,
};

let currentConfig: TimezoneConfig = { ...DEFAULT_CONFIG };

/**
 * Set the global timezone configuration.
 * Call this when user changes their display timezone preference.
 */
export function setTimezoneConfig(config: Partial<TimezoneConfig>): void {
  currentConfig = { ...currentConfig, ...config };
}

/**
 * Get the current timezone configuration.
 */
export function getTimezoneConfig(): Readonly<TimezoneConfig> {
  return currentConfig;
}

/**
 * Reset timezone config to defaults.
 */
export function resetTimezoneConfig(): void {
  currentConfig = { ...DEFAULT_CONFIG };
}

/**
 * Convert nanoseconds to milliseconds (for Date constructor).
 * Handles both ns and ms inputs defensively.
 */
export function nsToMs(ns: number): number {
  // If the value is already in ms range (less than year 3000 in ms), assume ms
  // Otherwise assume nanoseconds
  const MS_YEAR_3000 = 32503680000000;
  if (ns < MS_YEAR_3000) {
    return ns;
  }
  return Math.floor(ns / 1_000_000);
}

/**
 * Convert nanoseconds to Date object.
 */
export function nsToDate(ns: number): Date {
  return new Date(nsToMs(ns));
}

/**
 * Format a nanosecond timestamp as an ISO 8601 string in UTC.
 * This is the canonical format for backend timestamps.
 */
export function formatUtcIso(ns: number): string {
  const date = nsToDate(ns);
  if (!isFinite(date.getTime())) {
    return '---';
  }
  return date.toISOString();
}

/**
 * Format timestamp as UTC date string (YYYY-MM-DD).
 */
export function formatUtcDate(ns: number): string {
  const iso = formatUtcIso(ns);
  if (iso === '---') return iso;
  return iso.slice(0, 10);
}

/**
 * Format timestamp as UTC time string (HH:MM:SS).
 */
export function formatUtcTime(ns: number): string {
  const iso = formatUtcIso(ns);
  if (iso === '---') return iso;
  return iso.slice(11, 19);
}

/**
 * Format timestamp as UTC datetime string (YYYY-MM-DD HH:MM:SS).
 */
export function formatUtcDatetime(ns: number): string {
  const iso = formatUtcIso(ns);
  if (iso === '---') return iso;
  return iso.slice(0, 19).replace('T', ' ');
}

/**
 * Format a timestamp in the user's selected display timezone.
 * Always returns an explicit timezone label.
 */
export function formatDisplayDatetime(ns: number, config?: TimezoneConfig): string {
  const cfg = config || currentConfig;
  const date = nsToDate(ns);
  
  if (!isFinite(date.getTime())) {
    return '---';
  }

  if (cfg.displayTz === 'UTC') {
    return formatUtcDatetime(ns) + ' UTC';
  }

  try {
    const tz = cfg.displayTz === 'local' ? undefined : cfg.displayTz;
    const formatted = date.toLocaleString('en-US', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
      timeZone: tz,
      timeZoneName: 'short',
    });
    return formatted;
  } catch {
    // Fallback to UTC if timezone is invalid
    return formatUtcDatetime(ns) + ' UTC';
  }
}

/**
 * Format a timestamp for display with explicit timezone label.
 */
export function formatDisplayTime(ns: number, config?: TimezoneConfig): string {
  const cfg = config || currentConfig;
  const date = nsToDate(ns);
  
  if (!isFinite(date.getTime())) {
    return '---';
  }

  if (cfg.displayTz === 'UTC') {
    return formatUtcTime(ns) + ' UTC';
  }

  try {
    const tz = cfg.displayTz === 'local' ? undefined : cfg.displayTz;
    return date.toLocaleString('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
      timeZone: tz,
      timeZoneName: 'short',
    });
  } catch {
    return formatUtcTime(ns) + ' UTC';
  }
}

/**
 * Get the timezone offset in hours for a given timestamp.
 * Returns the offset from UTC for the configured display timezone.
 */
export function getTimezoneOffset(ns: number, config?: TimezoneConfig): number {
  const cfg = config || currentConfig;
  const date = nsToDate(ns);
  
  if (!isFinite(date.getTime())) {
    return 0;
  }

  if (cfg.displayTz === 'UTC') {
    return 0;
  }

  // Calculate offset by comparing UTC and local representations
  const utcParts = date.toLocaleString('en-US', {
    timeZone: 'UTC',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });

  try {
    const tz = cfg.displayTz === 'local' ? undefined : cfg.displayTz;
    const localParts = date.toLocaleString('en-US', {
      timeZone: tz,
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
    });

    const utcDate = new Date(utcParts);
    const localDate = new Date(localParts);
    
    return (localDate.getTime() - utcDate.getTime()) / (60 * 60 * 1000);
  } catch {
    return 0;
  }
}

/**
 * Format a settlement window's time range for display.
 * Shows both UTC and display timezone if configured.
 */
export function formatWindowRange(
  startNs: number,
  endNs: number,
  config?: TimezoneConfig
): { utc: string; display: string | null; crossesMidnight: boolean } {
  const cfg = config || currentConfig;
  
  const utcStart = formatUtcDatetime(startNs);
  const utcEnd = formatUtcDatetime(endNs);
  const utc = `${utcStart} - ${utcEnd.slice(11)} UTC`;

  let display: string | null = null;
  let crossesMidnight = false;

  if (cfg.displayTz !== 'UTC') {
    const displayStart = formatDisplayDatetime(startNs, cfg);
    const displayEnd = formatDisplayDatetime(endNs, cfg);
    
    // Check if the window crosses midnight in display timezone
    const startDate = nsToDate(startNs);
    const endDate = nsToDate(endNs);
    
    try {
      const tz = cfg.displayTz === 'local' ? undefined : cfg.displayTz;
      const startDay = startDate.toLocaleString('en-US', { day: '2-digit', timeZone: tz });
      const endDay = endDate.toLocaleString('en-US', { day: '2-digit', timeZone: tz });
      crossesMidnight = startDay !== endDay;
    } catch {
      // Ignore timezone errors
    }

    display = crossesMidnight
      ? `${displayStart} - ${displayEnd}`
      : `${displayStart.slice(0, -4)} - ${displayEnd.slice(11)}`;
  }

  return { utc, display, crossesMidnight };
}

/**
 * Get a tooltip string for a settlement window showing both timezones.
 */
export function getWindowTooltip(
  startNs: number,
  endNs: number,
  config?: TimezoneConfig
): string {
  const cfg = config || currentConfig;
  const result = formatWindowRange(startNs, endNs, cfg);
  
  let tooltip = `Window Start (UTC): ${formatUtcDatetime(startNs)}\n`;
  tooltip += `Window End (UTC): ${formatUtcDatetime(endNs)}\n`;

  if (cfg.showUtcInTooltips && result.display) {
    tooltip += `\n`;
    tooltip += `Display TZ: ${formatDisplayDatetime(startNs, cfg)} - ${formatDisplayDatetime(endNs, cfg)}`;
    if (result.crossesMidnight) {
      tooltip += ` (crosses midnight)`;
    }
  }

  return tooltip;
}

/**
 * Validate that a timestamp is a reasonable nanosecond value.
 * Returns true if the timestamp appears to be valid.
 */
export function isValidTimestampNs(ns: number): boolean {
  if (typeof ns !== 'number' || !isFinite(ns) || ns < 0) {
    return false;
  }
  
  // Reasonable range: 2000-01-01 to 2100-01-01 in nanoseconds
  const MIN_NS = 946684800000000000; // 2000-01-01 00:00:00 UTC
  const MAX_NS = 4102444800000000000; // 2100-01-01 00:00:00 UTC
  
  return ns >= MIN_NS && ns <= MAX_NS;
}

/**
 * Get the current browser timezone (IANA name).
 * Returns 'UTC' if detection fails.
 */
export function getBrowserTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return 'UTC';
  }
}

/**
 * Check if a timezone string is valid.
 */
export function isValidTimezone(tz: string): boolean {
  if (tz === 'UTC' || tz === 'local') {
    return true;
  }
  
  try {
    Intl.DateTimeFormat('en-US', { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

/**
 * Common timezone options for user selection.
 */
export const COMMON_TIMEZONES = [
  { value: 'UTC', label: 'UTC' },
  { value: 'local', label: 'Browser Local' },
  { value: 'America/New_York', label: 'New York (ET)' },
  { value: 'America/Chicago', label: 'Chicago (CT)' },
  { value: 'America/Denver', label: 'Denver (MT)' },
  { value: 'America/Los_Angeles', label: 'Los Angeles (PT)' },
  { value: 'Europe/London', label: 'London (GMT/BST)' },
  { value: 'Europe/Paris', label: 'Paris (CET/CEST)' },
  { value: 'Asia/Tokyo', label: 'Tokyo (JST)' },
  { value: 'Asia/Hong_Kong', label: 'Hong Kong (HKT)' },
  { value: 'Asia/Singapore', label: 'Singapore (SGT)' },
] as const;
