export interface User {
  id: string;
  username: string;
  role: UserRole;
  created_at: string;
}

export type UserRole = 'admin' | 'trader' | 'viewer';

export interface LoginRequest {
  username: string;
  password: string;
}

export interface LoginResponse {
  token: string;
  expires_at: string;
  user: User;
}

export interface AuthState {
  user: User | null;
  token: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  error: string | null;
}
