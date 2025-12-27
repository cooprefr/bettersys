import React, { useEffect, useMemo, useState } from 'react';
import { useIdentityToken, usePrivy } from '@privy-io/react-auth';
import { useAuth } from '../../hooks/useAuth';

export const LoginScreen: React.FC = () => {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const { login, loginWithPrivy, isLoading: authLoading, error: authError, isAuthenticated } = useAuth();
  const { ready, authenticated: privyAuthenticated, user: privyUser, login: privyLogin, logout: privyLogout } = usePrivy();
  const { identityToken } = useIdentityToken();
  const [privyError, setPrivyError] = useState<string | null>(null);
  const [exchangeAttempted, setExchangeAttempted] = useState(false);

  const privyConfigured = useMemo(() => {
    const appId = import.meta.env.VITE_PRIVY_APP_ID;
    return typeof appId === 'string' && appId.trim().length > 0;
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await login(username, password);
    } catch (err) {
      console.error('Login failed:', err);
    }
  };

  const handlePrivy = async () => {
    setPrivyError(null);
    setExchangeAttempted(false);
    if (!privyConfigured) {
      setPrivyError('Privy not configured (missing VITE_PRIVY_APP_ID)');
      return;
    }
    privyLogin();
  };

  useEffect(() => {
    if (!ready) return;
    if (isAuthenticated) return;
    if (!privyAuthenticated) return;
    if (!identityToken) return;
    if (exchangeAttempted) return;

    setExchangeAttempted(true);
    loginWithPrivy(identityToken).catch((err: any) => {
      setPrivyError(err?.message || 'Privy login failed');
    });
  }, [ready, isAuthenticated, privyAuthenticated, identityToken, exchangeAttempted, loginWithPrivy]);

  const error = authError || privyError;
  const isLoading = authLoading;

  return (
    <div className="min-h-screen flex items-center justify-center bg-void">
      <div className="w-full max-w-md px-8">
        {/* Logo */}
        <div className="text-center mb-12">
          <img src="/logo-better.svg" alt="BETTER" className="h-14 w-auto mx-auto mb-4" />
          <div className="text-grey/80 font-mono text-xs tracking-widest">
            POLYMARKET SIGNAL TERMINAL
          </div>
        </div>

        {/* Auth Container */}
        <div className="bg-surface border border-grey/10 p-8">

          {/* Privy */}
          <div className="mb-8 space-y-3">
            <button
              type="button"
              onClick={privyAuthenticated ? () => void privyLogout() : handlePrivy}
              disabled={!ready || isLoading || !privyConfigured}
              className={`w-full border py-3 transition-colors duration-150 disabled:opacity-50 disabled:cursor-not-allowed ${
                privyAuthenticated
                  ? 'border-success bg-success/10 hover:bg-success/15'
                  : 'border-grey/20 hover:border-better-blue hover:bg-better-blue/10'
              }`}
            >
              <span className="font-mono text-xs tracking-widest text-white">
                {privyAuthenticated ? 'PRIVY CONNECTED (CLICK TO LOG OUT)' : 'LOGIN WITH PRIVY'}
              </span>
            </button>

            <div className="text-[10px] font-mono text-grey/70">
              ACCESS: ≥100,000 $BETTER (BASE)
            </div>

            {privyAuthenticated && (
              <div className="p-3 border border-success/30 bg-success/5">
                <div className="text-[10px] font-mono text-success">
                  PRIVY USER: {privyUser?.id || '---'}
                </div>
              </div>
            )}
          </div>

          <div className="relative flex items-center gap-4 mb-8">
            <div className="h-px bg-grey/20 flex-1"></div>
            <div className="text-[10px] text-grey/70 font-mono">OR LOGIN</div>
            <div className="h-px bg-grey/20 flex-1"></div>
          </div>

          {/* Login Form */}
          <form onSubmit={handleSubmit} className="space-y-4">
            {/* Username */}
            <div className="space-y-1">
              <label className="block text-[10px] font-mono text-grey/80 tracking-widest">
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
              <label className="block text-[10px] font-mono text-grey/80 tracking-widest">
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
        <div className="mt-8 flex justify-between text-[10px] font-mono text-grey/70">
          <div>STATUS: ONLINE</div>
          <div className="flex items-center gap-2">
            <div className="w-1.5 h-1.5 rounded-full bg-success"></div>
            SECURE
          </div>
        </div>

        {/* Dev hint */}
        {import.meta.env.DEV && (
          <div className="mt-4 text-center text-[10px] font-mono text-grey/60">
            DEV: admin / admin123
          </div>
        )}
      </div>
    </div>
  );
};
