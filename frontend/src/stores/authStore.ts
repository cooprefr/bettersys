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

export const useAuthStore = create<AuthStore>((set, _get) => ({
  user: null,
  token: api.getToken(),
  isAuthenticated: false, // Start false, validate token on mount
  isLoading: !!api.getToken(), // Loading if we have a token to validate
  error: null,

  login: async (username, password) => {
    set({ isLoading: true, error: null });
    try {
      const response = await api.login({ username, password });
      set({
        user: response.user,
        token: response.token,
        isAuthenticated: true,
        isLoading: false,
        error: null,
      });
    } catch (error: any) {
      set({
        error: error.message || 'Login failed',
        isLoading: false,
        isAuthenticated: false,
      });
      throw error;
    }
  },

  loginWithPrivy: async (identityToken) => {
    set({ isLoading: true, error: null });
    try {
      const response = await api.loginWithPrivy({ identity_token: identityToken });
      set({
        user: response.user,
        token: response.token,
        isAuthenticated: true,
        isLoading: false,
        error: null,
      });
    } catch (error: any) {
      set({
        error: error.message || 'Privy login failed',
        isLoading: false,
        isAuthenticated: false,
      });
      throw error;
    }
  },

  logout: () => {
    api.logout();
    set({
      user: null,
      token: null,
      isAuthenticated: false,
      error: null,
    });
  },

  setUser: (user) => set({ user }),
  
  setError: (error) => set({ error }),

  validateToken: async () => {
    const token = api.getToken();
    if (!token) {
      set({ isAuthenticated: false, isLoading: false });
      return;
    }

    try {
      // Try to fetch user info to validate token
      const response = await api.getCurrentUser();
      set({
        user: response.user,
        isAuthenticated: true,
        isLoading: false,
      });
    } catch {
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
}));

// Auto-validate token on store creation
const token = api.getToken();
if (token) {
  useAuthStore.getState().validateToken();
}
