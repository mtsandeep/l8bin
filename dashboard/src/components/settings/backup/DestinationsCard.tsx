import {
  Cloud,
  CloudBackup,
  DatabaseBackup,
  HardDrive,
  Pencil,
  Plus,
  Trash2,
} from "lucide-react";
import { useEffect, useState } from "react";
import {
  addBackupS3,
  formatBytes,
  removeBackupS3,
  timeAgo,
  type BackupStatus,
} from "../../../api";
import { useToast } from "../../ToastContext";
import SettingsCard from "../SettingsCard";
import type { Busy, DerivedBackup, RunFn } from "./helpers";
import { inputCls, s3Provider } from "./helpers";

interface Props {
  status: BackupStatus;
  derived: DerivedBackup;
  busy: Busy;
  run: RunFn;
  onStatus: (status: BackupStatus) => void;
}

/** Destinations: R2/S3 (primary when configured) and the local copy. */
export default function DestinationsCard({
  status,
  derived,
  busy,
  run,
  onStatus,
}: Props) {
  const { s3Configured, replica, oldestMs, hasData, newestDaily } = derived;
  const { showToast } = useToast();

  const [formOpen, setFormOpen] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [bucket, setBucket] = useState("");
  const [prefix, setPrefix] = useState("");
  const [accessKey, setAccessKey] = useState("");
  const [secretKey, setSecretKey] = useState("");

  // Keep the form prefilled from status while it is closed.
  useEffect(() => {
    if (formOpen) return;
    setEndpoint(status.s3.endpoint || "");
    setBucket(status.s3.bucket || "");
    setPrefix(status.s3.prefix || "");
  }, [formOpen, status.s3.endpoint, status.s3.bucket, status.s3.prefix]);

  const formValid =
    endpoint.trim() && bucket.trim() && accessKey.trim() && secretKey.trim();

  return (
    <SettingsCard icon={HardDrive} title="Destinations">
      <div className="space-y-2">
        {/* R2/S3: primary when configured */}
        {s3Configured && !formOpen ? (
          <div className="rounded-md border border-cyan-700/30 bg-cyan-500/[0.03] px-3 py-2.5">
            <div className="flex items-center gap-3">
              <CloudBackup size={14} className="text-cyan-300 shrink-0" />
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-slate-200 truncate">
                  {status.s3.bucket}
                </p>
                <p className="text-xs text-slate-500">
                  {hasData && replica != null
                    ? `${replica.file_count} files · ${formatBytes(replica.size_bytes)} · restorable from ${
                        oldestMs !== null
                          ? new Date(oldestMs).toLocaleString(undefined, {
                              hour12: true,
                            })
                          : "—"
                      }`
                    : "waiting for first sync… · 30-day retention"}
                </p>
              </div>
              <span className="text-xs px-1.5 py-0.5 rounded bg-violet-500/15 text-violet-300 font-semibold shrink-0">
                PRIMARY
              </span>
              <span className="text-[11px] px-1.5 py-0.5 rounded border border-slate-600/70 text-slate-400 shrink-0">
                continuous
              </span>
            </div>
            <div className="flex items-center justify-between gap-1 mt-2 pt-2 border-t border-cyan-700/20">
              <span
                title={`${status.s3.endpoint}/${status.s3.prefix}/`}
                className="text-[11px] text-slate-500 cursor-help truncate"
              >
                {s3Provider(status.s3.endpoint)}
              </span>
              <div className="flex items-center gap-1 shrink-0">
                <button
                  type="button"
                  onClick={() => {
                    setFormOpen(true);
                    setAccessKey("");
                    setSecretKey("");
                  }}
                  disabled={busy !== null}
                  className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs font-medium text-slate-300 hover:text-slate-100 hover:bg-slate-700/50 transition-colors cursor-pointer disabled:opacity-50"
                >
                  <Pencil size={11} /> Credentials
                </button>
                <button
                  type="button"
                  onClick={() =>
                    run("remove-s3", async () => {
                      const res = await removeBackupS3();
                      onStatus(res.status);
                      showToast(
                        "R2/S3 destination removed; local backup keeps running",
                        "success",
                      );
                    })
                  }
                  disabled={busy !== null}
                  className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs font-medium text-slate-400 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50"
                >
                  <Trash2 size={11} /> Remove
                </button>
              </div>
            </div>
          </div>
        ) : null}

        {/* Local — primary when no R2/S3 */}
        <div className="flex items-center gap-3 rounded-md border border-slate-700/40 px-3 py-2.5">
          <DatabaseBackup size={14} className="text-green-400 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="text-xs font-medium text-slate-200">
              {s3Configured ? "Local daily backup" : "This server"}
            </p>
            <p className="text-xs text-slate-500">
              {s3Configured
                ? newestDaily
                  ? `${status.dailies.length} snapshot${status.dailies.length > 1 ? "s" : ""} · latest ${timeAgo(Math.floor(new Date(newestDaily.timestamp).getTime() / 1000))} · 30-day retention`
                  : "daily snapshot from R2/S3 · 30-day retention"
                : hasData && replica != null
                  ? `${replica.file_count} files · ${formatBytes(replica.size_bytes)} · 30-day retention`
                  : "backups/litebin.db · 30-day retention"}
            </p>
          </div>
          {s3Configured ? (
            <>
              <span className="text-xs px-1.5 py-0.5 rounded bg-slate-700/60 text-slate-400 font-semibold shrink-0">
                SECONDARY
              </span>
              <span className="text-[11px] px-1.5 py-0.5 rounded border border-slate-600/70 text-slate-400 shrink-0">
                daily
              </span>
            </>
          ) : (
            <>
              <span className="text-xs px-1.5 py-0.5 rounded bg-violet-500/15 text-violet-300 font-semibold shrink-0">
                PRIMARY
              </span>
              <span className="text-[11px] px-1.5 py-0.5 rounded border border-slate-600/70 text-slate-400 shrink-0">
                continuous
              </span>
            </>
          )}
        </div>

        {/* R2/S3: add row (when not configured and form closed) */}
        {!s3Configured && !formOpen ? (
          <button
            type="button"
            onClick={() => setFormOpen(true)}
            disabled={busy !== null}
            className="w-full flex items-center justify-center gap-1.5 rounded-md border border-dashed border-slate-700 px-3 py-2.5 text-xs text-slate-400 hover:text-slate-200 hover:border-slate-500 transition-colors cursor-pointer disabled:opacity-50"
          >
            <Plus size={13} />
            Add R2/S3 backup destination
          </button>
        ) : null}

        {/* R2/S3 form */}
        {formOpen ? (
          <div className="rounded-md border border-slate-700/40 p-3.5 space-y-3">
            <div className="flex items-center gap-2">
              <Cloud size={13} className="text-cyan-300" />
              <p className="text-xs font-medium text-slate-200">
                {s3Configured
                  ? "Update R2/S3 credentials"
                  : "Add R2/S3 destination"}
              </p>
            </div>
            <div>
              <label
                htmlFor="backup-s3-endpoint"
                className="block text-xs text-slate-400 mb-1"
              >
                Endpoint
              </label>
              <input
                id="backup-s3-endpoint"
                type="text"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder="https://<account_id>.r2.cloudflarestorage.com"
                className={inputCls}
              />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label
                  htmlFor="backup-s3-bucket"
                  className="block text-xs text-slate-400 mb-1"
                >
                  Bucket
                </label>
                <input
                  id="backup-s3-bucket"
                  type="text"
                  value={bucket}
                  onChange={(e) => setBucket(e.target.value)}
                  placeholder="my-backups"
                  className={inputCls}
                />
              </div>
              <div>
                <label
                  htmlFor="backup-s3-prefix"
                  className="block text-xs text-slate-400 mb-1"
                >
                  Prefix
                </label>
                <input
                  id="backup-s3-prefix"
                  type="text"
                  value={prefix}
                  onChange={(e) => setPrefix(e.target.value)}
                  placeholder="litebin/"
                  className={inputCls}
                />
              </div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label
                  htmlFor="backup-s3-access"
                  className="block text-xs text-slate-400 mb-1"
                >
                  Access key ID
                </label>
                <input
                  id="backup-s3-access"
                  type="text"
                  value={accessKey}
                  onChange={(e) => setAccessKey(e.target.value)}
                  placeholder="Access key ID"
                  className={inputCls}
                />
              </div>
              <div>
                <label
                  htmlFor="backup-s3-secret"
                  className="block text-xs text-slate-400 mb-1"
                >
                  Secret access key
                </label>
                <input
                  id="backup-s3-secret"
                  type="password"
                  value={secretKey}
                  onChange={(e) => setSecretKey(e.target.value)}
                  placeholder="Secret access key"
                  className={inputCls}
                />
              </div>
            </div>
            <div className="flex gap-2 pt-0.5">
              <button
                type="button"
                onClick={() =>
                  run("s3", async () => {
                    const res = await addBackupS3({
                      endpoint,
                      bucket,
                      prefix: prefix || undefined,
                      access_key_id: accessKey,
                      secret_access_key: secretKey,
                    });
                    onStatus(res.status);
                    setAccessKey("");
                    setSecretKey("");
                    setFormOpen(false);
                    showToast(
                      "Destination saved; backup now streams to R2/S3",
                      "success",
                    );
                  })
                }
                disabled={busy !== null || !formValid}
                className="px-4 py-1.5 rounded-md text-xs font-medium bg-cyan-600/80 text-white hover:bg-cyan-500 transition-colors disabled:opacity-50 cursor-pointer"
              >
                {busy === "s3" ? "Saving…" : "Save destination"}
              </button>
              <button
                type="button"
                onClick={() => setFormOpen(false)}
                disabled={busy !== null}
                className="px-4 py-1.5 rounded-md text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer disabled:opacity-50"
              >
                Cancel
              </button>
            </div>
          </div>
        ) : null}
      </div>
    </SettingsCard>
  );
}
