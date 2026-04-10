import { useState, type FormEvent } from 'react';
import { Container, Loader2 } from 'lucide-react';
import { useAuth } from './AuthContext';

export default function LoginScreen() {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const { login, register, needsSetup, error, clearError } = useAuth();

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!username.trim() || !password.trim()) return;
    if (needsSetup && password !== confirmPassword) {
      // handled by disabled button, but double-check
      return;
    }

    setIsSubmitting(true);
    try {
      const normalizedUsername = username.trim().toLowerCase();
      if (needsSetup) {
        await register(normalizedUsername, password.trim());
      } else {
        await login(normalizedUsername, password.trim());
      }
    } catch {
      // Error is handled in AuthContext
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="min-h-screen bg-slate-950 flex items-center justify-center p-4">
      <div className="w-full max-w-sm">
        {/* Logo */}
        <div className="flex items-center justify-center gap-3 mb-8">
          <div className="w-10 h-10 rounded-xl bg-violet-600 flex items-center justify-center shadow-lg shadow-violet-500/20">
            <Container size={20} className="text-white" />
          </div>
          <div>
            <h1 className="text-lg font-semibold text-slate-100">LiteBin</h1>
            <p className="text-xs text-slate-500">Container Dashboard</p>
          </div>
        </div>

        {/* Card */}
        <div className="bg-slate-900/50 border border-slate-800/80 rounded-xl p-6 shadow-xl">
          <h2 className="text-sm font-medium text-slate-200 mb-1">
            {needsSetup ? 'Create admin account' : 'Sign in to your account'}
          </h2>
          {needsSetup && (
            <p className="text-xs text-slate-500 mb-5">Set up the initial admin user for LiteBin</p>
          )}
          {!needsSetup && <div className="mb-5" />}

          <form onSubmit={handleSubmit} className="space-y-4">
            {/* Username */}
            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Username
              </label>
              <input
                type="text"
                value={username}
                onChange={(e) => {
                  setUsername(e.target.value);
                  clearError();
                }}
                placeholder="Enter username"
                className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                disabled={isSubmitting}
              />
            </div>

            {/* Password */}
            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Password
              </label>
              <input
                type="password"
                value={password}
                onChange={(e) => {
                  setPassword(e.target.value);
                  clearError();
                }}
                placeholder="Enter password"
                className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                disabled={isSubmitting}
              />
            </div>

            {/* Confirm Password (setup only) */}
            {needsSetup && (
              <div>
                <label className="block text-xs font-medium text-slate-400 mb-1.5">
                  Confirm password
                </label>
                <input
                  type="password"
                  value={confirmPassword}
                  onChange={(e) => {
                    setConfirmPassword(e.target.value);
                    clearError();
                  }}
                  placeholder="Confirm password"
                  className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                  disabled={isSubmitting}
                />
              </div>
            )}

            {/* Error */}
            {error && (
              <div className="px-3 py-2.5 rounded-lg bg-rose-500/10 border border-rose-500/20">
                <p className="text-xs text-rose-400">{error}</p>
              </div>
            )}

            {/* Submit */}
            <button
              type="submit"
              disabled={isSubmitting || !username.trim() || !password.trim() || (needsSetup && password !== confirmPassword)}
              className="w-full flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isSubmitting ? (
                <>
                  <Loader2 size={14} className="animate-spin" />
                  {needsSetup ? 'Creating...' : 'Signing in...'}
                </>
              ) : (
                needsSetup ? 'Create admin' : 'Sign in'
              )}
            </button>
          </form>
        </div>
      </div>
    </div>
  );
}
