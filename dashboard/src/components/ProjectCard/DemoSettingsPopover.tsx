import { ChevronDown, FlaskConical, RotateCcw } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { type DemoState, captureDemoBaseline, putDemoConfig, removeDemoBaseline, resetDemo } from '../../api';
import { useToast } from '../ToastContext';

/** "07/11/2026 8:10:12 pm" — locale date + 12-hour time with am/pm. */
function formatCaptured(epochSeconds: number): string {
  const d = new Date(epochSeconds * 1000);
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', second: '2-digit' })}`;
}

interface DemoSettingsPopoverProps {
  projectId: string;
  state: DemoState;
  /** Refresh the card's demo state (chips) after any change. */
  onDemoChanged: () => void;
  onClose: () => void;
}

/** Demo mode settings — mode, lifecycle options, banner, baseline.
 *  Replaces the Sleep slot on the card while demo mode is on. */
export default function DemoSettingsPopover({ projectId, state, onDemoChanged, onClose }: DemoSettingsPopoverProps) {
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
  const hadBaseline = !!cfg?.baseline_captured_at;
  const [useBaseline, setUseBaseline] = useState(hadBaseline);
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
      // Baseline checkbox applies transitions only — capturing on every save
      // would restart the app each time; unchecking removes the seed.
      if (useBaseline && !hadBaseline) {
        await captureDemoBaseline(projectId);
        showToast('Saved — baseline capture started (the app restarts briefly)', 'success');
      } else if (!useBaseline && hadBaseline) {
        await removeDemoBaseline(projectId);
        showToast('Saved — baseline removed, demos start from image defaults', 'success');
      } else {
        showToast('Demo settings saved', 'success');
      }
      onDemoChanged();
      // Capture runs async server-side; refresh again once it has settled.
      if (useBaseline && !hadBaseline) setTimeout(onDemoChanged, 4000);
    } catch (e) {
      const message = e instanceof Error ? e.message : 'Failed to save demo settings';
      setError(message);
      showToast(message);
    } finally {
      setBusy(null);
    }
  };

  const disable = () =>
    run('disable', () => putDemoConfig(projectId, { enabled: false }), 'Demo mode disabled — demo traffic now hits this app directly');

  const resetNow = () => run('reset', () => resetDemo(projectId), 'Demo reset started');

  const capture = () => run('capture', () => captureDemoBaseline(projectId), 'Baseline capture started (the app restarts briefly)');

  const baselineAt = cfg?.baseline_captured_at;

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
                className={`flex-1 py-1.5 rounded text-xs font-medium capitalize transition-colors cursor-pointer ${
                  mode === m ? 'bg-teal-600 text-white' : 'bg-slate-700/60 text-slate-300 hover:bg-slate-700'
                }`}
              >
                {m}
              </button>
            ))}
          </div>
          <p className="text-[11px] leading-relaxed text-slate-400 mt-1.5">
            {mode === 'shared'
              ? 'One demo instance for all visitors — data resets on the interval below.'
              : 'Every visitor gets a private instance with fresh data for the TTL below.'}
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
          <span className="text-xs text-slate-400 block mb-1.5">Bottom bar text</span>
          <input
            type="text"
            value={bannerText}
            onChange={(e) => setBannerText(e.target.value)}
            className="w-full bg-slate-700 border border-slate-600 rounded px-2 py-1.5 text-xs text-slate-200 focus:outline-none focus:border-violet-500"
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

        {/* Baseline */}
        <div className="border-t border-slate-700/50 pt-2 space-y-1.5">
          <span className="text-xs text-slate-400 block">Demo data</span>
          <div className="flex gap-1">
            {([false, true] as const).map((v) => (
              <button
                key={String(v)}
                type="button"
                onClick={() => setUseBaseline(v)}
                className={`flex-1 py-1.5 rounded text-xs font-medium transition-colors cursor-pointer ${
                  useBaseline === v ? 'bg-teal-600 text-white' : 'bg-slate-700/60 text-slate-300 hover:bg-slate-700'
                }`}
              >
                {v ? 'Current data' : 'Deployed data'}
              </button>
            ))}
          </div>
          <p className="text-[11px] leading-relaxed text-slate-400">
            {useBaseline
              ? baselineAt
                ? <>Last stored data captured at {formatCaptured(baselineAt)} · <button type="button" disabled={busy !== null} onClick={capture} className="text-teal-300 hover:text-teal-200 disabled:opacity-50 cursor-pointer underline underline-offset-2">{busy === 'capture' ? 'capturing…' : 'Recapture'}</button></>
                : 'Save captures the app’s current data'
              : mode === 'shared'
                ? 'Resets restore the app’s deployed data — whatever the image ships'
                : "Each visitor's instance starts from the app’s deployed data"}
          </p>
        </div>

        {/* Actions */}
        <div className="flex gap-2">
          <button
            type="button"
            disabled={busy !== null}
            onClick={save}
            className="flex-1 py-1.5 rounded text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 cursor-pointer"
          >
            {busy === 'save' ? 'Saving…' : 'Save'}
          </button>
          {mode === 'shared' && (
            <button
              type="button"
              disabled={busy !== null}
              onClick={resetNow}
              title={useBaseline && baselineAt ? 'Wipes the app and restores the captured data' : 'Wipes the app back to clean state'}
              className="flex-1 flex items-center justify-center gap-1.5 py-1.5 rounded text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 cursor-pointer"
            >
              <RotateCcw size={11} />
              {busy === 'reset'
                ? useBaseline && baselineAt
                  ? 'Restoring…'
                  : 'Resetting…'
                : useBaseline && baselineAt
                  ? 'Restore now'
                  : 'Reset now'}
            </button>
          )}
        </div>
        <button
          type="button"
          disabled={busy !== null}
          onClick={disable}
          className="w-full py-1.5 rounded text-xs font-medium bg-slate-800 text-slate-400 hover:text-red-400 disabled:opacity-50 cursor-pointer"
        >
          {busy === 'disable' ? 'Disabling…' : 'Disable demo mode'}
        </button>
      </div>
    </div>
  );
}
