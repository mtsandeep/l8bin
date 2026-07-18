import {
  AlertTriangle,
  ArrowLeft,
  Check,
  CheckCircle,
  CheckSquare,
  ChevronRight,
  Edit2,
  Globe,
  HardDrive,
  Info,
  Layers,
  Loader2,
  Package,
  RefreshCw,
  Search,
  Server,
  Square,
  X,
  XCircle,
} from 'lucide-react';
import { useCallback, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  DeployType,
  type ImportedGroup,
  type ImportGroupPayload,
  importContainers,
  type ScanContainer,
  type ScanGroup,
  scanContainers,
  updateProjectSettings,
} from '../api';
import { useNodes } from '../hooks/useNodes';
import { useAuth } from './AuthContext';

type Step = 'scan' | 'select' | 'review' | 'result';

function hasDockerSock(containers: ScanContainer[]): boolean {
  return containers.some((c) =>
    c.volumes.some((v) => v.source.endsWith('/docker.sock') || v.destination.endsWith('/docker.sock')),
  );
}

interface EditableGroup {
  originalGroup: ScanGroup;
  nodeId: string;
  nodeName: string;
  projectId: string;
  projectIdError?: string;
  publicService: string;
  name: string;
  description: string;
  allowRawPorts: boolean;
  hasDockerSocket: boolean;
  selected: boolean;
}

interface Props {
  onBack: () => void;
  onDone: () => void;
}

function StateChip({ state }: { state: string }) {
  const color =
    state === 'running'
      ? 'text-emerald-400 bg-emerald-400/10'
      : state === 'exited'
        ? 'text-slate-400 bg-slate-400/10'
        : 'text-amber-400 bg-amber-400/10';
  return <span className={`text-[10px] px-1.5 py-0.5 rounded font-mono ${color}`}>{state}</span>;
}

function NodeBadge({ name, isLocal }: { name: string; isLocal: boolean }) {
  return (
    <span
      className={`inline-flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full font-medium border ${
        isLocal
          ? 'bg-violet-500/10 border-violet-500/30 text-violet-300'
          : 'bg-blue-500/10 border-blue-500/30 text-blue-300'
      }`}
    >
      <Server size={9} />
      {isLocal ? 'local' : name}
    </span>
  );
}

