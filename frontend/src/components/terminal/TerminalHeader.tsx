import React from 'react';
import { usePrivy } from '@privy-io/react-auth';
import { SignalStats } from '../../types/signal';
import { useAuth } from '../../hooks/useAuth';
import { PRIVY_ENABLED } from '../../config/privy';

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
    <header className="bg-void border-b border-grey/20">
      {/* Row 1: Centered logo with breathing room */}
      <div className="py-6 flex justify-center items-center border-b border-grey/10">
        <button
          type="button"
          className="cursor-pointer flex items-center"
          onClick={() => onViewChange('terminal')}
          aria-label="Go to terminal"
        >
          <img src="/logo-better.svg" alt="BETTER" className="h-[52px] md:h-[62px] w-auto" />
        </button>
      </div>

      {/* Row 2: Nav + Stats + User controls */}
      <div className="px-4 md:px-6 py-3 flex flex-col md:flex-row items-center justify-between gap-4">
        {/* Left: Navigation Tabs */}
        <div className="flex items-center gap-6">
          <div className="flex border border-grey/30">
            <button
              onClick={() => onViewChange('terminal')}
              className={`px-4 py-2 text-[13px] font-mono transition-colors duration-150 ${
                currentView === 'terminal'
                  ? 'bg-white text-black font-semibold'
                  : 'text-grey/80 hover:text-white hover:bg-grey/10'
              }`}
            >
              TERMINAL
            </button>
            <button
              onClick={() => onViewChange('vault')}
              className={`px-4 py-2 text-[13px] font-mono transition-colors duration-150 ${
                currentView === 'vault'
                  ? 'bg-white text-black font-semibold'
                  : 'text-grey/80 hover:text-white hover:bg-grey/10'
              }`}
            >
              VAULT
            </button>
          </div>
        </div>

        {/* Right: Status & User */}
        <div className="flex items-center gap-8">
          {/* Stats */}
          {stats && (
            <div className="hidden lg:flex items-center gap-5 text-[12px] font-mono">
              <div className="text-right">
                <div className="text-grey/80 text-[11px]">TOTAL</div>
                <div className="text-white font-semibold">{stats.total_signals.toLocaleString()}</div>
              </div>
              <div className="text-right">
                <div className="text-grey/80 text-[11px]">HIGH CONF</div>
                <div className="text-white font-semibold">{stats.high_confidence_count.toLocaleString()}</div>
              </div>
              <div className="text-right">
                <div className="text-grey/80 text-[11px]">AVG CONF</div>
                <div className="text-white font-semibold">{(stats.avg_confidence * 100).toFixed(1)}%</div>
              </div>
            </div>
          )}

          {/* Latency - high precision */}
          <div className="text-[12px] font-mono text-right hidden sm:block">
            <div className="text-grey/80 text-[11px]">LATENCY</div>
            <div
              className={`font-semibold ${
                displayLatency < 10 ? 'text-success' : displayLatency < 50 ? 'text-warning' : 'text-danger'
              }`}
            >
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
            <div className="text-[12px] font-mono text-right hidden sm:block">
              <div className="text-grey/80 text-[11px]">OPERATOR</div>
              <div className="text-white">{user?.username || 'ANON'}</div>
            </div>

            <ExitButton onLogout={logout} />
          </div>
        </div>
      </div>
    </header>
  );
};

const exitButtonClassName =
  'border border-grey/30 px-3 py-1 text-[11px] font-mono text-grey/80 hover:text-danger hover:border-danger transition-colors duration-150';

const ExitButton: React.FC<{ onLogout: () => void }> = ({ onLogout }) => {
  if (!PRIVY_ENABLED) {
    return (
      <button onClick={onLogout} className={exitButtonClassName}>
        EXIT
      </button>
    );
  }

  return <ExitButtonPrivy onLogout={onLogout} />;
};

const ExitButtonPrivy: React.FC<{ onLogout: () => void }> = ({ onLogout }) => {
  const { logout: privyLogout } = usePrivy();

  return (
    <button
      onClick={async () => {
        try {
          await privyLogout();
        } catch {
          // Ignore Privy logout errors; still clear BetterBot session.
        } finally {
          onLogout();
        }
      }}
      className={exitButtonClassName}
    >
      EXIT
    </button>
  );
};
