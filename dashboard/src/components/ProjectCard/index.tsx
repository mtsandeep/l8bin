import { useState, useEffect, useRef } from "react";
import {
  Play,
  Square,
  Trash2,
  Clock,
  ExternalLink,
  Loader2,
  ScrollText,
  RotateCcw,
  RefreshCw,
  MoreHorizontal,
  Copy,
  ChevronRight,
  Moon,
  Terminal,
  Settings,
  Layers,
} from "lucide-react";
import StatusBadge from "../StatusBadge";
import LogViewer from "../LogViewer";
import { useToast } from "../ToastContext";
import {
  type Project,
  type Node as ApiNode,
  type ProjectStats,
  stopProject,
  startProject,
  deleteProject,
  recreateProject,
  redeployProject,
  timeAgo,
  formatBytes,
} from "../../api";
import SleepPopover from "./SleepPopover";
import StatsGrid from "./ProjectStats";
import AppSettingsPopover from "./AppSettingsPopover";
import SettingsPopover from "./SettingsPopover";
import RedeployModal from "./RedeployModal";

interface ProjectCardProps {
  project: Project;
  stats: ProjectStats | null;
  nodes: ApiNode[];
  onRefresh: () => void;
  projectsDir: string;
  domain: string;
  dnsTarget: string;
}

function shortImage(image: string | null): string {
  if (!image) return "—";
  const hash = image.startsWith("sha256:") ? image.slice(7) : image;
  return hash.length > 12 ? hash.slice(0, 12) : hash;
}

