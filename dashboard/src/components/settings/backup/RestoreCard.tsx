import { AlertTriangle, HardDrive, RotateCcw } from "lucide-react";
import { useState } from "react";
import {
  formatBytes,
  restoreBackup,
  restoreBackupLocal,
  type BackupStatus,
} from "../../../api";
import SettingsCard from "../SettingsCard";
import ManualBackupsList from "./ManualBackupsList";
import UndoRestore from "./UndoRestore";
import type { Busy, DerivedBackup, FileRestore, RunFn } from "./helpers";
import {
  fmtTs,
  msAgoIso,
  RESTORE_PICKS,
  toLocalInputValue,
  type RestorePickId,
} from "./helpers";
import { inputCls } from "./helpers";

interface Props {
  status: BackupStatus;
  derived: DerivedBackup;
  busy: Busy;
  run: RunFn;
  onStatus: (status: BackupStatus) => void;
  onFileRestore: (fileRestore: FileRestore) => void;
  onRestarting: () => void;
}

/** Replica point-in-time restore + daily snapshots + manual backups + undo. */
export default function RestoreCard({
  status,
  derived,
  busy,
  run,
  onStatus,
  onFileRestore,
  onRestarting,
}: Props) {
  const { s3Configured, oldestMs, hasData } = derived;
  const [pick, setPick] = useState<RestorePickId | null>("latest");
  const [timestamp, setTimestamp] = useState("");
  const [customValue, setCustomValue] = useState("");
  const [showConfirm, setShowConfirm] = useState(false);
  const [ack, setAck] = useState(false);

  const pickCovered = (ms: number) =>
    hasData && Date.now() - ms >= (oldestMs ?? 0);
  const choosePick = (id: (typeof RESTORE_PICKS)[number]["id"]) => {
    setPick(id);
    const found = RESTORE_PICKS.find((p) => p.id === id);
    if (found) setTimestamp(msAgoIso(found.ms));
  };
  const chooseCustom = (value: string) => {
    setCustomValue(value);
    setTimestamp(
      value ? new Date(value).toISOString().replace(/\.\d+Z$/, "Z") : "",
    );
  };
  const selectedLabel =
    pick && pick !== "latest" && pick !== "custom"
      ? RESTORE_PICKS.find((p) => p.id === pick)?.label
      : timestamp.trim()
        ? fmtTs(timestamp.trim())
        : "the latest backup state";
  const namedPoint = timestamp.trim()
    ? status.points.find((p) => p.timestamp === timestamp.trim())
    : undefined;
  // A requested time newer than the last write resolves to the last write
  // (litestream semantics) — surface that instead of implying data at that moment.
  const requestedMs = timestamp.trim()
    ? new Date(timestamp.trim()).getTime()
    : null;
  const lastWriteMs = status.last_sync
    ? new Date(status.last_sync).getTime()
    : null;
  const resolvesToLastWrite =
    requestedMs != null &&
    lastWriteMs != null &&
    requestedMs > lastWriteMs &&
    !namedPoint;

  return (
    <SettingsCard icon={RotateCcw} title="Restore">
      <div className="space-y-4">
        <p className="text-xs text-slate-500 -mt-1">
          {s3Configured
            ? "Point-in-time from your R2/S3 destination"
            : "Point-in-time from the local replica"}
        </p>
        <div>
          <div className="flex flex-wrap gap-1.5 mb-3">
            <button
              type="button"
              onClick={() => {
                setPick("latest");
                setTimestamp("");
              }}
              disabled={!hasData}
              className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
                !hasData
                  ? "bg-slate-900 text-slate-600 border border-slate-700/30 cursor-not-allowed"
                  : pick === "latest"
                    ? "bg-violet-500/15 text-violet-300 border border-violet-500/40 cursor-pointer"
                    : "bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200 cursor-pointer"
              }`}
              title={hasData ? undefined : "No backup data yet"}
            >
              Latest
            </button>
            {RESTORE_PICKS.map((entry) => {
              const covered = pickCovered(entry.ms);
              return (
                <button
                  key={entry.id}
                  type="button"
                  onClick={() => choosePick(entry.id)}
                  disabled={!covered}
                  className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
                    !covered
                      ? "bg-slate-900 text-slate-600 border border-slate-700/30 cursor-not-allowed"
                      : pick === entry.id
                        ? "bg-violet-500/15 text-violet-300 border border-violet-500/40 cursor-pointer"
                        : "bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200 cursor-pointer"
                  }`}
                  title={
                    covered
                      ? undefined
                      : "Backup history does not reach this far back yet"
                  }
                >
                  {entry.label}
                </button>
              );
            })}
            <button
              type="button"
              onClick={() => setPick("custom")}
              disabled={!hasData}
              className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
                !hasData
                  ? "bg-slate-900 text-slate-600 border border-slate-700/30 cursor-not-allowed"
                  : pick === "custom"
                    ? "bg-violet-500/15 text-violet-300 border border-violet-500/40 cursor-pointer"
                    : "bg-slate-900 text-slate-400 border border-slate-700/50 hover:text-slate-200 cursor-pointer"
              }`}
              title={hasData ? undefined : "No backup data yet"}
            >
              Custom…
            </button>
          </div>
          {pick === "custom" && hasData && (
            <input
              id="backup-restore-ts"
              type="datetime-local"
              value={customValue}
              onChange={(e) => chooseCustom(e.target.value)}
              min={oldestMs !== null ? toLocalInputValue(oldestMs) : undefined}
              max={toLocalInputValue(Date.now())}
              className={inputCls}
              style={{ colorScheme: "dark" }}
            />
          )}
          {!hasData && (
            <p className="text-xs text-slate-600 mt-1.5">
              Available after the first sync (~1 minute).
            </p>
          )}
        </div>

        <p className="text-xs text-slate-500">
          Restoring to{" "}
          <span className="text-slate-300 font-medium">
            {namedPoint
              ? `"${namedPoint.name}" (${selectedLabel})`
              : selectedLabel}
            {resolvesToLastWrite && status.last_sync
              ? ` — resolves to the latest state (${fmtTs(status.last_sync)})`
              : ""}
          </span>{" "}
          — everything after that point is discarded.
        </p>

        <button
          type="button"
          onClick={() => {
            setAck(false);
            setShowConfirm(true);
          }}
          disabled={busy !== null || !status.sidecar_running || !hasData}
          className="px-4 py-2 rounded-md text-xs font-medium bg-red-600/80 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
        >
          Restore database
        </button>

        {/* Daily snapshots */}
        {s3Configured && status.dailies.length > 0 ? (
          <div className="pt-3 border-t border-slate-800 space-y-2">
            <p className="text-xs text-slate-500">
              Daily snapshots{" "}
              <span className="text-slate-600">(from R2/S3, local copies)</span>
            </p>
            {status.dailies.map((daily) => (
              <div
                key={daily.date}
                className="flex items-center gap-3 rounded-md border border-slate-700/30 bg-slate-900/50 px-3 py-2"
              >
                <HardDrive size={13} className="text-slate-400 shrink-0" />
                <div className="min-w-0 flex-1">
                  <p className="text-xs font-medium text-slate-300">
                    {daily.date}
                  </p>
                  <p className="text-xs text-slate-500">
                    taken{" "}
                    {new Date(daily.timestamp).toLocaleString(undefined, {
                      hour12: true,
                    })}{" "}
                    · {formatBytes(daily.size_bytes)}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() =>
                    onFileRestore({
                      label: `the local daily snapshot from ${daily.date}`,
                      hint: "Bypasses the R2/S3 destination; use only if it is unreachable.",
                      run: async () => {
                        await restoreBackupLocal(daily.date);
                      },
                    })
                  }
                  disabled={busy !== null}
                  className="shrink-0 px-2.5 py-1 rounded text-xs font-medium text-slate-300 hover:text-red-300 hover:bg-red-500/10 border border-slate-700/50 transition-colors cursor-pointer disabled:opacity-40"
                >
                  Restore
                </button>
              </div>
            ))}
          </div>
        ) : null}

        <ManualBackupsList
          points={status.points}
          busy={busy}
          run={run}
          onStatus={onStatus}
          onRestore={onFileRestore}
        />

        <UndoRestore
          revertibleTo={status.revertible_to}
          busy={busy}
          run={run}
          onStarted={onRestarting}
        />

        {/* Point-in-time restore confirmation */}
        {showConfirm && (
          <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
            <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
              <div className="flex items-center gap-2 mb-3">
                <AlertTriangle size={16} className="text-red-400" />
                <h3 className="text-sm font-semibold text-slate-100">
                  Restore database
                </h3>
              </div>
              <p className="text-xs text-slate-300 mb-1">
                Replace the current database with{" "}
                {namedPoint
                  ? `the point "${namedPoint.name}" (${fmtTs(namedPoint.timestamp)})`
                  : resolvesToLastWrite && status.last_sync
                    ? `the latest backup state (${fmtTs(status.last_sync)})`
                    : timestamp.trim()
                      ? `the backup as of ${fmtTs(timestamp.trim())}`
                      : "the backup's latest state"}
                ?
              </p>
              <ul className="text-xs text-slate-400 list-disc list-inside mb-3 space-y-1">
                <li>
                  Discards everything written since — projects, tokens, settings
                </li>
                <li>
                  Sessions are restored from the backup — you will sign in again
                </li>
                <li>Kept on the server: litebin.db.pre-restore</li>
              </ul>
              <label className="flex items-center gap-2 cursor-pointer mb-4">
                <input
                  type="checkbox"
                  checked={ack}
                  onChange={(e) => setAck(e.target.checked)}
                  className="accent-violet-500"
                />
                <span className="text-xs text-slate-400">
                  I understand this cannot be undone
                </span>
              </label>
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
                      await restoreBackup(timestamp.trim() || undefined);
                      setShowConfirm(false);
                      onRestarting();
                    })
                  }
                  disabled={!ack || busy !== null}
                  className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
                >
                  {busy === "restore" ? "Restoring…" : "Restore"}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </SettingsCard>
  );
}
