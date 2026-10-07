import { AlertTriangle, Check, Copy, Key, Plus, Trash2 } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  createDeployToken,
  createProject,
  type DeployTokenInfo,
  fetchProjects,
  type Project,
  revokeDeployToken,
  type TokenScopeId,
  timeAgo,
} from '../../api';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

const API_BASE = '';

function scopeBadgeClass(scope: TokenScopeId | undefined): string {
  switch (scope) {
    case 'admin':
      return 'bg-red-500/15 text-red-400';
    case 'manage':
      return 'bg-amber-500/15 text-amber-400';
    case 'deploy':
      return 'bg-violet-500/15 text-violet-400';
    default:
      return 'bg-sky-500/15 text-sky-400';
  }
}

function scopeHint(level: TokenScopeId): string {
  switch (level) {
    case 'read':
      return 'View projects, status, and logs only';
    case 'deploy':
      return 'Read + build and deploy code (CI default)';
    case 'manage':
      return 'Deploy + restart, settings, env';
    case 'admin':
      return 'Everything, including delete and nodes';
  }
}

export default function TokensPanel() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [tokens, setTokens] = useState<DeployTokenInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revokingId, setRevokingId] = useState<string | null>(null);

  // Create token form state
  const [tokenScope, setTokenScope] = useState<'global' | 'project'>('global');
  const [tokenLevel, setTokenLevel] = useState<TokenScopeId>('deploy');
  const [projectSource, setProjectSource] = useState<'existing' | 'new'>('existing');
  const [selectedProject, setSelectedProject] = useState('');
  const [newProjectId, setNewProjectId] = useState('');
  const [newTokenName, setNewTokenName] = useState('');
  const [creating, setCreating] = useState(false);
  const [createdToken, setCreatedToken] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const { showToast } = useToast();

  const loadTokens = useCallback(() => {
    setLoading(true);
    fetch(`${API_BASE}/deploy-tokens`, { credentials: 'include' })
      .then((res) => (res.ok ? res.json() : Promise.reject()))
      .then(setTokens)
      .catch(() => setError('Failed to load tokens'))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    fetchProjects()
      .then((ps) => {
        setProjects(ps);
        if (ps.length > 0) setSelectedProject(ps[0].id);
      })
      .catch(() => setError('Failed to load projects'));
    loadTokens();
  }, [loadTokens]);

  const handleCreate = async () => {
    let projectId: string | null = null;

    if (tokenScope === 'project') {
      if (projectSource === 'new') {
        if (!newProjectId.trim()) {
          setError('Enter a project name');
          return;
        }
        try {
          const p = await createProject(newProjectId.trim());
          projectId = p.id;
          setProjects((prev) => [p, ...prev]);
          setSelectedProject(p.id);
          setNewProjectId('');
        } catch (e) {
          const msg = e instanceof Error ? e.message : 'Failed to create project';
          setError(msg);
          showToast(msg);
          return;
        }
      } else {
        if (!selectedProject) {
          setError('Select a project');
          return;
        }
        projectId = selectedProject;
      }
    }

    setCreating(true);
    setError(null);
    setCreatedToken(null);
    try {
      const resp = await createDeployToken(projectId, newTokenName || undefined, tokenLevel);
      setCreatedToken(resp.token);
      setNewTokenName('');
      setTokens((prev) => [resp.token_info, ...prev]);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to create token';
      setError(msg);
      showToast(msg);
    } finally {
      setCreating(false);
    }
  };

  const handleRevoke = async (tokenId: string) => {
    setRevokingId(tokenId);
    try {
      await revokeDeployToken(tokenId);
      setTokens((prev) => prev.filter((t) => t.id !== tokenId));
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to revoke token';
      setError(msg);
      showToast(msg);
    } finally {
      setRevokingId(null);
    }
  };

  const handleCopy = async () => {
    if (!createdToken) return;
    await navigator.clipboard.writeText(createdToken);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
      )}

      {/* Create token */}
      <SettingsCard
        icon={Plus}
        title="Create token"
        description="Authenticates the l8b CLI, CI/CD pipelines, and the GitHub Action. Shown once — copy it now."
      >
        <div className="space-y-3">
          {/* Name */}
          <div>
            <label htmlFor="new-token-name" className="block text-xs text-slate-400 mb-1">
              Name (optional)
            </label>
            <input
              id="new-token-name"
              type="text"
              placeholder="ci-deploy, laptop-cli, …"
              value={newTokenName}
              onChange={(e) => setNewTokenName(e.target.value)}
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
          </div>

          {/* Access level */}
          <div>
            <p className="text-xs text-slate-400 mb-1.5">Access level</p>
            <div className="grid grid-cols-4 gap-1.5">
              {(['read', 'deploy', 'manage', 'admin'] as const).map((level) => (
                <button
                  key={level}
                  type="button"
                  onClick={() => setTokenLevel(level)}
                  className={`px-1 py-1.5 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                    tokenLevel === level
                      ? 'bg-slate-600 text-white border border-slate-500'
                      : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                  }`}
                >
                  {level}
                </button>
              ))}
            </div>
            <p className="text-xs text-slate-500 mt-1">{scopeHint(tokenLevel)}</p>
          </div>

          {/* Scope selector */}
          <div>
            <p className="text-xs text-slate-400 mb-1.5">Scope</p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setTokenScope('global')}
                className={`flex-1 px-3 py-2 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                  tokenScope === 'global'
                    ? 'bg-cyan-600 text-white'
                    : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                }`}
              >
                Global
              </button>
              <button
                type="button"
                onClick={() => setTokenScope('project')}
                className={`flex-1 px-3 py-2 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                  tokenScope === 'project'
                    ? 'bg-cyan-600 text-white'
                    : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                }`}
              >
                Project-scoped
              </button>
            </div>
          </div>

          {/* Project scope options */}
          {tokenScope === 'project' && (
            <div className="space-y-3 rounded-md border border-slate-700/40 p-3">
              <div className="space-y-2">
                <label className="flex items-center gap-2.5 cursor-pointer">
                  <input
                    type="radio"
                    name="projectSource"
                    checked={projectSource === 'existing'}
                    onChange={() => setProjectSource('existing')}
                    className="accent-violet-500"
                  />
                  <span className="text-xs text-slate-300">Pick existing project</span>
                </label>
                <label className="flex items-center gap-2.5 cursor-pointer">
                  <input
                    type="radio"
                    name="projectSource"
                    checked={projectSource === 'new'}
                    onChange={() => setProjectSource('new')}
                    className="accent-violet-500"
                  />
                  <span className="text-xs text-slate-300">Create new project</span>
                </label>
              </div>

              {projectSource === 'existing' ? (
                <select
                  value={selectedProject}
                  onChange={(e) => setSelectedProject(e.target.value)}
                  className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
                >
                  <option value="" disabled>
                    Select project...
                  </option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.id}
                    </option>
                  ))}
                </select>
              ) : (
                <input
                  type="text"
                  placeholder="Project name (a-z, 0-9, -, _)"
                  value={newProjectId}
                  onChange={(e) => setNewProjectId(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ''))}
                  className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                />
              )}
            </div>
          )}

          <button
            type="button"
            onClick={handleCreate}
            disabled={creating}
            className="w-full py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            {creating ? 'Creating...' : 'Create Token'}
          </button>

          {/* Show created token */}
          {createdToken && (
            <div className="bg-amber-500/10 border border-amber-500/20 rounded-md p-3 space-y-2">
              <div className="flex items-center gap-1.5 text-xs text-amber-300">
                <AlertTriangle size={12} />
                <span className="font-medium">Copy this token now — it won&apos;t be shown again</span>
              </div>
              <div className="flex items-center gap-2">
                <code className="flex-1 bg-slate-900 rounded px-2.5 py-1.5 text-xs font-mono text-slate-300 break-all select-all">
                  {createdToken}
                </code>
                <button
                  type="button"
                  onClick={handleCopy}
                  className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
                  title={copied ? 'Copied!' : 'Copy to clipboard'}
                >
                  {copied ? <Check size={14} className="text-green-400" /> : <Copy size={14} />}
                </button>
              </div>
              <p className="text-xs text-slate-500">
                Use with: <code className="text-slate-400">L8B_TOKEN=&lt;token&gt;</code>
              </p>
            </div>
          )}
        </div>
      </SettingsCard>

      {/* Active tokens */}
      <SettingsCard
        icon={Key}
        title={`Active tokens${tokens.length > 0 ? ` (${tokens.length})` : ''}`}
        description="Revoke immediately if a token leaks — revocation takes effect at once."
      >
        {loading ? (
          <div className="flex justify-center py-4">
            <div className="w-4 h-4 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
          </div>
        ) : tokens.length === 0 ? (
          <p className="text-xs text-slate-500 py-3 text-center">No access tokens yet</p>
        ) : (
          <div className="space-y-1.5">
            {tokens.map((token) => (
              <div
                key={token.id}
                className="flex items-center justify-between bg-slate-900/50 border border-slate-700/30 rounded-md px-3 py-2"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="text-xs text-slate-200 truncate">{token.name || 'Unnamed'}</span>
                    <span
                      className={`shrink-0 px-1.5 py-0.5 rounded text-xs font-medium ${
                        token.project_id ? 'bg-slate-700/50 text-slate-400' : 'bg-violet-500/15 text-violet-400'
                      }`}
                    >
                      {token.project_id ? token.project_id : 'Global'}
                    </span>
                    <span
                      className={`shrink-0 px-1.5 py-0.5 rounded text-xs font-medium ${scopeBadgeClass(token.scope)}`}
                      title={`Access level: ${token.scope}`}
                    >
                      {token.scope}
                    </span>
                  </div>
                  <div className="text-xs text-slate-500">
                    Created {timeAgo(token.created_at)}
                    {token.last_used_at && ` · Used ${timeAgo(token.last_used_at)}`}
                  </div>
                </div>
                <button
                  type="button"
                  onClick={() => handleRevoke(token.id)}
                  disabled={revokingId === token.id}
                  className="ml-3 p-1 rounded text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50"
                  title="Revoke token"
                >
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
          </div>
        )}
      </SettingsCard>
    </div>
  );
}
