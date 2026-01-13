import { useState } from 'react';

export interface FilterState {
  hideUpDown: boolean;
  minConfidence: number;
  whaleOnly: boolean; // $1000+ trades
}

interface SignalFiltersProps {
  filters: FilterState;
  onFiltersChange: (filters: FilterState) => void;
}

export const SignalFilters: React.FC<SignalFiltersProps> = ({ filters, onFiltersChange }) => {
  const [isExpanded, setIsExpanded] = useState(false);

  const handleConfidenceChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    onFiltersChange({
      ...filters,
      minConfidence: parseInt(e.target.value, 10),
    });
  };

  return (
    <div className="border-b border-grey/20 bg-surface">
      {/* Filter Toggle Button */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full px-4 md:px-6 py-3 flex justify-between items-center text-[13px] font-mono text-grey/80 hover:text-white transition-colors"
      >
        <span>FILTERS</span>
        <span className={`transform transition-transform ${isExpanded ? 'rotate-180' : ''}`}>
          ▼
        </span>
      </button>

      {/* Expanded Filters */}
      {isExpanded && (
        <div className="px-4 md:px-6 pb-4 space-y-4">
          {/* Up/Down Markets Toggle */}
          <div className="flex items-center justify-between">
            <span className="text-[13px] font-mono text-grey/80">HIDE UP/DOWN MARKETS</span>
            <button
              onClick={() => onFiltersChange({ ...filters, hideUpDown: !filters.hideUpDown })}
              className={`w-12 h-6 rounded-none border transition-colors ${
                filters.hideUpDown
                  ? 'bg-white border-white'
                  : 'bg-transparent border-grey/30'
              }`}
            >
              <div
                className={`w-4 h-4 transition-transform ${
                  filters.hideUpDown
                    ? 'translate-x-6 bg-black'
                    : 'translate-x-1 bg-grey/50'
                }`}
              />
            </button>
          </div>

          {/* Whale Filter Toggle */}
          <div className="flex items-center justify-between">
            <span className="text-[13px] font-mono text-grey/80">WHALE ONLY ($1,000+)</span>
            <button
              onClick={() => onFiltersChange({ ...filters, whaleOnly: !filters.whaleOnly })}
              className={`w-12 h-6 rounded-none border transition-colors ${
                filters.whaleOnly
                  ? 'bg-white border-white'
                  : 'bg-transparent border-grey/30'
              }`}
            >
              <div
                className={`w-4 h-4 transition-transform ${
                  filters.whaleOnly
                    ? 'translate-x-6 bg-black'
                    : 'translate-x-1 bg-grey/50'
                }`}
              />
            </button>
          </div>

          {/* Confidence Slider */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <span className="text-[13px] font-mono text-grey/80">MIN CONFIDENCE</span>
              <span className="text-[13px] font-mono text-white tabular-nums">{filters.minConfidence}%</span>
            </div>
            <div className="relative">
              <input
                type="range"
                min="0"
                max="100"
                value={filters.minConfidence}
                onChange={handleConfidenceChange}
                className="w-full h-1 appearance-none cursor-pointer bg-grey/20"
                style={{
                  background: `linear-gradient(to right, white ${filters.minConfidence}%, #333 ${filters.minConfidence}%)`,
                }}
              />
              <style>{`
                input[type='range']::-webkit-slider-thumb {
                  -webkit-appearance: none;
                  appearance: none;
                  width: 12px;
                  height: 12px;
                  background: white;
                  cursor: pointer;
                  border: none;
                }
                input[type='range']::-moz-range-thumb {
                  width: 12px;
                  height: 12px;
                  background: white;
                  cursor: pointer;
                  border: none;
                  border-radius: 0;
                }
              `}</style>
            </div>
            <div className="flex justify-between text-[10px] font-mono text-grey/70">
              <span>0%</span>
              <span>50%</span>
              <span>100%</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
