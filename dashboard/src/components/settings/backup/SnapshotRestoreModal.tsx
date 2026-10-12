import { AlertTriangle } from "lucide-react";
import type { Busy, FileRestore, RunFn } from "./helpers";

interface Props {
  fileRestore: FileRestore | null;
  busy: Busy;
  run: RunFn;
  onClose: () => void;
  onStarted: () => void;
}

/** Confirmation for a file-based restore (daily snapshot or manual backup). */
export default function SnapshotRestoreModal({
  fileRestore,
  busy,
  run,
  onClose,
  onStarted,
}: Props) {
  if (!fileRestore) return null;

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
        <div className="flex items-center gap-2 mb-3">
          <AlertTriangle size={16} className="text-amber-400" />
          <h3 className="text-sm font-semibold text-slate-100">
            Restore local snapshot
          </h3>
        </div>
        <p className="text-xs text-slate-300 mb-1">
          Replace the current database with{" "}
          <span className="font-medium">{fileRestore.label}</span>?{" "}
          {fileRestore.hint}
        </p>
        <ul className="text-xs text-slate-400 list-disc list-inside mb-3 mt-2 space-y-1">
          <li>Discards everything written since that snapshot</li>
          <li>
            Sessions are restored from the snapshot — you will sign in again
          </li>
          <li>Kept on the server: litebin.db.pre-restore</li>
        </ul>
        <div className="flex gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={busy !== null}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() =>
              run("restore", async () => {
                await fileRestore.run();
                onClose();
                onStarted();
              })
            }
            disabled={busy !== null}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-amber-600 text-white hover:bg-amber-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            {busy === "restore" ? "Restoring…" : "Restore"}
          </button>
        </div>
      </div>
    </div>
  );
}
