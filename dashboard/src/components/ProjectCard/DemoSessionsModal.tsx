import { ChevronDown, ChevronRight, Info, Loader2, Play, Square, X } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  type DemoLongLived,
  type DemoPoolInstance,
  type DemoSession,
  type DemoSharedTraffic,
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

/** Modal listing a demo's instances: edit + shared (stop/start), live
 *  sessions (info icon opens a visitor-details modal stacked on top), the
 *  pre-warm pool, and shared-mode traffic. Row actions reflect immediately
 *  (per-row busy state) and refresh when the backend confirms. */
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
  const [pool, setPool] = useState<DemoPoolInstance[]>([]);
  const [traffic, setTraffic] = useState<DemoSharedTraffic | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string[]>([]);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [trafficOpen, setTrafficOpen] = useState(false);
  const { showToast } = useToast();

  const load = useCallback(() => {
    getDemoSessions(projectId)
      .then((r) => {
        setSessions(r.sessions);
        setLongLived(r.long_lived ?? []);
        setPool(r.pool ?? []);
        setTraffic(r.traffic ?? null);
      })
      .catch(() => setSessions(null));
  }, [projectId]);

  useEffect(() => {
    load();
    const timer = setInterval(load, 5000);
    const onEsc = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (detailId) setDetailId(null);
      else onClose();
    };
    window.addEventListener('keydown', onEsc);
    return () => {
      clearInterval(timer);
      window.removeEventListener('keydown', onEsc);
    };
  }, [load, onClose, detailId]);

  const act = (key: string, fn: () => Promise<unknown>, done: string) => {
    setBusy(key);
    Promise.resolve(fn())
      .then(() => {
        showToast(done, 'success');
        load();
        onChanged();
      })
      .catch((e) => showToast(e instanceof Error ? e.message : 'Failed'))
      .finally(() => setBusy(null));
  };

  const kill = (sessionId: string) => {
    setRemoving((r) => [...r, sessionId]); // reflect immediately; row shows Removing…
    killDemoSession(projectId, sessionId)
      .then(() => {
        showToast('Session ended — instance removed', 'success');
        load();
        onChanged();
      })
      .catch((e) => {
        showToast(e instanceof Error ? e.message : 'Failed to end session');
        setRemoving((r) => r.filter((id) => id !== sessionId));
      });
  };

  const active = (sessions ?? []).filter((s) => s.status === 'active' && !removing.includes(s.id)).length;
  const detail = detailId ? (sessions ?? []).find((s) => s.id === detailId) : null;

  const BusyIcon = () => <Loader2 size={12} className="animate-spin text-slate-400" />;

  const SectionLabel = ({ children }: { children: React.ReactNode }) => (
    <div className="pt-2 text-[10px] uppercase tracking-wider text-slate-500">{children}</div>
  );

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: full-screen backdrop — click outside dismisses
    // biome-ignore lint/a11y/useKeyWithClickEvents: Escape is handled by the window-level keydown listener
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="w-full max-w-lg bg-slate-800 border border-slate-700/70 rounded-lg shadow-2xl">
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50">
          <h2 className="text-sm font-medium text-slate-200">
            Demo instances{' '}
            <span className="text-slate-500 font-normal">
              · {active} live session{active === 1 ? '' : 's'}
              {pool.length > 0 && ` · ${pool.length} prewarm`}
            </span>
          </h2>
          <button type="button" onClick={onClose} className="text-slate-400 hover:text-slate-200 cursor-pointer">
            <X size={16} />
          </button>
        </div>
        <div className="max-h-96 overflow-y-auto px-4 py-3 space-y-1.5">
          {longLived.map((inst) => {
            const running = inst.status === 'running' || inst.status === 'degraded';
            const rowBusy = busy === `stop:${inst.id}` || busy === `start:${inst.id}`;
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
                  {rowBusy ? (
                    <>
                      <span className="text-amber-400">{busy === `stop:${inst.id}` ? 'Stopping…' : 'Starting…'}</span>
                      <BusyIcon />
                    </>
                  ) : (
                    <>
                      <span className={running ? 'text-emerald-400' : 'text-slate-500'}>{inst.status ?? '—'}</span>
                      {running ? (
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            act(`stop:${inst.id}`, () => stopProject(inst.id), `${inst.kind} instance stopped`)
                          }
                          title="Stop (auto-wakes when needed)"
                          className="text-slate-500 hover:text-red-400 disabled:opacity-50 cursor-pointer"
                        >
                          <Square size={12} />
                        </button>
                      ) : (
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            act(`start:${inst.id}`, () => startProject(inst.id), `${inst.kind} instance started`)
                          }
                          title="Start now"
                          className="text-slate-500 hover:text-emerald-400 disabled:opacity-50 cursor-pointer"
                        >
                          <Play size={12} />
                        </button>
                      )}
                    </>
                  )}
                </span>
              </div>
            );
          })}

          {traffic && (
            <>
              <button
                type="button"
                onClick={() => setTrafficOpen((v) => !v)}
                className="w-full flex items-center gap-2 rounded bg-slate-900/50 px-3 py-2 text-xs cursor-pointer hover:bg-slate-900/80"
              >
                {trafficOpen ? (
                  <ChevronDown size={12} className="text-slate-500 shrink-0" />
                ) : (
                  <ChevronRight size={12} className="text-slate-500 shrink-0" />
                )}
                <span className="text-slate-300">Traffic</span>
                <span className="text-slate-500">
                  · {traffic.unique_ips} IP{traffic.unique_ips === 1 ? '' : 's'} · {traffic.total_hits} hits
                </span>
              </button>
              {trafficOpen && (
                <div className="rounded bg-slate-900/50 px-3 py-2 space-y-1">
                  {traffic.ips.length === 0 && (
                    <p className="text-xs text-slate-500">No visits since the last restart.</p>
                  )}
                  {traffic.ips.map((t) => (
                    <div key={t.ip} className="flex items-center justify-between gap-3 text-xs">
                      <span className="font-mono text-slate-400 truncate" title={t.ip}>
                        {t.ip}
                      </span>
                      <span className="text-slate-500 shrink-0">
                        {t.hits} hit{t.hits === 1 ? '' : 's'} · seen {duration(t.last_seen)} ago
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}

          {sessions === null && <p className="text-xs text-slate-500">Loading…</p>}
          {sessions !== null && longLived.length > 0 && sessions.length > 0 && <SectionLabel>Sessions</SectionLabel>}
          {sessions !== null && sessions.length === 0 && longLived.length === 0 && pool.length === 0 && !traffic && (
            <p className="text-xs text-slate-500">No instances yet.</p>
          )}
          {sessions?.map((s) => {
            const isRemoving = removing.includes(s.id);
            return (
              <div
                key={s.id}
                className={`rounded bg-slate-900/50 transition-opacity ${isRemoving ? 'opacity-60' : ''}`}
              >
                <div className="flex items-center justify-between gap-3 px-3 py-2 text-xs">
                  <span className="flex items-center gap-2 min-w-0">
                    <button
                      type="button"
                      onClick={() => setDetailId(s.id)}
                      title="Visitor details"
                      className="text-slate-500 hover:text-slate-300 cursor-pointer shrink-0"
                    >
                      <Info size={12} />
                    </button>
                    <span className="font-mono text-slate-400 truncate" title={s.instance_project_id}>
                      {s.instance_project_id}
                    </span>
                  </span>
                  <span className="flex items-center gap-3 shrink-0">
                    {isRemoving ? (
                      <>
                        <span className="text-amber-400">Removing…</span>
                        <BusyIcon />
                      </>
                    ) : (
                      <>
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
                      </>
                    )}
                  </span>
                </div>
              </div>
            );
          })}

          {pool.length > 0 && <SectionLabel>Prewarm pool</SectionLabel>}
          {pool.map((inst) => (
            <div
              key={inst.id}
              className="flex items-center justify-between gap-3 rounded bg-slate-900/50 px-3 py-2 text-xs"
            >
              <span className="flex items-center gap-2 min-w-0">
                <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium uppercase bg-amber-600/20 text-amber-300">
                  prewarm
                </span>
                <span className="font-mono text-slate-400 truncate" title={inst.id}>
                  {inst.id}
                </span>
              </span>
              <span className="text-slate-500 shrink-0">
                {inst.status} · {duration(inst.created_at)}
              </span>
            </div>
          ))}
        </div>
      </div>
      {detail && <SessionDetailModal session={detail} onClose={() => setDetailId(null)} />}
    </div>
  );
}

const Chip = ({ children }: { children: React.ReactNode }) => (
  <span className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-slate-700/60 text-slate-300">{children}</span>
);

const DetailLabel = ({ children }: { children: React.ReactNode }) => (
  <div className="text-[10px] uppercase tracking-wider text-slate-500">{children}</div>
);

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded bg-slate-900/50 px-3 py-2">
      <div className="text-[10px] uppercase tracking-wider text-slate-500">{label}</div>
      <div className="text-xs text-slate-300 mt-0.5">{children}</div>
    </div>
  );
}

/** One labeled line inside a detail card — fixed label column, value row. */
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex gap-3 px-3 py-1.5 text-xs">
      <span className="w-14 shrink-0 text-slate-500">{label}</span>
      <span className="min-w-0 flex-1">{children}</span>
    </div>
  );
}

