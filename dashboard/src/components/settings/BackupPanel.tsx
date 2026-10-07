import {
  AlertTriangle,
  Check,
  Cloud,
  DatabaseBackup,
  HardDrive,
  History,
  Pencil,
  Plus,
  RotateCcw,
  Trash2,
  Zap,
} from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  addBackupS3,
  type BackupStatus,
  disableBackup,
  enableBackup,
  fetchBackupStatus,
  formatBytes,
  removeBackupS3,
  restoreBackup,
  timeAgo,
} from '../../api';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

type Busy = 'enable' | 'disable' | 's3' | 'remove-s3' | 'restore' | null;

const Card = SettingsCard;

const inputCls =
  'w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500';

/** ISO timestamp for N ms ago (empty for latest). */
function msAgoIso(ms: number): string {
  return new Date(Date.now() - ms).toISOString().replace(/\.\d+Z$/, 'Z');
}

/** ms → value for <input type="datetime-local"> ("YYYY-MM-DDTHH:mm", browser-local). */
function toLocalInputValue(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const RESTORE_PICKS = [
  { id: '10m', label: '10 min ago', ms: 10 * 60_000 },
  { id: '1h', label: '1 hour ago', ms: 3_600_000 },
  { id: '6h', label: '6 hours ago', ms: 21_600_000 },
  { id: '12h', label: '12 hours ago', ms: 43_200_000 },
  { id: '24h', label: '24 hours ago', ms: 86_400_000 },
] as const;

type RestorePickId = 'latest' | 'custom' | (typeof RESTORE_PICKS)[number]['id'];

export default function BackupPanel() {
  const [status, setStatus] = useState<BackupStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [showDisableConfirm, setShowDisableConfirm] = useState(false);
  const [showRestoreConfirm, setShowRestoreConfirm] = useState(false);
  const [restoreAck, setRestoreAck] = useState(false);
  const [restoreTimestamp, setRestoreTimestamp] = useState('');
  const [restorePick, setRestorePick] = useState<RestorePickId | null>('latest');
  const [customValue, setCustomValue] = useState('');
  const [restarting, setRestarting] = useState(false);
  // S3 form
  const [s3FormOpen, setS3FormOpen] = useState(false);
  const [endpoint, setEndpoint] = useState('');
  const [bucket, setBucket] = useState('');
  const [prefix, setPrefix] = useState('');
  const [accessKey, setAccessKey] = useState('');
  const [secretKey, setSecretKey] = useState('');
  const { showToast } = useToast();

  const refresh = useCallback(() => {
    fetchBackupStatus()
      .then((s) => {
        setStatus(s);
        setError(null);
        if (!s3FormOpen) {
          setEndpoint(s.s3.endpoint || '');
          setBucket(s.s3.bucket || '');
          setPrefix(s.s3.prefix || '');
        }
      })
      .catch(() => setError('Failed to load backup status'));
  }, [s3FormOpen]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Keep the status live while backup is enabled (last sync, generations).
  const enabled = status?.enabled === true;
  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(refresh, 10_000);
    return () => clearInterval(timer);
  }, [enabled, refresh]);

  // After a restore, the orchestrator restarts — poll until it answers again.
  useEffect(() => {
    if (!restarting) return;
    const timer = setInterval(() => {
      fetchBackupStatus()
        .then(() => {
          setRestarting(false);
          showToast('Database restored from backup', 'success');
        })
        .catch(() => {});
    }, 1500);
    return () => clearInterval(timer);
  }, [restarting, showToast]);

  const run = async (op: Busy, fn: () => Promise<void>) => {
    setBusy(op);
    setError(null);
    try {
      await fn();
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Operation failed';
      setError(msg);
      showToast(msg);
    } finally {
      setBusy(null);
    }
  };

  if (!status) {
    return <p className="text-xs text-slate-400">{error ?? 'Loading backup status…'}</p>;
  }

  if (restarting) {
    return (
      <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 py-10 space-y-4">
        <div className="flex flex-col items-center gap-3">
          <RotateCcw size={22} className="text-violet-400 animate-spin" />
          <p className="text-sm font-medium text-slate-100">Applying restore</p>
        </div>
        <ol className="max-w-xs mx-auto space-y-2 text-xs">
          <li className="flex items-center gap-2 text-slate-300">
            <Check size={13} className="text-green-400" /> Restore snapshot staged
          </li>
          <li className="flex items-center gap-2 text-slate-300">
            <span className="w-[13px] h-[13px] rounded-full border-2 border-violet-400 border-t-transparent animate-spin" />
            Orchestrator restarting…
          </li>
          <li className="flex items-center gap-2 text-slate-500">
            <span className="w-[13px] h-[13px] rounded-full border-2 border-slate-600" /> Back online shortly
          </li>
        </ol>
        <p className="text-xs text-slate-500 text-center max-w-xs mx-auto">
          You may be signed out — sessions are restored from the backup. Sign in again if prompted.
        </p>
      </div>
    );
  }

  // ── Not enabled: explainer ──────────────────────────────────────────────
  if (!status.enabled) {
    return (
      <div className="space-y-4">
        {error && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
        )}
        <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 p-6">
          <div className="flex items-center gap-2.5 mb-4">
            <DatabaseBackup size={18} className="text-violet-400" />
            <h2 className="text-sm font-semibold text-slate-100">Database backup</h2>
          </div>
          <p className="text-xs text-slate-400 leading-relaxed mb-5">
            Continuous backup of the LiteBin platform database — every project, node, route, and setting. A Litestream
            sidecar streams each write the moment it happens.
          </p>
          <ul className="space-y-3 mb-6">
            {[
              {
                icon: Zap,
                title: 'Continuous streaming',
                text: 'Every write replicated in ~1s — no schedules, no windows',
              },
              {
                icon: RotateCcw,
                title: 'Point-in-time restore',
                text: 'Roll the database back to any second since backup started',
              },
              {
                icon: Cloud,
                title: 'Optional off-site copy',
                text: 'Add an S3/R2 replica later for survival of the whole server',
              },
            ].map(({ icon: Icon, title, text }) => (
              <li key={title} className="flex items-start gap-3">
                <span className="p-1.5 rounded-md bg-violet-500/10 text-violet-300 shrink-0">
                  <Icon size={13} />
                </span>
                <div>
                  <p className="text-xs font-medium text-slate-200">{title}</p>
                  <p className="text-xs text-slate-500 mt-0.5">{text}</p>
                </div>
              </li>
            ))}
          </ul>
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={() =>
                run('enable', async () => {
                  const res = await enableBackup();
                  setStatus(res.status);
                  showToast('Backup enabled — Litestream sidecar started', 'success');
                })
              }
              disabled={busy !== null || !status.available}
              className="px-4 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
            >
              {busy === 'enable' ? 'Enabling…' : 'Enable backup'}
            </button>
            <span className="text-xs text-slate-500">
              {status.available ? '~5-10 MB RAM · local replica on this server' : 'requires the orchestrator container'}
            </span>
          </div>
          {busy === 'enable' && (
            <p className="text-xs text-slate-500 mt-3">Pulling the litestream image — may take ~30s on first run.</p>
          )}
        </div>
      </div>
    );
  }

  // ── Enabled: status, destinations, restore ──────────────────────────────
  const s3Configured = status.s3.configured;
  const s3FormValid = endpoint.trim() && bucket.trim() && accessKey.trim() && secretKey.trim();

  // Restore coverage: litestream can restore to any second from the oldest
  // generation's start until now. Quick picks only appear inside that window.
  const generationStarts = status.generations
    .map((g) => g.start_time)
    .filter((s): s is string => !!s)
    .map((s) => new Date(s).getTime());
  const oldestMs = generationStarts.length > 0 ? Math.min(...generationStarts) : null;
  const availablePicks = RESTORE_PICKS.filter((p) => oldestMs !== null && Date.now() - p.ms >= oldestMs);
  const choosePick = (id: (typeof RESTORE_PICKS)[number]['id']) => {
    setRestorePick(id);
    const pick = RESTORE_PICKS.find((p) => p.id === id);
    if (pick) setRestoreTimestamp(msAgoIso(pick.ms));
  };
  const chooseCustom = (value: string) => {
    setCustomValue(value);
    setRestoreTimestamp(value ? new Date(value).toISOString().replace(/\.\d+Z$/, 'Z') : '');
  };
  const selectedLabel =
    restorePick && restorePick !== 'latest' && restorePick !== 'custom'
      ? RESTORE_PICKS.find((p) => p.id === restorePick)?.label
      : restoreTimestamp.trim() || 'the latest backup state';

  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
      )}

      {/* Status banner */}
      <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 px-4 py-3 flex items-center gap-x-4 gap-y-1.5 flex-wrap">
        <span className={`h-2 w-2 rounded-full shrink-0 ${status.sidecar_running ? 'bg-green-400' : 'bg-amber-400'}`} />
        <span className="text-xs font-medium text-slate-100">
          {status.sidecar_running ? 'Continuous backup active' : 'Backup sidecar not running'}
        </span>
        {status.last_sync ? (
          <span className="text-xs text-slate-500">
            last write replicated {timeAgo(Math.floor(new Date(status.last_sync).getTime() / 1000))}
          </span>
        ) : (
          <span className="text-xs text-slate-500">waiting for first sync…</span>
        )}
        {status.replica_size_bytes != null && (
          <span className="text-xs text-slate-500 ml-auto">
            backup {formatBytes(status.replica_size_bytes)}
            {status.db_size_bytes != null && ` · live db ${formatBytes(status.db_size_bytes)}`}
          </span>
        )}
      </div>

      {/* Destinations */}
      <Card
        icon={HardDrive}
        title="Destinations"
        description="Where backups are written. The local replica is always on; S3/R2 adds an off-server copy."
      >
        <div className="space-y-2">
          {/* Local */}
          <div className="flex items-center gap-3 rounded-md border border-slate-700/40 px-3 py-2.5">
            <Check size={14} className="text-green-400 shrink-0" />
            <div className="min-w-0 flex-1">
              <p className="text-xs font-medium text-slate-200">This server</p>
              <p className="text-xs text-slate-500">
                {status.generations.length > 0
                  ? `${status.generations.length} generation${status.generations.length === 1 ? '' : 's'} · ${
                      status.replica_size_bytes != null ? formatBytes(status.replica_size_bytes) : '—'
                    } · 30-day retention`
                  : 'backups/litebin.db · 30-day retention'}
              </p>
            </div>
            <span className="text-xs px-1.5 py-0.5 rounded bg-green-500/10 text-green-400 font-medium shrink-0">
              ALWAYS ON
            </span>
          </div>

          {/* S3: configured summary */}
          {s3Configured && !s3FormOpen ? (
            <div className="flex items-center gap-3 rounded-md border border-cyan-700/30 bg-cyan-500/[0.03] px-3 py-2.5">
              <Cloud size={14} className="text-cyan-300 shrink-0" />
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-slate-200">{status.s3.bucket}</p>
                <p className="text-xs text-slate-500 font-mono truncate">
                  {status.s3.endpoint}/{status.s3.prefix}/
                </p>
              </div>
              <button
                type="button"
                onClick={() => {
                  setS3FormOpen(true);
                  setAccessKey('');
                  setSecretKey('');
                }}
                disabled={busy !== null}
                className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs font-medium text-slate-300 hover:text-slate-100 hover:bg-slate-700/50 transition-colors cursor-pointer disabled:opacity-50"
              >
                <Pencil size={11} /> Credentials
              </button>
              <button
                type="button"
                onClick={() =>
                  run('remove-s3', async () => {
                    const res = await removeBackupS3();
                    setStatus(res.status);
                    setEndpoint('');
                    setBucket('');
                    setPrefix('');
                    showToast('S3 replica removed — local backup keeps running', 'success');
                  })
                }
                disabled={busy !== null}
                className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs font-medium text-slate-400 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50"
              >
                <Trash2 size={11} /> Remove
              </button>
            </div>
          ) : null}

          {/* S3: add row (when not configured and form closed) */}
          {!s3Configured && !s3FormOpen ? (
            <button
              type="button"
              onClick={() => setS3FormOpen(true)}
              disabled={busy !== null}
              className="w-full flex items-center justify-center gap-1.5 rounded-md border border-dashed border-slate-700 px-3 py-2.5 text-xs text-slate-400 hover:text-slate-200 hover:border-slate-500 transition-colors cursor-pointer disabled:opacity-50"
            >
              <Plus size={13} />
              Add S3/R2 off-site replica
            </button>
          ) : null}

          {/* S3 form */}
          {s3FormOpen ? (
            <div className="rounded-md border border-slate-700/40 p-3.5 space-y-3">
              <div className="flex items-center gap-2">
                <Cloud size={13} className="text-cyan-300" />
                <p className="text-xs font-medium text-slate-200">
                  {s3Configured ? 'Update S3/R2 credentials' : 'Add S3/R2 replica'}
                </p>
              </div>
              <div>
                <label htmlFor="backup-s3-endpoint" className="block text-xs text-slate-400 mb-1">
                  Endpoint
                </label>
                <input
                  id="backup-s3-endpoint"
                  type="text"
                  value={endpoint}
                  onChange={(e) => setEndpoint(e.target.value)}
                  placeholder="https://<account_id>.r2.cloudflarestorage.com"
                  className={inputCls}
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label htmlFor="backup-s3-bucket" className="block text-xs text-slate-400 mb-1">
                    Bucket
                  </label>
                  <input
                    id="backup-s3-bucket"
                    type="text"
                    value={bucket}
                    onChange={(e) => setBucket(e.target.value)}
                    placeholder="my-backups"
                    className={inputCls}
                  />
                </div>
                <div>
                  <label htmlFor="backup-s3-prefix" className="block text-xs text-slate-400 mb-1">
                    Prefix
                  </label>
                  <input
                    id="backup-s3-prefix"
                    type="text"
                    value={prefix}
                    onChange={(e) => setPrefix(e.target.value)}
                    placeholder="litebin/"
                    className={inputCls}
                  />
                </div>
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label htmlFor="backup-s3-access" className="block text-xs text-slate-400 mb-1">
                    Access key ID
                  </label>
                  <input
                    id="backup-s3-access"
                    type="text"
                    value={accessKey}
                    onChange={(e) => setAccessKey(e.target.value)}
                    placeholder="Access key ID"
                    className={inputCls}
                  />
                </div>
                <div>
                  <label htmlFor="backup-s3-secret" className="block text-xs text-slate-400 mb-1">
                    Secret access key
                  </label>
                  <input
                    id="backup-s3-secret"
                    type="password"
                    value={secretKey}
                    onChange={(e) => setSecretKey(e.target.value)}
                    placeholder="Secret access key"
                    className={inputCls}
                  />
                </div>
              </div>
              <div className="flex gap-2 pt-0.5">
                <button
                  type="button"
                  onClick={() =>
                    run('s3', async () => {
                      const res = await addBackupS3({
                        endpoint,
                        bucket,
                        prefix: prefix || undefined,
                        access_key_id: accessKey,
                        secret_access_key: secretKey,
                      });
                      setStatus(res.status);
                      setAccessKey('');
                      setSecretKey('');
                      setS3FormOpen(false);
                      showToast('S3 replica saved — Litestream restarted', 'success');
                    })
                  }
                  disabled={busy !== null || !s3FormValid}
                  className="px-4 py-1.5 rounded-md text-xs font-medium bg-cyan-600/80 text-white hover:bg-cyan-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {busy === 's3' ? 'Saving…' : 'Save replica'}
                </button>
                <button
                  type="button"
                  onClick={() => setS3FormOpen(false)}
                  disabled={busy !== null}
                  className="px-4 py-1.5 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer disabled:opacity-50"
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : null}
        </div>
      </Card>

      {/* Backup history */}
      <SettingsCard
        icon={History}
        title="Backup history"
        description="Each generation is a continuous run of replicated writes. Restore to any of these points — or to any second in between."
      >
        {status.generations.length === 0 ? (
          <div className="py-5 text-center">
            {status.sidecar_running ? (
              <>
                <p className="text-xs text-slate-400">First replication hasn&apos;t completed yet</p>
                <p className="text-xs text-slate-600 mt-1">
                  Litestream syncs within seconds of the next database write — this list fills in automatically.
                </p>
              </>
            ) : (
              <p className="text-xs text-slate-500">Backup sidecar is not running — start it to resume replication.</p>
            )}
          </div>
        ) : (
          <div className="space-y-1.5">
            {status.generations.slice(0, 10).map((gen) => (
              <div
                key={gen.id}
                className="flex items-center gap-3 rounded-md border border-slate-700/30 bg-slate-900/50 px-3 py-2"
              >
                <div className="min-w-0 flex-1">
                  <p className="text-xs text-slate-200">
                    {gen.start_time ? new Date(gen.start_time).toLocaleString() : 'unknown start'}
                  </p>
                  <p className="text-xs text-slate-500 font-mono truncate">
                    {gen.file_count} segment{gen.file_count === 1 ? '' : 's'} · {formatBytes(gen.size_bytes)}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    if (gen.start_time) setRestoreTimestamp(gen.start_time);
                    setRestorePick(null);
                    setRestoreAck(false);
                    setShowRestoreConfirm(true);
                  }}
                  disabled={!gen.start_time || busy !== null}
                  className="shrink-0 px-2.5 py-1 rounded text-xs font-medium text-slate-300 hover:text-red-300 hover:bg-red-500/10 border border-slate-700/50 transition-colors cursor-pointer disabled:opacity-40"
                  title={gen.start_time ? `Restore the database to ${gen.start_time}` : undefined}
                >
                  Restore here
                </button>
              </div>
            ))}
            {status.generations.length > 10 && (
              <p className="text-xs text-slate-600 pt-1">
                Showing 10 of {status.generations.length} generations — older ones still restore via an exact timestamp
                below.
              </p>
            )}
          </div>
        )}
      </SettingsCard>

      {/* Restore */}
      <Card
        icon={RotateCcw}
        title="Restore"
        description="Roll the database back to any moment since backup was enabled — any second, not just scheduled points."
      >
        <div className="space-y-4">
          <div>
            <p className="text-xs text-slate-400 mb-1.5">Restore point</p>
            <div className="flex flex-wrap gap-1.5 mb-3">
              <button
                type="button"
                onClick={() => {
                  setRestorePick('latest');
                  setRestoreTimestamp('');
                }}
                className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                  restorePick === 'latest'
                    ? 'bg-violet-500/15 text-violet-300 border border-violet-500/40'
                    : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                }`}
              >
                Latest
              </button>
              {availablePicks.map((pick) => (
                <button
                  key={pick.id}
                  type="button"
                  onClick={() => choosePick(pick.id)}
                  className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                    restorePick === pick.id
                      ? 'bg-violet-500/15 text-violet-300 border border-violet-500/40'
                      : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                  }`}
                >
                  {pick.label}
                </button>
              ))}
              <button
                type="button"
                onClick={() => setRestorePick('custom')}
                className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors cursor-pointer ${
                  restorePick === 'custom'
                    ? 'bg-violet-500/15 text-violet-300 border border-violet-500/40'
                    : 'bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200'
                }`}
              >
                Custom…
              </button>
            </div>
            {restorePick === 'custom' && (
              <input
                id="backup-restore-ts"
                type="datetime-local"
                value={customValue}
                onChange={(e) => chooseCustom(e.target.value)}
                min={oldestMs !== null ? toLocalInputValue(oldestMs) : undefined}
                max={toLocalInputValue(Date.now())}
                className={inputCls}
                style={{ colorScheme: 'dark' }}
              />
            )}
            {oldestMs !== null && availablePicks.length < RESTORE_PICKS.length && (
              <p className="text-xs text-slate-600 mt-1.5">
                Older quick picks appear as backup history builds up (backup started{' '}
                {timeAgo(Math.floor(oldestMs / 1000))}).
              </p>
            )}
          </div>

          <div className="rounded-md bg-slate-900/70 border border-slate-700/40 px-3 py-2.5 space-y-1">
            <p className="text-xs text-slate-400">
              Restoring to <span className="text-slate-200 font-medium">{selectedLabel}</span>:
            </p>
            <ul className="text-xs text-slate-500 list-disc list-inside space-y-0.5">
              <li>Everything written after that point is discarded (projects, tokens, settings)</li>
              <li>The current database is kept on the server as litebin.db.pre-restore</li>
              <li>The orchestrator restarts to apply — a few seconds of downtime, and you sign in again</li>
            </ul>
          </div>

          <button
            type="button"
            onClick={() => {
              setRestoreAck(false);
              setShowRestoreConfirm(true);
            }}
            disabled={busy !== null || !status.sidecar_running}
            className="px-4 py-2 rounded-md text-xs font-medium bg-red-600/80 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            Restore database
          </button>
        </div>
      </Card>

      {/* Disable */}
      <div className="flex items-center justify-between rounded-lg border border-slate-800 px-4 py-3">
        <div>
          <p className="text-xs text-slate-300 font-medium">Disable backup</p>
          <p className="text-xs text-slate-500 mt-0.5">
            Stops the sidecar. Existing replicas stay on disk and resume when re-enabled.
          </p>
        </div>
        <button
          type="button"
          onClick={() => setShowDisableConfirm(true)}
          disabled={busy !== null}
          className="shrink-0 px-3 py-1.5 rounded-md text-xs font-medium text-slate-400 hover:text-red-400 hover:bg-red-500/10 border border-slate-700/50 transition-colors cursor-pointer disabled:opacity-50"
        >
          Disable
        </button>
      </div>

      {/* Disable confirmation */}
      {showDisableConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-amber-400" />
              <h3 className="text-sm font-semibold text-slate-100">Disable backup</h3>
            </div>
            <p className="text-xs text-slate-400 mb-4">
              Stops continuous backup of the LiteBin database. Existing replicas stay on disk and resume from where they
              left off if you re-enable.
            </p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setShowDisableConfirm(false)}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() =>
                  run('disable', async () => {
                    const res = await disableBackup();
                    setStatus(res.status);
                    setShowDisableConfirm(false);
                    showToast('Backup disabled', 'success');
                  })
                }
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === 'disable' ? 'Disabling…' : 'Disable'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Restore confirmation */}
      {showRestoreConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-red-400" />
              <h3 className="text-sm font-semibold text-slate-100">Restore database</h3>
            </div>
            <p className="text-xs text-slate-300 mb-1">
              Replace the current database with the backup
              {restoreTimestamp.trim() ? ` as of ${restoreTimestamp.trim()}` : "'s latest state"}?
            </p>
            <ul className="text-xs text-slate-400 list-disc list-inside mb-3 space-y-1">
              <li>Discards everything written since — projects, tokens, settings</li>
              <li>Sessions are restored from the backup — you will sign in again</li>
              <li>Kept on the server: litebin.db.pre-restore</li>
            </ul>
            <label className="flex items-center gap-2 cursor-pointer mb-4">
              <input
                type="checkbox"
                checked={restoreAck}
                onChange={(e) => setRestoreAck(e.target.checked)}
                className="accent-violet-500"
              />
              <span className="text-xs text-slate-400">I understand this cannot be undone</span>
            </label>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setShowRestoreConfirm(false)}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() =>
                  run('restore', async () => {
                    await restoreBackup(restoreTimestamp.trim() || undefined);
                    setShowRestoreConfirm(false);
                    setRestarting(true);
                  })
                }
                disabled={!restoreAck || busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === 'restore' ? 'Restoring…' : 'Restore'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
