import { useState, useEffect, useRef, useCallback } from 'react';
import { X, RefreshCw, ArrowDown, Loader2, Rocket } from 'lucide-react';
import { fetchLogs, fetchDeployLogs, type ServiceInfo, ProjectStatus } from '../api';
import { useIntervalWhileVisible } from '../hooks';

interface LogViewerProps {
  projectId: string;
  services?: ServiceInfo[];
  status?: ProjectStatus;
  onClose: () => void;
}

interface TabState {
  lines: string[];
  loading: boolean;
  stale: boolean;
}

export default function LogViewer({ projectId, services = [], status, onClose }: LogViewerProps) {
  const isMultiService = services.length > 1;
  const serviceNames = services.map(s => s.service_name);
  const publicService = services.find(s => s.is_public);
  const isDeploying = status === ProjectStatus.Deploying;
  const isError = status === ProjectStatus.Error;

  // Default to "deploy" tab when deploying or error, otherwise public service or first
  const shouldShowDeploy = isDeploying || isError;
  const defaultTab = shouldShowDeploy ? 'deploy' : (publicService?.service_name || serviceNames[0] || '');
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
    if (shouldShowDeploy) {
      initial['deploy'] = { lines: [], loading: true, stale: false };
    }
    return initial;
  });

  // For error state, check if deploy logs exist; if not, fall back to service tab
  useEffect(() => {
    if (isError) {
      fetchDeployLogs(projectId)
        .then(data => {
          if (data.lines.length === 0) {
            // No deploy logs — switch away from deploy tab
            setActiveTab(publicService?.service_name || serviceNames[0] || '');
          }
        })
        .catch(() => {
          setActiveTab(publicService?.service_name || serviceNames[0] || '');
        });
    }
  }, [projectId, isError]); // eslint-disable-line react-hooks/exhaustive-deps

  // Single-service state
  const [lines, setLines] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);

  const loadLogs = useCallback(async (tab?: string) => {
    try {
      if (tab === 'deploy') {
        const data = await fetchDeployLogs(projectId);
        setTabStates(prev => ({
          ...prev,
          deploy: { lines: data.lines, loading: false, stale: false },
        }));
      } else {
        const data = await fetchLogs(projectId, 200, tab);
        if (isMultiService) {
          setTabStates(prev => ({
            ...prev,
            [tab || '']: { lines: data.lines, loading: false, stale: false },
          }));
        } else {
          setLines(data.lines);
        }
      }
    } catch (e) {
      console.error('Failed to fetch logs:', e);
      if (isMultiService || tab === 'deploy') {
        setTabStates(prev => ({
          ...prev,
          [tab || '']: { ...prev[tab || ''], loading: false },
        }));
      } else {
        setLoading(false);
      }
    } finally {
      if (!isMultiService && tab !== 'deploy') setLoading(false);
    }
  }, [projectId, isMultiService]);

  // Load logs for active tab on interval
  useIntervalWhileVisible(() => {
    if (isMultiService || activeTab === 'deploy') {
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
  const isDeployTab = activeTab === 'deploy';
  const currentLines = (isMultiService || isDeployTab) ? (tabStates[activeTab]?.lines ?? []) : lines;
  const currentLoading = (isMultiService || isDeployTab) ? (tabStates[activeTab]?.loading ?? false) : loading;

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
              onClick={() => (isMultiService || isDeployTab) ? loadLogs(activeTab) : loadLogs()}
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

        {/* Service tabs (multi-service or deploying) */}
        {(isMultiService || shouldShowDeploy) && (
          <div className="flex border-b border-slate-700/50 flex-shrink-0 overflow-x-auto">
            {shouldShowDeploy && (
              <button
                onClick={() => handleTabSwitch('deploy')}
                className={`flex items-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors cursor-pointer whitespace-nowrap border-b-2 ${
                  activeTab === 'deploy'
                    ? 'text-amber-300 border-amber-500 bg-amber-500/5'
                    : 'text-slate-400 border-transparent hover:text-slate-200 hover:bg-slate-800/50'
                }`}
              >
                {tabStates['deploy']?.loading && activeTab === 'deploy' && <Loader2 size={10} className="animate-spin text-amber-400" />}
                <Rocket size={10} />
                Deploy
              </button>
            )}
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
          {/* Deploy finished indicator */}
          {isDeployTab && currentLines.length > 0 && !currentLoading && status === ProjectStatus.Error && (
            <div className="flex items-center gap-2 mt-3 px-2 py-2 rounded-md bg-red-500/10 border border-red-500/20">
              <span className="w-2 h-2 rounded-full bg-red-500 shrink-0" />
              <span className="text-xs text-red-400 font-medium">Deploy failed — no more logs</span>
            </div>
          )}
          {isDeployTab && currentLines.length > 0 && !currentLoading && status === ProjectStatus.Running && (
            <div className="flex items-center gap-2 mt-3 px-2 py-2 rounded-md bg-emerald-500/10 border border-emerald-500/20">
              <span className="w-2 h-2 rounded-full bg-emerald-500 shrink-0" />
              <span className="text-xs text-emerald-400 font-medium">Deploy completed</span>
            </div>
          )}
          <div ref={bottomRef} />
        </div>
      </div>
    </div>
  );
}
