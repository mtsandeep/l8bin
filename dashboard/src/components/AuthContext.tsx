import { createContext, useContext, useState, useEffect, type ReactNode } from 'react';

export interface User {
  id: string;
  username: string;
  email?: string;
  is_admin: boolean;
}

interface AuthContextType {
  user: User | null;
  loading: boolean;
  needsSetup: boolean;
  login: (username: string, password: string) => Promise<void>;
  register: (username: string, password: string, email?: string) => Promise<void>;
  logout: () => Promise<void>;
  changePassword: (currentPassword: string, newPassword: string) => Promise<void>;
  error: string | null;
  clearError: () => void;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

const API_BASE = '';

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState(true);
  const [needsSetup, setNeedsSetup] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    checkSession();
  }, []);

  const checkSession = async () => {
    try {
      const [meResp, setupResp] = await Promise.all([
        fetch(`${API_BASE}/auth/me`, { credentials: 'include' }),
        fetch(`${API_BASE}/auth/setup`, { credentials: 'include' }),
      ]);

      if (setupResp.ok) {
        const setup = await setupResp.json();
        setNeedsSetup(setup.needs_setup);
      }

      if (meResp.ok) {
        const data = await meResp.json();
        setUser(data);
      }
    } catch (e) {
      console.error('Session check failed:', e);
    } finally {
      setLoading(false);
    }
  };

  const login = async (username: string, password: string) => {
    setError(null);
    try {
      const response = await fetch(`${API_BASE}/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify({ username, password }),
      });

      if (!response.ok) {
        const data = await response.json().catch(() => ({}));
        throw new Error(data.error || 'Login failed');
      }

      const data = await response.json();
      setUser(data.user);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Login failed');
      throw e;
    }
  };

  const register = async (username: string, password: string, email?: string) => {
    setError(null);
    try {
      const response = await fetch(`${API_BASE}/auth/register`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify({ username, password, email }),
      });

      if (!response.ok) {
        const data = await response.json().catch(() => ({}));
        if (response.status === 409) {
          throw new Error('Username already exists');
        }
        throw new Error(data.error || 'Registration failed');
      }

      const data = await response.json();
      setUser(data.user);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Registration failed');
      throw e;
    }
  };

  const logout = async () => {
    try {
      await fetch(`${API_BASE}/auth/logout`, {
        method: 'POST',
        credentials: 'include',
      });
      setUser(null);
    } catch (e) {
      console.error('Logout failed:', e);
    }
  };

  const changePassword = async (currentPassword: string, newPassword: string) => {
    setError(null);
    try {
      const response = await fetch(`${API_BASE}/auth/change-password`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
      });

      if (!response.ok) {
        const data = await response.json().catch(() => ({}));
        if (response.status === 401) {
          throw new Error('Current password is incorrect');
        }
        throw new Error(data.error || 'Failed to change password');
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to change password');
      throw e;
    }
  };

  const clearError = () => setError(null);

  return (
    <AuthContext.Provider
      value={{ user, loading, needsSetup, login, register, logout, changePassword, error, clearError }}
    >
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth() {
  const context = useContext(AuthContext);
  if (context === undefined) {
    throw new Error('useAuth must be used within an AuthProvider');
  }
  return context;
}
