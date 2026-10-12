import { Bookmark, X } from "lucide-react";
import {
  deleteBackupPoint,
  restoreBackupPoint,
  type BackupPoint,
  type BackupStatus,
  formatBytes,
} from "../../../api";
import type { Busy, FileRestore, RunFn } from "./helpers";

interface Props {
  points: BackupPoint[];
  /** Top divider — false when the list is the card's only content. */
  divider?: boolean;
  busy: Busy;
  run: RunFn;
  onStatus: (status: BackupStatus) => void;
  onRestore: (fileRestore: FileRestore) => void;
}

/** Manual backups list with per-row restore and delete. */
export default function ManualBackupsList({
  points,
  divider = true,
  busy,
  run,
  onStatus,
  onRestore,
}: Props) {
  if (points.length === 0) return null;

  return (
    <div
      className={
        divider ? "pt-3 border-t border-slate-800 space-y-2" : "space-y-2"
      }
    >
      <p className="text-xs text-slate-500">Manual backups</p>
      {points.map((pt) => (
        <div
          key={pt.id}
          className="flex items-center gap-3 rounded-md border border-slate-700/30 bg-slate-900/50 px-3 py-2"
        >
          <Bookmark
            size={13}
            className={
              pt.available
                ? "text-amber-300 shrink-0"
                : "text-slate-600 shrink-0"
            }
          />
          <div className="min-w-0 flex-1">
            <p className="text-xs font-medium text-slate-200 truncate">
              {pt.name}
            </p>
            <p className="text-xs text-slate-500">
              {new Date(pt.timestamp).toLocaleString(undefined, {
                hour12: true,
              })}{" "}
              ·{" "}
              {pt.available
                ? formatBytes(pt.size_bytes)
                : "snapshot file missing"}
            </p>
          </div>
          <button
            type="button"
            onClick={() =>
              onRestore({
                label: `the manual backup "${pt.name}"`,
                hint: "Restores the database exactly as it was when the backup was taken.",
                run: async () => {
                  await restoreBackupPoint(pt.id);
                },
              })
            }
            disabled={busy !== null || !pt.available}
            className="shrink-0 px-2.5 py-1 rounded text-xs font-medium text-slate-300 hover:text-red-300 hover:bg-red-500/10 border border-slate-700/50 transition-colors cursor-pointer disabled:opacity-40"
          >
            Restore
          </button>
          <button
            type="button"
            onClick={() =>
              run("point-del", async () => {
                const res = await deleteBackupPoint(pt.id);
                onStatus(res.status);
              })
            }
            disabled={busy !== null}
            className="shrink-0 p-1 rounded text-slate-500 hover:text-red-400 hover:bg-red-500/10 cursor-pointer disabled:opacity-40"
            title="Delete manual backup and its snapshot"
          >
            <X size={13} />
          </button>
        </div>
      ))}
    </div>
  );
}
