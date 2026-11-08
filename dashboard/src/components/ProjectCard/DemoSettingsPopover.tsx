import { ChevronDown, FlaskConical, Pencil, RotateCcw, Trash2, Upload } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { captureDemoBaseline, type DemoState, putDemoConfig, removeDemoBaseline, resetDemo } from '../../api';
import { useToast } from '../ToastContext';

/** "07/11/2026 8:10:12 pm" — locale date + 12-hour time with am/pm. */
function formatCaptured(epochSeconds: number): string {
  const d = new Date(epochSeconds * 1000);
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit', second: '2-digit', hour12: true })}`;
}

interface DemoSettingsPopoverProps {
  projectId: string;
  domain: string;
  state: DemoState;
  /** Refresh the card's demo state (chips) after any change. */
  onDemoChanged: () => void;
  onClose: () => void;
}

/** Demo mode settings — mode, lifecycle options, banner, baseline.
 *  Replaces the Sleep slot on the card while demo mode is on. */
export default function DemoSettingsPopover({ projectId, domain, state, onDemoChanged, onClose }: DemoSettingsPopoverProps) {
  const cfg = state.config;
  const [mode, setMode] = useState<'shared' | 'session'>(cfg?.mode ?? 'shared');
  const [resetHours, setResetHours] = useState<string>(
    cfg?.reset_interval_minutes ? String(Math.round(cfg.reset_interval_minutes / 60)) : '',
  );
  const [ttl, setTtl] = useState<string>(String(cfg?.session_ttl_minutes ?? 15));
  const [maxInstances, setMaxInstances] = useState<string>(String(cfg?.max_instances ?? 5));
  const [prewarm, setPrewarm] = useState<string>(String(cfg?.prewarm ?? 1));
  const [bannerText, setBannerText] = useState(cfg?.banner_text ?? 'Demo environment — data resets periodically');
  const [barEnabled, setBarEnabled] = useState(cfg?.bar_enabled ?? true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const baselineAt = cfg?.baseline_captured_at;
  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  });

  const run = async (label: string, fn: () => Promise<unknown>, done: string) => {
    setBusy(label);
    setError(null);
    try {
      await fn();
      showToast(done, 'success');
      onDemoChanged();
    } catch (e) {
      const message = e instanceof Error ? e.message : 'Failed';
      setError(message);
      showToast(message);
    } finally {
      setBusy(null);
    }
  };

  const save = async () => {
    setBusy('save');
    setError(null);
    try {
      await putDemoConfig(projectId, {
        enabled: true,
        mode,
        reset_interval_minutes: mode === 'shared' ? Math.round(parseFloat(resetHours) * 60) || null : null,
        session_ttl_minutes: parseInt(ttl, 10) || 15,
        max_instances: parseInt(maxInstances, 10) || 5,
        prewarm: parseInt(prewarm, 10) || 0,
        banner_text: bannerText,
        bar_enabled: barEnabled,
      });
      showToast('Demo settings saved', 'success');
      onDemoChanged();
    } catch (e) {
      const message = e instanceof Error ? e.message : 'Failed to save demo settings';
      setError(message);
      showToast(message);
    } finally {
      setBusy(null);
    }
  };

  const resetNow = () => run('reset', () => resetDemo(projectId), 'Demo reset to published data');

  const publish = () =>
    run(
      'capture',
      () => captureDemoBaseline(projectId).then(() => new Promise((r) => setTimeout(r, 4000))),
      'Publishing — the demo picks it up automatically',
    );

  return (
    <div ref={ref}>
      <button
        type="button"
        onClick={(e) => e.stopPropagation()}
        className="w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer bg-slate-900/80 border-violet-500/40 text-slate-300"
      >
        <div className="flex items-center gap-1.5">
          <FlaskConical size={12} />
          <span className="text-[10px] uppercase tracking-wider">Demo</span>
        </div>
        <ChevronDown size={12} className="text-slate-500" />
      </button>
      <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
        {error && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">{error}</div>
        )}

        {/* Mode */}
        <div>
          <span className="text-xs text-slate-400 block mb-1.5">Mode</span>
          <div className="flex gap-1">
            {(['shared', 'session'] as const).map((m) => (
              <button
                key={m}
                type="button"
                onClick={() => setMode(m)}
                className={`flex-1 py-1.5 rounded text-xs font-medium transition-colors cursor-pointer ${
                  mode === m ? 'bg-teal-600 text-white' : 'bg-slate-700/60 text-slate-300 hover:bg-slate-700'
                }`}
              >
                {m === 'shared' ? 'Shared demo' : 'Personal demos'}
              </button>
            ))}
          </div>
          <p className="text-[11px] leading-relaxed text-slate-400 mt-1.5">
            {mode === 'shared' ? 'Everyone sees the same demo and data' : 'Every visitor gets a private demo with fresh data'}
          </p>
        </div>

        {mode === 'shared' ? (
          <div>
            <span className="text-xs text-slate-400 block mb-1.5">Reset interval (hours, empty = manual only)</span>
            <input
              type="number"
              min={1}
              value={resetHours}
              onChange={(e) => setResetHours(e.target.value)}
              placeholder="manual resets only"
              className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-violet-500"
            />
          </div>
        ) : (
          <div className="grid grid-cols-3 gap-2">
            <label className="block">
              <span className="text-[10px] text-slate-400 block mb-1">TTL (min)</span>
              <input
                type="number"
                min={1}
                value={ttl}
                onChange={(e) => setTtl(e.target.value)}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
              />
            </label>
            <label className="block">
              <span className="text-[10px] text-slate-400 block mb-1">Max live</span>
              <input
                type="number"
                min={1}
                max={50}
                value={maxInstances}
                onChange={(e) => setMaxInstances(e.target.value)}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
              />
            </label>
            <label className="block">
              <span className="text-[10px] text-slate-400 block mb-1">Prewarm</span>
              <input
                type="number"
                min={0}
                max={10}
                value={prewarm}
                onChange={(e) => setPrewarm(e.target.value)}
                className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
              />
            </label>
          </div>
        )}

        {/* Bar */}
        <div>
          <span className="text-xs text-slate-400 block mb-1.5">Bottom bar text (HTML allowed, e.g. links)</span>
          <textarea
            rows={2}
            value={bannerText}
            onChange={(e) => setBannerText(e.target.value)}
            className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-violet-500 resize-y"
          />
          <label className="flex items-center justify-between gap-2 cursor-pointer mt-1.5">
            <span className="text-xs text-slate-300">Show the demo bar to visitors</span>
            <button
              type="button"
              role="switch"
              aria-checked={barEnabled}
              onClick={() => setBarEnabled(!barEnabled)}
              className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${barEnabled ? 'bg-violet-500' : 'bg-slate-600'}`}
            >
              <span
                className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${barEnabled ? 'translate-x-3.5' : 'translate-x-0.5'}`}
              />
            </button>
          </label>
        </div>

        {/* Save — config above; demo data below acts directly */}
        <button
          type="button"
          disabled={busy !== null}
          onClick={save}
          className="w-full py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
        >
          {busy === 'save' ? 'Saving…' : 'Save'}
        </button>

        {/* Demo data — icon actions; tooltips carry the verbs */}
        <div className="border-t border-slate-700/50 pt-2 space-y-1.5">
          <span className="text-xs text-slate-400 block">Demo data</span>
          <div className="grid grid-cols-4 gap-2">
            <button
              type="button"
              disabled={busy !== null || !cfg?.edit_instance_id}
              onClick={() => cfg?.edit_instance_id && window.open(`https://${cfg.edit_instance_id}.${domain}/`, '_blank')}
              title="Edit demo data — your private copy; Publish to make it live"
              aria-label="Edit demo data"
              className="flex items-center justify-center py-1.5 rounded text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 cursor-pointer"
            >
              <Pencil size={13} />
            </button>
            <button
              type="button"
              disabled={busy !== null || !cfg?.edit_instance_id}
              onClick={publish}
              title="Publish — make the edit copy's data what visitors see"
              aria-label="Publish"
              className="flex items-center justify-center py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
            >
              <Upload size={13} />
            </button>
            <button
              type="button"
              disabled={busy !== null || !baselineAt}
              onClick={() => run('scratch', () => removeDemoBaseline(projectId), 'Edits discarded — demos run the original app data')}
              title="Discard edits — drop published data and edit-copy changes; demos boot the original app"
              aria-label="Discard edits"
              className="flex items-center justify-center py-1.5 rounded text-xs font-medium bg-slate-800 text-slate-400 hover:text-red-400 disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
            >
              <Trash2 size={13} />
            </button>
            <button
              type="button"
              disabled={busy !== null || !baselineAt || mode !== 'shared'}
              onClick={resetNow}
              title={mode === 'shared' ? 'Reset — shared demo back to the published data' : 'Personal demos reset automatically — nothing to reset'}
              aria-label="Reset to published data"
              className="flex items-center justify-center py-1.5 rounded text-xs font-medium bg-slate-800 text-slate-400 hover:text-slate-200 disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
            >
              <RotateCcw size={13} />
            </button>
          </div>
          <p className="text-[11px] leading-relaxed text-slate-400">
            {baselineAt
              ? <>Visitors see the data published {formatCaptured(baselineAt)}</>
              : 'Fresh app state — nothing published; visitors get a freshly initialized app'}
          </p>
        </div>
      </div>
    </div>
  );
}
