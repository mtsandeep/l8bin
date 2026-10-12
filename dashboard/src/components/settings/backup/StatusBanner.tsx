import type { ReactNode } from "react";
import { formatBytes, timeAgo, type BackupStatus } from "../../../api";
import type { DerivedBackup } from "./helpers";

interface Props {
  status: BackupStatus;
  derived: DerivedBackup;
  children?: ReactNode;
}

/** Status header: health + title + action on top, labeled stat grid below. */
export default function StatusBanner({ status, derived, children }: Props) {
  const { oldestMs, s3Configured, newestDaily, dailyStale } = derived;

  const destinationDown =
    status.s3.configured && status.destination_health?.ok === false;

  return (
    <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 px-4 py-3 space-y-2.5">
      <div className="flex items-center gap-2.5">
        <span
          title={
            destinationDown
              ? (status.destination_health?.detail ?? "")
              : undefined
          }
          className={`h-2 w-2 rounded-full shrink-0 ${
            status.sidecar_running && !destinationDown
              ? "bg-green-400"
              : "bg-amber-400"
          }`}
        />
        <span className="text-xs font-medium text-slate-100">
          {!status.sidecar_running
            ? "Backup sidecar not running"
            : destinationDown
              ? `Backup destination not working${
                  status.destination_health?.failing_since
                    ? ` · failing since ${new Date(status.destination_health.failing_since).toLocaleString(undefined, { hour12: true })}`
                    : ""
                }`
              : "Continuous backup active"}
        </span>
        <span className="ml-auto">{children}</span>
      </div>
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-x-5 gap-y-1.5">
        <div className="min-w-0">
          <p className="text-[11px] text-slate-500">Last write</p>
          <p className="text-xs text-slate-300 truncate">
            {status.last_sync
              ? timeAgo(Math.floor(new Date(status.last_sync).getTime() / 1000))
              : "waiting for first sync…"}
          </p>
        </div>
        <div className="min-w-0">
          <p className="text-[11px] text-slate-500">Coverage</p>
          <p className="text-xs text-slate-300 truncate">
            {oldestMs !== null
              ? `from ${new Date(oldestMs).toLocaleString(undefined, { hour12: true })}`
              : "—"}
          </p>
        </div>
        {s3Configured ? (
          <div className="min-w-0">
            <p className="text-[11px] text-slate-500">Local daily</p>
            <p
              className={`text-xs truncate ${dailyStale ? "text-amber-400" : "text-slate-300"}`}
            >
              {newestDaily
                ? `${timeAgo(Math.floor(new Date(newestDaily.timestamp).getTime() / 1000))} · ${status.dailies.length} kept`
                : "first snapshot coming"}
            </p>
          </div>
        ) : (
          <div className="min-w-0">
            <p className="text-[11px] text-slate-500">Live database</p>
            <p className="text-xs text-slate-300 truncate">
              {status.db_size_bytes != null
                ? formatBytes(status.db_size_bytes)
                : "—"}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
