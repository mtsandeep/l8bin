import { useState, useEffect, useRef, useCallback } from 'react';
import { X, RefreshCw, ArrowDown, Loader2 } from 'lucide-react';
import { fetchLogs, type ServiceInfo } from '../api';
import { useIntervalWhileVisible } from '../hooks';

interface LogViewerProps {
  projectId: string;
  services?: ServiceInfo[];
  onClose: () => void;
}

interface TabState {
  lines: string[];
  loading: boolean;
  stale: boolean;
}

export default function LogViewer({ projectId, services = [], onClose }: LogViewerProps) {
  const isMultiService = services.length > 1;
  const serviceNames = services.map(s => s.service_name);
  const publicService = services.find(s => s.is_public);

  // Default to public service, or first service
  const defaultTab = publicService?.service_name || serviceNames[0] || '';
  const [activeTab, setActiveTab] = useState(defaultTab);
  const [autoScroll, setAutoScroll] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  // Per-tab state
  const [tabStates, setTabStates] = useState<Record<string, TabState>>(() => {
    const initial: Record<string, TabState> = {};
    if (isMultiService) {
      for (const name of serviceNames) {
        initial[name] = { lines: [], loading: false, stale: false };
      }
    }
    return initial;
  });

  // Single-service state (backward compat)
  const [lines, setLines] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);

  const loadLogs = useCallback(async (serviceName?: string) => {
    try {
      const data = await fetchLogs(projectId, 200, serviceName);
      if (isMultiService) {
        setTabStates(prev => ({
          ...prev,
          [serviceName || '']: { lines: data.lines, loading: false, stale: false },
        }));
      } else {
        setLines(data.lines);
      }
    } catch (e) {
      console.error('Failed to fetch logs:', e);
      if (isMultiService) {
        setTabStates(prev => ({
          ...prev,
          [serviceName || '']: { ...prev[serviceName || ''], loading: false },
        }));
      } else {
        setLoading(false);
      }
    } finally {
      if (!isMultiService) setLoading(false);
    }
  }, [projectId, isMultiService]);

  // Load logs for active tab on interval
  useIntervalWhileVisible(() => {
    if (isMultiService) {
      loadLogs(activeTab);
    } else {
      loadLogs();
    }
  }, 3000);

  // When switching tabs, mark previous as stale and start loading new
  const handleTabSwitch = (tab: string) => {
    if (tab === activeTab) return;
    setTabStates(prev => ({
      ...prev,
      [activeTab]: { ...prev[activeTab], stale: true },
      [tab]: { ...prev[tab], loading: true },
    }));
    setActiveTab(tab);
  };

  // Track if the active tab was stale when we switched to it (for showing "Updating...")
  const wasStaleOnSwitch = useRef(false);
  useEffect(() => {
    wasStaleOnSwitch.current = tabStates[activeTab]?.stale ?? false;
  }, [activeTab]); // eslint-disable-line react-hooks/exhaustive-deps

  // Get current display state
  const currentLines = isMultiService ? (tabStates[activeTab]?.lines ?? []) : lines;
  const currentLoading = isMultiService ? (tabStates[activeTab]?.loading ?? true) : loading;

  useEffect(() => {
    if (autoScroll && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [currentLines, autoScroll]);

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 backdrop-blur-sm pt-8">
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg w-full max-w-3xl mx-4 shadow-2xl flex flex-col max-h-[80vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50 flex-shrink-0">
          <div className="flex items-center gap-2 min-w-0">
            <h2 className="text-sm font-semibold text-slate-100">
              Logs
            </h2>
            <span className="text-xs text-slate-500 font-mono truncate">{projectId}</span>
          </div>
          <div className="flex items-center gap-1.5">
            <button
              onClick={() => isMultiService ? loadLogs(activeTab) : loadLogs()}
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

        {/* Service tabs (multi-service only) */}
        {isMultiService && (
          <div className="flex border-b border-slate-700/50 flex-shrink-0 overflow-x-auto">
            {serviceNames.map(name => {
              const tab = tabStates[name];
              const isActive = name === activeTab;
              const isStale = tab?.stale && !isActive;
              const isLoading = tab?.loading && isActive;
              const svc = services.find(s => s.service_name === name);
              return (
                <button
                  key={name}
                  onClick={() => handleTabSwitch(name)}
                  className={`flex items-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors cursor-pointer whitespace-nowrap border-b-2 ${
                    isActive
                      ? 'text-violet-300 border-violet-500 bg-violet-500/5'
                      : 'text-slate-400 border-transparent hover:text-slate-200 hover:bg-slate-800/50'
                  }`}
                >
                  {isLoading && <Loader2 size={10} className="animate-spin text-violet-400" />}
                  {name}
                  {svc?.is_public && (
                    <span className="text-[9px] px-1 py-0.5 rounded bg-sky-500/20 text-sky-400">public</span>
                  )}
                  {isStale && (
                    <span className="w-1.5 h-1.5 rounded-full bg-amber-400/70 shrink-0" title="Data may be outdated" />
                  )}
                </button>
              );
            })}
          </div>
        )}

        {/* Log content */}
        <div className="flex-1 overflow-y-auto p-4 font-mono text-xs leading-5 relative">
          {/* Updating banner when refreshing stale data */}
          {isMultiService && currentLoading && wasStaleOnSwitch.current && currentLines.length > 0 && (
            <div className="flex items-center gap-2 text-[10px] text-slate-500 mb-2">
              <Loader2 size={10} className="animate-spin" />
              <span>Updating...</span>
            </div>
          )}
          {currentLoading && currentLines.length === 0 ? (
            <div className="flex items-center justify-center py-10">
              <div className="w-5 h-5 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
            </div>
          ) : currentLines.length === 0 ? (
            <p className="text-slate-600 text-center py-10">No logs available</p>
          ) : (
            currentLines.map((line, i) => (
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
