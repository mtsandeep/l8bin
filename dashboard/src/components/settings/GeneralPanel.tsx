import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  Copy,
  Globe,
  Loader2,
  Minus,
  Network,
  SlidersHorizontal,
  XCircle,
} from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import {
  cleanupDnsRecords,
  type DomainJob,
  type DomainJobStep,
  type DomainPreflightResult,
  domainApply,
  domainPreflight,
  fetchActiveDomainJob,
  fetchDomainJob,
  fetchGlobalSettings,
  type GlobalSettings,
  RoutingMode,
  retirePreviousDomain,
  retryDomainJob,
  sslipDomainPreview,
  syncDnsRecords,
  updateGlobalSettings,
  validateCloudflare,
} from '../../api';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

function DomainStepList({ steps }: { steps: DomainJobStep[] }) {
  return (
    <ul className="space-y-2">
      {steps.map((step) => (
        <li key={step.id} className="flex items-start gap-2 text-xs">
          <span className="mt-0.5 shrink-0">
            {step.status === 'running' ? (
              <Loader2 size={12} className="animate-spin text-violet-400" />
            ) : step.status === 'done' ? (
              <CheckCircle2 size={12} className="text-green-400" />
            ) : step.status === 'failed' ? (
              <XCircle size={12} className="text-red-400" />
            ) : step.status === 'skipped' ? (
              <Minus size={12} className="text-slate-600" />
            ) : (
              <Circle size={12} className="text-slate-600" />
            )}
          </span>
          <span className={step.status === 'pending' ? 'text-slate-500' : 'text-slate-300'}>
            {step.label}
            {step.status === 'skipped' ? <span className="text-slate-600"> (skipped)</span> : null}
          </span>
        </li>
      ))}
    </ul>
  );
}