export default function ProjectCard({
  project,
  stats,
  nodes,
  onRefresh,
  projectsDir,
  domain,
  dnsTarget,
}: ProjectCardProps) {
  const [loading, setLoading] = useState<string | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showLogs, setShowLogs] = useState(false);
  const [showActions, setShowActions] = useState(false);
  const [envCopied, setEnvCopied] = useState(false);
  const actionsRef = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  // Which popover is open: null | 'sleep' | 'app' | 'settings'
  const [openPopover, setOpenPopover] = useState<string | null>(null);

  // Sleep badge display values — updated by SleepPopover onChange
  const [autoStop, setAutoStop] = useState(project.auto_stop_enabled);
  const [timeoutMins, setTimeoutMins] = useState(
    project.auto_stop_timeout_mins,
  );
  const [autoStart, setAutoStart] = useState(project.auto_start_enabled);

  // Settings state for gear popover
  const [projectName, setProjectName] = useState(project.name ?? "");
  const [projectDescription, setProjectDescription] = useState(
    project.description ?? "",
  );
  const [customDomainInput, setCustomDomainInput] = useState(
    project.custom_domain ?? "",
  );
  const [customDomainSaving, setCustomDomainSaving] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);

  // Redeploy modal from actions dropdown (uses project values, not edited)
  const [showRedeployModal, setShowRedeployModal] = useState(false);

  // Services popover (data comes from stats)
  const [showServicesPopover, setShowServicesPopover] = useState(false);
  const services = stats?.services ?? [];
  const servicesRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!showServicesPopover) return;
    const handler = (e: MouseEvent) => {
      if (
        servicesRef.current &&
        !servicesRef.current.contains(e.target as Node)
      ) {
        setShowServicesPopover(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showServicesPopover]);

  // Keep badge + settings state in sync when project prop changes
  useEffect(() => {
    setAutoStop(project.auto_stop_enabled);
    setTimeoutMins(project.auto_stop_timeout_mins);
    setAutoStart(project.auto_start_enabled);
    setProjectName(project.name ?? "");
    setProjectDescription(project.description ?? "");
    setCustomDomainInput(project.custom_domain ?? "");
  }, [
    project.auto_stop_enabled,
    project.auto_stop_timeout_mins,
    project.auto_start_enabled,
    project.name,
    project.description,
    project.custom_domain,
  ]);

  // Close actions dropdown on outside click
  useEffect(() => {
    if (!showActions) return;
    const handler = (e: MouseEvent) => {
      if (
        actionsRef.current &&
        !actionsRef.current.contains(e.target as unknown as globalThis.Node)
      ) {
        setShowActions(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showActions]);

  const handleAction = async (
    action: "stop" | "start" | "delete" | "redeploy" | "recreate",
    fn: () => Promise<void>,
  ) => {
    setLoading(action);
    try {
      await fn();
      onRefresh();
    } catch (e) {
      console.error(e);
      showToast(e instanceof Error ? e.message : `${action} failed`);
    } finally {
      setLoading(null);
      setShowDeleteConfirm(false);
    }
  };

  const handleActionsRedeploy = (cleanupVolumes: boolean) => {
    setShowRedeployModal(false);
    handleAction("redeploy", () =>
      redeployProject(
        project.id,
        project.image ?? "",
        project.internal_port ?? 3000,
        project.cmd,
        project.memory_limit_mb,
        project.cpu_limit,
        cleanupVolumes,
      ),
    );
  };

  const isRunning = project.status === "running";
  const isStopped = project.status === "stopped";
  const isStopping = project.status === "stopping";
  const isUnconfigured =
    project.status === "unconfigured" ||
    (project.status === "stopped" && !project.image);
  return (
    <div className="relative bg-slate-800/50 border border-slate-700/50 rounded-lg p-5 hover:border-slate-600/50 transition-colors">
      {/* Header */}
      <div className="mb-4">
        <div className="min-w-0">
          <div className="relative flex items-center gap-2 mb-1">
            <div className="flex items-center gap-1.5 min-w-0">
              <h3
                className="text-sm font-semibold text-slate-100 truncate"
                title={project.description || undefined}>
                {project.name || project.id}
              </h3>
              <StatusBadge status={project.status} />
              {services.length > 1 && (
                <span
                  className={`inline-flex items-center gap-1 px-2 py-0.5 rounded cursor-pointer border transition-colors ${
                    showServicesPopover
                      ? "bg-violet-500/30 border-violet-400/50"
                      : "bg-violet-500/15 border-violet-500/40 hover:bg-violet-500/25"
                  }`}
                  onClick={(e) => {
                    e.stopPropagation();
                    setShowServicesPopover(!showServicesPopover);
                    setOpenPopover(null);
                  }}
                  title="Services">
                  <Layers size={11} className="text-violet-300" />
                  <span className="text-violet-200 text-[10px] font-medium">
                    {services.length}
                  </span>
                </span>
              )}
            </div>
            <div className="flex items-center gap-1 ml-auto flex-shrink-0">
              <a
                href={`https://${project.custom_domain || `${project.id}.${domain}`}`}
                target="_blank"
                rel="noopener noreferrer"
                className="text-slate-400 hover:text-sky-400 transition-colors"
                title="Open app">
                <ExternalLink size={14} />
              </a>
            </div>
            {/* Services popover — below the title row */}
            {showServicesPopover && (
              <div
                ref={servicesRef}
                className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-1 py-1">
                {services.length === 0 ? (
                  <div className="px-3 py-2 text-xs text-slate-500">
                    No services
                  </div>
                ) : (
                  services.map((svc) => (
                    <div key={svc.service_name} className="px-3 py-2">
                      <div className="flex items-center justify-between gap-2">
                        <div className="flex items-center gap-1.5 min-w-0">
                          <span className="text-xs font-medium text-slate-300 truncate">
                            {svc.service_name}
                          </span>
                          {svc.is_public && (
                            <span className="text-[10px] px-1 py-0.5 rounded bg-sky-500/20 text-sky-400">
                              public
                            </span>
                          )}
                          <span
                            className={`text-[10px] px-1 py-0.5 rounded ${
                              svc.status === "running"
                                ? "bg-emerald-500/20 text-emerald-400"
                                : "bg-slate-700/60 text-slate-500"
                            }`}>
                            {svc.status}
                          </span>
                        </div>
                        {svc.cpu_percent !== undefined && (
                          <span className="text-[10px] text-slate-500 font-mono shrink-0">
                            {svc.cpu_percent.toFixed(1)}% cpu
                          </span>
                        )}
                      </div>
                      <p
                        className="text-[10px] text-slate-500 truncate font-mono mt-0.5"
                        title={svc.image}>
                        {shortImage(svc.image)}
                        {svc.port ? ` | :${svc.port}` : ""}
                      </p>
                      <div className="flex items-center gap-2 mt-1 text-[10px] font-mono">
                        {svc.cpu_limit !== undefined && (
                          <span className="text-slate-500">
                            cpu {svc.cpu_limit.toFixed(1)}
                          </span>
                        )}
                        {svc.memory_limit !== undefined && (
                          <span className="text-slate-500">
                            mem limit {formatBytes(svc.memory_limit)}
                          </span>
                        )}
                        {svc.memory_usage !== undefined && (
                          <span className="text-slate-400">
                            mem {formatBytes(svc.memory_usage)}
                          </span>
                        )}
                        {svc.disk_gb !== undefined && svc.disk_gb > 0 && (
                          <span className="text-slate-500">
                            disk {svc.disk_gb.toFixed(2)} GB
                          </span>
                        )}
                      </div>
                    </div>
                  ))
                )}
              </div>
            )}
          </div>
          <p
            className="text-xs text-slate-500 truncate font-mono"
            title={project.image ?? ""}>
            {shortImage(project.image)}
            {project.mapped_port ? ` | port: ${project.mapped_port}` : ""}
          </p>
          <div className="flex items-center gap-1.5 mt-1.5 text-[10px]">
            <span
              className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full ${
                autoStop ? "bg-slate-700/60" : "bg-slate-800/40"
              }`}>
              <span
                className={`w-1.5 h-1.5 rounded-full ${autoStop ? "bg-emerald-400" : "bg-slate-600"}`}
              />
              <span className="text-slate-400">
                Auto-stop{autoStop && ` · ${timeoutMins}m`}
              </span>
            </span>
            <span
              className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full ${
                autoStart ? "bg-slate-700/60" : "bg-slate-800/40"
              }`}>
              <span
                className={`w-1.5 h-1.5 rounded-full ${autoStart ? "bg-emerald-400" : "bg-slate-600"}`}
              />
              <span className="text-slate-400">Auto-start</span>
            </span>
          </div>
        </div>
      </div>

      <StatsGrid
        stats={stats}
        isRunning={isRunning}
        isUnconfigured={isUnconfigured}
      />

      {/* Node */}
      <div className="mb-4 px-3 py-2 bg-slate-900/50 rounded-md flex items-center gap-2 min-w-0">
        <span className="text-[10px] uppercase tracking-wider text-slate-500 shrink-0">
          Node
        </span>
        {project.node_id ? (
          (() => {
            const node = nodes.find((n) => n.id === project.node_id);
            return (
              <span className="text-xs font-mono text-slate-400 truncate">
                {node ? `${node.name} (${node.id})` : project.node_id}
              </span>
            );
          })()
        ) : (
          <span className="text-xs font-mono text-slate-400">—</span>
        )}
      </div>

      {/* Sleep + App + Services + Settings popovers */}
      <div className="relative mb-4">
        <div className="flex gap-2">
          {/* Sleep button */}
          <div className="flex-1">
            {openPopover === "sleep" ? (
              <SleepPopover
                project={project}
                onChange={(as, tm, astart) => {
                  setAutoStop(as);
                  setTimeoutMins(tm);
                  setAutoStart(astart);
                }}
                onClose={() => setOpenPopover(null)}
              />
            ) : (
              <button
                onClick={() => setOpenPopover("sleep")}
                className="w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80">
                <div className="flex items-center gap-1.5">
                  <Moon size={12} />
                  <span className="text-[10px] uppercase tracking-wider">
                    Sleep
                  </span>
                </div>
                <ChevronRight size={12} className="text-slate-500" />
              </button>
            )}
          </div>

          {/* App Settings button */}
          <div className="flex-1">
            {openPopover === "app" ? (
              <AppSettingsPopover
                project={project}
                isStopping={isStopping}
                onRefresh={onRefresh}
                onClose={() => setOpenPopover(null)}
              />
            ) : (
              <button
                onClick={() => setOpenPopover("app")}
                className="w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80">
                <div className="flex items-center gap-1.5">
                  <Terminal size={12} />
                  <span className="text-[10px] uppercase tracking-wider">
                    App
                  </span>
                </div>
                <ChevronRight size={12} className="text-slate-500" />
              </button>
            )}
          </div>

          {/* Settings gear button */}
          <div className="flex-none">
            {openPopover === "settings" ? (
              <SettingsPopover
                project={project}
                domain={domain}
                dnsTarget={dnsTarget}
                projectName={projectName}
                projectDescription={projectDescription}
                customDomainInput={customDomainInput}
                settingsError={settingsError}
                customDomainSaving={customDomainSaving}
                onProjectNameChange={setProjectName}
                onProjectDescriptionChange={setProjectDescription}
                onCustomDomainChange={setCustomDomainInput}
                onSettingsErrorChange={setSettingsError}
                onCustomDomainSavingChange={setCustomDomainSaving}
                onRefresh={onRefresh}
                onClose={() => {
                  setOpenPopover(null);
                  setProjectName(project.name ?? "");
                  setProjectDescription(project.description ?? "");
                  setCustomDomainInput(project.custom_domain ?? "");
                  setSettingsError(null);
                  setCustomDomainSaving(false);
                }}
              />
            ) : (
              <button
                onClick={() => setOpenPopover("settings")}
                className={`flex items-center justify-center px-2.5 py-2 rounded-md border transition-colors cursor-pointer ${
                  project.custom_domain || project.name
                    ? "bg-slate-900/50 border-violet-500/20 text-violet-400 hover:bg-slate-900/80"
                    : "bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80"
                }`}
                title="Project settings">
                <Settings size={12} />
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1 text-[11px] text-slate-500">
          <Clock size={11} />
          <span>{timeAgo(project.last_active_at)}</span>
        </div>

        <div className="flex items-center gap-1.5">
          <button
            onClick={() => setShowLogs(true)}
            className="inline-flex items-center gap-1 p-1.5 rounded-md text-slate-500 hover:text-violet-400 hover:bg-violet-500/10 transition-colors cursor-pointer"
            title="Logs">
            <ScrollText size={13} />
          </button>
          <div ref={actionsRef} className="relative">
            <button
              onClick={() => setShowActions(!showActions)}
              disabled={loading !== null || isStopping}
              className="inline-flex items-center gap-1 p-1.5 rounded-md text-slate-500 hover:text-slate-200 hover:bg-slate-800 transition-colors disabled:opacity-50 cursor-pointer"
              title="Actions">
              {loading === "redeploy" || loading === "recreate" ? (
                <Loader2 size={13} className="animate-spin" />
              ) : (
                <MoreHorizontal size={13} />
              )}
            </button>
            {showActions && (
              <div className="absolute right-0 bottom-full mb-1 w-60 bg-slate-800 border border-slate-700/60 rounded-lg shadow-xl py-1 z-50">
                {!isUnconfigured && (
                  <>
                    <button
                      onClick={() => {
                        setShowActions(false);
                        setShowRedeployModal(true);
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer">
                      <RotateCcw size={13} className="text-sky-400" />
                      <div className="text-left">
                        <div>Redeploy</div>
                        <div className="text-[10px] text-slate-500">
                          Pull latest image &amp; restart
                        </div>
                      </div>
                    </button>
                    <button
                      onClick={() => {
                        setShowActions(false);
                        handleAction("recreate", () =>
                          recreateProject(project.id).then(() => onRefresh()),
                        );
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer">
                      <RefreshCw size={13} className="text-emerald-400" />
                      <div className="text-left">
                        <div>Recreate</div>
                        <div className="text-[10px] text-slate-500">
                          Restart with updated env/config
                        </div>
                      </div>
                    </button>
                    <div className="mx-2 my-1 border-t border-slate-700/50" />
                  </>
                )}
                <div className="px-3 py-2">
                  <div className="flex items-center justify-between mb-1.5">
                    <span className="text-[10px] text-slate-500">
                      Env file on node:
                    </span>
                    <button
                      onClick={() => {
                        navigator.clipboard.writeText(
                          `${projectsDir}/${project.id}/.env`,
                        );
                        setEnvCopied(true);
                        setTimeout(() => setEnvCopied(false), 1500);
                      }}
                      className="rounded text-slate-500 hover:text-slate-300 transition-colors flex cursor-pointer">
                      {envCopied ? (
                        <span className="text-[10px] text-emerald-400">
                          Copied
                        </span>
                      ) : (
                        <span className="p-0.5">
                          <Copy size={11} />
                        </span>
                      )}
                    </button>
                  </div>
                  <div className="bg-slate-900 rounded px-2 py-1.5 overflow-x-auto">
                    <input
                      readOnly
                      value={`${projectsDir}/${project.id}/.env`}
                      className="bg-transparent text-[11px] text-slate-300 font-mono w-full min-w-0 outline-none cursor-text"
                      onClick={(e) => (e.target as HTMLInputElement).select()}
                    />
                  </div>
                  {project.node_id &&
                    project.node_id !== "local" &&
                    (() => {
                      const node = nodes.find((n) => n.id === project.node_id);
                      return node ? (
                        <div className="text-[10px] text-slate-600 mt-1.5 font-mono truncate">
                          SSH → {node.host}
                        </div>
                      ) : null;
                    })()}
                </div>
              </div>
            )}
          </div>
          {isStopped && (
            <button
              onClick={() =>
                handleAction("start", () => startProject(project.id))
              }
              disabled={loading !== null}
              className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20 transition-colors disabled:opacity-50 cursor-pointer"
              title="Start">
              {loading === "start" ? (
                <Loader2 size={12} className="animate-spin" />
              ) : (
                <Play size={12} />
              )}
              Start
            </button>
          )}
          {isRunning && (
            <button
              onClick={() => {
                stopProject(project.id).catch((e) => {
                  console.error(e);
                  showToast(e instanceof Error ? e.message : "Stop failed");
                });
                onRefresh();
              }}
              className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-amber-500/10 text-amber-400 hover:bg-amber-500/20 transition-colors cursor-pointer"
              title="Stop">
              <Square size={12} />
              Stop
            </button>
          )}
          {isStopping && (
            <span className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-orange-500/10 text-orange-400">
              <Loader2 size={12} className="animate-spin" />
              Stopping
            </span>
          )}

          {showDeleteConfirm ? (
            <div className="flex items-center gap-1">
              <button
                onClick={() =>
                  handleAction("delete", () => deleteProject(project.id))
                }
                disabled={loading !== null}
                className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-red-500/20 text-red-400 hover:bg-red-500/30 transition-colors disabled:opacity-50 cursor-pointer">
                {loading === "delete" ? (
                  <Loader2 size={12} className="animate-spin" />
                ) : (
                  "Confirm"
                )}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(false)}
                className="px-2 py-1.5 rounded-md text-xs text-slate-400 hover:text-slate-300 transition-colors cursor-pointer">
                Cancel
              </button>
            </div>
          ) : (
            <button
              onClick={() => setShowDeleteConfirm(true)}
              disabled={loading !== null}
              className="inline-flex items-center gap-1 p-1.5 rounded-md text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50 cursor-pointer"
              title="Delete">
              <Trash2 size={13} />
            </button>
          )}
        </div>
      </div>
      {showLogs && (
        <LogViewer projectId={project.id} onClose={() => setShowLogs(false)} />
      )}

      {/* Redeploy modal from actions dropdown (uses project values) */}
      {showRedeployModal && (
        <RedeployModal
          project={project}
          appImage={project.image ?? ""}
          appPort={project.internal_port ?? 3000}
          isStopping={isStopping}
          onRedeploy={handleActionsRedeploy}
          onCancel={() => setShowRedeployModal(false)}
        />
      )}
    </div>
  );
}
