import { useState, useEffect, useRef } from "react";
import { ChevronDown, Terminal, Layers, ChevronRight, Info } from "lucide-react";
import ResourceLimitInput from "../ResourceLimitInput";
import { useToast } from "../ToastContext";
import {
  type Project,
  updateProjectSettings,
  recreateProject,
} from "../../api";

interface AppSettingsPopoverProps {
  project: Project;
  isStopping: boolean;
  onRefresh: () => void;
  onClose: () => void;
  onViewServices?: () => void;
}

export default function AppSettingsPopover({
  project,
  isStopping,
  onRefresh,
  onClose,
  onViewServices,
}: AppSettingsPopoverProps) {
  const ps = project.public_stats;
  const isMultiService = (project.service_count ?? 0) > 1;

  const [appImage, setAppImage] = useState(ps?.image ?? "");
  const [appPort, setAppPort] = useState(ps?.port ?? 3000);
  const [cmd, setCmd] = useState(ps?.cmd ?? "");
  const [memMb, setMemMb] = useState(ps?.memory_limit_mb ?? 256);
  const [cpuLimit, setCpuLimit] = useState(ps?.cpu_limit ?? 0.5);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  // Sync from project prop (e.g. after refresh)
  useEffect(() => {
    setAppImage(ps?.image ?? "");
    setAppPort(ps?.port ?? 3000);
    setCmd(ps?.cmd ?? "");
    setMemMb(ps?.memory_limit_mb ?? 256);
    setCpuLimit(ps?.cpu_limit ?? 0.5);
  }, [ps?.image, ps?.port, ps?.cmd, ps?.memory_limit_mb, ps?.cpu_limit]);

  // Close on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [onClose]);

  const handleSave = async () => {
    const prev = { cmd, memMb, cpuLimit };
    setSettingsError(null);

    try {
      await updateProjectSettings(project.id, { cmd, memory_limit_mb: memMb, cpu_limit: cpuLimit });
      onClose();
      onRefresh();
    } catch (e) {
      setCmd(prev.cmd);
      setMemMb(prev.memMb);
      setCpuLimit(prev.cpuLimit);
      setSettingsError(e instanceof Error ? e.message : "Failed to update settings");
      showToast(e instanceof Error ? e.message : "Failed to update settings");
    }
  };

  const handleSaveAndRecreate = async () => {
    setSettingsError(null);
    setLoading(true);
    try {
      await updateProjectSettings(project.id, { cmd, memory_limit_mb: memMb, cpu_limit: cpuLimit });
      await recreateProject(project.id);
      onClose();
      onRefresh();
    } catch (e) {
      setSettingsError(e instanceof Error ? e.message : "Failed to save and recreate");
      showToast(e instanceof Error ? e.message : "Failed to save and recreate");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div ref={ref}>
      <button
        onClick={(e) => e.stopPropagation()}
        className={`w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer ${
          "bg-slate-900/80 border-violet-500/40 text-slate-300"
        }`}
      >
        <div className="flex items-center gap-1.5">
          <Terminal size={12} />
          <span className="text-[10px] uppercase tracking-wider">App</span>
        </div>
        {loading ? (
          <span className="text-[10px] text-violet-400 animate-pulse">recreating...</span>
        ) : (
          <ChevronDown size={12} className="text-slate-500" />
        )}
      </button>
      <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
        {settingsError && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
            {settingsError}
          </div>
        )}
        <div>
          <span className="text-xs text-slate-400 block mb-1.5">Docker image</span>
          {isMultiService ? (
            <input
              type="text"
              value={appImage}
              readOnly
              className="w-full bg-slate-800 border border-slate-700/50 rounded px-2 py-1.5 text-xs text-slate-500 font-mono cursor-default"
            />
          ) : (
            <input
              type="text"
              value={appImage}
              onChange={(e) => setAppImage(e.target.value)}
              placeholder="nginx:alpine"
              className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
            />
          )}
        </div>
        <div>
          <span className="text-xs text-slate-400 block mb-1.5">App port</span>
          {isMultiService ? (
            <input
              type="number"
              value={appPort}
              readOnly
              className="w-full bg-slate-800 border border-slate-700/50 rounded px-2 py-1.5 text-xs text-slate-500 font-mono cursor-default"
            />
          ) : (
            <input
              type="number"
              min={1}
              max={65535}
              value={appPort}
              onChange={(e) => setAppPort(Number(e.target.value))}
              className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 font-mono focus:outline-none focus:border-violet-500"
            />
          )}
        </div>
        {!isMultiService && (
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
        )}
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
            onClick={handleSave}
            disabled={loading}
            className="flex-1 py-1.5 rounded text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors disabled:opacity-50 cursor-pointer"
          >
            Save
          </button>
          <button
            onClick={handleSaveAndRecreate}
            disabled={loading || isStopping}
            className="flex-1 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            Save & Recreate
          </button>
        </div>
        {isMultiService && (
          <div className="flex items-start gap-1.5 text-[10px] text-slate-500">
            <Info size={10} className="mt-0.5 shrink-0" />
            <span>
              Image and port are managed via compose.yaml. Update it and use Redeploy to apply changes.
            </span>
          </div>
        )}
        {isMultiService && onViewServices && (
          <>
            <div className="border-t border-slate-700/50" />
            <button
              onClick={() => {
                onClose();
                onViewServices();
              }}
              className="w-full flex items-center justify-center gap-1.5 py-2 text-xs font-medium text-violet-300 hover:bg-violet-500/10 rounded transition-colors cursor-pointer"
            >
              <Layers size={12} />
              View all services
              <ChevronRight size={12} />
            </button>
          </>
        )}
      </div>
    </div>
  );
}
