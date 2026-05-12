import { useEffect, useState } from 'react';
import { fetchGlobalSettings } from '../api';

export function useSettings(user: { username: string; is_admin: boolean } | null) {
  const [domain, setDomain] = useState('localhost');
  const [projectsDir, setProjectsDir] = useState('projects');
  const [dnsTarget, setDnsTarget] = useState('');

  useEffect(() => {
    if (!user) return;
    (async () => {
      try {
        const settings = await fetchGlobalSettings();
        setDomain(settings.domain);
        setProjectsDir(settings.projects_dir);
        setDnsTarget(settings.dns_target);
      } catch (e) {
        console.error('Failed to fetch settings:', e);
      }
    })();
  }, [user]);

  return { domain, projectsDir, dnsTarget };
}
