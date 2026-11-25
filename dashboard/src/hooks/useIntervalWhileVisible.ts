import { useEffect, useRef } from 'react';

/**
 * Like setInterval but pauses when the tab is hidden and skips a tick while the
 * previous callback is still running (no stacked duplicate requests).
 * Fires `callback` immediately when visible, then every `ms`.
 */
export function useIntervalWhileVisible(callback: () => void | Promise<void>, ms: number) {
  const cb = useRef(callback);
  cb.current = callback;
  const running = useRef(false);

  useEffect(() => {
    let id: ReturnType<typeof setInterval>;

    const tick = async () => {
      if (running.current) return;
      running.current = true;
      try {
        await cb.current();
      } finally {
        running.current = false;
      }
    };

    const start = () => {
      void tick();
      id = setInterval(() => void tick(), ms);
    };
    const stop = () => {
      clearInterval(id);
    };

    if (!document.hidden) start();

    const onVis = () => {
      document.hidden ? stop() : start();
    };
    document.addEventListener('visibilitychange', onVis);
    return () => {
      stop();
      document.removeEventListener('visibilitychange', onVis);
    };
  }, [ms]);
}
