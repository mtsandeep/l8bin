import { AlertTriangle, Globe, Network, SlidersHorizontal } from 'lucide-react';
import { useEffect, useState } from 'react';
import {
  cleanupDnsRecords,
  type DomainJob,
  type DomainPreflightResult,
  domainApply,
  domainPreflight,
  fetchDomainJob,
  fetchGlobalSettings,
  type GlobalSettings,
  RoutingMode,
  retryDomainJob,
  syncDnsRecords,
  updateGlobalSettings,
} from '../../api';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

export default function GeneralPanel() {
  const [settings, setSettings] = useState<GlobalSettings | null>(null);
  const [memMb, setMemMb] = useState(256);
  const [cpu, setCpu] = useState(0.5);
  const [domain, setDomain] = useState('');
  const [dnsTarget, setDnsTarget] = useState('');
  const [routingMode, setRoutingMode] = useState<RoutingMode>(RoutingMode.MasterProxy);
  const [cfToken, setCfToken] = useState('');
  const [cfZoneId, setCfZoneId] = useState('');
  const [dashboardSubdomain, setDashboardSubdomain] = useState('l8bin');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const hasUnsavedChanges =
    settings &&
    (cfToken !== (settings.cloudflare_api_token || '') ||
      cfZoneId !== (settings.cloudflare_zone_id || '') ||
      routingMode !== (settings.routing_mode || RoutingMode.MasterProxy) ||
      dnsTarget !== settings.dns_target ||
      memMb !== settings.default_memory_limit_mb ||
      cpu !== settings.default_cpu_limit ||
      dashboardSubdomain !== (settings.dashboard_subdomain || 'l8bin'));
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<{
    created: number;
    deleted: number;
    unchanged: number;
    errors: number;
  } | null>(null);
  const [removing, setRemoving] = useState(false);
  const [showRemoveConfirm, setShowRemoveConfirm] = useState(false);
  const [showDomainConfirm, setShowDomainConfirm] = useState(false);
  const [domainDraft, setDomainDraft] = useState('');
  const [domainConfirmTyped, setDomainConfirmTyped] = useState('');
  const [domainAck, setDomainAck] = useState(false);
  const [domainPreflightResult, setDomainPreflightResult] = useState<DomainPreflightResult | null>(null);
  const [domainJob, setDomainJob] = useState<DomainJob | null>(null);
  const [domainBusy, setDomainBusy] = useState(false);
  const { showToast } = useToast();

  useEffect(() => {
    fetchGlobalSettings()
      .then((s) => {
        setSettings(s);
        setMemMb(s.default_memory_limit_mb);
        setCpu(s.default_cpu_limit);
        setDomain(s.domain);
        setDnsTarget(s.dns_target);
        setRoutingMode(s.routing_mode || RoutingMode.MasterProxy);
        setCfToken(s.cloudflare_api_token || '');
        setCfZoneId(s.cloudflare_zone_id || '');
        setDashboardSubdomain(s.dashboard_subdomain || 'l8bin');
      })
      .catch(() => setError('Failed to load settings'));
  }, []);

  useEffect(() => {
    if (!domainJob || domainJob.status === 'completed' || domainJob.status === 'failed') return;
    const timer = setInterval(() => {
      fetchDomainJob(domainJob.id)
        .then((job) => {
          setDomainJob(job);
          if (job.status === 'completed') {
            setDomain(job.domain);
            setSettings((prev) => (prev ? { ...prev, domain: job.domain, tryout: undefined } : prev));
            setShowDomainConfirm(false);
            showToast('Platform domain updated', 'success');
          }
        })
        .catch(() => {});
    }, 800);
    return () => clearInterval(timer);
  }, [domainJob, showToast]);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      const updated = await updateGlobalSettings({
        default_memory_limit_mb: memMb,
        default_cpu_limit: cpu,
        dns_target: dnsTarget.trim(),
        routing_mode: routingMode,
        cloudflare_api_token: routingMode === RoutingMode.CloudflareDns ? cfToken : '',
        cloudflare_zone_id: routingMode === RoutingMode.CloudflareDns ? cfZoneId : '',
        dashboard_subdomain: dashboardSubdomain.trim() || undefined,
      });
      setSettings(updated);
      setDomain(updated.domain);
      setDashboardSubdomain(updated.dashboard_subdomain || 'l8bin');
      setSaved(true);
      showToast('Settings saved', 'success');
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to save';
      setError(msg);
      showToast(msg);
    } finally {
      setSaving(false);
    }
  };

  const openDomainChange = () => {
    setDomainDraft(domain);
    setDomainConfirmTyped('');
    setDomainAck(false);
    setDomainPreflightResult(null);
    setDomainJob(null);
    setShowDomainConfirm(true);
  };

  const runDomainPreflight = async () => {
    setDomainBusy(true);
    setError(null);
    try {
      const result = await domainPreflight(domainDraft.trim());
      setDomainPreflightResult(result);
      if (!result.ok) {
        showToast(result.errors[0] || 'Domain preflight failed');
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Domain preflight failed';
      setError(msg);
      showToast(msg);
    } finally {
      setDomainBusy(false);
    }
  };

  const applyDomainChange = async () => {
    setDomainBusy(true);
    setError(null);
    try {
      const needsAck = (domainPreflightResult?.warnings.length ?? 0) > 0;
      const { job_id } = await domainApply(domainDraft.trim(), needsAck ? domainAck : true);
      const job = await fetchDomainJob(job_id);
      setDomainJob(job);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to apply domain';
      setError(msg);
      showToast(msg);
    } finally {
      setDomainBusy(false);
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

  const domainDirty = domainDraft.trim().toLowerCase() !== settings.domain.toLowerCase();
  const canContinueDomain =
    domainDirty &&
    domainConfirmTyped.trim().toLowerCase() === domainDraft.trim().toLowerCase() &&
    (!domainPreflightResult || domainPreflightResult.ok) &&
    (!domainPreflightResult?.warnings.length || domainAck);

  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
      )}

      {settings.tryout && (
        <div className="text-xs text-amber-300 bg-amber-500/10 border border-amber-500/20 rounded px-3 py-2">
          Tryout DNS ({settings.domain}) — fine for exploring; Let&apos;s Encrypt may fail. Switch to a real domain when
          ready.
        </div>
      )}

      {/* Platform */}
      <SettingsCard
        icon={Globe}
        title="Platform"
        description="The domain every project URL lives under, and where the dashboard is served."
      >
        <div className="space-y-4">
          <div>
            <label htmlFor="global-domain" className="block text-xs text-slate-400 mb-1">
              Platform domain
            </label>
            <div className="flex gap-2">
              <input
                id="global-domain"
                type="text"
                value={domain}
                readOnly
                className="flex-1 bg-slate-900/60 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-300 font-mono"
              />
              <button
                type="button"
                onClick={openDomainChange}
                className="shrink-0 px-3 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-100 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Change
              </button>
            </div>
            <p className="text-xs text-slate-600 mt-1">
              Projects get subdomains like{' '}
              <span className="text-slate-400 font-mono">
                {'{id}'}.{domain || 'example.com'}
              </span>
              . Changing domain rebuilds routes and requires re-login on the new host.
            </p>
          </div>

          <div>
            <label htmlFor="global-dashboard-subdomain" className="block text-xs text-slate-400 mb-1">
              Dashboard subdomain
            </label>
            <input
              id="global-dashboard-subdomain"
              type="text"
              value={dashboardSubdomain}
              onChange={(e) => setDashboardSubdomain(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ''))}
              placeholder="l8bin"
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
            <p className="text-xs text-slate-600 mt-1">
              Dashboard served at{' '}
              <span className="text-slate-400 font-mono">
                {dashboardSubdomain || 'l8bin'}.{domain || 'example.com'}
              </span>
            </p>
          </div>
        </div>
      </SettingsCard>

      {/* Routing & DNS */}
      <SettingsCard
        icon={Network}
        title="Routing & DNS"
        description="How traffic reaches your apps — through this server, or directly to each node via Cloudflare."
      >
        <div className="space-y-4">
          <div>
            <p className="text-xs text-slate-400 mb-1.5">Routing mode</p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setRoutingMode(RoutingMode.MasterProxy)}
                className={`flex-1 px-3 py-2 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                  routingMode === RoutingMode.MasterProxy
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
                  routingMode === RoutingMode.CloudflareDns
                    ? 'bg-cyan-600 text-white'
                    : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                }`}
              >
                Cloudflare
              </button>
            </div>
            <p className="text-xs text-slate-600 mt-1">
              {routingMode === RoutingMode.MasterProxy
                ? 'All traffic routes through this server via Caddy reverse proxy.'
                : 'DNS records point directly to each node. Requires Cloudflare API token.'}
            </p>
          </div>

          <div>
            <label htmlFor="global-dns-target" className="block text-xs text-slate-400 mb-1">
              DNS target (server IP or hostname)
            </label>
            <input
              id="global-dns-target"
              type="text"
              value={dnsTarget}
              onChange={(e) => setDnsTarget(e.target.value)}
              placeholder="203.0.113.5"
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
            <p className="text-xs text-slate-600 mt-1">
              Shown in custom domain setup instructions. For apex domains, users need an A record pointing here.
            </p>
          </div>

          {routingMode === RoutingMode.CloudflareDns && (
            <div className="space-y-3 border-t border-slate-700/40 pt-4">
              <div>
                <label htmlFor="global-cf-token" className="block text-xs text-slate-400 mb-1">
                  Cloudflare API token
                </label>
                <input
                  id="global-cf-token"
                  type="password"
                  value={cfToken}
                  onChange={(e) => setCfToken(e.target.value)}
                  placeholder="Enter Cloudflare API token"
                  className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                />
                <p className="text-xs text-slate-600 mt-1">Needs Zone:DNS:Edit and Zone:Zone:Read permissions.</p>
              </div>
              <div>
                <label htmlFor="global-cf-zone-id" className="block text-xs text-slate-400 mb-1">
                  Cloudflare Zone ID
                </label>
                <input
                  id="global-cf-zone-id"
                  type="text"
                  value={cfZoneId}
                  onChange={(e) => setCfZoneId(e.target.value)}
                  placeholder="Enter Cloudflare Zone ID"
                  className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                />
                <p className="text-xs text-slate-600 mt-1">
                  Found in your Cloudflare dashboard under the domain overview.
                </p>
              </div>
              {cfToken && cfZoneId && (
                <div className="space-y-2">
                  <button
                    type="button"
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
                          showToast(
                            parts.length > 0 ? `DNS synced: ${parts.join(', ')}` : 'DNS already up to date',
                            'success',
                          );
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
                    <p className={`text-xs ${syncResult.errors > 0 ? 'text-amber-400' : 'text-green-400'}`}>
                      {syncResult.created > 0 && `${syncResult.created} added`}
                      {syncResult.created > 0 && syncResult.unchanged > 0 && ', '}
                      {syncResult.unchanged > 0 && `${syncResult.unchanged} already exist`}
                      {syncResult.deleted > 0 &&
                        `${syncResult.created > 0 || syncResult.unchanged > 0 ? ', ' : ''}${syncResult.deleted} removed`}
                      {syncResult.errors > 0 && `, ${syncResult.errors} failed`}
                      {syncResult.created === 0 &&
                        syncResult.unchanged === 0 &&
                        syncResult.deleted === 0 &&
                        syncResult.errors === 0 &&
                        'No records to sync'}
                    </p>
                  )}
                  <p className="text-xs text-slate-600">
                    Recreates DNS records for all projects, dashboard, and poke domains in Cloudflare.
                  </p>

                  <button
                    type="button"
                    onClick={() => setShowRemoveConfirm(true)}
                    disabled={removing}
                    className="w-full py-2 rounded-md text-xs font-medium bg-red-600/80 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                  >
                    Remove Cloudflare &amp; Cleanup DNS
                  </button>
                  <p className="text-xs text-slate-600">
                    Deletes LiteBin-managed A records (records you created yourself are left alone) and clears
                    credentials.
                  </p>
                </div>
              )}
            </div>
          )}
        </div>
      </SettingsCard>

      {/* Deployment defaults */}
      <SettingsCard
        icon={SlidersHorizontal}
        title="Deployment defaults"
        description="Applied to every new deployment. Per-app overrides live in each project's App Settings."
      >
        <div className="space-y-5">
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
              onChange={(e) => setMemMb(Number(e.target.value))}
              className="w-full accent-violet-500"
            />
            <div className="flex justify-between text-xs text-slate-600 mt-1">
              <span>64 MB</span>
              <span>4096 MB</span>
            </div>
          </div>

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
              onChange={(e) => setCpu(Number(e.target.value))}
              className="w-full accent-violet-500"
            />
            <div className="flex justify-between text-xs text-slate-600 mt-1">
              <span>0.1</span>
              <span>4.0</span>
            </div>
          </div>
        </div>
      </SettingsCard>

      {/* Save */}
      <button
        type="button"
        onClick={handleSave}
        disabled={saving || !hasUnsavedChanges}
        className="w-full py-2 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
      >
        {saving ? 'Saving...' : saved ? 'Saved' : 'Save changes'}
      </button>

      {/* Remove Cloudflare confirmation modal */}
      {showRemoveConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-red-400" />
              <h3 className="text-sm font-semibold text-slate-100">Remove Cloudflare &amp; Cleanup DNS</h3>
            </div>
            <p className="text-xs text-slate-300 mb-1">This will:</p>
            <ul className="text-xs text-slate-400 list-disc list-inside mb-4 space-y-1">
              <li>
                Delete LiteBin-managed A records from Cloudflare (records you created yourself, e.g.{' '}
                <span className="font-mono text-slate-300">mail.{domain}</span>, are left alone)
              </li>
              <li>Clear your Cloudflare API token and Zone ID</li>
              <li>Switch routing mode to Local (master proxy)</li>
            </ul>
            <p className="text-xs text-red-400 mb-4">This cannot be undone.</p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setShowRemoveConfirm(false)}
                disabled={removing}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={async () => {
                  setRemoving(true);
                  try {
                    const result = await cleanupDnsRecords();
                    setCfToken('');
                    setCfZoneId('');
                    setRoutingMode(RoutingMode.MasterProxy);
                    await updateGlobalSettings({
                      cloudflare_api_token: '',
                      cloudflare_zone_id: '',
                      routing_mode: RoutingMode.MasterProxy,
                    });
                    setShowRemoveConfirm(false);
                    showToast(`Removed Cloudflare: ${result.deleted_count} DNS record(s) deleted`, 'success');
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

      {/* Domain change modal */}
      {showDomainConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 p-5 shadow-2xl max-h-[85vh] overflow-y-auto">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-amber-400" />
              <h3 className="text-sm font-semibold text-slate-100">Change platform domain</h3>
            </div>

            {domainJob && (domainJob.status === 'running' || domainJob.status === 'pending') ? (
              <div className="space-y-3">
                <p className="text-xs text-slate-300">Applying domain change to {domainJob.domain}…</p>
                <ul className="space-y-2">
                  {domainJob.steps.map((step) => (
                    <li key={step.id} className="flex items-start gap-2 text-xs">
                      <span
                        className={`mt-0.5 w-2 h-2 rounded-full shrink-0 ${
                          step.status === 'done' || step.status === 'skipped'
                            ? 'bg-green-400'
                            : step.status === 'running'
                              ? 'bg-violet-400 animate-pulse'
                              : step.status === 'failed'
                                ? 'bg-red-400'
                                : 'bg-slate-600'
                        }`}
                      />
                      <span className="text-slate-300">
                        {step.label}
                        {step.status === 'skipped' ? ' (skipped)' : ''}
                        {step.error ? <span className="block text-red-400 mt-0.5">{step.error}</span> : null}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            ) : domainJob?.status === 'completed' ? (
              <div className="space-y-3">
                <p className="text-xs text-green-400">Domain updated. Open the new dashboard and sign in again.</p>
                <a
                  href={domainJob.dashboard_url}
                  className="block text-xs text-violet-300 font-mono break-all hover:underline"
                >
                  {domainJob.dashboard_url}
                </a>
                <button
                  type="button"
                  onClick={() => setShowDomainConfirm(false)}
                  className="w-full py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 cursor-pointer"
                >
                  Close
                </button>
              </div>
            ) : domainJob?.status === 'failed' ? (
              <div className="space-y-3">
                <p className="text-xs text-red-400">{domainJob.error || 'Domain change failed'}</p>
                <ul className="space-y-1">
                  {domainJob.steps.map((step) => (
                    <li key={step.id} className="text-xs text-slate-400">
                      {step.label}: {step.status}
                      {step.error ? ` — ${step.error}` : ''}
                    </li>
                  ))}
                </ul>
                <div className="flex gap-2">
                  <button
                    type="button"
                    onClick={() => setShowDomainConfirm(false)}
                    className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 cursor-pointer"
                  >
                    Close
                  </button>
                  <button
                    type="button"
                    onClick={async () => {
                      setDomainBusy(true);
                      try {
                        await retryDomainJob(domainJob.id);
                        const job = await fetchDomainJob(domainJob.id);
                        setDomainJob(job);
                      } catch (e) {
                        showToast(e instanceof Error ? e.message : 'Retry failed');
                      } finally {
                        setDomainBusy(false);
                      }
                    }}
                    disabled={domainBusy}
                    className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                  >
                    Retry
                  </button>
                </div>
              </div>
            ) : (
              <div className="space-y-3">
                <ul className="text-xs text-slate-400 list-disc list-inside space-y-1">
                  <li>
                    Old URLs under <span className="font-mono text-slate-300">*.{settings.domain}</span> will stop
                    working
                  </li>
                  <li>Custom domains on projects are unchanged</li>
                  <li>Update DNS (wildcard or Cloudflare) for the new domain</li>
                  <li>You must reopen the dashboard on the new host and sign in again</li>
                </ul>
                <div>
                  <label htmlFor="domain-draft" className="block text-xs text-slate-300 mb-1.5">
                    New domain
                  </label>
                  <input
                    id="domain-draft"
                    type="text"
                    value={domainDraft}
                    onChange={(e) => {
                      setDomainDraft(e.target.value);
                      setDomainPreflightResult(null);
                      setDomainAck(false);
                    }}
                    placeholder="example.com"
                    className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
                  />
                </div>
                <div>
                  <label htmlFor="domain-confirm-typed" className="block text-xs text-slate-300 mb-1.5">
                    Type the new domain to confirm
                  </label>
                  <input
                    id="domain-confirm-typed"
                    type="text"
                    value={domainConfirmTyped}
                    onChange={(e) => setDomainConfirmTyped(e.target.value)}
                    className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
                  />
                </div>
                {domainPreflightResult && (
                  <div className="space-y-2">
                    {domainPreflightResult.errors.map((msg) => (
                      <p key={msg} className="text-xs text-red-400">
                        {msg}
                      </p>
                    ))}
                    {domainPreflightResult.warnings.map((msg) => (
                      <p key={msg} className="text-xs text-amber-300">
                        {msg}
                      </p>
                    ))}
                    {domainPreflightResult.warnings.length > 0 && domainPreflightResult.ok && (
                      <label className="flex items-start gap-2 text-xs text-slate-300 cursor-pointer">
                        <input
                          type="checkbox"
                          checked={domainAck}
                          onChange={(e) => setDomainAck(e.target.checked)}
                          className="mt-0.5"
                        />
                        I have pointed DNS at this server (or accept that traffic will fail until I do)
                      </label>
                    )}
                  </div>
                )}
                <div className="flex gap-2 pt-1">
                  <button
                    type="button"
                    onClick={() => setShowDomainConfirm(false)}
                    disabled={domainBusy}
                    className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 cursor-pointer"
                  >
                    Cancel
                  </button>
                  {!domainPreflightResult?.ok ? (
                    <button
                      type="button"
                      onClick={runDomainPreflight}
                      disabled={domainBusy || !domainDirty}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                    >
                      {domainBusy ? 'Checking…' : 'Check DNS'}
                    </button>
                  ) : (
                    <button
                      type="button"
                      onClick={applyDomainChange}
                      disabled={domainBusy || !canContinueDomain}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                    >
                      {domainBusy ? 'Starting…' : 'Apply'}
                    </button>
                  )}
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
