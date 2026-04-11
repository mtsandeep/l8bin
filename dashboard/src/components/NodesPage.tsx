import { useState, useCallback } from 'react';
import {
  Server, Plus, Trash2, RefreshCw, CheckCircle, XCircle,
  Clock, Copy, ChevronRight, Loader
} from 'lucide-react';
import { useIntervalWhileVisible } from '../hooks';
import { type Node, type NodeImageStats, fetchNodes, fetchNodeImageStats, pruneNodeImages, addNode, connectNode, deleteNode, timeAgo, formatBytes } from '../api';

function statusColor(status: string) {
  if (status === 'online') return 'text-emerald-400';
  if (status === 'offline') return 'text-rose-400';
  return 'text-slate-500';
}

function StatusDot({ status }: { status: string }) {
  const color =
    status === 'online' ? 'bg-emerald-400' :
    status === 'offline' ? 'bg-rose-400' : 'bg-slate-600';
  return <span className={`inline-block w-2 h-2 rounded-full ${color}`} />;
}

function formatMem(bytes: number | null) {
  if (!bytes) return '—';
  const gb = bytes / 1024 / 1024 / 1024;
  if (gb < 1) return `${Math.round(bytes / 1024 / 1024)} MB`;
  return `${gb.toFixed(1)} GB`;
}

// ── Add Agent Wizard ───────────────────────────────────────────────────────────

type WizardStep = 'form' | 'instructions' | 'connecting';

interface AddAgentWizardProps {
  onClose: () => void;
  onAdded: () => void;
}

