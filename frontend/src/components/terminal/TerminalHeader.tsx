import React from 'react';
import { SignalStats } from '../../types/signal';
import { useAuth } from '../../hooks/useAuth';

interface TerminalHeaderProps {
  stats: SignalStats | null;
  latency: number;
  isConnected: boolean;
  currentView: 'terminal' | 'vault';
  onViewChange: (view: 'terminal' | 'vault') => void;
}

export const TerminalHeader: React.FC<TerminalHeaderProps> = ({
  stats,
  latency: wsLatency,
  isConnected,
  currentView,
  onViewChange,
}) => {
  const { user, logout } = useAuth();
  
  // Only use WebSocket latency - REST latency is not relevant for real-time display
  const displayLatency = wsLatency;

  return (
    <div className="bg-void border-b border-grey/20 px-6 flex flex-col md:flex-row justify-between items-center gap-4">
      {/* Left: Logo & Nav */}
      <div className="flex items-center gap-6">
        <button
          type="button"
          className="cursor-pointer flex items-center"
          onClick={() => onViewChange('terminal')}
          aria-label="Go to terminal"
        >
          <img
            src="/logo-better.svg"
            alt="BETTER"
            className="h-10 md:h-12 w-auto"
          />
        </button>

        {/* Navigation Tabs */}
        <div className="flex border border-grey/30">
          <button
            onClick={() => onViewChange('terminal')}
            className={`px-4 py-2 text-xs font-mono transition-colors duration-150 ${
              currentView === 'terminal'
                ? 'bg-white text-black font-semibold'
                : 'text-grey/80 hover:text-white hover:bg-grey/10'
            }`}
          >
            TERMINAL
          </button>
          <button
            onClick={() => onViewChange('vault')}
            className={`px-4 py-2 text-xs font-mono transition-colors duration-150 ${
              currentView === 'vault'
                ? 'bg-white text-black font-semibold'
                : 'text-grey/80 hover:text-white hover:bg-grey/10'
            }`}
          >
            VAULT
          </button>
        </div>
      </div>

      {/* Center spacer */}
      <div className="hidden md:block" />

      {/* Right: Status & User */}
      <div className="flex items-center gap-6">
        {/* Stats */}
        {stats && (
          <div className="hidden lg:flex items-center gap-4 text-xs font-mono">
            <div className="text-right">
              <div className="text-grey/80 text-[10px]">TOTAL</div>
              <div className="text-white font-semibold">{stats.total_signals.toLocaleString()}</div>
            </div>
            <div className="text-right">
              <div className="text-grey/80 text-[10px]">HIGH CONF</div>
              <div className="text-white font-semibold">{stats.high_confidence_count.toLocaleString()}</div>
            </div>
            <div className="text-right">
              <div className="text-grey/80 text-[10px]">AVG CONF</div>
              <div className="text-white font-semibold">{(stats.avg_confidence * 100).toFixed(1)}%</div>
            </div>
          </div>
        )}

        {/* Latency - high precision */}
        <div className="text-xs font-mono text-right hidden sm:block">
          <div className="text-grey/80 text-[10px]">LATENCY</div>
          <div className={`font-semibold ${
            displayLatency < 10 ? 'text-success' : displayLatency < 50 ? 'text-warning' : 'text-danger'
          }`}>
            {displayLatency > 0 ? `${displayLatency.toFixed(3)}ms` : '---'}
          </div>
        </div>

        {/* Connection Status */}
        <div className="flex items-center gap-2" title={isConnected ? 'Connected' : 'Disconnected'}>
          <div className={`w-2 h-2 rounded-full ${isConnected ? 'bg-success' : 'bg-danger'}`} />
        </div>

        <div className="h-6 w-px bg-grey/20 hidden sm:block" />

        {/* User */}
        <div className="flex items-center gap-3">
          <div className="text-xs font-mono text-right hidden sm:block">
            <div className="text-grey/80 text-[10px]">OPERATOR</div>
            <div className="text-white">{user?.username || 'ANON'}</div>
          </div>
          
          <button
            onClick={logout}
            className="border border-grey/30 px-3 py-1 text-[10px] font-mono text-grey/80 hover:text-danger hover:border-danger transition-colors duration-150"
          >
            EXIT
          </button>
        </div>
      </div>
    </div>
  );
};
