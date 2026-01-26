import { useState } from 'react';
import { AuthGuard } from './components/auth/AuthGuard';
import { AppShell } from './components/layout/AppShell';
import { TerminalHeader } from './components/terminal/TerminalHeader';
import { SignalFeed } from './components/terminal/SignalFeed';
import { VaultDashboard } from './components/terminal/VaultDashboard';
import { PerformanceDashboard } from './components/terminal/PerformanceDashboard';
import { StatusBar } from './components/layout/StatusBar';
import { useWebSocket } from './hooks/useWebSocket';
import { useSignals } from './hooks/useSignals';
import { useAuth } from './hooks/useAuth';
import { useTheme } from './hooks/useTheme';

export type AppView = 'terminal' | 'vault' | 'performance';

// Main app content (shown on "/" routes)
const MainApp: React.FC = () => {
  const { latency, isConnected } = useWebSocket();
  const { signals, stats, error } = useSignals({ wsConnected: isConnected });
  const { user } = useAuth();
  const { theme, toggleTheme } = useTheme();
  const [currentView, setCurrentView] = useState<AppView>('terminal');

  const isAdmin = user?.role === 'admin';

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
            isAdmin={isAdmin}
            theme={theme}
            onToggleTheme={toggleTheme}
          />

          {/* Main Content */}
          <div className="flex-1 overflow-hidden">
            {currentView === 'terminal' && (
              <SignalFeed signals={signals} stats={stats} error={error} />
            )}
            {currentView === 'vault' && <VaultDashboard />}
            {currentView === 'performance' && isAdmin && <PerformanceDashboard />}
          </div>

          {/* Status Bar */}
          <StatusBar isConnected={isConnected} />
        </div>
      </AuthGuard>
    </AppShell>
  );
};

// Root App component
const App: React.FC = () => {
  return <MainApp />;
};

export default App;
