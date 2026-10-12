import { Cloud, DatabaseBackup, RotateCcw, Zap } from "lucide-react";
import type { ReactNode } from "react";
import { enableBackup, type BackupStatus } from "../../../api";
import { useToast } from "../../ToastContext";
import type { Busy, RunFn } from "./helpers";

interface Props {
  status: BackupStatus;
  busy: Busy;
  run: RunFn;
  onStatus: (status: BackupStatus) => void;
  /** Extra actions next to Enable (the manual backup button). */
  children?: ReactNode;
}

/** Not-enabled explainer: what continuous backup is + enable action. */
export default function ExplainerCard({
  status,
  busy,
  run,
  onStatus,
  children,
}: Props) {
  const { showToast } = useToast();

  return (
    <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 p-6">
      <div className="flex items-center gap-2.5 mb-4">
        <DatabaseBackup size={18} className="text-violet-400" />
        <h2 className="text-sm font-semibold text-slate-100">
          Continuous backup
        </h2>
      </div>
      <p className="text-xs text-slate-400 leading-relaxed mb-5">
        Continuous backup of the LiteBin platform database. A Litestream sidecar
        streams each write the moment it happens.
      </p>
      <ul className="space-y-3 mb-6">
        {[
          {
            icon: Zap,
            title: "Continuous streaming",
            text: "Every write replicated in ~1s — no schedules, no windows",
          },
          {
            icon: RotateCcw,
            title: "Point-in-time restore",
            text: "Roll the database back to any second since backup started",
          },
          {
            icon: Cloud,
            title: "R2/S3 destination",
            text: "Add R2/S3 storage and backups stream there; this server keeps a daily restorable copy",
          },
        ].map(({ icon: Icon, title, text }) => (
          <li key={title} className="flex items-start gap-3">
            <span className="p-1.5 rounded-md bg-violet-500/10 text-violet-300 shrink-0">
              <Icon size={13} />
            </span>
            <div>
              <p className="text-xs font-medium text-slate-200">{title}</p>
              <p className="text-xs text-slate-500 mt-0.5">{text}</p>
            </div>
          </li>
        ))}
      </ul>
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() =>
            run("enable", async () => {
              const res = await enableBackup();
              onStatus(res.status);
              showToast(
                "Backup enabled — Litestream sidecar started",
                "success",
              );
            })
          }
          disabled={busy !== null || !status.available}
          className="px-4 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
        >
          {busy === "enable" ? "Enabling…" : "Enable continuous backup"}
        </button>
        {children}
        <span className="text-xs text-slate-500">
          {status.available
            ? "~5-10 MB RAM"
            : "requires the orchestrator container"}
        </span>
      </div>
      {busy === "enable" && (
        <p className="text-xs text-slate-500 mt-3">
          Pulling the litestream image — may take ~30s on first run.
        </p>
      )}
    </div>
  );
}
