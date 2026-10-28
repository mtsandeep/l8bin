import { Copy, Database, Loader2, X } from 'lucide-react';
import { useState } from 'react';
import { type Node, duplicateProject } from '../../api';
import { useToast } from '../ToastContext';

interface Props {
  projectId: string;
  projectName: string;
  currentNodeId: string;
  nodes: Node[];
  sizeHint: string;
  serviceCount: number;
  volumeCount: number;
  hasUploadedImage: boolean;
  ephemeralGb?: number;
  onClose: () => void;
  onStarted: () => void;
  /** Fired on success only — closes the settings popover behind this modal. */
  onSucceeded: () => void;
}

const ID_PATTERN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

export default function DuplicateModal({
  projectId,
  projectName,
  currentNodeId,
  nodes,
  sizeHint,
  serviceCount,
  volumeCount,
  hasUploadedImage,
  ephemeralGb,
  onClose,
  onStarted,
  onSucceeded,
}: Props) {
  const [newId, setNewId] = useState(`${projectId}-copy`);
  const [name, setName] = useState('');
  const [targetId, setTargetId] = useState(currentNodeId);
  const [copyVolumes, setCopyVolumes] = useState(true);
  const [includeEnv, setIncludeEnv] = useState(true);
  const [startAfter, setStartAfter] = useState(true);
  const [duplicating, setDuplicating] = useState(false);
  const { showToast } = useToast();

  // Current node first (side-by-side copy), then other online nodes.
  const current = nodes.find((n) => n.id === currentNodeId) ?? null;
  const others = nodes.filter((n) => n.id !== currentNodeId && n.status === 'online');
  const sourceArch = current?.architecture ?? null;

  const idValid = ID_PATTERN.test(newId) && newId.length <= 63;
  const idTakenLocally = newId === projectId;

  const handleDuplicate = async () => {
    if (!idValid || idTakenLocally) return;
    setDuplicating(true);
    try {
      const resp = await duplicateProject(projectId, {
        new_id: newId || undefined,
        name: name.trim() || undefined,
        target_node_id: targetId,
        copy_volumes: copyVolumes,
        include_env: includeEnv,
        start_after: startAfter,
      });
      const message = [`Duplicating as ${resp.project_id}`, ...resp.warnings].join('; ');
      showToast(message, 'success');
      onSucceeded();
      onStarted();
      onClose();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Duplication failed');
    } finally {
      setDuplicating(false);
    }
  };

  const nodeButton = (node: Node, isCurrent: boolean) => {
    const archBlocked = hasUploadedImage && !isCurrent && !!sourceArch && !!node.architecture && node.architecture !== sourceArch;
    const selected = targetId === node.id;
    return (
      <button
        key={node.id}
        type="button"
        onClick={() => !archBlocked && setTargetId(node.id)}
        disabled={archBlocked}
        title={
          archBlocked
            ? `Different architecture (${node.architecture} vs ${sourceArch}) — uploaded images can't run there; redeploy from source instead`
            : sourceArch && node.architecture && node.architecture !== sourceArch
              ? `Different architecture (${node.architecture} vs ${sourceArch}) — registry images must be multi-arch`
              : undefined
        }
        className={`w-full flex items-center justify-between px-3 py-2 rounded-md text-xs border transition-colors ${
          archBlocked
            ? 'bg-slate-900/30 border-slate-800/40 text-slate-600 cursor-not-allowed opacity-60'
            : selected
              ? 'bg-sky-500/10 border-sky-500/40 text-sky-200 cursor-pointer'
              : 'bg-slate-800/40 border-slate-700/40 text-slate-300 hover:border-slate-600/50 cursor-pointer'
        }`}
      >
        <span className="font-medium">
          {node.name || node.id}
          {isCurrent && <span className="text-slate-500 font-normal"> · current</span>}
        </span>
        <span className="text-slate-500 font-mono text-[10px]">
          {node.architecture || '?'}
          {node.available_memory != null && ` · ${Math.round(node.available_memory / 1024 / 1024)}MB free`}
        </span>
      </button>
    );
  };

  return (
    <div
      data-l8b-modal
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    >
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg p-5 max-w-md w-full mx-4 shadow-xl max-h-[90vh] overflow-y-auto">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <Copy size={14} className="text-sky-400" />
            <h3 className="text-sm font-semibold text-slate-100">Duplicate project</h3>
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
          Copy <span className="text-slate-200 font-medium">{projectName}</span>
          {serviceCount > 1 && <span className="text-slate-500"> ({serviceCount} services)</span>} to a new id. The
          original keeps running untouched.
        </p>

        <ol className="space-y-1.5 mb-4">
          {[
            'Files, volumes, and uploaded images copy to the target',
            'A new subdomain and fresh volumes are created for the copy',
            startAfter ? 'The copy starts alongside the original' : 'The copy is left stopped',
          ].map((step, i) => (
            <li key={i} className="flex items-start gap-2 text-[11px] text-slate-400">
              <span className="w-4 h-4 rounded-full bg-slate-800 border border-slate-700 text-slate-500 text-[9px] flex items-center justify-center shrink-0 mt-0.5">
                {i + 1}
              </span>
              {step}
            </li>
          ))}
        </ol>

        {ephemeralGb ? (
          <div className="rounded-md bg-amber-500/10 border border-amber-500/30 px-3 py-2 text-[11px] text-amber-300">
            {ephemeralGb.toFixed(2)} GB in anonymous volumes is not carried - name them in compose to persist.
          </div>
        ) : null}

        <div className="space-y-4">
          <div>
            <label className="block text-xs text-slate-400 mb-1.5">New project id</label>
            <input
              type="text"
              value={newId}
              onChange={(e) => setNewId(e.target.value.toLowerCase())}
              placeholder={`${projectId}-copy`}
              className={`w-full px-3 py-2 rounded-md text-xs font-mono bg-slate-800/60 border ${
                newId && (!idValid || idTakenLocally)
                  ? 'border-red-500/50 text-red-300'
                  : 'border-slate-700/60 text-slate-200'
              } focus:outline-none focus:border-sky-500/50`}
            />
            {newId && !idValid && (
              <p className="text-[10px] text-red-400 mt-1">
                Lowercase letters, digits, hyphens; 1–63 chars; no leading/trailing hyphens
              </p>
            )}
            {idTakenLocally && <p className="text-[10px] text-red-400 mt-1">Same as the original id</p>}
            {idValid && !idTakenLocally && (
              <p className="text-[10px] text-slate-500 mt-1">
                Subdomain: <span className="font-mono text-slate-400">{newId}.&lt;domain&gt;</span> — taken ids get a
                -2 suffix automatically
              </p>
            )}
          </div>

          <div>
            <label className="block text-xs text-slate-400 mb-1">Name (optional)</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={`${projectName} (copy)`}
              className="w-full px-3 py-2 rounded-md text-xs bg-slate-800/60 border border-slate-700/60 text-slate-200 focus:outline-none focus:border-sky-500/50"
            />
          </div>

          <div>
            <label className="block text-xs text-slate-400 mb-1.5">Target node</label>
            <div className="space-y-1">
              {current && nodeButton(current, true)}
              {others.map((node) => nodeButton(node, false))}
            </div>
          </div>

          {volumeCount > 0 && (
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={copyVolumes}
                onChange={(e) => setCopyVolumes(e.target.checked)}
                className="accent-sky-500"
              />
              <span className="text-xs text-slate-400 flex items-center gap-1">
                <Database size={11} className="text-slate-500" />
                Copy {volumeCount} volume{volumeCount > 1 ? 's' : ''} {copyVolumes ? '' : '(start fresh)'}
              </span>
            </label>
          )}

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={includeEnv}
              onChange={(e) => setIncludeEnv(e.target.checked)}
              className="accent-sky-500"
            />
            <span className="text-xs text-slate-400">Copy .env secrets {includeEnv ? '' : '(copy starts with a blank .env)'}</span>
          </label>

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={startAfter}
              onChange={(e) => setStartAfter(e.target.checked)}
              className="accent-sky-500"
            />
            <span className="text-xs text-slate-400">Start the copy when done</span>
          </label>

          {sizeHint && copyVolumes && (
            <div className="flex items-center gap-2 rounded-md bg-slate-800/60 border border-slate-700/40 px-3 py-2">
              <Database size={12} className="text-slate-500 shrink-0" />
              <p className="text-[11px] text-slate-400">{sizeHint}</p>
            </div>
          )}

          <button
            type="button"
            onClick={handleDuplicate}
            disabled={duplicating || !idValid || idTakenLocally || !targetId}
            className="w-full py-2 rounded-md text-xs font-medium bg-sky-600 text-white hover:bg-sky-500 transition-colors disabled:opacity-50 cursor-pointer flex items-center justify-center gap-2"
          >
            {duplicating ? (
              <>
                <Loader2 size={12} className="animate-spin" />
                Starting duplication…
              </>
            ) : (
              'Duplicate'
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
