import { useState } from "react";
import type { ServiceInfo } from "../../api";

interface ServiceSelectModalProps {
  projectName: string;
  services: ServiceInfo[];
  title: string;
  confirmLabel: string;
  onConfirm: (selectedServices: string[]) => void;
  onCancel: () => void;
}

export default function ServiceSelectModal({
  projectName,
  services,
  title,
  confirmLabel,
  onConfirm,
  onCancel,
}: ServiceSelectModalProps) {
  const [selected, setSelected] = useState<Set<string>>(
    new Set(services.map((s) => s.service_name)),
  );
  const allSelected = selected.size === services.length;

  const toggle = (name: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const toggleAll = () => {
    if (allSelected) {
      setSelected(new Set());
    } else {
      setSelected(new Set(services.map((s) => s.service_name)));
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-sm mx-4 shadow-2xl">
        <div className="px-5 py-4 border-b border-slate-700/50">
          <h2 className="text-sm font-semibold text-slate-100">
            {title} — {projectName}
          </h2>
        </div>
        <div className="px-5 py-3 space-y-1 max-h-60 overflow-y-auto">
          <button
            onClick={toggleAll}
            className="w-full flex items-center gap-2 px-2 py-1.5 rounded text-xs text-slate-400 hover:bg-slate-700/40 transition-colors cursor-pointer"
          >
            <input
              type="checkbox"
              checked={allSelected}
              onChange={toggleAll}
              className="rounded border-slate-600 bg-slate-900 text-violet-500 focus:ring-violet-500/25 focus:ring-offset-0"
            />
            <span className="font-medium">
              {allSelected ? "Deselect all" : "Select all"}
            </span>
          </button>
          {services.map((svc) => (
            <label
              key={svc.service_name}
              className="flex items-center gap-2 px-2 py-1.5 rounded cursor-pointer hover:bg-slate-700/40 transition-colors"
            >
              <input
                type="checkbox"
                checked={selected.has(svc.service_name)}
                onChange={() => toggle(svc.service_name)}
                className="rounded border-slate-600 bg-slate-900 text-violet-500 focus:ring-violet-500/25 focus:ring-offset-0"
              />
              <div className="flex items-center gap-1.5 min-w-0">
                <span className="text-xs text-slate-300 truncate">
                  {svc.service_name}
                </span>
                {svc.is_public && (
                  <span className="text-[10px] px-1 py-0.5 rounded bg-sky-500/20 text-sky-400">
                    public
                  </span>
                )}
                <span
                  className={`text-[10px] px-1 py-0.5 rounded ${
                    svc.status === "running"
                      ? "bg-emerald-500/20 text-emerald-400"
                      : "bg-slate-700/60 text-slate-500"
                  }`}>
                  {svc.status}
                </span>
              </div>
            </label>
          ))}
        </div>
        <div className="flex gap-2 px-5 py-3 border-t border-slate-700/50">
          <button
            onClick={onCancel}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
          >
            Cancel
          </button>
          <button
            onClick={() => onConfirm(Array.from(selected))}
            disabled={selected.size === 0}
            className="flex-1 py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
