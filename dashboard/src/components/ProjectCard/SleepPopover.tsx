import { useState, useEffect, useRef } from "react";
import { ChevronDown, Moon } from "lucide-react";
import { useToast } from "../ToastContext";
import { type Project, updateProjectSettings } from "../../api";

interface SleepPopoverProps {
  project: Project;
  onChange: (autoStop: boolean, timeoutMins: number, autoStart: boolean) => void;
  onClose: () => void;
}

export default function SleepPopover({
  project,
  onChange,
  onClose,
}: SleepPopoverProps) {
  const [autoStop, setAutoStop] = useState(project.auto_stop_enabled);
  const [timeoutMins, setTimeoutMins] = useState(project.auto_stop_timeout_mins);
  const [autoStart, setAutoStart] = useState(project.auto_start_enabled);
  const [settingsError, setSettingsError] = useState<string | null>(null);

  const initialRef = useRef({ autoStop, timeoutMins, autoStart });
  const ref = useRef<HTMLDivElement>(null);
  const { showToast } = useToast();

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        handleSaveAndClose();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  });

  const handleSaveAndClose = async () => {
    const initial = initialRef.current;
    const patch: Parameters<typeof updateProjectSettings>[1] = {};

    if (autoStop !== initial.autoStop) patch.auto_stop_enabled = autoStop;
    if (timeoutMins !== initial.timeoutMins) patch.auto_stop_timeout_mins = timeoutMins;
    if (autoStart !== initial.autoStart) patch.auto_start_enabled = autoStart;

    if (Object.keys(patch).length > 0) {
      setSettingsError(null);
      try {
        await updateProjectSettings(project.id, patch);
        onChange(autoStop, timeoutMins, autoStart);
      } catch (e) {
        setSettingsError(e instanceof Error ? e.message : "Failed to update settings");
        showToast(e instanceof Error ? e.message : "Failed to update settings");
        return; // don't close on error
      }
    }
    onClose();
  };

  return (
    <div ref={ref}>
      <button
        onClick={(e) => e.stopPropagation()}
        className={`w-full flex items-center justify-between px-3 py-2 rounded-md border transition-colors cursor-pointer ${
          "bg-slate-900/80 border-violet-500/40 text-slate-300"
        }`}
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
        <label className="flex items-center justify-between gap-2 cursor-pointer">
          <span className="text-xs text-slate-300">Auto-stop when idle</span>
          <button
            role="switch"
            aria-checked={autoStop}
            onClick={() => setAutoStop(!autoStop)}
            className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStop ? "bg-violet-500" : "bg-slate-600"}`}
          >
            <span className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStop ? "translate-x-3.5" : "translate-x-0.5"}`} />
          </button>
        </label>
        {autoStop && (
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-slate-300">Idle timeout (mins)</span>
            <input
              type="number"
              min={1}
              value={timeoutMins}
              onChange={(e) => setTimeoutMins(Number(e.target.value))}
              className="w-16 bg-slate-700 border border-slate-600 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500"
            />
          </div>
        )}
        <label className="flex items-center justify-between gap-2 cursor-pointer">
          <span className="text-xs text-slate-300">Auto-start on visit</span>
          <button
            role="switch"
            aria-checked={autoStart}
            onClick={() => setAutoStart(!autoStart)}
            className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${autoStart ? "bg-violet-500" : "bg-slate-600"}`}
          >
            <span className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${autoStart ? "translate-x-3.5" : "translate-x-0.5"}`} />
          </button>
        </label>
      </div>
    </div>
  );
}
