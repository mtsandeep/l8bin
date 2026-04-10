import { useState, useEffect, useRef } from "react";
import {
  Play,
  Square,
  Trash2,
  Cpu,
  MemoryStick,
  Clock,
  ExternalLink,
  Loader2,
  ScrollText,
  RotateCcw,
  RefreshCw,
  MoreHorizontal,
  Copy,
  ChevronDown,
  ChevronRight,
  Moon,
  Terminal,
  HardDrive,
  Settings,
  X as XIcon,
} from "lucide-react";
import StatusBadge from "./StatusBadge";
import LogViewer from "./LogViewer";
import ResourceLimitInput from "./ResourceLimitInput";
import { useToast } from "./ToastContext";
import {
  type Project,
  type Node as ApiNode,
  type ProjectStats,
  stopProject,
  startProject,
  deleteProject,
  redeployProject,
  recreateProject,
  updateProjectSettings,
  formatBytes,
  timeAgo,
} from "../api";

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
  const [sleepOpen, setSleepOpen] = useState(false);
  const [cmdOpen, setCmdOpen] = useState(false);
  const [envCopied, setEnvCopied] = useState(false);
  const popoverContainerRef = useRef<HTMLDivElement>(null);
  const actionsRef = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  // Local sleep settings state for optimistic updates
  const [autoStop, setAutoStop] = useState(project.auto_stop_enabled);
  const [timeoutMins, setTimeoutMins] = useState(
    project.auto_stop_timeout_mins,
  );
  const [autoStart, setAutoStart] = useState(project.auto_start_enabled);
  const [cmd, setCmd] = useState(project.cmd ?? "");
  const [appImage, setAppImage] = useState(project.image ?? "");
  const [appPort, setAppPort] = useState(project.internal_port ?? 3000);
  const [memMb, setMemMb] = useState(project.memory_limit_mb ?? 256);
  const [cpuLimit, setCpuLimit] = useState(project.cpu_limit ?? 0.5);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [projectName, setProjectName] = useState(project.name ?? "");
  const [projectDescription, setProjectDescription] = useState(project.description ?? "");
  const [customDomainInput, setCustomDomainInput] = useState(project.custom_domain ?? "");
  const [customDomainSaving, setCustomDomainSaving] = useState(false);
  const [showCustomDomain, setShowCustomDomain] = useState(false);
  const sleepStateRef = useRef({ autoStop, timeoutMins, autoStart });

  // Keep local state in sync when project prop changes (e.g. after refresh)
  useEffect(() => {
    setAutoStop(project.auto_stop_enabled);
    setTimeoutMins(project.auto_stop_timeout_mins);
    setAutoStart(project.auto_start_enabled);
    setCmd(project.cmd ?? "");
    setAppImage(project.image ?? "");
    setAppPort(project.internal_port ?? 3000);
    setMemMb(project.memory_limit_mb ?? 256);
    setCpuLimit(project.cpu_limit ?? 0.5);
    setProjectName(project.name ?? "");
    setProjectDescription(project.description ?? "");
    setCustomDomainInput(project.custom_domain ?? "");
  }, [
    project.auto_stop_enabled,
    project.auto_stop_timeout_mins,
    project.auto_start_enabled,
    project.cmd,
    project.image,
    project.internal_port,
    project.memory_limit_mb,
    project.cpu_limit,
    project.custom_domain,
    project.name,
    project.description,
  ]);

  // Close custom domain popover on outside click
  useEffect(() => {
    if (!showCustomDomain) return;
    const handler = (e: MouseEvent) => {
      if (popoverContainerRef.current && !popoverContainerRef.current.contains(e.target as Node)) {
        setShowCustomDomain(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showCustomDomain]);

  // Close sleep popover on outside click — save on close
  useEffect(() => {
    if (!sleepOpen) return;
    const handler = (e: MouseEvent) => {
      if (popoverContainerRef.current && !popoverContainerRef.current.contains(e.target as Node)) {
        setSleepOpen(false);
        // Build patch from changed values
        const snap = sleepStateRef.current;
        const patch: Parameters<typeof handleSettingsChange>[0] = {};
        if (autoStop !== snap.autoStop) patch.auto_stop_enabled = autoStop;
        if (timeoutMins !== snap.timeoutMins) patch.auto_stop_timeout_mins = timeoutMins;
        if (autoStart !== snap.autoStart) patch.auto_start_enabled = autoStart;
        if (Object.keys(patch).length > 0) handleSettingsChange(patch);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [sleepOpen, autoStop, timeoutMins, autoStart]);

  // Close cmd popover on outside click
  useEffect(() => {
    if (!cmdOpen) return;
    const handler = (e: MouseEvent) => {
      if (popoverContainerRef.current && !popoverContainerRef.current.contains(e.target as Node)) {
        setCmdOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [cmdOpen]);

  // Close actions dropdown on outside click
  useEffect(() => {
    if (!showActions) return;
    const handler = (e: MouseEvent) => {
      if (actionsRef.current && !actionsRef.current.contains(e.target as unknown as globalThis.Node)) {
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

  const handleSettingsChange = async (patch: {
    auto_stop_enabled?: boolean;
    auto_stop_timeout_mins?: number;
    auto_start_enabled?: boolean;
    cmd?: string;
    memory_limit_mb?: number | null;
    cpu_limit?: number | null;
  }) => {
    // Snapshot current values for revert
    const prev = { autoStop, timeoutMins, autoStart, cmd, memMb, cpuLimit };

    // Optimistic update
    if (patch.auto_stop_enabled !== undefined)
      setAutoStop(patch.auto_stop_enabled);
    if (patch.auto_stop_timeout_mins !== undefined)
      setTimeoutMins(patch.auto_stop_timeout_mins);
    if (patch.auto_start_enabled !== undefined)
      setAutoStart(patch.auto_start_enabled);
    if (patch.cmd !== undefined) setCmd(patch.cmd);
    if (patch.memory_limit_mb !== undefined) setMemMb(patch.memory_limit_mb ?? 256);
    if (patch.cpu_limit !== undefined) setCpuLimit(patch.cpu_limit ?? 0.5);
    setSettingsError(null);

    try {
      await updateProjectSettings(project.id, patch);
    } catch (e) {
      // Revert on failure
      setAutoStop(prev.autoStop);
      setTimeoutMins(prev.timeoutMins);
      setAutoStart(prev.autoStart);
      setCmd(prev.cmd);
      setMemMb(prev.memMb);
      setCpuLimit(prev.cpuLimit);
      setSettingsError(
        e instanceof Error ? e.message : "Failed to update settings",
      );
      showToast(e instanceof Error ? e.message : "Failed to update settings");
    }
  };

  const isRunning = project.status === "running";
  const isStopped = project.status === "stopped";
  const isStopping = project.status === "stopping";
  const isUnconfigured = project.status === "unconfigured" || (project.status === "stopped" && !project.image);
  const memoryPercent =
    stats && stats.memory_limit > 0
      ? ((stats.memory_usage / stats.memory_limit) * 100).toFixed(1)
      : "0";

  return (
    <div className="relative bg-slate-800/50 border border-slate-700/50 rounded-lg p-5 hover:border-slate-600/50 transition-colors">
      {/* Header */}
      <div className="flex items-start justify-between mb-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 mb-1">
            <h3 className="text-sm font-semibold text-slate-100 truncate" title={project.description || undefined}>
              {project.name || project.id}
            </h3>
            <StatusBadge status={project.status} />
          </div>
          <p className="text-xs text-slate-500 truncate font-mono" title={project.image ?? ""}>
            {shortImage(project.image)}{project.mapped_port ? ` | port: ${project.mapped_port}` : ""}
          </p>
          <div className="flex items-center gap-1.5 mt-1.5 text-[10px]">
            <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full ${
              autoStop ? 'bg-slate-700/60' : 'bg-slate-800/40'
            }`}>
              <span className={`w-1.5 h-1.5 rounded-full ${autoStop ? 'bg-emerald-400' : 'bg-slate-600'}`} />
              <span className="text-slate-400">Auto-stop{autoStop && ` · ${timeoutMins}m`}</span>
            </span>
            <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full ${
              autoStart ? 'bg-slate-700/60' : 'bg-slate-800/40'
            }`}>
              <span className={`w-1.5 h-1.5 rounded-full ${autoStart ? 'bg-emerald-400' : 'bg-slate-600'}`} />
              <span className="text-slate-400">Auto-start</span>
            </span>
          </div>
        </div>
        <div className="flex items-center gap-1 ml-2 flex-shrink-0">
          <a
            href={`https://${project.custom_domain || `${project.id}.${domain}`}`}
            target="_blank"
            rel="noopener noreferrer"
            className="text-slate-400 hover:text-sky-400 transition-colors"
            title="Open app"
          >
            <ExternalLink size={14} />
          </a>
        </div>
      </div>

      {/* Stats or pending message */}
      {isUnconfigured ? (
        <div className="mb-4 px-3 py-4 bg-indigo-500/5 border border-indigo-500/15 rounded-md text-center">
          <p className="text-xs text-indigo-300">Awaiting first deploy</p>
          <p className="text-[10px] text-slate-500 mt-1">Deploy via CLI or GitHub Action</p>
        </div>
      ) : (
      <div className="grid grid-cols-3 gap-3 mb-4">
        <div className="bg-slate-900/50 rounded-md px-3 py-2">
          <div className="flex items-center gap-1.5 text-slate-500 mb-1">
            <Cpu size={12} />
            <span className="text-[10px] uppercase tracking-wider">CPU</span>
          </div>
          <p className="text-sm font-medium text-slate-200">
            {isRunning && stats ? `${stats.cpu_percent}%` : "—"}
          </p>
        </div>
        <div className="bg-slate-900/50 rounded-md px-3 py-2" title={isRunning && stats && stats.memory_limit > 0 ? `${formatBytes(stats.memory_usage)}/${formatBytes(stats.memory_limit)}` : undefined}>
          <div className="flex items-center gap-1.5 text-slate-500 mb-1">
            <MemoryStick size={12} />
            <span className="text-[10px] uppercase tracking-wider">Memory</span>
          </div>
          <p className="text-sm font-medium text-slate-200">
            {isRunning && stats ? `${formatBytes(stats.memory_usage)}` : "—"}
          </p>
          {isRunning && stats && stats.memory_limit > 0 && (
            <div className="mt-1.5 h-1 bg-slate-700 rounded-full overflow-hidden">
              <div
                className="h-full bg-violet-500 rounded-full transition-all"
                style={{
                  width: `${Math.min(parseFloat(memoryPercent), 100)}%`,
                }}
              />
            </div>
          )}
        </div>
        <div className="bg-slate-900/50 rounded-md px-3 py-2">
          <div className="flex items-center gap-1.5 text-slate-500 mb-1">
            <HardDrive size={12}/>
            <span className="text-[10px] uppercase tracking-wider">Disk</span>
          </div>
          <p className="text-sm font-medium text-slate-200">
            {isRunning && stats && stats.disk_gb > 0 ? `${stats.disk_gb.toFixed(2)} GB` : "—"}
          </p>
        </div>
      </div>
      )}

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

      {/* Sleep Settings + CMD + Domain popovers */}
      <div ref={popoverContainerRef} className="relative mb-4">
        <div className="flex gap-2">
          {/* Sleep button */}
          <div className="flex-1">
            <button
              onClick={() => {
                setSleepOpen((o) => {
                  if (!o) sleepStateRef.current = { autoStop, timeoutMins, autoStart };
                  return !o;
                });
                setCmdOpen(false);
                setShowCustomDomain(false);
              }}
              className={`w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer ${
                sleepOpen
                  ? "bg-slate-900/80 border-violet-500/40 text-slate-300"
                  : "bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80"
              }`}
            >
              <div className="flex items-center gap-1.5">
                <Moon size={12} />
                <span className="text-[10px] uppercase tracking-wider">Sleep</span>
              </div>
              {sleepOpen ? <ChevronDown size={12} className="text-slate-500" /> : <ChevronRight size={12} className="text-slate-500" />}
            </button>
          </div>

          {/* App Settings button */}
          <div className="flex-1">
            <button
              onClick={() => { setCmdOpen((o) => !o); setSleepOpen(false); setShowCustomDomain(false); }}
              className={`w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer ${
                cmdOpen
                  ? "bg-slate-900/80 border-violet-500/40 text-slate-300"
                  : cmd || appImage !== project.image || appPort !== project.internal_port
                    ? "bg-slate-900/50 border-violet-500/20 text-violet-400 hover:bg-slate-900/80"
                    : "bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80"
              }`}
            >
              <div className="flex items-center gap-1.5">
                <Terminal size={12} />
                <span className="text-[10px] uppercase tracking-wider">App</span>
              </div>
              {cmdOpen ? <ChevronDown size={12} className="text-slate-500" /> : <ChevronRight size={12} className="text-slate-500" />}
            </button>
          </div>

          {/* Settings gear button */}
          <div className="flex-none">
            <button
              onClick={() => { setShowCustomDomain((o) => !o); setSleepOpen(false); setCmdOpen(false); }}
              className={`flex items-center justify-center px-2.5 py-2 rounded-md border transition-colors cursor-pointer ${
                showCustomDomain
                  ? "bg-slate-900/80 border-violet-500/40 text-slate-300"
                  : project.custom_domain || project.name
                    ? "bg-slate-900/50 border-violet-500/20 text-violet-400 hover:bg-slate-900/80"
                    : "bg-slate-900/50 border-slate-700/50 text-slate-400 hover:bg-slate-900/80"
              }`}
              title="Project settings"
            >
              <Settings size={12} />
            </button>
          </div>
        </div>

        {/* Sleep popover — full card width */}
        {sleepOpen && (
          <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
            {settingsError && (
              <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
                {settingsError}
              </div>
            )}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-stop when idle</span>
              <button
                role="switch"
                aria-checked={autoStop}
                onClick={() => setAutoStop(!autoStop)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStop ? "bg-violet-500" : "bg-slate-600"}`}
              >
                <span className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStop ? "translate-x-3.5" : "translate-x-0.5"}`} />
              </button>
            </label>
            {autoStop && (
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs text-slate-300">Idle timeout (mins)</span>
                <input
                  type="number"
                  min={1}
                  value={timeoutMins}
                  onChange={(e) => setTimeoutMins(Number(e.target.value))}
                  className="w-16 bg-slate-700 border border-slate-600 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500"
                />
              </div>
            )}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-start on visit</span>
              <button
                role="switch"
                aria-checked={autoStart}
                onClick={() => setAutoStart(!autoStart)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStart ? "bg-violet-500" : "bg-slate-600"}`}
              >
                <span className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStart ? "translate-x-3.5" : "translate-x-0.5"}`} />
              </button>
            </label>
          </div>
        )}

        {/* App Settings popover — full card width */}
        {cmdOpen && (
          <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
            {settingsError && (
              <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
                {settingsError}
              </div>
            )}
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Docker image</span>
              <input
                type="text"
                value={appImage}
                onChange={(e) => setAppImage(e.target.value)}
                placeholder="nginx:alpine"
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">App port</span>
              <input
                type="number"
                min={1}
                max={65535}
                value={appPort}
                onChange={(e) => setAppPort(Number(e.target.value))}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Command override</span>
              <input
                type="text"
                value={cmd}
                onChange={(e) => setCmd(e.target.value)}
                placeholder="default (image entrypoint)"
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <ResourceLimitInput
              label="Memory limit"
              value={memMb}
              onChange={setMemMb}
              unit="MB"
              min={64}
              normalMax={4096}
              absoluteMax={65536}
              normalStep={64}
              overStep={1024}
              highLabel="high memory"
              minLabel="64 MB"
              normalMaxLabel="4 GB"
              inputClass="bg-slate-700"
            />
            <ResourceLimitInput
              label="CPU limit"
              value={cpuLimit}
              onChange={setCpuLimit}
              unit="vCPU"
              min={0.1}
              normalMax={4}
              absoluteMax={32}
              normalStep={0.1}
              overStep={1}
              highLabel="high cpu"
              minLabel="0.1"
              normalMaxLabel="4 vCPU"
              inputClass="bg-slate-700"
            />
            <div className="flex gap-2">
              <button
                onClick={async () => {
                  await handleSettingsChange({ cmd, memory_limit_mb: memMb, cpu_limit: cpuLimit });
                  setCmdOpen(false);
                }}
                className="flex-1 py-1.5 rounded text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Save
              </button>
              <button
                onClick={async () => {
                  await handleSettingsChange({ cmd, memory_limit_mb: memMb, cpu_limit: cpuLimit });
                  setCmdOpen(false);
                  handleAction("redeploy", () => redeployProject(project.id, appImage, appPort, cmd, memMb, cpuLimit));
                }}
                disabled={loading !== null || isStopping || !appImage.trim()}
                className="flex-1 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                Save & Redeploy
              </button>
            </div>
          </div>
        )}

        {/* Settings popover — full card width */}
        {showCustomDomain && (
          <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
            {settingsError && (
              <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
                {settingsError}
              </div>
            )}
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Project name</span>
              <input
                type="text"
                value={projectName}
                onChange={(e) => setProjectName(e.target.value)}
                placeholder={project.id}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <span className="text-xs text-slate-400 block mb-1.5">Description</span>
              <input
                type="text"
                value={projectDescription}
                onChange={(e) => setProjectDescription(e.target.value)}
                placeholder="What this app does"
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
              />
            </div>
            <button
              onClick={async () => {
                setSettingsError(null);
                try {
                  await updateProjectSettings(project.id, {
                    name: projectName,
                    description: projectDescription,
                  });
                  onRefresh();
                } catch (e) {
                  setSettingsError(e instanceof Error ? e.message : "Failed to save");
                  showToast(e instanceof Error ? e.message : "Failed to save");
                }
              }}
              className="w-full py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
            >
              Save
            </button>

            <div className="border-t border-slate-700/50 pt-3 space-y-2">
              <div className="text-[11px] text-slate-500">
                Subdomain: <span className="text-slate-300 font-mono">{project.id}.{domain}</span>
              </div>
              <div className="flex items-center gap-1.5">
                <input
                  type="text"
                  value={customDomainInput}
                  onChange={(e) => setCustomDomainInput(e.target.value)}
                  placeholder="app.example.com"
                  className="flex-1 bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
                />
                {project.custom_domain && (
                  <button
                    onClick={async () => {
                      setCustomDomainSaving(true);
                      setSettingsError(null);
                      try {
                        await updateProjectSettings(project.id, { custom_domain: "" });
                        setCustomDomainInput("");
                        onRefresh();
                      } catch (e) {
                        setSettingsError(e instanceof Error ? e.message : "Failed to remove domain");
                        showToast(e instanceof Error ? e.message : "Failed to remove domain");
                      } finally {
                        setCustomDomainSaving(false);
                      }
                    }}
                    disabled={customDomainSaving}
                    className="p-1.5 text-slate-400 hover:text-red-400 transition-colors cursor-pointer disabled:opacity-50"
                    title="Remove custom domain"
                  >
                    <XIcon size={14} />
                  </button>
                )}
                <button
                  onClick={async () => {
                    const d = customDomainInput.trim();
                    if (!d) return;
                    setCustomDomainSaving(true);
                    setSettingsError(null);
                    try {
                      await updateProjectSettings(project.id, { custom_domain: d });
                      onRefresh();
                    } catch (e) {
                      setSettingsError(e instanceof Error ? e.message : "Failed to set domain");
                      showToast(e instanceof Error ? e.message : "Failed to set domain");
                    } finally {
                      setCustomDomainSaving(false);
                    }
                  }}
                  disabled={customDomainSaving || !customDomainInput.trim()}
                  className="px-2.5 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {customDomainSaving ? <Loader2 size={12} className="animate-spin" /> : "Set"}
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
                          <div>A record <span className="text-slate-400 font-mono">{cd}</span> → <span className="text-slate-400 font-mono">{dnsTarget}</span></div>
                        )}
                        <div>CNAME <span className="text-slate-400 font-mono">{cd}</span> → <span className="text-slate-400 font-mono">{project.id}.{domain}</span> <span className="text-slate-500">(Cloudflare only)</span></div>
                      </>
                    );
                  }
                  return (
                    <div>CNAME <span className="text-slate-400 font-mono">{cd}</span> → <span className="text-slate-400 font-mono">{project.id}.{domain}</span></div>
                  );
                })()}
              </div>
            </div>
          </div>
        )}
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
            title="Logs"
          >
            <ScrollText size={13} />
          </button>
          <div ref={actionsRef} className="relative">
            <button
              onClick={() => setShowActions(!showActions)}
              disabled={loading !== null || isStopping}
              className="inline-flex items-center gap-1 p-1.5 rounded-md text-slate-500 hover:text-slate-200 hover:bg-slate-800 transition-colors disabled:opacity-50 cursor-pointer"
              title="Actions"
            >
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
                    handleAction("redeploy", () =>
                      redeployProject(project.id, appImage, appPort, cmd, memMb, cpuLimit),
                    );
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer"
                >
                  <RotateCcw size={13} className="text-sky-400" />
                  <div className="text-left">
                    <div>Redeploy</div>
                    <div className="text-[10px] text-slate-500">Pull latest image &amp; restart</div>
                  </div>
                </button>
                <button
                  onClick={() => {
                    setShowActions(false);
                    handleAction("recreate", () =>
                      recreateProject(project.id).then(() => onRefresh()),
                    );
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-xs text-slate-300 hover:bg-slate-700/50 transition-colors cursor-pointer"
                >
                  <RefreshCw size={13} className="text-emerald-400" />
                  <div className="text-left">
                    <div>Recreate</div>
                    <div className="text-[10px] text-slate-500">Restart with updated env/config</div>
                  </div>
                </button>
                <div className="mx-2 my-1 border-t border-slate-700/50" />
                  </>
                )}
                <div className="px-3 py-2">
                  <div className="flex items-center justify-between mb-1.5">
                    <span className="text-[10px] text-slate-500">Env file on node:</span>
                    <button
                      onClick={() => {
                        navigator.clipboard.writeText(`${projectsDir}/${project.id}/.env`);
                        setEnvCopied(true);
                        setTimeout(() => setEnvCopied(false), 1500);
                      }}
                      className="rounded text-slate-500 hover:text-slate-300 transition-colors flex cursor-pointer"
                    >
                      {envCopied ? (
                        <span className="text-[10px] text-emerald-400">Copied</span>
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
                  {project.node_id && project.node_id !== "local" && (() => {
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
              title="Start"
            >
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
                // fire and forget — status will update via polling
                stopProject(project.id).catch((e) => {
                  console.error(e);
                  showToast(e instanceof Error ? e.message : "Stop failed");
                });
                onRefresh();
              }}
              className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-amber-500/10 text-amber-400 hover:bg-amber-500/20 transition-colors cursor-pointer"
              title="Stop"
            >
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
                className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-xs font-medium bg-red-500/20 text-red-400 hover:bg-red-500/30 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {loading === "delete" ? (
                  <Loader2 size={12} className="animate-spin" />
                ) : (
                  "Confirm"
                )}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(false)}
                className="px-2 py-1.5 rounded-md text-xs text-slate-400 hover:text-slate-300 transition-colors cursor-pointer"
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              onClick={() => setShowDeleteConfirm(true)}
              disabled={loading !== null}
              className="inline-flex items-center gap-1 p-1.5 rounded-md text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50 cursor-pointer"
              title="Delete"
            >
              <Trash2 size={13} />
            </button>
          )}
        </div>
      </div>
      {showLogs && (
        <LogViewer projectId={project.id} onClose={() => setShowLogs(false)} />
      )}
    </div>
  );
}
