import { useState } from 'react';
import { AuthGuard } from './components/auth/AuthGuard';
import { AppShell } from './components/layout/AppShell';
import { TerminalHeader } from './components/terminal/TerminalHeader';
import { SignalFeed } from './components/terminal/SignalFeed';
import { VaultDashboard } from './components/terminal/VaultDashboard';
import { StatusBar } from './components/layout/StatusBar';
import { useWebSocket } from './hooks/useWebSocket';
import { useSignals } from './hooks/useSignals';

const App: React.FC = () => {
  const { latency, isConnected } = useWebSocket();
  const { signals, stats, error } = useSignals({ wsConnected: isConnected });
  const [currentView, setCurrentView] = useState<'terminal' | 'vault'>('terminal');

  return (
    <AppShell>
      <AuthGuard>
        <div className="flex flex-col h-screen">
          {/* Header */}
          <TerminalHeader
            stats={stats}
            latency={latency}
            isConnected={isConnected}
            currentView={currentView}
            onViewChange={setCurrentView}
          />

          {/* Main Content */}
          <div className="flex-1 overflow-hidden">
            {currentView === 'terminal' ? (
              <SignalFeed signals={signals} stats={stats} error={error} />
            ) : (
              <VaultDashboard />
            )}
          </div>

          {/* Status Bar */}
          <StatusBar isConnected={isConnected} />
        </div>
      </AuthGuard>
    </AppShell>
  );
};

export default App;
