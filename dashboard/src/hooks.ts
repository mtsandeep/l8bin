import { useEffect, useRef } from 'react';

/**
 * Like setInterval but pauses when the browser tab is hidden.
 * Fires `callback` immediately on mount, then every `ms` while visible.
 */
export function useIntervalWhileVisible(callback: () => void, ms: number) {
  const cb = useRef(callback);
  cb.current = callback;

  useEffect(() => {
    cb.current();
    let id: ReturnType<typeof setInterval>;

    const start = () => { cb.current(); id = setInterval(() => cb.current(), ms); };
    const stop = () => { clearInterval(id); };

    if (!document.hidden) start();

    const onVis = () => { document.hidden ? stop() : start(); };
    document.addEventListener('visibilitychange', onVis);
    return () => { stop(); document.removeEventListener('visibilitychange', onVis); };
  }, [ms]);
}
