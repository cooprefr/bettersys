import React from 'react';

interface StatusBarProps {
  isConnected: boolean;
}

export const StatusBar: React.FC<StatusBarProps> = ({ isConnected }) => {
  return (
    <div className="border-t border-grey/10 px-4 py-2 flex justify-between items-center text-[10px] font-mono">
      <div className="flex items-center gap-4 text-grey/70">
        <span>BETTER TERMINAL</span>
      </div>

      <div className="flex items-center gap-2">
        <div className="flex items-center gap-1">
          <div className={`w-1.5 h-1.5 rounded-full ${isConnected ? 'bg-success' : 'bg-danger'}`} />
          <span className={isConnected ? 'text-success' : 'text-danger'}>
            {isConnected ? 'CONNECTED' : 'OFFLINE'}
          </span>
        </div>
      </div>
    </div>
  );
};
