const API_BASE = '';

export interface Project {
  id: string;
  name: string | null;
  description: string | null;
  image: string | null;
  internal_port: number | null;
  mapped_port: number | null;
  container_id: string | null;
  node_id: string | null;
  status: string;
  last_active_at: number | null;
  auto_stop_enabled: boolean;
  auto_stop_timeout_mins: number;
  auto_start_enabled: boolean;
  cmd: string | null;
  memory_limit_mb: number | null;
  cpu_limit: number | null;
  custom_domain: string | null;
  created_at: number;
  updated_at: number;
}

export interface ProjectStats {
  project_id: string;
  status: string;
  cpu_percent: number;
  memory_usage: number;
  memory_limit: number;
  disk_gb: number;
}

export async function fetchProjects(): Promise<Project[]> {
  const res = await fetch(`${API_BASE}/projects`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch projects');
  return res.json();
}

export async function createProject(id: string): Promise<Project> {
  const res = await fetch(`${API_BASE}/projects`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ id }),
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to create project');
  }
  return res.json();
}

export async function fetchAllStats(): Promise<ProjectStats[]> {
  const res = await fetch(`${API_BASE}/projects/stats`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch stats');
  const data = await res.json();
  return data.stats;
}

export async function deployProject(payload: DeployPayload): Promise<void> {
  const res = await fetch(`${API_BASE}/deploy`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    credentials: 'include',
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Deploy failed');
  }
}

export interface ProjectSettings {
  name?: string;
  description?: string;
  custom_domain?: string;
  auto_stop_enabled?: boolean;
  auto_stop_timeout_mins?: number;
  auto_start_enabled?: boolean;
  cmd?: string;
  memory_limit_mb?: number | null;
  cpu_limit?: number | null;
}

export interface DeployPayload {
  project_id: string;
  image: string;
  port: number;
  name?: string;
  description?: string;
  node_id?: string | null;
  auto_stop_enabled?: boolean;
  auto_stop_timeout_mins?: number;
  auto_start_enabled?: boolean;
  cmd?: string;
  memory_limit_mb?: number | null;
  cpu_limit?: number | null;
}

export interface GlobalSettings {
  default_memory_limit_mb: number;
  default_cpu_limit: number;
  projects_dir: string;
  domain: string;
  dns_target: string;
  routing_mode: string;
  cloudflare_api_token: string;
  cloudflare_zone_id: string;
  dashboard_subdomain: string;
  poke_subdomain: string;
}

export async function fetchGlobalSettings(): Promise<GlobalSettings> {
  const res = await fetch(`${API_BASE}/settings`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch settings');
  return res.json();
}

export async function updateGlobalSettings(patch: Partial<GlobalSettings>): Promise<GlobalSettings> {
  const res = await fetch(`${API_BASE}/settings`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify(patch),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to update settings');
  }
  return res.json();
}

export async function cleanupDnsRecords(): Promise<{ deleted_count: number }> {
  const res = await fetch(`${API_BASE}/settings/cleanup-dns`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to cleanup DNS records');
  }
  return res.json();
}

export async function syncDnsRecords(): Promise<{ created: number; deleted: number; unchanged: number; errors: number }> {
  const res = await fetch(`${API_BASE}/settings/sync-dns`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to sync DNS records');
  }
  return res.json();
}

export async function updateProjectSettings(
  projectId: string,
  settings: ProjectSettings,
): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/settings`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify(settings),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to update project settings');
  }
}

export async function redeployProject(projectId: string, image: string, port: number, cmd?: string | null, memoryLimitMb?: number | null, cpuLimit?: number | null): Promise<void> {
  const res = await fetch(`${API_BASE}/deploy`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ project_id: projectId, image, port, cmd: cmd ?? undefined, memory_limit_mb: memoryLimitMb ?? undefined, cpu_limit: cpuLimit ?? undefined }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Redeploy failed');
  }
}

export async function recreateProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/recreate`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Recreate failed');
  }
}

