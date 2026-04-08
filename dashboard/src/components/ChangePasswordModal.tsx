import { useState, type FormEvent } from 'react';
import { X, Loader2, Lock } from 'lucide-react';
import { useAuth } from './AuthContext';

interface ChangePasswordModalProps {
  onClose: () => void;
}

export default function ChangePasswordModal({ onClose }: ChangePasswordModalProps) {
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [success, setSuccess] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const { changePassword, error, clearError } = useAuth();

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    clearError();
    setValidationError(null);

    if (!currentPassword || !newPassword || !confirmPassword) {
      setValidationError('All fields are required');
      return;
    }

    if (newPassword.length < 8) {
      setValidationError('New password must be at least 8 characters');
      return;
    }

    if (newPassword !== confirmPassword) {
      setValidationError('New passwords do not match');
      return;
    }

    setIsSubmitting(true);
    try {
      await changePassword(currentPassword, newPassword);
      setSuccess(true);
      setTimeout(() => onClose(), 1500);
    } catch {
      // Error is handled in AuthContext
    } finally {
      setIsSubmitting(false);
    }
  };

  const displayError = validationError || error;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-slate-950/80 backdrop-blur-sm">
      <div className="w-full max-w-sm bg-slate-900/90 border border-slate-800 rounded-xl shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-800">
          <div className="flex items-center gap-2">
            <Lock size={16} className="text-violet-400" />
            <h2 className="text-sm font-medium text-slate-200">Change Password</h2>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="p-5">
          {success ? (
            <div className="text-center py-4">
              <div className="w-12 h-12 rounded-full bg-emerald-500/10 border border-emerald-500/20 flex items-center justify-center mx-auto mb-3">
                <svg className="w-6 h-6 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" />
                </svg>
              </div>
              <p className="text-sm text-emerald-400">Password changed successfully!</p>
            </div>
          ) : (
            <form onSubmit={handleSubmit} className="space-y-4">
              {/* Current Password */}
              <div>
                <label className="block text-xs font-medium text-slate-400 mb-1.5">
                  Current Password
                </label>
                <input
                  type="password"
                  value={currentPassword}
                  onChange={(e) => {
                    setCurrentPassword(e.target.value);
                    clearError();
                    setValidationError(null);
                  }}
                  placeholder="Enter current password"
                  className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                  disabled={isSubmitting}
                />
              </div>

              {/* New Password */}
              <div>
                <label className="block text-xs font-medium text-slate-400 mb-1.5">
                  New Password
                </label>
                <input
                  type="password"
                  value={newPassword}
                  onChange={(e) => {
                    setNewPassword(e.target.value);
                    clearError();
                    setValidationError(null);
                  }}
                  placeholder="Min 8 characters"
                  className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                  disabled={isSubmitting}
                />
              </div>

              {/* Confirm Password */}
              <div>
                <label className="block text-xs font-medium text-slate-400 mb-1.5">
                  Confirm New Password
                </label>
                <input
                  type="password"
                  value={confirmPassword}
                  onChange={(e) => {
                    setConfirmPassword(e.target.value);
                    clearError();
                    setValidationError(null);
                  }}
                  placeholder="Re-enter new password"
                  className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 placeholder-slate-500 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                  disabled={isSubmitting}
                />
              </div>

              {/* Error */}
              {displayError && (
                <div className="px-3 py-2.5 rounded-lg bg-rose-500/10 border border-rose-500/20">
                  <p className="text-xs text-rose-400">{displayError}</p>
                </div>
              )}

              {/* Actions */}
              <div className="flex gap-3 pt-2">
                <button
                  type="button"
                  onClick={onClose}
                  disabled={isSubmitting}
                  className="flex-1 px-4 py-2 rounded-lg text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors"
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={isSubmitting || !currentPassword || !newPassword || !confirmPassword}
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                >
                  {isSubmitting ? (
                    <>
                      <Loader2 size={14} className="animate-spin" />
                      Saving...
                    </>
                  ) : (
                    'Change Password'
                  )}
                </button>
              </div>
            </form>
          )}
        </div>
      </div>
    </div>
  );
}
