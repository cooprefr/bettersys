import { create } from 'zustand';
import { User } from '../types/auth';
import { api } from '../services/api';

interface AuthStore {
  user: User | null;
  token: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  error: string | null;

  login: (username: string, password: string) => Promise<void>;
  loginWithPrivy: (identityToken: string) => Promise<void>;
  logout: () => void;
  setUser: (user: User | null) => void;
  setError: (error: string | null) => void;
  validateToken: () => Promise<void>;
}

export const useAuthStore = create<AuthStore>((set) => {
  // Prevent stale auth requests (e.g. token validation) from overwriting a newer login.
  let requestId = 0;
  const nextRequestId = () => {
    requestId += 1;
    return requestId;
  };

  return {
    user: null,
    token: api.getToken(),
    isAuthenticated: false, // Start false, validate token on mount
    isLoading: false, // Don't block manual login while validating stored tokens
    error: null,

    login: async (username, password) => {
      const req = nextRequestId();
      set({ isLoading: true, error: null });
      try {
        const response = await api.login({ username, password });
        if (req !== requestId) return;
        set({
          user: response.user,
          token: response.token,
          isAuthenticated: true,
          isLoading: false,
          error: null,
        });
      } catch (error: any) {
        if (req !== requestId) return;
        set({
          error: error.message || 'Login failed',
          isLoading: false,
          isAuthenticated: false,
        });
        throw error;
      }
    },

    loginWithPrivy: async (identityToken) => {
      const req = nextRequestId();
      set({ isLoading: true, error: null });
      try {
        const response = await api.loginWithPrivy({ identity_token: identityToken });
        if (req !== requestId) return;
        set({
          user: response.user,
          token: response.token,
          isAuthenticated: true,
          isLoading: false,
          error: null,
        });
      } catch (error: any) {
        if (req !== requestId) return;
        set({
          error: error.message || 'Privy login failed',
          isLoading: false,
          isAuthenticated: false,
        });
        throw error;
      }
    },

    logout: () => {
      nextRequestId();
      api.logout();
      set({
        user: null,
        token: null,
        isAuthenticated: false,
        isLoading: false,
        error: null,
      });
    },

    setUser: (user) => set({ user }),

    setError: (error) => set({ error }),

    validateToken: async () => {
      const token = api.getToken();
      if (!token) {
        nextRequestId();
        set({ user: null, token: null, isAuthenticated: false, isLoading: false });
        return;
      }

      const req = nextRequestId();
      try {
        // Try to fetch user info to validate token
        const response = await api.getCurrentUser();
        if (req !== requestId) return;
        set({
          user: response.user,
          isAuthenticated: true,
          isLoading: false,
        });
      } catch {
        if (req !== requestId) return;
        // Token invalid, clear it
        api.logout();
        set({
          user: null,
          token: null,
          isAuthenticated: false,
          isLoading: false,
        });
      }
    },
  };
});

// Auto-validate token on store creation
const token = api.getToken();
if (token) {
  useAuthStore.getState().validateToken();
}
