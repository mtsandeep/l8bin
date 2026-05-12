import { useEffect, useState } from 'react';

export default function Footer() {
  const [version, setVersion] = useState('');

  useEffect(() => {
    (async () => {
      try {
        const res = await fetch('/status', { credentials: 'include' });
        if (res.ok) {
          const data = await res.json();
          if (data.version) setVersion(data.version);
        }
      } catch {}
    })();
  }, []);

  return (
    <footer className="text-center py-6 text-xs text-slate-600">
      Powered by{' '}
      <a
        href="https://l8bin.com"
        target="_blank"
        rel="noopener noreferrer"
        className="text-slate-500 hover:text-slate-300 transition-colors"
      >
        l8bin.com
      </a>
      {version && <span className="ml-1.5 text-slate-400 font-medium">v{version}</span>}
    </footer>
  );
}
