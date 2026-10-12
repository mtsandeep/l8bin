import type { BackupStatus } from "../../../api";

export const inputCls =
  "w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500";

export type Busy =
  | "enable"
  | "disable"
  | "s3"
  | "remove-s3"
  | "restore"
  | "point"
  | "point-del"
  | null;

export type RunFn = (op: Busy, fn: () => Promise<void>) => Promise<void>;

/** A pending file-based restore (daily snapshot or manual backup) awaiting confirmation. */
export type FileRestore = {
  label: string;
  hint: string;
  run: () => Promise<void>;
};

/** ISO timestamp for N ms ago (empty for latest). */
export function msAgoIso(ms: number): string {
  return new Date(Date.now() - ms).toISOString().replace(/\.\d+Z$/, "Z");
}

/** Display an ISO timestamp in the browser's locale (raw Z-suffixed ISOs are confusing). */
export function fmtTs(iso: string): string {
  return new Date(iso).toLocaleString(undefined, { hour12: true });
}

/** ms → value for <input type="datetime-local"> ("YYYY-MM-DDTHH:mm", browser-local). */
export function toLocalInputValue(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Provider label from the endpoint — anything non-R2/non-AWS is S3-compatible. */
export function s3Provider(endpoint: string | null): string {
  if (endpoint?.includes("r2.cloudflarestorage.com")) return "Cloudflare R2";
  if (endpoint?.includes("amazonaws.com")) return "AWS S3";
  return "S3 compatible";
}

export const RESTORE_PICKS = [
  { id: "1h", label: "1h ago", ms: 3_600_000 },
  { id: "12h", label: "12h ago", ms: 12 * 3_600_000 },
  { id: "1d", label: "1d ago", ms: 86_400_000 },
  { id: "1w", label: "1w ago", ms: 7 * 86_400_000 },
] as const;

export type RestorePickId =
  "latest" | "custom" | (typeof RESTORE_PICKS)[number]["id"];

/** Shared derived values for the enabled view. */
export function deriveBackup(status: BackupStatus) {
  const s3Configured = status.s3.configured;
  const replica = status.replica;
  const oldestMs = status.coverage_start
    ? new Date(status.coverage_start).getTime()
    : null;
  const hasData = s3Configured || (replica?.file_count ?? 0) > 0;
  const newestDaily = status.dailies[0] ?? null;
  const dailyStale =
    !!newestDaily &&
    Date.now() - new Date(newestDaily.timestamp).getTime() > 26 * 3_600_000;
  return { s3Configured, replica, oldestMs, hasData, newestDaily, dailyStale };
}

export type DerivedBackup = ReturnType<typeof deriveBackup>;
