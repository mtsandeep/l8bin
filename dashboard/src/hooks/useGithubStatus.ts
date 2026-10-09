import { useState } from 'react';
import { fetchGithubBulkStatus, type GitHubBulkRunStatus } from '../api';
import { useIntervalWhileVisible } from './useIntervalWhileVisible';

/**
 * Latest workflow-run status per linked project, polled while the tab is
 * visible. Absent key = project has no GitHub link (badge hidden).
 */
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
