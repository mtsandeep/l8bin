import { useState } from "react";
import {
  X,
  Play,
  Square,
  RotateCcw,
  Loader2,
  Cpu,
  MemoryStick,
  HardDrive,
  Database,
} from "lucide-react";
import type { Project, ServiceInfo } from "../../api";
import { formatBytes, startService, stopService, restartService } from "../../api";

interface ServicesModalProps {
  project: Project;
  services: ServiceInfo[];
  onClose: () => void;
  onRefresh: () => void;
}

function shortImage(image: string): string {
  if (!image) return "—";
  const hash = image.startsWith("sha256:") ? image.slice(7) : image;
  return hash.length > 12 ? hash.slice(0, 12) : hash;
}

export default function ServicesModal({
  project,
  services,
  onClose,
  onRefresh,
}: ServicesModalProps) {
  const [loadingService, setLoadingService] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleAction = (name: string, fn: () => Promise<void>) => {
    setLoadingService(name);
    setError(null);
    fn()
      .then(() => onRefresh())
      .catch((err) =>
        setError(err instanceof Error ? err.message : "Action failed"),
      )
      .finally(() => setLoadingService(null));
  };

  const runningCount = services.filter((s) => s.status === "running").length;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-lg mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold text-slate-100">
              {project.name || project.id}
            </span>
            <span className="text-xs text-slate-500">
              {runningCount}/{services.length} running
            </span>
          </div>
          <button
            onClick={onClose}
            className="text-slate-400 hover:text-slate-200 transition-colors p-1 cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="overflow-y-auto px-5 py-4 space-y-3">
          {error && (
            <div className="px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
              {error}
            </div>
          )}

          {services.map((svc) => {
            const isLoading = loadingService === svc.service_name;
            const isRunning = svc.status === "running";
            const memPercent =
              svc.memory_limit && svc.memory_usage
                ? (svc.memory_usage / svc.memory_limit) * 100
                : 0;

            return (
              <div
                key={svc.service_name}
                className="bg-slate-900/60 border border-slate-700/40 rounded-lg p-3">
                {/* Service header */}
                <div className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-1.5 min-w-0">
                    <span className="text-xs font-medium text-slate-200 truncate">
                      {svc.service_name}
                    </span>
                    {svc.is_public && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-sky-500/20 text-sky-400">
                        public
                      </span>
                    )}
                    <span
                      className={`text-[10px] px-1.5 py-0.5 rounded ${
                        isRunning
                          ? "bg-emerald-500/20 text-emerald-400"
                          : "bg-slate-700/60 text-slate-500"
                      }`}>
                      {svc.status}
                    </span>
                  </div>
                  <div className="flex items-center gap-1 shrink-0">
                    {isRunning ? (
                      <>
                        <button
                          onClick={() =>
                            handleAction(svc.service_name, () =>
                              stopService(project.id, svc.service_name),
                            )
                          }
                          disabled={isLoading}
                          className="text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50 cursor-pointer p-1"
                          title="Stop">
                          {isLoading ? (
                            <Loader2 size={12} className="animate-spin" />
                          ) : (
                            <Square size={12} />
                          )}
                        </button>
                        <button
                          onClick={() =>
                            handleAction(svc.service_name, () =>
                              restartService(project.id, svc.service_name),
                            )
                          }
                          disabled={isLoading}
                          className="text-slate-500 hover:text-violet-400 hover:bg-violet-500/10 transition-colors disabled:opacity-50 cursor-pointer p-1"
                          title="Restart">
                          <RotateCcw size={12} />
                        </button>
                      </>
                    ) : (
                      <button
                        onClick={() =>
                          handleAction(svc.service_name, () =>
                            startService(project.id, svc.service_name),
                          )
                        }
                        disabled={isLoading}
                        className="text-slate-500 hover:text-emerald-400 hover:bg-emerald-500/10 transition-colors disabled:opacity-50 cursor-pointer p-1"
                        title="Start">
                        {isLoading ? (
                          <Loader2 size={12} className="animate-spin" />
                        ) : (
                          <Play size={12} />
                        )}
                      </button>
                    )}
                  </div>
                </div>

                {/* Image + port */}
                <p
                  className="text-[10px] text-slate-500 truncate font-mono mt-1.5"
                  title={svc.image}>
                  {shortImage(svc.image)}
                  {svc.port
                    ? svc.mapped_port && svc.mapped_port > 0
                      ? ` | ${svc.mapped_port}:${svc.port}`
                      : ` | :${svc.port}`
                    : ""}
                </p>

                {/* Stats grid */}
                {(isRunning ||
                  svc.cpu_limit !== undefined ||
                  svc.memory_limit !== undefined ||
                  svc.disk_gb !== undefined) && (
                  <div className="grid grid-cols-3 gap-2 mt-2">
                    {/* CPU */}
                    <div className="bg-slate-800/60 rounded px-2 py-1.5">
                      <div className="flex items-center gap-1 text-slate-500 mb-0.5">
                        <Cpu size={10} />
                        <span className="text-[9px] uppercase tracking-wider">
                          CPU
                        </span>
                      </div>
                      {isRunning && svc.cpu_percent !== undefined ? (
                        <p className="text-[11px] font-medium text-slate-300">
                          {svc.cpu_percent.toFixed(1)}
                          <span className="text-slate-500">%</span>
                        </p>
                      ) : (
                        <p className="text-[11px] text-slate-600">—</p>
                      )}
                      {svc.cpu_limit !== undefined && (
                        <p className="text-[9px] text-slate-600">
                          limit {svc.cpu_limit.toFixed(1)}
                        </p>
                      )}
                    </div>

                    {/* Memory */}
                    <div className="bg-slate-800/60 rounded px-2 py-1.5">
                      <div className="flex items-center gap-1 text-slate-500 mb-0.5">
                        <MemoryStick size={10} />
                        <span className="text-[9px] uppercase tracking-wider">
                          Mem
                        </span>
                      </div>
                      {isRunning && svc.memory_usage !== undefined ? (
                        <p className="text-[11px] font-medium text-slate-300">
                          {formatBytes(svc.memory_usage)}
                        </p>
                      ) : (
                        <p className="text-[11px] text-slate-600">—</p>
                      )}
                      {svc.memory_limit !== undefined && (
                        <p className="text-[9px] text-slate-600">
                          / {formatBytes(svc.memory_limit)}
                        </p>
                      )}
                      {isRunning && svc.memory_limit && memPercent > 0 && (
                        <div className="mt-1 h-0.5 bg-slate-700 rounded-full overflow-hidden">
                          <div
                            className="h-full bg-violet-500 rounded-full"
                            style={{
                              width: `${Math.min(memPercent, 100)}%`,
                            }}
                          />
                        </div>
                      )}
                    </div>

                    {/* Disk */}
                    <div className="bg-slate-800/60 rounded px-2 py-1.5">
                      <div className="flex items-center gap-1 text-slate-500 mb-0.5">
                        <HardDrive size={10} />
                        <span className="text-[9px] uppercase tracking-wider">
                          Disk
                        </span>
                      </div>
                      {svc.disk_gb !== undefined && svc.disk_gb > 0 ? (
                        <p className="text-[11px] font-medium text-slate-300">
                          {svc.disk_gb.toFixed(2)}
                          <span className="text-slate-500"> GB</span>
                        </p>
                      ) : (
                        <p className="text-[11px] text-slate-600">—</p>
                      )}
                    </div>
                  </div>
                )}

                {/* Volumes */}
                {svc.volumes && svc.volumes.length > 0 && (
                  <div className="mt-2 flex items-start gap-1.5">
                    <Database
                      size={10}
                      className="text-slate-600 mt-0.5 shrink-0"
                    />
                    <div className="text-[10px] font-mono">
                      {svc.volumes.map((v, i) => (
                        <span
                          key={i}
                          className="inline-block mr-3 last:mr-0">
                          <span className="text-slate-500">
                            {v.container_path}
                          </span>
                          {v.volume_name && (
                            <span className="text-slate-600">
                              {" "}
                              &larr; {v.volume_name}
                            </span>
                          )}
                        </span>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
