import { ChevronDown, Moon } from 'lucide-react';
import { useEffect, useId, useRef, useState } from 'react';
import { fetchNodes, type Project, type SleepMode, updateProjectSettings } from '../../api';
import { useToast } from '../ToastContext';

interface SleepPopoverProps {
  project: Project;
  /** Parent-tracked so a saved choice persists across reopens. */
  sleepMode?: SleepMode;
  onChange: (autoStop: boolean, timeoutMins: number, autoStart: boolean, sleepMode: SleepMode) => void;
  onClose: () => void;
}

export default function SleepPopover({ project, sleepMode: parentSleepMode, onChange, onClose }: SleepPopoverProps) {
  const [autoStop, setAutoStop] = useState(project.auto_stop_enabled);
  const [timeoutMins, setTimeoutMins] = useState(project.auto_stop_timeout_mins);
  const [autoStart, setAutoStart] = useState(project.auto_start_enabled);
  const [sleepMode, setSleepMode] = useState<SleepMode>(parentSleepMode ?? project.sleep_mode ?? 'stop');
  // Unknown (null) until the node list resolves; checkpoint stays disabled meanwhile.
  const [checkpointSupported, setCheckpointSupported] = useState<boolean | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);

  const autoStopWarningId = useId();
  const initialRef = useRef({ autoStop, timeoutMins, autoStart, sleepMode });
  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  useEffect(() => {
    let cancelled = false;
    fetchNodes()
      .then((nodes) => {
        if (cancelled) return;
        const node = nodes.find((n) => n.id === (project.node_id ?? 'local'));
        setCheckpointSupported(node?.supports_checkpoint ?? false);
      })
      .catch(() => !cancelled && setCheckpointSupported(false));
    return () => {
      cancelled = true;
    };
  }, [project.node_id]);

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        handleSaveAndClose();
      }
    };
    const escapeHandler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        handleSaveAndClose();
      }
    };
    document.addEventListener('mousedown', handler);
    document.addEventListener('keydown', escapeHandler);
    return () => {
      document.removeEventListener('mousedown', handler);
      document.removeEventListener('keydown', escapeHandler);
    };
  });

  const handleSaveAndClose = async () => {
    const initial = initialRef.current;
    const patch: Parameters<typeof updateProjectSettings>[1] = {};

    if (autoStop !== initial.autoStop) patch.auto_stop_enabled = autoStop;
    if (timeoutMins !== initial.timeoutMins) patch.auto_stop_timeout_mins = timeoutMins;
    if (autoStart !== initial.autoStart) patch.auto_start_enabled = autoStart;
    if (sleepMode !== initial.sleepMode) patch.sleep_mode = sleepMode;

    if (Object.keys(patch).length > 0) {
      setSettingsError(null);
      try {
        await updateProjectSettings(project.id, patch);
        onChange(autoStop, timeoutMins, autoStart, sleepMode);
      } catch (e) {
        setSettingsError(e instanceof Error ? e.message : 'Failed to update settings');
        showToast(e instanceof Error ? e.message : 'Failed to update settings');
        return; // don't close on error
      }
    }
    onClose();
  };

  return (
    <div ref={ref}>
      <button
        type="button"
        onClick={(e) => e.stopPropagation()}
        className={`w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer ${'bg-slate-900/80 border-violet-500/40 text-slate-300'}`}
      >
        <div className="flex items-center gap-1.5">
          <Moon size={12} />
          <span className="text-[10px] uppercase tracking-wider">Sleep</span>
        </div>
        <ChevronDown size={12} className="text-slate-500" />
      </button>
      <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-slate-800 border border-slate-700/70 rounded-md shadow-xl px-3 py-3 space-y-3">
        {settingsError && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1.5">
            {settingsError}
          </div>
        )}
        {project.is_background ? (
          <p className="text-[11px] leading-relaxed text-slate-400">
            Auto-stop and request-driven auto-start are not available for background projects.
          </p>
        ) : null}
        {!project.is_background && (
          <label className="flex items-center justify-between gap-2 cursor-pointer">
            <span className="text-xs text-slate-300">Auto-stop when idle</span>
            <button
              type="button"
              role="switch"
              aria-checked={autoStop}
              aria-describedby={autoStopWarningId}
              onClick={() => setAutoStop(!autoStop)}
              className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStop ? 'bg-violet-500' : 'bg-slate-600'}`}
            >
              <span
                className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStop ? 'translate-x-3.5' : 'translate-x-0.5'}`}
              />
            </button>
          </label>
        )}
        {!project.is_background && (
          <p id={autoStopWarningId} className="text-[11px] leading-relaxed text-slate-400">
            Auto-stop stops every service in this project. Disable it for scheduled jobs, queue workers, or other tasks
            that must remain running.
          </p>
        )}
        {!project.is_background && autoStop && (
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-slate-300">Idle timeout (mins)</span>
            <input
              type="number"
              min={1}
              value={timeoutMins}
              onChange={(e) => setTimeoutMins(Number(e.target.value))}
              className="w-16 bg-slate-700 border border-slate-600 rounded px-1.5 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500"
            />
          </div>
        )}
        {!project.is_background && autoStop && (
          <div className="space-y-1.5">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-slate-300">Sleep mode</span>
              <div className="flex rounded-md border border-slate-600 overflow-hidden">
                <button
                  type="button"
                  onClick={() => setSleepMode('stop')}
                  className={`px-2 py-0.5 text-[11px] cursor-pointer transition-colors ${sleepMode === 'stop' ? 'bg-violet-500 text-white' : 'bg-slate-700 text-slate-300 hover:bg-slate-600'}`}
                >
                  Stop
                </button>
                <button
                  type="button"
                  disabled={!checkpointSupported}
                  onClick={() => setSleepMode('checkpoint')}
                  title={checkpointSupported ? undefined : 'Requires a podman node with CRIU'}
                  className={`px-2 py-0.5 text-[11px] transition-colors border-l border-slate-600 ${
                    sleepMode === 'checkpoint'
                      ? 'bg-violet-500 text-white'
                      : checkpointSupported
                        ? 'bg-slate-700 text-slate-300 hover:bg-slate-600 cursor-pointer'
                        : 'bg-slate-700 text-slate-500 cursor-not-allowed'
                  }`}
                >
                  Checkpoint
                </button>
              </div>
            </div>
            <p className="text-[11px] leading-relaxed text-slate-400">
              {checkpointSupported
                ? sleepMode === 'checkpoint'
                  ? 'Idle apps are frozen with CRIU and resume in ~1s on the next request — no reboot, state preserved. Uses disk equal to app memory while sleeping.'
                  : 'Idle apps stop and reboot on the next request (seconds for most frameworks).'
                : 'Checkpoint sleep needs a node running podman with CRIU. This project’s node uses plain stop.'}
            </p>
          </div>
        )}
        {!project.is_background && (
          <label className="flex items-center justify-between gap-2 cursor-pointer">
            <span className="text-xs text-slate-300">Auto-start on visit</span>
            <button
              type="button"
              role="switch"
              aria-checked={autoStart}
              onClick={() => setAutoStart(!autoStart)}
              className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStart ? 'bg-violet-500' : 'bg-slate-600'}`}
            >
              <span
                className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStart ? 'translate-x-3.5' : 'translate-x-0.5'}`}
              />
            </button>
          </label>
        )}
      </div>
    </div>
  );
}
