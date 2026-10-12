import { DatabaseBackup } from "lucide-react";
import { useState } from "react";
import { createBackupPoint, type BackupStatus } from "../../../api";
import { useToast } from "../../ToastContext";
import type { Busy, RunFn } from "./helpers";
import { inputCls } from "./helpers";

interface Props {
  busy: Busy;
  run: RunFn;
  onStatus: (status: BackupStatus) => void;
}

/** Manual backup trigger: button + name modal (defaults to the current date-time). */
export default function ManualBackupButton({ busy, run, onStatus }: Props) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const { showToast } = useToast();

  const defaultName = () => {
    const d = new Date();
    const month = [
      "january",
      "february",
      "march",
      "april",
      "may",
      "june",
      "july",
      "august",
      "september",
      "october",
      "november",
      "december",
    ][d.getMonth()];
    const hour = d.getHours() % 12 || 12;
    const mins = String(d.getMinutes()).padStart(2, "0");
    const ampm = d.getHours() < 12 ? "am" : "pm";
    return `${d.getDate()} ${month} ${d.getFullYear()} ${hour}:${mins} ${ampm}`;
  };

  const create = async () => {
    await run("point", async () => {
      const res = await createBackupPoint(name.trim() || defaultName());
      onStatus(res.status);
      showToast("Manual backup saved, listed under Restore", "success");
    });
    setOpen(false);
  };

  return (
    <>
      <button
        type="button"
        onClick={() => {
          setName(defaultName());
          setOpen(true);
        }}
        disabled={busy !== null}
        className="inline-flex items-center gap-1.5 px-4 py-2 rounded-md text-xs font-medium bg-violet-500/15 text-violet-300 border border-violet-500/40 hover:bg-violet-500/25 transition-colors cursor-pointer disabled:opacity-50 shrink-0"
      >
        <DatabaseBackup size={12} /> Manual backup
      </button>

      {open && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <DatabaseBackup size={16} className="text-violet-400" />
              <h3 className="text-sm font-semibold text-slate-100">
                Create manual backup
              </h3>
            </div>
            <p className="text-xs text-slate-400 mb-3">
              Create snapshot of the live database right now
            </p>
            <label
              htmlFor="manual-backup-name"
              className="block text-xs text-slate-400 mb-1"
            >
              Name
            </label>
            <input
              id="manual-backup-name"
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className={inputCls}
              maxLength={64}
              autoFocus
            />
            <div className="flex gap-2 mt-4">
              <button
                type="button"
                onClick={() => setOpen(false)}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => create()}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === "point" ? "Creating…" : "Create backup"}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