export async function stopProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/stop`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Stop failed');
  }
}

export async function startProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/start`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Start failed');
  }
}

export async function deleteProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Delete failed');
  }
}

export interface LogsResponse {
  project_id: string;
  lines: string[];
}

export async function fetchLogs(projectId: string, tail = 100): Promise<LogsResponse> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/logs?tail=${tail}`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch logs');
  return res.json();
}

export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

// --- System Stats (LiteBin stack services) ---

export interface ServiceStats {
  name: string;
  memory_usage: number;
  cpu_percent: number;
  disk_usage: number;
}

export async function fetchSystemStats(): Promise<ServiceStats[]> {
  const res = await fetch(`${API_BASE}/system/stats`, { credentials: 'include' });
  if (!res.ok) return [];
  const data = await res.json();
  return data.services;
}

export function timeAgo(timestamp: number | null): string {
  if (!timestamp) return 'never';
  const seconds = Math.floor(Date.now() / 1000 - timestamp);
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export interface Node {
  id: string;
  name: string;
  host: string;
  public_ip: string | null;
  agent_port: number;
  region: string | null;
  status: string; // 'online' | 'offline' | 'decommissioned'
  total_memory: number | null;
  available_memory: number | null;
  total_cpu: number | null;
  disk_free: number | null;
  disk_total: number | null;
  container_count: number;
  last_seen_at: number | null;
  fail_count: number;
  created_at: number;
  updated_at: number;
}

export interface AddNodePayload {
  name: string;
  host: string;
  agent_port?: number;
  region?: string;
  public_ip?: string;
}

export async function fetchNodes(): Promise<Node[]> {
  const res = await fetch(`${API_BASE}/nodes`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch nodes');
  return res.json();
}

export async function addNode(payload: AddNodePayload): Promise<Node> {
  const res = await fetch(`${API_BASE}/nodes`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to add agent');
  }
  return res.json();
}

export async function connectNode(nodeId: string): Promise<Node> {
  const res = await fetch(`${API_BASE}/nodes/${nodeId}/connect`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to connect to agent');
  }
  return res.json();
}

export async function deleteNode(nodeId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/nodes/${nodeId}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to remove node');
  }
}

// --- Image Stats ---

export interface ImageStats {
  dangling_count: number;
  dangling_size: number;
  in_use_count: number;
  in_use_size: number;
  total_count: number;
  total_size: number;
}

export interface NodeImageStats {
  node_id: string;
  node_name: string;
  image_stats: ImageStats;
}

export async function fetchNodeImageStats(): Promise<NodeImageStats[]> {
  const res = await fetch(`${API_BASE}/nodes/image-stats`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch image stats');
  return res.json();
}

export async function pruneNodeImages(nodeId: string): Promise<{ bytes_reclaimed: number }> {
  const res = await fetch(`${API_BASE}/nodes/${nodeId}/images/prune`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to prune images');
  }
  return res.json();
}

// --- Deploy Tokens ---

export interface DeployTokenInfo {
  id: string;
  name: string | null;
  project_id: string | null;
  last_used_at: number | null;
  expires_at: number | null;
  created_at: number;
}

export interface CreateTokenResponse {
  token: string;
  token_info: DeployTokenInfo;
}

export async function fetchDeployTokens(projectId: string): Promise<DeployTokenInfo[]> {
  const res = await fetch(`${API_BASE}/deploy-tokens?project_id=${encodeURIComponent(projectId)}`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch deploy tokens');
  return res.json();
}

export async function createDeployToken(projectId: string | null, name?: string): Promise<CreateTokenResponse> {
  const res = await fetch(`${API_BASE}/deploy-tokens`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ project_id: projectId || undefined, name: name || undefined }),
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to create deploy token');
  }
  return res.json();
}

export async function revokeDeployToken(tokenId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/deploy-tokens/${encodeURIComponent(tokenId)}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to revoke deploy token');
  }
}

