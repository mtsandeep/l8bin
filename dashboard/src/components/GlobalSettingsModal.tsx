import { useState, useEffect } from 'react';
import { X, Settings, Key, Trash2, Copy, Check, AlertTriangle } from 'lucide-react';
import {
  fetchGlobalSettings, updateGlobalSettings, fetchProjects,
  createDeployToken, revokeDeployToken, createProject,
  cleanupDnsRecords, syncDnsRecords,
  timeAgo, type GlobalSettings, type Project, type DeployTokenInfo,
} from '../api';
import { useToast } from './ToastContext';

const API_BASE = '';

type Tab = 'general' | 'tokens';

interface Props {
  onClose: () => void;
}

export default function GlobalSettingsModal({ onClose }: Props) {
  const [tab, setTab] = useState<Tab>('general');

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50 shrink-0">
          <div className="flex items-center gap-2">
            <Settings size={14} className="text-violet-400" />
            <h2 className="text-sm font-semibold text-slate-100">Settings</h2>
          </div>
          <button onClick={onClose} className="p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer">
            <X size={16} />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex border-b border-slate-700/50 shrink-0">
          <button
            onClick={() => setTab('general')}
            className={`flex-1 px-4 py-2.5 text-xs font-medium transition-colors cursor-pointer ${
              tab === 'general'
                ? 'text-violet-300 border-b-2 border-violet-500 bg-slate-800'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            General
          </button>
          <button
            onClick={() => setTab('tokens')}
            className={`flex items-center justify-center gap-1.5 flex-1 px-4 py-2.5 text-xs font-medium transition-colors cursor-pointer ${
              tab === 'tokens'
                ? 'text-violet-300 border-b-2 border-violet-500 bg-slate-800'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            <Key size={12} />
            Deploy Tokens
          </button>
        </div>

        <div className="p-5 overflow-y-auto">
          {tab === 'general' ? <GeneralTab /> : <TokensTab />}
        </div>
      </div>
    </div>
  );
}

function GeneralTab() {
  const [settings, setSettings] = useState<GlobalSettings | null>(null);
  const [memMb, setMemMb] = useState(256);
  const [cpu, setCpu] = useState(0.5);
  const [domain, setDomain] = useState('');
  const [dnsTarget, setDnsTarget] = useState('');
  const [routingMode, setRoutingMode] = useState('master_proxy');
  const [cfToken, setCfToken] = useState('');
  const [cfZoneId, setCfZoneId] = useState('');
  const [dashboardSubdomain, setDashboardSubdomain] = useState('l8bin');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const hasUnsavedChanges = settings && (
    cfToken !== (settings.cloudflare_api_token || '') ||
    cfZoneId !== (settings.cloudflare_zone_id || '') ||
    routingMode !== (settings.routing_mode || 'master_proxy') ||
    domain !== settings.domain ||
    dnsTarget !== settings.dns_target ||
    memMb !== settings.default_memory_limit_mb ||
    cpu !== settings.default_cpu_limit ||
    dashboardSubdomain !== (settings.dashboard_subdomain || 'l8bin')
  );
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<{ created: number; deleted: number; unchanged: number; errors: number } | null>(null);
  const [removing, setRemoving] = useState(false);
  const [showRemoveConfirm, setShowRemoveConfirm] = useState(false);
  const { showToast } = useToast();

  useEffect(() => {
    fetchGlobalSettings().then(s => {
      setSettings(s);
      setMemMb(s.default_memory_limit_mb);
      setCpu(s.default_cpu_limit);
      setDomain(s.domain);
      setDnsTarget(s.dns_target);
      setRoutingMode(s.routing_mode || 'master_proxy');
      setCfToken(s.cloudflare_api_token || '');
      setCfZoneId(s.cloudflare_zone_id || '');
      setDashboardSubdomain(s.dashboard_subdomain || 'l8bin');
    }).catch(() => setError('Failed to load settings'));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      await updateGlobalSettings({
        default_memory_limit_mb: memMb,
        default_cpu_limit: cpu,
        domain: domain.trim() || undefined,
        dns_target: dnsTarget.trim() || undefined,
        routing_mode: routingMode,
        cloudflare_api_token: routingMode === 'cloudflare_dns' ? cfToken : '',
        cloudflare_zone_id: routingMode === 'cloudflare_dns' ? cfZoneId : '',
        dashboard_subdomain: dashboardSubdomain.trim() || undefined,
      });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to save';
      setError(msg);
      showToast(msg);
    } finally {
      setSaving(false);
    }
  };

  if (error && !settings) {
    return <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>;
  }

  if (!settings) {
    return (
      <div className="flex justify-center py-6">
        <div className="w-5 h-5 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
      )}

      {/* Memory */}
      <div>
        <div className="flex items-center justify-between mb-2">
          <span className="text-xs text-slate-300">Default memory limit</span>
          <span className="text-xs font-mono text-violet-300">{memMb} MB</span>
        </div>
        <input
          type="range"
          min={64}
          max={4096}
          step={64}
          value={memMb}
          onChange={e => setMemMb(Number(e.target.value))}
          className="w-full accent-violet-500"
        />
        <div className="flex justify-between text-[10px] text-slate-600 mt-1">
          <span>64 MB</span><span>4096 MB</span>
        </div>
      </div>

      {/* CPU */}
      <div>
        <div className="flex items-center justify-between mb-2">
          <span className="text-xs text-slate-300">Default CPU limit</span>
          <span className="text-xs font-mono text-violet-300">{cpu.toFixed(2)} vCPU</span>
        </div>
        <input
          type="range"
          min={0.1}
          max={4}
          step={0.1}
          value={cpu}
          onChange={e => setCpu(Number(e.target.value))}
          className="w-full accent-violet-500"
        />
        <div className="flex justify-between text-[10px] text-slate-600 mt-1">
          <span>0.1</span><span>4.0</span>
        </div>
      </div>

      {/* Domain */}
      <div>
        <label className="block text-xs text-slate-300 mb-1.5">Platform Domain</label>
        <input
          type="text"
          value={domain}
          onChange={e => setDomain(e.target.value)}
          placeholder="l8b.in"
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />
        <p className="text-[10px] text-slate-600 mt-1">Projects get subdomains like <span className="text-slate-400 font-mono">{'{id}'}.{domain || 'example.com'}</span></p>
      </div>

      {/* Dashboard Subdomain */}
      <div>
        <label className="block text-xs text-slate-300 mb-1.5">Dashboard Subdomain</label>
        <input
          type="text"
          value={dashboardSubdomain}
          onChange={e => setDashboardSubdomain(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ''))}
          placeholder="l8bin"
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />
        <p className="text-[10px] text-slate-600 mt-1">Dashboard served at <span className="text-slate-400 font-mono">{dashboardSubdomain || 'l8bin'}.{domain || 'example.com'}</span></p>
      </div>

      {/* DNS Target */}
      <div>
        <label className="block text-xs text-slate-300 mb-1.5">DNS Target (server IP or hostname)</label>
        <input
          type="text"
          value={dnsTarget}
          onChange={e => setDnsTarget(e.target.value)}
          placeholder="203.0.113.5"
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />
        <p className="text-[10px] text-slate-600 mt-1">Shown in custom domain setup instructions. For apex domains, users need an A record pointing here.</p>
      </div>

      {/* Routing Mode */}
      <div>
        <label className="block text-xs text-slate-300 mb-1.5">Routing Mode</label>
        <div className="flex gap-2">
          <button
            type="button"
            onClick={() => setRoutingMode('master_proxy')}
            className={`flex-1 px-3 py-2 rounded-md text-xs font-medium transition-colors cursor-pointer ${
              routingMode === 'master_proxy'
                ? 'bg-cyan-600 text-white'
                : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
            }`}
          >
            Local
          </button>
          <button
            type="button"
            onClick={() => setRoutingMode('cloudflare_dns')}
            className={`flex-1 px-3 py-2 rounded-md text-xs font-medium transition-colors cursor-pointer ${
              routingMode === 'cloudflare_dns'
                ? 'bg-cyan-600 text-white'
                : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
            }`}
          >
            Cloudflare
          </button>
        </div>
        <p className="text-[10px] text-slate-600 mt-1">
          {routingMode === 'master_proxy'
            ? 'All traffic routes through this server via Caddy reverse proxy.'
            : 'DNS records point directly to each node. Requires Cloudflare API token.'}
        </p>
      </div>

      {/* Cloudflare Config (conditional) */}
      {routingMode === 'cloudflare_dns' && (
        <div className="space-y-3">
          <div>
            <label className="block text-xs text-slate-300 mb-1.5">Cloudflare API Token</label>
            <input
              type="password"
              value={cfToken}
              onChange={e => setCfToken(e.target.value)}
              placeholder="Enter Cloudflare API token"
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
            <p className="text-[10px] text-slate-600 mt-1">Needs Zone:DNS:Edit and Zone:Zone:Read permissions.</p>
          </div>
          <div>
            <label className="block text-xs text-slate-300 mb-1.5">Cloudflare Zone ID</label>
            <input
              type="text"
              value={cfZoneId}
              onChange={e => setCfZoneId(e.target.value)}
              placeholder="Enter Cloudflare Zone ID"
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
            <p className="text-[10px] text-slate-600 mt-1">Found in your Cloudflare dashboard under the domain overview.</p>
          </div>
          {cfToken && cfZoneId && (
          <div className="pt-2 space-y-2">
            {/* Sync DNS Records */}
            <button
              onClick={async () => {
                if (hasUnsavedChanges) {
                  showToast('Save your changes before syncing DNS');
                  return;
                }
                setSyncing(true);
                setSyncResult(null);
                try {
                  const result = await syncDnsRecords();
                  setSyncResult(result);
                  if (result.errors === 0) {
                    const parts = [];
                    if (result.created > 0) parts.push(`${result.created} added`);
                    if (result.unchanged > 0) parts.push(`${result.unchanged} already exist`);
                    if (result.deleted > 0) parts.push(`${result.deleted} removed`);
                    showToast(parts.length > 0 ? `DNS synced: ${parts.join(', ')}` : 'DNS already up to date');
                  } else {
                    showToast(`DNS sync: ${result.created} added, ${result.errors} failed`);
                  }
                } catch (e) {
                  const msg = e instanceof Error ? e.message : 'Sync failed';
                  setError(msg);
                  showToast(msg);
                } finally {
                  setSyncing(false);
                }
              }}
              disabled={syncing}
              className="w-full py-2 rounded-md text-xs font-medium bg-cyan-600/80 text-white hover:bg-cyan-500 transition-colors disabled:opacity-50 cursor-pointer"
            >
              {syncing ? 'Syncing...' : 'Sync DNS Records'}
            </button>
            {syncResult !== null && (
              <p className={`text-[10px] ${syncResult.errors > 0 ? 'text-amber-400' : 'text-green-400'}`}>
                {syncResult.created > 0 && `${syncResult.created} added`}
                {syncResult.created > 0 && syncResult.unchanged > 0 && ', '}
                {syncResult.unchanged > 0 && `${syncResult.unchanged} already exist`}
                {syncResult.deleted > 0 && `${syncResult.created > 0 || syncResult.unchanged > 0 ? ', ' : ''}${syncResult.deleted} removed`}
                {syncResult.errors > 0 && `, ${syncResult.errors} failed`}
                {(syncResult.created === 0 && syncResult.unchanged === 0 && syncResult.deleted === 0 && syncResult.errors === 0) && 'No records to sync'}
              </p>
            )}
            <p className="text-[10px] text-slate-600">
              Recreates DNS records for all projects, dashboard, and poke domains in Cloudflare.
            </p>

            {/* Remove Cloudflare & Cleanup */}
            <button
              onClick={() => setShowRemoveConfirm(true)}
              disabled={removing}
              className="w-full py-2 rounded-md text-xs font-medium bg-red-600/80 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
            >
              Remove Cloudflare &amp; Cleanup DNS
            </button>
            <p className="text-[10px] text-slate-600">
              Deletes all A records matching <span className="text-slate-400 font-mono">*.{domain}</span> and clears credentials.
            </p>
          </div>
          )}

          {/* Remove confirmation modal */}
          {showRemoveConfirm && (
            <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
              <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
                <div className="flex items-center gap-2 mb-3">
                  <AlertTriangle size={16} className="text-red-400" />
                  <h3 className="text-sm font-semibold text-slate-100">Remove Cloudflare &amp; Cleanup DNS</h3>
                </div>
                <p className="text-xs text-slate-300 mb-1">This will:</p>
                <ul className="text-xs text-slate-400 list-disc list-inside mb-4 space-y-1">
                  <li>Delete all A records matching <span className="font-mono text-slate-300">*.{domain}</span> from Cloudflare</li>
                  <li>Clear your Cloudflare API token and Zone ID</li>
                  <li>Switch routing mode to Local (master proxy)</li>
                </ul>
                <p className="text-xs text-red-400 mb-4">This cannot be undone.</p>
                <div className="flex gap-2">
                  <button
                    onClick={() => setShowRemoveConfirm(false)}
                    disabled={removing}
                    className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
                  >
                    Cancel
                  </button>
                  <button
                    onClick={async () => {
                      setRemoving(true);
                      try {
                        const result = await cleanupDnsRecords();
                        setCfToken('');
                        setCfZoneId('');
                        setRoutingMode('master_proxy');
                        await updateGlobalSettings({
                          cloudflare_api_token: '',
                          cloudflare_zone_id: '',
                          routing_mode: 'master_proxy',
                        });
                        setShowRemoveConfirm(false);
                        showToast(`Removed Cloudflare: ${result.deleted_count} DNS record(s) deleted`);
                      } catch (e) {
                        const msg = e instanceof Error ? e.message : 'Failed to remove Cloudflare';
                        setError(msg);
                        showToast(msg);
                      } finally {
                        setRemoving(false);
                      }
                    }}
                    disabled={removing}
                    className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                  >
                    {removing ? 'Removing...' : 'Confirm Remove'}
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      )}

      <p className="text-[11px] text-slate-500">
        These are defaults for new deployments. Per-app overrides can be set in each project&apos;s App Settings.
      </p>

      <button
        onClick={handleSave}
        disabled={saving}
        className="w-full py-2 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
      >
        {saving ? 'Saving...' : saved ? 'Saved' : 'Save'}
      </button>
    </div>
  );
}

function TokensTab() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [tokens, setTokens] = useState<DeployTokenInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revokingId, setRevokingId] = useState<string | null>(null);

  // Create token form state
  const [tokenScope, setTokenScope] = useState<'global' | 'project'>('global');
  const [projectSource, setProjectSource] = useState<'existing' | 'new'>('existing');
  const [selectedProject, setSelectedProject] = useState('');
  const [newProjectId, setNewProjectId] = useState('');
  const [newTokenName, setNewTokenName] = useState('');
  const [creating, setCreating] = useState(false);
  const [createdToken, setCreatedToken] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const { showToast } = useToast();

  const loadTokens = () => {
    setLoading(true);
    fetch(`${API_BASE}/deploy-tokens`, { credentials: 'include' })
      .then(res => res.ok ? res.json() : Promise.reject())
      .then(setTokens)
      .catch(() => setError('Failed to load tokens'))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchProjects().then(ps => {
      setProjects(ps);
      if (ps.length > 0) setSelectedProject(ps[0].id);
    }).catch(() => setError('Failed to load projects'));
    loadTokens();
  }, []);

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
          setProjects(prev => [p, ...prev]);
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
      const resp = await createDeployToken(projectId, newTokenName || undefined);
      setCreatedToken(resp.token);
      setNewTokenName('');
      setTokens(prev => [resp.token_info, ...prev]);
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
      setTokens(prev => prev.filter(t => t.id !== tokenId));
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

      {/* Token list */}
      <div>
        <label className="block text-xs text-slate-400 mb-1.5">Active Tokens</label>
        {loading ? (
          <div className="flex justify-center py-4">
            <div className="w-4 h-4 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
          </div>
        ) : tokens.length === 0 ? (
          <p className="text-xs text-slate-500 py-3 text-center">No deploy tokens yet</p>
        ) : (
          <div className="space-y-1.5">
            {tokens.map(token => (
              <div key={token.id} className="flex items-center justify-between bg-slate-900/50 border border-slate-700/30 rounded-md px-3 py-2">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="text-xs text-slate-200 truncate">{token.name || 'Unnamed'}</span>
                    <span className={`shrink-0 px-1.5 py-0.5 rounded text-[9px] font-medium ${
                      token.project_id
                        ? 'bg-slate-700/50 text-slate-400'
                        : 'bg-violet-500/15 text-violet-400'
                    }`}>
                      {token.project_id ? token.project_id : 'Global'}
                    </span>
                  </div>
                  <div className="text-[10px] text-slate-500">
                    Created {timeAgo(token.created_at)}
                    {token.last_used_at && ` · Used ${timeAgo(token.last_used_at)}`}
                  </div>
                </div>
                <button
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
      </div>

      {/* Create token */}
      <div className="border-t border-slate-700/50 pt-4 space-y-3">
        <label className="block text-xs text-slate-400">Create new token</label>

        {/* Name */}
        <input
          type="text"
          placeholder="Name (optional)"
          value={newTokenName}
          onChange={e => setNewTokenName(e.target.value)}
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />

        {/* Scope selector */}
        <div className="flex gap-2">
          <button
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

        {/* Project scope options */}
        {tokenScope === 'project' && (
          <div className="space-y-3">
            {/* Radio: existing vs new */}
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
                onChange={e => setSelectedProject(e.target.value)}
                className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
              >
                <option value="" disabled>Select project...</option>
                {projects.map(p => (
                  <option key={p.id} value={p.id}>{p.id}</option>
                ))}
              </select>
            ) : (
              <input
                type="text"
                placeholder="Project name (a-z, 0-9, -, _)"
                value={newProjectId}
                onChange={e => setNewProjectId(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ''))}
                className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
              />
            )}
          </div>
        )}

        <button
          onClick={handleCreate}
          disabled={creating}
          className="w-full py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
        >
          {creating ? 'Creating...' : 'Create Token'}
        </button>
      </div>

      {/* Show created token */}
      {createdToken && (
        <div className="bg-amber-500/10 border border-amber-500/20 rounded-md p-3 space-y-2">
          <div className="flex items-center gap-1.5 text-xs text-amber-300">
            <AlertTriangle size={12} />
            <span className="font-medium">Copy this token now — it won&apos;t be shown again</span>
          </div>
          <div className="flex items-center gap-2">
            <code className="flex-1 bg-slate-900 rounded px-2.5 py-1.5 text-[11px] font-mono text-slate-300 break-all select-all">
              {createdToken}
            </code>
            <button
              onClick={handleCopy}
              className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
              title={copied ? 'Copied!' : 'Copy to clipboard'}
            >
              {copied ? <Check size={14} className="text-green-400" /> : <Copy size={14} />}
            </button>
          </div>
          <p className="text-[10px] text-slate-500">Use with: <code className="text-slate-400">L8B_TOKEN=&lt;token&gt;</code></p>
        </div>
      )}
    </div>
  );
}