export default function ScanImportPage({ onBack, onDone }: Props) {
  const { user } = useAuth();
  const { nodes } = useNodes(user);
  const [searchParams] = useSearchParams();
  const filterNodeId = searchParams.get('node') || undefined;
  const filterNodeName = filterNodeId
    ? filterNodeId === 'local'
      ? 'local'
      : (nodes.find((n) => n.id === filterNodeId)?.name ?? filterNodeId)
    : undefined;
  const isSingleScan = !!filterNodeId;
  const [step, setStep] = useState<Step>('scan');
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState('');
  const [groups, setGroups] = useState<EditableGroup[]>([]);
  const [importing, setImporting] = useState(false);
  const [importResult, setImportResult] = useState<{ imported: ImportedGroup[]; errors: string[] } | null>(null);
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState<Partial<EditableGroup>>({});

  // ── Step 1: Scan ───────────────────────────────────────────────────────────
  const runScan = useCallback(async () => {
    setScanning(true);
    setScanError('');
    try {
      const result = await scanContainers(filterNodeId ?? undefined);

      // Build editable groups
      const built: EditableGroup[] = [];

      // Local groups
      for (const g of result.local) {
        const pub = g.containers.find((c) => c.suggested_public)?.service_name ?? g.containers[0]?.service_name ?? '';
        built.push({
          originalGroup: g,
          nodeId: 'local',
          nodeName: 'local',
          projectId: g.suggested_project_id,
          publicService: pub,
          name: '',
          description: '',
          allowRawPorts: false,
          hasDockerSocket: hasDockerSock(g.containers),
          selected: !hasDockerSock(g.containers),
        });
      }

      // Agent node groups
      for (const [nodeId, nodeGroups] of Object.entries(result.nodes)) {
        const node = nodes.find((n) => n.id === nodeId);
        const nodeName = node?.name ?? nodeId;
        for (const g of nodeGroups) {
          const pub = g.containers.find((c) => c.suggested_public)?.service_name ?? g.containers[0]?.service_name ?? '';
          built.push({
            originalGroup: g,
            nodeId,
            nodeName,
            projectId: g.suggested_project_id,
            publicService: pub,
            name: '',
            description: '',
            allowRawPorts: false,
            hasDockerSocket: hasDockerSock(g.containers),
            selected: !hasDockerSock(g.containers),
          });
        }
      }

      setGroups(built);
      setStep(built.length > 0 ? 'select' : 'scan');
      if (built.length === 0)
        setScanError('No foreign containers found — all containers appear to be managed by LiteBin already.');
    } catch (e: unknown) {
      setScanError(e instanceof Error ? e.message : 'Scan failed');
    } finally {
      setScanning(false);
    }
  }, [filterNodeId, nodes]);

  // ── Step 2: Selection ──────────────────────────────────────────────────────
  const toggleSelect = (idx: number) =>
    setGroups((gs) =>
      gs.map((g, i) => (i === idx && !g.hasDockerSocket ? { ...g, selected: !g.selected } : g)),
    );

  const toggleAll = () => {
    const importable = groups.filter((g) => !g.hasDockerSocket);
    const allSelected = importable.every((g) => g.selected);
    setGroups((gs) => gs.map((g) => ({ ...g, selected: g.hasDockerSocket ? false : !allSelected })));
  };

  const selectedCount = groups.filter((g) => g.selected).length;

  // ── Step 3: Review / Edit ──────────────────────────────────────────────────
  const startEdit = (idx: number) => {
    setEditDraft({ ...groups[idx] });
    setEditingIdx(idx);
  };

  const saveEdit = () => {
    if (editingIdx === null) return;
    const pid = (editDraft.projectId ?? '').trim();
    const error = !pid
      ? 'Project ID required'
      : !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(pid)
        ? 'Lowercase letters, digits, hyphens only'
        : undefined;
    setGroups((gs) =>
      gs.map((g, i) => (i === editingIdx ? { ...g, ...editDraft, projectId: pid, projectIdError: error } : g)),
    );
    if (!error) setEditingIdx(null);
  };

  // ── Step 4: Import ─────────────────────────────────────────────────────────
  const runImport = async () => {
    const selected = groups.filter((g) => g.selected);
    if (selected.some((g) => g.projectIdError)) return;
    setImporting(true);
    try {
      const payload: ImportGroupPayload[] = selected.map((g) => ({
        node_id: g.nodeId,
        project_id: g.projectId,
        group_key: g.originalGroup.group_key,
        public_service: g.publicService || null,
        setup_routing: true,
        containers: g.originalGroup.containers,
        deploy_type: g.originalGroup.deploy_type,
        compose_working_dir: g.originalGroup.compose_working_dir,
        compose_file_found: g.originalGroup.compose_file_found,
        env_file_found: g.originalGroup.env_file_found,
        name: g.name || undefined,
        description: g.description || undefined,
      }));
      const result = await importContainers(payload);

      // Apply raw-ports setting for imported projects.
      const settingsPromises = selected
        .filter((g) => g.allowRawPorts)
        .map((g) =>
          updateProjectSettings(g.projectId, {
            allow_raw_ports: true,
          }).catch(() => {}),
        );
      await Promise.all(settingsPromises);

      setImportResult(result);
      setStep('result');
    } catch (e: unknown) {
      setImportResult({ imported: [], errors: [e instanceof Error ? e.message : 'Import failed'] });
      setStep('result');
    } finally {
      setImporting(false);
    }
  };

  // ── Render ─────────────────────────────────────────────────────────────────
  return (
    <div className="min-h-screen bg-slate-950 text-slate-200">
      {/* Header */}
      <header className="border-b border-slate-800/80 bg-slate-900/60 backdrop-blur-md sticky top-0 z-40">
        <div className="max-w-6xl mx-auto px-4 sm:px-6 py-3 flex items-center gap-3">
          <button
            type="button"
            onClick={onBack}
            className="p-1.5 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-800 transition-colors cursor-pointer"
          >
            <ArrowLeft size={18} />
          </button>
          <div>
            <h1 className="text-sm font-semibold text-slate-100">Import Existing Projects</h1>
            <p className="text-[11px] text-slate-500">Detect & migrate Docker containers into LiteBin</p>
          </div>
          {/* Step indicator */}
          <div className="ml-auto flex items-center gap-1.5 text-[11px]">
            {(['scan', 'select', 'review', 'result'] as Step[]).map((s, i) => (
              <div key={s} className="flex items-center gap-1.5">
                <div
                  className={`w-5 h-5 rounded-full flex items-center justify-center text-[10px] font-medium ${
                    step === s
                      ? 'bg-violet-600 text-white'
                      : ['scan', 'select', 'review', 'result'].indexOf(step) > i
                        ? 'bg-slate-700 text-slate-400'
                        : 'bg-slate-800 text-slate-600'
                  }`}
                >
                  {i + 1}
                </div>
                {i < 3 && <ChevronRight size={10} className="text-slate-700" />}
              </div>
            ))}
          </div>
        </div>
      </header>

      <main className="max-w-6xl mx-auto px-4 sm:px-6 py-8">
        {/* ── STEP 1: Scan ── */}
        {step === 'scan' && (
          <div className="flex flex-col items-center text-center py-16 gap-6">
            <div className="w-16 h-16 rounded-2xl bg-violet-600/10 border border-violet-500/20 flex items-center justify-center">
              <Search size={28} className="text-violet-400" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-slate-100 mb-1">
                {isSingleScan ? `Scan ${filterNodeName}` : 'Scan All Nodes'}
              </h2>
              <p className="text-sm text-slate-400 max-w-md">
                {isSingleScan
                  ? `Inspect Docker containers on ${filterNodeName} and group them by compose project or standalone service.`
                  : 'Inspect Docker containers across all online nodes and group them by compose project or standalone service.'}
              </p>
            </div>
            {scanError && (
              <div className="flex items-start gap-2 text-sm text-amber-300 bg-amber-400/5 border border-amber-400/20 rounded-lg px-4 py-3 max-w-md text-left">
                <Info size={14} className="mt-0.5 shrink-0" />
                {scanError}
              </div>
            )}
            <button
              type="button"
              onClick={runScan}
              disabled={scanning}
              className="inline-flex items-center gap-2 px-6 py-2.5 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
            >
              {scanning ? <Loader2 size={15} className="animate-spin" /> : <Search size={15} />}
              {scanning ? 'Scanning…' : isSingleScan ? `Scan ${filterNodeName}` : 'Scan All Nodes'}
            </button>
            {scanning && (
              <p className="text-xs text-slate-500 animate-pulse">
                {isSingleScan
                  ? `Inspecting containers on ${filterNodeName}…`
                  : 'Inspecting containers across all online nodes…'}
              </p>
            )}
          </div>
        )}

        {/* ── STEP 2: Select ── */}
        {step === 'select' && (
          <>
            {scanning && (
              <div className="fixed inset-0 z-50 bg-slate-950/70 backdrop-blur-sm flex flex-col items-center justify-center gap-3">
                <Loader2 size={24} className="animate-spin text-violet-400" />
                <span className="text-sm text-slate-300">
                  {isSingleScan ? `Re-scanning ${filterNodeName}…` : 'Re-scanning all nodes…'}
                </span>
              </div>
            )}
            <div className="space-y-6">
              <div className="flex items-center justify-between">
                <div>
                  <h2 className="text-base font-semibold text-slate-100">
                    Found {groups.length} project{groups.length !== 1 ? 's' : ''}
                  </h2>
                  <p className="text-xs text-slate-500 mt-0.5">Select which projects to import into LiteBin</p>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={runScan}
                    disabled={scanning}
                    className="p-1.5 text-slate-500 hover:text-slate-300 hover:bg-slate-800 rounded-md transition-colors cursor-pointer disabled:opacity-40"
                    title="Re-scan"
                  >
                    <RefreshCw size={14} className={scanning ? 'animate-spin' : ''} />
                  </button>
                  <button
                    type="button"
                    onClick={toggleAll}
                    disabled={scanning}
                    className="inline-flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 transition-colors cursor-pointer disabled:opacity-40"
                  >
                    {groups.filter((g) => !g.hasDockerSocket).every((g) => g.selected) ? (
                      <CheckSquare size={13} className="text-violet-400" />
                    ) : (
                      <Square size={13} />
                    )}
                    {groups.filter((g) => !g.hasDockerSocket).every((g) => g.selected)
                      ? 'Deselect all'
                      : 'Select all'}
                  </button>
                </div>
              </div>

              <div className="space-y-3">
                {groups.map((g, idx) => (
                  <GroupCard
                    key={`${g.nodeId}-${g.originalGroup.group_key}`}
                    group={g}
                    onToggle={() => toggleSelect(idx)}
                    selectable={!g.hasDockerSocket}
                  />
                ))}
              </div>

              <div className="flex items-center justify-end gap-3 pt-2">
                <span className="text-xs text-slate-500">{selectedCount} selected</span>
                <button
                  type="button"
                  onClick={() => setStep('review')}
                  disabled={selectedCount === 0 || scanning}
                  className="inline-flex items-center gap-1.5 px-5 py-2 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-40 cursor-pointer"
                >
                  Review & Import
                  <ChevronRight size={15} />
                </button>
              </div>
            </div>
          </>
        )}

        {/* ── STEP 3: Review / Edit ── */}
        {step === 'review' && (
          <div className="space-y-6">
            <div className="flex items-center justify-between">
              <div>
                <h2 className="text-base font-semibold text-slate-100">Review Import</h2>
                <p className="text-xs text-slate-500 mt-0.5">
                  Confirm project IDs and routing settings before importing
                </p>
              </div>
              <button
                type="button"
                onClick={() => setStep('select')}
                className="text-xs text-slate-400 hover:text-slate-200 cursor-pointer"
              >
                ← Back
              </button>
            </div>

            <div className="space-y-3">
              {groups
                .filter((g) => g.selected)
                .map((g, _idx) => {
                  const realIdx = groups.indexOf(g);
                  const isEditing = editingIdx === realIdx;
                  return (
                    <div
                      key={`${g.nodeId}-${g.originalGroup.group_key}`}
                      className={`rounded-xl border p-4 space-y-3 transition-colors ${
                        g.projectIdError ? 'border-amber-500/40 bg-amber-500/5' : 'border-slate-700/60 bg-slate-900/50'
                      }`}
                    >
                      {isEditing ? (
                        <EditForm
                          draft={editDraft}
                          services={g.originalGroup.containers.map((c) => c.service_name)}
                          onChange={(d) => setEditDraft((prev) => ({ ...prev, ...d }))}
                          onSave={saveEdit}
                          onCancel={() => setEditingIdx(null)}
                        />
                      ) : (
                        <>
                          <div className="flex items-start justify-between gap-3">
                            <div className="space-y-1">
                              <div className="flex items-center gap-2 flex-wrap">
                                <NodeBadge name={g.nodeName} isLocal={g.nodeId === 'local'} />
                                <TypeChip type={g.originalGroup.deploy_type} />
                                {g.originalGroup.compose_file_found && (
                                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-400/10 text-emerald-400 border border-emerald-400/20">
                                    compose.yaml found
                                  </span>
                                )}
                              </div>
                              <div className="flex items-baseline gap-2">
                                <span className="text-sm font-mono font-semibold text-slate-100">{g.projectId}</span>
                                <span className="text-xs text-slate-500">← {g.originalGroup.group_key}</span>
                              </div>
                              {(g.name || g.description) && (
                                <p className="text-xs text-slate-400">
                                  {g.name && <span className="text-slate-300">{g.name}</span>}
                                  {g.name && g.description && <span className="text-slate-600 mx-1">·</span>}
                                  {g.description && <span className="text-slate-500">{g.description}</span>}
                                </p>
                              )}
                              <p className="text-xs text-slate-500">
                                Public: <span className="text-slate-300">{g.publicService || '—'}</span>
                                {' · '}
                                {g.originalGroup.containers.length} service
                                {g.originalGroup.containers.length !== 1 ? 's' : ''}
                              </p>
                              {(g.allowRawPorts || g.hasDockerSocket) && (
                                <div className="flex items-center gap-2 flex-wrap">
                                  {g.allowRawPorts && (
                                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-sky-400/10 text-sky-300 border border-sky-400/20">
                                      raw ports
                                    </span>
                                  )}
                                  {g.hasDockerSocket && (
                                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-red-400/10 text-red-300 border border-red-400/20">
                                      Docker socket import blocked
                                    </span>
                                  )}
                                </div>
                              )}
                            </div>
                            <button
                              type="button"
                              onClick={() => startEdit(realIdx)}
                              className="p-1.5 text-slate-500 hover:text-violet-400 hover:bg-violet-400/10 rounded-md transition-colors cursor-pointer"
                            >
                              <Edit2 size={13} />
                            </button>
                          </div>
                          {g.projectIdError && (
                            <div className="flex items-center gap-1.5 text-xs text-amber-400">
                              <AlertTriangle size={12} /> {g.projectIdError}
                            </div>
                          )}
                          <ContainerTable containers={g.originalGroup.containers} />
                        </>
                      )}
                    </div>
                  );
                })}
            </div>

            <div className="flex items-center justify-end gap-3 pt-2">
              {groups.filter((g) => g.selected && g.projectIdError).length > 0 && (
                <span className="text-xs text-amber-400 flex items-center gap-1">
                  <AlertTriangle size={12} /> Fix errors before importing
                </span>
              )}
              <button
                type="button"
                onClick={runImport}
                disabled={importing || groups.filter((g) => g.selected && g.projectIdError).length > 0}
                className="inline-flex items-center gap-2 px-6 py-2 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-40 cursor-pointer"
              >
                {importing ? <Loader2 size={14} className="animate-spin" /> : <Check size={14} />}
                {importing
                  ? 'Importing…'
                  : `Import ${groups.filter((g) => g.selected).length} project${groups.filter((g) => g.selected).length !== 1 ? 's' : ''}`}
              </button>
            </div>
          </div>
        )}

        {/* ── STEP 4: Result ── */}
        {step === 'result' && importResult && (
          <div className="space-y-6">
            <div className="text-center py-4">
              {importResult.errors.length === 0 ? (
                <>
                  <CheckCircle size={40} className="text-emerald-400 mx-auto mb-3" />
                  <h2 className="text-lg font-semibold text-slate-100">Import Complete</h2>
                  <p className="text-sm text-slate-400 mt-1">
                    {importResult.imported.length} project{importResult.imported.length !== 1 ? 's' : ''} imported
                    successfully
                  </p>
                </>
              ) : (
                <>
                  <XCircle size={40} className="text-red-400 mx-auto mb-3" />
                  <h2 className="text-lg font-semibold text-slate-100">Import finished with errors</h2>
                  <p className="text-sm text-slate-400 mt-1">
                    {importResult.imported.length} succeeded, {importResult.errors.length} failed
                  </p>
                </>
              )}
            </div>

            {importResult.imported.map((imp) => (
              <div key={imp.project_id} className="rounded-xl border border-emerald-500/20 bg-emerald-500/5 p-4">
                <div className="flex items-center gap-2 mb-2">
                  <CheckCircle size={14} className="text-emerald-400" />
                  <span className="text-sm font-mono font-semibold text-slate-100">{imp.project_id}</span>
                  <span className="text-xs text-slate-500">on {imp.node_id}</span>
                </div>
                <p className="text-xs text-slate-400">
                  {imp.containers_imported.length} container{imp.containers_imported.length !== 1 ? 's' : ''} imported
                </p>
                {imp.warnings.length > 0 && (
                  <div className="mt-2 space-y-1">
                    {imp.warnings.map((w) => (
                      <div key={w} className="flex items-start gap-1.5 text-xs text-amber-400">
                        <AlertTriangle size={11} className="mt-0.5 shrink-0" /> {w}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ))}

            {importResult.errors.map((err) => (
              <div key={err} className="rounded-xl border border-red-500/20 bg-red-500/5 p-4">
                <div className="flex items-start gap-2 text-sm text-red-300">
                  <XCircle size={14} className="mt-0.5 shrink-0" /> {err}
                </div>
              </div>
            ))}

            <div className="flex justify-center pt-4">
              <button
                type="button"
                onClick={onDone}
                className="inline-flex items-center gap-2 px-6 py-2.5 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
              >
                Go to Dashboard
              </button>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}

// ── Sub-components ─────────────────────────────────────────────────────────────

function TypeChip({ type }: { type: string }) {
  const isCompose = type === DeployType.Compose;
  return (
    <span
      className={`inline-flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded font-medium ${
        isCompose ? 'bg-blue-400/10 text-blue-300' : 'bg-slate-400/10 text-slate-400'
      }`}
    >
      {isCompose ? <Layers size={9} /> : <Package size={9} />}
      {isCompose ? 'compose' : 'image'}
    </span>
  );
}

function ContainerTable({ containers }: { containers: ScanContainer[] }) {
  return (
    <div className="border-t border-slate-700/40 px-4 py-2">
      <div className="grid grid-cols-[minmax(100px,0.15fr)_minmax(140px,1fr)_minmax(60px,0.1fr)_minmax(100px,0.2fr)_minmax(60px,0.1fr)] gap-2 px-2 pb-1.5 text-[10px] text-slate-600 font-medium uppercase tracking-wider">
        <span>Service</span>
        <span>Image</span>
        <span>State</span>
        <span>Ports</span>
        <span>Volumes</span>
      </div>
      <div className="space-y-1">
        {containers.map((c) => (
          <div
            key={c.container_id}
            className="grid grid-cols-[minmax(100px,0.15fr)_minmax(140px,1fr)_minmax(60px,0.1fr)_minmax(100px,0.2fr)_minmax(60px,0.1fr)] items-center gap-2 px-2 py-1.5 rounded-lg text-xs hover:bg-slate-800/50"
          >
            <div className="flex items-center gap-1.5 min-w-0">
              {c.suggested_public && <Globe size={11} className="text-violet-400 shrink-0" />}
              <span className="font-medium text-slate-300 truncate">{c.service_name}</span>
            </div>
            <div className="min-w-0 flex items-center gap-1.5">
              <span className="font-mono text-slate-500 truncate" title={c.image}>
                {c.image}
              </span>
              {c.image_is_local && (
                <span className="shrink-0 text-[10px] px-1 py-0.5 rounded bg-amber-400/10 text-amber-400 border border-amber-400/20 flex items-center gap-0.5">
                  <AlertTriangle size={8} /> local
                </span>
              )}
            </div>
            <StateChip state={c.state} />
            <div className="flex items-center gap-1 flex-wrap">
              {c.ports.filter((p) => p.external).length > 0 ? (
                c.ports
                  .filter((p) => p.external)
                  .map((p) => (
                    <span key={`${p.external}-${p.internal}`} className="font-mono text-slate-400">
                      {p.external}:{p.internal}
                    </span>
                  ))
              ) : (
                <span className="text-slate-700">—</span>
              )}
            </div>
            <div className="flex items-center gap-1">
              {c.volumes.length > 0 ? (
                <span
                  className="flex items-center gap-1 text-slate-500"
                  title={c.volumes.map((v) => v.source).join(', ')}
                >
                  <HardDrive size={11} />
                  {c.volumes.length}
                </span>
              ) : (
                <span className="text-slate-700">—</span>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function GroupCard({
  group,
  onToggle,
  selectable,
}: {
  group: EditableGroup;
  onToggle?: () => void;
  selectable?: boolean;
}) {
  const g = group.originalGroup;
  const [expanded, setExpanded] = useState(false);

  return (
    <div
      className={`rounded-xl border transition-all ${
        selectable ? 'cursor-pointer hover:border-violet-500/40' : ''
      } ${group.selected ? 'border-violet-500/40 bg-violet-500/5' : 'border-slate-700/50 bg-slate-900/40'}`}
    >
      {/* biome-ignore lint/a11y/noStaticElementInteractions: card header row, needs click-to-toggle */}
      <div
        className="flex items-center gap-3 px-4 py-3"
        role={selectable ? 'button' : undefined}
        tabIndex={selectable ? 0 : undefined}
        onClick={selectable ? onToggle : undefined}
        onKeyDown={
          selectable
            ? (e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  onToggle?.();
                }
              }
            : undefined
        }
      >
        {selectable && (
          <div className="shrink-0">
            {group.selected ? (
              <CheckSquare size={16} className="text-violet-400" />
            ) : (
              <Square size={16} className="text-slate-600" />
            )}
          </div>
        )}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-sm font-mono font-semibold text-slate-100 truncate">{g.group_key}</span>
            <NodeBadge name={group.nodeName} isLocal={group.nodeId === 'local'} />
            <TypeChip type={g.deploy_type} />
            {g.compose_file_found && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-400/10 text-emerald-400 border border-emerald-400/20">
                compose.yaml
              </span>
            )}
            {g.env_file_found && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-sky-400/10 text-sky-400 border border-sky-400/20">
                .env
              </span>
            )}
            {group.hasDockerSocket && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-red-400/10 text-red-300 border border-red-400/20">
                import blocked: Docker socket mount
              </span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-3 shrink-0 text-xs text-slate-500">
          <span>
            {g.containers.length} service{g.containers.length !== 1 ? 's' : ''}
          </span>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setExpanded(!expanded);
            }}
            className="p-1 rounded hover:bg-slate-800 transition-colors cursor-pointer"
          >
            <ChevronRight size={14} className={`transition-transform ${expanded ? 'rotate-90' : ''}`} />
          </button>
        </div>
      </div>

      {/* Containers — collapsed by default */}
      {expanded && <ContainerTable containers={g.containers} />}
    </div>
  );
}

function EditForm({
  draft,
  services,
  onChange,
  onSave,
  onCancel,
}: {
  draft: Partial<EditableGroup>;
  services: string[];
  onChange: (d: Partial<EditableGroup>) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <label htmlFor="import-project-id" className="block text-xs font-medium text-slate-400 mb-1.5">
          Project ID
        </label>
        <input
          id="import-project-id"
          type="text"
          value={draft.projectId ?? ''}
          onChange={(e) =>
            onChange({
              projectId: e.target.value
                .toLowerCase()
                .replace(/[^a-z0-9-]/g, '')
                .slice(0, 63),
            })
          }
          className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
          placeholder="my-project"
        />
        <p className="text-[11px] text-slate-500 mt-1">Lowercase letters, digits, hyphens only</p>
      </div>
      <div>
        <label htmlFor="import-display-name" className="block text-xs font-medium text-slate-400 mb-1.5">
          Display Name <span className="text-slate-600 font-normal">(optional)</span>
        </label>
        <input
          id="import-display-name"
          type="text"
          value={draft.name ?? ''}
          onChange={(e) => onChange({ name: e.target.value })}
          className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
          placeholder="My App"
        />
      </div>
      <div>
        <label htmlFor="import-description" className="block text-xs font-medium text-slate-400 mb-1.5">
          Description <span className="text-slate-600 font-normal">(optional)</span>
        </label>
        <input
          id="import-description"
          type="text"
          value={draft.description ?? ''}
          onChange={(e) => onChange({ description: e.target.value })}
          className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
          placeholder="What this app does"
        />
      </div>
      <div>
        <label htmlFor="import-public-service" className="block text-xs font-medium text-slate-400 mb-1.5">
          Public service (for routing)
        </label>
        <select
          id="import-public-service"
          value={draft.publicService ?? ''}
          onChange={(e) => onChange({ publicService: e.target.value })}
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
        >
          {services.map((s) => (
            <option key={s} value={s}>
              {s}
            </option>
          ))}
        </select>
      </div>
      <div className="border-t border-slate-700/50 pt-3 space-y-3">
        <label className="flex items-center justify-between gap-2 cursor-pointer">
          <div>
            <span className="text-xs text-slate-300">Allow raw ports</span>
            <p className="text-[10px] text-slate-500 mt-0.5">Expose all compose ports directly on host (TCP/UDP)</p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={draft.allowRawPorts ?? false}
            onClick={() => onChange({ allowRawPorts: !(draft.allowRawPorts ?? false) })}
            className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${(draft.allowRawPorts ?? false) ? 'bg-violet-500' : 'bg-slate-600'}`}
          >
            <span
              className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${(draft.allowRawPorts ?? false) ? 'translate-x-3.5' : 'translate-x-0.5'}`}
            />
          </button>
        </label>
      </div>
      <div className="flex gap-2 pt-1 justify-end">
        <button
          type="button"
          onClick={onSave}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
        >
          <Check size={12} /> Save
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs text-slate-400 hover:bg-slate-800 transition-colors cursor-pointer"
        >
          <X size={12} /> Cancel
        </button>
      </div>
    </div>
  );
}
