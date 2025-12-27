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
  expires_in: number;
  role: UserRole;
  user: User;
}

export interface PrivyLoginRequest {
  identity_token: string;
}

export interface AuthState {
  user: User | null;
  token: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  error: string | null;
}
