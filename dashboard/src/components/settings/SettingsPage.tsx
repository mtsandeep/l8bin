import { DatabaseBackup, Key, SlidersHorizontal } from 'lucide-react';
import { NavLink, Outlet, useNavigate } from 'react-router-dom';
import { useAuth } from '../AuthContext';
import BackupPanel from './BackupPanel';
import GeneralPanel from './GeneralPanel';
import TokensPanel from './TokensPanel';

interface Props {
  section: 'general' | 'tokens' | 'backup';
}

const SECTIONS = [
  { id: 'general', label: 'General', icon: SlidersHorizontal },
  { id: 'tokens', label: 'Access Tokens', icon: Key },
  { id: 'backup', label: 'Backup', icon: DatabaseBackup },
] as const;

export default function SettingsPage({ section }: Props) {
  const navigate = useNavigate();
  const { user } = useAuth();

  return (
    <div className="min-h-screen bg-slate-950 text-slate-200">
      <header className="border-b border-slate-800/80 bg-slate-900/50 backdrop-blur-md sticky top-0 z-40">
        <div className="max-w-6xl mx-auto px-4 sm:px-6 py-3 sm:py-4 flex items-center gap-3">
          <button
            type="button"
            onClick={() => navigate('/')}
            className="text-slate-500 hover:text-slate-300 text-sm transition-colors"
          >
            ← Back
          </button>
          <div className="w-px h-4 bg-slate-700" />
          <h1 className="text-sm font-semibold text-slate-100">Settings</h1>
        </div>
      </header>

      <main className="max-w-6xl mx-auto px-4 sm:px-6 py-6 flex flex-col md:flex-row gap-6 md:gap-10">
        {/* Section nav */}
        <nav className="flex md:flex-col gap-1 md:w-52 shrink-0 md:border-r border-slate-800/80 md:pr-4">
          {SECTIONS.map(({ id, label, icon: Icon }) => (
            <NavLink
              key={id}
              to={`/settings/${id}`}
              className={`flex items-center gap-2 px-3 py-2 rounded-md text-xs font-medium transition-colors ${
                section === id
                  ? 'bg-violet-500/10 text-violet-300'
                  : 'text-slate-400 hover:text-slate-200 hover:bg-slate-800/60'
              }`}
            >
              <Icon size={13} />
              {label}
            </NavLink>
          ))}
        </nav>

        {/* Section content */}
        <div className="flex-1 min-w-0 max-w-3xl">
          {!user?.is_admin ? (
            <p className="text-xs text-slate-400 py-8 text-center">Administrator access required.</p>
          ) : section === 'general' ? (
            <GeneralPanel />
          ) : section === 'tokens' ? (
            <TokensPanel />
          ) : (
            <BackupPanel />
          )}
        </div>
      </main>
      <Outlet />
    </div>
  );
}
