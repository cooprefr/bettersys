import React from 'react';

interface SignalSearchProps {
  value: string;
  onChange: (value: string) => void;
  resultsCount: number;
  isSearching?: boolean;
  mode?: 'server' | 'local';
  banner?: { tone: 'info' | 'warning' | 'error'; message: string } | null;
}

export const SignalSearch: React.FC<SignalSearchProps> = ({
  value,
  onChange,
  resultsCount,
  isSearching,
  mode = 'server',
  banner,
}) => {
  const trimmed = value.trim();

  return (
    <div className="border-b border-grey/20 bg-surface px-4 md:px-6 py-4">
      <div className="flex items-center gap-3">
        <div className="flex-1 relative">
          <input
            type="text"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder="Search markets (keywords, phrases, market_slug:btc)..."
            className="w-full bg-void border border-grey/30 px-4 py-2 text-[13px] font-mono text-white placeholder-grey/60 focus:border-better-blue focus:outline-none transition-colors"
            spellCheck={false}
          />

          {value && (
            <button
              type="button"
              onClick={() => onChange('')}
              className="absolute right-3 top-1/2 -translate-y-1/2 text-grey/60 hover:text-white transition-colors"
              aria-label="Clear search"
            >
              x
            </button>
          )}
        </div>

        {trimmed && (
          <div className="flex items-center gap-2 whitespace-nowrap">
            {mode === 'local' && (
              <span className="text-[10px] font-mono text-grey/80 border border-grey/30 px-2 py-1">
                LOCAL
              </span>
            )}
            <div className="text-[12px] font-mono text-grey/70 tabular-nums">
              {isSearching
                ? 'SEARCHING...'
                : `${resultsCount} ${resultsCount === 1 ? 'MATCH' : 'MATCHES'}`}
            </div>
          </div>
        )}
      </div>

      {trimmed && banner?.message && (
        <div
          className={`mt-2 text-[11px] font-mono whitespace-pre-wrap ${
            banner.tone === 'error'
              ? 'text-red-400'
              : banner.tone === 'warning'
                ? 'text-yellow-300'
                : 'text-grey/60'
          }`}
        >
          {banner.message}
        </div>
      )}

      {trimmed && !banner?.message && !isSearching && resultsCount === 0 && (
        <div className="mt-2 text-[11px] font-mono text-grey/60">
          No matches for "{trimmed}". Try different keywords or clear search.
        </div>
      )}
    </div>
  );
};
