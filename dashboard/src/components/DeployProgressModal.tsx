import { useState, useEffect, useRef } from "react";
import { X, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import { fetchDeployLogs, fetchAllStats, ProjectStatus } from "../api";

interface DeployProgressModalProps {
  projectId: string;
  domain: string;
  onClose: () => void;
}

export default function DeployProgressModal({
  projectId,
  domain,
  onClose,
}: DeployProgressModalProps) {
  const [deployLines, setDeployLines] = useState<string[]>([]);
  const hadLogs = useRef(false);
  const [status, setStatus] = useState<ProjectStatus>(ProjectStatus.Deploying);
  const [elapsed, setElapsed] = useState(0);
  const [showTimeoutMsg, setShowTimeoutMsg] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);

  // Track elapsed time
  useEffect(() => {
    const start = Date.now();
    const timer = setInterval(() => {
      setElapsed(Math.floor((Date.now() - start) / 1000));
    }, 1000);
    return () => clearInterval(timer);
  }, []);

  // Show timeout message after 30 seconds
  useEffect(() => {
    if (elapsed >= 30) setShowTimeoutMsg(true);
  }, [elapsed]);

  // Poll deploy logs
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const data = await fetchDeployLogs(projectId);
        if (!cancelled && data.lines.length > 0) {
          setDeployLines(data.lines);
          hadLogs.current = true;
        }
      } catch {}
    };
    load();
    const interval = setInterval(load, 2000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [projectId]);

  // Poll project status
  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      try {
        const stats = await fetchAllStats();
        const proj = stats.find((s) => s.project_id === projectId);
        if (proj && !cancelled) {
          setStatus(proj.status);
        }
      } catch {}
    };
    const interval = setInterval(check, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [projectId]);

  // Auto-scroll
  useEffect(() => {
    if (bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: "smooth" });
    }
  }, [deployLines]);

  const isSuccess = status === ProjectStatus.Running;
  const isError = status === ProjectStatus.Error;
  const isDone = isSuccess || isError;

  const formatTime = (secs: number) => {
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return m > 0 ? `${m}m ${s}s` : `${s}s`;
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 backdrop-blur-sm pt-8">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-2xl mx-4 shadow-2xl flex flex-col max-h-[80vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50 flex-shrink-0">
          <div className="flex items-center gap-2">
            {isDone ? (
              isError ? (
                <AlertCircle size={16} className="text-red-400" />
              ) : (
                <CheckCircle2 size={16} className="text-emerald-400" />
              )
            ) : (
              <Loader2 size={16} className="text-amber-400 animate-spin" />
            )}
            <h2 className="text-sm font-semibold text-slate-100">
              {isDone ? (isError ? "Deploy failed" : "Deployed") : "Deploying"}
            </h2>
            <span className="text-xs text-slate-500 font-mono">
              {projectId}
            </span>
          </div>
          <div className="flex items-center gap-3">
            <span className="text-xs text-slate-500 font-mono">
              {formatTime(elapsed)}
            </span>
            <button
              onClick={onClose}
              className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700 transition-colors cursor-pointer">
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Info */}
        <div className="px-4 py-2 border-b border-slate-700/30 flex items-center gap-2 text-xs text-slate-400 flex-shrink-0">
          <span>
            https://{projectId}.{domain}
          </span>
        </div>

        {/* Log content */}
        <div className="flex-1 overflow-y-auto p-4 font-mono text-xs leading-5 relative">
          {deployLines.length === 0 && !hadLogs.current ? (
            <div className="flex items-center justify-center py-10">
              <div className="w-5 h-5 border-2 border-slate-700 border-t-amber-500 rounded-full animate-spin" />
            </div>
          ) : (
            deployLines.map((line, i) => (
              <div
                key={i}
                className="text-amber-200/80 hover:text-amber-100 hover:bg-slate-800/50 px-2 py-0.5 rounded-sm transition-colors whitespace-pre-wrap break-all">
                <span className="text-amber-600/60 select-none mr-3 inline-block w-8 text-right">
                  {i + 1}
                </span>
                {line}
              </div>
            ))
          )}
          <div ref={bottomRef} />
        </div>

        {/* Footer */}
        <div className="px-4 py-3 border-t border-slate-700/50 flex-shrink-0">
          {showTimeoutMsg && !isDone && (
            <div className="mb-2 px-3 py-2 rounded-md bg-amber-500/10 border border-amber-500/20 text-xs text-amber-400">
              Deployment is taking longer than usual. You can continue waiting
              or close to check status from the project card.
            </div>
          )}
          {isDone && isSuccess && (
            <div className="mb-2 px-3 py-2 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-xs text-emerald-400">
              Deployment complete. Your app is live at{" "}
              <a
                href={`https://${projectId}.${domain}`}
                target="_blank"
                rel="noopener noreferrer"
                className="text-emerald-300 underline">
                https://{projectId}.{domain}
              </a>
            </div>
          )}
          {isDone && isError && (
            <div className="mb-2 px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
              Deployment failed. Check the project card for details.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
