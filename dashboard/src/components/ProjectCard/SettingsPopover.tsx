import { ExternalLink, Loader2, Plus, Route, Settings, Trash2, X as XIcon } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import {
  createProjectRoute,
  DeployType,
  deleteProjectRoute,
  fetchProjectCapabilities,
  fetchProjectRoutes,
  grantProjectCapabilities,
  type Project,
  type ProjectCapabilityStatus,
  type ProjectRoute,
  revokeProjectCapability,
  RouteType,
  type ServiceInfo,
  updateProjectSettings,
} from '../../api';
import { useToast } from '../ToastContext';

interface SettingsPopoverProps {
  project: Project;
  domain: string;
  services: ServiceInfo[];
  dnsTarget: string;
  projectName: string;
  projectDescription: string;
  customDomainInput: string;
  settingsError: string | null;
  customDomainSaving: boolean;
  onProjectNameChange: (v: string) => void;
  onProjectDescriptionChange: (v: string) => void;
  onCustomDomainChange: (v: string) => void;
  onSettingsErrorChange: (v: string | null) => void;
  onCustomDomainSavingChange: (v: boolean) => void;
  onRefresh: () => void;
  onClose: () => void;
}

export default function SettingsPopover({
  project,
  domain,
  services,
  dnsTarget,
  projectName,
  projectDescription,
  customDomainInput,
  settingsError,
  customDomainSaving,
  onProjectNameChange,
  onProjectDescriptionChange,
  onCustomDomainChange,
  onSettingsErrorChange,
  onCustomDomainSavingChange,
  onRefresh,
  onClose,
}: SettingsPopoverProps) {
  const isCompose = project.deploy_type === DeployType.Compose;
  const [settingsTab, setSettingsTab] = useState<'general' | 'routes' | 'capabilities'>('general');

  // Routes state
  const [routes, setRoutes] = useState<ProjectRoute[]>([]);
  const [routesLoading, setRoutesLoading] = useState(false);
  const [newRouteType, setNewRouteType] = useState<RouteType>(RouteType.Path);
  const [newRoutePath, setNewRoutePath] = useState('');
  const [newRouteSubdomain, setNewRouteSubdomain] = useState('');
  const [newRouteUpstream, setNewRouteUpstream] = useState('');
  const [newRoutePriority, setNewRoutePriority] = useState(100);
  const [addingRoute, setAddingRoute] = useState(false);
  const [deletingRouteId, setDeletingRouteId] = useState<string | null>(null);

  // Capabilities state
  const [capabilities, setCapabilities] = useState<ProjectCapabilityStatus[]>([]);
  const [capabilitiesLoading, setCapabilitiesLoading] = useState(false);
  const [capabilityActionId, setCapabilityActionId] = useState<string | null>(null);
  const [capabilityCatalogOpen, setCapabilityCatalogOpen] = useState(false);
  const [capabilitySearch, setCapabilitySearch] = useState('');

  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  });

  const loadRoutes = useCallback(async () => {
    setRoutesLoading(true);
    try {
      const r = await fetchProjectRoutes(project.id);
      r.sort((a, b) => a.priority - b.priority);
      setRoutes(r);
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to load routes');
    } finally {
      setRoutesLoading(false);
    }
  }, [project.id, showToast]);

  const loadCapabilities = useCallback(async () => {
    setCapabilitiesLoading(true);
    try {
      const caps = await fetchProjectCapabilities(project.id);
      setCapabilities(caps);
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to load capabilities');
    } finally {
      setCapabilitiesLoading(false);
    }
  }, [project.id, showToast]);

  const handleAddRoute = async () => {
    if (newRouteType === RouteType.Path && !newRoutePath.trim()) return;
    if (newRouteType === RouteType.Alias && !newRouteSubdomain.trim()) return;
    if (!newRouteUpstream.trim()) return;
    setAddingRoute(true);
    try {
      await createProjectRoute(project.id, {
        route_type: newRouteType,
        path: newRouteType === RouteType.Path ? newRoutePath : undefined,
        subdomain: newRouteType === RouteType.Alias ? newRouteSubdomain : undefined,
        upstream: newRouteUpstream,
        priority: newRoutePriority,
      });
      setNewRoutePath('');
      setNewRouteSubdomain('');
      setNewRouteUpstream('');
      setNewRoutePriority(100);
      await loadRoutes();
      onRefresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to create route');
    } finally {
      setAddingRoute(false);
    }
  };

  const handleDeleteRoute = async (routeId: string) => {
    setDeletingRouteId(routeId);
    try {
      await deleteProjectRoute(project.id, routeId);
      await loadRoutes();
      onRefresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to delete route');
    } finally {
      setDeletingRouteId(null);
    }
  };

  const handleGrantCapability = async (cap: ProjectCapabilityStatus) => {
    const recreateNote = cap.requires_recreate ? ' A recreate will be required for this to take effect.' : '';
    if (!confirm(`Grant "${cap.label}" to this project?${recreateNote}`)) return;
    setCapabilityActionId(cap.id);
    try {
      const updated = await grantProjectCapabilities(project.id, [cap.id]);
      setCapabilities(updated);
      showToast(`Granted ${cap.label}`, 'success');
      onRefresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to grant capability');
    } finally {
      setCapabilityActionId(null);
    }
  };

  const handleRevokeCapability = async (cap: ProjectCapabilityStatus) => {
    const recreateNote = cap.requires_recreate ? ' A recreate will be required for this to take effect.' : '';
    if (!confirm(`Revoke "${cap.label}" from this project?${recreateNote}`)) return;
    setCapabilityActionId(cap.id);
    try {
      const updated = await revokeProjectCapability(project.id, cap.id);
      setCapabilities(updated);
      showToast(`Revoked ${cap.label}`, 'success');
      onRefresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to revoke capability');
    } finally {
      setCapabilityActionId(null);
    }
  };

  // Load routes when switching to routes tab
  useEffect(() => {
    if (settingsTab === 'routes') {
      loadRoutes();
    }
  }, [settingsTab, loadRoutes]);

  // Load capabilities when switching to capabilities tab
  useEffect(() => {
    if (settingsTab === 'capabilities' && isCompose) {
      loadCapabilities();
    }
  }, [settingsTab, isCompose, loadCapabilities]);

  const [saving, setSaving] = useState(false);

  const handleSaveNameDesc = async () => {
    onSettingsErrorChange(null);
    setSaving(true);
    try {
      await updateProjectSettings(project.id, {
        name: projectName,
        description: projectDescription,
      });
      showToast('Settings saved', 'success');
      onClose();
    } catch (e) {
      onSettingsErrorChange(e instanceof Error ? e.message : 'Failed to save');
      showToast(e instanceof Error ? e.message : 'Failed to save');
    } finally {
      setSaving(false);
    }
  };

  const handleSetCustomDomain = async () => {
    const d = customDomainInput.trim();
    if (!d) return;
    onCustomDomainSavingChange(true);
    onSettingsErrorChange(null);
    try {
      await updateProjectSettings(project.id, { custom_domain: d });
      onRefresh();
    } catch (e) {
      onSettingsErrorChange(e instanceof Error ? e.message : 'Failed to set domain');
      showToast(e instanceof Error ? e.message : 'Failed to set domain');
    } finally {
      onCustomDomainSavingChange(false);
    }
  };

  const handleRemoveCustomDomain = async () => {
    onCustomDomainSavingChange(true);
    onSettingsErrorChange(null);
    try {
      await updateProjectSettings(project.id, { custom_domain: '' });
      onCustomDomainChange('');
      onRefresh();
    } catch (e) {
      onSettingsErrorChange(e instanceof Error ? e.message : 'Failed to remove domain');
      showToast(e instanceof Error ? e.message : 'Failed to remove domain');
    } finally {
      onCustomDomainSavingChange(false);
    }
  };

  return (
    <div ref={ref}>
      <button
        type="button"
        onClick={(e) => e.stopPropagation()}
        className={`flex items-center justify-center px-2.5 py-2 rounded-md border transition-colors cursor-pointer ${'bg-slate-900/80 border-violet-500/40 text-slate-300'}`}
        title="Project settings"
      >
        <Settings size={12} />
      </button>
      <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3">
        {settingsError && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5 -mx-3 -mt-1 mb-3">
            {settingsError}
          </div>
        )}
        {/* Tabs */}
        <div className="flex border-b border-slate-700/50 -mx-3 -mt-1 mb-3">
          <button
            type="button"
            onClick={() => setSettingsTab('general')}
            className={`flex-1 px-3 py-2 text-[10px] font-medium transition-colors cursor-pointer ${
              settingsTab === 'general'
                ? 'text-violet-300 border-b-2 border-violet-500'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            General
          </button>
          <button
            type="button"
            onClick={() => setSettingsTab('routes')}
            className={`flex-1 px-3 py-2 text-[10px] font-medium transition-colors cursor-pointer ${
              settingsTab === 'routes'
                ? 'text-violet-300 border-b-2 border-violet-500'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            Routes
          </button>
          {isCompose && (
            <button
              type="button"
              onClick={() => setSettingsTab('capabilities')}
              className={`flex-1 px-3 py-2 text-[10px] font-medium transition-colors cursor-pointer ${
                settingsTab === 'capabilities'
                  ? 'text-violet-300 border-b-2 border-violet-500'
                  : 'text-slate-400 hover:text-slate-200'
              }`}
            >
              Capabilities
            </button>
          )}
        </div>

        {settingsTab === 'general' && (
          <div className="space-y-3">
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Project name</span>
              <input
                type="text"
                value={projectName}
                onChange={(e) => onProjectNameChange(e.target.value)}
                placeholder={project.id}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Description</span>
              <input
                type="text"
                value={projectDescription}
                onChange={(e) => onProjectDescriptionChange(e.target.value)}
                placeholder="What this app does"
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>

            <button
              type="button"
              onClick={handleSaveNameDesc}
              disabled={saving}
              className="w-full py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors cursor-pointer"
            >
              {saving ? 'Saving...' : 'Save'}
            </button>

            {/* Custom domain */}
            <div className="border-t border-slate-700/50 pt-3 space-y-2">
              <div className="text-[11px] text-slate-500">
                Subdomain:{' '}
                <span className="text-slate-300 font-mono">
                  {project.id}.{domain}
                </span>
              </div>
              <div className="flex items-center gap-1.5">
                <input
                  type="text"
                  value={customDomainInput}
                  onChange={(e) => onCustomDomainChange(e.target.value)}
                  placeholder="app.example.com"
                  className="flex-1 bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
                />
                {project.custom_domain && (
                  <button
                    type="button"
                    onClick={handleRemoveCustomDomain}
                    disabled={customDomainSaving}
                    className="p-1.5 text-slate-400 hover:text-red-400 transition-colors cursor-pointer disabled:opacity-50"
                    title="Remove custom domain"
                  >
                    <XIcon size={14} />
                  </button>
                )}
                <button
                  type="button"
                  onClick={handleSetCustomDomain}
                  disabled={customDomainSaving || !customDomainInput.trim()}
                  className="px-2.5 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {customDomainSaving ? <Loader2 size={12} className="animate-spin" /> : 'Set'}
                </button>
              </div>
              {project.custom_domain && (
                <div className="flex items-center gap-1 text-[10px] text-slate-600">
                  <span className="text-slate-500">Active:</span>
                  <span className="text-slate-400 font-mono">{project.custom_domain}</span>
                </div>
              )}
              <div className="text-[10px] text-slate-600 space-y-0.5">
                {(() => {
                  const cd = customDomainInput.trim() || project.custom_domain;
                  if (!cd) return null;
                  const parts = cd.split('.');
                  const isApex = parts.length <= 2;
                  if (isApex) {
                    return (
                      <>
                        {dnsTarget && (
                          <div>
                            A record <span className="text-slate-400 font-mono">{cd}</span> →{' '}
                            <span className="text-slate-400 font-mono">{dnsTarget}</span>
                          </div>
                        )}
                        <div>
                          CNAME <span className="text-slate-400 font-mono">{cd}</span> →{' '}
                          <span className="text-slate-400 font-mono">
                            {project.id}.{domain}
                          </span>{' '}
                          <span className="text-slate-500">(Cloudflare only)</span>
                        </div>
                      </>
                    );
                  }
                  return (
                    <div>
                      CNAME <span className="text-slate-400 font-mono">{cd}</span> →{' '}
                      <span className="text-slate-400 font-mono">
                        {project.id}.{domain}
                      </span>
                    </div>
                  );
                })()}
              </div>
            </div>
          </div>
        )}

        {settingsTab === 'routes' && (
          <div className="space-y-3">
            {routesLoading ? (
              <div className="flex items-center justify-center py-4">
                <Loader2 size={16} className="animate-spin text-slate-500" />
              </div>
            ) : routes.length === 0 ? (
              <div className="text-center py-1">
                <Route size={20} className="mx-auto text-slate-600 mb-2" />
                <p className="text-xs text-slate-500">No custom routes configured</p>
              </div>
            ) : (
              <div className="space-y-1.5">
                {routes.map((route) => (
                  <div key={route.id} className="bg-slate-900/50 rounded px-2 py-1.5">
                    <div className="flex items-center gap-2">
                      <span
                        className={`text-[9px] px-1.5 py-0.5 rounded font-medium shrink-0 ${
                          route.route_type === RouteType.Path
                            ? 'bg-sky-500/15 text-sky-400'
                            : 'bg-violet-500/15 text-violet-400'
                        }`}
                      >
                        {route.route_type === RouteType.Path ? 'PATH' : 'ALIAS'}
                      </span>
                      {route.route_type === RouteType.Path ? (
                        <>
                          <span className="text-xs text-slate-300 font-mono truncate min-w-0">{route.path}</span>
                          <span
                            className="text-[10px] text-slate-500 font-mono truncate min-w-0"
                            title={route.upstream}
                          >
                            {route.upstream}
                          </span>
                        </>
                      ) : (
                        <>
                          <span className="text-xs text-slate-300 font-mono truncate min-w-0">{route.subdomain}</span>
                          <span className="text-[10px] text-slate-600">→</span>
                          <span
                            className="text-[10px] text-slate-500 font-mono truncate min-w-0"
                            title={route.upstream}
                          >
                            {route.upstream}
                          </span>
                        </>
                      )}
                      <button
                        type="button"
                        onClick={() => handleDeleteRoute(route.id)}
                        disabled={deletingRouteId === route.id}
                        className="p-1 text-slate-500 hover:text-red-400 transition-colors cursor-pointer disabled:opacity-50 shrink-0 ml-auto"
                      >
                        {deletingRouteId === route.id ? (
                          <Loader2 size={11} className="animate-spin" />
                        ) : (
                          <Trash2 size={11} />
                        )}
                      </button>
                    </div>
                    {route.route_type === RouteType.Alias && route.subdomain && (
                      <div className="flex items-center gap-2 text-[10px] text-slate-500 mt-1">
                        <span className="text-[10px] px-1.5 py-0.5 rounded font-medium bg-amber-500/15 text-amber-400">
                          P{route.priority}
                        </span>
                        <a
                          href={`https://${route.subdomain}.${domain}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="flex items-center gap-1 font-mono hover:text-sky-400 transition-colors"
                        >
                          {route.subdomain}.{domain}
                          <ExternalLink size={10} />
                        </a>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}

            {/* Add route form */}
            <div className="border-t border-slate-700/50 pt-3 space-y-2">
              <div className="text-[11px] text-slate-500">Add route</div>
              {/* Type toggle */}
              <div className="flex gap-1">
                <button
                  type="button"
                  onClick={() => setNewRouteType(RouteType.Path)}
                  className={`flex-1 py-1.5 rounded text-[10px] font-medium transition-colors cursor-pointer ${
                    newRouteType === RouteType.Path
                      ? 'bg-sky-500/20 text-sky-400'
                      : 'bg-slate-900/50 text-slate-500 hover:text-slate-300'
                  }`}
                >
                  Path
                </button>
                <button
                  type="button"
                  onClick={() => setNewRouteType(RouteType.Alias)}
                  className={`flex-1 py-1.5 rounded text-[10px] font-medium transition-colors cursor-pointer ${
                    newRouteType === RouteType.Alias
                      ? 'bg-violet-500/20 text-violet-400'
                      : 'bg-slate-900/50 text-slate-500 hover:text-slate-300'
                  }`}
                >
                  Alias
                </button>
              </div>
              {newRouteType === RouteType.Path ? (
                <input
                  type="text"
                  value={newRoutePath}
                  onChange={(e) => setNewRoutePath(e.target.value)}
                  placeholder="/api/*"
                  className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
                />
              ) : (
                <div>
                  <input
                    type="text"
                    value={newRouteSubdomain}
                    onChange={(e) => setNewRouteSubdomain(e.target.value)}
                    placeholder="api"
                    className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
                  />
                  {newRouteSubdomain && (
                    <div className="text-[10px] text-slate-600 mt-1 font-mono">
                      {newRouteSubdomain}.{project.id}.{domain} &middot; {newRouteSubdomain}.{domain}
                    </div>
                  )}
                </div>
              )}
              <div className="relative">
                <input
                  type="text"
                  value={newRouteUpstream}
                  onChange={(e) => setNewRouteUpstream(e.target.value)}
                  placeholder="litebin-myapp-api:3001"
                  className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500 pr-2"
                />
                {services.length > 0 && (
                  <div className="absolute right-1.5 top-1/2 -translate-y-1/2 flex gap-0.5">
                    {services
                      .filter((s) => s.port != null)
                      .map((s) => (
                        <button
                          key={s.service_name}
                          type="button"
                          onClick={() => setNewRouteUpstream(`litebin-${project.id}.${s.service_name}:${s.port}`)}
                          className="px-1.5 py-0.5 rounded text-[9px] bg-slate-600 text-slate-300 hover:bg-slate-500 transition-colors cursor-pointer"
                          title={`${s.service_name}:${s.port}`}
                        >
                          {s.port}
                        </button>
                      ))}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[10px] text-slate-500 shrink-0">Priority</span>
                <input
                  type="number"
                  value={newRoutePriority}
                  onChange={(e) => setNewRoutePriority(Number(e.target.value))}
                  className="w-16 bg-slate-700 border border-slate-600 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500"
                />
                <button
                  type="button"
                  onClick={handleAddRoute}
                  disabled={
                    addingRoute ||
                    !newRouteUpstream.trim() ||
                    (newRouteType === RouteType.Path && !newRoutePath.trim()) ||
                    (newRouteType === RouteType.Alias && !newRouteSubdomain.trim())
                  }
                  className="ml-auto inline-flex items-center gap-1 px-2.5 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {addingRoute ? (
                    <Loader2 size={11} className="animate-spin" />
                  ) : (
                    <>
                      <Plus size={11} /> Add
                    </>
                  )}
                </button>
              </div>
            </div>
          </div>
        )}

        {settingsTab === 'capabilities' && isCompose && (
          <div className="space-y-3">
            {capabilitiesLoading ? (
              <div className="flex items-center justify-center py-4">
                <Loader2 size={16} className="animate-spin text-slate-500" />
              </div>
            ) : (
              <>
                {(() => {
                  const requested = capabilities.filter((c) => c.requested_reason);
                  const grantedExtra = capabilities.filter(
                    (c) => c.granted && !c.requested_reason,
                  );
                  const catalog = capabilities.filter(
                    (c) => !c.requested_reason && !c.granted,
                  );
                  const q = capabilitySearch.trim().toLowerCase();
                  const catalogFiltered = q
                    ? catalog.filter(
                        (c) =>
                          c.label.toLowerCase().includes(q) ||
                          c.id.toLowerCase().includes(q) ||
                          c.description.toLowerCase().includes(q),
                      )
                    : catalog;

                  const renderCap = (
                    cap: ProjectCapabilityStatus,
                    opts?: { showReason?: boolean },
                  ) => (
                    <div key={cap.id} className="bg-slate-900/50 rounded px-2.5 py-2 space-y-1.5">
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <div className="flex items-center gap-1.5 flex-wrap">
                            <span className="text-xs text-slate-200 font-medium">{cap.label}</span>
                            {cap.granted ? (
                              <span className="text-[9px] px-1.5 py-0.5 rounded font-medium bg-emerald-500/15 text-emerald-400">
                                Granted
                              </span>
                            ) : cap.requested_reason ? (
                              <span className="text-[9px] px-1.5 py-0.5 rounded font-medium bg-amber-500/15 text-amber-400">
                                Needed
                              </span>
                            ) : null}
                          </div>
                          <p className="text-[11px] text-slate-400 mt-0.5">{cap.description}</p>
                          {opts?.showReason && cap.requested_reason && (
                            <p className="text-[10px] text-amber-300/80 mt-1">{cap.requested_reason}</p>
                          )}
                          {cap.risk && (
                            <p className="text-[10px] text-amber-400/80 mt-1">{cap.risk}</p>
                          )}
                          {cap.requires_recreate && (
                            <p className="text-[10px] text-slate-500 mt-1">Requires recreate to apply</p>
                          )}
                        </div>
                        {cap.granted ? (
                          <button
                            type="button"
                            onClick={() => handleRevokeCapability(cap)}
                            disabled={capabilityActionId === cap.id}
                            className="shrink-0 px-2 py-1 rounded text-[10px] font-medium bg-slate-700 text-slate-300 hover:bg-red-500/20 hover:text-red-300 transition-colors disabled:opacity-50 cursor-pointer"
                          >
                            {capabilityActionId === cap.id ? (
                              <Loader2 size={11} className="animate-spin" />
                            ) : (
                              'Revoke'
                            )}
                          </button>
                        ) : (
                          <button
                            type="button"
                            onClick={() => handleGrantCapability(cap)}
                            disabled={capabilityActionId === cap.id}
                            className="shrink-0 px-2 py-1 rounded text-[10px] font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                          >
                            {capabilityActionId === cap.id ? (
                              <Loader2 size={11} className="animate-spin" />
                            ) : (
                              'Grant'
                            )}
                          </button>
                        )}
                      </div>
                    </div>
                  );

                  return (
                    <div className="space-y-3">
                      <div className="space-y-2">
                        <p className="text-[10px] font-medium text-slate-500 uppercase tracking-wide">
                          Needed by Compose
                        </p>
                        {requested.length === 0 ? (
                          <p className="text-[11px] text-slate-500 px-0.5">
                            Current compose file does not request any capabilities.
                          </p>
                        ) : (
                          requested.map((cap) => renderCap(cap, { showReason: true }))
                        )}
                      </div>

                      {grantedExtra.length > 0 && (
                        <div className="space-y-2">
                          <p className="text-[10px] font-medium text-slate-500 uppercase tracking-wide">
                            Granted (not requested by current compose)
                          </p>
                          {grantedExtra.map((cap) => renderCap(cap))}
                        </div>
                      )}

                      {catalog.length > 0 && (
                        <details
                          open={capabilityCatalogOpen}
                          onToggle={(e) =>
                            setCapabilityCatalogOpen((e.target as HTMLDetailsElement).open)
                          }
                          className="rounded border border-slate-700/50 bg-slate-900/30"
                        >
                          <summary className="px-2.5 py-2 text-[11px] text-slate-400 cursor-pointer select-none">
                            Browse other capabilities
                            <span className="ml-1 opacity-60">({catalog.length})</span>
                          </summary>
                          <div className="px-2.5 pb-2.5 space-y-2 border-t border-slate-700/40 pt-2">
                            <input
                              type="search"
                              value={capabilitySearch}
                              onChange={(e) => setCapabilitySearch(e.target.value)}
                              placeholder="Search capabilities…"
                              className="w-full px-2 py-1.5 rounded bg-slate-900/60 border border-slate-700/50 text-[11px] text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50"
                            />
                            {catalogFiltered.length === 0 ? (
                              <p className="text-[11px] text-slate-500">No matches</p>
                            ) : (
                              catalogFiltered.map((cap) => renderCap(cap))
                            )}
                          </div>
                        </details>
                      )}
                    </div>
                  );
                })()}
              </>
            )}
          </div>
        )}

      </div>
    </div>
  );
}
