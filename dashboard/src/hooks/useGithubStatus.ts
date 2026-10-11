import { useState } from 'react';
import { fetchGithubBulkStatus, type GitHubBulkRunStatus } from '../api';
import { useIntervalWhileVisible } from './useIntervalWhileVisible';

export function useGithubStatus() {
  const [status, setStatus] = useState<Record<string, GitHubBulkRunStatus>>({});

  useIntervalWhileVisible(async () => {
    try {
      const res = await fetchGithubBulkStatus();
      setStatus(Object.fromEntries(res.projects.map((p) => [p.project_id, p])));
    } catch {
      // Not connected / transient — keep the last snapshot.
    }
  }, 30000);

  return status;
}
