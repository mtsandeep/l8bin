import { Cpu, MemoryStick, HardDrive } from "lucide-react";
import type { ProjectStats } from "../../api";
import { formatBytes } from "../../api";

interface ProjectStatsProps {
  stats: ProjectStats | null;
  isRunning: boolean;
  isUnconfigured: boolean;
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

  const memoryPercent =
    stats && stats.memory_limit > 0
      ? ((stats.memory_usage / stats.memory_limit) * 100).toFixed(1)
      : "0";

  return (
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
          {stats && stats.disk_gb > 0 ? `${stats.disk_gb.toFixed(2)} GB` : "—"}
        </p>
      </div>
    </div>
  );
}
