import { Cpu, MemoryStick, HardDrive } from "lucide-react";
import type { ProjectStats, ServiceInfo } from "../../api";
import { formatBytes } from "../../api";

interface ProjectStatsProps {
  stats: ProjectStats | null;
  isRunning: boolean;
  isUnconfigured: boolean;
}

function aggregateFromServices(services: ServiceInfo[]) {
  const running = services.filter((s) => s.status === "running");
  const totalCpu = running.reduce((sum, s) => sum + (s.cpu_percent ?? 0), 0);
  const totalMem = running.reduce((sum, s) => sum + (s.memory_usage ?? 0), 0);
  const totalLimit = running.reduce((sum, s) => sum + ((s.memory_limit_mb ?? 0) * 1024 * 1024), 0);
  const totalDisk = services.reduce((sum, s) => sum + (s.disk_gb ?? 0), 0);
  return { totalCpu, totalMem, totalLimit, totalDisk };
}

export default function ProjectStats({
  stats,
  isRunning,
  isUnconfigured,
}: ProjectStatsProps) {
  if (isUnconfigured) {
    return (
      <div className="mb-4 px-3 py-4 bg-indigo-500/5 border border-indigo-500/15 rounded-md text-center">
        <p className="text-xs text-indigo-300">Awaiting first deploy</p>
        <p className="text-[10px] text-slate-500 mt-1">Deploy via CLI or GitHub Action</p>
      </div>
    );
  }

  const services = stats?.services ?? [];
  const { totalCpu, totalMem, totalLimit, totalDisk } = aggregateFromServices(services);
  const memoryPercent =
    totalLimit > 0
      ? ((totalMem / totalLimit) * 100).toFixed(1)
      : "0";

  return (
    <div className="grid grid-cols-3 gap-3 mb-4">
      <div className="bg-slate-900/50 rounded-md px-3 py-2">
        <div className="flex items-center gap-1.5 text-slate-500 mb-1">
          <Cpu size={12} />
          <span className="text-[10px] uppercase tracking-wider">CPU</span>
        </div>
        <p className="text-sm font-medium text-slate-200">
          {!isRunning ? "—" : `${totalCpu.toFixed(1)}%`}
        </p>
      </div>
      <div className="bg-slate-900/50 rounded-md px-3 py-2" title={totalLimit > 0 ? `${formatBytes(totalMem)}/${formatBytes(totalLimit)}` : undefined}>
        <div className="flex items-center gap-1.5 text-slate-500 mb-1">
          <MemoryStick size={12} />
          <span className="text-[10px] uppercase tracking-wider">Memory</span>
        </div>
        <p className="text-sm font-medium text-slate-200">
          {totalMem > 0 ? `${formatBytes(totalMem)}` : "—"}
        </p>
        {totalLimit > 0 && (
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
          {totalDisk > 0 ? `${totalDisk.toFixed(2)} GB` : "—"}
        </p>
      </div>
    </div>
  );
}