const CHECK_LABELS: Record<string, string> = {
  public_ip: 'server IP',
  dns: 'DNS',
  wildcard: 'DNS',
  dns_dash: 'DNS',
  cf_credentials: 'Cloudflare credentials',
  cf_zone: 'Cloudflare zone',
  dns_plan: 'sync plan',
};

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
    repointed: number;
    unchanged: number;
    errors: number;
  } | null>(null);
  const [removing, setRemoving] = useState(false);
  const [removeAction, setRemoveAction] = useState<'disconnect' | 'delete' | null>(null);
  const [showDomainConfirm, setShowDomainConfirm] = useState(false);
  const [domainDraft, setDomainDraft] = useState('');
  const [domainConfirmTyped, setDomainConfirmTyped] = useState('');
  const [domainAck, setDomainAck] = useState(false);
  const [domainPreflightResult, setDomainPreflightResult] = useState<DomainPreflightResult | null>(null);
  const [domainJob, setDomainJob] = useState<DomainJob | null>(null);
  const [domainBusy, setDomainBusy] = useState(false);
  const [domainRouting, setDomainRouting] = useState<'master' | 'cloudflare'>('master');
  const [sslipMode, setSslipMode] = useState(false);
  const [domainCfToken, setDomainCfToken] = useState('');
  const [domainCfZone, setDomainCfZone] = useState('');
  const [retireBusy, setRetireBusy] = useState(false);
  const [cfValidating, setCfValidating] = useState(false);
  const [revealCount, setRevealCount] = useState<number | null>(null);
  const didAnimate = useRef(false);
  const [cfValidation, setCfValidation] = useState<{ ok: boolean; zone_name?: string | null; errors: string[] } | null>(null);
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
            if (!didAnimate.current && domainJob && domainJob.status !== 'completed') {
              didAnimate.current = true;
              setRevealCount(0);
            }
            setDomain(job.domain);
            // previous_domain must reflect this change, not the page-load snapshot.
            fetchGlobalSettings()
              .then((fresh) => setSettings(fresh))
              .catch(() => setSettings((prev) => (prev ? { ...prev, domain: job.domain } : prev)));
          }
        })
        .catch(() => {});
    }, 800);
    return () => clearInterval(timer);
  }, [domainJob, showToast]);

  // Reveal completed steps one by one before the summary.
  useEffect(() => {
    if (revealCount === null || !domainJob) return;
    const total = domainJob.steps.length;
    if (revealCount < total) {
      const t = setTimeout(() => setRevealCount((c) => (c === null ? null : c + 1)), 280);
      return () => clearTimeout(t);
    }
    const t = setTimeout(() => {
      setRevealCount(null);
      showToast('Platform domain updated', 'success');
    }, 450);
    return () => clearTimeout(t);
  }, [revealCount, domainJob, showToast]);

  const handleSave = async () => {
    if (routingMode === RoutingMode.CloudflareDns && (!cfToken.trim() || !cfZoneId.trim())) {
      showToast('Cloudflare API token and Zone ID are required');
      return;
    }
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

  const cfSaved = !!(settings && settings.cloudflare_api_token && settings.cloudflare_zone_id);
  const cfDirty =
    !!settings && (cfToken !== (settings.cloudflare_api_token || '') || cfZoneId !== (settings.cloudflare_zone_id || ''));

  const validateCf = async () => {
    setCfValidating(true);
    try {
      const result = await validateCloudflare({
        cloudflare_api_token: cfToken.trim() || undefined,
        cloudflare_zone_id: cfZoneId.trim() || undefined,
      });
      setCfValidation(result);
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Validation failed');
    } finally {
      setCfValidating(false);
    }
  };

  const removeCloudflare = async (deleteRecords: boolean) => {
    setRemoving(true);
    try {
      let deleted = 0;
      if (deleteRecords) {
        deleted = (await cleanupDnsRecords()).deleted_count;
      }
      setCfToken('');
      setCfZoneId('');
      setRoutingMode(RoutingMode.MasterProxy);
      const updated = await updateGlobalSettings({
        cloudflare_api_token: '',
        cloudflare_zone_id: '',
        routing_mode: RoutingMode.MasterProxy,
      });
      setSettings(updated);
      showToast(
        deleteRecords ? `Removed Cloudflare: ${deleted} DNS record(s) deleted` : 'Cloudflare removed — DNS records kept',
        'success',
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to remove Cloudflare';
      setError(msg);
      showToast(msg);
    } finally {
      setRemoving(false);
    }
  };

  const openDomainChange = () => {
    setDomainDraft(domain);
    setDomainConfirmTyped('');
    setDomainAck(false);
    setDomainPreflightResult(null);
    setDomainJob(null);
    setDomainRouting(routingMode === RoutingMode.CloudflareDns ? 'cloudflare' : 'master');
    setDomainCfToken('');
    setDomainCfZone('');
    setSslipMode(false);
    setRevealCount(null);
    setShowDomainConfirm(true);
    // Re-attach to a still-running job (e.g. after a page reload).
    fetchActiveDomainJob()
      .then((job) => {
        if (job && (job.status === 'running' || job.status === 'pending')) setDomainJob(job);
      })
      .catch(() => {});
  };

  const toggleSslipMode = async (on: boolean) => {
    if (!on) {
      setSslipMode(false);
      setDomainDraft(domain);
      setDomainConfirmTyped('');
      setDomainPreflightResult(null);
      setDomainAck(false);
      return;
    }
    try {
      const { domain: sslip } = await sslipDomainPreview();
      if (!sslip) {
        showToast('Could not determine this server public IP');
        return;
      }
      setSslipMode(true);
      setDomainDraft(sslip);
      setDomainRouting('master');
      setDomainConfirmTyped('');
      setDomainPreflightResult(null);
      setDomainAck(false);
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Lookup failed');
    }
  };

  const runDomainPreflight = async () => {
    setDomainBusy(true);
    setError(null);
    try {
      const result = await domainPreflight(domainDraft.trim(), {
        routing_mode: domainRouting === 'cloudflare' ? 'cloudflare_dns' : 'master_proxy',
        ...(domainRouting === 'cloudflare' && domainCfToken.trim() ? { cloudflare_api_token: domainCfToken.trim() } : {}),
        ...(domainRouting === 'cloudflare' && domainCfZone.trim() ? { cloudflare_zone_id: domainCfZone.trim() } : {}),
      });
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
      const needsAck = !sslipMode && (domainPreflightResult?.warnings.length ?? 0) > 0;
      const { job_id } = await domainApply(domainDraft.trim(), needsAck ? domainAck : false, {
        routing_mode: domainRouting === 'cloudflare' ? 'cloudflare_dns' : 'master_proxy',
        ...(domainRouting === 'cloudflare' && domainCfToken.trim() ? { cloudflare_api_token: domainCfToken.trim() } : {}),
        ...(domainRouting === 'cloudflare' && domainCfZone.trim() ? { cloudflare_zone_id: domainCfZone.trim() } : {}),
      });
      const job = await fetchDomainJob(job_id);
      didAnimate.current = false;
      setDomainJob(job);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to apply domain';
      if (msg.includes('already in progress')) {
        try {
          const active = await fetchActiveDomainJob();
          if (active) setDomainJob(active);
        } catch {}
      }
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
    (sslipMode || domainConfirmTyped.trim().toLowerCase() === domainDraft.trim().toLowerCase()) &&
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
            <div className="flex items-center gap-3">
              <span className="text-sm text-slate-200 font-mono">{domain || 'example.com'}</span>
              <button
                type="button"
                onClick={openDomainChange}
                className="px-3 py-1.5 rounded-md text-xs font-medium bg-slate-700 text-slate-100 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Change
              </button>
            </div>
            <p className="text-xs text-slate-600 mt-1">
              Projects get subdomains like <span className="text-slate-400 font-mono">{'{id}'}.{domain}</span>
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
            <label htmlFor="global-dns-target" className="block text-xs text-slate-400 mb-1">
              DNS target (this server, as DNS records point at it)
            </label>
            <input
              id="global-dns-target"
              type="text"
              value={dnsTarget}
              onChange={(e) => setDnsTarget(e.target.value)}
              placeholder={settings.public_ip || 'auto-detect on save'}
              className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
            />
            <p className="text-xs text-slate-600 mt-1">
              {dnsTarget.trim()
                ? 'Used for every DNS record this server creates.'
                : settings.public_ip
                  ? `Detected ${settings.public_ip} — leave empty to use it.`
                  : 'Could not detect this server public IP — set it explicitly.'}
            </p>
          </div>

          <div>
            <p className="text-xs text-slate-400 mb-1.5">Routing mode</p>
            <div className="grid grid-cols-2 gap-2">
              <button
                type="button"
                onClick={() => setRoutingMode(RoutingMode.MasterProxy)}
                className={`px-3 py-2 rounded-md text-xs border text-left cursor-pointer transition-colors ${
                  routingMode === RoutingMode.MasterProxy
                    ? 'bg-cyan-600/10 border-cyan-500/40 text-cyan-200'
                    : 'bg-slate-900 border-slate-700/50 text-slate-400 hover:text-slate-200'
                }`}
              >
                Master proxy
                <span className="block text-[10px] text-slate-500">your DNS, records point here</span>
              </button>
              <button
                type="button"
                onClick={() => setRoutingMode('cloudflare_dns')}
                className={`px-3 py-2 rounded-md text-xs border text-left cursor-pointer transition-colors ${
                  routingMode === RoutingMode.CloudflareDns
                    ? 'bg-cyan-600/10 border-cyan-500/40 text-cyan-200'
                    : 'bg-slate-900 border-slate-700/50 text-slate-400 hover:text-slate-200'
                }`}
              >
                Cloudflare DNS
                <span className="block text-[10px] text-slate-500">records managed for you</span>
              </button>
            </div>
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
                  onChange={(e) => {
                    setCfToken(e.target.value);
                    setCfValidation(null);
                  }}
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
                  onChange={(e) => {
                    setCfZoneId(e.target.value);
                    setCfValidation(null);
                  }}
                  placeholder="Enter Cloudflare Zone ID"
                  className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                />
                <p className="text-xs text-slate-600 mt-1">
                  Found in your Cloudflare dashboard under the domain overview.
                </p>
              </div>
              {!cfSaved || cfDirty ? (
                <div className="space-y-2">
                  <button
                    type="button"
                    onClick={validateCf}
                    disabled={cfValidating || !cfToken.trim() || !cfZoneId.trim()}
                    className="w-full py-2 rounded-md text-xs font-medium bg-cyan-600/80 text-white hover:bg-cyan-500 transition-colors disabled:opacity-50 cursor-pointer"
                  >
                    {cfValidating ? 'Checking…' : 'Check credentials'}
                  </button>
                  {cfValidation && (
                    <p className={`text-xs ${cfValidation.ok ? 'text-green-400' : 'text-red-400'}`}>
                      {cfValidation.ok
                        ? `Zone ${cfValidation.zone_name} covers ${settings.domain}`
                        : cfValidation.errors[0] || 'Validation failed'}
                    </p>
                  )}
                  <p className="text-xs text-slate-600">Save below to apply, then sync records.</p>
                  {cfSaved && (
                    <button
                      type="button"
                      onClick={() => setRemoveAction('disconnect')}
                      disabled={removing}
                      className="w-full py-2 rounded-md text-xs font-medium bg-red-600/60 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                    >
                      Remove Cloudflare
                    </button>
                  )}
                </div>
              ) : (
                <div className="space-y-2">
                  <button
                    type="button"
                    onClick={async () => {
                      setSyncing(true);
                      setSyncResult(null);
                      try {
                        const result = await syncDnsRecords();
                        setSyncResult(result);
                        if (result.errors === 0) {
                          const parts = [];
                          if (result.created > 0) parts.push(`${result.created} added`);
                          if (result.repointed > 0) parts.push(`${result.repointed} re-pointed`);
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
                      {[
                        syncResult.created > 0 && `${syncResult.created} added`,
                        syncResult.repointed > 0 && `${syncResult.repointed} re-pointed`,
                        syncResult.unchanged > 0 && `${syncResult.unchanged} already exist`,
                        syncResult.deleted > 0 && `${syncResult.deleted} removed`,
                        syncResult.errors > 0 && `${syncResult.errors} failed`,
                      ]
                        .filter(Boolean)
                        .join(', ') || 'No records to sync'}
                    </p>
                  )}
                  <div className="flex gap-2">
                    <button
                      type="button"
                      onClick={() => setRemoveAction('disconnect')}
                      disabled={removing}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600/60 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                    >
                      Remove
                    </button>
                    <button
                      type="button"
                      onClick={() => setRemoveAction('delete')}
                      disabled={removing}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                    >
                      Remove + delete records
                    </button>
                  </div>
                  <p className="text-xs text-slate-600">
                    Remove keeps your DNS records; delete only touches the ones LiteBin created.
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

      {/* Remove Cloudflare confirmation */}
      {removeAction && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className={removeAction === 'delete' ? 'text-red-400' : 'text-slate-400'} />
              <h3 className="text-sm font-semibold text-slate-100">
                {removeAction === 'delete' ? 'Remove Cloudflare and delete records' : 'Remove Cloudflare'}
              </h3>
            </div>
            <p className="text-xs text-slate-400 mb-4">
              {removeAction === 'delete'
                ? 'Clears the credentials, switches routing to master proxy, and deletes the A records LiteBin created. You cannot undo the deletion.'
                : 'Clears the credentials and switches routing to master proxy. Your DNS records stay in Cloudflare.'}
            </p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setRemoveAction(null)}
                disabled={removing}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={async () => {
                  const action = removeAction;
                  setRemoveAction(null);
                  await removeCloudflare(action === 'delete');
                }}
                disabled={removing}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {removing ? 'Removing…' : 'Confirm'}
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
              <Globe size={16} className="text-cyan-400" />
              <h3 className="text-sm font-semibold text-slate-100">Change platform domain</h3>
            </div>

            {domainJob &&
            (domainJob.status === 'running' || domainJob.status === 'pending' || revealCount !== null) ? (
              <div className="space-y-3">
                <p className="text-xs text-slate-300">
                  {revealCount !== null ? 'Finishing up…' : `Applying domain change to ${domainJob.domain}…`}
                </p>
                <DomainStepList
                  steps={
                    revealCount === null
                      ? domainJob.steps
                      : domainJob.steps.map((st, i) => (i < revealCount ? st : { ...st, status: 'pending' as const }))
                  }
                />
              </div>
            ) : domainJob?.status === 'completed' && revealCount === null ? (
              <div className="space-y-4">
                <DomainStepList steps={domainJob.steps} />
                <p className="text-[10px] uppercase tracking-wider text-slate-500 border-t border-slate-700/40 pt-4">
                  Domain updated
                </p>
                <div className="flex items-center justify-between gap-2 rounded-md bg-slate-900/60 border border-slate-700/40 px-3 py-2.5">
                  <a
                    href={domainJob.dashboard_url}
                    className="text-sm font-mono text-slate-100 hover:underline break-all"
                  >
                    {domainJob.dashboard_url.replace('https://', '')}
                  </a>
                  <button
                    type="button"
                    onClick={() => navigator.clipboard?.writeText(domainJob.dashboard_url)}
                    className="p-1 rounded text-slate-500 hover:text-slate-300 cursor-pointer shrink-0"
                  >
                    <Copy size={13} />
                  </button>
                </div>
                <p className="text-xs text-slate-500">Open the new dashboard and sign in again.</p>
                {settings.previous_domain && (
                  <p className="text-xs text-slate-500">
                    <span className="font-mono text-slate-400">*.{settings.previous_domain}</span> keeps working for
                    24h ·{' '}
                    <button
                      type="button"
                      onClick={async () => {
                        setRetireBusy(true);
                        try {
                          const { retired } = await retirePreviousDomain();
                          showToast(
                            retired ? `Retired ${settings.previous_domain}` : 'No previous domain to retire',
                            'success',
                          );
                          const fresh = await fetchGlobalSettings();
                          setSettings(fresh);
                        } catch (e) {
                          showToast(e instanceof Error ? e.message : 'Retire failed');
                        } finally {
                          setRetireBusy(false);
                        }
                      }}
                      disabled={retireBusy}
                      className="underline hover:text-slate-300 cursor-pointer disabled:opacity-50"
                    >
                      retire it now
                    </button>
                  </p>
                )}
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
                <p className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded-md px-3 py-2">
                  {domainJob.error || 'Domain change failed'}
                </p>
                <DomainStepList steps={domainJob.steps} />
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
                <p className="text-xs text-slate-500">
                  <span className="font-mono text-slate-400">{settings.domain}</span> keeps working for 24h after
                  the change.
                </p>
                <div className="flex items-center justify-between rounded-md border border-slate-700/50 bg-slate-900/60 px-3 py-2">
                  <div>
                    <p className="text-xs text-slate-300">Temporary mode</p>
                    <p className="text-[10px] text-slate-500">runs on an auto-assigned sslip.io domain — no domain needed</p>
                  </div>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={sslipMode}
                    onClick={() => toggleSslipMode(!sslipMode)}
                    className={`relative w-9 h-5 rounded-full transition-colors cursor-pointer shrink-0 ${
                      sslipMode ? 'bg-amber-500/80' : 'bg-slate-700'
                    }`}
                  >
                    <span
                      className={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                        sslipMode ? 'translate-x-4' : ''
                      }`}
                    />
                  </button>
                </div>

                <div>
                  <label htmlFor="domain-draft" className="block text-xs text-slate-300 mb-1.5">
                    New domain
                  </label>
                  {sslipMode ? (
                    <p className="bg-slate-900/60 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-300 font-mono">
                      {domainDraft}
                      <span className="block text-[10px] text-slate-500 font-sans">
                        auto-detected from this server IP — master proxy, no DNS setup
                      </span>
                    </p>
                  ) : (
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
                  )}
                </div>

                {!sslipMode && (
                <div>
                  <label className="block text-xs text-slate-300 mb-1.5">Routing mode</label>
                  <div className="grid grid-cols-2 gap-2">
                    <button
                      type="button"
                      onClick={() => {
                        setDomainRouting('master');
                        setDomainPreflightResult(null);
                        setDomainAck(false);
                      }}
                      className={`px-3 py-2 rounded-md text-xs border text-left cursor-pointer ${
                        domainRouting === 'master'
                          ? 'bg-cyan-600/10 border-cyan-500/40 text-cyan-200'
                          : 'bg-slate-900 border-slate-700/50 text-slate-400'
                      }`}
                    >
                      Master proxy
                      <span className="block text-[10px] text-slate-500">your DNS, records point here</span>
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setDomainRouting('cloudflare');
                        setDomainPreflightResult(null);
                        setDomainAck(false);
                      }}
                      className={`px-3 py-2 rounded-md text-xs border text-left cursor-pointer ${
                        domainRouting === 'cloudflare'
                          ? 'bg-cyan-600/10 border-cyan-500/40 text-cyan-200'
                          : 'bg-slate-900 border-slate-700/50 text-slate-400'
                      }`}
                    >
                      Cloudflare DNS
                      <span className="block text-[10px] text-slate-500">records managed for you</span>
                    </button>
                  </div>
                </div>
                )}

                {domainRouting === 'cloudflare' && !sslipMode && (
                  <div className="space-y-2">
                    <div>
                      <label htmlFor="domain-cf-token" className="block text-xs text-slate-300 mb-1">
                        API token{settings.cloudflare_api_token ? ' (blank keeps current)' : ''}
                      </label>
                      <input
                        id="domain-cf-token"
                        type="password"
                        value={domainCfToken}
                        onChange={(e) => {
                          setDomainCfToken(e.target.value);
                          setDomainPreflightResult(null);
                          setDomainAck(false);
                        }}
                        className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
                      />
                    </div>
                    <div>
                      <label htmlFor="domain-cf-zone" className="block text-xs text-slate-300 mb-1">
                        Zone ID{settings.cloudflare_zone_id ? ' (blank keeps current)' : ''}
                      </label>
                      <input
                        id="domain-cf-zone"
                        type="text"
                        value={domainCfZone}
                        onChange={(e) => {
                          setDomainCfZone(e.target.value);
                          setDomainPreflightResult(null);
                          setDomainAck(false);
                        }}
                        className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
                      />
                    </div>
                  </div>
                )}

                {!sslipMode && domainRouting === 'master' && settings.public_ip && (
                  <div className="rounded-md bg-slate-900/60 border border-slate-700/40 px-3 py-2 space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-xs text-slate-400">Server IP</span>
                      <span className="text-xs font-mono text-slate-200">{settings.public_ip}</span>
                    </div>
                    <div className="flex items-center justify-between border-t border-slate-700/40 pt-2">
                      <span className="text-[11px] font-mono text-slate-300">
                        A *.{domainDraft.trim().toLowerCase()} → {settings.public_ip}
                      </span>
                      <button
                        type="button"
                        onClick={() => navigator.clipboard?.writeText(`*.${domainDraft.trim().toLowerCase()}`)}
                        className="p-1 rounded text-slate-500 hover:text-slate-300 cursor-pointer"
                      >
                        <Copy size={11} />
                      </button>
                    </div>
                  </div>
                )}
                {!sslipMode && (
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
                )}
                {domainPreflightResult && (
                  <div className="space-y-2">
                    {domainPreflightResult.checks?.length ? (
                      <ul className="space-y-1">
                        {domainPreflightResult.checks
                          .filter((c) => c.id !== 'public_ip')
                          .map((c) => (
                          <li key={c.id} className="flex items-center gap-2 text-[11px]">
                            <span
                              className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                                c.status === 'ok'
                                  ? 'bg-green-400'
                                  : c.status === 'warn'
                                    ? 'bg-amber-400'
                                    : c.status === 'error'
                                      ? 'bg-red-400'
                                      : 'bg-slate-600'
                              }`}
                            />
                            <span className="text-slate-400">
                              {CHECK_LABELS[c.id] ?? c.id}
                              {c.status !== 'skipped' && c.detail ? (
                                <span className="text-slate-500"> — {c.detail}</span>
                              ) : null}
                              {c.status === 'skipped' ? <span className="text-slate-600"> (skipped)</span> : null}
                            </span>
                          </li>
                        ))}
                      </ul>
                    ) : null}
                    {domainPreflightResult.plan && (
                      <p className="text-[11px] text-slate-500">
                        sync plan: {domainPreflightResult.plan.created.length} to create,{' '}
                        {domainPreflightResult.plan.repointed.length} to re-point,{' '}
                        {domainPreflightResult.plan.unchanged.length} unchanged
                      </p>
                    )}
                    {domainPreflightResult.checks?.length ? null : (
                      <>
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
                      </>
                    )}
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
                  {sslipMode ? (
                    <button
                      type="button"
                      onClick={applyDomainChange}
                      disabled={domainBusy || !canContinueDomain}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                    >
                      {domainBusy ? 'Starting…' : 'Apply'}
                    </button>
                  ) : domainPreflightResult && domainPreflightResult.ok ? (
                    <>
                      <button
                        type="button"
                        onClick={runDomainPreflight}
                        disabled={domainBusy}
                        className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-600 text-slate-100 hover:bg-slate-500 disabled:opacity-50 cursor-pointer"
                      >
                        {domainBusy ? 'Checking…' : 'Check again'}
                      </button>
                      <button
                        type="button"
                        onClick={applyDomainChange}
                        disabled={domainBusy || !canContinueDomain}
                        className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                      >
                        {domainBusy ? 'Starting…' : 'Apply'}
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      onClick={runDomainPreflight}
                      disabled={domainBusy || !domainDirty}
                      className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
                    >
                      {domainBusy ? 'Checking…' : domainPreflightResult ? 'Check again' : 'Check DNS'}
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
