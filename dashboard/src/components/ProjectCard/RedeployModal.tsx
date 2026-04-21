import { useState } from "react";
import type { Project, VolumeMount } from "../../api";

interface RedeployModalProps {
  project: Project;
  appImage: string;
  appPort: number;
  isStopping: boolean;
  onRedeploy: (cleanupVolumes: boolean) => void;
  onCancel: () => void;
}

function shortImage(image: string): string {
  const hash = image.startsWith("sha256:") ? image.slice(7) : image;
  return hash.length > 12 ? hash.slice(0, 12) : hash;
}

export default function RedeployModal({
  project,
  appImage,
  appPort,
  isStopping,
  onRedeploy,
  onCancel,
}: RedeployModalProps) {
  const [cleanup, setCleanup] = useState(false);

  const parsedVolumes: VolumeMount[] = (() => {
    try {
      return project.volumes ? JSON.parse(project.volumes) : [];
    } catch {
      return [];
    }
  })();

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 shadow-2xl">
        <div className="px-5 py-4 border-b border-slate-700/50">
          <h2 className="text-sm font-semibold text-slate-100">Redeploy {project.name || project.id}?</h2>
        </div>
        <div className="px-5 py-4 space-y-3">
          <div className="text-xs text-slate-400">
            Pull latest <span className="text-slate-300 font-mono">{shortImage(appImage)}</span> and restart on port <span className="text-slate-300">{appPort}</span>
          </div>
          {parsedVolumes.length > 0 && (
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={cleanup}
                onChange={(e) => setCleanup(e.target.checked)}
                className="rounded border-slate-600 bg-slate-900 text-violet-500 focus:ring-violet-500/25 focus:ring-offset-0"
              />
              <span className="text-xs text-slate-300">Remove unused volume data</span>
            </label>
          )}
        </div>
        <div className="flex gap-2 px-5 py-3 border-t border-slate-700/50">
          <button
            onClick={onCancel}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
          >
            Cancel
          </button>
          <button
            onClick={() => onRedeploy(cleanup)}
            disabled={isStopping || !appImage.trim()}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            Redeploy
          </button>
        </div>
      </div>
    </div>
  );
}
