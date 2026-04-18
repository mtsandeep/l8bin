import { DebugBarLive } from "./debug-bar-live";

interface DebugBarProps {
  dataSize: number;
  forcedDelay: number;
  extra?: React.ReactNode;
}

export function DebugBar({ dataSize, forcedDelay, extra }: DebugBarProps) {
  return (
    <div className="sticky top-0 z-50 border-b border-border bg-surface/80 backdrop-blur-md">
      <div className="mx-auto max-w-7xl px-4 py-2 text-xs font-mono">
        {/* Desktop: single row */}
        <div className="hidden sm:flex sm:items-center sm:justify-between">
          <span className="font-bold text-accent">SSR Test</span>
          <div className="flex items-center gap-3 text-muted">
            <DebugBarLive />
            <span className="rounded bg-warning/20 px-2 py-0.5 text-warning">
              incl. {forcedDelay}ms forced delay
            </span>
            {extra}
            <span>
              Data: <span className="text-foreground">~{dataSize}KB</span>
            </span>
          </div>
        </div>

        {/* Mobile: two rows */}
        <div className="flex flex-col gap-1 sm:hidden">
          <div className="flex items-center justify-between">
            <span className="font-bold text-accent">SSR Test</span>
            <div className="flex items-center gap-2 text-muted">
              <DebugBarLive />
              <span className="rounded bg-warning/20 px-2 py-0.5 text-warning">
                {forcedDelay}ms forced
              </span>
            </div>
          </div>
          <div className="flex items-center justify-end gap-3 text-muted">
            {extra}
            <span>
              Data: <span className="text-foreground">~{dataSize}KB</span>
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
