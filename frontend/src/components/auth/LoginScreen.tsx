import React, { useState } from 'react';
import { useAuth } from '../../hooks/useAuth';
import { useWallet } from '../../hooks/useWallet';

export const LoginScreen: React.FC = () => {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const { login, isLoading: authLoading, error: authError } = useAuth();
  const { 
    connect: connectWallet, 
    isLoading: walletLoading, 
    error: walletError,
    isConnected: walletConnected,
    address: walletAddress,
    walletType,
  } = useWallet();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await login(username, password);
    } catch (err) {
      console.error('Login failed:', err);
    }
  };

  const handleWalletConnect = async (type: 'metamask' | 'phantom') => {
    await connectWallet(type);
  };

  // Format wallet address for display
  const formatAddress = (addr: string) => {
    return `${addr.slice(0, 6)}...${addr.slice(-4)}`;
  };

  const error = authError || walletError;
  const isLoading = authLoading || walletLoading;

  return (
    <div className="min-h-screen flex items-center justify-center bg-void">
      <div className="w-full max-w-md px-8">
        {/* Logo */}
        <div className="text-center mb-12">
          <img src="/logo-better.svg" alt="BETTER" className="h-14 w-auto mx-auto mb-4" />
          <div className="text-grey/50 font-mono text-xs tracking-widest">
            POLYMARKET SIGNAL TERMINAL
          </div>
        </div>

        {/* Auth Container */}
        <div className="bg-surface border border-grey/10 p-8">
          
          {/* Wallet Connect Buttons */}
          <div className="grid grid-cols-2 gap-4 mb-8">
            <button 
              onClick={() => handleWalletConnect('metamask')}
              disabled={walletLoading}
              className={`flex flex-col items-center justify-center gap-1 py-4 px-4 border transition-colors duration-150 ${
                walletConnected && walletType === 'metamask' 
                  ? 'border-success bg-success/10' 
                  : 'border-grey/20 hover:border-better-blue hover:bg-better-blue/10'
              }`}
            >
              <span className="text-xs font-mono text-white">METAMASK</span>
              <span className="text-[9px] font-mono text-grey/50">BASE CHAIN</span>
              {walletConnected && walletType === 'metamask' && (
                <span className="text-[9px] font-mono text-success mt-1">
                  {formatAddress(walletAddress!)}
                </span>
              )}
            </button>
            <button 
              onClick={() => handleWalletConnect('phantom')}
              disabled={walletLoading}
              className={`flex flex-col items-center justify-center gap-1 py-4 px-4 border transition-colors duration-150 ${
                walletConnected && walletType === 'phantom' 
                  ? 'border-success bg-success/10' 
                  : 'border-grey/20 hover:border-better-blue hover:bg-better-blue/10'
              }`}
            >
              <span className="text-xs font-mono text-white">PHANTOM</span>
              <span className="text-[9px] font-mono text-grey/50">SOLANA</span>
              {walletConnected && walletType === 'phantom' && (
                <span className="text-[9px] font-mono text-success mt-1">
                  {formatAddress(walletAddress!)}
                </span>
              )}
            </button>
          </div>

          {walletConnected && (
            <div className="mb-6 p-3 border border-success/30 bg-success/5">
              <div className="text-[10px] font-mono text-success text-center">
                WALLET CONNECTED: {formatAddress(walletAddress!)}
              </div>
            </div>
          )}

          <div className="relative flex items-center gap-4 mb-8">
            <div className="h-px bg-grey/20 flex-1"></div>
            <div className="text-[10px] text-grey/40 font-mono">OR LOGIN</div>
            <div className="h-px bg-grey/20 flex-1"></div>
          </div>

          {/* Login Form */}
          <form onSubmit={handleSubmit} className="space-y-4">
            {/* Username */}
            <div className="space-y-1">
              <label className="block text-[10px] font-mono text-grey/50 tracking-widest">
                USERNAME
              </label>
              <input
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full bg-void border border-grey/20 p-3 text-white font-mono text-sm focus:outline-none focus:border-better-blue transition-colors duration-150 placeholder-grey/30"
                placeholder="Enter username"
                autoComplete="username"
                required
              />
            </div>

            {/* Password */}
            <div className="space-y-1">
              <label className="block text-[10px] font-mono text-grey/50 tracking-widest">
                PASSWORD
              </label>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full bg-void border border-grey/20 p-3 text-white font-mono text-sm focus:outline-none focus:border-better-blue transition-colors duration-150 placeholder-grey/30"
                placeholder="Enter password"
                autoComplete="current-password"
                required
              />
            </div>

            {/* Error Message */}
            {error && (
              <div className="border border-danger/50 bg-danger/10 p-3 flex items-center gap-3">
                <div className="text-danger text-xs font-mono">{error}</div>
              </div>
            )}

            {/* Submit Button */}
            <button
              type="submit"
              disabled={isLoading}
              className="w-full bg-better-blue hover:bg-blue-700 text-white font-semibold py-3 transition-colors duration-150 disabled:opacity-50 disabled:cursor-not-allowed mt-4"
            >
              <span className="font-mono text-xs tracking-widest">
                {authLoading ? 'CONNECTING...' : 'CONNECT'}
              </span>
            </button>
          </form>
        </div>

        {/* Status */}
        <div className="mt-8 flex justify-between text-[10px] font-mono text-grey/30">
          <div>STATUS: ONLINE</div>
          <div className="flex items-center gap-2">
            <div className="w-1.5 h-1.5 rounded-full bg-success"></div>
            SECURE
          </div>
        </div>

        {/* Dev hint */}
        {import.meta.env.DEV && (
          <div className="mt-4 text-center text-[10px] font-mono text-grey/20">
            DEV: admin / admin123
          </div>
        )}
      </div>
    </div>
  );
};
