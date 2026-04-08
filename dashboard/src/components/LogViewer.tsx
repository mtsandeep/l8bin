import { useState, useEffect, useRef } from 'react';
import { X, RefreshCw, ArrowDown } from 'lucide-react';
import { fetchLogs } from '../api';
import { useIntervalWhileVisible } from '../hooks';

interface LogViewerProps {
  projectId: string;
  onClose: () => void;
}

export default function LogViewer({ projectId, onClose }: LogViewerProps) {
  const [lines, setLines] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [autoScroll, setAutoScroll] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  const loadLogs = async () => {
    try {
      const data = await fetchLogs(projectId, 200);
      setLines(data.lines);
    } catch (e) {
      console.error('Failed to fetch logs:', e);
    } finally {
      setLoading(false);
    }
  };

  useIntervalWhileVisible(loadLogs, 3000);

  useEffect(() => {
    if (autoScroll && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [lines, autoScroll]);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg w-full max-w-3xl mx-4 shadow-2xl flex flex-col max-h-[80vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50 flex-shrink-0">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-semibold text-slate-100">
              Logs
            </h2>
            <span className="text-xs text-slate-500 font-mono">{projectId}</span>
          </div>
          <div className="flex items-center gap-1.5">
            <button
              onClick={loadLogs}
              className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors cursor-pointer"
              title="Refresh"
            >
              <RefreshCw size={13} />
            </button>
            <button
              onClick={() => setAutoScroll(!autoScroll)}
              className={`p-1.5 rounded-md transition-colors cursor-pointer ${
                autoScroll
                  ? 'text-violet-400 bg-violet-500/10'
                  : 'text-slate-500 hover:text-slate-300 hover:bg-slate-800'
              }`}
              title={autoScroll ? 'Auto-scroll on' : 'Auto-scroll off'}
            >
              <ArrowDown size={13} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors cursor-pointer"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Log content */}
        <div className="flex-1 overflow-y-auto p-4 font-mono text-xs leading-5">
          {loading ? (
            <div className="flex items-center justify-center py-10">
              <div className="w-5 h-5 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
            </div>
          ) : lines.length === 0 ? (
            <p className="text-slate-600 text-center py-10">No logs available</p>
          ) : (
            lines.map((line, i) => (
              <div
                key={i}
                className="text-slate-400 hover:text-slate-200 hover:bg-slate-800/50 px-2 py-0.5 rounded-sm transition-colors whitespace-pre-wrap break-all"
              >
                <span className="text-slate-600 select-none mr-3 inline-block w-8 text-right">
                  {i + 1}
                </span>
                {line}
              </div>
            ))
          )}
          <div ref={bottomRef} />
        </div>
      </div>
    </div>
  );
}