function AddAgentWizard({ onClose, onAdded }: AddAgentWizardProps) {
  const [step, setStep] = useState<WizardStep>('form');
  const [name, setName] = useState('');
  const [host, setHost] = useState('');
  const [port, setPort] = useState('5083');
  const [region, setRegion] = useState('');
  const [error, setError] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [copied, setCopied] = useState(false);

  const installCommand = `curl -fsSL https://l8b.in | bash -s agent`;

  function copyCommand() {
    navigator.clipboard.writeText(installCommand);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  async function handleConnect() {
    setConnecting(true);
    setError('');
    setStep('connecting');
    try {
      const node = await addNode({
        name,
        host,
        agent_port: parseInt(port) || 5083,
        region: region || undefined,
      });
      // Actually try to connect via mTLS health check
      await connectNode(node.id);
      onAdded();
      onClose();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Connection failed');
      setStep('instructions');
    } finally {
      setConnecting(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50 p-4">
      <div className="bg-slate-900 border border-slate-700/60 rounded-xl w-full max-w-lg shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-slate-800">
          <div className="flex items-center gap-2">
            <Server size={16} className="text-violet-400" />
            <h2 className="text-sm font-semibold text-slate-100">Add Agent</h2>
          </div>
          <button onClick={onClose} className="text-slate-500 hover:text-slate-300 text-lg leading-none">×</button>
        </div>

        {/* Step indicator */}
        <div className="flex items-center gap-1 px-6 pt-4">
          {(['form', 'instructions', 'connecting'] as WizardStep[]).map((s, i) => (
            <div key={s} className="flex items-center gap-1">
              <div className={`w-6 h-6 rounded-full flex items-center justify-center text-[10px] font-medium
                ${step === s ? 'bg-violet-600 text-white' :
                  (step === 'instructions' && s === 'form') || step === 'connecting'
                    ? 'bg-violet-900/50 text-violet-400' : 'bg-slate-800 text-slate-500'}`}>
                {i + 1}
              </div>
              {i < 2 && <ChevronRight size={12} className="text-slate-700" />}
            </div>
          ))}
          <span className="ml-2 text-xs text-slate-500">
            {step === 'form' ? 'Server details' : step === 'instructions' ? 'Install agent' : 'Connecting…'}
          </span>
        </div>

        <div className="px-6 py-5 space-y-4">
          {/* Step 1: Form */}
          {step === 'form' && (
            <>
              <div className="grid grid-cols-2 gap-3">
                <div className="col-span-2">
                  <label className="block text-xs text-slate-400 mb-1">Agent server name</label>
                  <input
                    className="w-full bg-slate-800 border border-slate-700 rounded-lg px-3 py-2 text-sm text-slate-100 focus:outline-none focus:border-violet-500"
                    placeholder="server-eu-1"
                    value={name}
                    onChange={e => setName(e.target.value)}
                  />
                </div>
                <div>
                  <label className="block text-xs text-slate-400 mb-1">Server IP / hostname</label>
                  <input
                    className="w-full bg-slate-800 border border-slate-700 rounded-lg px-3 py-2 text-sm text-slate-100 focus:outline-none focus:border-violet-500"
                    placeholder="10.0.0.5"
                    value={host}
                    onChange={e => setHost(e.target.value)}
                  />
                </div>
                <div>
                  <label className="block text-xs text-slate-400 mb-1">Agent port</label>
                  <input
                    className="w-full bg-slate-800 border border-slate-700 rounded-lg px-3 py-2 text-sm text-slate-100 focus:outline-none focus:border-violet-500"
                    placeholder="5083"
                    value={port}
                    onChange={e => setPort(e.target.value)}
                  />
                </div>
                <div className="col-span-2">
                  <label className="block text-xs text-slate-400 mb-1">Region <span className="text-slate-600">(optional)</span></label>
                  <input
                    className="w-full bg-slate-800 border border-slate-700 rounded-lg px-3 py-2 text-sm text-slate-100 focus:outline-none focus:border-violet-500"
                    placeholder="eu-west"
                    value={region}
                    onChange={e => setRegion(e.target.value)}
                  />
                </div>
              </div>
              <button
                disabled={!name || !host}
                onClick={() => setStep('instructions')}
                className="w-full py-2 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
              >
                Next — Install agent
              </button>
            </>
          )}

          {/* Step 2: Instructions */}
          {step === 'instructions' && (
            <>
              <p className="text-xs text-slate-400">
                SSH into <span className="text-slate-200 font-mono">{host}</span> and run this command to install the LiteBin agent:
              </p>
              <div className="relative bg-slate-950 border border-slate-700/60 rounded-lg p-3">
                <pre className="text-xs text-emerald-400 font-mono whitespace-pre-wrap break-all pr-8">
                  {installCommand}
                </pre>
                <button
                  onClick={copyCommand}
                  className="absolute top-2 right-2 p-1.5 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors"
                  title="Copy"
                >
                  {copied ? <CheckCircle size={14} className="text-emerald-400" /> : <Copy size={14} />}
                </button>
              </div>
              <p className="text-xs text-slate-500">
                The script installs Docker, configures the firewall, and starts the agent on port <span className="text-slate-300">{port}</span>.
                Once it's running, click <span className="text-slate-300">Connect</span> to verify and register the agent.
              </p>
              {error && (
                <div className="flex items-start gap-2 p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-400">
                  <XCircle size={14} className="mt-0.5 shrink-0" />
                  {error}
                </div>
              )}
              <div className="flex gap-2">
                <button
                  onClick={() => setStep('form')}
                  className="flex-1 py-2 rounded-lg text-sm text-slate-400 hover:text-slate-200 border border-slate-700 hover:border-slate-600 transition-colors"
                >
                  Back
                </button>
                <button
                  onClick={handleConnect}
                  disabled={connecting}
                  className="flex-1 py-2 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-40 transition-colors"
                >
                  Connect
                </button>
              </div>
            </>
          )}

          {/* Step 3: Connecting */}
          {step === 'connecting' && (
            <div className="flex flex-col items-center py-6 gap-3">
              <Loader size={24} className="text-violet-400 animate-spin" />
              <p className="text-sm text-slate-400">Connecting to agent at <span className="text-slate-200">{host}:{port}</span>…</p>
              <p className="text-xs text-slate-600">Verifying mTLS health check</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Prune Modal ────────────────────────────────────────────────────────────────

type PruneModalState = 'confirming' | 'pruning' | 'success' | 'error';

interface PruneModalProps {
  nodeName: string;
  danglingCount: number;
  danglingSize: number;
  state: PruneModalState;
  reclaimed: number | null;
  error: string;
  onConfirm: () => void;
  onClose: () => void;
}

function PruneModal({ nodeName, danglingCount, danglingSize, state, reclaimed, error, onConfirm, onClose }: PruneModalProps) {
  return (
    <div className="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50 p-4">
      <div className="bg-slate-900 border border-slate-700/60 rounded-xl w-full max-w-sm shadow-2xl">
        <div className="px-6 py-5 space-y-4">
          {state === 'confirming' && (
            <>
              <div className="flex items-center gap-3">
                <div className="w-9 h-9 rounded-lg bg-amber-500/10 border border-amber-500/20 flex items-center justify-center shrink-0">
                  <Trash2 size={16} className="text-amber-400" />
                </div>
                <div>
                  <h3 className="text-sm font-semibold text-slate-100">Prune dangling images</h3>
                  <p className="text-xs text-slate-500 mt-0.5">{nodeName}</p>
                </div>
              </div>
              <p className="text-xs text-slate-400">
                This will remove <span className="text-amber-400 font-medium">{danglingCount} dangling image{danglingCount !== 1 ? 's' : ''}</span> and free up{' '}
                <span className="text-amber-400 font-medium">{formatBytes(danglingSize)}</span> of disk space. This action cannot be undone.
              </p>
              <div className="flex gap-2">
                <button
                  onClick={onClose}
                  className="flex-1 py-2 rounded-lg text-sm text-slate-400 hover:text-slate-200 border border-slate-700 hover:border-slate-600 transition-colors"
                >
                  Cancel
                </button>
                <button
                  onClick={onConfirm}
                  className="flex-1 py-2 rounded-lg text-sm font-medium bg-amber-500/80 text-white hover:bg-amber-500 transition-colors"
                >
                  Prune
                </button>
              </div>
            </>
          )}

          {state === 'pruning' && (
            <div className="flex flex-col items-center py-4 gap-3">
              <Loader size={24} className="text-amber-400 animate-spin" />
              <p className="text-sm text-slate-400">Pruning images on <span className="text-slate-200">{nodeName}</span>…</p>
            </div>
          )}

          {state === 'success' && (
            <>
              <div className="flex flex-col items-center py-4 gap-3">
                <div className="w-10 h-10 rounded-full bg-emerald-500/10 border border-emerald-500/20 flex items-center justify-center">
                  <CheckCircle size={20} className="text-emerald-400" />
                </div>
                <div className="text-center">
                  <h3 className="text-sm font-semibold text-slate-100">Prune complete</h3>
                  <p className="text-xs text-slate-400 mt-1">
                    Reclaimed <span className="text-emerald-400 font-medium">{formatBytes(reclaimed!)}</span> from {danglingCount} image{danglingCount !== 1 ? 's' : ''}
                  </p>
                </div>
              </div>
              <button
                onClick={onClose}
                className="w-full py-2 rounded-lg text-sm font-medium bg-slate-800 text-slate-200 hover:bg-slate-700 transition-colors"
              >
                OK
              </button>
            </>
          )}

          {state === 'error' && (
            <>
              <div className="flex flex-col items-center py-4 gap-3">
                <div className="w-10 h-10 rounded-full bg-rose-500/10 border border-rose-500/20 flex items-center justify-center">
                  <XCircle size={20} className="text-rose-400" />
                </div>
                <div className="text-center">
                  <h3 className="text-sm font-semibold text-slate-100">Prune failed</h3>
                  <p className="text-xs text-rose-400 mt-1">{error}</p>
                </div>
              </div>
              <button
                onClick={onClose}
                className="w-full py-2 rounded-lg text-sm font-medium bg-slate-800 text-slate-200 hover:bg-slate-700 transition-colors"
              >
                OK
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Nodes Page ─────────────────────────────────────────────────────────────────

interface NodesPageProps {
  onBack: () => void;
}

export default function NodesPage({ onBack }: NodesPageProps) {
  const [nodes, setNodes] = useState<Node[]>([]);
  const [imageStats, setImageStats] = useState<NodeImageStats[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAdd, setShowAdd] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const [pruning, setPruning] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [deleteModal, setDeleteModal] = useState<{
    nodeId: string;
    nodeName: string;
    state: 'confirming' | 'deleting' | 'error';
    error: string;
  } | null>(null);
  const [pruneModal, setPruneModal] = useState<{
    nodeId: string;
    nodeName: string;
    danglingCount: number;
    danglingSize: number;
    state: 'confirming' | 'pruning' | 'success' | 'error';
    reclaimed: number | null;
    error: string;
  } | null>(null);

  const load = useCallback(async () => {
    try {
      const [n, stats] = await Promise.all([fetchNodes(), fetchNodeImageStats().catch(() => [])]);
      setNodes(n);
      setImageStats(stats);
    } catch {
      setError('Failed to load agents');
    } finally {
      setLoading(false);
    }
  }, []);

  useIntervalWhileVisible(load, 30000);

  async function handleRemove(id: string) {
    setRemoving(id);
    setDeleteModal(prev => prev ? { ...prev, state: 'deleting' } : null);
    try {
      await deleteNode(id);
      setDeleteModal(null);
      await load();
    } catch (e: unknown) {
      setDeleteModal(prev => prev ? { ...prev, state: 'error', error: e instanceof Error ? e.message : 'Failed to remove agent' } : null);
    } finally {
      setRemoving(null);
    }
  }

  async function handlePruneConfirm() {
    if (!pruneModal) return;
    const { nodeId } = pruneModal;
    setPruning(nodeId);
    setPruneModal(prev => prev ? { ...prev, state: 'pruning' } : null);
    try {
      const result = await pruneNodeImages(nodeId);
      setPruneModal(prev => prev ? { ...prev, state: 'success', reclaimed: result.bytes_reclaimed } : null);
      await load();
    } catch (e: unknown) {
      setPruneModal(prev => prev ? { ...prev, state: 'error', error: e instanceof Error ? e.message : 'Failed to prune images' } : null);
    } finally {
      setPruning(null);
    }
  }

  return (
    <div className="min-h-screen bg-slate-950 text-slate-200">
      <header className="border-b border-slate-800/80 bg-slate-900/50 backdrop-blur-md sticky top-0 z-40">
        <div className="max-w-5xl mx-auto px-6 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <button onClick={onBack} className="text-slate-500 hover:text-slate-300 text-sm transition-colors">
              ← Back
            </button>
            <div className="w-px h-4 bg-slate-700" />
            <Server size={16} className="text-violet-400" />
            <h1 className="text-sm font-semibold text-slate-100">Agents</h1>
          </div>
          <div className="flex items-center gap-2">
            <button onClick={load} className="p-1.5 rounded-md text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors">
              <RefreshCw size={14} />
            </button>
            <button
              onClick={() => setShowAdd(true)}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors"
            >
              <Plus size={13} />
              Add agent
            </button>
          </div>
        </div>
      </header>

      <main className="max-w-5xl mx-auto px-6 py-6">
        {error && (
          <div className="mb-4 p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-400">
            {error}
          </div>
        )}

        {loading ? (
          <div className="flex items-center justify-center py-20">
            <div className="w-6 h-6 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
          </div>
        ) : (
          <div className="space-y-3">
            {nodes.map(node => {
              const stats = imageStats.find(s => s.node_id === node.id)?.image_stats;
              return (
              <div
                key={node.id}
                className="p-4 rounded-xl bg-slate-900/60 border border-slate-800/60 hover:border-slate-700/60 transition-colors"
              >
                {/* Top row: icon + name + status */}
                <div className="flex items-center gap-3">
                  <div className="w-9 h-9 rounded-lg bg-slate-800 border border-slate-700/50 flex items-center justify-center shrink-0">
                    <Server size={16} className="text-slate-400" />
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium text-slate-100">{node.name}</span>
                      {node.id === 'local' && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-violet-500/20 text-violet-400">local</span>
                      )}
                      {node.region && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-slate-800 text-slate-500">{node.region}</span>
                      )}
                    </div>
                    <span className="text-xs font-mono text-slate-600 truncate">{node.id}</span>
                  </div>
                  <div className="flex items-center gap-3 shrink-0">
                    <div className="flex items-center gap-1.5">
                      <StatusDot status={node.status} />
                      <span className={`text-xs ${statusColor(node.status)}`}>{node.status}</span>
                    </div>
                    {node.fail_count > 0 && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-rose-500/10 text-rose-400">
                        {node.fail_count} fail{node.fail_count > 1 ? 's' : ''}
                      </span>
                    )}
                    {node.id !== 'local' && (
                      <button
                        onClick={() => setDeleteModal({ nodeId: node.id, nodeName: node.name, state: 'confirming', error: '' })}
                        disabled={removing === node.id}
                        className="p-1.5 rounded text-slate-600 hover:text-rose-400 hover:bg-rose-500/10 transition-colors disabled:opacity-40"
                        title="Remove agent"
                      >
                        {removing === node.id
                          ? <Loader size={14} className="animate-spin" />
                          : <Trash2 size={14} />}
                      </button>
                    )}
                  </div>
                </div>

                {/* Details grid */}
                <div className="mt-3 grid grid-cols-2 sm:grid-cols-4 gap-2">
                  <div className="bg-slate-800/40 rounded-md px-2.5 py-1.5">
                    <span className="text-[10px] text-slate-600 uppercase tracking-wider block">Address</span>
                    <span className="text-xs text-slate-400 font-mono truncate">
                      {node.id === 'local' ? node.host : `${node.host}:${node.agent_port}`}
                    </span>
                  </div>
                  <div className="bg-slate-800/40 rounded-md px-2.5 py-1.5">
                    <span className="text-[10px] text-slate-600 uppercase tracking-wider block">Resources</span>
                    <span className="text-xs text-slate-400">
                      {node.total_memory
                        ? `${node.available_memory != null ? `${formatMem(node.available_memory)}/` : ''}${formatMem(node.total_memory)}`
                        : '—'}
                      {node.total_cpu ? ` · ${Math.round(node.total_cpu)} vCPU` : ''}
                    </span>
                  </div>
                  <div className="bg-slate-800/40 rounded-md px-2.5 py-1.5">
                    <span className="text-[10px] text-slate-600 uppercase tracking-wider block">Containers</span>
                    <span className="text-xs text-slate-400">{node.container_count || 0}</span>
                  </div>
                  <div className="bg-slate-800/40 rounded-md px-2.5 py-1.5">
                    <span className="text-[10px] text-slate-600 uppercase tracking-wider block">Disk</span>
                    <span className="text-xs text-slate-400">
                      {node.disk_free != null ? `${formatBytes(node.disk_free)} free` : '—'}
                    </span>
                  </div>
                </div>

                {/* Last seen + images row */}
                <div className="mt-2 flex items-center justify-between gap-2">
                  {node.last_seen_at && (
                    <span className="text-[10px] text-slate-600 flex items-center gap-1">
                      <Clock size={10} />
                      Last seen {timeAgo(node.last_seen_at)}
                    </span>
                  )}
                  {stats && stats.total_count > 0 && (
                    <div className="flex items-center gap-2 text-xs">
                      <span className="text-slate-500">
                        Images: {stats.total_count} ({formatBytes(stats.total_size)})
                      </span>
                      {stats.dangling_count > 0 && (
                        <span className="flex items-center gap-1.5">
                          <span className="text-amber-400">
                            {stats.dangling_count} dangling ({formatBytes(stats.dangling_size)})
                          </span>
                          <button
                            onClick={() =>
                              setPruneModal({
                                nodeId: node.id,
                                nodeName: node.name,
                                danglingCount: stats.dangling_count,
                                danglingSize: stats.dangling_size,
                                state: 'confirming',
                                reclaimed: null,
                                error: '',
                              })
                            }
                            disabled={pruning === node.id}
                            className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs font-medium bg-amber-500/15 text-amber-400 border border-amber-500/25 hover:bg-amber-500/25 disabled:opacity-40 transition-colors cursor-pointer"
                          >
                            <Trash2 size={11} />
                            {pruning === node.id ? 'Pruning…' : 'Prune'}
                          </button>
                        </span>
                      )}
                    </div>
                  )}
                </div>
              </div>
              );
            })}
          </div>
        )}
      </main>

      {showAdd && (
        <AddAgentWizard
          onClose={() => setShowAdd(false)}
          onAdded={load}
        />
      )}

      {deleteModal && (
        <div className="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50 p-4">
          <div className="bg-slate-900 border border-slate-700/60 rounded-xl w-full max-w-sm shadow-2xl">
            <div className="px-6 py-5 space-y-4">
              {deleteModal.state === 'confirming' && (
                <>
                  <div className="flex items-center gap-3">
                    <div className="w-9 h-9 rounded-lg bg-rose-500/10 border border-rose-500/20 flex items-center justify-center shrink-0">
                      <Trash2 size={16} className="text-rose-400" />
                    </div>
                    <div>
                      <h3 className="text-sm font-semibold text-slate-100">Remove agent</h3>
                      <p className="text-xs text-slate-500 mt-0.5">{deleteModal.nodeName}</p>
                    </div>
                  </div>
                  <p className="text-xs text-slate-400">
                    This will remove <span className="text-rose-400 font-medium">{deleteModal.nodeName}</span> from the dashboard. Any containers running on this agent will not be affected. This action cannot be undone.
                  </p>
                  <div className="flex gap-2">
                    <button
                      onClick={() => setDeleteModal(null)}
                      className="flex-1 py-2 rounded-lg text-sm text-slate-400 hover:text-slate-200 border border-slate-700 hover:border-slate-600 transition-colors"
                    >
                      Cancel
                    </button>
                    <button
                      onClick={() => handleRemove(deleteModal.nodeId)}
                      className="flex-1 py-2 rounded-lg text-sm font-medium bg-rose-500/80 text-white hover:bg-rose-500 transition-colors"
                    >
                      Remove
                    </button>
                  </div>
                </>
              )}

              {deleteModal.state === 'deleting' && (
                <div className="flex flex-col items-center py-4 gap-3">
                  <Loader size={24} className="text-rose-400 animate-spin" />
                  <p className="text-sm text-slate-400">Removing <span className="text-slate-200">{deleteModal.nodeName}</span>…</p>
                </div>
              )}

              {deleteModal.state === 'error' && (
                <>
                  <div className="flex flex-col items-center py-4 gap-3">
                    <div className="w-10 h-10 rounded-full bg-rose-500/10 border border-rose-500/20 flex items-center justify-center">
                      <XCircle size={20} className="text-rose-400" />
                    </div>
                    <div className="text-center">
                      <h3 className="text-sm font-semibold text-slate-100">Failed to remove agent</h3>
                      <p className="text-xs text-rose-400 mt-1">{deleteModal.error}</p>
                    </div>
                  </div>
                  <button
                    onClick={() => setDeleteModal(null)}
                    className="w-full py-2 rounded-lg text-sm font-medium bg-slate-800 text-slate-200 hover:bg-slate-700 transition-colors"
                  >
                    OK
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}

      {pruneModal && (
        <PruneModal
          nodeName={pruneModal.nodeName}
          danglingCount={pruneModal.danglingCount}
          danglingSize={pruneModal.danglingSize}
          state={pruneModal.state}
          reclaimed={pruneModal.reclaimed}
          error={pruneModal.error}
          onConfirm={handlePruneConfirm}
          onClose={() => setPruneModal(null)}
        />
      )}
    </div>
  );
}
