import {
  ArrowRight,
  CheckSquare,
  Copy,
  Download,
  FileArchive,
  FolderOpen,
  ListChecks,
  Loader2,
  RefreshCw,
  Square,
  Trash2,
  Upload,
  X,
} from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  type ExportJobInfo,
  type ExportSummary,
  type ImportArchiveSummary,
  type Project,
  deleteExport,
  downloadExport,
  exportProject,
  fetchProjects,
  getExportJob,
  importBundle,
  listExports,
  listImportArchives,
  uploadImportArchive,
} from '../../api';
import { useNodes } from '../../hooks/useNodes';
import { useAuth } from '../AuthContext';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

const ID_PATTERN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/** Poll one export job to a terminal state (bounded). */
async function waitForJob(jobId: string, onTick?: (job: ExportJobInfo) => void): Promise<ExportJobInfo> {
  for (let i = 0; i < 600; i++) {
    const job = await getExportJob(jobId);
    onTick?.(job);
    if (job.status !== 'running') return job;
    await new Promise((r) => setTimeout(r, 1000));
  }
  return { ...({} as ExportJobInfo), status: 'failed', error: 'timed out waiting' } as ExportJobInfo;
}

export default function TransferPanel() {
  // Export
  const [projects, setProjects] = useState<Project[]>([]);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [showPicker, setShowPicker] = useState(false);
  const [includeEnv, setIncludeEnv] = useState(true);
  const [includeVolumes, setIncludeVolumes] = useState(true);
  const [exportingNow, setExportingNow] = useState<string | null>(null);
  const [exportLog, setExportLog] = useState<{ id: string; state: 'running' | 'done' | 'failed'; note?: string }[]>([]);
  const [exports, setExports] = useState<ExportSummary[]>([]);
  const [exportsFolder, setExportsFolder] = useState<string | null>(null);
  const [exportsLoading, setExportsLoading] = useState(false);

  // Import
  const [archives, setArchives] = useState<ImportArchiveSummary[]>([]);
  const [importsFolder, setImportsFolder] = useState<string | null>(null);
  const [archivesLoading, setArchivesLoading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [selected, setSelected] = useState<ImportArchiveSummary | null>(null);
  const [newId, setNewId] = useState('');
  const [targetId, setTargetId] = useState('local');
  const [importEnv, setImportEnv] = useState(true);
  const [importVolumes, setImportVolumes] = useState(true);
  const [startAfter, setStartAfter] = useState(true);
  const [deleteAfter, setDeleteAfter] = useState(false);
  const [importing, setImporting] = useState(false);

  const { user } = useAuth();
  const { nodes } = useNodes(user);
  const { showToast } = useToast();

  const refreshExports = useCallback(() => {
    setExportsLoading(true);
    listExports()
      .then((r) => {
        setExports(r.exports);
        setExportsFolder(r.folder_path);
      })
      .catch(() => {})
      .finally(() => setExportsLoading(false));
  }, []);

  const refreshArchives = useCallback(() => {
    setArchivesLoading(true);
    listImportArchives()
      .then((r) => {
        setArchives(r.archives);
        setImportsFolder(r.folder_path);
      })
      .catch(() => {})
      .finally(() => setArchivesLoading(false));
  }, []);

  useEffect(() => {
    fetchProjects().then(setProjects).catch(() => {});
    refreshExports();
    refreshArchives();
  }, [refreshExports, refreshArchives]);

  const toggle = (id: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const exportable = (p: Project) =>
    !['deploying', 'migrating', 'exporting', 'stopping', 'importing'].includes(p.status);

  const handleExportSelected = async () => {
    const queue = projects.filter((p) => checked.has(p.id) && exportable(p));
    if (queue.length === 0) return;
    setExportLog(queue.map((p) => ({ id: p.id, state: 'running' as const })));
    let failures = 0;
    for (const p of queue) {
      setExportingNow(p.id);
      try {
        const started = await exportProject(p.id, { include_env: includeEnv, include_volumes: includeVolumes });
        const job = await waitForJob(started.job_id);
        setExportLog((prev) =>
          prev.map((l) =>
            l.id === p.id
              ? job.status === 'completed'
                ? { ...l, state: 'done', note: job.artifact ?? undefined }
                : { ...l, state: 'failed', note: job.error ?? 'failed' }
              : l,
          ),
        );
        if (job.status !== 'completed') failures += 1;
      } catch (e) {
        failures += 1;
        setExportLog((prev) =>
          prev.map((l) => (l.id === p.id ? { ...l, state: 'failed', note: e instanceof Error ? e.message : 'failed' } : l)),
        );
      }
    }
    setExportingNow(null);
    refreshExports();
    showToast(
      failures === 0 ? `Exported ${queue.length} project(s)` : `${failures}/${queue.length} export(s) failed`,
      failures === 0 ? 'success' : undefined,
    );
  };

  const handleUpload = async (file: File) => {
    setUploading(true);
    try {
      const resp = await uploadImportArchive(file);
      showToast(`Uploaded ${resp.file}`, 'success');
      refreshArchives();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Upload failed');
    } finally {
      setUploading(false);
    }
  };

  const handleImport = async () => {
    if (!selected) return;
    setImporting(true);
    try {
      const resp = await importBundle({
        file: selected.file,
        new_id: newId.trim() || undefined,
        target_node_id: targetId,
        include_env: importEnv,
        include_volumes: importVolumes,
        start_after_import: startAfter,
        delete_after_import: deleteAfter,
      });
      const message = [`Importing as ${resp.project_id}`, ...resp.warnings].join('; ');
      showToast(message, 'success');
      refreshArchives();
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Import failed');
    } finally {
      setImporting(false);
    }
  };

  const onlineNodes = nodes.filter((n) => n.status === 'online');
  const idValid = !newId || (ID_PATTERN.test(newId) && newId.length <= 63);

  return (
    <div className="space-y-6">
      <SettingsCard
        icon={FileArchive}
        title="Export projects"
        description="Full project bundles. Each project stops briefly for a consistent snapshot."
      >
        <div className="space-y-3">
          <div className="flex items-center justify-between gap-2">
            <label className="flex items-center gap-2 cursor-pointer text-xs text-slate-400">
              <input
                type="checkbox"
                checked={includeVolumes}
                onChange={(e) => setIncludeVolumes(e.target.checked)}
                className="accent-violet-500"
              />
              volume data
            </label>
            <label className="flex items-center gap-2 cursor-pointer text-xs text-slate-400">
              <input
                type="checkbox"
                checked={includeEnv}
                onChange={(e) => setIncludeEnv(e.target.checked)}
                className="accent-violet-500"
              />
              .env secrets
              {includeEnv && <span className="text-amber-400/70 text-[10px]">(bundle = secret)</span>}
            </label>
            <button
              type="button"
              onClick={handleExportSelected}
              disabled={exportingNow !== null || checked.size === 0}
              className="ml-auto inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
            >
              {exportingNow ? <Loader2 size={12} className="animate-spin" /> : <FileArchive size={12} />}
              {exportingNow ? `Exporting ${exportingNow}…` : 'Export selected'}
            </button>
          </div>

          <div className="flex items-center justify-between gap-2 rounded-md border border-slate-700/40 px-3 py-2">
            <p className="text-xs text-slate-400 truncate">
              {checked.size === 0 ? (
                'No projects selected'
              ) : checked.size === 1 ? (
                <>
                  <span className="font-mono text-slate-200">{[...checked][0]}</span> selected
                </>
              ) : (
                `${checked.size} projects selected`
              )}
            </p>
            <button
              type="button"
              onClick={() => setShowPicker(true)}
              disabled={exportingNow !== null}
              className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-[11px] font-medium text-violet-300 hover:text-violet-200 hover:bg-violet-500/10 transition-colors cursor-pointer disabled:opacity-50 shrink-0"
            >
              <ListChecks size={12} />
              Pick projects
            </button>
          </div>

          {exportLog.length > 0 && (
            <div className="rounded-md bg-slate-800/40 border border-slate-700/40 px-3 py-2 space-y-1">
              {exportLog.map((l) => (
                <p
                  key={l.id}
                  className={`text-[11px] flex items-center gap-1.5 ${
                    l.state === 'done' ? 'text-emerald-400' : l.state === 'failed' ? 'text-red-400' : 'text-slate-400'
                  }`}
                >
                  {l.state === 'running' && <Loader2 size={10} className="animate-spin" />}
                  <span className="font-mono">{l.id}</span>
                  {l.note && <span className="text-slate-500 truncate">{l.note}</span>}
                </p>
              ))}
            </div>
          )}

          <div>
            <div className="flex items-center justify-between mb-1.5">
              <div className="flex items-center gap-1.5 text-xs text-slate-400">
                <FolderOpen size={12} className="text-slate-500" />
                exports folder{' '}
                <span className="text-slate-600 font-mono text-[10px]">{exportsFolder ?? '~/litebin/exports/'}</span>
              </div>
              <button
                type="button"
                onClick={refreshExports}
                className="p-1 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
              >
                {exportsLoading ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
              </button>
            </div>
            <div className="rounded-md border border-slate-700/40 divide-y divide-slate-800">
              {exports.map((exp) => (
                <div key={exp.file} className="flex items-center justify-between gap-2 px-3 py-2">
                  <div className="min-w-0">
                    <p className="text-[11px] text-slate-300 font-mono truncate">{exp.file}</p>
                    <p className="text-[10px] text-slate-500">
                      {formatSize(exp.size_bytes)} · {new Date(exp.created_at * 1000).toLocaleString()}
                    </p>
                  </div>
                  <div className="flex items-center gap-1 shrink-0">
                    <button
                      type="button"
                      onClick={async () => {
                        const path = exportsFolder ? `${exportsFolder}/${exp.file}` : `exports/${exp.file}`;
                        try {
                          await navigator.clipboard.writeText(path);
                          showToast(`Copied ${path}`, 'success');
                        } catch {
                          showToast(path);
                        }
                      }}
                      className="p-1.5 rounded text-slate-500 hover:text-violet-400 hover:bg-slate-800 transition-colors cursor-pointer"
                      title="Copy server path"
                    >
                      <Copy size={12} />
                    </button>
                    <button
                      type="button"
                      onClick={() => downloadExport(exp.file).catch((e) => showToast(String(e)))}
                      className="p-1.5 rounded text-slate-500 hover:text-teal-400 hover:bg-slate-800 transition-colors cursor-pointer"
                      title="Download"
                    >
                      <Download size={12} />
                    </button>
                    <button
                      type="button"
                      onClick={async () => {
                        try {
                          await deleteExport(exp.file);
                          setExports((prev) => prev.filter((x) => x.file !== exp.file));
                        } catch (e) {
                          showToast(e instanceof Error ? e.message : 'Delete failed');
                        }
                      }}
                      className="p-1.5 rounded text-slate-500 hover:text-red-400 hover:bg-slate-800 transition-colors cursor-pointer"
                      title="Delete"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>
              ))}
              {exports.length === 0 && (
                <p className="text-xs text-slate-600 px-3 py-4 text-center">No export bundles yet</p>
              )}
            </div>
          </div>
        </div>
      </SettingsCard>

      <SettingsCard
        icon={ArrowRight}
        title="Import from bundle"
        description="Recreate a bundled project on this master."
      >
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <label className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium bg-slate-800 text-slate-200 hover:bg-slate-700 border border-slate-700/60 transition-colors cursor-pointer">
              {uploading ? <Loader2 size={12} className="animate-spin" /> : <Upload size={12} />}
              Upload bundle
              <input
                type="file"
                accept=".tar.gz,.tgz,.tar,.l8b.tar.gz"
                className="hidden"
                disabled={uploading}
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) handleUpload(f);
                  e.target.value = '';
                }}
              />
            </label>
            <span className="text-[10px] text-slate-600">
              up to 4 GB per upload · exported bundles can be placed directly in{' '}
              <span className="font-mono">{importsFolder ?? '~/litebin/imports/'}</span>
            </span>
            <button
              type="button"
              onClick={refreshArchives}
              className="ml-auto p-1 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
            >
              {archivesLoading ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
            </button>
          </div>

          <div className="rounded-md border border-slate-700/40 divide-y divide-slate-800 max-h-48 overflow-y-auto">
            {archives.map((a) => (
              <button
                key={a.file}
                type="button"
                onClick={() => setSelected(a)}
                className={`w-full flex items-center justify-between gap-2 px-3 py-2 text-left transition-colors cursor-pointer ${
                  selected?.file === a.file ? 'bg-violet-500/10' : 'hover:bg-slate-800/50'
                }`}
              >
                <div className="min-w-0">
                  <p className="text-[11px] font-mono text-slate-200 truncate">{a.file}</p>
                  <p className="text-[10px] text-slate-500">
                    {formatSize(a.size_bytes)} · {new Date(a.created_at * 1000).toLocaleString()}
                  </p>
                </div>
                {selected?.file === a.file && <span className="text-[10px] text-violet-300 shrink-0">selected</span>}
              </button>
            ))}
            {archives.length === 0 && (
              <p className="text-xs text-slate-600 px-3 py-4 text-center">No bundles in the imports folder</p>
            )}
          </div>

          {selected && (
            <div className="rounded-md border border-slate-700/40 bg-slate-800/30 p-3 space-y-3">
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                <div>
                  <label className="block text-[11px] text-slate-400 mb-1">
                    new id <span className="text-slate-600">(blank = original)</span>
                  </label>
                  <input
                    type="text"
                    value={newId}
                    onChange={(e) => setNewId(e.target.value.toLowerCase())}
                    className={`w-full px-2.5 py-1.5 rounded text-xs font-mono bg-slate-800/60 border ${
                      !idValid ? 'border-red-500/50 text-red-300' : 'border-slate-700/60 text-slate-200'
                    } focus:outline-none focus:border-violet-500/50`}
                  />
                </div>
                <div>
                  <label className="block text-[11px] text-slate-400 mb-1">target node</label>
                  <select
                    value={targetId}
                    onChange={(e) => setTargetId(e.target.value)}
                    className="w-full px-2.5 py-1.5 rounded text-xs bg-slate-800/60 border border-slate-700/60 text-slate-200 focus:outline-none focus:border-violet-500/50 cursor-pointer"
                  >
                    {onlineNodes.map((n) => (
                      <option key={n.id} value={n.id}>
                        {n.name || n.id}
                        {n.id === 'local' ? ' (this server)' : ''}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
                <label className="flex items-center gap-1.5 cursor-pointer text-xs text-slate-400">
                  <input type="checkbox" checked={importVolumes} onChange={(e) => setImportVolumes(e.target.checked)} className="accent-violet-500" />
                  volumes
                </label>
                <label className="flex items-center gap-1.5 cursor-pointer text-xs text-slate-400">
                  <input type="checkbox" checked={importEnv} onChange={(e) => setImportEnv(e.target.checked)} className="accent-violet-500" />
                  .env
                </label>
                <label className="flex items-center gap-1.5 cursor-pointer text-xs text-slate-400">
                  <input type="checkbox" checked={startAfter} onChange={(e) => setStartAfter(e.target.checked)} className="accent-violet-500" />
                  start
                </label>
                <label className="flex items-center gap-1.5 cursor-pointer text-xs text-slate-400">
                  <input type="checkbox" checked={deleteAfter} onChange={(e) => setDeleteAfter(e.target.checked)} className="accent-violet-500" />
                  delete file after
                </label>
                <button
                  type="button"
                  onClick={handleImport}
                  disabled={importing || !idValid}
                  className="ml-auto inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {importing ? <Loader2 size={12} className="animate-spin" /> : <ArrowRight size={12} />}
                  {importing ? 'Importing…' : 'Import'}
                </button>
              </div>
            </div>
          )}
        </div>
      </SettingsCard>

      {showPicker && (
        <div
          data-l8b-modal
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
        >
          <div className="bg-slate-900 border border-slate-700/50 rounded-lg p-5 max-w-md w-full mx-4 shadow-xl">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-2">
                <ListChecks size={14} className="text-violet-400" />
                <h3 className="text-sm font-semibold text-slate-100">Pick projects to export</h3>
              </div>
              <button
                type="button"
                onClick={() => setShowPicker(false)}
                className="p-1 rounded text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
              >
                <X size={14} />
              </button>
            </div>

            <div className="max-h-72 overflow-y-auto rounded-md border border-slate-700/40 divide-y divide-slate-800 mb-4">
              {projects.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  disabled={!exportable(p)}
                  onClick={() => toggle(p.id)}
                  className={`w-full flex items-center gap-2.5 px-3 py-2 text-left transition-colors ${
                    exportable(p) ? 'hover:bg-slate-800/50 cursor-pointer' : 'opacity-40 cursor-not-allowed'
                  }`}
                >
                  {checked.has(p.id) ? (
                    <CheckSquare size={13} className="text-violet-400 shrink-0" />
                  ) : (
                    <Square size={13} className="text-slate-600 shrink-0" />
                  )}
                  <span className="text-xs text-slate-200 font-mono">{p.id}</span>
                  <span className="text-[11px] text-slate-500 truncate">{p.name ?? ''}</span>
                  <span className="ml-auto text-[10px] text-slate-600 shrink-0">{p.status}</span>
                </button>
              ))}
              {projects.length === 0 && <p className="text-xs text-slate-600 px-3 py-4 text-center">No projects</p>}
            </div>

            <button
              type="button"
              onClick={() => setShowPicker(false)}
              className="w-full py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
            >
              Done{checked.size > 0 ? ` (${checked.size})` : ''}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
