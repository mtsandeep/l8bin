import { useState } from "react";
import { AlertTriangle, HardDrive, FolderTree, Folder } from "lucide-react";
import type { Project, ServiceVolumeInfo } from "../../api";

interface DeleteConfirmModalProps {
  project: Project;
  isDeleting: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function DeleteConfirmModal({
  project,
  isDeleting,
  onConfirm,
  onCancel,
}: DeleteConfirmModalProps) {
  const [confirmed, setConfirmed] = useState(false);

  const volumes: ServiceVolumeInfo[] = project.public_stats?.volumes ?? [];

  const namedVolumes = volumes.filter((v) => v.volume_name?.startsWith("litebin_"));
  const relativeBinds = volumes.filter((v) => v.volume_name?.startsWith("projects/"));
  const absoluteBinds = volumes.filter((v) => v.volume_name?.startsWith("/") || !v.volume_name);

  const hasAnyVolumes = volumes.length > 0;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-2 px-5 py-4 border-b border-slate-700/50">
          <AlertTriangle size={16} className="text-red-400 shrink-0" />
          <h2 className="text-sm font-semibold text-slate-100">
            Delete {project.name || project.id}?
          </h2>
        </div>

        {/* Body */}
        <div className="px-5 py-4 space-y-3">
          <p className="text-xs text-slate-400">
            This action is <span className="text-red-400 font-medium">permanent and irreversible</span>.
            All containers, networks, and project data will be removed.
          </p>

          {hasAnyVolumes && (
            <div className="space-y-2">
              <p className="text-xs font-medium text-slate-300">Volume cleanup</p>

              {namedVolumes.length > 0 && (
                <div className="flex items-start gap-2 bg-slate-900/60 border border-slate-700/40 rounded-md px-3 py-2">
                  <HardDrive size={12} className="text-red-400 mt-0.5 shrink-0" />
                  <div>
                    <p className="text-[11px] text-slate-300">
                      Docker volumes removed
                    </p>
                    <p className="text-[10px] text-slate-500 font-mono mt-0.5">
                      {namedVolumes.map((v) => v.volume_name).join(", ")}
                    </p>
                  </div>
                </div>
              )}

              {relativeBinds.length > 0 && (
                <div className="flex items-start gap-2 bg-slate-900/60 border border-slate-700/40 rounded-md px-3 py-2">
                  <FolderTree size={12} className="text-red-400 mt-0.5 shrink-0" />
                  <div>
                    <p className="text-[11px] text-slate-300">
                      Relative bind mounts removed
                    </p>
                    <p className="text-[10px] text-slate-500 font-mono mt-0.5">
                      {relativeBinds.map((v) => v.volume_name).join(", ")}
                    </p>
                  </div>
                </div>
              )}

              {absoluteBinds.length > 0 && (
                <div className="flex items-start gap-2 bg-slate-900/60 border border-amber-500/20 rounded-md px-3 py-2">
                  <Folder size={12} className="text-amber-400 mt-0.5 shrink-0" />
                  <div>
                    <p className="text-[11px] text-slate-300">
                      Absolute bind mounts — not removed
                    </p>
                    <p className="text-[10px] text-amber-400/80 mt-0.5">
                      Clean up manually if needed:{" "}
                      <span className="font-mono">
                        {absoluteBinds.map((v) => v.volume_name || v.container_path).join(", ")}
                      </span>
                    </p>
                  </div>
                </div>
              )}
            </div>
          )}

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
              className="rounded border-slate-600 bg-slate-900 text-red-500 focus:ring-red-500/25 focus:ring-offset-0"
            />
            <span className="text-xs text-slate-400">
              I understand this cannot be undone
            </span>
          </label>
        </div>

        {/* Footer */}
        <div className="flex gap-2 px-5 py-3 border-t border-slate-700/50">
          <button
            onClick={onCancel}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer">
            Cancel
          </button>
          <button
            onClick={onConfirm}
            disabled={!confirmed || isDeleting}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-red-600 text-white hover:bg-red-500 transition-colors disabled:opacity-50 cursor-pointer">
            {isDeleting ? "Deleting..." : "Delete"}
          </button>
        </div>
      </div>
    </div>
  );
}
