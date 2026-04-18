"use client";

import { useSyncExternalStore } from "react";

interface TimingData {
  server: string;
}

let cachedData: TimingData = { server: "--ms" };
let cachedJSON = JSON.stringify(cachedData);

function subscribe(cb: () => void) {
  window.addEventListener("ssr-timing", cb);
  return () => window.removeEventListener("ssr-timing", cb);
}

function getServerSnapshot(): TimingData {
  return cachedData;
}

function getSnapshot(): TimingData {
  const incoming = (window as unknown as { __ssrTiming: TimingData }).__ssrTiming;
  if (incoming) {
    const json = JSON.stringify(incoming);
    if (json !== cachedJSON) {
      cachedData = incoming;
      cachedJSON = json;
    }
  }
  return cachedData;
}

export function DebugBarLive() {
  const timing = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);

  return (
    <span>
      Server: <span className="text-warning">{timing.server}</span>
    </span>
  );
}
