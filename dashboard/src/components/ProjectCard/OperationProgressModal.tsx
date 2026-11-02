import { AlertCircle, CheckCircle2, ExternalLink, Loader2, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { ProjectStatus, fetchAllStats, fetchDeployLogs } from '../../api';

interface Props {
  /** Title; " failed" is appended when the operation errors. */
  title: string;
  /** Project to watch (the new copy for duplicate, the project itself for migrate). */
  projectId: string;
  domain: string;
  /** Statuses that count as success (Running, or also Stopped when not auto-starting). */
  successStatuses: ProjectStatus[];
  /** Warnings shown under the result. */
  warnings?: string[];
  /** Disable the URL row (background projects). */
  noUrl?: boolean;
  onClose: (done: boolean) => void;
}

export default function OperationProgressModal({
  title,
  projectId,
  domain,
  successStatuses,
  warnings = [],
  noUrl,
  onClose,
}: Props) {
  const [status, setStatus] = useState<ProjectStatus | null>(null);
  const [lines, setLines] = useState<string[]>([]);
  const url = `https://${projectId}.${domain}`;

  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      try {
        const stats = await fetchAllStats();
        const proj = stats.find((s) => s.project_id === projectId);
        if (proj && !cancelled) setStatus(proj.status);
      } catch {}
    };
    check();
    const interval = setInterval(check, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [projectId]);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const data = await fetchDeployLogs(projectId);
        if (!cancelled) setLines(data.lines);
      } catch {}
    };
    load();
    const interval = setInterval(load, 2000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [projectId]);

  const isSuccess = status !== null && successStatuses.includes(status);
  const isError = status === ProjectStatus.Error;
  const isDone = isSuccess || isError;
  const errorTail = isDone && isError ? lines.slice(-4) : [];

  const handleClose = () => onClose(isDone);

  return (
    <div
      data-l8b-modal
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    >
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg p-5 max-w-md w-full mx-4 shadow-xl max-h-[90vh] overflow-y-auto">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            {isDone &&
              (isError ? (
                <AlertCircle size={14} className="text-red-400" />
              ) : (
                <CheckCircle2 size={14} className="text-emerald-400" />
              ))}
            <h3 className="text-sm font-semibold text-slate-100">
              {isDone ? (isError ? `${title} failed` : title) : title}
            </h3>
          </div>
          <button
            type="button"
            onClick={handleClose}
            className="p-1 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
          >
            <X size={14} />
          </button>
        </div>

        {!isDone && (
          <div className="py-8 flex flex-col items-center gap-3">
            <Loader2 size={22} className="animate-spin text-sky-400" />
            <p className="text-xs text-slate-400">
              Setting up <span className="font-mono text-slate-300">{projectId}</span>
            </p>
          </div>
        )}

        {isDone && (
          <div className="space-y-3">
            <div className="rounded-md bg-slate-800/60 border border-slate-700/40 px-3 py-2 space-y-1.5">
              <div className="flex items-center justify-between gap-2">
                <span className="text-[11px] text-slate-500">Project</span>
                <span className="text-xs font-mono text-slate-200">{projectId}</span>
              </div>
              {!noUrl && (
                <div className="flex items-center justify-between gap-2">
                  <span className="text-[11px] text-slate-500">URL</span>
                  <a
                    href={url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-xs text-sky-300 hover:text-sky-200 underline flex items-center gap-1 shrink-0"
                  >
                    {url.replace('https://', '')}
                    <ExternalLink size={10} />
                  </a>
                </div>
              )}
              <div className="flex items-center justify-between gap-2">
                <span className="text-[11px] text-slate-500">Status</span>
                <span className={`text-xs ${isError ? 'text-red-400' : 'text-emerald-400'}`}>
                  {isError ? 'error' : status}
                </span>
              </div>
            </div>

            {isError && errorTail.length > 0 && (
              <div className="rounded-md bg-red-500/5 border border-red-500/20 px-3 py-2">
                <p className="text-[10px] text-red-400/80 mb-1">last log lines</p>
                {errorTail.map((l) => (
                  <p key={l} className="text-[10px] font-mono text-red-300/90 break-all">
                    {l}
                  </p>
                ))}
              </div>
            )}
          </div>
        )}

        {warnings.length > 0 && (
          <div className="mt-3 rounded-md bg-amber-500/5 border border-amber-500/20 px-3 py-2 space-y-1">
            {warnings.map((w) => (
              <p key={w} className="text-[11px] text-amber-300">
                {w}
              </p>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