/** Stacked on top of the sessions modal — visitor, activity and pages for
 *  one session, with room the compact list can't spare. Data comes from the
 *  parent's 5s poll, so the view stays live. */
function SessionDetailModal({ session: s, onClose }: { session: DemoSession; onClose: () => void }) {
  const hasDetails = s.ip !== null || s.user_agent !== null || s.hits !== null;
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: full-screen backdrop — click outside dismisses
    // biome-ignore lint/a11y/useKeyWithClickEvents: Escape bubbles to the parent modal's window listener
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="w-full max-w-md bg-slate-800 border border-slate-700/70 rounded-lg shadow-2xl">
        <div className="flex items-center justify-between px-4 py-3 border-b border-slate-700/50">
          <h3 className="text-sm font-medium text-slate-200 truncate">
            Visitor session <span className="font-mono text-slate-500 font-normal">{s.instance_project_id}</span>
          </h3>
          <button type="button" onClick={onClose} className="text-slate-400 hover:text-slate-200 cursor-pointer">
            <X size={16} />
          </button>
        </div>
        <div className="max-h-[70vh] overflow-y-auto px-4 py-3 space-y-4 text-xs">
          {!hasDetails ? (
            <p className="text-slate-500">No visitor details — cleared by an orchestrator restart.</p>
          ) : (
            <>
              <section className="space-y-2">
                <DetailLabel>Visitor</DetailLabel>
                <div className="flex items-center gap-1.5 flex-wrap">
                  <Chip>{s.browser ?? '—'}</Chip>
                  <Chip>{s.os ?? '—'}</Chip>
                  <Chip>{s.device ?? '—'}</Chip>
                  {s.bot && <span className="text-amber-400 font-medium">bot</span>}
                </div>
                <div className="rounded bg-slate-900/50 divide-y divide-slate-800">
                  <Row label="IP">
                    <span className="font-mono">{s.ip ?? '—'}</span>
                  </Row>
                  {s.referer && (
                    <Row label="Referrer">
                      <a
                        href={s.referer}
                        target="_blank"
                        rel="noreferrer noopener"
                        title={s.referer}
                        className="text-slate-400 hover:text-slate-300 underline decoration-slate-600 truncate block"
                      >
                        {s.referer}
                      </a>
                    </Row>
                  )}
                  <Row label="UA">
                    <span className="break-all text-slate-400">{s.user_agent ?? '—'}</span>
                  </Row>
                </div>
              </section>
              <section className="space-y-2">
                <DetailLabel>Activity</DetailLabel>
                <div className="grid grid-cols-2 gap-1.5">
                  <Stat label="Requests">{s.hits ?? '—'}</Stat>
                  <Stat label="First seen">{duration(s.created_at)} ago</Stat>
                  <Stat label="Last seen">{duration(s.last_seen_at)} ago</Stat>
                  <Stat label="Session">{s.status === 'active' ? `${minsLeft(s.expires_at)}m left` : s.status}</Stat>
                </div>
              </section>
              <section className="space-y-1.5">
                <DetailLabel>Pages</DetailLabel>
                {!s.pages || s.pages.length === 0 ? (
                  <p className="text-slate-500">No pages recorded.</p>
                ) : (
                  s.pages.map((p) => (
                    <div key={p.path} className="flex items-center gap-2 rounded bg-slate-900/50 px-3 py-1.5">
                      <span className="font-mono text-slate-300 truncate" title={p.path}>
                        {p.path}
                      </span>
                      {s.landing === p.path && (
                        <span className="shrink-0 text-[10px] text-violet-400" title="Landing page">
                          landing
                        </span>
                      )}
                      <span className="ml-auto shrink-0 text-slate-500">{p.hits}×</span>
                    </div>
                  ))
                )}
              </section>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
