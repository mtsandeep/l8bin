import { useState, useEffect } from 'react';
import { Rocket, Loader2, X, ChevronRight, ChevronLeft, Moon, Server } from 'lucide-react';
import { deployProject, fetchNodes, fetchGlobalSettings, type Node } from '../api';

interface DeployFormProps {
  onDeploy: () => void;
  onClose: () => void;
}

export default function DeployForm({ onDeploy, onClose }: DeployFormProps) {
  const [step, setStep] = useState<1 | 2>(1);

  // Step 1 fields
  const [projectId, setProjectId] = useState('');
  const [projectName, setProjectName] = useState('');
  const [projectDescription, setProjectDescription] = useState('');
  const [image, setImage] = useState('');
  const [port, setPort] = useState('80');

  // Step 2 fields — pre-populated with defaults
  const [autoStop, setAutoStop] = useState(true);
  const [timeoutMins, setTimeoutMins] = useState(15);
  const [autoStart, setAutoStart] = useState(true);
  const [cmd, setCmd] = useState('');
  const [memMb, setMemMb] = useState<number | null>(null); // null = use global default
  const [cpuLimit, setCpuLimit] = useState<number | null>(null);
  const [globalMemMb, setGlobalMemMb] = useState(256);
  const [globalCpu, setGlobalCpu] = useState(0.5);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [domain, setDomain] = useState('localhost');
  const [nodes, setNodes] = useState<Node[]>([]);

  // Fetch nodes + global settings when entering step 2
  useEffect(() => {
    if (step === 2) {
      if (nodes.length === 0) fetchNodes().then(setNodes).catch(() => {});
      fetchGlobalSettings().then(s => {
        setGlobalMemMb(s.default_memory_limit_mb);
        setGlobalCpu(s.default_cpu_limit);
        setDomain(s.domain);
        if (memMb === null) setMemMb(s.default_memory_limit_mb);
        if (cpuLimit === null) setCpuLimit(s.default_cpu_limit);
      }).catch(() => {});
    }
  }, [step]);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const step1Valid = projectId.trim() !== '' && image.trim() !== '';
  const timeoutValid = timeoutMins >= 1;

  const handleNext = (e: React.FormEvent) => {
    e.preventDefault();
    if (step1Valid) setStep(2);
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!timeoutValid) return;
    setError(null);
    setLoading(true);

    try {
      await deployProject({
        project_id: projectId.trim(),
        image: image.trim(),
        port: parseInt(port, 10),
        name: projectName.trim() || undefined,
        description: projectDescription.trim() || undefined,
        node_id: selectedNode,
        auto_stop_enabled: autoStop,
        auto_stop_timeout_mins: timeoutMins,
        auto_start_enabled: autoStart,
        cmd: cmd.trim() || undefined,
        memory_limit_mb: memMb,
        cpu_limit: cpuLimit,
      });
      onDeploy();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Deploy failed');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50">
          <h2 className="text-sm font-semibold text-slate-100">Deploy New App</h2>
          <button
            onClick={onClose}
            className="p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        {/* Step 1 */}
        {step === 1 && (
          <form onSubmit={handleNext} className="p-5 space-y-4">
            {/* Step indicator */}
            <div className="flex items-center gap-1">
              {([1, 2] as const).map((s, i) => (
                <div key={s} className="flex items-center gap-1">
                  <div className={`w-6 h-6 rounded-full flex items-center justify-center text-[10px] font-medium ${
                    step === s ? 'bg-violet-600 text-white' : step > s ? 'bg-violet-900/50 text-violet-400' : 'bg-slate-800 text-slate-500'
                  }`}>{s}</div>
                  {i < 1 && <ChevronRight size={12} className="text-slate-700" />}
                </div>
              ))}
              <span className="ml-2 text-xs text-slate-500">App details</span>
            </div>

            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Project ID
              </label>
              <input
                type="text"
                value={projectId}
                onChange={(e) => setProjectId(e.target.value)}
                placeholder="my-app"
                required
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
              <p className="text-[11px] text-slate-500 mt-1">
                Used as subdomain: <span className="text-slate-400">{projectId || 'my-app'}.{domain}</span>
              </p>
            </div>

            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Display Name <span className="text-slate-600 font-normal">(optional)</span>
              </label>
              <input
                type="text"
                value={projectName}
                onChange={(e) => setProjectName(e.target.value)}
                placeholder="My App"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Description <span className="text-slate-600 font-normal">(optional)</span>
              </label>
              <input
                type="text"
                value={projectDescription}
                onChange={(e) => setProjectDescription(e.target.value)}
                placeholder="What this app does"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Docker Image
              </label>
              <input
                type="text"
                value={image}
                onChange={(e) => setImage(e.target.value)}
                placeholder="nginx:alpine"
                required
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                App Port
              </label>
              <input
                type="number"
                value={port}
                onChange={(e) => setPort(e.target.value)}
                placeholder="80"
                required
                min={1}
                max={65535}
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
              <p className="text-[11px] text-slate-500 mt-1">
                Port your app listens on inside the container (e.g. 80 for nginx, 3000 for Node)
              </p>
            </div>

            <button
              type="submit"
              disabled={!step1Valid}
              className="w-full inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
            >
              Next
              <ChevronRight size={14} />
            </button>
          </form>
        )}

        {/* Step 2 */}
        {step === 2 && (
          <form onSubmit={handleSubmit} className="p-5 space-y-4">
            {/* Step indicator */}
            <div className="flex items-center gap-1">
              {([1, 2] as const).map((s, i) => (
                <div key={s} className="flex items-center gap-1">
                  <div className={`w-6 h-6 rounded-full flex items-center justify-center text-[10px] font-medium ${
                    step === s ? 'bg-violet-600 text-white' : step > s ? 'bg-violet-900/50 text-violet-400' : 'bg-slate-800 text-slate-500'
                  }`}>{s}</div>
                  {i < 1 && <ChevronRight size={12} className="text-slate-700" />}
                </div>
              ))}
              <span className="ml-2 text-xs text-slate-500">Settings</span>
            </div>

            <div className="flex items-center gap-1.5 text-slate-400 mb-1">
              <Moon size={13} />
              <span className="text-xs font-medium">Sleep Settings</span>
            </div>

            {/* Auto-stop toggle */}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-stop when idle</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoStop}
                onClick={() => setAutoStop((v) => !v)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  autoStop ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    autoStop ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </label>

            {/* Idle timeout — only shown when auto-stop is enabled */}
            {autoStop && (
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs text-slate-300">Idle timeout (mins)</span>
                <input
                  type="number"
                  min={1}
                  value={timeoutMins}
                  onChange={(e) => setTimeoutMins(Number(e.target.value))}
                  className="w-16 bg-slate-900/50 border border-slate-700/50 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
                />
              </div>
            )}

            {/* Auto-start toggle */}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-start on visit</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoStart}
                onClick={() => setAutoStart((v) => !v)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  autoStart ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    autoStart ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </label>

            {/* Command override */}
            <div className="pt-1 border-t border-slate-700/50">
              <label className="block text-xs font-medium text-slate-400 mb-1.5">
                Command override <span className="text-slate-600 font-normal">(optional)</span>
              </label>
              <input
                type="text"
                value={cmd}
                onChange={(e) => setCmd(e.target.value)}
                placeholder="e.g. prefect server start --host 0.0.0.0"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-xs text-slate-200 font-mono placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            {/* Resource limits */}
            <div className="pt-1 border-t border-slate-700/50 space-y-3">
              <div>
                <div className="flex items-center justify-between mb-1.5">
                  <span className="text-xs text-slate-400">Memory limit</span>
                  <span className="text-xs font-mono text-violet-300">{memMb ?? globalMemMb} MB</span>
                </div>
                <input type="range" min={64} max={4096} step={64}
                  value={memMb ?? globalMemMb}
                  onChange={e => setMemMb(Number(e.target.value))}
                  className="w-full accent-violet-500"
                />
                <div className="flex justify-between text-[10px] text-slate-600 mt-0.5"><span>64 MB</span><span>4096 MB</span></div>
              </div>
              <div>
                <div className="flex items-center justify-between mb-1.5">
                  <span className="text-xs text-slate-400">CPU limit</span>
                  <span className="text-xs font-mono text-violet-300">{(cpuLimit ?? globalCpu).toFixed(2)} vCPU</span>
                </div>
                <input type="range" min={0.1} max={4} step={0.1}
                  value={cpuLimit ?? globalCpu}
                  onChange={e => setCpuLimit(Number(e.target.value))}
                  className="w-full accent-violet-500"
                />
                <div className="flex justify-between text-[10px] text-slate-600 mt-0.5"><span>0.1</span><span>4.0</span></div>
              </div>
            </div>

            {/* Node picker */}
            <div className="pt-1">
              <div className="flex items-center gap-1.5 text-slate-400 mb-2">
                <Server size={13} />
                <span className="text-xs font-medium">Node</span>
              </div>
              <div className="space-y-1">
                <button
                  type="button"
                  onClick={() => setSelectedNode(null)}
                  className={`w-full flex items-center justify-between px-3 py-2 rounded-md border text-xs transition-colors cursor-pointer ${
                    selectedNode === null
                      ? 'border-violet-500/50 bg-violet-500/10 text-violet-300'
                      : 'border-slate-700/50 bg-slate-900/50 text-slate-400 hover:border-slate-600'
                  }`}
                >
                  <span>Automatic</span>
                  {selectedNode === null && <span className="text-[10px] text-violet-400">selected</span>}
                </button>
                {nodes.filter(n => n.status === 'online').map(node => (
                  <button
                    key={node.id}
                    type="button"
                    onClick={() => setSelectedNode(node.id)}
                    className={`w-full flex items-center justify-between px-3 py-2 rounded-md border text-xs transition-colors cursor-pointer ${
                      selectedNode === node.id
                        ? 'border-violet-500/50 bg-violet-500/10 text-violet-300'
                        : 'border-slate-700/50 bg-slate-900/50 text-slate-400 hover:border-slate-600'
                    }`}
                  >
                    <span className="font-mono">{node.name} <span className="text-slate-600">({node.id})</span></span>
                    {selectedNode === node.id && <span className="text-[10px] text-violet-400">selected</span>}
                  </button>
                ))}
              </div>
            </div>

            {/* Validation message */}
            {autoStop && !timeoutValid && (
              <div className="px-3 py-2 rounded-md bg-amber-500/10 border border-amber-500/20 text-xs text-amber-400">
                Idle timeout must be at least 1 minute.
              </div>
            )}

            {error && (
              <div className="px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
                {error}
              </div>
            )}

            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setStep(1)}
                className="inline-flex items-center justify-center gap-1.5 px-4 py-2.5 rounded-md text-sm font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                <ChevronLeft size={14} />
                Back
              </button>
              <button
                type="submit"
                disabled={loading || (autoStop && !timeoutValid)}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
              >
                {loading ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <Rocket size={14} />
                )}
                {loading ? 'Deploying...' : 'Deploy'}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
