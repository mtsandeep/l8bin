import { AlertTriangle } from "lucide-react";
import { useState } from "react";
import { revertBackupRestore } from "../../../api";
import type { Busy, RunFn } from "./helpers";

interface Props {
  revertibleTo: string | null;
  busy: Busy;
  run: RunFn;
  onStarted: () => void;
}

/** Undo last restore: row + confirmation modal, shown only when an undo exists. */
export default function UndoRestore({
  revertibleTo,
  busy,
  run,
  onStarted,
}: Props) {
  const [showConfirm, setShowConfirm] = useState(false);
  if (!revertibleTo) return null;

  return (
    <div className="pt-3 border-t border-slate-800">
      <button
        type="button"
        onClick={() => setShowConfirm(true)}
        disabled={busy !== null}
        className="text-xs font-medium text-amber-400/90 hover:text-amber-300 cursor-pointer disabled:opacity-50"
      >
        Undo last restore
      </button>
      <p className="text-[11px] text-slate-600 mt-0.5">
        returns the database to its state from{" "}
        {new Date(revertibleTo).toLocaleString(undefined, { hour12: true })}
      </p>

      {showConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-amber-400" />
              <h3 className="text-sm font-semibold text-slate-100">
                Undo last restore
              </h3>
            </div>
            <p className="text-xs text-slate-300 mb-1">
              Return the database to the state right before the last restore
              (kept as litebin.db.pre-restore)?
            </p>
            <ul className="text-xs text-slate-400 list-disc list-inside mb-3 mt-2 space-y-1">
              <li>Everything written since the restore is discarded</li>
              <li>
                The restored state is kept aside in turn; undoing again flips
                back
              </li>
              <li>Sessions are restored — you will sign in again</li>
            </ul>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setShowConfirm(false)}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() =>
                  run("restore", async () => {
                    await revertBackupRestore();
                    setShowConfirm(false);
                    onStarted();
                  })
                }
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-amber-600 text-white hover:bg-amber-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === "restore" ? "Undoing…" : "Undo restore"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
