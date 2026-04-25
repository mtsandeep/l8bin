import { useState, useEffect, useRef } from "react";
import { Info } from "lucide-react";
import ResourceLimitInput from "../ResourceLimitInput";
import { useToast } from "../ToastContext";
import {
  type ServiceInfo,
  updateServiceSettings,
  recreateProject,
} from "../../api";

interface ServiceSettingsPopoverProps {
  projectId: string;
  service: ServiceInfo;
  onRefresh: () => void;
  onClose: () => void;
}

export default function ServiceSettingsPopover({
  projectId,
  service,
  onRefresh,
  onClose,
}: ServiceSettingsPopoverProps) {
  const [memMb, setMemMb] = useState(service.memory_limit_mb ?? 256);
  const [cpuLimit, setCpuLimit] = useState(service.cpu_limit ?? 0.5);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  // Sync from service prop
  useEffect(() => {
    setMemMb(service.memory_limit_mb ?? 256);
    setCpuLimit(service.cpu_limit ?? 0.5);
  }, [service.memory_limit_mb, service.cpu_limit]);

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
    setError(null);
    try {
      await updateServiceSettings(projectId, service.service_name, {
        memory_limit_mb: memMb,
        cpu_limit: cpuLimit,
      });
      onClose();
      onRefresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to update settings");
      showToast(e instanceof Error ? e.message : "Failed to update settings");
    }
  };

  const handleSaveAndRecreate = async () => {
    setError(null);
    setLoading(true);
    try {
      await updateServiceSettings(projectId, service.service_name, {
        memory_limit_mb: memMb,
        cpu_limit: cpuLimit,
      });
      await recreateProject(projectId, [service.service_name]);
      onClose();
      onRefresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to save and recreate");
      showToast(e instanceof Error ? e.message : "Failed to save and recreate");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div ref={ref} className="absolute left-0 right-0 top-full mt-1 z-30 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl max-w-sm px-3 py-3 space-y-3">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
          {error}
        </div>
      )}

      <div>
        <span className="text-[10px] text-slate-500 block mb-1">Image</span>
        <input
          type="text"
          value={service.image ? (service.image.length > 20 ? service.image.slice(0, 20) + "..." : service.image) : "—"}
          readOnly
          className="w-full bg-slate-800 border border-slate-700/50 rounded px-2 py-1 text-[11px] text-slate-500 font-mono cursor-default"
          title={service.image}
        />
      </div>

      <div>
        <span className="text-[10px] text-slate-500 block mb-1">Port</span>
        <input
          type="text"
          value={service.port ? `${service.mapped_port ?? ""}:${service.port}` : "—"}
          readOnly
          className="w-full bg-slate-800 border border-slate-700/50 rounded px-2 py-1 text-[11px] text-slate-500 font-mono cursor-default"
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
          onClick={handleSave}
          disabled={loading}
          className="flex-1 py-1.5 rounded text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors disabled:opacity-50 cursor-pointer"
        >
          Save
        </button>
        <button
          onClick={handleSaveAndRecreate}
          disabled={loading}
          className="flex-1 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
        >
          {loading ? "Recreating..." : "Save & Recreate"}
        </button>
      </div>

      <div className="flex items-start gap-1.5 text-[10px] text-slate-500">
        <Info size={10} className="mt-0.5 shrink-0" />
        <span>
          Image and port are managed via compose.yaml
        </span>
      </div>
    </div>
  );
}
