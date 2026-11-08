import { Play, Square, X } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  type DemoLongLived,
  type DemoSession,
  getDemoSessions,
  killDemoSession,
  startProject,
  stopProject,
} from '../../api';
import { useToast } from '../ToastContext';

function minsLeft(ts: number): number {
  return Math.max(0, Math.round((ts * 1000 - Date.now()) / 60000));
}

function duration(sec: number): string {
  const m = Math.max(0, Math.round((Date.now() / 1000 - sec) / 60));
  return m >= 60 ? `${Math.floor(m / 60)}h ${m % 60}m` : `${m}m`;
}

/** Modal listing a demo's instances: edit + shared (stop/start) and live
 *  sessions (running time, time left, end). */
export default function DemoSessionsModal({
  projectId,
  onChanged,
  onClose,
}: {
  projectId: string;
  onChanged: () => void;
  onClose: () => void;
}) {
  const [sessions, setSessions] = useState<DemoSession[] | null>(null);
  const [longLived, setLongLived] = useState<DemoLongLived[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const { showToast } = useToast();

  const load = useCallback(() => {
    getDemoSessions(projectId)
      .then((r) => {
        setSessions(r.sessions);
        setLongLived(r.long_lived ?? []);
      })
      .catch(() => setSessions(null));
  }, [projectId]);

  useEffect(() => {
    load();
    const timer = setInterval(load, 5000);
    const onEsc = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onEsc);
    return () => {
      clearInterval(timer);
      window.removeEventListener('keydown', onEsc);
    };
  }, [load, onClose]);

  const act = (key: string, fn: () => Promise<unknown>, done: string) => {
    setBusy(key);
    Promise.resolve(fn())
      .then(() => {
        showToast(done, 'success');
        setTimeout(load, 1500);
        onChanged();
      })
      .catch((e) => showToast(e instanceof Error ? e.message : 'Failed'))
      .finally(() => setBusy(null));
  };

  const kill = (sessionId: string) =>
    act(`kill:${sessionId}`, () => killDemoSession(projectId, sessionId), 'Session ended');

  const active = (sessions ?? []).filter((s) => s.status === 'active').length;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4" onClick={onClose}>
      <div
        className="w-full max-w-lg bg-slate-800 border border-slate-700/70 rounded-lg shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50">
          <h2 className="text-sm font-medium text-slate-200">
            Demo instances <span className="text-slate-500 font-normal">· {active} live session{active === 1 ? '' : 's'}</span>
          </h2>
          <button type="button" onClick={onClose} className="text-slate-400 hover:text-slate-200 cursor-pointer">
            <X size={16} />
          </button>
        </div>
        <div className="max-h-96 overflow-y-auto px-4 py-3 space-y-1.5">
          {longLived.map((inst) => {
            const running = inst.status === 'running' || inst.status === 'degraded';
            return (
              <div
                key={inst.id}
                className="flex items-center justify-between gap-3 rounded bg-slate-900/50 px-3 py-2 text-xs"
              >
                <span className="flex items-center gap-2 min-w-0">
                  <span
                    className={`shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium uppercase ${
                      inst.kind === 'edit' ? 'bg-violet-600/20 text-violet-300' : 'bg-teal-600/20 text-teal-300'
                    }`}
                  >
                    {inst.kind}
                  </span>
                  <span className="font-mono text-slate-400 truncate" title={inst.id}>
                    {inst.id}
                  </span>
                </span>
                <span className="flex items-center gap-3 shrink-0">
                  <span className={running ? 'text-emerald-400' : 'text-slate-500'}>{inst.status ?? '—'}</span>
                  {running ? (
                    <button
                      type="button"
                      disabled={busy !== null}
                      onClick={() => act(`stop:${inst.id}`, () => stopProject(inst.id), `${inst.kind} instance stopped`)}
                      title="Stop (auto-wakes when needed)"
                      className="text-slate-500 hover:text-red-400 disabled:opacity-50 cursor-pointer"
                    >
                      <Square size={12} />
                    </button>
                  ) : (
                    <button
                      type="button"
                      disabled={busy !== null}
                      onClick={() => act(`start:${inst.id}`, () => startProject(inst.id), `${inst.kind} instance started`)}
                      title="Start now"
                      className="text-slate-500 hover:text-emerald-400 disabled:opacity-50 cursor-pointer"
                    >
                      <Play size={12} />
                    </button>
                  )}
                </span>
              </div>
            );
          })}

          {sessions === null && <p className="text-xs text-slate-500">Loading…</p>}
          {sessions !== null && longLived.length > 0 && sessions.length > 0 && (
            <div className="pt-2 text-[10px] uppercase tracking-wider text-slate-500">Sessions</div>
          )}
          {sessions !== null && sessions.length === 0 && longLived.length === 0 && (
            <p className="text-xs text-slate-500">No instances yet.</p>
          )}
          {sessions?.map((s) => (
            <div
              key={s.id}
              className="flex items-center justify-between gap-3 rounded bg-slate-900/50 px-3 py-2 text-xs"
            >
              <span className="font-mono text-slate-400 truncate" title={s.instance_project_id}>
                {s.instance_project_id}
              </span>
              <span className="flex items-center gap-3 shrink-0">
                <span className="text-slate-400" title="Running for">
                  {duration(s.created_at)}
                </span>
                {s.status === 'active' ? (
                  <span className={minsLeft(s.expires_at) > 3 ? 'text-emerald-400' : 'text-amber-400'}>
                    {minsLeft(s.expires_at)}m left
                  </span>
                ) : (
                  <span className="text-slate-500">{s.status}</span>
                )}
                <span className="text-slate-500">{s.instance_status ?? '—'}</span>
                {(s.status === 'active' || s.status === 'expired') && (
                  <button
                    type="button"
                    onClick={() => kill(s.id)}
                    title={
                      s.status === 'active'
                        ? 'End this session and remove its instance'
                        : 'Remove this lingering instance now'
                    }
                    className="text-slate-500 hover:text-red-400 cursor-pointer"
                  >
                    <X size={12} />
                  </button>
                )}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
