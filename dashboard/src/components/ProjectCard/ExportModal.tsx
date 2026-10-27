import { Database, Download, FileArchive, Loader2, X } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { type ExportJobInfo, downloadExport, exportProject, getExportJob } from '../../api';
import { useToast } from '../ToastContext';

interface Props {
  projectId: string;
  projectName: string;
  volumeCount: number;
  onClose: () => void;
  /** Fired when the export job completes — closes the settings popover behind this modal. */
  onSucceeded: () => void;
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

export default function ExportModal({ projectId, projectName, volumeCount, onClose, onSucceeded }: Props) {
  const [includeEnv, setIncludeEnv] = useState(true);
  const [includeVolumes, setIncludeVolumes] = useState(true);
  const [job, setJob] = useState<ExportJobInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const { showToast } = useToast();

  const stopPolling = useCallback(() => {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  useEffect(() => stopPolling, [stopPolling]);

  const handleExport = async () => {
    setBusy(true);
    try {
      const resp = await exportProject(projectId, { include_env: includeEnv, include_volumes: includeVolumes });
      const started = await getExportJob(resp.job_id);
      setJob(started);
      showToast('Export started — project restarts briefly for a consistent snapshot', 'info');
      stopPolling();
      pollRef.current = setInterval(async () => {
        try {
          const current = await getExportJob(resp.job_id);
          setJob(current);
          if (current.status !== 'running') {
            stopPolling();
            if (current.status === 'completed') {
              onSucceeded();
              showToast(`Export ready: ${current.artifact}`, 'success');
            } else {
              showToast(`Export failed: ${current.error ?? 'unknown error'}`);
            }
          }
        } catch {
          stopPolling();
        }
      }, 2000);
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Export failed');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      data-l8b-modal
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
    >
      <div className="bg-slate-900 border border-slate-700/50 rounded-lg p-5 max-w-md w-full mx-4 shadow-xl max-h-[90vh] overflow-y-auto">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <FileArchive size={14} className="text-teal-400" />
            <h3 className="text-sm font-semibold text-slate-100">Export project</h3>
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
          Bundle <span className="text-slate-200 font-medium">{projectName}</span> as a portable{' '}
          <span className="font-mono text-slate-300">.l8b.tar.gz</span> — full backup that imports on any LiteBin
          master.
        </p>

        <ol className="space-y-1.5 mb-4">
          {[
            'Project stops briefly for a consistent snapshot',
            includeVolumes && volumeCount > 0
              ? `Files, ${volumeCount} volume${volumeCount > 1 ? 's' : ''}, and uploaded images are bundled`
              : 'Files and uploaded images are bundled',
            'Project restarts; the bundle lands in the exports folder',
          ].map((step, i) => (
            <li key={i} className="flex items-start gap-2 text-[11px] text-slate-400">
              <span className="w-4 h-4 rounded-full bg-slate-800 border border-slate-700 text-slate-500 text-[9px] flex items-center justify-center shrink-0 mt-0.5">
                {i + 1}
              </span>
              {step}
            </li>
          ))}
        </ol>

        <div className="space-y-4">
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={includeVolumes}
              onChange={(e) => setIncludeVolumes(e.target.checked)}
              className="accent-teal-500"
            />
            <span className="text-xs text-slate-400 flex items-center gap-1">
              <Database size={11} className="text-slate-500" />
              Include volume data {includeVolumes ? '' : '(config-only backup)'}
            </span>
          </label>

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={includeEnv}
              onChange={(e) => setIncludeEnv(e.target.checked)}
              className="accent-teal-500"
            />
            <span className="text-xs text-slate-400">
              Include .env secrets{' '}
              {includeEnv ? (
                <span className="text-amber-400/80">(bundle contains secrets — treat carefully)</span>
              ) : (
                '(import creates a blank .env)'
              )}
            </span>
          </label>

          {job && (
            <div className="rounded-md bg-slate-800/60 border border-slate-700/40 px-3 py-2">
              {job.status === 'running' ? (
                <p className="text-[11px] text-slate-400 flex items-center gap-2">
                  <Loader2 size={11} className="animate-spin" />
                  Writing bundle…
                </p>
              ) : job.status === 'completed' ? (
                <div className="flex items-center justify-between gap-2">
                  <p className="text-[11px] text-emerald-400 truncate">
                    {job.artifact}
                    {job.size_bytes != null && ` · ${formatSize(job.size_bytes)}`}
                  </p>
                  {job.artifact && (
                    <button
                      type="button"
                      onClick={() => downloadExport(job.artifact!).catch((e) => showToast(String(e)))}
                      className="text-[11px] text-teal-400 hover:text-teal-300 transition-colors cursor-pointer flex items-center gap-1 shrink-0"
                    >
                      <Download size={11} />
                      Download
                    </button>
                  )}
                </div>
              ) : (
                <p className="text-[11px] text-red-400">Failed: {job.error ?? 'unknown error'}</p>
              )}
            </div>
          )}

          <button
            type="button"
            onClick={handleExport}
            disabled={busy || job?.status === 'running'}
            className="w-full py-2 rounded-md text-xs font-medium bg-teal-600 text-white hover:bg-teal-500 transition-colors disabled:opacity-50 cursor-pointer flex items-center justify-center gap-2"
          >
            {busy || job?.status === 'running' ? (
              <>
                <Loader2 size={12} className="animate-spin" />
                Exporting…
              </>
            ) : (
              'Export bundle'
            )}
          </button>

          <p className="text-[10px] text-slate-600 text-center">
            All bundles live in Settings → Export / Import (downloads, deletes, bulk export)
          </p>
        </div>
      </div>
    </div>
  );
}
