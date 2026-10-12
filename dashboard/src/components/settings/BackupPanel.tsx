import { AlertTriangle, Check, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { disableBackup, fetchBackupStatus, type BackupStatus } from "../../api";
import { useToast } from "../ToastContext";
import SettingsCard from "./SettingsCard";
import DestinationsCard from "./backup/DestinationsCard";
import ExplainerCard from "./backup/ExplainerCard";
import ManualBackupButton from "./backup/ManualBackupButton";
import ManualBackupsList from "./backup/ManualBackupsList";
import RestoreCard from "./backup/RestoreCard";
import SnapshotRestoreModal from "./backup/SnapshotRestoreModal";
import StatusBanner from "./backup/StatusBanner";
import UndoRestore from "./backup/UndoRestore";
import type { Busy, FileRestore, RunFn } from "./backup/helpers";
import { deriveBackup } from "./backup/helpers";

/**
 * Backup settings page. Owns the shared state (status, busy, restarting,
 * pending file-restore) and composes the cards from ./backup/.
 */
export default function BackupPanel() {
  const [status, setStatus] = useState<BackupStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [fileRestore, setFileRestore] = useState<FileRestore | null>(null);
  const [restarting, setRestarting] = useState(false);
  const [showDisableConfirm, setShowDisableConfirm] = useState(false);
  const { showToast } = useToast();

  const refresh = useCallback(() => {
    fetchBackupStatus()
      .then((s) => {
        setStatus(s);
        setError(null);
      })
      .catch(() => setError("Failed to load backup status"));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Keep the status live while backup is enabled.
  const enabled = status?.enabled === true;
  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(refresh, 10_000);
    return () => clearInterval(timer);
  }, [enabled, refresh]);

  // After a restore, the orchestrator restarts — poll until it answers again.
  useEffect(() => {
    if (!restarting) return;
    const timer = setInterval(() => {
      fetchBackupStatus()
        .then(() => {
          setRestarting(false);
          showToast("Database restored from backup", "success");
        })
        .catch(() => {});
    }, 1500);
    return () => clearInterval(timer);
  }, [restarting, showToast]);

  const run: RunFn = async (op, fn) => {
    setBusy(op);
    setError(null);
    try {
      await fn();
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Operation failed";
      setError(msg);
      showToast(msg);
    } finally {
      setBusy(null);
    }
  };

  if (!status) {
    return (
      <p className="text-xs text-slate-400">
        {error ?? "Loading backup status…"}
      </p>
    );
  }

  if (restarting) {
    return (
      <div className="rounded-lg border border-slate-700/50 bg-slate-900/50 py-10 space-y-4">
        <div className="flex flex-col items-center gap-3">
          <RotateCcw size={22} className="text-violet-400 animate-spin" />
          <p className="text-sm font-medium text-slate-100">Applying restore</p>
        </div>
        <ol className="max-w-xs mx-auto space-y-2 text-xs">
          <li className="flex items-center gap-2 text-slate-300">
            <Check size={13} className="text-green-400" /> Restore snapshot
            staged
          </li>
          <li className="flex items-center gap-2 text-slate-300">
            <span className="w-[13px] h-[13px] rounded-full border-2 border-violet-400 border-t-transparent animate-spin" />
            Orchestrator restarting…
          </li>
          <li className="flex items-center gap-2 text-slate-500">
            <span className="w-[13px] h-[13px] rounded-full border-2 border-slate-600" />{" "}
            Back online shortly
          </li>
        </ol>
        <p className="text-xs text-slate-500 text-center max-w-xs mx-auto">
          You may be signed out — sessions are restored from the backup. Sign in
          again if prompted.
        </p>
      </div>
    );
  }

  const derived = deriveBackup(status);

  // ── Not enabled: explainer + restore options ─────────────────────────────
  if (!status.enabled) {
    return (
      <div className="space-y-4">
        {error && (
          <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">
            {error}
          </div>
        )}
        <ExplainerCard
          status={status}
          busy={busy}
          run={run}
          onStatus={setStatus}
        >
          <ManualBackupButton busy={busy} run={run} onStatus={setStatus} />
        </ExplainerCard>
        {(status.points.length > 0 || status.revertible_to) && (
          <SettingsCard icon={RotateCcw} title="Restore">
            <div className="space-y-4">
              <ManualBackupsList
                points={status.points}
                divider={false}
                busy={busy}
                run={run}
                onStatus={setStatus}
                onRestore={setFileRestore}
              />
              <UndoRestore
                revertibleTo={status.revertible_to}
                busy={busy}
                run={run}
                onStarted={() => setRestarting(true)}
              />
            </div>
          </SettingsCard>
        )}
        <SnapshotRestoreModal
          fileRestore={fileRestore}
          busy={busy}
          run={run}
          onClose={() => setFileRestore(null)}
          onStarted={() => setRestarting(true)}
        />
      </div>
    );
  }

  // ── Enabled: status, destinations, restore ───────────────────────────────
  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">
          {error}
        </div>
      )}

      <StatusBanner status={status} derived={derived}>
        <ManualBackupButton busy={busy} run={run} onStatus={setStatus} />
      </StatusBanner>

      <DestinationsCard
        status={status}
        derived={derived}
        busy={busy}
        run={run}
        onStatus={setStatus}
      />

      <RestoreCard
        status={status}
        derived={derived}
        busy={busy}
        run={run}
        onStatus={setStatus}
        onFileRestore={setFileRestore}
        onRestarting={() => setRestarting(true)}
      />

      {/* Disable */}
      <div className="flex items-center justify-between rounded-lg border border-slate-800 px-4 py-3">
        <div>
          <p className="text-xs text-slate-300 font-medium">Disable backup</p>
          <p className="text-xs text-slate-500 mt-0.5">
            Stops the sidecar. Existing replicas stay on disk and resume when
            re-enabled.
          </p>
        </div>
        <button
          type="button"
          onClick={() => setShowDisableConfirm(true)}
          disabled={busy !== null}
          className="shrink-0 px-3 py-1.5 rounded-md text-xs font-medium text-slate-400 hover:text-red-400 hover:bg-red-500/10 border border-slate-700/50 transition-colors cursor-pointer disabled:opacity-50"
        >
          Disable
        </button>
      </div>

      {/* Disable confirmation */}
      {showDisableConfirm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm">
          <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 p-5 shadow-2xl">
            <div className="flex items-center gap-2 mb-3">
              <AlertTriangle size={16} className="text-amber-400" />
              <h3 className="text-sm font-semibold text-slate-100">
                Disable backup
              </h3>
            </div>
            <p className="text-xs text-slate-400 mb-4">
              Stops continuous backup of the LiteBin database. Existing replicas
              stay on disk and resume from where they left off if you re-enable.
            </p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setShowDisableConfirm(false)}
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() =>
                  run("disable", async () => {
                    const res = await disableBackup();
                    setStatus(res.status);
                    setShowDisableConfirm(false);
                    showToast("Backup disabled", "success");
                  })
                }
                disabled={busy !== null}
                className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === "disable" ? "Disabling…" : "Disable"}
              </button>
            </div>
          </div>
        </div>
      )}

      <SnapshotRestoreModal
        fileRestore={fileRestore}
        busy={busy}
        run={run}
        onClose={() => setFileRestore(null)}
        onStarted={() => setRestarting(true)}
      />
    </div>
  );
}
