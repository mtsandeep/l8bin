import { ArrowRightLeft, Database, Loader2, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { type Node, migrateProject } from '../../api';
import { useToast } from '../ToastContext';

interface Props {
  projectId: string;
  projectName: string;
  currentNodeId: string;
  nodes: Node[];
  sizeHint: string;
  serviceCount: number;
  volumeCount: number;
  onClose: () => void;
  onStarted: () => void;
  /** Fired on success only — closes the settings popover behind this modal. */
  onSucceeded: () => void;
}

export default function MigrateModal({
  projectId,
  projectName,
  currentNodeId,
  nodes,
  sizeHint,
  serviceCount,
  volumeCount,
  onClose,
  onStarted,
  onSucceeded,
}: Props) {
  const sourceArch = nodes.find((n) => n.id === currentNodeId)?.architecture ?? null;
  const onlineNodes = nodes.filter((n) => n.id !== currentNodeId && n.status === 'online');
  const compatible = onlineNodes.filter(
    (n) => !sourceArch || !n.architecture || n.architecture === sourceArch,
  );
  const incompatible = onlineNodes.filter(
    (n) => sourceArch && n.architecture && n.architecture !== sourceArch,
  );
  const [targetId, setTargetId] = useState('');
  const [migrateVolumes, setMigrateVolumes] = useState(true);
  const [migrating, setMigrating] = useState(false);
  const { showToast } = useToast();

  useEffect(() => {
    if (!targetId && compatible.length > 0) setTargetId(compatible[0].id);
  }, [compatible, targetId]);

  const handleMigrate = async () => {
    if (!targetId) return;
    setMigrating(true);
    try {
      const resp = await migrateProject(projectId, targetId, migrateVolumes);
      showToast(resp.message || 'Migration started', 'success');
      onSucceeded();
      onStarted();
      onClose();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Migration failed');
    } finally {
      setMigrating(false);
    }
  };

  const nodeButton = (node: Node, disabled: boolean) => (
    <button
      key={node.id}
      type="button"
      onClick={() => !disabled && setTargetId(node.id)}
      disabled={disabled}
      title={
        disabled
          ? `Different architecture (${node.architecture} vs ${sourceArch}) — migrate to a same-arch node or redeploy from source`
          : undefined
      }
      className={`w-full flex items-center justify-between px-3 py-2 rounded-md text-xs border transition-colors ${
        disabled
          ? 'bg-slate-900/30 border-slate-800/40 text-slate-600 cursor-not-allowed opacity-60'
          : targetId === node.id
            ? 'bg-sky-500/10 border-sky-500/40 text-sky-200 cursor-pointer'
            : 'bg-slate-800/40 border-slate-700/40 text-slate-300 hover:border-slate-600/50 cursor-pointer'
      }`}
    >
      <span className="font-medium">{node.name || node.id}</span>
      <span className="text-slate-500 font-mono text-[10px]">
        {node.architecture || '?'}
        {node.available_memory != null && ` · ${Math.round(node.available_memory / 1024 / 1024)}MB free`}
      </span>
    </button>
  );

  return (
    <div
      data-l8b-modal
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    >
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg p-5 max-w-md w-full mx-4 shadow-xl">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <ArrowRightLeft size={14} className="text-sky-400" />
            <h3 className="text-sm font-semibold text-slate-100">Migrate project</h3>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-1 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
          >
            <X size={14} />
          </button>
        </div>

        <p className="text-xs text-slate-400 mb-3">
          Move <span className="text-slate-200 font-medium">{projectName}</span>
          {serviceCount > 1 && <span className="text-slate-500"> ({serviceCount} services)</span>} to another node.
        </p>

        {/* Steps */}
        <ol className="space-y-1.5 mb-4">
          {[
            'Project stops on the current node',
            migrateVolumes && volumeCount > 0
              ? `Files and ${volumeCount} volume${volumeCount > 1 ? 's' : ''} transfer to the target`
              : 'Files transfer to the target',
            'Project restarts on the target node',
          ].map((step, i) => (
            <li key={i} className="flex items-start gap-2 text-[11px] text-slate-400">
              <span className="w-4 h-4 rounded-full bg-slate-800 border border-slate-700 text-slate-500 text-[9px] flex items-center justify-center shrink-0 mt-0.5">
                {i + 1}
              </span>
              {step}
            </li>
          ))}
        </ol>

        {sizeHint && (
          <div className="flex items-center gap-2 rounded-md bg-slate-800/60 border border-slate-700/40 px-3 py-2 mb-4">
            <Database size={12} className="text-slate-500 shrink-0" />
            <p className="text-[11px] text-slate-400">
              {sizeHint}
              {volumeCount === 0 && <span className="text-slate-600"> · no volumes</span>}
            </p>
          </div>
        )}

        {onlineNodes.length === 0 ? (
          <p className="text-xs text-slate-500 py-4 text-center">
            No other online nodes available. Add a node first.
          </p>
        ) : (
          <div className="space-y-4">
            <div>
              <label className="block text-xs text-slate-400 mb-1.5">Target node</label>
              <div className="space-y-1">
                {compatible.map((node) => nodeButton(node, false))}
                {incompatible.map((node) => nodeButton(node, true))}
              </div>
            </div>

            {volumeCount > 0 && (
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={migrateVolumes}
                  onChange={(e) => setMigrateVolumes(e.target.checked)}
                  className="accent-sky-500"
                />
                <span className="text-xs text-slate-400 flex items-center gap-1">
                  <Database size={11} className="text-slate-500" />
                  Move volume data {migrateVolumes ? '(included above)' : '(start fresh)'}
                </span>
              </label>
            )}

            <button
              type="button"
              onClick={handleMigrate}
              disabled={migrating || !targetId}
              className="w-full py-2 rounded-md text-xs font-medium bg-sky-600 text-white hover:bg-sky-500 transition-colors disabled:opacity-50 cursor-pointer flex items-center justify-center gap-2"
            >
              {migrating ? (
                <>
                  <Loader2 size={12} className="animate-spin" />
                  Starting migration…
                </>
              ) : !targetId ? (
                'No compatible target nodes'
              ) : (
                'Migrate'
              )}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
