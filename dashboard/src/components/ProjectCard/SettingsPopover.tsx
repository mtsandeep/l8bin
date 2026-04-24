import { useState, useEffect, useRef } from "react";
import {
  Loader2,
  Settings,
  Plus,
  Route,
  Trash2,
  X as XIcon,
  ExternalLink,
} from "lucide-react";
import { useToast } from "../ToastContext";
import {
  type Project,
  type ProjectRoute,
  type ServiceInfo,
  fetchProjectRoutes,
  createProjectRoute,
  deleteProjectRoute,
  updateProjectSettings,
} from "../../api";

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
  const [settingsTab, setSettingsTab] = useState<"general" | "routes">(
    "general",
  );

  // Routes state
  const [routes, setRoutes] = useState<ProjectRoute[]>([]);
  const [routesLoading, setRoutesLoading] = useState(false);
  const [newRouteType, setNewRouteType] = useState<"path" | "alias">("path");
  const [newRoutePath, setNewRoutePath] = useState("");
  const [newRouteSubdomain, setNewRouteSubdomain] = useState("");
  const [newRouteUpstream, setNewRouteUpstream] = useState("");
  const [newRoutePriority, setNewRoutePriority] = useState(100);
  const [addingRoute, setAddingRoute] = useState(false);
  const [deletingRouteId, setDeletingRouteId] = useState<string | null>(null);

  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  });

  const loadRoutes = async () => {
    setRoutesLoading(true);
    try {
      const r = await fetchProjectRoutes(project.id);
      r.sort((a, b) => a.priority - b.priority);
      setRoutes(r);
    } catch (e) {
      showToast(e instanceof Error ? e.message : "Failed to load routes");
    } finally {
      setRoutesLoading(false);
    }
  };

  const handleAddRoute = async () => {
    if (newRouteType === "path" && !newRoutePath.trim()) return;
    if (newRouteType === "alias" && !newRouteSubdomain.trim()) return;
    if (!newRouteUpstream.trim()) return;
    setAddingRoute(true);
    try {
      await createProjectRoute(project.id, {
        route_type: newRouteType,
        path: newRouteType === "path" ? newRoutePath : undefined,
        subdomain: newRouteType === "alias" ? newRouteSubdomain : undefined,
        upstream: newRouteUpstream,
        priority: newRoutePriority,
      });
      setNewRoutePath("");
      setNewRouteSubdomain("");
      setNewRouteUpstream("");
      setNewRoutePriority(100);
      await loadRoutes();
      onRefresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : "Failed to create route");
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
      showToast(e instanceof Error ? e.message : "Failed to delete route");
    } finally {
      setDeletingRouteId(null);
    }
  };

  // Load routes when switching to routes tab
  useEffect(() => {
    if (settingsTab === "routes") {
      loadRoutes();
    }
  }, [settingsTab]);

  const handleSaveNameDesc = async () => {
    onSettingsErrorChange(null);
    try {
      await updateProjectSettings(project.id, {
        name: projectName,
        description: projectDescription,
      });
      onRefresh();
    } catch (e) {
      onSettingsErrorChange(e instanceof Error ? e.message : "Failed to save");
      showToast(e instanceof Error ? e.message : "Failed to save");
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
      onSettingsErrorChange(
        e instanceof Error ? e.message : "Failed to set domain",
      );
      showToast(e instanceof Error ? e.message : "Failed to set domain");
    } finally {
      onCustomDomainSavingChange(false);
    }
  };

  const handleRemoveCustomDomain = async () => {
    onCustomDomainSavingChange(true);
    onSettingsErrorChange(null);
    try {
      await updateProjectSettings(project.id, { custom_domain: "" });
      onCustomDomainChange("");
      onRefresh();
    } catch (e) {
      onSettingsErrorChange(
        e instanceof Error ? e.message : "Failed to remove domain",
      );
      showToast(e instanceof Error ? e.message : "Failed to remove domain");
    } finally {
      onCustomDomainSavingChange(false);
    }
  };

  return (
    <div ref={ref}>
      <button
        onClick={(e) => e.stopPropagation()}
        className={`flex items-center justify-center px-2.5 py-2 rounded-md border transition-colors cursor-pointer ${"bg-slate-900/80 border-violet-500/40 text-slate-300"}`}
        title="Project settings">
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
            onClick={() => setSettingsTab("general")}
            className={`flex-1 px-3 py-2 text-[10px] font-medium transition-colors cursor-pointer ${
              settingsTab === "general"
                ? "text-violet-300 border-b-2 border-violet-500"
                : "text-slate-400 hover:text-slate-200"
            }`}>
            General
          </button>
          <button
            onClick={() => setSettingsTab("routes")}
            className={`flex-1 px-3 py-2 text-[10px] font-medium transition-colors cursor-pointer ${
              settingsTab === "routes"
                ? "text-violet-300 border-b-2 border-violet-500"
                : "text-slate-400 hover:text-slate-200"
            }`}>
            Routes
          </button>
        </div>

        {settingsTab === "general" && (
          <div className="space-y-3">
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">
                Project name
              </span>
              <input
                type="text"
                value={projectName}
                onChange={(e) => onProjectNameChange(e.target.value)}
                placeholder={project.id}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">
                Description
              </span>
              <input
                type="text"
                value={projectDescription}
                onChange={(e) => onProjectDescriptionChange(e.target.value)}
                placeholder="What this app does"
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <button
              onClick={handleSaveNameDesc}
              className="w-full py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer">
              Save
            </button>

            {/* Custom domain */}
            <div className="border-t border-slate-700/50 pt-3 space-y-2">
              <div className="text-[11px] text-slate-500">
                Subdomain:{" "}
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
                    onClick={handleRemoveCustomDomain}
                    disabled={customDomainSaving}
                    className="p-1.5 text-slate-400 hover:text-red-400 transition-colors cursor-pointer disabled:opacity-50"
                    title="Remove custom domain">
                    <XIcon size={14} />
                  </button>
                )}
                <button
                  onClick={handleSetCustomDomain}
                  disabled={customDomainSaving || !customDomainInput.trim()}
                  className="px-2.5 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer">
                  {customDomainSaving ? (
                    <Loader2 size={12} className="animate-spin" />
                  ) : (
                    "Set"
                  )}
                </button>
              </div>
              {project.custom_domain && (
                <div className="flex items-center gap-1 text-[10px] text-slate-600">
                  <span className="text-slate-500">Active:</span>
                  <span className="text-slate-400 font-mono">
                    {project.custom_domain}
                  </span>
                </div>
              )}
              <div className="text-[10px] text-slate-600 space-y-0.5">
                {(() => {
                  const cd = customDomainInput.trim() || project.custom_domain;
                  if (!cd) return null;
                  const parts = cd.split(".");
                  const isApex = parts.length <= 2;
                  if (isApex) {
                    return (
                      <>
                        {dnsTarget && (
                          <div>
                            A record{" "}
                            <span className="text-slate-400 font-mono">
                              {cd}
                            </span>{" "}
                            →{" "}
                            <span className="text-slate-400 font-mono">
                              {dnsTarget}
                            </span>
                          </div>
                        )}
                        <div>
                          CNAME{" "}
                          <span className="text-slate-400 font-mono">{cd}</span>{" "}
                          →{" "}
                          <span className="text-slate-400 font-mono">
                            {project.id}.{domain}
                          </span>{" "}
                          <span className="text-slate-500">
                            (Cloudflare only)
                          </span>
                        </div>
                      </>
                    );
                  }
                  return (
                    <div>
                      CNAME{" "}
                      <span className="text-slate-400 font-mono">{cd}</span> →{" "}
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

        {settingsTab === "routes" && (
          <div className="space-y-3">
            {routesLoading ? (
              <div className="flex items-center justify-center py-4">
                <Loader2 size={16} className="animate-spin text-slate-500" />
              </div>
            ) : routes.length === 0 ? (
              <div className="text-center py-1">
                <Route size={20} className="mx-auto text-slate-600 mb-2" />
                <p className="text-xs text-slate-500">
                  No custom routes configured
                </p>
              </div>
            ) : (
              <div className="space-y-1.5">
                {routes.map((route) => (
                  <div
                    key={route.id}
                    className="bg-slate-900/50 rounded px-2 py-1.5">
                    <div className="flex items-center gap-2">
                      <span
                        className={`text-[9px] px-1.5 py-0.5 rounded font-medium shrink-0 ${
                          route.route_type === "path"
                            ? "bg-sky-500/15 text-sky-400"
                            : "bg-violet-500/15 text-violet-400"
                        }`}>
                        {route.route_type === "path" ? "PATH" : "ALIAS"}
                      </span>
                      {route.route_type === "path" ? (
                        <>
                          <span className="text-xs text-slate-300 font-mono truncate min-w-0">
                            {route.path}
                          </span>
                          <span
                            className="text-[10px] text-slate-500 font-mono truncate min-w-0"
                            title={route.upstream}>
                            {route.upstream}
                          </span>
                        </>
                      ) : (
                        <>
                          <span className="text-xs text-slate-300 font-mono truncate min-w-0">
                            {route.subdomain}
                          </span>
                          <span className="text-[10px] text-slate-600">→</span>
                          <span
                            className="text-[10px] text-slate-500 font-mono truncate min-w-0"
                            title={route.upstream}>
                            {route.upstream}
                          </span>
                        </>
                      )}
                      <button
                        onClick={() => handleDeleteRoute(route.id)}
                        disabled={deletingRouteId === route.id}
                        className="p-1 text-slate-500 hover:text-red-400 transition-colors cursor-pointer disabled:opacity-50 shrink-0 ml-auto">
                        {deletingRouteId === route.id ? (
                          <Loader2 size={11} className="animate-spin" />
                        ) : (
                          <Trash2 size={11} />
                        )}
                      </button>
                    </div>
                    {route.route_type === "alias" && route.subdomain && (
                      <div className="flex items-center gap-2 text-[10px] text-slate-500 mt-1">
                        <span className="text-[10px] px-1.5 py-0.5 rounded font-medium bg-amber-500/15 text-amber-400">
                          P{route.priority}
                        </span>
                        <a
                          href={`https://${route.subdomain}.${domain}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="flex items-center gap-1 font-mono hover:text-sky-400 transition-colors">
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
                  onClick={() => setNewRouteType("path")}
                  className={`flex-1 py-1.5 rounded text-[10px] font-medium transition-colors cursor-pointer ${
                    newRouteType === "path"
                      ? "bg-sky-500/20 text-sky-400"
                      : "bg-slate-900/50 text-slate-500 hover:text-slate-300"
                  }`}>
                  Path
                </button>
                <button
                  type="button"
                  onClick={() => setNewRouteType("alias")}
                  className={`flex-1 py-1.5 rounded text-[10px] font-medium transition-colors cursor-pointer ${
                    newRouteType === "alias"
                      ? "bg-violet-500/20 text-violet-400"
                      : "bg-slate-900/50 text-slate-500 hover:text-slate-300"
                  }`}>
                  Alias
                </button>
              </div>
              {newRouteType === "path" ? (
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
                      {newRouteSubdomain}.{project.id}.{domain} &middot;{" "}
                      {newRouteSubdomain}.{domain}
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
                    {services.filter(s => s.port != null).map(s => (
                      <button
                        key={s.service_name}
                        type="button"
                        onClick={() =>
                          setNewRouteUpstream(
                            `litebin-${project.id}.${s.service_name}:${s.port}`,
                          )
                        }
                        className="px-1.5 py-0.5 rounded text-[9px] bg-slate-600 text-slate-300 hover:bg-slate-500 transition-colors cursor-pointer"
                        title={`${s.service_name}:${s.port}`}>
                        {s.port}
                      </button>
                    ))}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[10px] text-slate-500 shrink-0">
                  Priority
                </span>
                <input
                  type="number"
                  value={newRoutePriority}
                  onChange={(e) => setNewRoutePriority(Number(e.target.value))}
                  className="w-16 bg-slate-700 border border-slate-600 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500"
                />
                <button
                  onClick={handleAddRoute}
                  disabled={
                    addingRoute ||
                    !newRouteUpstream.trim() ||
                    (newRouteType === "path" && !newRoutePath.trim()) ||
                    (newRouteType === "alias" && !newRouteSubdomain.trim())
                  }
                  className="ml-auto inline-flex items-center gap-1 px-2.5 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer">
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
      </div>
    </div>
  );
}
