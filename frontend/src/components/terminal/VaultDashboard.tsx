import React from 'react';

export const VaultDashboard: React.FC = () => {
  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* Header Section */}
      <div className="flex items-end justify-between mb-6 border-b border-grey/20 pb-4">
        <div>
          <h1 className="text-3xl font-semibold text-white font-mono">AGENT SPIKE</h1>
          <div className="text-[10px] text-grey/50 tracking-widest mt-1">MANAGED PORTFOLIO // BASE NETWORK</div>
        </div>
        <div className="text-right">
          <div className="text-[10px] text-grey/50 tracking-widest mb-1">NET APY</div>
          <div className="text-2xl font-semibold text-better-blue font-mono">420.69%</div>
        </div>
      </div>

      {/* Stats Grid */}
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4 mb-6">
        <div className="bg-surface border border-grey/10 p-4">
          <div className="text-[10px] text-grey/50 mb-2">TOTAL VALUE LOCKED</div>
          <div className="text-xl font-mono text-white">$1,245,890.00</div>
        </div>
        <div className="bg-surface border border-grey/10 p-4">
          <div className="text-[10px] text-grey/50 mb-2">24H PNL</div>
          <div className="text-xl font-mono text-better-blue">+$12,450.23</div>
        </div>
        <div className="bg-surface border border-grey/10 p-4">
          <div className="text-[10px] text-grey/50 mb-2">SHARE PRICE</div>
          <div className="text-xl font-mono text-white">$1.45 USDC</div>
        </div>
      </div>

      {/* Chart Placeholder */}
      <div className="bg-surface border border-grey/10 h-64 mb-6 flex items-center justify-center relative overflow-hidden">
        <div className="text-grey/30 font-mono text-sm">
          PERFORMANCE CHART
        </div>
        <svg className="absolute inset-0 h-full w-full p-4" preserveAspectRatio="none">
          <path d="M0,200 C50,180 100,210 150,150 C200,100 250,120 300,80 C350,40 400,60 500,20 L500,300 L0,300 Z" fill="rgba(59, 130, 246, 0.05)" stroke="none" />
          <path d="M0,200 C50,180 100,210 150,150 C200,100 250,120 300,80 C350,40 400,60 500,20" fill="none" stroke="#3B82F6" strokeWidth="1" />
        </svg>
      </div>

      {/* Actions */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <button className="bg-better-blue hover:bg-blue-700 text-white font-semibold py-4 px-6 transition-colors duration-150 flex items-center justify-center gap-2">
          <span>DEPOSIT USDC</span>
          <span className="text-xs opacity-70">(15% FEE)</span>
        </button>
        <button className="bg-surface border border-grey/20 hover:border-better-blue text-white font-semibold py-4 px-6 transition-colors duration-150 flex items-center justify-center gap-2">
          <span>DEPOSIT $BETTER</span>
          <span className="text-xs opacity-70">(5% FEE)</span>
        </button>
      </div>

      {/* Disclaimer */}
      <div className="mt-8 text-[10px] text-grey/30 text-center font-mono">
        PAST PERFORMANCE DOES NOT GUARANTEE FUTURE RESULTS. AGENT SPIKE IS AN EXPERIMENTAL AUTONOMOUS TRADING AGENT.
      </div>
    </div>
  );
};
