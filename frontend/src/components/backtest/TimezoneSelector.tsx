/**
 * Timezone Selector Component
 * 
 * Allows users to select their display timezone for backtest results.
 * - UTC is always available and is the default
 * - Browser local timezone is detected and offered
 * - Common trading timezones are listed
 * - Custom IANA timezone can be entered
 */

import React, { useState, useCallback, useMemo } from 'react';
import {
  COMMON_TIMEZONES,
  getBrowserTimezone,
  isValidTimezone,
  setTimezoneConfig,
  getTimezoneConfig,
  type DisplayTimezone,
} from '../../utils/timezone';

export interface TimezoneSelectorProps {
  /** Current selected timezone */
  value?: DisplayTimezone;
  /** Callback when timezone changes */
  onChange?: (tz: DisplayTimezone) => void;
  /** Show compact single-line version */
  compact?: boolean;
  /** Additional CSS classes */
  className?: string;
}

export const TimezoneSelector: React.FC<TimezoneSelectorProps> = ({
  value,
  onChange,
  compact = false,
  className = '',
}) => {
  const currentConfig = getTimezoneConfig();
  const selectedTz = value ?? currentConfig.displayTz;
  const browserTz = useMemo(() => getBrowserTimezone(), []);
  const [customTz, setCustomTz] = useState('');
  const [customError, setCustomError] = useState<string | null>(null);
  const [showCustom, setShowCustom] = useState(false);
  
  const handleChange = useCallback((newTz: DisplayTimezone) => {
    if (isValidTimezone(newTz)) {
      setTimezoneConfig({ displayTz: newTz });
      onChange?.(newTz);
      setCustomError(null);
    }
  }, [onChange]);
  
  const handleCustomSubmit = useCallback(() => {
    const trimmed = customTz.trim();
    if (!trimmed) {
      setCustomError('Please enter a timezone');
      return;
    }
    if (!isValidTimezone(trimmed)) {
      setCustomError('Invalid IANA timezone');
      return;
    }
    handleChange(trimmed);
    setShowCustom(false);
    setCustomTz('');
    setCustomError(null);
  }, [customTz, handleChange]);
  
  // Get browser timezone label
  const browserLabel = useMemo(() => {
    if (browserTz === 'UTC') return null;
    try {
      const now = new Date();
      const offset = now.toLocaleString('en-US', {
        timeZone: browserTz,
        timeZoneName: 'short',
      }).split(' ').pop();
      return `Browser (${browserTz.split('/').pop()} - ${offset})`;
    } catch {
      return `Browser (${browserTz})`;
    }
  }, [browserTz]);
  
  if (compact) {
    return (
      <div className={`flex items-center gap-2 ${className}`}>
        <span className="text-[10px] text-fg/60 tracking-widest">TZ:</span>
        <select
          value={selectedTz}
          onChange={(e) => handleChange(e.target.value as DisplayTimezone)}
          className="bg-void/50 border border-grey/20 px-2 py-1 text-[11px] font-mono text-fg
                     focus:border-better-blue focus:outline-none"
          aria-label="Select display timezone"
        >
          <option value="UTC">UTC</option>
          {browserLabel && browserTz !== 'UTC' && (
            <option value="local">{browserLabel}</option>
          )}
          {COMMON_TIMEZONES.filter(tz => tz.value !== 'UTC' && tz.value !== 'local').map(tz => (
            <option key={tz.value} value={tz.value}>{tz.label}</option>
          ))}
        </select>
      </div>
    );
  }
  
  return (
    <div className={`bg-surface border border-grey/10 p-4 ${className}`}>
      <div className="text-[10px] text-fg/90 tracking-widest mb-3">DISPLAY TIMEZONE</div>
      
      {/* Quick options */}
      <div className="flex flex-wrap gap-2 mb-3">
        <button
          onClick={() => handleChange('UTC')}
          className={`px-3 py-1.5 text-[11px] font-mono border transition-colors
                     ${selectedTz === 'UTC' 
                       ? 'border-better-blue text-better-blue bg-better-blue/10' 
                       : 'border-grey/20 text-fg/80 hover:border-grey/40'}`}
          aria-pressed={selectedTz === 'UTC'}
        >
          UTC
        </button>
        {browserLabel && browserTz !== 'UTC' && (
          <button
            onClick={() => handleChange('local')}
            className={`px-3 py-1.5 text-[11px] font-mono border transition-colors
                       ${selectedTz === 'local' 
                         ? 'border-better-blue text-better-blue bg-better-blue/10' 
                         : 'border-grey/20 text-fg/80 hover:border-grey/40'}`}
            aria-pressed={selectedTz === 'local'}
          >
            {browserLabel}
          </button>
        )}
      </div>
      
      {/* Common timezones */}
      <div className="mb-3">
        <div className="text-[9px] text-fg/50 tracking-widest mb-2">COMMON TIMEZONES</div>
        <div className="grid grid-cols-2 gap-1">
          {COMMON_TIMEZONES.filter(tz => tz.value !== 'UTC' && tz.value !== 'local').map(tz => (
            <button
              key={tz.value}
              onClick={() => handleChange(tz.value)}
              className={`px-2 py-1 text-[10px] font-mono text-left border transition-colors
                         ${selectedTz === tz.value 
                           ? 'border-better-blue text-better-blue bg-better-blue/10' 
                           : 'border-grey/10 text-fg/70 hover:border-grey/30'}`}
              aria-pressed={selectedTz === tz.value}
            >
              {tz.label}
            </button>
          ))}
        </div>
      </div>
      
      {/* Custom timezone */}
      <div>
        <button
          onClick={() => setShowCustom(!showCustom)}
          className="text-[10px] font-mono text-fg/60 hover:text-fg/80 tracking-widest"
        >
          {showCustom ? '[-] HIDE CUSTOM' : '[+] CUSTOM TIMEZONE'}
        </button>
        
        {showCustom && (
          <div className="mt-2">
            <div className="flex gap-2">
              <input
                type="text"
                value={customTz}
                onChange={(e) => setCustomTz(e.target.value)}
                placeholder="e.g., Europe/Berlin"
                className="flex-grow bg-void/50 border border-grey/20 px-3 py-1.5 text-[11px] font-mono text-fg
                           focus:border-better-blue focus:outline-none"
                aria-label="Custom IANA timezone"
                onKeyDown={(e) => e.key === 'Enter' && handleCustomSubmit()}
              />
              <button
                onClick={handleCustomSubmit}
                className="px-3 py-1.5 text-[10px] font-mono border border-grey/30 text-fg/80 
                           hover:border-grey/50 tracking-widest"
              >
                [SET]
              </button>
            </div>
            {customError && (
              <div className="text-[10px] text-danger mt-1">{customError}</div>
            )}
            <div className="text-[9px] text-fg/40 mt-1">
              Enter a valid IANA timezone identifier (e.g., America/New_York)
            </div>
          </div>
        )}
      </div>
      
      {/* Current selection display */}
      <div className="mt-4 pt-3 border-t border-grey/10">
        <div className="text-[9px] text-fg/50 tracking-widest mb-1">CURRENT SELECTION</div>
        <div className="text-[11px] font-mono text-fg">
          {selectedTz === 'local' ? `Browser Local (${browserTz})` : selectedTz}
        </div>
        <div className="text-[9px] text-fg/40 mt-1">
          All backend timestamps are UTC. Conversion is display-only.
        </div>
      </div>
    </div>
  );
};

export default TimezoneSelector;
