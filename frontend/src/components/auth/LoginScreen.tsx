import React, { useEffect, useState } from 'react';
import { useIdentityToken, usePrivy } from '@privy-io/react-auth';
import { useAuth } from '../../hooks/useAuth';
import { PRIVY_ENABLED } from '../../config/privy';

const PrivyLoginSection: React.FC = () => {
  const { loginWithPrivy, isLoading: authLoading, isAuthenticated } = useAuth();
  const {
    ready,
    authenticated: privyAuthenticated,
    user: privyUser,
    login: privyLogin,
    logout: privyLogout,
  } = usePrivy();
  const { identityToken } = useIdentityToken();
  const [exchangeAttempted, setExchangeAttempted] = useState(false);

  useEffect(() => {
    if (!ready) return;
    if (isAuthenticated) return;
    if (!privyAuthenticated) return;
    if (!identityToken) return;
    if (exchangeAttempted) return;

    setExchangeAttempted(true);
    loginWithPrivy(identityToken).catch(() => {});
  }, [ready, isAuthenticated, privyAuthenticated, identityToken, exchangeAttempted, loginWithPrivy]);

  return (
    <div className="mb-8 space-y-3">
      <button
        type="button"
        onClick={privyAuthenticated ? () => void privyLogout() : () => {
          setExchangeAttempted(false);
          privyLogin();
        }}
        disabled={!ready || authLoading}
        className={`w-full border py-3 transition-colors duration-150 disabled:opacity-50 disabled:cursor-not-allowed ${
          privyAuthenticated
            ? 'border-success bg-success/10 hover:bg-success/15'
            : 'border-grey/20 hover:border-better-blue hover:bg-better-blue/10'
        }`}
      >
        <span className="font-mono text-xs tracking-widest text-fg">
          {privyAuthenticated ? 'PRIVY CONNECTED (CLICK TO LOG OUT)' : 'LOGIN WITH PRIVY'}
        </span>
      </button>

      <div className="text-[10px] font-mono text-fg/80">ACCESS: ≥100,000 $BETTER (BASE)</div>

      {privyAuthenticated && (
        <div className="p-3 border border-success/30 bg-success/5">
          <div className="text-[10px] font-mono text-success">PRIVY USER: {privyUser?.id || '---'}</div>
        </div>
      )}
    </div>
  );
};

export const LoginScreen: React.FC = () => {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const { login, isLoading: authLoading, error: authError } = useAuth();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await login(username, password);
    } catch (err) {
      console.error('Login failed:', err);
    }
  };

  const error = authError;
  const isLoading = authLoading;

  return (
    <div className="min-h-screen flex items-center justify-center bg-void">
      <div className="w-full max-w-md px-8">
        {/* Logo */}
        <div className="text-center mb-12">
          <img src="/logo-better.svg" alt="BETTER" className="h-14 w-auto mx-auto mb-4" />
          <div className="text-fg/90 font-mono text-xs tracking-widest">
            POLYMARKET SIGNAL TERMINAL
          </div>
        </div>

        {/* Auth Container */}
        <div className="bg-surface border border-grey/10 p-8">

          {/* Privy */}
          {PRIVY_ENABLED && (
            <>
              <PrivyLoginSection />
              <div className="relative flex items-center gap-4 mb-8">
                <div className="h-px bg-grey/20 flex-1"></div>
                <div className="text-[10px] text-fg/80 font-mono">OR LOGIN</div>
                <div className="h-px bg-grey/20 flex-1"></div>
              </div>
            </>
          )}

          {/* Login Form */}
          <form onSubmit={handleSubmit} className="space-y-4">
            {/* Username */}
            <div className="space-y-1">
              <label className="block text-[10px] font-mono text-fg/90 tracking-widest">
                USERNAME
              </label>
              <input
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full bg-void border border-grey/20 p-3 text-fg font-mono text-sm focus:outline-none focus:border-better-blue transition-colors duration-150 placeholder-grey/30"
                placeholder="Enter username"
                autoComplete="username"
                required
              />
            </div>

            {/* Password */}
            <div className="space-y-1">
              <label className="block text-[10px] font-mono text-fg/90 tracking-widest">
                PASSWORD
              </label>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full bg-void border border-grey/20 p-3 text-fg font-mono text-sm focus:outline-none focus:border-better-blue transition-colors duration-150 placeholder-grey/30"
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
        <div className="mt-8 flex justify-between text-[10px] font-mono text-fg/80">
          <div>STATUS: ONLINE</div>
          <div className="flex items-center gap-2">
            <div className="w-1.5 h-1.5 rounded-full bg-success"></div>
            SECURE
          </div>
        </div>

        {/* Dev hint */}
        {import.meta.env.DEV && (
          <div className="mt-4 text-center text-[10px] font-mono text-fg/70">
            DEV: admin / admin123
          </div>
        )}
      </div>
    </div>
  );
};
