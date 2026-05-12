import {
  ChevronDown,
  ChevronUp,
  Container,
  HardDrive,
  KeyRound,
  LogOut,
  MemoryStick,
  Plus,
  RefreshCw,
  Search,
  Server,
  Settings,
  User,
  X,
} from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { Navigate, Route, Routes, useNavigate } from 'react-router-dom';
import {
  fetchAllStats,
  fetchGlobalSettings,
  fetchNodes,
  fetchProjects,
  fetchSystemStats,
  formatBytes,
  type Node,
  type Project,
  type ProjectStats,
  ProjectStatus,
  type ServiceStats,
} from './api';
import { AuthProvider, useAuth } from './components/AuthContext';
import ChangePasswordModal from './components/ChangePasswordModal';
import DeployForm from './components/DeployForm';
import Footer from './components/Footer';
import GlobalSettingsModal from './components/GlobalSettingsModal';
import LoginScreen from './components/LoginScreen';
import NodesPage from './components/NodesPage';
import ProjectCard from './components/ProjectCard';
import ScanImportPage from './components/ScanImportPage';
import { ToastProvider } from './components/ToastContext';
import { useIntervalWhileVisible } from './hooks';

function HomePage({
  projects,
  stats,
  systemStats,
  nodes,
  projectsDir,
  domain,
  dnsTarget,
  loading,
  statusFilter,
  setStatusFilter,
  loadProjectsAndStats,
  setShowDeploy,
  setShowGlobalSettings,
  setShowChangePassword,
  showDeploy,
  showGlobalSettings,
  showChangePassword,
  showUserMenu,
  setShowUserMenu,
  userMenuRefMobile,
  userMenuRefDesktop,
  user,
  logout,
  stackExpanded,
  setStackExpanded,
}: {
  projects: Project[];
  stats: ProjectStats[];
  systemStats: ServiceStats[];
  nodes: Node[];
  projectsDir: string;
  domain: string;
  dnsTarget: string;
  loading: boolean;
  statusFilter: ProjectStatus | null;
  setStatusFilter: (f: ProjectStatus | null) => void;
  loadProjectsAndStats: () => void;
  setShowDeploy: (v: boolean) => void;
  setShowGlobalSettings: (v: boolean) => void;
  setShowChangePassword: (v: boolean) => void;
  showDeploy: boolean;
  showGlobalSettings: boolean;
  showChangePassword: boolean;
  showUserMenu: boolean;
  setShowUserMenu: (v: boolean) => void;
  userMenuRefMobile: React.RefObject<HTMLDivElement | null>;
  userMenuRefDesktop: React.RefObject<HTMLDivElement | null>;
  user: { username: string; is_admin: boolean };
  logout: () => void;
  stackExpanded: boolean;
  setStackExpanded: (v: boolean) => void;
}) {
  const navigate = useNavigate();

  const running = projects.filter((p) => p.status === ProjectStatus.Running).length;
  const stopped = projects.filter((p) => p.status === ProjectStatus.Stopped).length;
  const stopping = projects.filter((p) => p.status === ProjectStatus.Stopping).length;

  const sortedProjects = [...projects].sort((a, b) => b.id.localeCompare(a.id));
  const filteredProjects = statusFilter ? sortedProjects.filter((p) => p.status === statusFilter) : sortedProjects;

  return (
    <div className="min-h-screen bg-slate-950 text-slate-200">
      {/* Header */}
      <header className="border-b border-slate-800/80 bg-slate-900/50 backdrop-blur-md sticky top-0 z-40">
        <div className="max-w-6xl mx-auto px-4 sm:px-6 py-3 sm:py-4 flex flex-col gap-2 sm:flex-row sm:items-center">
          {/* Mobile: logo left, user right. Desktop: logo left. */}
          <div className="flex items-center justify-between sm:justify-start sm:gap-3">
            <div className="flex items-center gap-3">
              <div className="w-8 h-8 rounded-lg bg-violet-600 flex items-center justify-center">
                <Container size={16} className="text-white" />
              </div>
              <div>
                <h1 className="text-base font-semibold text-slate-100 leading-none">LiteBin</h1>
                <p className="text-[11px] text-slate-500 mt-0.5">Container Dashboard</p>
              </div>
            </div>
            {/* User dropdown — mobile only here */}
            <div className="relative sm:hidden" ref={userMenuRefMobile}>
              <button
                type="button"
                onClick={() => setShowUserMenu(!showUserMenu)}
                className="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-slate-800/50 border border-slate-700/50 hover:border-slate-600/50 transition-colors cursor-pointer"
              >
                <User size={14} className="text-slate-400" />
                <span className="text-xs text-slate-300">{user.username}</span>
                {user.is_admin && (
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-violet-500/20 text-violet-400">admin</span>
                )}
                <ChevronDown size={12} className="text-slate-500" />
              </button>
              {showUserMenu && (
                <div className="absolute right-0 mt-1 w-44 bg-slate-800 border border-slate-700/50 rounded-lg shadow-xl py-1 z-50">
                  <button
                    type="button"
                    onClick={() => {
                      setShowUserMenu(false);
                      setShowChangePassword(true);
                    }}
                    className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer"
                  >
                    <KeyRound size={14} className="text-slate-400" />
                    Change Password
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setShowUserMenu(false);
                      logout();
                    }}
                    className="w-full flex items-center gap-2 px-3 py-2 text-xs text-rose-400 hover:bg-rose-500/10 transition-colors cursor-pointer"
                  >
                    <LogOut size={14} />
                    Logout
                  </button>
                </div>
              )}
            </div>
          </div>

          {/* Desktop: profile + action buttons pushed to right */}
          <div className="flex items-center justify-end gap-3 sm:ml-auto">
            {/* User dropdown — desktop only */}
            <div className="relative hidden sm:block" ref={userMenuRefDesktop}>
              <button
                type="button"
                onClick={() => setShowUserMenu(!showUserMenu)}
                className="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-slate-800/50 border border-slate-700/50 hover:border-slate-600/50 transition-colors cursor-pointer"
              >
                <User size={14} className="text-slate-400" />
                <span className="text-xs text-slate-300">{user.username}</span>
                {user.is_admin && (
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-violet-500/20 text-violet-400">admin</span>
                )}
                <ChevronDown size={12} className="text-slate-500" />
              </button>
              {showUserMenu && (
                <div className="absolute right-0 mt-1 w-44 bg-slate-800 border border-slate-700/50 rounded-lg shadow-xl py-1 z-50">
                  <button
                    type="button"
                    onClick={() => {
                      setShowUserMenu(false);
                      setShowChangePassword(true);
                    }}
                    className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer"
                  >
                    <KeyRound size={14} className="text-slate-400" />
                    Change Password
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setShowUserMenu(false);
                      logout();
                    }}
                    className="w-full flex items-center gap-2 px-3 py-2 text-xs text-rose-400 hover:bg-rose-500/10 transition-colors cursor-pointer"
                  >
                    <LogOut size={14} />
                    Logout
                  </button>
                </div>
              )}
            </div>
            <button
              type="button"
              onClick={() => navigate('/manage/nodes')}
              className="p-2 rounded-lg text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors"
              title="Agents"
            >
              <Server size={16} />
            </button>
            {user.is_admin && (
              <button
                type="button"
                onClick={() => setShowGlobalSettings(true)}
                className="p-2 rounded-lg text-slate-400 hover:text-violet-400 hover:bg-violet-500/10 transition-colors"
                title="Global Settings"
              >
                <Settings size={16} />
              </button>
            )}
            <button
              type="button"
              onClick={() => setShowDeploy(true)}
              className="inline-flex items-center gap-1.5 px-3.5 py-2 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
            >
              <Plus size={14} />
              Deploy
            </button>
          </div>
        </div>
      </header>

      {/* Stack services RAM / disk bar */}
      {systemStats.length > 0 &&
        (() => {
          const totalRam = systemStats.reduce((sum, s) => sum + s.memory_usage, 0);
          const totalDisk = systemStats.reduce((sum, s) => sum + s.disk_usage, 0);
          return (
            <div className="border-b border-slate-800/60 bg-slate-900/30">
              {/* Desktop: single row */}
              <div className="hidden md:flex max-w-6xl mx-auto px-6 py-2 items-center gap-5 text-[11px]">
                <div className="flex items-center gap-1.5">
                  <span className="text-slate-400 font-medium">total</span>
                  <MemoryStick size={11} className="text-slate-500" />
                  <span className="text-slate-200 font-medium">{formatBytes(totalRam)}</span>
                  <HardDrive size={11} className="text-slate-500" />
                  <span className="text-slate-400">{formatBytes(totalDisk)}</span>
                </div>
                <span className="text-slate-700">|</span>
                {systemStats.map((s) => (
                  <div key={s.name} className="flex items-center gap-1.5">
                    <span className="text-slate-500">{s.name}</span>
                    <MemoryStick size={11} className="text-slate-600" />
                    <span className="text-slate-300">{formatBytes(s.memory_usage)}</span>
                    <HardDrive size={11} className="text-slate-600" />
                    <span className="text-slate-500">{formatBytes(s.disk_usage)}</span>
                  </div>
                ))}
              </div>
              {/* Mobile: total row + expandable services */}
              <div className="md:hidden px-4 py-2 text-[11px]">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-1.5">
                    <span className="text-slate-400 font-medium">total</span>
                    <MemoryStick size={11} className="text-slate-500" />
                    <span className="text-slate-200 font-medium">{formatBytes(totalRam)}</span>
                    <HardDrive size={11} className="text-slate-500" />
                    <span className="text-slate-400">{formatBytes(totalDisk)}</span>
                  </div>
                  <button
                    type="button"
                    onClick={() => setStackExpanded(!stackExpanded)}
                    className="p-1 text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
                  >
                    {stackExpanded ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
                  </button>
                </div>
                {stackExpanded && (
                  <div className="flex flex-col gap-1.5 mt-1.5 pt-1.5 border-t border-slate-800/60">
                    {systemStats.map((s) => (
                      <div key={s.name} className="flex items-center gap-1.5">
                        <span className="text-slate-500">{s.name}</span>
                        <MemoryStick size={11} className="text-slate-600" />
                        <span className="text-slate-300">{formatBytes(s.memory_usage)}</span>
                        <HardDrive size={11} className="text-slate-600" />
                        <span className="text-slate-500">{formatBytes(s.disk_usage)}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          );
        })()}

      <main className="max-w-6xl mx-auto px-6 py-6">
        {/* Stats bar */}
        <div className="flex items-center gap-4 mb-6 flex-wrap">
          <div className="flex items-center gap-4 text-xs text-slate-500">
            <span>
              <span className="text-slate-300 font-medium">{projects.length}</span> total
            </span>
            <button
              type="button"
              onClick={() => setStatusFilter(statusFilter === ProjectStatus.Running ? null : ProjectStatus.Running)}
              className={`hover:underline cursor-pointer ${statusFilter === ProjectStatus.Running ? 'underline' : ''}`}
            >
              <span className="text-emerald-400 font-medium">{running}</span> running
            </button>
            <button
              type="button"
              onClick={() => setStatusFilter(statusFilter === ProjectStatus.Stopped ? null : ProjectStatus.Stopped)}
              className={`hover:underline cursor-pointer ${statusFilter === ProjectStatus.Stopped ? 'underline' : ''}`}
            >
              <span className="text-slate-400 font-medium">{stopped}</span> stopped
            </button>
            {stopping > 0 && (
              <button
                type="button"
                onClick={() => setStatusFilter(statusFilter === ProjectStatus.Stopping ? null : ProjectStatus.Stopping)}
                className={`hover:underline cursor-pointer ${statusFilter === ProjectStatus.Stopping ? 'underline' : ''}`}
              >
                <span className="text-orange-400 font-medium">{stopping}</span> stopping
              </button>
            )}
          </div>
          {statusFilter && (
            <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-xs font-medium bg-violet-500/15 text-violet-300 border border-violet-500/25">
              {statusFilter}
              <button
                type="button"
                onClick={() => setStatusFilter(null)}
                className="hover:text-violet-100 cursor-pointer"
              >
                <X size={12} />
              </button>
            </span>
          )}
          <button
            type="button"
            onClick={loadProjectsAndStats}
            className="ml-auto p-1.5 rounded-md text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
            title="Refresh"
          >
            <RefreshCw size={14} />
          </button>
        </div>

        {/* Project grid */}
        {loading ? (
          <div className="flex items-center justify-center py-20">
            <div className="w-6 h-6 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
          </div>
        ) : filteredProjects.length === 0 ? (
          <div className="text-center py-20">
            <div className="w-12 h-12 rounded-xl bg-slate-800/50 border border-slate-700/50 flex items-center justify-center mx-auto mb-4">
              <Container size={20} className="text-slate-600" />
            </div>
            <p className="text-sm text-slate-500 mb-4">
              {statusFilter ? `No ${statusFilter} projects` : 'No projects yet'}
            </p>
            {!statusFilter && (
              <div className="flex flex-col sm:flex-row items-center gap-3 justify-center">
                <button
                  type="button"
                  onClick={() => setShowDeploy(true)}
                  className="inline-flex items-center gap-1.5 px-4 py-2 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
                >
                  <Plus size={14} />
                  Deploy your first app
                </button>
                <button
                  type="button"
                  onClick={() => navigate('/manage/import')}
                  className="inline-flex items-center gap-1.5 px-4 py-2 rounded-lg text-xs font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 border border-slate-700/60 transition-colors cursor-pointer"
                >
                  <Search size={14} />
                  Scan &amp; import existing
                </button>
              </div>
            )}
          </div>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
            {filteredProjects.map((project) => (
              <ProjectCard
                key={project.id}
                project={project}
                stats={stats.find((s) => s.project_id === project.id) ?? null}
                nodes={nodes}
                onRefresh={loadProjectsAndStats}
                projectsDir={projectsDir}
                domain={domain}
                dnsTarget={dnsTarget}
              />
            ))}
          </div>
        )}
      </main>

      {/* Deploy modal */}
      {showDeploy && (
        <DeployForm onDeploy={loadProjectsAndStats} onClose={() => setShowDeploy(false)} domain={domain} />
      )}

      {/* Change Password modal */}
      {showChangePassword && <ChangePasswordModal onClose={() => setShowChangePassword(false)} />}

      {/* Global Settings modal */}
      {showGlobalSettings && <GlobalSettingsModal onClose={() => setShowGlobalSettings(false)} />}
    </div>
  );
}

function AppContent() {
  const navigate = useNavigate();
  const [projects, setProjects] = useState<Project[]>([]);
  const [stats, setStats] = useState<ProjectStats[]>([]);
  const [systemStats, setSystemStats] = useState<ServiceStats[]>([]);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [projectsDir, setProjectsDir] = useState('projects');
  const [domain, setDomain] = useState('localhost');
  const [dnsTarget, setDnsTarget] = useState('');
  const [showDeploy, setShowDeploy] = useState(false);
  const [showGlobalSettings, setShowGlobalSettings] = useState(false);
  const [showChangePassword, setShowChangePassword] = useState(false);
  const [showUserMenu, setShowUserMenu] = useState(false);
  const [loading, setLoading] = useState(true);
  const [stackExpanded, setStackExpanded] = useState(false);
  const [statusFilter, setStatusFilter] = useState<ProjectStatus | null>(null);
  const { user, loading: authLoading, logout } = useAuth();
  const userMenuRefMobile = useRef<HTMLDivElement>(null);
  const userMenuRefDesktop = useRef<HTMLDivElement>(null);

  // Fetch nodes and settings once on mount
  useEffect(() => {
    if (!user) return;
    (async () => {
      try {
        const [nodeData, settings] = await Promise.all([fetchNodes(), fetchGlobalSettings()]);
        setNodes(nodeData);
        setProjectsDir(settings.projects_dir);
        setDomain(settings.domain);
        setDnsTarget(settings.dns_target);
      } catch (e) {
        console.error('Failed to fetch nodes/settings:', e);
      }
    })();
  }, [user]);

  const loadProjectsAndStats = useCallback(async () => {
    if (!user) return;
    try {
      const [data, statsData, sysData] = await Promise.all([fetchProjects(), fetchAllStats(), fetchSystemStats()]);
      setProjects(data);
      setStats(statsData);
      setSystemStats(sysData);
    } catch (e) {
      console.error('Failed to fetch projects/stats:', e);
    } finally {
      setLoading(false);
    }
  }, [user]);

  useIntervalWhileVisible(() => {
    if (user) loadProjectsAndStats();
  }, 5000);

  // Fetch immediately once user is available (auth may resolve after mount)
  useEffect(() => {
    if (user) loadProjectsAndStats();
  }, [loadProjectsAndStats, user]);

  // Close user menu on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      const mobileEl = userMenuRefMobile.current;
      const desktopEl = userMenuRefDesktop.current;
      const target = e.target as unknown as globalThis.Node;
      if (mobileEl?.contains(target)) return;
      if (desktopEl?.contains(target)) return;
      setShowUserMenu(false);
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, []);

  // Show auth screens if not logged in
  if (authLoading) {
    return (
      <div className="min-h-screen bg-slate-950 flex items-center justify-center">
        <div className="w-6 h-6 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
      </div>
    );
  }

  if (!user) {
    return <LoginScreen />;
  }

  const homeProps = {
    projects,
    stats,
    systemStats,
    nodes,
    projectsDir,
    domain,
    dnsTarget,
    loading,
    statusFilter,
    setStatusFilter,
    loadProjectsAndStats,
    setShowDeploy,
    setShowGlobalSettings,
    setShowChangePassword,
    showDeploy,
    showGlobalSettings,
    showChangePassword,
    showUserMenu,
    setShowUserMenu,
    userMenuRefMobile,
    userMenuRefDesktop,
    user,
    logout,
    stackExpanded,
    setStackExpanded,
  };

  return (
    <>
      <Routes>
        <Route path="/manage" element={<Navigate to="/" replace />} />
        <Route
          path="/manage/nodes"
          element={
            <NodesPage
              onBack={() => navigate('/')}
              projects={projects}
              onScanNode={(nodeId) => navigate(nodeId ? `/manage/import?node=${nodeId}` : '/manage/import')}
            />
          }
        />
        <Route
          path="/manage/import"
          element={
            <ScanImportPage
              onBack={() => navigate('/')}
              onDone={() => {
                navigate('/');
                loadProjectsAndStats();
              }}
              nodes={nodes}
            />
          }
        />
        <Route path="/" element={<HomePage {...homeProps} />} />
      </Routes>
      <Footer />
    </>
  );
}

function App() {
  return (
    <AuthProvider>
      <ToastProvider>
        <AppContent />
      </ToastProvider>
    </AuthProvider>
  );
}

export default App;
