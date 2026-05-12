import { useCallback, useEffect, useState } from 'react';
import {
  fetchAllStats,
  fetchProjects,
  fetchSystemStats,
  type Project,
  type ProjectStats,
  type ServiceStats,
} from '../api';
import { useIntervalWhileVisible } from './useIntervalWhileVisible';

export function useHomeData(user: { username: string; is_admin: boolean } | null, refreshKey?: number) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [stats, setStats] = useState<ProjectStats[]>([]);
  const [systemStats, setSystemStats] = useState<ServiceStats[]>([]);
  const [loading, setLoading] = useState(true);

  const loadProjectsAndStats = useCallback(async () => {
    if (!user) return;
    try {
      const [data, statsData, sysData] = await Promise.all([fetchProjects(), fetchAllStats(), fetchSystemStats()]);
      setProjects(data);
      setStats(statsData);
      setSystemStats(sysData);
    } catch (e) {
      console.error('Failed to fetch projects/stats:', e);
    } finally {
      setLoading(false);
    }
  }, [user]);

  useIntervalWhileVisible(() => {
    if (user) loadProjectsAndStats();
  }, 5000);

  useEffect(() => {
    if (user) loadProjectsAndStats();
  }, [loadProjectsAndStats, user, refreshKey]);

  return { projects, stats, systemStats, loading, loadProjectsAndStats };
}
