const API_BASE = '';

function parseErrorMessage(text: string, fallback: string): string {
  try {
    const json = JSON.parse(text);
    if (json.error) return json.error;
  } catch {}
  return text || fallback;
}

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
  volumes: string | null; // JSON-encoded array of VolumeMount
  service_count: number | null;
  service_summary: string | null;
  created_at: number;
  updated_at: number;
}

export interface ProjectStats {
  project_id: string;
  status: string;
  services: ServiceInfo[];
}

export interface ServiceVolumeInfo {
  volume_name?: string;
  container_path: string;
}

export interface ServiceInfo {
  service_name: string;
  image: string;
  port: number | null;
  mapped_port?: number | null;
  is_public: boolean;
  status: string;
  cpu_percent?: number;
  memory_usage?: number;
  memory_limit?: number;
  cpu_limit?: number;
  disk_gb?: number;
  volumes?: ServiceVolumeInfo[];
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
    throw new Error(parseErrorMessage(text, 'Deploy failed'));
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
    throw new Error(parseErrorMessage(text, 'Failed to update settings'));
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
    throw new Error(parseErrorMessage(text, 'Failed to cleanup DNS records'));
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
    throw new Error(parseErrorMessage(text, 'Failed to sync DNS records'));
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
    throw new Error(parseErrorMessage(text, 'Failed to update project settings'));
  }
}

export async function redeployProject(projectId: string, image: string, port: number, cmd?: string | null, memoryLimitMb?: number | null, cpuLimit?: number | null, cleanupVolumes?: boolean): Promise<void> {
  const res = await fetch(`${API_BASE}/deploy`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ project_id: projectId, image, port, cmd: cmd ?? undefined, memory_limit_mb: memoryLimitMb ?? undefined, cpu_limit: cpuLimit ?? undefined, cleanup_volumes: cleanupVolumes || undefined }),
  });
  if (!res.ok) {
    const text = await res.text();
    let msg = text || 'Redeploy failed';
    try {
      const json = JSON.parse(text);
      if (json.error) msg = json.error;
    } catch {}
    throw new Error(msg);
  }
}

export async function recreateProject(projectId: string, services?: string[], pullImages?: boolean): Promise<void> {
  const hasBody = services || pullImages;
  const res = await fetch(`${API_BASE}/projects/${projectId}/recreate`, {
    method: 'POST',
    headers: hasBody ? { 'Content-Type': 'application/json' } : undefined,
    credentials: 'include',
    body: hasBody ? JSON.stringify({ services, pull_images: pullImages || undefined }) : undefined,
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Recreate failed'));
  }
}

export async function stopProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/stop`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Stop failed'));
  }
}

export async function startProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/start`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Start failed'));
  }
}

export async function startService(projectId: string, serviceName: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/services/${encodeURIComponent(serviceName)}/start`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Start service failed'));
  }
}

export async function stopService(projectId: string, serviceName: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/services/${encodeURIComponent(serviceName)}/stop`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Stop service failed'));
  }
}

export async function restartService(projectId: string, serviceName: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/services/${encodeURIComponent(serviceName)}/restart`, {
    method: 'POST',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Restart service failed'));
  }
}

export async function deleteProject(projectId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(parseErrorMessage(text, 'Delete failed'));
  }
}

export interface LogsResponse {
  project_id: string;
  service_name: string | null;
  lines: string[];
}

export async function fetchLogs(projectId: string, tail = 100, service?: string): Promise<LogsResponse> {
  const params = new URLSearchParams({ tail: String(tail) });
  if (service) params.set('service', service);
  const res = await fetch(`${API_BASE}/projects/${projectId}/logs?${params}`, { credentials: 'include' });
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

// --- Volumes ---

export interface VolumeMount {
  path: string;
  name?: string;
}

// --- Custom Routes ---

export interface ProjectRoute {
  id: string;
  project_id: string;
  route_type: string; // "path" | "alias"
  path: string | null;
  subdomain: string | null;
  upstream: string;
  priority: number;
  created_at: number;
}

export async function fetchProjectRoutes(projectId: string): Promise<ProjectRoute[]> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/routes`, { credentials: 'include' });
  if (!res.ok) throw new Error('Failed to fetch routes');
  return res.json();
}

export async function createProjectRoute(projectId: string, payload: { route_type: string; path?: string; subdomain?: string; upstream: string; priority?: number }): Promise<ProjectRoute> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/routes`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to create route');
  }
  return res.json();
}

export async function deleteProjectRoute(projectId: string, routeId: string): Promise<void> {
  const res = await fetch(`${API_BASE}/projects/${projectId}/routes/${encodeURIComponent(routeId)}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    throw new Error(data.error || 'Failed to delete route');
  }
}

